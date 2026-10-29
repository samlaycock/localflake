//! Snowflake data types → DuckDB storage types.
//!
//! | Snowflake | DuckDB |
//! |---|---|
//! | `NUMBER`, `NUMBER(p,0)`, `INT` and friends | `BIGINT` |
//! | `NUMBER(p,s)`, s > 0 | `DECIMAL(p,s)` |
//! | `FLOAT`, `DOUBLE`, `REAL` | `DOUBLE` |
//! | `VARCHAR(n)`, `STRING`, `TEXT`, `CHAR` | `VARCHAR` |
//! | `BINARY`, `VARBINARY` | `BLOB` |
//! | `TIMESTAMP_NTZ`, `DATETIME` | `TIMESTAMP` |
//! | `TIMESTAMP_LTZ`, `TIMESTAMP_TZ` | `TIMESTAMPTZ` |
//! | `VARIANT`, `OBJECT`, `ARRAY` | `VARIANT` |
//! | `VECTOR(INT, n)`, `VECTOR(FLOAT, n)` | `INTEGER[n]`, `FLOAT[n]` |
//!
//! Integers are stored as `BIGINT` rather than `DECIMAL(38,0)` because DuckDB
//! functions accept `BIGINT` far more widely; values beyond ±9.2e18 overflow.
//! Vectors are the only 32-bit numbers and fixed-size arrays, which is how the
//! engine recognises them in results.

use sqlparser::ast::{DataType, ExactNumberInfo, TimezoneInfo};

use crate::ast::custom_type;
use crate::{Context, SqlError, TimestampMapping};

/// TIMESTAMP_TZ storage: TIMESTAMP_NS with the offset in the nanosecond digits
/// (see the engine's `tz` module, which registers the conversions).
fn timestamp_tz() -> DataType {
    custom_type("TIMESTAMP_NS")
}

pub fn map_type(data_type: &DataType, ctx: &Context) -> Result<DataType, SqlError> {
    use DataType as D;
    let mapped = match data_type {
        D::TinyInt(_)
        | D::SmallInt(_)
        | D::Int(_)
        | D::Integer(_)
        | D::BigInt(_)
        | D::Int2(_)
        | D::Int4(_)
        | D::Int8(_)
        | D::Int16
        | D::Int32
        | D::Int64 => D::BigInt(None),
        D::Numeric(info) | D::Decimal(info) | D::Dec(info) | D::BigNumeric(info) | D::BigDecimal(info) => number(info),
        D::Float(_) | D::Float4 | D::Float8 | D::Float32 | D::Float64 | D::Real | D::Double(_) | D::DoublePrecision => {
            D::Double(ExactNumberInfo::None)
        }
        D::Varchar(_)
        | D::Nvarchar(_)
        | D::Char(_)
        | D::Character(_)
        | D::CharVarying(_)
        | D::CharacterVarying(_)
        | D::String(_)
        | D::Text
        | D::Clob(_) => D::Varchar(None),
        D::Binary(_) | D::Varbinary(_) | D::Blob(_) | D::Bytea | D::Bytes(_) => D::Blob(None),
        D::Bool | D::Boolean => D::Boolean,
        D::Date => D::Date,
        D::Time(..) => D::Time(None, TimezoneInfo::None),
        D::Datetime(_) | D::TimestampNtz(_) => ntz(),
        D::Timestamp(_, TimezoneInfo::None | TimezoneInfo::WithoutTimeZone) => match ctx.timestamp_type_mapping {
            TimestampMapping::Ntz => ntz(),
            TimestampMapping::Ltz => tz(),
            TimestampMapping::Tz => timestamp_tz(),
        },
        // `TIMESTAMP WITH TIME ZONE` is Snowflake's TIMESTAMP_TZ.
        D::Timestamp(_, TimezoneInfo::WithTimeZone | TimezoneInfo::Tz) => timestamp_tz(),
        D::Array(sqlparser::ast::ArrayElemTypeDef::None) => custom_type("VARIANT[]"),
        D::JSON | D::JSONB => custom_type("VARIANT"),
        D::Custom(name, modifiers) => {
            let name = name.to_string().trim_matches('"').to_uppercase();
            match name.as_str() {
                "NUMBER" => number(&exact_number(modifiers)?),
                "BYTEINT" => D::BigInt(None),
                "FLOAT4" | "FLOAT8" => D::Double(ExactNumberInfo::None),
                "NCHAR" | "NVARCHAR2" | "CHAR VARYING" | "NCHAR VARYING" => D::Varchar(None),
                "VARBINARY" => D::Blob(None),
                "TIMESTAMP_NTZ" | "TIMESTAMPNTZ" | "TIMESTAMP WITHOUT TIME ZONE" => ntz(),
                "TIMESTAMP_LTZ" | "TIMESTAMPLTZ" | "TIMESTAMP WITH LOCAL TIME ZONE" => tz(),
                "TIMESTAMP_TZ" | "TIMESTAMPTZ" => timestamp_tz(),
                "VARIANT" => custom_type("VARIANT"),
                // OBJECT and ARRAY get their own storage types so they report as
                // OBJECT and ARRAY; both convert to and from VARIANT.
                "OBJECT" => custom_type("JSON"),
                "ARRAY" => custom_type("VARIANT[]"),
                "VECTOR" => vector(modifiers)?,
                "GEOGRAPHY" | "GEOMETRY" => return Err(SqlError::Unsupported(format!("data type {name}"))),
                _ => data_type.clone(),
            }
        }
        other => other.clone(),
    };
    Ok(mapped)
}

/// True for Snowflake types whose DuckDB mapping is `VARCHAR`.
pub fn is_text(data_type: &DataType) -> bool {
    matches!(map_type(data_type, &Context::default()), Ok(DataType::Varchar(_)))
}

fn number(info: &ExactNumberInfo) -> DataType {
    match info {
        ExactNumberInfo::PrecisionAndScale(p, s) if *s > 0 => {
            DataType::Decimal(ExactNumberInfo::PrecisionAndScale((*p).min(38), (*s).min(37)))
        }
        _ => DataType::BigInt(None),
    }
}

fn exact_number(modifiers: &[String]) -> Result<ExactNumberInfo, SqlError> {
    let parse = |s: &String| s.trim().parse::<u64>().map_err(|_| SqlError::Unsupported(format!("NUMBER({s})")));
    Ok(match modifiers {
        [] => ExactNumberInfo::None,
        [p] => ExactNumberInfo::Precision(parse(p)?),
        [p, s] => ExactNumberInfo::PrecisionAndScale(parse(p)?, parse(s)? as i64),
        _ => return Err(SqlError::Unsupported(format!("NUMBER({})", modifiers.join(", ")))),
    })
}

/// The most dimensions Snowflake allows in a VECTOR.
const MAX_VECTOR_DIMENSION: u64 = 4096;

/// `VECTOR(<INT | FLOAT>, <dimension>)`.
fn vector(modifiers: &[String]) -> Result<DataType, SqlError> {
    let invalid = || SqlError::Unsupported(format!("VECTOR({})", modifiers.join(", ")));
    let [element, dimension] = modifiers else { return Err(invalid()) };
    let element = match element.trim().to_uppercase().as_str() {
        "INT" | "INTEGER" => "INTEGER",
        "FLOAT" => "FLOAT",
        _ => return Err(invalid()),
    };
    let dimension: u64 = dimension.trim().parse().map_err(|_| invalid())?;
    if !(1..=MAX_VECTOR_DIMENSION).contains(&dimension) {
        return Err(invalid());
    }
    Ok(custom_type(&format!("{element}[{dimension}]")))
}

fn ntz() -> DataType {
    DataType::Timestamp(None, TimezoneInfo::None)
}

fn tz() -> DataType {
    DataType::Timestamp(None, TimezoneInfo::WithTimeZone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::SnowflakeDialect;
    use sqlparser::parser::Parser;

    fn mapped(sf: &str) -> String {
        let data_type = Parser::new(&SnowflakeDialect).try_with_sql(sf).unwrap().parse_data_type().unwrap();
        map_type(&data_type, &Context::default()).unwrap().to_string()
    }

    #[test]
    fn maps_snowflake_types() {
        for (sf, duck) in [
            ("NUMBER", "BIGINT"),
            ("NUMBER(38,0)", "BIGINT"),
            ("NUMBER(10,2)", "DECIMAL(10,2)"),
            ("DECIMAL(12,4)", "DECIMAL(12,4)"),
            ("INT", "BIGINT"),
            ("BYTEINT", "BIGINT"),
            ("FLOAT", "DOUBLE"),
            ("VARCHAR(10)", "VARCHAR"),
            ("STRING", "VARCHAR"),
            ("TEXT", "VARCHAR"),
            ("BINARY", "BLOB"),
            ("TIMESTAMP", "TIMESTAMP"),
            ("TIMESTAMP_NTZ(9)", "TIMESTAMP"),
            ("TIMESTAMP_LTZ", "TIMESTAMP WITH TIME ZONE"),
            ("TIMESTAMP_TZ(3)", "TIMESTAMP_NS"),
            ("DATETIME", "TIMESTAMP"),
            ("VARIANT", "VARIANT"),
            ("OBJECT", "JSON"),
            ("ARRAY", "VARIANT[]"),
            ("VECTOR(FLOAT, 3)", "FLOAT[3]"),
            ("VECTOR(INT, 256)", "INTEGER[256]"),
        ] {
            assert_eq!(mapped(sf), duck, "{sf}");
        }
    }

    #[test]
    fn rejects_invalid_vectors() {
        for sf in ["VECTOR(FLOAT)", "VECTOR(VARCHAR, 3)", "VECTOR(INT, 0)", "VECTOR(FLOAT, 4097)"] {
            let data_type = Parser::new(&SnowflakeDialect).try_with_sql(sf).unwrap().parse_data_type().unwrap();
            assert!(map_type(&data_type, &Context::default()).is_err(), "{sf}");
        }
    }
}
