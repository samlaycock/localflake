//! Staged file formats: file format options, decompression, and reading and
//! writing CSV and JSON with Snowflake's semantics.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use localflake_sql::stage::{OptValue, Options};
use serde_json::Value;

use crate::EngineError;

/// File format options, keys upper case. Values are strings or lists of strings.
#[derive(Debug, Clone, Default)]
pub(crate) struct Format {
    pub options: BTreeMap<String, Value>,
}

/// Default options per format type, in the order `DESCRIBE` lists them.
pub(crate) fn defaults(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "JSON" => &[
            ("TYPE", "JSON"),
            ("FILE_EXTENSION", ""),
            ("DATE_FORMAT", "AUTO"),
            ("TIME_FORMAT", "AUTO"),
            ("TIMESTAMP_FORMAT", "AUTO"),
            ("BINARY_FORMAT", "HEX"),
            ("TRIM_SPACE", "false"),
            ("NULL_IF", "[]"),
            ("COMPRESSION", "AUTO"),
            ("ENABLE_OCTAL", "false"),
            ("ALLOW_DUPLICATE", "false"),
            ("STRIP_OUTER_ARRAY", "false"),
            ("STRIP_NULL_VALUES", "false"),
            ("IGNORE_UTF8_ERRORS", "false"),
            ("REPLACE_INVALID_CHARACTERS", "false"),
            ("SKIP_BYTE_ORDER_MARK", "true"),
        ],
        "PARQUET" => &[
            ("TYPE", "PARQUET"),
            ("TRIM_SPACE", "false"),
            ("NULL_IF", "[]"),
            ("COMPRESSION", "AUTO"),
            ("BINARY_AS_TEXT", "true"),
            ("USE_LOGICAL_TYPE", "true"),
            ("REPLACE_INVALID_CHARACTERS", "false"),
        ],
        _ => &[
            ("TYPE", "CSV"),
            ("RECORD_DELIMITER", "\n"),
            ("FIELD_DELIMITER", ","),
            ("FILE_EXTENSION", ""),
            ("SKIP_HEADER", "0"),
            ("PARSE_HEADER", "false"),
            ("DATE_FORMAT", "AUTO"),
            ("TIME_FORMAT", "AUTO"),
            ("TIMESTAMP_FORMAT", "AUTO"),
            ("BINARY_FORMAT", "HEX"),
            ("ESCAPE", "NONE"),
            ("ESCAPE_UNENCLOSED_FIELD", "\\"),
            ("TRIM_SPACE", "false"),
            ("FIELD_OPTIONALLY_ENCLOSED_BY", "NONE"),
            ("NULL_IF", "[\\N]"),
            ("COMPRESSION", "AUTO"),
            ("ERROR_ON_COLUMN_COUNT_MISMATCH", "true"),
            ("VALIDATE_UTF8", "true"),
            ("SKIP_BLANK_LINES", "false"),
            ("REPLACE_INVALID_CHARACTERS", "false"),
            ("EMPTY_FIELD_AS_NULL", "true"),
            ("SKIP_BYTE_ORDER_MARK", "true"),
            ("ENCODING", "UTF8"),
        ],
    }
}

/// Converts parsed options to stored JSON values.
pub(crate) fn options_json(options: &Options) -> BTreeMap<String, Value> {
    fn convert(value: &OptValue) -> Value {
        match value {
            OptValue::Str(s) | OptValue::Num(s) | OptValue::Word(s) => Value::String(s.clone()),
            OptValue::List(items) => Value::Array(items.iter().map(convert).collect()),
            OptValue::Nested(nested) => Value::Object(options_json(nested).into_iter().collect()),
        }
    }
    options.iter().map(|(k, v)| (k.clone(), convert(v))).collect()
}

impl Format {
    /// Overlays `options` (later wins). Changing TYPE drops the other options'
    /// meaning, but Snowflake keeps them, so they are kept here too.
    pub fn apply(&mut self, options: &BTreeMap<String, Value>) {
        for (k, v) in options {
            if k != "FORMAT_NAME" {
                self.options.insert(k.clone(), v.clone());
            }
        }
    }

    pub fn kind(&self) -> String {
        self.text("TYPE").map(|t| t.to_uppercase()).unwrap_or_else(|| "CSV".to_owned())
    }

    /// An option's text, falling back to the type's default.
    pub fn text(&self, key: &str) -> Option<String> {
        match self.options.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(items)) => items.first().and_then(Value::as_str).map(str::to_owned),
            Some(_) => None,
            None => {
                let kind = match self.options.get("TYPE") {
                    Some(Value::String(t)) => t.to_uppercase(),
                    _ => "CSV".to_owned(),
                };
                defaults(&kind).iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_owned())
            }
        }
    }

    pub fn flag(&self, key: &str) -> bool {
        self.text(key).is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    /// A single-character-or-NONE option such as FIELD_OPTIONALLY_ENCLOSED_BY.
    pub fn char_option(&self, key: &str) -> Option<String> {
        self.text(key).filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("NONE"))
    }

    pub fn null_if(&self) -> Vec<String> {
        match self.options.get("NULL_IF") {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ if self.kind() == "CSV" => vec!["\\N".to_owned()],
            _ => vec![],
        }
    }

    /// A format option that is not AUTO (DATE_FORMAT, ...).
    pub fn explicit(&self, key: &str) -> Option<String> {
        self.text(key).filter(|v| !v.eq_ignore_ascii_case("AUTO") && !v.is_empty())
    }

    /// Every option with its value as Snowflake displays it, for DESCRIBE.
    pub fn described(&self) -> Vec<(String, String, String)> {
        let kind = self.kind();
        defaults(&kind)
            .iter()
            .map(|(key, default)| {
                let value = match self.options.get(*key) {
                    Some(Value::Array(items)) => {
                        format!("[{}]", items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                    }
                    Some(Value::String(s)) if *key == "TYPE" => s.to_uppercase(),
                    Some(Value::String(s)) => s.clone(),
                    _ => (*default).to_owned(),
                };
                ((*key).to_owned(), value, (*default).to_owned())
            })
            .collect()
    }
}

/// Decompresses file content per the COMPRESSION option (AUTO detects gzip).
pub(crate) fn decompress(bytes: Vec<u8>, compression: &str, name: &str) -> Result<Vec<u8>, EngineError> {
    let compression = compression.to_uppercase();
    let gzip = bytes.starts_with(&[0x1f, 0x8b]);
    let mut out = Vec::new();
    let unsupported =
        |what: &str| EngineError::load("100069", format!("Unsupported compression '{what}' for file '{name}'"));
    match compression.as_str() {
        "AUTO" | "GZIP" if gzip => {
            flate2::read::MultiGzDecoder::new(&bytes[..])
                .read_to_end(&mut out)
                .map_err(|e| EngineError::load("100069", format!("Error decompressing file '{name}': {e}")))?;
            Ok(out)
        }
        "DEFLATE" => {
            flate2::read::ZlibDecoder::new(&bytes[..])
                .read_to_end(&mut out)
                .map_err(|e| EngineError::load("100069", format!("Error decompressing file '{name}': {e}")))?;
            Ok(out)
        }
        "RAW_DEFLATE" => {
            flate2::read::DeflateDecoder::new(&bytes[..])
                .read_to_end(&mut out)
                .map_err(|e| EngineError::load("100069", format!("Error decompressing file '{name}': {e}")))?;
            Ok(out)
        }
        "AUTO" if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) => Err(unsupported("ZSTD")),
        "AUTO" if bytes.starts_with(b"BZh") => Err(unsupported("BZ2")),
        "AUTO" | "NONE" | "GZIP" => Ok(bytes),
        other => Err(unsupported(other)),
    }
}

/// Compresses unloaded content; returns the bytes and the file extension to add.
pub(crate) fn compress(bytes: Vec<u8>, compression: &str) -> Result<(Vec<u8>, &'static str), EngineError> {
    let io = |e: std::io::Error| EngineError::internal(e);
    match compression.to_uppercase().as_str() {
        "AUTO" | "GZIP" => {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&bytes).map_err(io)?;
            Ok((encoder.finish().map_err(io)?, ".gz"))
        }
        "DEFLATE" => {
            let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&bytes).map_err(io)?;
            Ok((encoder.finish().map_err(io)?, ".deflate"))
        }
        "RAW_DEFLATE" => {
            let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&bytes).map_err(io)?;
            Ok((encoder.finish().map_err(io)?, ".raw_deflate"))
        }
        "NONE" => Ok((bytes, "")),
        other => Err(EngineError::load("100069", format!("Unsupported compression '{other}' for unloading"))),
    }
}

/// Decodes text per the ENCODING option (UTF-8, or Latin-1 style single bytes).
pub(crate) fn decode_text(bytes: Vec<u8>, format: &Format) -> String {
    let encoding = format.text("ENCODING").unwrap_or_default().to_uppercase().replace(['-', '_'], "");
    let mut text = if matches!(encoding.as_str(), "" | "UTF8") {
        String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    } else {
        bytes.iter().map(|&b| b as char).collect()
    };
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    text
}

/// One record of a delimited file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    /// The line the record starts on (1-based).
    pub line: usize,
    pub fields: Vec<Option<String>>,
}

/// Parses delimited text with Snowflake's CSV options. SKIP_HEADER is applied
/// by the caller (records keep their line numbers).
pub(crate) fn parse_csv(text: &str, format: &Format) -> Vec<Record> {
    let delimiter: Vec<char> = format.char_option("FIELD_DELIMITER").unwrap_or_default().chars().collect();
    let record: Vec<char> = format.char_option("RECORD_DELIMITER").unwrap_or_else(|| "\n".into()).chars().collect();
    let enclosure = format.char_option("FIELD_OPTIONALLY_ENCLOSED_BY").and_then(|s| s.chars().next());
    let escape = format.char_option("ESCAPE").and_then(|s| s.chars().next());
    let escape_unenclosed = format.char_option("ESCAPE_UNENCLOSED_FIELD").and_then(|s| s.chars().next());
    let null_if = format.null_if();
    let empty_as_null = format.text("EMPTY_FIELD_AS_NULL").is_none_or(|v| !v.eq_ignore_ascii_case("false"));
    let trim = format.flag("TRIM_SPACE");
    let skip_blank = format.flag("SKIP_BLANK_LINES");

    let chars: Vec<char> = text.chars().collect();
    let at =
        |i: usize, pat: &[char]| !pat.is_empty() && chars.len() >= i + pat.len() && chars[i..i + pat.len()] == *pat;
    // A `\r\n` record ends like `\n` when the record delimiter is `\n`.
    let record_end = |i: usize| -> Option<usize> {
        if at(i, &record) {
            Some(record.len())
        } else if record == ['\n'] && at(i, &['\r', '\n']) {
            Some(2)
        } else {
            None
        }
    };

    let mut records = vec![];
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let start_line = line;
        let mut fields: Vec<Option<String>> = vec![];
        let mut blank = true;
        loop {
            // One field.
            let mut value = String::new();
            let mut enclosed = false;
            if trim {
                while i < chars.len() && chars[i] == ' ' {
                    i += 1;
                }
            }
            if let Some(q) = enclosure
                && i < chars.len()
                && chars[i] == q
            {
                enclosed = true;
                blank = false;
                i += 1;
                while i < chars.len() {
                    let c = chars[i];
                    if Some(c) == escape && escape != Some(q) && i + 1 < chars.len() {
                        value.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if c == q {
                        if i + 1 < chars.len() && chars[i + 1] == q {
                            value.push(q);
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    if c == '\n' {
                        line += 1;
                    }
                    value.push(c);
                    i += 1;
                }
            }
            // Unenclosed text (or text after a closing quote, up to the delimiter).
            let mut raw_tail = String::new();
            while i < chars.len() && !at(i, &delimiter) && record_end(i).is_none() {
                let c = chars[i];
                if Some(c) == escape_unenclosed && i + 1 < chars.len() {
                    let next = chars[i + 1];
                    if at(i + 1, &delimiter) || record_end(i + 1).is_some() || Some(next) == escape_unenclosed {
                        raw_tail.push(next);
                        i += 2;
                        blank = false;
                        continue;
                    }
                }
                raw_tail.push(c);
                blank = false;
                i += 1;
            }
            if !enclosed || !raw_tail.trim().is_empty() {
                value.push_str(&raw_tail);
            }
            if trim {
                value = value.trim().to_owned();
            }
            let null = (!enclosed && value.is_empty() && empty_as_null) || null_if.contains(&value);
            let field = if null { None } else { Some(value) };
            fields.push(field);
            if at(i, &delimiter) {
                i += delimiter.len();
                blank = false;
                continue;
            }
            if let Some(len) = record_end(i) {
                i += len;
                line += 1;
            }
            break;
        }
        if blank && skip_blank {
            continue;
        }
        records.push(Record { line: start_line, fields });
    }
    records
}

/// Parses JSON documents (one or more values, optionally inside an outer array).
pub(crate) fn parse_json_documents(text: &str, format: &Format, name: &str) -> Result<Vec<Value>, EngineError> {
    let mut values = vec![];
    for value in serde_json::Deserializer::from_str(text).into_iter::<Value>() {
        let value = value.map_err(|e| {
            EngineError::load("100069", format!("Error parsing JSON: {e}\n  File '{name}', line {}", e.line()))
        })?;
        match value {
            Value::Array(items) if format.flag("STRIP_OUTER_ARRAY") => values.extend(items),
            other => values.push(other),
        }
    }
    if format.flag("STRIP_NULL_VALUES") {
        values.iter_mut().for_each(strip_nulls);
    }
    Ok(values)
}

fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// Writes rows as delimited text. `text_columns` marks string-typed columns,
/// which are enclosed when FIELD_OPTIONALLY_ENCLOSED_BY is set.
pub(crate) fn write_csv(
    header: Option<&[String]>,
    rows: &[Vec<Option<String>>],
    text_columns: &[bool],
    format: &Format,
) -> String {
    let delimiter = format.char_option("FIELD_DELIMITER").unwrap_or_default();
    let record = format.char_option("RECORD_DELIMITER").unwrap_or_else(|| "\n".into());
    let enclosure = format.char_option("FIELD_OPTIONALLY_ENCLOSED_BY");
    let escape = format.char_option("ESCAPE");
    let escape_unenclosed = format.char_option("ESCAPE_UNENCLOSED_FIELD");
    let null = format.null_if().into_iter().next().unwrap_or_default();
    let empty_as_null = format.text("EMPTY_FIELD_AS_NULL").is_none_or(|v| !v.eq_ignore_ascii_case("false"));

    let field = |value: &str, text: bool| -> String {
        match &enclosure {
            Some(q) if text => {
                let escaped = value.replace(q.as_str(), &format!("{}{q}", escape.as_deref().unwrap_or(q)));
                format!("{q}{escaped}{q}")
            }
            _ => match &escape_unenclosed {
                Some(e) if !value.is_empty() => {
                    let mut out = value.replace(e.as_str(), &format!("{e}{e}"));
                    if !delimiter.is_empty() {
                        out = out.replace(delimiter.as_str(), &format!("{e}{delimiter}"));
                    }
                    out.replace(record.as_str(), &format!("{e}{record}"))
                }
                _ => value.to_owned(),
            },
        }
    };

    let mut out = String::new();
    if let Some(names) = header {
        let cells: Vec<String> = names.iter().map(|n| field(n, true)).collect();
        out.push_str(&cells.join(&delimiter));
        out.push_str(&record);
    }
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, value)| match value {
                None => null.clone(),
                // Without an enclosure an empty string would read back as NULL.
                Some(v) if v.is_empty() && enclosure.is_none() && empty_as_null => String::new(),
                Some(v) => field(v, text_columns.get(i).copied().unwrap_or(false)),
            })
            .collect();
        out.push_str(&cells.join(&delimiter));
        out.push_str(&record);
    }
    out
}

/// Glob-free LIKE matching for SHOW ... LIKE (case-insensitive, `%` and `_`).
pub(crate) fn like(pattern: &str, value: &str) -> bool {
    fn go(p: &[char], v: &[char]) -> bool {
        match p.first() {
            None => v.is_empty(),
            Some('%') => (0..=v.len()).any(|i| go(&p[1..], &v[i..])),
            Some('_') => !v.is_empty() && go(&p[1..], &v[1..]),
            Some(c) => v.first().is_some_and(|x| x.to_lowercase().eq(c.to_lowercase())) && go(&p[1..], &v[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    go(&p, &v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(pairs: &[(&str, Value)]) -> Format {
        Format { options: pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect() }
    }

    fn fields(text: &str, f: &Format) -> Vec<Vec<Option<String>>> {
        parse_csv(text, f).into_iter().map(|r| r.fields).collect()
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_owned())
    }

    #[test]
    fn parses_csv_with_defaults() {
        let f = Format::default();
        assert_eq!(
            fields("a,b\n1,\\N\n,x\r\n", &f),
            vec![vec![s("a"), s("b")], vec![s("1"), None], vec![None, s("x")]]
        );
        // Without an enclosure, quotes are data.
        assert_eq!(fields("\"a,b\"\n", &f), vec![vec![s("\"a"), s("b\"")]]);
        assert_eq!(fields("a\\,b,c\n", &f), vec![vec![s("a,b"), s("c")]]);
    }

    #[test]
    fn parses_enclosed_fields() {
        let f = format(&[("FIELD_OPTIONALLY_ENCLOSED_BY", Value::String("\"".into()))]);
        let records = parse_csv("\"a,b\",\"say \"\"hi\"\"\",\"\"\n\"multi\nline\",2\n", &f);
        assert_eq!(records[0].fields, vec![s("a,b"), s("say \"hi\""), s("")]);
        assert_eq!(records[1].fields, vec![s("multi\nline"), s("2")]);
        assert_eq!((records[0].line, records[1].line), (1, 2));
    }

    #[test]
    fn parses_custom_delimiters_and_nulls() {
        let f = format(&[
            ("FIELD_DELIMITER", Value::String("|".into())),
            ("NULL_IF", Value::Array(vec![Value::String("NA".into())])),
            ("TRIM_SPACE", Value::String("TRUE".into())),
        ]);
        assert_eq!(fields(" a | NA |\n", &f), vec![vec![s("a"), None, None]]);
    }

    #[test]
    fn writes_csv() {
        let f = format(&[("FIELD_OPTIONALLY_ENCLOSED_BY", Value::String("\"".into()))]);
        let rows = vec![vec![s("1"), s("a\"b"), None]];
        assert_eq!(
            write_csv(Some(&["ID".into(), "NAME".into(), "X".into()]), &rows, &[false, true, false], &f),
            "\"ID\",\"NAME\",\"X\"\n1,\"a\"\"b\",\\N\n"
        );
        assert_eq!(write_csv(None, &[vec![s("a,b")]], &[true], &Format::default()), "a\\,b\n");
    }
}
