//! `PARSE_IP`, `SEARCH_IP` and `SEARCH`.

use std::error::Error;
use std::net::Ipv6Addr;
use std::sync::Arc;

use duckdb::arrow::array::{Array, ArrayRef, AsArray, BooleanArray, RecordBatch, StringArray};
use duckdb::arrow::datatypes::{DataType, Int64Type};
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};
use regex::Regex;
use serde_json::{Map, Value, json};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// An IPv4 or IPv6 address, with an optional prefix length.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ip {
    V4(u32, Option<u8>),
    V6(u128, Option<u8>),
}

impl Ip {
    /// Parses `a.b.c.d[/n]` (each part up to three digits, leading zeros
    /// allowed) or an IPv6 address with up to four digits per group.
    fn parse(text: &str) -> Option<Self> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()) => {
                (address, Some(prefix.parse::<u8>().ok()?))
            }
            Some(_) => return None,
            None => (text, None),
        };
        if let Some(v4) = parse_v4(address) {
            return (prefix.unwrap_or(0) <= 32).then_some(Ip::V4(v4, prefix));
        }
        let v6: Ipv6Addr = address.parse().ok()?;
        (prefix.unwrap_or(0) <= 128).then_some(Ip::V6(u128::from(v6), prefix))
    }

    /// The first and last address of the network (the address itself without a prefix).
    fn range(self) -> (u128, u128) {
        let (value, bits, prefix): (u128, u32, Option<u8>) = match self {
            Ip::V4(v, p) => (v as u128, 32, p),
            Ip::V6(v, p) => (v, 128, p),
        };
        let host_bits = bits - prefix.map_or(bits, u32::from);
        let mask = if host_bits == 0 { 0 } else { (1u128 << host_bits) - 1 };
        (value & !mask, value | mask)
    }

    fn is_v4(self) -> bool {
        matches!(self, Ip::V4(..))
    }
}

fn parse_v4(text: &str) -> Option<u32> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    parts.iter().try_fold(0u32, |acc, part| {
        let valid = (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
        let octet: u32 = if valid { part.parse().ok()? } else { return None };
        (octet <= 255).then_some(acc << 8 | octet)
    })
}

/// The OBJECT `PARSE_IP` returns, as Snowflake documents it.
fn parse_ip_object(text: &str, kind: &str) -> std::result::Result<Value, String> {
    let ip = Ip::parse(text.trim()).ok_or_else(|| format!("invalid IP address '{text}'"))?;
    let host = text.trim().split('/').next().unwrap_or_default();
    let mut object = Map::new();
    object.insert("host".into(), json!(host));
    object.insert("ip_type".into(), json!(kind.to_lowercase()));
    object.insert("snowflake$type".into(), json!("ip_address"));
    let (start, end) = ip.range();
    match ip {
        Ip::V4(value, prefix) => {
            object.insert("family".into(), json!(4));
            object.insert("ipv4".into(), json!(value));
            object.insert("ip_fields".into(), json!([value, 0, 0, 0]));
            if let Some(prefix) = prefix {
                object.insert("netmask_prefix_length".into(), json!(prefix));
                object.insert("ipv4_range_start".into(), json!(start as u32));
                object.insert("ipv4_range_end".into(), json!(end as u32));
            }
        }
        Ip::V6(value, prefix) => {
            let fields: Vec<u32> = (0..4).map(|i| (value >> (96 - 32 * i)) as u32).collect();
            object.insert("family".into(), json!(6));
            object.insert("ip_fields".into(), json!(fields));
            object.insert("hex_ipv6".into(), json!(format!("{value:032X}")));
            if let Some(prefix) = prefix {
                object.insert("netmask_prefix_length".into(), json!(prefix));
                object.insert("hex_ipv6_range_start".into(), json!(format!("{start:032X}")));
                object.insert("hex_ipv6_range_end".into(), json!(format!("{end:032X}")));
            }
        }
    }
    Ok(Value::Object(object))
}

/// `sf_parse_ip(text, type, permissive) -> VARCHAR` (JSON).
pub struct ParseIp;

impl VArrowScalar for ParseIp {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let texts = input.column(0).as_string::<i32>();
        let kinds = input.column(1).as_string::<i32>();
        let permissive = input.column(2).as_primitive::<Int64Type>();
        let out = (0..texts.len())
            .map(|i| -> Result<Option<String>> {
                if texts.is_null(i) || kinds.is_null(i) {
                    return Ok(None);
                }
                let kind = kinds.value(i);
                if !kind.eq_ignore_ascii_case("INET") && !kind.eq_ignore_ascii_case("CIDR") {
                    return Err(format!("invalid IP address type '{kind}': expected INET or CIDR").into());
                }
                match parse_ip_object(texts.value(i), kind) {
                    Ok(object) => Ok(Some(object.to_string())),
                    Err(message) if !permissive.is_null(i) && permissive.value(i) == 1 => {
                        Ok(Some(json!({ "error": message }).to_string()))
                    }
                    Err(message) => Err(message.into()),
                }
            })
            .collect::<Result<StringArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Utf8, DataType::Int64], DataType::Utf8)]
    }
}

/// The text values to search: the value itself, or a VARIANT's scalar leaves.
fn fields(text: &str, is_json: bool) -> Vec<String> {
    fn leaves(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(object) => object.values().for_each(|v| leaves(v, out)),
            Value::Array(items) => items.iter().for_each(|v| leaves(v, out)),
            Value::String(s) => out.push(s.clone()),
            Value::Null => {}
            other => out.push(other.to_string()),
        }
    }
    if !is_json {
        return vec![text.to_owned()];
    }
    let mut out = Vec::new();
    match serde_json::from_str::<Value>(text) {
        Ok(value) => leaves(&value, &mut out),
        Err(_) => out.push(text.to_owned()),
    }
    out
}

fn ip_candidates() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[0-9A-Fa-f:.]*[:.][0-9A-Fa-f:.]*").unwrap())
}

/// `sf_search_ip(text, is_json, search) -> BOOLEAN`: whether the text holds the
/// address, or an address in the CIDR range.
pub struct SearchIp;

impl VArrowScalar for SearchIp {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let texts = input.column(0).as_string::<i32>();
        let is_json = input.column(1).as_boolean();
        let searches = input.column(2).as_string::<i32>();
        let out = (0..texts.len())
            .map(|i| -> Result<Option<bool>> {
                if texts.is_null(i) || searches.is_null(i) {
                    return Ok(None);
                }
                let search = searches.value(i);
                let target = Ip::parse(search.trim()).ok_or_else(|| {
                    format!("invalid argument for function SEARCH_IP: unexpected argument [{search}]")
                })?;
                let (start, end) = target.range();
                let found = fields(texts.value(i), is_json.value(i)).iter().any(|field| {
                    ip_candidates().find_iter(field).filter_map(|m| Ip::parse(m.as_str().trim_matches('.'))).any(|ip| {
                        let value = ip.range().0;
                        ip.is_v4() == target.is_v4() && (start..=end).contains(&value)
                    })
                });
                Ok(Some(found))
            })
            .collect::<Result<BooleanArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Boolean, DataType::Utf8], DataType::Boolean)]
    }
}

const DEFAULT_DELIMITERS: &str = " []; <>(){}|!,'\"*&?+/:=@.-$%\\_\n\r\t";

fn is_default_delimiter(c: char) -> bool {
    DEFAULT_DELIMITERS.contains(c)
}

#[derive(Clone, Copy, PartialEq)]
enum Analyzer {
    Default,
    Unicode,
    NoOp,
}

/// Lower-cased tokens, split as the analyzer does.
fn tokens(text: &str, analyzer: Analyzer) -> Vec<String> {
    match analyzer {
        Analyzer::NoOp => vec![text.to_owned()],
        Analyzer::Default => {
            text.split(is_default_delimiter).filter(|t| !t.is_empty()).map(str::to_lowercase).collect()
        }
        // Word boundaries that keep periods and apostrophes inside words
        // (`snowflake.com`, `bob's`), as Unicode segmentation does.
        Analyzer::Unicode => text
            .split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '\''))
            .map(|t| t.trim_matches(['.', '\'']))
            .filter(|t| !t.is_empty())
            .map(str::to_lowercase)
            .collect(),
    }
}

/// Whether `query` appears in `data` as a whole run of text bounded by
/// delimiters (SEARCH_MODE => 'EXACT').
fn exact_match(data: &str, query: &str) -> bool {
    let (data, query) = (data.to_lowercase(), query.to_lowercase());
    let (first, last) = (query.chars().next(), query.chars().last());
    data.match_indices(&query).any(|(at, _)| {
        let before = data[..at].chars().last();
        let after = data[at + query.len()..].chars().next();
        let bounded_before = before.is_none_or(is_default_delimiter) || first.is_some_and(is_default_delimiter);
        let bounded_after = after.is_none_or(is_default_delimiter) || last.is_some_and(is_default_delimiter);
        bounded_before && bounded_after
    })
}

fn search_field(field: &str, query: &str, analyzer: Analyzer, mode: &str) -> bool {
    if analyzer == Analyzer::NoOp {
        return field == query;
    }
    if mode == "EXACT" {
        return exact_match(field, query);
    }
    let data = tokens(field, analyzer);
    let wanted = tokens(query, analyzer);
    match mode {
        "AND" => wanted.iter().all(|t| data.contains(t)),
        "PHRASE" => !wanted.is_empty() && data.windows(wanted.len()).any(|w| w == wanted.as_slice()),
        _ => wanted.iter().any(|t| data.contains(t)),
    }
}

/// `sf_search(text, is_json, query, analyzer, mode) -> BOOLEAN`.
pub struct Search;

impl VArrowScalar for Search {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let texts = input.column(0).as_string::<i32>();
        let is_json = input.column(1).as_boolean();
        let queries = input.column(2).as_string::<i32>();
        let analyzers = input.column(3).as_string::<i32>();
        let modes = input.column(4).as_string::<i32>();
        let out = (0..texts.len())
            .map(|i| -> Result<Option<bool>> {
                if texts.is_null(i) || queries.is_null(i) {
                    return Ok(None);
                }
                let analyzer = match analyzers.value(i).to_uppercase().as_str() {
                    "DEFAULT_ANALYZER" => Analyzer::Default,
                    "UNICODE_ANALYZER" => Analyzer::Unicode,
                    "NO_OP_ANALYZER" => Analyzer::NoOp,
                    other => return Err(format!("invalid analyzer '{other}'").into()),
                };
                let mode = modes.value(i).to_uppercase();
                if !matches!(mode.as_str(), "OR" | "AND" | "PHRASE" | "EXACT") {
                    return Err(format!("invalid SEARCH_MODE '{mode}'").into());
                }
                if mode == "EXACT" && analyzer == Analyzer::Unicode {
                    return Err("SEARCH_MODE 'EXACT' is not supported with UNICODE_ANALYZER".into());
                }
                let query = queries.value(i);
                let found = fields(texts.value(i), is_json.value(i))
                    .iter()
                    .any(|field| search_field(field, query, analyzer, &mode));
                Ok(Some(found))
            })
            .collect::<Result<BooleanArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Utf8, DataType::Boolean, DataType::Utf8, DataType::Utf8, DataType::Utf8],
            DataType::Boolean,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ip_objects() {
        let v4 = parse_ip_object("192.168.242.188/24", "INET").unwrap();
        assert_eq!(v4["ipv4"], json!(3232297660u32));
        assert_eq!(v4["ipv4_range_start"], json!(3232297472u32));
        assert_eq!(v4["ipv4_range_end"], json!(3232297727u32));
        let v6 = parse_ip_object("fe80::20c:29ff:fe2c:429/64", "INET").unwrap();
        assert_eq!(v6["hex_ipv6"], json!("FE80000000000000020C29FFFE2C0429"));
        assert_eq!(v6["hex_ipv6_range_end"], json!("FE80000000000000FFFFFFFFFFFFFFFF"));
        assert_eq!(v6["ip_fields"], json!([4269801472u32, 0, 34351615, 4264297513u32]));
    }

    #[test]
    fn tokenizes_like_the_analyzers() {
        assert_eq!(tokens("docs@snowflake.com", Analyzer::Default), ["docs", "snowflake", "com"]);
        assert_eq!(tokens("docs@snowflake.com", Analyzer::Unicode), ["docs", "snowflake.com"]);
        assert_eq!(tokens("Bob's Burgers", Analyzer::Unicode), ["bob's", "burgers"]);
        assert_eq!(tokens("/opt/homebrew/README.md", Analyzer::Unicode), ["opt", "homebrew", "readme.md"]);
        assert_eq!(tokens("192.0.2.0/24", Analyzer::Unicode), ["192.0.2.0", "24"]);
    }

    #[test]
    fn exact_matches_need_delimiter_bounds() {
        assert!(exact_match("-blue,red,green;", "blue,red,green"));
        assert!(!exact_match("darkblue,red,green", "blue,red,green"));
        assert!(!exact_match("blue, red, green", "blue,red,green"));
        assert!(exact_match("foo [blue] bar", "[blue]"));
        assert!(exact_match("[[blue]]", "[blue]"));
        assert!(!exact_match("(blue)", "[blue]"));
    }
}
