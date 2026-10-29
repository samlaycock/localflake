use std::collections::{BTreeMap, HashMap};

/// Session state the translator needs: names for context functions,
/// session variables and parameters that change how SQL is interpreted.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub account: String,
    pub user: String,
    pub role: Option<String>,
    pub warehouse: Option<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub session_id: u64,
    /// Most recent query IDs, newest last (for `LAST_QUERY_ID()`).
    pub query_history: Vec<String>,
    /// Session variables set with `SET`, already rendered as DuckDB literals.
    pub variables: HashMap<String, String>,
    pub timestamp_type_mapping: TimestampMapping,
    /// Session parameters changed from their defaults with `ALTER SESSION`.
    pub parameters: BTreeMap<String, String>,
    /// Operand types for type-directed rewrites (see [`crate::SiteTypes`]).
    pub types: crate::SiteTypes,
    /// The statement being run, as written (for `CURRENT_STATEMENT()`).
    pub statement: String,
    /// The open transaction's ID, and the last one committed or rolled back.
    pub transaction: Option<String>,
    pub last_transaction: Option<String>,
}

/// Session parameters and their defaults, as reported by `SHOW PARAMETERS`.
/// `TIMEZONE` defaults to UTC: without time zone data DuckDB can only work in UTC.
pub const DEFAULT_PARAMETERS: &[(&str, &str)] = &[
    ("ABORT_DETACHED_QUERY", "false"),
    ("AUTOCOMMIT", "true"),
    ("BINARY_INPUT_FORMAT", "HEX"),
    ("BINARY_OUTPUT_FORMAT", "HEX"),
    ("CLIENT_TIMESTAMP_TYPE_MAPPING", "TIMESTAMP_LTZ"),
    ("DATE_INPUT_FORMAT", "AUTO"),
    ("DATE_OUTPUT_FORMAT", "YYYY-MM-DD"),
    ("ERROR_ON_NONDETERMINISTIC_MERGE", "true"),
    ("ERROR_ON_NONDETERMINISTIC_UPDATE", "false"),
    ("LOCK_TIMEOUT", "43200"),
    ("MULTI_STATEMENT_COUNT", "1"),
    ("QUERY_TAG", ""),
    ("QUOTED_IDENTIFIERS_IGNORE_CASE", "false"),
    ("ROWS_PER_RESULTSET", "0"),
    ("STATEMENT_TIMEOUT_IN_SECONDS", "172800"),
    ("TIMESTAMP_DAY_IS_ALWAYS_24H", "false"),
    ("TIMESTAMP_INPUT_FORMAT", "AUTO"),
    ("TIMESTAMP_LTZ_OUTPUT_FORMAT", ""),
    ("TIMESTAMP_NTZ_OUTPUT_FORMAT", "YYYY-MM-DD HH24:MI:SS.FF3"),
    ("TIMESTAMP_OUTPUT_FORMAT", "YYYY-MM-DD HH24:MI:SS.FF3 TZHTZM"),
    ("TIMESTAMP_TYPE_MAPPING", "TIMESTAMP_NTZ"),
    ("TIMESTAMP_TZ_OUTPUT_FORMAT", ""),
    ("TIMEZONE", "UTC"),
    ("TIME_INPUT_FORMAT", "AUTO"),
    ("TIME_OUTPUT_FORMAT", "HH24:MI:SS"),
    ("TRANSACTION_DEFAULT_ISOLATION_LEVEL", "READ COMMITTED"),
    ("TWO_DIGIT_CENTURY_START", "1970"),
    ("USE_CACHED_RESULT", "true"),
    ("WEEK_OF_YEAR_POLICY", "0"),
    ("WEEK_START", "0"),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimestampMapping {
    #[default]
    Ntz,
    Ltz,
    Tz,
}

impl Context {
    /// `LAST_QUERY_ID(n)`: positive counts from the session start, negative from the end.
    pub fn last_query_id(&self, n: i64) -> Option<&str> {
        let index =
            if n < 0 { self.query_history.len().checked_sub(n.unsigned_abs() as usize)? } else { (n - 1) as usize };
        self.query_history.get(index).map(String::as_str)
    }
}
