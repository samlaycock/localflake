//! Snowflake function calls → DuckDB expressions.
//!
//! Three mechanisms, in order of preference:
//! - a rename, when DuckDB has the same function under another name;
//! - an AST rewrite, for differences in arguments or NULL handling;
//! - a call to a helper defined in [`MACROS`] (SQL) or registered by the
//!   engine in Rust (`sf_div`, `sf_cast_like`, `sf_to_timestamp`, ...). Rust
//!   helpers are used where the result type depends on the argument type, which
//!   DuckDB resolves through overloads.

use sqlparser::ast::{
    BinaryOperator, DataType, DuplicateTreatment, ExactNumberInfo, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentClause, FunctionArguments, OrderByExpr, TimezoneInfo, UnaryOperator, Value,
};

use crate::ast::*;
use crate::datetime::{Part, convert_format};
use crate::variant;
use crate::{Context, SqlError};

/// SQL helpers created in every DuckDB session (as temporary macros).
pub const MACROS: &str = r#"
CREATE OR REPLACE TEMP MACRO sf_is_json_null(x) AS
    CASE WHEN typeof(x) = 'VARIANT' THEN coalesce(TRY_CAST(CAST(x AS VARIANT) AS VARCHAR) = chr(57344) || 'null', false)
        ELSE false END;
CREATE OR REPLACE TEMP MACRO sf_json_variant(text) AS CAST(CAST(sf_json_nulls(text) AS JSON) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_json_null() AS CAST(chr(57344) || 'null' AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_variant_text_value(v) AS
    CASE WHEN variant_typeof(v) = 'VARCHAR' THEN nullif(CAST(v AS VARCHAR), chr(57344) || 'null')
        WHEN regexp_matches(variant_typeof(v), '^(OBJECT|ARRAY)') THEN sf_json_render(CAST(CAST(v AS JSON) AS VARCHAR))
        ELSE CAST(v AS VARCHAR) END;
CREATE OR REPLACE TEMP MACRO sf_to_varchar(x) AS
    CASE WHEN typeof(x) IN ('VARIANT', 'JSON') OR typeof(x) LIKE '%[]' THEN
            CASE WHEN sf_is_json_null(x) THEN NULL
                WHEN regexp_matches(variant_typeof(CAST(x AS VARIANT)), '^(OBJECT|ARRAY)')
                    THEN sf_json_render(CAST(CAST(CAST(x AS VARIANT) AS JSON) AS VARCHAR))
                ELSE CAST(CAST(x AS VARIANT) AS VARCHAR) END
        WHEN typeof(x) = 'BOOLEAN' THEN CAST(x AS VARCHAR)
        WHEN typeof(x) = 'BLOB' THEN upper(hex(CAST(x AS BLOB)))
        ELSE CAST(x AS VARCHAR) END;
CREATE OR REPLACE TEMP MACRO sf_variant_type(v) AS sf_variant_label(variant_typeof(v), TRY_CAST(v AS VARCHAR));
CREATE OR REPLACE TEMP MACRO sf_typeof(x) AS sf_variant_type(CAST(x AS VARIANT));
CREATE OR REPLACE TEMP MACRO sf_get(v, k) AS
    CASE WHEN regexp_matches(typeof(k), '^(U?(TINY|SMALL|BIG|HUGE)?INT(EGER)?|DECIMAL.*)$')
        THEN CASE WHEN CAST(k AS BIGINT) >= 0 AND regexp_matches(variant_typeof(CAST(v AS VARIANT)), '^ARRAY')
            THEN CAST(CAST(v AS VARIANT) AS VARIANT[])[CAST(k AS BIGINT) + 1] END
        ELSE CASE WHEN regexp_matches(variant_typeof(CAST(v AS VARIANT)), '^OBJECT')
            THEN CAST(json_extract(CAST(CAST(v AS VARIANT) AS JSON), '$."' || replace(CAST(k AS VARCHAR), '"', '\"') || '"') AS VARIANT) END
        END;
CREATE OR REPLACE TEMP MACRO sf_array_size(x) AS
    CASE WHEN regexp_matches(variant_typeof(CAST(x AS VARIANT)), '^ARRAY')
        THEN CAST(variant_array_length(CAST(x AS VARIANT)) AS BIGINT) END;
CREATE OR REPLACE TEMP MACRO sf_variant_array(x) AS
    CASE WHEN regexp_matches(variant_typeof(CAST(x AS VARIANT)), '^ARRAY')
        THEN CAST(CAST(x AS VARIANT) AS VARIANT[]) END;
CREATE OR REPLACE TEMP MACRO sf_to_boolean(x) AS
    CASE WHEN typeof(x) = 'VARCHAR' THEN
        CASE lower(trim(CAST(x AS VARCHAR)))
            WHEN 'true' THEN true WHEN 't' THEN true WHEN 'yes' THEN true WHEN 'y' THEN true WHEN 'on' THEN true WHEN '1' THEN true
            WHEN 'false' THEN false WHEN 'f' THEN false WHEN 'no' THEN false WHEN 'n' THEN false WHEN 'off' THEN false WHEN '0' THEN false
            ELSE error('Boolean value ''' || CAST(x AS VARCHAR) || ''' is not recognized') END
        ELSE CAST(x AS BOOLEAN) END;
CREATE OR REPLACE TEMP MACRO sf_try_to_boolean(x) AS
    CASE WHEN typeof(x) = 'VARCHAR' THEN
        CASE lower(trim(CAST(x AS VARCHAR)))
            WHEN 'true' THEN true WHEN 't' THEN true WHEN 'yes' THEN true WHEN 'y' THEN true WHEN 'on' THEN true WHEN '1' THEN true
            WHEN 'false' THEN false WHEN 'f' THEN false WHEN 'no' THEN false WHEN 'n' THEN false WHEN 'off' THEN false WHEN '0' THEN false
            END
        ELSE TRY_CAST(x AS BOOLEAN) END;
CREATE OR REPLACE TEMP MACRO sf_tokens(s, d) AS
    list_filter(string_split_regex(s, '[' || regexp_escape(d) || ']'), lambda t: t <> '');
CREATE OR REPLACE TEMP MACRO sf_array_to_string(a, sep) AS
    array_to_string(list_transform(sf_variant_array(a), lambda x: sf_to_varchar(x)), sep);
CREATE OR REPLACE TEMP MACRO sf_array_compact(a) AS
    CAST(list_filter(sf_variant_array(a), lambda x: x IS NOT NULL AND NOT sf_is_json_null(x)) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_length(x) AS
    CASE WHEN typeof(x) = 'BLOB' THEN octet_length(CAST(x AS BLOB)) ELSE length(CAST(x AS VARCHAR)) END;
CREATE OR REPLACE TEMP MACRO sf_ltz(x) AS
    CASE WHEN typeof(x) = 'VARCHAR' AND NOT sf_has_tz_offset(CAST(x AS VARCHAR))
            AND NOT regexp_full_match(trim(CAST(x AS VARCHAR)), '-?[0-9]+(\.[0-9]*)?')
            THEN CAST(sf_to_timestamp(x) AS TIMESTAMPTZ)
        WHEN typeof(x) IN ('TIMESTAMP', 'DATE') THEN CAST(x AS TIMESTAMPTZ)
        WHEN typeof(x) = 'TIMESTAMP_NS' THEN sf_tz_instant(x)
        ELSE sf_to_timestamp_tz(x) END;
CREATE OR REPLACE TEMP MACRO sf_tz_offset(ts, colon) AS
    printf('%s%02d' || CASE WHEN colon THEN ':' ELSE '' END || '%02d',
        CASE WHEN date_diff('minute', make_timestamp(epoch_us(ts)), CAST(ts AS TIMESTAMP)) < 0 THEN '-' ELSE '+' END,
        abs(date_diff('minute', make_timestamp(epoch_us(ts)), CAST(ts AS TIMESTAMP))) // 60,
        abs(date_diff('minute', make_timestamp(epoch_us(ts)), CAST(ts AS TIMESTAMP))) % 60);
CREATE OR REPLACE TEMP MACRO sf_split_part(s, d, n) AS
    CASE WHEN d = '' THEN CASE WHEN n IN (0, 1, -1) THEN s ELSE '' END
        ELSE split_part(s, d, CASE WHEN n = 0 THEN 1 ELSE n END) END;
CREATE OR REPLACE TEMP MACRO sf_zipf(s, n) AS
    (SELECT min(k) FROM (SELECT k, sum(1 / power(k, s)) OVER (ORDER BY k) / sum(1 / power(k, s)) OVER () AS cdf
        FROM range(1, n + 1) AS t(k)) WHERE cdf >= random());
CREATE OR REPLACE TEMP MACRO sf_width_bucket(x, lo, hi, n) AS
    CASE WHEN x < lo THEN 0 WHEN x >= hi THEN n + 1
        ELSE CAST(floor((x - lo) * n / (hi - lo)) AS BIGINT) + 1 END;
CREATE OR REPLACE TEMP MACRO sf_haversine(lat1, lon1, lat2, lon2) AS
    2 * 6371 * asin(sqrt(power(sin(radians(lat2 - lat1) / 2), 2)
        + cos(radians(lat1)) * cos(radians(lat2)) * power(sin(radians(lon2 - lon1) / 2), 2)));
CREATE OR REPLACE TEMP MACRO sf_insert(s, pos, len, repl) AS
    substr(s, 1, pos - 1) || repl || substr(s, pos + len);
CREATE OR REPLACE TEMP MACRO sf_regexp_instr(s, p, pos, occ, opt, flags) AS
    CASE WHEN len(regexp_extract_all(substr(s, pos), p, 0, flags)) < occ THEN 0
        ELSE pos
            + coalesce(list_sum(list_transform(string_split_regex(substr(s, pos), p, flags)[1:occ], lambda x: length(x))), 0)
            + coalesce(list_sum(list_transform(regexp_extract_all(substr(s, pos), p, 0, flags)[1:occ - 1], lambda x: length(x))), 0)
            + CASE WHEN opt = 1 THEN length(regexp_extract_all(substr(s, pos), p, 0, flags)[occ]) ELSE 0 END
        END;
CREATE OR REPLACE TEMP MACRO sf_randstr(n) AS
    array_to_string(list_transform(range(n), lambda i: substr('0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz', CAST(floor(random() * 62) AS INTEGER) + 1, 1)), '');
CREATE OR REPLACE TEMP MACRO sf_months_between(a, b) AS
    CAST((year(a) - year(b)) * 12 + month(a) - month(b)
        + CASE WHEN day(a) = day(b) OR (CAST(a AS DATE) = last_day(a) AND CAST(b AS DATE) = last_day(b)) THEN 0
            ELSE (day(a) - day(b)) / 31.0
                + (epoch(CAST(a AS TIMESTAMP)) - epoch(CAST(CAST(a AS DATE) AS TIMESTAMP))
                    - epoch(CAST(b AS TIMESTAMP)) + epoch(CAST(CAST(b AS DATE) AS TIMESTAMP))) / 86400.0 / 31 END
        AS DECIMAL(38,6));
CREATE OR REPLACE TEMP MACRO sf_dow_number(name) AS
    CASE lower(left(trim(name), 2)) WHEN 'su' THEN 0 WHEN 'mo' THEN 1 WHEN 'tu' THEN 2 WHEN 'we' THEN 3
        WHEN 'th' THEN 4 WHEN 'fr' THEN 5 WHEN 'sa' THEN 6 ELSE error('Invalid day of week: ' || name) END;
CREATE OR REPLACE TEMP MACRO sf_next_day(d, name) AS
    CAST(d AS DATE) + CAST((sf_dow_number(name) - dayofweek(d) + 6) % 7 + 1 AS INTEGER);
CREATE OR REPLACE TEMP MACRO sf_previous_day(d, name) AS
    CAST(d AS DATE) - CAST((dayofweek(d) - sf_dow_number(name) + 6) % 7 + 1 AS INTEGER);
CREATE OR REPLACE TEMP MACRO sf_variant_text(x) AS CAST(CAST(CAST(x AS VARIANT) AS JSON) AS VARCHAR);
CREATE OR REPLACE TEMP MACRO sf_variant_texts(a) AS list_transform(sf_variant_array(a), lambda x: sf_variant_text(x));
CREATE OR REPLACE TEMP MACRO sf_array_position(item, a) AS
    list_position(sf_variant_texts(a), sf_variant_text(item)) - 1;
CREATE OR REPLACE TEMP MACRO sf_array_remove(a, item) AS
    CAST(list_filter(sf_variant_array(a), lambda x: sf_variant_text(x) <> sf_variant_text(item)) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_array_intersection(a, b) AS
    CAST(list_filter(sf_variant_array(a), lambda x: list_contains(sf_variant_texts(b), sf_variant_text(x))) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_array_except(a, b) AS
    CAST(list_filter(sf_variant_array(a), lambda x: NOT list_contains(sf_variant_texts(b), sf_variant_text(x))) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_arrays_overlap(a, b) AS list_has_any(sf_variant_texts(a), sf_variant_texts(b));
CREATE OR REPLACE TEMP MACRO sf_array_insert(a, pos, item) AS
    CAST(list_concat(list_concat(sf_variant_array(a)[1:pos], [CAST(item AS VARIANT)]), sf_variant_array(a)[pos + 1:]) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_array_distinct(a) AS
    CAST(list_filter(sf_variant_array(a), lambda x, i: list_position(sf_variant_texts(a), sf_variant_text(x)) = i) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_array_flatten(a) AS
    CAST(flatten(list_transform(sf_variant_array(a), lambda x: sf_variant_array(x))) AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_object_json(o) AS CAST(CAST(o AS VARIANT) AS JSON);
CREATE OR REPLACE TEMP MACRO sf_get_ignore_case(o, key) AS
    CAST(json_extract(sf_object_json(o), '$."' || replace(coalesce(
        list_filter(json_keys(sf_object_json(o)), lambda k: k = key)[1],
        list_filter(json_keys(sf_object_json(o)), lambda k: lower(k) = lower(key))[1]), '"', '\"') || '"') AS VARIANT);
CREATE OR REPLACE TEMP MACRO sf_charindex(needle, haystack, start) AS
    CASE WHEN instr(substr(haystack, start), needle) = 0 THEN 0
        ELSE instr(substr(haystack, start), needle) + start - 1 END;
"#;

pub fn rewrite(f: &mut Function, ctx: &Context) -> Result<Option<Expr>, SqlError> {
    if f.name.0.len() != 1 {
        return Ok(None);
    }
    let name = f.name.to_string().trim_matches('"').to_uppercase();
    let result = rewrite_call(f, ctx)?;
    // OBJECT and ARRAY results get their storage types (JSON and VARIANT[]),
    // which also makes them report as OBJECT and ARRAY.
    Ok(result.map(|expr| {
        if OBJECT_RESULTS.contains(&name.as_str()) {
            object_value(expr)
        } else if ARRAY_RESULTS.contains(&name.as_str()) {
            array_value(expr)
        } else {
            expr
        }
    }))
}

/// Functions whose Snowflake result type is OBJECT.
const OBJECT_RESULTS: &[&str] = &[
    "OBJECT_CONSTRUCT",
    "OBJECT_CONSTRUCT_KEEP_NULL",
    "OBJECT_INSERT",
    "OBJECT_DELETE",
    "OBJECT_PICK",
    "OBJECT_AGG",
    "PARSE_URL",
    "TO_OBJECT",
    "AS_OBJECT",
];

/// Functions whose Snowflake result type is ARRAY.
const ARRAY_RESULTS: &[&str] = &[
    "ARRAY_CONSTRUCT",
    "ARRAY_CONSTRUCT_COMPACT",
    "ARRAY_AGG",
    "ARRAYAGG",
    "ARRAY_UNIQUE_AGG",
    "ARRAY_UNION_AGG",
    "ARRAY_APPEND",
    "ARRAY_PREPEND",
    "ARRAY_CAT",
    "ARRAY_COMPACT",
    "ARRAY_DISTINCT",
    "ARRAY_EXCEPT",
    "ARRAY_FLATTEN",
    "ARRAY_GENERATE_RANGE",
    "ARRAY_INSERT",
    "ARRAY_INTERSECTION",
    "ARRAY_REMOVE",
    "ARRAY_REVERSE",
    "ARRAY_SLICE",
    "ARRAY_SORT",
    "ARRAYS_ZIP",
    "SPLIT",
    "STRTOK_TO_ARRAY",
    "OBJECT_KEYS",
    "TO_ARRAY",
    "AS_ARRAY",
    "FILTER",
    "TRANSFORM",
    "APPROX_TOP_K",
];

fn rewrite_call(f: &mut Function, ctx: &Context) -> Result<Option<Expr>, SqlError> {
    let name = f.name.to_string().trim_matches('"').to_uppercase();
    coerce_temporal_literals(f, &name);
    // `LAG(x) IGNORE NULLS OVER (...)`: DuckDB only accepts IGNORE NULLS inside the parentheses.
    if let Some(treatment) = f.null_treatment.take()
        && let FunctionArguments::List(list) = &mut f.args
    {
        list.clauses.push(FunctionArgumentClause::IgnoreOrRespectNulls(treatment));
    }
    let args = || plain_args(f, &name);
    let arg = |i: usize| -> Result<Expr, SqlError> {
        args()?.into_iter().nth(i).ok_or_else(|| SqlError::Arguments(name.clone()))
    };
    let opt_arg = |i: usize| -> Result<Option<Expr>, SqlError> { Ok(args()?.into_iter().nth(i)) };

    let expr = match name.as_str() {
        // Conditional and NULL handling.
        "IFF" => case(vec![(arg(0)?, arg(1)?)], Some(arg(2)?)),
        "NVL" | "IFNULL" => return rename(f, "coalesce"),
        "NVL2" => case(vec![(is_not_null(arg(0)?), arg(1)?)], Some(arg(2)?)),
        "ZEROIFNULL" => func("coalesce", vec![arg(0)?, number(0)]),
        "NULLIFZERO" => func("nullif", vec![arg(0)?, number(0)]),
        "DIV0" => case(
            vec![(binop(arg(1)?, BinaryOperator::Eq, number(0)), number(0))],
            Some(crate::typing::divide(&ctx.types, arg(0)?, arg(1)?)),
        ),
        "DIV0NULL" => {
            let zero = binop(arg(1)?, BinaryOperator::Eq, number(0));
            case(
                vec![(or_all(vec![zero, is_null(arg(1)?)]), number(0))],
                Some(crate::typing::divide(&ctx.types, arg(0)?, arg(1)?)),
            )
        }
        "EQUAL_NULL" => Expr::IsNotDistinctFrom(Box::new(nested(arg(0)?)), Box::new(nested(arg(1)?))),
        "DECODE" => decode(args()?, &name)?,
        "GREATEST" | "LEAST" | "CONCAT" | "CONCAT_WS" => {
            // Snowflake returns NULL if any argument is NULL; DuckDB skips NULLs.
            let args = args()?;
            let args = if name.starts_with("CONCAT") {
                args.into_iter().map(|a| crate::typing::text(ctx, a)).collect()
            } else {
                args
            };
            let any_null = or_all(args.iter().cloned().map(is_null).collect());
            case(vec![(any_null, null())], Some(func(&name.to_lowercase(), args)))
        }

        // Strings.
        // Characters for text, bytes for BINARY.
        "LEN" | "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => func("sf_length", vec![arg(0)?]),
        "STARTSWITH" => return rename(f, "starts_with"),
        "SPLIT_PART" => func("sf_split_part", vec![arg(0)?, arg(1)?, arg(2)?]),
        "OCTET_LENGTH" => func("strlen", vec![cast(arg(0)?, DataType::Varchar(None))]),
        "BIT_LENGTH" => {
            binop(func("strlen", vec![cast(arg(0)?, DataType::Varchar(None))]), BinaryOperator::Multiply, number(8))
        }
        "ENDSWITH" => return rename(f, "ends_with"),
        "EDITDISTANCE" => func("levenshtein", vec![arg(0)?, arg(1)?]),
        "JAROWINKLER_SIMILARITY" => cast(
            func(
                "round",
                vec![binop(
                    func("jaro_winkler_similarity", vec![arg(0)?, arg(1)?]),
                    BinaryOperator::Multiply,
                    number(100),
                )],
            ),
            DataType::BigInt(None),
        ),
        "SOUNDEX" => func("sf_soundex", vec![arg(0)?]),
        "INSERT" => func("sf_insert", vec![arg(0)?, arg(1)?, arg(2)?, arg(3)?]),
        "RANDSTR" => func("sf_randstr", vec![arg(0)?]),
        "PARSE_URL" => func("sf_json_variant", vec![func("sf_parse_url", vec![arg(0)?])]),
        "REGEXP_INSTR" => {
            let flags = string(regexp_flags(opt_arg(5)?.as_ref()));
            let position = opt_arg(2)?.unwrap_or_else(|| number(1));
            let occurrence = opt_arg(3)?.unwrap_or_else(|| number(1));
            let option = opt_arg(4)?.unwrap_or_else(|| number(0));
            func("sf_regexp_instr", vec![arg(0)?, arg(1)?, position, occurrence, option, flags])
        }
        "CHARINDEX" | "POSITION" => match opt_arg(2)? {
            Some(start) => func("sf_charindex", vec![arg(0)?, arg(1)?, start]),
            None => func("instr", vec![arg(1)?, arg(0)?]),
        },
        "INITCAP" => func("sf_initcap", args()?),
        "SPLIT" => cast(func("string_split", vec![arg(0)?, arg(1)?]), custom_type("VARIANT")),
        "STRTOK" => {
            let delimiter = opt_arg(1)?.unwrap_or_else(|| string(" "));
            let part = opt_arg(2)?.unwrap_or_else(|| number(1));
            nullif_empty(func("list_extract", vec![func("sf_tokens", vec![arg(0)?, delimiter]), part]))
        }
        "STRTOK_TO_ARRAY" => {
            let delimiter = opt_arg(1)?.unwrap_or_else(|| string(" "));
            cast(func("sf_tokens", vec![arg(0)?, delimiter]), custom_type("VARIANT"))
        }
        "REPLACE" if args()?.len() == 2 => func("replace", vec![arg(0)?, arg(1)?, string("")]),
        "LPAD" | "RPAD" if args()?.len() == 2 => func(&name.to_lowercase(), vec![arg(0)?, arg(1)?, string(" ")]),
        "SPACE" => func("repeat", vec![string(" "), arg(0)?]),
        "CHAR" | "CHR" => func("chr", vec![arg(0)?]),
        "REGEXP_LIKE" | "RLIKE" => regexp_like(arg(0)?, arg(1)?, opt_arg(2)?),
        "REGEXP_REPLACE" => {
            let replacement = opt_arg(2)?.unwrap_or_else(|| string(""));
            // Occurrence 0 replaces every match; 1 only the first.
            let global = match opt_arg(4)?.as_ref().map(integer_literal) {
                None | Some(Some(0)) => true,
                Some(Some(1)) => false,
                // A later (or computed) occurrence: the engine's sf_regexp_replace.
                _ => {
                    let flags = opt_arg(5)?.unwrap_or_else(|| string(""));
                    let position = opt_arg(3)?.unwrap_or_else(|| number(1));
                    return Ok(Some(func(
                        "sf_regexp_replace",
                        vec![
                            cast(arg(0)?, DataType::Varchar(None)),
                            arg(1)?,
                            replacement,
                            cast(position, DataType::BigInt(None)),
                            cast(arg(4)?, DataType::BigInt(None)),
                            flags,
                        ],
                    )));
                }
            };
            let flags = format!("{}{}", if global { "g" } else { "" }, regexp_flags(opt_arg(5)?.as_ref()));
            match opt_arg(3)? {
                None => func("regexp_replace", vec![arg(0)?, arg(1)?, replacement, string(flags)]),
                Some(position) => {
                    let head = func(
                        "substr",
                        vec![arg(0)?, number(1), binop(position.clone(), BinaryOperator::Minus, number(1))],
                    );
                    let tail = func("substr", vec![arg(0)?, position]);
                    let replaced = func("regexp_replace", vec![tail, arg(1)?, replacement, string(flags)]);
                    binop(head, BinaryOperator::StringConcat, replaced)
                }
            }
        }
        "REGEXP_SUBSTR" => {
            let params = opt_arg(4)?;
            let extract = params.as_ref().and_then(string_literal).is_some_and(|p| p.contains('e'));
            let default_group = number(if extract { 1 } else { 0 });
            let group = opt_arg(5)?.unwrap_or(default_group);
            let position = opt_arg(2)?.unwrap_or_else(|| number(1));
            let occurrence = opt_arg(3)?.unwrap_or_else(|| number(1));
            let subject = func("substr", vec![arg(0)?, position]);
            let flags = string(regexp_flags(params.as_ref()));
            let all = func("regexp_extract_all", vec![subject, arg(1)?, group, flags]);
            nullif_empty(func("list_extract", vec![all, occurrence]))
        }
        "REGEXP_COUNT" => {
            let subject = func("substr", vec![arg(0)?, opt_arg(2)?.unwrap_or_else(|| number(1))]);
            let flags = string(regexp_flags(opt_arg(3)?.as_ref()));
            func("length", vec![func("regexp_extract_all", vec![subject, arg(1)?, number(0), flags])])
        }
        "SHA2" | "SHA2_HEX" => match opt_arg(1)? {
            None => func("sha256", vec![arg(0)?]),
            Some(bits) => {
                func("sf_sha2", vec![cast(arg(0)?, DataType::Varchar(None)), cast(bits, DataType::BigInt(None))])
            }
        },
        "SHA2_BINARY" => {
            let bits = opt_arg(1)?.unwrap_or_else(|| number(256));
            func(
                "unhex",
                vec![func("sf_sha2", vec![cast(arg(0)?, DataType::Varchar(None)), cast(bits, DataType::BigInt(None))])],
            )
        }
        "MD5_BINARY" => func("unhex", vec![func("md5", vec![arg(0)?])]),
        "SHA1_BINARY" => func("unhex", vec![func("sha1", vec![arg(0)?])]),
        "MD5_HEX" => func("md5", vec![arg(0)?]),
        "SHA1_HEX" => func("sha1", vec![arg(0)?]),
        "UUID_STRING" => cast(func("uuid", vec![]), DataType::Varchar(None)),
        "BASE64_ENCODE" => func("to_base64", vec![cast(arg(0)?, DataType::Blob(None))]),
        "BASE64_DECODE_STRING" | "TRY_BASE64_DECODE_STRING" => {
            try_if(&name, cast(func("from_base64", vec![arg(0)?]), DataType::Varchar(None)))
        }
        "BASE64_DECODE_BINARY" | "TRY_BASE64_DECODE_BINARY" => try_if(&name, func("from_base64", vec![arg(0)?])),
        "HEX_ENCODE" => func("upper", vec![func("hex", vec![arg(0)?])]),
        "HEX_DECODE_STRING" | "TRY_HEX_DECODE_STRING" => {
            try_if(&name, cast(func("unhex", vec![arg(0)?]), DataType::Varchar(None)))
        }
        "HEX_DECODE_BINARY" | "TRY_HEX_DECODE_BINARY" => try_if(&name, func("unhex", vec![arg(0)?])),
        "TO_BINARY" | "TRY_TO_BINARY" => {
            let format = opt_arg(1)?.as_ref().and_then(string_literal).map(str::to_uppercase);
            let converted = match format.as_deref() {
                None | Some("HEX") => func("unhex", vec![arg(0)?]),
                Some("BASE64") => func("from_base64", vec![arg(0)?]),
                Some("UTF-8" | "UTF8") => func("encode", vec![arg(0)?]),
                Some(other) => return Err(SqlError::Unsupported(format!("binary format {other}"))),
            };
            try_if(&name, converted)
        }

        // Numbers.
        "SQUARE" => func("power", vec![arg(0)?, number(2)]),
        "WIDTH_BUCKET" => func("sf_width_bucket", vec![arg(0)?, arg(1)?, arg(2)?, arg(3)?]),
        "HAVERSINE" => func("sf_haversine", vec![arg(0)?, arg(1)?, arg(2)?, arg(3)?]),
        "BOOLAND" | "BOOLOR" => {
            let op = if name == "BOOLAND" { BinaryOperator::And } else { BinaryOperator::Or };
            binop(cast(arg(0)?, DataType::Boolean), op, cast(arg(1)?, DataType::Boolean))
        }
        "BOOLXOR" => binop(cast(arg(0)?, DataType::Boolean), BinaryOperator::NotEq, cast(arg(1)?, DataType::Boolean)),
        "BOOLNOT" => Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(nested(cast(arg(0)?, DataType::Boolean))) },
        "CEIL" | "FLOOR" if args()?.len() == 2 => {
            let scale = func("power", vec![number(10), arg(1)?]);
            let scaled = binop(arg(0)?, BinaryOperator::Multiply, scale.clone());
            binop(func(&name.to_lowercase(), vec![scaled]), BinaryOperator::Divide, scale)
        }
        "ROUND" if args()?.len() == 3 => match opt_arg(2)?.as_ref().and_then(string_literal).map(str::to_uppercase) {
            Some(mode) if mode == "HALF_TO_EVEN" => func("round_even", vec![arg(0)?, arg(1)?]),
            _ => func("round", vec![arg(0)?, arg(1)?]),
        },
        "TRUNC" | "TRUNCATE" => match opt_arg(1)?.as_ref().and_then(word_or_string).and_then(|w| Part::parse(&w)) {
            Some(part) => crate::typing::tz_preserving(ctx, arg(0)?, |v| Ok(date_trunc(part, v)))?,
            None => return rename(f, "trunc"),
        },
        "RANDOM" => parse_expr("CAST((random() * 2 - 1) * 9.2e18 AS BIGINT)"),
        "UNIFORM" => {
            let (lo, hi) = (arg(0)?, arg(1)?);
            if integer_literal(&lo).is_some() && integer_literal(&hi).is_some() {
                let range = binop(binop(hi, BinaryOperator::Minus, lo.clone()), BinaryOperator::Plus, number(1));
                let offset = func("floor", vec![binop(func("random", vec![]), BinaryOperator::Multiply, range)]);
                cast(binop(offset, BinaryOperator::Plus, lo), DataType::BigInt(None))
            } else {
                let range = binop(hi, BinaryOperator::Minus, lo.clone());
                binop(lo, BinaryOperator::Plus, binop(func("random", vec![]), BinaryOperator::Multiply, range))
            }
        }
        "SEQ1" | "SEQ2" | "SEQ4" | "SEQ8" => {
            let n = ctx.types.next_sequence();
            if n >= crate::typing::SEQUENCES {
                return Err(SqlError::Unsupported(format!(
                    "more than {} SEQ calls in one statement",
                    crate::typing::SEQUENCES
                )));
            }
            func("nextval", vec![string(format!("__lf_seq{n}"))])
        }
        "COMPRESS" => func("sf_compress", vec![cast(arg(0)?, DataType::Varchar(None)), arg(1)?]),
        "DECOMPRESS_BINARY" | "DECOMPRESS_STRING" => {
            let bytes = func("sf_decompress", vec![arg(0)?, arg(1)?]);
            if name == "DECOMPRESS_STRING" { cast(bytes, DataType::Varchar(None)) } else { bytes }
        }
        "SF_COLLATE" => Expr::Collate { expr: Box::new(arg(0)?), collation: collation_name(&arg(1)?)? },
        "COLLATION" => string(""),
        "NTH_VALUE_FROM_LAST" => {
            // NTH_VALUE(x, n) FROM LAST: count from the end of the frame by
            // reversing the window's order and mirroring its frame.
            let mut call = f.clone();
            call.name = object_name(&["nth_value"]);
            mirror_window(&mut call);
            Expr::Function(call)
        }
        "HASH" => {
            cast(binop(func("hash", args()?), BinaryOperator::PGBitwiseShiftRight, number(1)), DataType::BigInt(None))
        }
        "BITAND" => binop(arg(0)?, BinaryOperator::BitwiseAnd, arg(1)?),
        "BITOR" => binop(arg(0)?, BinaryOperator::BitwiseOr, arg(1)?),
        "BITXOR" => func("xor", vec![arg(0)?, arg(1)?]),
        "BITNOT" => Expr::UnaryOp { op: UnaryOperator::BitwiseNot, expr: Box::new(nested(arg(0)?)) },
        "GETBIT" => binop(
            nested(binop(arg(0)?, BinaryOperator::PGBitwiseShiftRight, arg(1)?)),
            BinaryOperator::BitwiseAnd,
            number(1),
        ),
        "RTRIMMED_LENGTH" => func("length", vec![func("rtrim", vec![arg(0)?])]),
        "NORMAL" => {
            // Box-Muller from two uniform draws.
            let z = parse_expr("sqrt(-2 * ln(1 - random())) * cos(2 * pi() * random())");
            binop(arg(0)?, BinaryOperator::Plus, binop(arg(1)?, BinaryOperator::Multiply, z))
        }
        "ZIPF" => {
            // Inverse-transform sampling of a Zipf distribution over 1..n with exponent s.
            func(
                "sf_zipf",
                vec![cast(arg(0)?, DataType::Double(ExactNumberInfo::None)), cast(arg(1)?, DataType::BigInt(None))],
            )
        }
        "BITSHIFTLEFT" => binop(cast(arg(0)?, DataType::BigInt(None)), BinaryOperator::PGBitwiseShiftLeft, arg(1)?),
        "BITSHIFTRIGHT" => binop(cast(arg(0)?, DataType::BigInt(None)), BinaryOperator::PGBitwiseShiftRight, arg(1)?),
        "MD5_NUMBER_LOWER64" | "MD5_NUMBER_UPPER64" => {
            let start = if name.ends_with("LOWER64") { 17 } else { 1 };
            let hex = func("substr", vec![func("md5", vec![arg(0)?]), number(start), number(16)]);
            cast(binop(string("0x"), BinaryOperator::StringConcat, hex), custom_type("UBIGINT"))
        }
        "TO_NUMBER" | "TO_DECIMAL" | "TO_NUMERIC" | "TRY_TO_NUMBER" | "TRY_TO_DECIMAL" | "TRY_TO_NUMERIC" => {
            to_number(&name, args()?)?
        }
        "TO_DOUBLE" => cast(arg(0)?, DataType::Double(ExactNumberInfo::None)),
        "TRY_TO_DOUBLE" => try_cast(arg(0)?, DataType::Double(ExactNumberInfo::None)),
        "TO_BOOLEAN" => func("sf_to_boolean", vec![arg(0)?]),
        "TRY_TO_BOOLEAN" => func("sf_try_to_boolean", vec![arg(0)?]),

        // Dates and times.
        "DATEADD" | "TIMEADD" | "TIMESTAMPADD" => {
            let (part, amount) = (part_arg(&arg(0)?, &name)?, arg(1)?);
            crate::typing::tz_preserving(ctx, arg(2)?, |v| dateadd(part, amount, v))?
        }
        "ADD_MONTHS" => dateadd(Part::Month, arg(1)?, arg(0)?)?,
        "DATEDIFF" | "TIMEDIFF" | "TIMESTAMPDIFF" => {
            let part = part_arg(&arg(0)?, &name)?;
            let (from, to) =
                (crate::typing::utc_if_timestamp_tz(ctx, arg(1)?), crate::typing::utc_if_timestamp_tz(ctx, arg(2)?));
            let arg = |i: usize| -> Result<Expr, SqlError> { Ok(if i == 1 { from.clone() } else { to.clone() }) };
            if part == Part::Week {
                // Snowflake counts week boundaries (weeks start on Monday).
                let trunc = |e| func("date_trunc", vec![string("week"), e]);
                func("date_diff", vec![string("week"), trunc(arg(1)?), trunc(arg(2)?)])
            } else {
                func("date_diff", vec![string(part.duckdb_name()), arg(1)?, arg(2)?])
            }
        }
        "MONTHS_BETWEEN" => func("sf_months_between", vec![arg(0)?, arg(1)?]),
        "NEXT_DAY" => func("sf_next_day", vec![arg(0)?, arg(1)?]),
        "PREVIOUS_DAY" => func("sf_previous_day", vec![arg(0)?, arg(1)?]),
        "TIME_SLICE" => {
            let part = part_arg(&arg(2)?, &name)?;
            let Some((interval_fn, multiplier)) = part.interval_fn() else {
                return Err(SqlError::Unsupported(format!("TIME_SLICE by {}", part.duckdb_name())));
            };
            let amount =
                if multiplier == 1 { arg(1)? } else { binop(arg(1)?, BinaryOperator::Multiply, number(multiplier)) };
            let width = func(interval_fn, vec![cast(amount, DataType::Integer(None))]);
            let value = arg(0)?;
            let origin = cast(string("1970-01-01"), DataType::Timestamp(None, TimezoneInfo::None));
            let start = func(
                "time_bucket",
                vec![width.clone(), cast(value.clone(), DataType::Timestamp(None, TimezoneInfo::None)), origin],
            );
            let end = opt_arg(3)?.as_ref().and_then(string_literal).is_some_and(|s| s.eq_ignore_ascii_case("END"));
            let bucket = if end { binop(start, BinaryOperator::Plus, width) } else { start };
            func("sf_cast_like", vec![bucket, value])
        }
        "DATE_TRUNC" => {
            let part = part_arg(&arg(0)?, &name)?;
            crate::typing::tz_preserving(ctx, arg(1)?, |v| Ok(date_trunc(part, v)))?
        }
        "DATE_PART" => date_part(part_arg(&arg(0)?, &name)?, arg(1)?),
        "LAST_DAY" => match opt_arg(1)?.as_ref().and_then(word_or_string).and_then(|w| Part::parse(&w)) {
            None | Some(Part::Month) => func("last_day", vec![arg(0)?]),
            Some(part) => {
                let start = func("date_trunc", vec![string(part.duckdb_name()), arg(0)?]);
                let next = dateadd(part, number(1), start)?;
                cast(binop(next, BinaryOperator::Minus, func("to_days", vec![number(1)])), DataType::Date)
            }
        },
        "DAYNAME" => func("left", vec![func("dayname", vec![arg(0)?]), number(3)]),
        "MONTHNAME" => func("left", vec![func("monthname", vec![arg(0)?]), number(3)]),
        "DAYOFWEEKISO" => func("isodow", vec![arg(0)?]),
        "WEEKISO" => func("weekofyear", vec![arg(0)?]),
        "YEAROFWEEK" | "YEAROFWEEKISO" => func("isoyear", vec![arg(0)?]),
        // Out-of-range parts carry over, as in Snowflake: DATE_FROM_PARTS(2024, 2, 30) is 2024-03-01.
        "DATE_FROM_PARTS" | "DATEFROMPARTS" => cast(date_from_parts(arg(0)?, arg(1)?, arg(2)?), DataType::Date),
        "TIME_FROM_PARTS" | "TIMEFROMPARTS" => {
            let midnight = cast(string("00:00:00"), DataType::Time(None, TimezoneInfo::None));
            binop(midnight, BinaryOperator::Plus, seconds_interval(arg(0)?, arg(1)?, with_nanos(arg(2)?, opt_arg(3)?)))
        }
        "TIMESTAMP_FROM_PARTS" | "TIMESTAMPFROMPARTS" | "TIMESTAMP_NTZ_FROM_PARTS" | "TIMESTAMPNTZFROMPARTS" => {
            match args()?.len() {
                2 => binop(arg(0)?, BinaryOperator::Plus, arg(1)?),
                _ => {
                    let day = date_from_parts(arg(0)?, arg(1)?, arg(2)?);
                    let time = seconds_interval(arg(3)?, arg(4)?, with_nanos(arg(5)?, opt_arg(6)?));
                    cast(binop(day, BinaryOperator::Plus, time), DataType::Timestamp(None, TimezoneInfo::None))
                }
            }
        }
        "TO_DATE" | "DATE" | "TRY_TO_DATE" => {
            let converted = match opt_arg(1)? {
                Some(format) => cast(strptime(arg(0)?, &format, &name)?, DataType::Date),
                None => func("sf_to_date", vec![arg(0)?, crate::typing::session_timezone(ctx)]),
            };
            try_if(&name, converted)
        }
        "TO_TIME" | "TIME" | "TRY_TO_TIME" => {
            let converted = match opt_arg(1)? {
                Some(format) => cast(strptime(arg(0)?, &format, &name)?, DataType::Time(None, TimezoneInfo::None)),
                None => cast(arg(0)?, DataType::Time(None, TimezoneInfo::None)),
            };
            try_if(&name, converted)
        }
        "TO_TIMESTAMP"
        | "TO_TIMESTAMP_NTZ"
        | "TO_TIMESTAMP_LTZ"
        | "TO_TIMESTAMP_TZ"
        | "TRY_TO_TIMESTAMP"
        | "TRY_TO_TIMESTAMP_NTZ"
        | "TRY_TO_TIMESTAMP_LTZ"
        | "TRY_TO_TIMESTAMP_TZ" => {
            let tz_result = name.ends_with("_TZ") && !name.ends_with("_NTZ") && !name.ends_with("_LTZ")
                || matches!(name.as_str(), "TO_TIMESTAMP" | "TRY_TO_TIMESTAMP")
                    && ctx.timestamp_type_mapping == crate::TimestampMapping::Tz;
            let with_tz = tz_result
                || name.ends_with("_LTZ")
                || matches!(name.as_str(), "TO_TIMESTAMP" | "TRY_TO_TIMESTAMP")
                    && ctx.timestamp_type_mapping != crate::TimestampMapping::Ntz;
            // TIMESTAMP_TZ keeps the text's own offset.
            if tz_result && opt_arg(1)?.is_none() {
                return Ok(Some(try_if(&name, crate::typing::to_timestamp_tz(ctx, arg(0)?))));
            }
            let target = if with_tz { "sf_ltz" } else { "sf_to_timestamp" };
            let converted = match opt_arg(1)? {
                Some(scale) if integer_literal(&scale).is_some() => {
                    let divisor = 10_f64.powi(integer_literal(&scale).unwrap() as i32);
                    let seconds = binop(
                        cast(arg(0)?, DataType::Double(ExactNumberInfo::None)),
                        BinaryOperator::Divide,
                        number(divisor),
                    );
                    func(target, vec![seconds])
                }
                Some(format) => func(target, vec![strptime(arg(0)?, &format, &name)?]),
                None if with_tz => func(target, vec![arg(0)?]),
                None => func(target, vec![arg(0)?, crate::typing::session_timezone(ctx)]),
            };
            try_if(&name, converted)
        }
        "TO_CHAR" | "TO_VARCHAR" => match opt_arg(1)? {
            Some(format) => to_char(arg(0)?, &format, &name)?,
            None => crate::typing::text(ctx, arg(0)?),
        },
        "CURRENT_TIMESTAMP" | "GETDATE" | "SYSTIMESTAMP" | "LOCALTIMESTAMP" => func("get_current_timestamp", vec![]),
        // SYSDATE is the UTC wall clock whatever the session time zone.
        "SYSDATE" => func("make_timestamp", vec![func("epoch_us", vec![func("get_current_timestamp", vec![])])]),
        "CURRENT_TIME" | "LOCALTIME" => {
            // No direct TIMESTAMPTZ → TIME cast without ICU.
            let utc = cast(func("get_current_timestamp", vec![]), DataType::Timestamp(None, TimezoneInfo::None));
            cast(utc, DataType::Time(None, TimezoneInfo::None))
        }
        "CONVERT_TIMEZONE" => match args()?.len() {
            3 => func(
                "sf_convert_timezone",
                vec![arg(0)?, arg(1)?, cast(arg(2)?, DataType::Timestamp(None, TimezoneInfo::None))],
            ),
            // The instant, as a TIMESTAMP_TZ with the target zone's offset.
            _ => func("sf_tz_in_zone", vec![func("sf_tz_instant", vec![arg(1)?]), arg(0)?]),
        },

        // Semi-structured data.
        "PARSE_JSON" => func("sf_json_variant", vec![arg(0)?]),
        "TRY_PARSE_JSON" => func("try", vec![func("sf_json_variant", vec![arg(0)?])]),
        "CHECK_JSON" => case(vec![(func("json_valid", vec![arg(0)?]), null())], Some(string("Invalid JSON"))),
        "TO_VARIANT" | "TO_OBJECT" => cast(arg(0)?, custom_type("VARIANT")),
        "TO_ARRAY" => {
            let value = cast(arg(0)?, custom_type("VARIANT"));
            let is_array = func("regexp_matches", vec![func("variant_typeof", vec![value.clone()]), string("^ARRAY")]);
            let wrapped = cast(
                Expr::Array(sqlparser::ast::Array { elem: vec![value.clone()], named: false }),
                custom_type("VARIANT"),
            );
            case(vec![(is_null(value.clone()), null()), (is_array, value)], Some(wrapped))
        }
        "TO_JSON" => cast(cast(cast(arg(0)?, custom_type("VARIANT")), custom_type("JSON")), DataType::Varchar(None)),
        "OBJECT_CONSTRUCT" | "OBJECT_CONSTRUCT_KEEP_NULL" => {
            let mut args = args()?;
            if name == "OBJECT_CONSTRUCT_KEEP_NULL" {
                // SQL NULL values become JSON nulls instead of being dropped.
                for value in args.iter_mut().skip(1).step_by(2) {
                    *value = func(
                        "coalesce",
                        vec![
                            cast(std::mem::replace(value, null()), custom_type("VARIANT")),
                            func("sf_json_null", vec![]),
                        ],
                    );
                }
            }
            if args.is_empty() {
                variant::empty_object()
            } else {
                let object = func("json_object", args);
                // Snowflake drops keys whose value is NULL; a JSON merge patch does the same.
                let object = if name == "OBJECT_CONSTRUCT" {
                    func("json_merge_patch", vec![string("{}"), object])
                } else {
                    object
                };
                cast(object, custom_type("VARIANT"))
            }
        }
        "OBJECT_INSERT" => {
            let patch = func("json_object", vec![arg(1)?, arg(2)?]);
            let base = cast(cast(arg(0)?, custom_type("VARIANT")), custom_type("JSON"));
            cast(func("json_merge_patch", vec![base, patch]), custom_type("VARIANT"))
        }
        "OBJECT_KEYS" => {
            cast(func("variant_keys", vec![cast(arg(0)?, custom_type("VARIANT"))]), custom_type("VARIANT"))
        }
        "ARRAY_CONSTRUCT" | "ARRAY_CONSTRUCT_COMPACT" => {
            let mut args = args()?;
            if args.is_empty() {
                variant::empty_array()
            } else {
                if name == "ARRAY_CONSTRUCT_COMPACT" {
                    args.retain(|a| !matches!(a, Expr::Value(v) if v.value == Value::Null));
                }
                cast(func("json_array", args), custom_type("VARIANT"))
            }
        }
        "ARRAY_SIZE" => func("sf_array_size", vec![arg(0)?]),
        "ARRAY_CONTAINS" => {
            let haystack = cast(cast(arg(1)?, custom_type("VARIANT")), custom_type("JSON"));
            let needle = cast(cast(arg(0)?, custom_type("VARIANT")), custom_type("JSON"));
            func("json_contains", vec![haystack, needle])
        }
        "ARRAY_TO_STRING" => func("sf_array_to_string", vec![arg(0)?, arg(1)?]),
        "ARRAY_APPEND" | "ARRAY_PREPEND" => {
            let list = func("sf_variant_array", vec![arg(0)?]);
            let item = cast(arg(1)?, custom_type("VARIANT"));
            let f = if name == "ARRAY_APPEND" { "list_append" } else { "list_prepend" };
            let args = if name == "ARRAY_APPEND" { vec![list, item] } else { vec![item, list] };
            cast(func(f, args), custom_type("VARIANT"))
        }
        "ARRAY_CAT" => cast(
            func("list_concat", vec![func("sf_variant_array", vec![arg(0)?]), func("sf_variant_array", vec![arg(1)?])]),
            custom_type("VARIANT"),
        ),
        "ARRAY_SLICE" => {
            // Snowflake: 0-based, end exclusive, negative counts from the end.
            // DuckDB: 1-based, end inclusive, negative counts from the end too.
            let (from, to) = (arg(1)?, arg(2)?);
            let negative = |e: &Expr| binop(e.clone(), BinaryOperator::Lt, number(0));
            let from = case(vec![(negative(&from), from.clone())], Some(binop(from, BinaryOperator::Plus, number(1))));
            let to = case(vec![(negative(&to), binop(to.clone(), BinaryOperator::Minus, number(1)))], Some(to));
            cast(func("list_slice", vec![func("sf_variant_array", vec![arg(0)?]), from, to]), custom_type("VARIANT"))
        }
        "ARRAY_DISTINCT" => func("sf_array_distinct", vec![arg(0)?]),
        "ARRAY_COMPACT" => func("sf_array_compact", vec![arg(0)?]),
        "ARRAY_POSITION" => func("sf_array_position", vec![arg(0)?, arg(1)?]),
        "ARRAY_REMOVE" => func("sf_array_remove", vec![arg(0)?, arg(1)?]),
        "ARRAY_INTERSECTION" => func("sf_array_intersection", vec![arg(0)?, arg(1)?]),
        "ARRAY_EXCEPT" => func("sf_array_except", vec![arg(0)?, arg(1)?]),
        "ARRAYS_OVERLAP" => func("sf_arrays_overlap", vec![arg(0)?, arg(1)?]),
        "ARRAY_INSERT" => func("sf_array_insert", vec![arg(0)?, arg(1)?, arg(2)?]),
        "ARRAY_FLATTEN" => func("sf_array_flatten", vec![arg(0)?]),
        "ARRAY_REVERSE" => {
            cast(func("list_reverse", vec![func("sf_variant_array", vec![arg(0)?])]), custom_type("VARIANT"))
        }
        "ARRAY_SORT" => {
            let ascending =
                opt_arg(1)?.is_none_or(|e| !matches!(&e, Expr::Value(v) if v.value == Value::Boolean(false)));
            let nulls_first =
                opt_arg(2)?.is_some_and(|e| matches!(&e, Expr::Value(v) if v.value == Value::Boolean(true)));
            let order = string(if ascending { "ASC" } else { "DESC" });
            let nulls = string(if nulls_first { "NULLS FIRST" } else { "NULLS LAST" });
            cast(func("list_sort", vec![func("sf_variant_array", vec![arg(0)?]), order, nulls]), custom_type("VARIANT"))
        }
        "ARRAY_MAX" | "ARRAY_MIN" => {
            let f = if name == "ARRAY_MAX" { "list_max" } else { "list_min" };
            func(f, vec![func("sf_variant_array", vec![arg(0)?])])
        }
        "ARRAY_GENERATE_RANGE" => {
            let step = opt_arg(2)?.unwrap_or_else(|| number(1));
            cast(func("range", vec![arg(0)?, arg(1)?, step]), custom_type("VARIANT"))
        }
        "GET_IGNORE_CASE" => func("sf_get_ignore_case", vec![arg(0)?, arg(1)?]),
        "OBJECT_DELETE" => {
            let mut args = args()?.into_iter();
            let object = func("sf_object_json", vec![args.next().ok_or_else(|| SqlError::Arguments(name.clone()))?]);
            // A merge patch with NULL values removes those keys.
            let removals = args.flat_map(|key| [key, null()]).collect::<Vec<_>>();
            cast(func("json_merge_patch", vec![object, func("json_object", removals)]), custom_type("VARIANT"))
        }
        "OBJECT_PICK" => {
            let mut args = args()?.into_iter();
            let object = func("sf_object_json", vec![args.next().ok_or_else(|| SqlError::Arguments(name.clone()))?]);
            let picked = args
                .flat_map(|key| {
                    let path = binop(
                        binop(string("$.\""), BinaryOperator::StringConcat, key.clone()),
                        BinaryOperator::StringConcat,
                        string("\""),
                    );
                    [key, func("json_extract", vec![object.clone(), path])]
                })
                .collect::<Vec<_>>();
            let object = func("json_merge_patch", vec![string("{}"), func("json_object", picked)]);
            cast(object, custom_type("VARIANT"))
        }
        "JSON_EXTRACT_PATH_TEXT" => {
            let path = arg(1)?;
            let Some(path) = string_literal(&path) else {
                return Err(SqlError::Unsupported("JSON_EXTRACT_PATH_TEXT with a non-literal path".into()));
            };
            let value = func("sf_json_variant", vec![arg(0)?]);
            func("sf_to_varchar", vec![variant::extract_path(value, path)?])
        }
        // Higher-order functions; lambdas are rewritten to DuckDB's `lambda x: ...` syntax.
        "FILTER" => {
            cast(func("list_filter", vec![func("sf_variant_array", vec![arg(0)?]), arg(1)?]), custom_type("VARIANT"))
        }
        "TRANSFORM" => {
            cast(func("list_transform", vec![func("sf_variant_array", vec![arg(0)?]), arg(1)?]), custom_type("VARIANT"))
        }
        "REDUCE" => func("list_reduce", vec![func("sf_variant_array", vec![arg(0)?]), arg(2)?, arg(1)?]),
        "GET" => match variant::literal_key(&arg(1)?) {
            Some(key) => variant::extract(arg(0)?, key),
            None => func("sf_get", vec![arg(0)?, arg(1)?]),
        },
        "GET_PATH" => {
            let path = arg(1)?;
            let Some(path) = string_literal(&path) else {
                return Err(SqlError::Unsupported("GET_PATH with a non-literal path".into()));
            };
            variant::extract_path(arg(0)?, path)?
        }
        "IS_NULL_VALUE" => {
            let value = cast(arg(0)?, custom_type("VARIANT"));
            case(vec![(is_null(value.clone()), null())], Some(func("sf_is_json_null", vec![value])))
        }
        "STRIP_NULL_VALUE" => {
            let value = cast(arg(0)?, custom_type("VARIANT"));
            case(vec![(func("sf_is_json_null", vec![value.clone()]), null())], Some(value))
        }
        "TYPEOF" => func("sf_typeof", vec![arg(0)?]),
        "SYSTEM$TYPEOF" => crate::typing::system_typeof(ctx, arg(0)?),
        "IS_OBJECT" | "IS_ARRAY" | "IS_INTEGER" | "IS_DECIMAL" | "IS_DOUBLE" | "IS_REAL" | "IS_BOOLEAN" | "IS_CHAR"
        | "IS_VARCHAR" | "IS_DATE" | "IS_TIMESTAMP_NTZ" | "IS_TIMESTAMP_LTZ" | "IS_BINARY" => {
            let types: &[&str] = match &name[3..] {
                "DECIMAL" => &["INTEGER", "DECIMAL"],
                "DOUBLE" | "REAL" => &["INTEGER", "DECIMAL", "DOUBLE"],
                "CHAR" | "VARCHAR" => &["VARCHAR"],
                other => &[other][..],
            };
            let typeof_ = func("sf_typeof", vec![arg(0)?]);
            Expr::InList { expr: Box::new(typeof_), list: types.iter().map(|t| string(*t)).collect(), negated: false }
        }
        "AS_VARCHAR" | "AS_CHAR" => as_type(arg(0)?, &["VARCHAR"], DataType::Varchar(None)),
        "AS_INTEGER" => as_type(arg(0)?, &["INTEGER"], DataType::BigInt(None)),
        "AS_NUMBER" | "AS_DECIMAL" | "AS_NUMERIC" => {
            let precision = opt_arg(1)?.as_ref().and_then(integer_literal).unwrap_or(38) as u64;
            let scale = opt_arg(2)?.as_ref().and_then(integer_literal).unwrap_or(0);
            as_type(
                arg(0)?,
                &["INTEGER", "DECIMAL"],
                DataType::Decimal(ExactNumberInfo::PrecisionAndScale(precision, scale)),
            )
        }
        "AS_DOUBLE" | "AS_REAL" => {
            as_type(arg(0)?, &["INTEGER", "DECIMAL", "DOUBLE"], DataType::Double(ExactNumberInfo::None))
        }
        "AS_BOOLEAN" => as_type(arg(0)?, &["BOOLEAN"], DataType::Boolean),
        "AS_OBJECT" => as_type(arg(0)?, &["OBJECT"], custom_type("VARIANT")),
        "AS_ARRAY" => as_type(arg(0)?, &["ARRAY"], custom_type("VARIANT")),
        "AS_DATE" => as_type(arg(0)?, &["DATE"], DataType::Date),
        "AS_TIME" => as_type(arg(0)?, &["TIME"], DataType::Time(None, TimezoneInfo::None)),
        "AS_TIMESTAMP_NTZ" => as_type(arg(0)?, &["TIMESTAMP_NTZ"], DataType::Timestamp(None, TimezoneInfo::None)),
        "AS_TIMESTAMP_LTZ" | "AS_TIMESTAMP_TZ" => {
            as_type(arg(0)?, &["TIMESTAMP_LTZ"], DataType::Timestamp(None, TimezoneInfo::WithTimeZone))
        }
        "AS_BINARY" => as_type(arg(0)?, &["BINARY"], DataType::Blob(None)),
        "ARRAYS_ZIP" => {
            // One object per position: {"$1": a1[i], "$2": a2[i], ...}, padded with nulls.
            let arrays: Vec<Expr> = args()?.into_iter().map(|a| func("sf_variant_array", vec![a])).collect();
            let longest = func(
                "greatest",
                arrays.iter().map(|a| func("coalesce", vec![func("len", vec![a.clone()]), number(0)])).collect(),
            );
            let fields = arrays
                .iter()
                .enumerate()
                .flat_map(|(i, a)| {
                    let item = func(
                        "list_extract",
                        vec![
                            a.clone(),
                            binop(Expr::Identifier(sqlparser::ast::Ident::new("__i")), BinaryOperator::Plus, number(1)),
                        ],
                    );
                    [string(format!("${}", i + 1)), cast(item, custom_type("JSON"))]
                })
                .collect();
            let lambda = Expr::Lambda(sqlparser::ast::LambdaFunction {
                params: sqlparser::ast::OneOrManyWithParens::One(sqlparser::ast::LambdaFunctionParameter {
                    name: sqlparser::ast::Ident::new("__i"),
                    data_type: None,
                }),
                body: Box::new(func("json_object", fields)),
                syntax: sqlparser::ast::LambdaSyntax::LambdaKeyword,
            });
            cast(func("list_transform", vec![func("range", vec![longest]), lambda]), custom_type("VARIANT"))
        }

        // Aggregates.
        "AVG" => return Ok(crate::typing::avg(&ctx.types, f, arg(0)?)),
        "MEDIAN" => return Ok(crate::typing::median(&ctx.types, f, arg(0)?)),
        "VARIANCE_POP" => return rename(f, "var_pop"),
        "VARIANCE_SAMP" => return rename(f, "var_samp"),
        "BOOLXOR_AGG" => {
            let mut trues = f.clone();
            trues.name = object_name(&["sum"]);
            trues.args = function_args(vec![case(vec![(arg(0)?, number(1))], Some(number(0)))], None);
            binop(Expr::Function(trues), BinaryOperator::Eq, number(1))
        }
        "HASH_AGG" => {
            let mut agg = f.clone();
            agg.name = object_name(&["bit_xor"]);
            agg.args = function_args(vec![func("hash", args()?)], distinct_of(f));
            cast(binop(Expr::Function(agg), BinaryOperator::PGBitwiseShiftRight, number(1)), DataType::BigInt(None))
        }
        "ARRAY_UNION_AGG" => {
            let mut agg = f.clone();
            agg.name = object_name(&["list"]);
            agg.args = function_args(vec![func("sf_variant_array", vec![arg(0)?])], None);
            func("sf_array_distinct", vec![cast(func("flatten", vec![Expr::Function(agg)]), custom_type("VARIANT"))])
        }
        "APPROX_TOP_K" => {
            // [[value, count], ...] for the k most frequent values.
            let k = opt_arg(1)?.unwrap_or_else(|| number(1));
            let mut histogram = f.clone();
            histogram.name = object_name(&["histogram"]);
            histogram.args = function_args(vec![arg(0)?], None);
            let ranked =
                parse_expr("list_transform(map_entries(__h), lambda __e: struct_pack(c := __e.value, v := __e.key))");
            let ranked = replace_identifier(ranked, "__h", Expr::Function(histogram));
            let top = func("list_slice", vec![func("list_reverse_sort", vec![ranked]), number(1), k]);
            let pairs =
                parse_expr("list_transform(__top, lambda __e: [CAST(__e.v AS VARIANT), CAST(__e.c AS VARIANT)])");
            cast(replace_identifier(pairs, "__top", top), custom_type("VARIANT"))
        }
        "BITAND_AGG" => return rename(f, "bit_and"),
        "BITOR_AGG" => return rename(f, "bit_or"),
        "BITXOR_AGG" => return rename(f, "bit_xor"),
        "SKEW" => return rename(f, "skewness"),
        "HLL" | "APPROX_COUNT_DISTINCT" => return rename(f, "approx_count_distinct"),
        "COUNT" if matches!(&f.args, FunctionArguments::List(l) if l.args.len() > 1) => {
            // COUNT(DISTINCT a, b): rows where every argument is non-NULL.
            let args = args()?;
            let all_present = and_all(args.iter().cloned().map(is_not_null).collect());
            let tuple = func("row", args);
            let mut agg = std::mem::replace(f, function_shell());
            agg.name = object_name(&["count"]);
            agg.args = function_args(vec![case(vec![(all_present, tuple)], None)], distinct_of(&agg));
            Expr::Function(agg)
        }
        "RATIO_TO_REPORT" => {
            let mut total = f.clone();
            total.name = object_name(&["sum"]);
            crate::typing::divide(&ctx.types, arg(0)?, Expr::Function(total))
        }
        "CONDITIONAL_TRUE_EVENT" => {
            let mut running = f.clone();
            running.name = object_name(&["sum"]);
            running.args = function_args(vec![case(vec![(arg(0)?, number(1))], Some(number(0)))], None);
            running_frame(&mut running);
            Expr::Function(running)
        }
        "BOOLAND_AGG" => return rename(f, "bool_and"),
        "BOOLOR_AGG" => return rename(f, "bool_or"),
        "APPROX_PERCENTILE" => return rename(f, "approx_quantile"),
        "ARRAY_UNIQUE_AGG" => {
            let mut agg = f.clone();
            agg.name = object_name(&["ARRAY_AGG"]);
            agg.args = function_args(vec![arg(0)?], Some(DuplicateTreatment::Distinct));
            return rewrite(&mut agg, ctx);
        }
        "ARRAY_AGG" | "ARRAYAGG" => {
            let value = arg(0)?;
            let mut agg = std::mem::replace(f, function_shell());
            agg.name = object_name(&["array_agg"]);
            move_within_group(&mut agg);
            let not_null = is_not_null(value);
            agg.filter = Some(Box::new(match agg.filter.take() {
                Some(existing) => binop(*existing, BinaryOperator::And, not_null),
                None => not_null,
            }));
            let windowed = agg.over.is_some();
            let result = cast(Expr::Function(agg), custom_type("VARIANT"));
            if windowed { result } else { func("coalesce", vec![result, variant::empty_array()]) }
        }
        "OBJECT_AGG" => {
            // list() of key/value entries, skipping NULL values, then a JSON object.
            let (key, value) = (arg(0)?, arg(1)?);
            let entry =
                func("row", vec![cast(key, DataType::Varchar(None)), func("sf_object_json", vec![value.clone()])]);
            let mut agg = std::mem::replace(f, function_shell());
            agg.name = object_name(&["list"]);
            agg.args = function_args(vec![entry], None);
            agg.filter = Some(Box::new(is_not_null(value)));
            let object = func("to_json", vec![func("map_from_entries", vec![Expr::Function(agg)])]);
            cast(object, custom_type("VARIANT"))
        }
        "LISTAGG" => {
            let mut agg = std::mem::replace(f, function_shell());
            agg.name = object_name(&["string_agg"]);
            if let FunctionArguments::List(list) = &mut agg.args
                && list.args.len() == 1
            {
                list.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(string(""))));
            }
            move_within_group(&mut agg);
            Expr::Function(agg)
        }

        // Session context.
        "CURRENT_USER" => string(&ctx.user),
        "CURRENT_ROLE" => optional_string(ctx.role.as_deref()),
        "CURRENT_WAREHOUSE" => optional_string(ctx.warehouse.as_deref()),
        "CURRENT_DATABASE" => optional_string(ctx.database.as_deref()),
        "CURRENT_SCHEMA" => optional_string(ctx.database.as_ref().and(ctx.schema.as_deref())),
        "CURRENT_SCHEMAS" => {
            let schemas = match (&ctx.database, &ctx.schema) {
                (Some(db), Some(schema)) => format!("[\"{db}.{schema}\"]"),
                _ => "[]".to_owned(),
            };
            string(schemas)
        }
        "CURRENT_ACCOUNT" | "CURRENT_ACCOUNT_NAME" => string(ctx.account.to_uppercase()),
        "CURRENT_ORGANIZATION_NAME" => string("LOCALFLAKE"),
        "CURRENT_REGION" => string("LOCAL"),
        "CURRENT_VERSION" => string(env!("CARGO_PKG_VERSION")),
        "CURRENT_CLIENT" => string("localflake"),
        "CURRENT_SESSION" => string(ctx.session_id.to_string()),
        "CURRENT_IP_ADDRESS" => string("127.0.0.1"),
        "CURRENT_ROLE_TYPE" => string("ROLE"),
        "CURRENT_AVAILABLE_ROLES" => string(format!("[\"{}\"]", ctx.role.as_deref().unwrap_or("ACCOUNTADMIN"))),
        "CURRENT_SECONDARY_ROLES" => string("{\"roles\":\"\",\"value\":\"\"}"),
        "CURRENT_TRANSACTION" => null(),
        "LAST_QUERY_ID" => {
            let n = opt_arg(0)?.as_ref().and_then(integer_literal).unwrap_or(-1);
            optional_string(ctx.last_query_id(n))
        }
        "IDENTIFIER" => match string_literal(&arg(0)?) {
            Some(name) => identifier_expr(name),
            None => return Err(SqlError::Unsupported("IDENTIFIER with a non-literal argument".into())),
        },
        _ => return Ok(None),
    };
    Ok(Some(expr))
}

/// `DATEADD(part, n, value)`. DATE inputs stay DATEs for date-level parts.
pub fn dateadd(part: Part, amount: Expr, value: Expr) -> Result<Expr, SqlError> {
    let Some((interval_fn, multiplier)) = part.interval_fn() else {
        if part == Part::Nanosecond {
            let micros = binop(amount, BinaryOperator::Divide, number(1000));
            return Ok(binop(
                value,
                BinaryOperator::Plus,
                func("to_microseconds", vec![cast(micros, DataType::BigInt(None))]),
            ));
        }
        return Err(SqlError::Unsupported(format!("DATEADD with date part {part:?}")));
    };
    let amount = cast(amount, DataType::BigInt(None));
    let amount = if multiplier == 1 { amount } else { binop(amount, BinaryOperator::Multiply, number(multiplier)) };
    let added = binop(value.clone(), BinaryOperator::Plus, func(interval_fn, vec![amount]));
    Ok(if part.is_date_level() { func("sf_cast_like", vec![added, value]) } else { added })
}

fn date_trunc(part: Part, value: Expr) -> Expr {
    func("sf_cast_like", vec![func("date_trunc", vec![string(part.duckdb_name()), value.clone()]), value])
}

fn part_arg(expr: &Expr, function: &str) -> Result<Part, SqlError> {
    word_or_string(expr)
        .and_then(|w| Part::parse(&w))
        .ok_or_else(|| SqlError::Unsupported(format!("date part {expr} in {function}")))
}

fn strptime(value: Expr, format: &Expr, function: &str) -> Result<Expr, SqlError> {
    let Some(format) = string_literal(format) else {
        return Err(SqlError::Unsupported(format!("non-literal format in {function}")));
    };
    let try_ = function.starts_with("TRY_");
    Ok(match convert_format(format) {
        Some(converted) => func(
            if try_ { "try_strptime" } else { "strptime" },
            vec![cast(value, DataType::Varchar(None)), string(converted)],
        ),
        None => value,
    })
}

fn to_char(value: Expr, format: &Expr, function: &str) -> Result<Expr, SqlError> {
    let Some(format) = string_literal(format) else {
        return Err(SqlError::Unsupported(format!("non-literal format in {function}")));
    };
    if is_number_format(format) {
        return Ok(func("sf_format_number", vec![cast(value, DataType::Varchar(None)), string(format)]));
    }
    let is_datetime = ["YY", "MM", "DD", "HH", "MI", "SS", "MON", "DY", "FF"]
        .iter()
        .any(|token| format.to_uppercase().contains(token));
    if is_datetime {
        return Ok(match convert_format(format) {
            Some(converted) => func("strftime", vec![value, string(converted)]),
            None => func("sf_to_varchar", vec![value]),
        });
    }
    // Numeric formats like '999,999.00': honour the number of decimals.
    let decimals =
        format.split_once('.').map_or(0, |(_, frac)| frac.chars().filter(|c| matches!(c, '0' | '9')).count());
    let grouped = format.contains(',');
    let pattern = if grouped { format!("{{:,.{decimals}f}}") } else { format!("{{:.{decimals}f}}") };
    Ok(func("format", vec![string(pattern), cast(value, DataType::Double(ExactNumberInfo::None))]))
}

/// Whether a TO_CHAR format is a numeric format model (`999,999.00`, `FM$9.99`, ...).
fn is_number_format(format: &str) -> bool {
    let upper = format.to_uppercase();
    let rest = upper.replacen("FM", "", 1).replace("MI", "").replace("EEEE", "");
    !rest.is_empty() && rest.chars().all(|c| matches!(c, '9' | '0' | '.' | ',' | 'D' | 'G' | '$' | 'S' | 'X' | ' '))
}

fn to_number(name: &str, args: Vec<Expr>) -> Result<Expr, SqlError> {
    let mut args = args.into_iter().peekable();
    let mut value = args.next().ok_or_else(|| SqlError::Arguments(name.to_owned()))?;
    // TO_NUMBER(text, format[, precision, scale]): drop the format's decoration first.
    if let Some(format) = args.peek().and_then(string_literal).map(str::to_owned) {
        args.next();
        value = func("sf_number_text", vec![cast(value, DataType::Varchar(None)), string(format)]);
    }
    let numbers: Vec<i64> = args.filter_map(|a| integer_literal(&a)).collect();
    let (precision, scale) = match numbers.as_slice() {
        [p, s, ..] => (*p as u64, *s),
        [p] => (*p as u64, 0),
        [] => (38, 0),
    };
    let wide = DataType::Decimal(ExactNumberInfo::PrecisionAndScale(38, 12));
    let target = if scale == 0 {
        DataType::BigInt(None)
    } else {
        DataType::Decimal(ExactNumberInfo::PrecisionAndScale(precision, scale))
    };
    let try_ = name.starts_with("TRY_");
    let as_decimal = if try_ { try_cast(value, wide) } else { cast(value, wide) };
    let rounded = func("round", vec![as_decimal, number(scale)]);
    Ok(if try_ { try_cast(rounded, target) } else { cast(rounded, target) })
}

fn decode(args: Vec<Expr>, name: &str) -> Result<Expr, SqlError> {
    let mut args = args.into_iter();
    let subject = args.next().ok_or_else(|| SqlError::Arguments(name.to_owned()))?;
    let rest: Vec<Expr> = args.collect();
    let (pairs, remainder) = rest.as_chunks::<2>();
    let conditions = pairs
        .iter()
        .map(|[search, result]| {
            let matches = Expr::IsNotDistinctFrom(Box::new(nested(subject.clone())), Box::new(nested(search.clone())));
            (matches, result.clone())
        })
        .collect();
    let default = remainder.first().cloned();
    Ok(case(conditions, default))
}

fn regexp_like(subject: Expr, pattern: Expr, flags: Option<Expr>) -> Expr {
    let flags = regexp_flags(flags.as_ref());
    if flags.is_empty() {
        func("regexp_full_match", vec![subject, pattern])
    } else {
        func("regexp_full_match", vec![subject, pattern, string(flags)])
    }
}

/// Snowflake regex parameters (`c`, `i`, `m`, `e`, `s`) → DuckDB/RE2 options.
fn regexp_flags(flags: Option<&Expr>) -> String {
    flags
        .and_then(string_literal)
        .map_or_else(String::new, |f| f.chars().filter(|c| matches!(c, 'c' | 'i' | 'm' | 's')).collect())
}

fn as_type(value: Expr, types: &[&str], target: DataType) -> Expr {
    let typeof_ = func("sf_typeof", vec![value.clone()]);
    let matches =
        Expr::InList { expr: Box::new(typeof_), list: types.iter().map(|t| string(*t)).collect(), negated: false };
    case(vec![(matches, cast(value, target))], Some(null()))
}

fn with_nanos(seconds: Expr, nanos: Option<Expr>) -> Expr {
    let seconds = cast(seconds, DataType::Double(ExactNumberInfo::None));
    match nanos {
        Some(n) => binop(seconds, BinaryOperator::Plus, binop(n, BinaryOperator::Divide, number(1e9))),
        None => seconds,
    }
}

fn try_if(name: &str, expr: Expr) -> Expr {
    if name.starts_with("TRY_") { func("try", vec![expr]) } else { expr }
}

fn nullif_empty(expr: Expr) -> Expr {
    func("nullif", vec![expr, string("")])
}

fn optional_string(value: Option<&str>) -> Expr {
    value.map_or_else(null, string)
}

fn identifier_expr(name: &str) -> Expr {
    let parts: Vec<_> = name.split('.').map(|p| ident(&p.trim_matches('"').to_uppercase())).collect();
    if parts.len() == 1 { Expr::Identifier(parts.into_iter().next().unwrap()) } else { Expr::CompoundIdentifier(parts) }
}

fn rename(f: &mut Function, name: &str) -> Result<Option<Expr>, SqlError> {
    f.name = object_name(&[name]);
    Ok(None)
}

/// Moves `WITHIN GROUP (ORDER BY ...)` into the argument list, which is how DuckDB spells it.
fn move_within_group(f: &mut Function) {
    let order: Vec<OrderByExpr> = std::mem::take(&mut f.within_group);
    if order.is_empty() {
        return;
    }
    if let FunctionArguments::List(list) = &mut f.args {
        list.clauses.push(FunctionArgumentClause::OrderBy(order));
    }
}

fn function_shell() -> Function {
    match func("_", vec![]) {
        Expr::Function(f) => f,
        _ => unreachable!(),
    }
}

/// Snowflake implicitly converts string literals passed to date functions;
/// DuckDB can't choose an overload for them, so convert explicitly.
fn coerce_temporal_literals(f: &mut Function, name: &str) {
    let positions: &[usize] = match name {
        "DATEADD" | "TIMEADD" | "TIMESTAMPADD" => &[2],
        "DATEDIFF" | "TIMEDIFF" | "TIMESTAMPDIFF" => &[1, 2],
        "DATE_TRUNC" | "DATE_PART" => &[1],
        "MONTHS_BETWEEN" => &[0, 1],
        "YEAR" | "MONTH" | "DAY" | "DAYOFMONTH" | "DAYOFWEEK" | "DAYOFWEEKISO" | "DAYOFYEAR" | "WEEK"
        | "WEEKOFYEAR" | "WEEKISO" | "QUARTER" | "HOUR" | "MINUTE" | "SECOND" | "DAYNAME" | "MONTHNAME"
        | "LAST_DAY" | "ADD_MONTHS" | "YEAROFWEEK" | "YEAROFWEEKISO" => &[0],
        _ => return,
    };
    if let FunctionArguments::List(list) = &mut f.args {
        for &i in positions {
            if let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) = list.args.get_mut(i)
                && string_literal(e).is_some()
            {
                *e = func("sf_to_timestamp", vec![e.clone()]);
            }
        }
    }
}

/// Positional expression arguments of a call.
fn plain_args(f: &Function, name: &str) -> Result<Vec<Expr>, SqlError> {
    match &f.args {
        FunctionArguments::None => Ok(vec![]),
        FunctionArguments::List(list) => {
            if matches!(list.duplicate_treatment, Some(DuplicateTreatment::Distinct)) && !is_aggregate(name) {
                return Err(SqlError::Arguments(name.to_owned()));
            }
            list.args
                .iter()
                .map(|a| match a {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Ok(e.clone()),
                    FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. } => Ok(e.clone()),
                    _ => Err(SqlError::Unsupported(format!("wildcard arguments to {name}"))),
                })
                .collect()
        }
        FunctionArguments::Subquery(_) => Err(SqlError::Unsupported(format!("subquery argument to {name}"))),
    }
}

fn is_aggregate(name: &str) -> bool {
    matches!(
        name,
        "ARRAY_AGG"
            | "ARRAYAGG"
            | "ARRAY_UNIQUE_AGG"
            | "LISTAGG"
            | "OBJECT_AGG"
            | "AVG"
            | "MEDIAN"
            | "COUNT"
            | "SUM"
            | "MIN"
            | "MAX"
    )
}

/// `DATE_PART` / `EXTRACT` with Snowflake's part semantics.
pub fn date_part(part: Part, value: Expr) -> Expr {
    match part {
        Part::Epoch => cast(func("floor", vec![func("epoch", vec![value])]), DataType::BigInt(None)),
        Part::EpochMillisecond => func("epoch_ms", vec![value]),
        Part::EpochMicrosecond => func("epoch_us", vec![value]),
        Part::EpochNanosecond => func("epoch_ns", vec![value]),
        // Snowflake's NANOSECOND is the fraction of the second; DuckDB's includes the seconds.
        Part::Nanosecond => {
            let micros = func("date_part", vec![string("microsecond"), value]);
            binop(
                nested(binop(micros, BinaryOperator::Modulo, number(1_000_000))),
                BinaryOperator::Multiply,
                number(1000),
            )
        }
        _ => func("date_part", vec![string(part.duckdb_name()), value]),
    }
}

fn date_from_parts(year: Expr, month: Expr, day: Expr) -> Expr {
    let jan1 = func("make_date", vec![cast(year, DataType::BigInt(None)), number(1), number(1)]);
    let months = func("to_months", vec![cast(binop(month, BinaryOperator::Minus, number(1)), DataType::Integer(None))]);
    let days = func("to_days", vec![cast(binop(day, BinaryOperator::Minus, number(1)), DataType::Integer(None))]);
    binop(binop(jan1, BinaryOperator::Plus, months), BinaryOperator::Plus, days)
}

fn seconds_interval(hours: Expr, minutes: Expr, seconds: Expr) -> Expr {
    let total = binop(
        binop(
            binop(hours, BinaryOperator::Multiply, number(3600)),
            BinaryOperator::Plus,
            binop(minutes, BinaryOperator::Multiply, number(60)),
        ),
        BinaryOperator::Plus,
        seconds,
    );
    func("to_seconds", vec![cast(total, DataType::Double(ExactNumberInfo::None))])
}

fn function_args(args: Vec<Expr>, duplicate_treatment: Option<DuplicateTreatment>) -> FunctionArguments {
    FunctionArguments::List(sqlparser::ast::FunctionArgumentList {
        duplicate_treatment,
        args: args.into_iter().map(|a| FunctionArg::Unnamed(FunctionArgExpr::Expr(a))).collect(),
        clauses: vec![],
    })
}

fn distinct_of(f: &Function) -> Option<DuplicateTreatment> {
    match &f.args {
        FunctionArguments::List(list) => list.duplicate_treatment,
        _ => None,
    }
}

/// `ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW` on a window function.
fn running_frame(f: &mut Function) {
    use sqlparser::ast::{WindowFrame, WindowFrameBound, WindowFrameUnits, WindowType};
    if let Some(WindowType::WindowSpec(spec)) = &mut f.over {
        spec.window_frame = Some(WindowFrame {
            units: WindowFrameUnits::Rows,
            start_bound: WindowFrameBound::Preceding(None),
            end_bound: Some(WindowFrameBound::CurrentRow),
        });
    }
}

/// Replaces every `name` identifier in a parsed template with `value`.
fn replace_identifier(mut template: Expr, name: &str, value: Expr) -> Expr {
    let _ = sqlparser::ast::visit_expressions_mut(&mut template, |e| {
        if matches!(e, Expr::Identifier(i) if i.value == name) {
            *e = value.clone();
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    template
}

/// A Snowflake collation spec (`'en-ci'`, `'en-ci-ai'`, `'upper'`, ...) as a
/// DuckDB collation. Case- and accent-insensitivity map to DuckDB's NOCASE and
/// NOACCENT; locales and the other options don't change comparisons here.
pub fn collation_name(spec: &Expr) -> Result<sqlparser::ast::ObjectName, SqlError> {
    let Some(spec) = string_literal(spec) else {
        return Err(SqlError::Unsupported("COLLATE with a non-literal specification".into()));
    };
    let parts: Vec<String> = spec.to_lowercase().split('-').map(str::to_owned).collect();
    let mut names = vec![];
    if parts.iter().any(|p| p == "ci" || p == "upper" || p == "lower") {
        names.push("nocase");
    }
    if parts.iter().any(|p| p == "ai") {
        names.push("noaccent");
    }
    if names.is_empty() {
        names.push("binary");
    }
    Ok(object_name(&[&names.join(".")]))
}

/// Reverses a window's ORDER BY and mirrors its frame.
fn mirror_window(f: &mut Function) {
    use sqlparser::ast::{WindowFrameBound, WindowType};
    let Some(WindowType::WindowSpec(spec)) = &mut f.over else { return };
    for order in &mut spec.order_by {
        use sqlparser::ast::OrderBySort;
        let ascending = !matches!(order.options.sort, Some(OrderBySort::Desc));
        order.options.sort = Some(if ascending { OrderBySort::Desc } else { OrderBySort::Asc });
        order.options.nulls_first = Some(!order.options.nulls_first.unwrap_or(!ascending));
    }
    if let Some(frame) = &mut spec.window_frame {
        let flip = |b: &WindowFrameBound| match b {
            WindowFrameBound::Preceding(n) => WindowFrameBound::Following(n.clone()),
            WindowFrameBound::Following(n) => WindowFrameBound::Preceding(n.clone()),
            WindowFrameBound::CurrentRow => WindowFrameBound::CurrentRow,
        };
        let end = frame.end_bound.clone().unwrap_or(WindowFrameBound::CurrentRow);
        frame.end_bound = Some(flip(&frame.start_bound));
        frame.start_bound = flip(&end);
    }
}
