//! Errors carrying the Snowflake error code and SQLSTATE that drivers expose
//! to applications (e.g. Python's `ProgrammingError.errno` / `.sqlstate`).

use std::fmt;

use localflake_sql::SqlError;

#[derive(Debug, Clone)]
pub struct EngineError {
    /// Six-digit Snowflake error code, e.g. `002003`.
    pub code: &'static str,
    pub sql_state: &'static str,
    pub message: String,
}

impl EngineError {
    fn new(code: &'static str, sql_state: &'static str, message: impl Into<String>) -> Self {
        Self { code, sql_state, message: message.into() }
    }

    fn compilation(code: &'static str, sql_state: &'static str, detail: impl fmt::Display) -> Self {
        Self::new(code, sql_state, format!("SQL compilation error:\n{detail}"))
    }

    pub(crate) fn internal(err: impl fmt::Display) -> Self {
        Self::new("000603", "XX000", format!("Internal error: {err}"))
    }

    /// A data loading or unloading error.
    pub(crate) fn load(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, "22000", message)
    }

    pub(crate) fn statement_count(actual: usize, desired: usize) -> Self {
        Self::new(
            "000008",
            "0A000",
            format!("Actual statement count {actual} did not match the desired statement count {desired}."),
        )
    }

    pub(crate) fn unknown_query(query_id: &str) -> Self {
        Self::new("000709", "02000", format!("Statement {query_id} not found"))
    }

    pub(crate) fn does_not_exist(object: &str, name: &str) -> Self {
        Self::compilation("002003", "02000", format!("{object} '{name}' does not exist or not authorized."))
    }

    pub(crate) fn already_exists(object: &str, name: &str) -> Self {
        Self::compilation("002002", "42710", format!("{object} '{name}' already exists."))
    }

    pub(crate) fn transaction_rolled_back() -> Self {
        Self::new(
            "000603",
            "40000",
            "Transaction rolled back: a statement in it failed, and DuckDB cannot continue a transaction after an error. \
             Nothing in the transaction was committed.",
        )
    }

    pub(crate) fn canceled() -> Self {
        Self::new("000604", "57014", "SQL execution canceled")
    }

    pub(crate) fn timed_out(seconds: u64) -> Self {
        Self::new(
            "000630",
            "57014",
            format!("Statement reached its statement or warehouse timeout of {seconds} second(s) and was canceled."),
        )
    }

    pub(crate) fn invalid_parameter(name: &str, value: &str) -> Self {
        Self::new("002043", "42601", format!("SQL compilation error:\ninvalid value [{value}] for parameter '{name}'"))
    }

    pub(crate) fn no_current_database() -> Self {
        Self::new(
            "090105",
            "22000",
            "Cannot perform operation. This session does not have a current database. Call 'USE DATABASE', or use a qualified name.",
        )
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.sql_state, self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<SqlError> for EngineError {
    fn from(err: SqlError) -> Self {
        match err {
            SqlError::Empty => Self::new("000900", "42601", "Empty SQL statement."),
            SqlError::Unsupported(feature) => {
                Self::compilation("002040", "0A000", format!("Unsupported feature '{feature}'."))
            }
            SqlError::NoDatabase => Self::no_current_database(),
            SqlError::UnknownVariable(name) => {
                Self::compilation("002211", "02000", format!("Session variable '${name}' does not exist"))
            }
            SqlError::Arguments(name) => {
                Self::compilation("001044", "42P13", format!("Invalid argument types for function '{name}'"))
            }
            other => Self::compilation("001003", "42000", format!("syntax error: {other}")),
        }
    }
}

impl From<duckdb::Error> for EngineError {
    fn from(err: duckdb::Error) -> Self {
        let message = localflake_sql::decode_identifier(&err.to_string());
        // DuckDB appends "Did you mean ...?" hints and a caret diagram after the first line.
        let first_line = message.lines().next().unwrap_or_default();
        let (kind, detail) = first_line.split_once(": ").unwrap_or(("", first_line));
        // Errors raised by localflake's own functions keep their Snowflake wording.
        for (pattern, code, state) in [
            ("Division by zero", "100051", "22012"),
            ("Timestamp '", "100035", "22007"),
            ("Date '", "100040", "22007"),
            ("Boolean value '", "100037", "22018"),
            ("Number out of representable range", "100046", "22003"),
            ("Key array and value array had unequal lengths", "215001", "22000"),
            ("Key supplied for ARRAYS_TO_OBJECT", "215002", "22000"),
            ("Decryption failed.", "100311", "22023"),
        ] {
            if let Some(start) = first_line.find(pattern) {
                return Self::new(code, state, first_line[start..].trim_end_matches(['"', '\'']).to_owned());
            }
        }
        match (kind, missing_object(detail)) {
            ("Catalog Error", Some(("Scalar Function" | "Aggregate Function" | "Table Function", name))) => {
                Self::compilation("002140", "42601", format!("Unknown function {}", name.to_uppercase()))
            }
            ("Catalog Error", Some(("Type", name))) => {
                Self::compilation("002040", "0A000", format!("Unsupported data type '{}'.", name.to_uppercase()))
            }
            ("Catalog Error", _) if detail.contains(" already exists") => match quoted_name(detail) {
                Some(name) => Self::already_exists("Object", name),
                None => Self::compilation("002002", "42710", detail),
            },
            ("Catalog Error", Some((_, name))) => {
                Self::compilation("002003", "42S02", format!("Object '{name}' does not exist or not authorized."))
            }
            ("Catalog Error", None) => Self::compilation("002003", "42S02", detail),
            ("Binder Error", _) if detail.starts_with("Referenced column") => match quoted_name(detail) {
                Some(name) => Self::compilation(
                    "000904",
                    "42000",
                    format!("error line 0 at position 0\ninvalid identifier '{name}'"),
                ),
                None => Self::compilation("000904", "42000", detail),
            },
            ("Parser Error", _) => Self::compilation("001003", "42000", format!("syntax error: {detail}")),
            ("Binder Error", _) => Self::compilation("002262", "42000", detail),
            // The CHECK localflake adds for declared VARCHAR lengths.
            ("Constraint Error", _) if detail.contains("CHECK constraint") && detail.contains("length(") => {
                Self::new("100074", "22001", "String is too long and would be truncated")
            }
            ("Constraint Error", _) => Self::new("100072", "22000", detail),
            ("Conversion Error", _) => match numeric_cast(detail) {
                Some(value) => Self::new("100038", "22018", format!("Numeric value '{value}' is not recognized")),
                None => Self::new("100038", "22018", detail),
            },
            ("Invalid Input Error" | "Out of Range Error", _) => Self::new("100035", "22000", detail),
            _ => Self::internal(message),
        }
    }
}

impl From<duckdb::arrow::error::ArrowError> for EngineError {
    fn from(err: duckdb::arrow::error::ArrowError) -> Self {
        Self::internal(err)
    }
}

/// Parses `<Kind> with name <NAME> does not exist!` into `(kind, name)`.
fn missing_object(detail: &str) -> Option<(&str, &str)> {
    let (kind, rest) = detail.split_once(" with name ")?;
    let (name, _) = rest.split_once(" does not exist")?;
    Some((kind, name))
}

/// The value of a failed string-to-number cast: `Could not convert string 'x' to INT64 ...`.
fn numeric_cast(detail: &str) -> Option<&str> {
    let rest = detail.strip_prefix("Could not convert string '")?;
    let (value, target) = rest.rsplit_once("' to ")?;
    let numeric = ["INT", "UINT", "HUGEINT", "DECIMAL", "DOUBLE", "FLOAT", "BIGINT", "SMALLINT", "TINYINT"];
    numeric.iter().any(|t| target.starts_with(t)).then_some(value)
}

/// Extracts the first double-quoted name from a DuckDB error message,
/// e.g. `Referenced column "NOPE" not found`.
fn quoted_name(detail: &str) -> Option<&str> {
    let start = detail.find('"')? + 1;
    let len = detail[start..].find('"')?;
    Some(&detail[start..start + len])
}
