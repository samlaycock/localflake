//! Semi-structured data on top of DuckDB's VARIANT type.
//!
//! Snowflake path access (`v:a.b[0]`, `v['a']`, `GET_PATH(v, 'a.b')`) becomes
//! nested `variant_extract` calls. DuckDB arrays are 1-based, so literal
//! indexes are shifted; dynamic keys go through the `sf_get` macro, which picks
//! index or key semantics from the key's type. Like Snowflake, extracting a
//! missing key or out-of-range index yields NULL.

use sqlparser::ast::{Expr, JsonPath, JsonPathElem, TableAlias, TableFactor};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;

use crate::SqlError;
use crate::ast::*;

pub enum Key {
    Name(String),
    Index(i64),
}

/// `value:path` → `variant_extract(...)` chain.
pub fn path_access(value: Expr, path: JsonPath) -> Expr {
    let mut expr = cast(value, custom_type("VARIANT"));
    for elem in path.path {
        let key = match elem {
            JsonPathElem::Dot { key, .. } => Key::Name(key),
            JsonPathElem::Bracket { key } | JsonPathElem::ColonBracket { key } => match literal_key(&key) {
                Some(k) => k,
                None => {
                    expr = func("sf_get", vec![expr, key]);
                    continue;
                }
            },
        };
        expr = extract(expr, key);
    }
    expr
}

pub fn literal_key(expr: &Expr) -> Option<Key> {
    if let Some(s) = string_literal(expr) {
        return Some(Key::Name(s.to_owned()));
    }
    integer_literal(expr).map(Key::Index)
}

pub fn extract(value: Expr, key: Key) -> Expr {
    let key = match key {
        Key::Name(name) => string(name),
        Key::Index(i) if i < 0 => return null(),
        Key::Index(i) => cast(number(i + 1), custom_type("UINTEGER")),
    };
    func("variant_extract", vec![cast(value, custom_type("VARIANT")), key])
}

/// Parses a `GET_PATH` / `FLATTEN(path => ...)` path such as `a.b[0]."c d"`.
pub fn extract_path(value: Expr, path: &str) -> Result<Expr, SqlError> {
    Ok(parse_path(path)?.into_iter().fold(value, extract))
}

fn parse_path(path: &str) -> Result<Vec<Key>, SqlError> {
    let invalid = || SqlError::Unsupported(format!("path '{path}'"));
    let mut keys = Vec::new();
    let mut chars = path.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
            }
            '[' => {
                chars.next();
                let inner: String = chars.by_ref().take_while(|&c| c != ']').collect();
                let inner = inner.trim();
                keys.push(match inner.parse::<i64>() {
                    Ok(i) => Key::Index(i),
                    Err(_) => Key::Name(inner.trim_matches(|c| c == '\'' || c == '"').to_owned()),
                });
            }
            '"' => {
                chars.next();
                keys.push(Key::Name(chars.by_ref().take_while(|&c| c != '"').collect()));
            }
            _ => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '[' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name.is_empty() {
                    return Err(invalid());
                }
                keys.push(Key::Name(name));
            }
        }
    }
    Ok(keys)
}

pub fn empty_object() -> Expr {
    cast(cast(string("{}"), custom_type("JSON")), custom_type("VARIANT"))
}

pub fn empty_array() -> Expr {
    cast(cast(string("[]"), custom_type("JSON")), custom_type("VARIANT"))
}

pub struct FlattenArgs {
    pub input: Expr,
    pub path: Option<String>,
    pub outer: bool,
    pub recursive: bool,
    pub mode: String,
}

/// `FLATTEN(...)` as a lateral subquery producing Snowflake's
/// `SEQ, KEY, PATH, INDEX, VALUE, THIS` columns.
pub fn flatten(args: FlattenArgs, alias: Option<TableAlias>) -> Result<TableFactor, SqlError> {
    let this = match &args.path {
        Some(path) => extract_path(args.input, path)?,
        None => args.input,
    };
    let this = cast(this, custom_type("VARIANT")).to_string();
    let prefix = quote_literal(args.path.as_deref().unwrap_or(""));
    let dot = quote_literal(if args.path.is_some() { "." } else { "" });
    if args.recursive {
        return recursive_flatten(&this, &prefix, &args.mode, args.outer, alias);
    }
    let empty = "CAST([] AS STRUCT(k VARCHAR, i BIGINT, v VARIANT, t VARIANT)[])";
    let arrays = if args.mode == "OBJECT" {
        empty.to_owned()
    } else {
        format!(
            "list_transform(range(len(CAST({this} AS VARIANT[]))), \
             lambda __i: {{'k': CAST(NULL AS VARCHAR), 'i': __i, 'v': CAST({this} AS VARIANT[])[__i + 1], 't': {this}}})"
        )
    };
    let objects = if args.mode == "ARRAY" {
        empty.to_owned()
    } else {
        format!(
            "list_transform(map_entries(CAST(CAST({this} AS JSON) AS MAP(VARCHAR, JSON))), \
             lambda __m: {{'k': __m.key, 'i': CAST(NULL AS BIGINT), 'v': CAST(__m.value AS VARIANT), 't': {this}}})"
        )
    };
    let items = format!(
        "CASE WHEN regexp_matches(variant_typeof({this}), '^ARRAY') THEN {arrays} \
         WHEN regexp_matches(variant_typeof({this}), '^OBJECT') THEN {objects} ELSE {empty} END"
    );
    // OUTER => TRUE emits one row with NULLs for inputs with nothing to flatten.
    let items = if args.outer {
        format!(
            "CASE WHEN len({items}) = 0 THEN [{{'k': CAST(NULL AS VARCHAR), 'i': CAST(NULL AS BIGINT), \
             'v': CAST(NULL AS VARIANT), 't': {this}}}] ELSE {items} END"
        )
    } else {
        items
    };
    // A lone unnest in the lateral subquery keeps element order; extra
    // subquery layers would be decorrelated into joins that don't.
    let sql = format!(
        "SELECT CAST(1 AS BIGINT) AS \"SEQ\", __e.k AS \"KEY\", \
         CASE WHEN __e.k IS NOT NULL THEN {prefix} || {dot} || __e.k WHEN __e.i IS NOT NULL THEN {prefix} || '[' || __e.i || ']' END AS \"PATH\", \
         __e.i AS \"INDEX\", __e.v AS \"VALUE\", __e.t AS \"THIS\" \
         FROM (SELECT unnest({items}) AS __e) AS __flatten"
    );
    derived(&sql, alias, true)
}

/// `GENERATOR(ROWCOUNT => n)`.
/// `FLATTEN(..., RECURSIVE => TRUE)`: the engine's `sf_flatten_recursive`
/// walks the JSON and returns every element with its key, index, path and parent.
fn recursive_flatten(
    this: &str,
    prefix: &str,
    mode: &str,
    outer: bool,
    alias: Option<TableAlias>,
) -> Result<TableFactor, SqlError> {
    let mode = quote_literal(mode);
    let items = format!(
        "list_transform(CAST(CAST(sf_flatten_recursive(CAST(CAST({this} AS JSON) AS VARCHAR), {prefix}, {mode}) AS JSON) AS JSON[]), \
         lambda __j: {{'k': json_extract_string(__j, '$.k'), 'i': CAST(json_extract(__j, '$.i') AS BIGINT), \
         'p': json_extract_string(__j, '$.p'), 'v': CAST(json_extract(__j, '$.v') AS VARIANT), \
         't': CAST(json_extract(__j, '$.t') AS VARIANT)}})"
    );
    let items = if outer {
        format!(
            "CASE WHEN len({items}) = 0 THEN [{{'k': CAST(NULL AS VARCHAR), 'i': CAST(NULL AS BIGINT), \
             'p': CAST(NULL AS VARCHAR), 'v': CAST(NULL AS VARIANT), 't': {this}}}] ELSE {items} END"
        )
    } else {
        items
    };
    let sql = format!(
        "SELECT CAST(1 AS BIGINT) AS \"SEQ\", __e.k AS \"KEY\", __e.p AS \"PATH\", __e.i AS \"INDEX\", \
         __e.v AS \"VALUE\", __e.t AS \"THIS\" FROM (SELECT unnest({items}) AS __e) AS __flatten"
    );
    derived(&sql, alias, true)
}

pub fn generator(rowcount: Expr, alias: Option<TableAlias>) -> Result<TableFactor, SqlError> {
    derived(&format!("SELECT range AS __row FROM range(CAST({rowcount} AS BIGINT))"), alias, false)
}

/// `SPLIT_TO_TABLE(string, delimiter)`.
pub fn split_to_table(input: Expr, delimiter: Expr, alias: Option<TableAlias>) -> Result<TableFactor, SqlError> {
    let sql = format!(
        "SELECT CAST(1 AS BIGINT) AS \"SEQ\", CAST(__i AS BIGINT) AS \"INDEX\", __parts[__i] AS \"VALUE\" \
         FROM (SELECT __parts, unnest(range(1, len(__parts) + 1)) AS __i FROM (SELECT string_split({input}, {delimiter}) AS __parts))"
    );
    derived(&sql, alias, true)
}

/// `STRTOK_SPLIT_TO_TABLE(s, delimiters)`: like SPLIT_TO_TABLE, but any
/// delimiter character splits and empty tokens are dropped.
pub fn strtok_split_to_table(
    input: Expr,
    delimiters: Expr,
    alias: Option<TableAlias>,
) -> Result<TableFactor, SqlError> {
    let sql = format!(
        "SELECT CAST(1 AS BIGINT) AS \"SEQ\", CAST(__i AS BIGINT) AS \"INDEX\", __parts[__i] AS \"VALUE\" \
         FROM (SELECT __parts, unnest(range(1, len(__parts) + 1)) AS __i FROM (SELECT sf_tokens({input}, {delimiters}) AS __parts))"
    );
    derived(&sql, alias, true)
}

fn derived(sql: &str, alias: Option<TableAlias>, lateral: bool) -> Result<TableFactor, SqlError> {
    let subquery = Parser::new(&DuckDbDialect).try_with_sql(sql)?.parse_query()?;
    Ok(TableFactor::Derived { lateral, subquery, alias, sample: None })
}
