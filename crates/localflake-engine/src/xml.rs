//! XML: `PARSE_XML`, `CHECK_XML`, `XMLGET`, `TO_XML`, and how XML values print.
//!
//! As in Snowflake, parsed XML is an OBJECT per element: `"@"` holds the tag
//! name, `"@name"` each attribute and `"$"` the content, which is a single
//! value or child element, or an array of them. Text that prints back the same
//! as a number or boolean becomes one, unless auto-conversion is disabled.
//! Objects with a `"@"` key print as XML rather than JSON.

use std::error::Error;
use std::sync::Arc;

use duckdb::arrow::array::{Array, ArrayRef, AsArray, RecordBatch, StringArray};
use duckdb::arrow::datatypes::{DataType, Int64Type};
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use serde_json::{Map, Value};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Parses an XML document into Snowflake's OBJECT representation.
pub fn parse(text: &str, auto_convert: bool) -> std::result::Result<Value, String> {
    let mut reader = Reader::from_str(text);
    // Elements being read, innermost last: the element, its content so far,
    // and text not yet added to the content (text and entities arrive in pieces).
    let mut stack: Vec<(Map<String, Value>, Vec<Value>, String)> = Vec::new();
    let mut root = None;
    let flush = |stack: &mut Vec<(Map<String, Value>, Vec<Value>, String)>| {
        if let Some((_, content, text)) = stack.last_mut() {
            let pending = std::mem::take(text);
            if !pending.trim().is_empty() {
                content.push(scalar(pending.trim(), auto_convert));
            }
        }
    };
    loop {
        let position = reader.buffer_position();
        let event = reader.read_event().map_err(|e| match &e {
            quick_xml::Error::IllFormed(
                quick_xml::errors::IllFormedError::UnmatchedEndTag(name)
                | quick_xml::errors::IllFormedError::MismatchedEndTag { found: name, .. },
            ) => format!("no opening tag for </{name}>, pos {}", reader.buffer_position()),
            _ => format!("{e}, pos {position}"),
        })?;
        match event {
            Event::Start(start) => {
                flush(&mut stack);
                stack.push((element(&start, auto_convert)?, Vec::new(), String::new()));
            }
            Event::Empty(start) => {
                flush(&mut stack);
                let item = Value::Object(element(&start, auto_convert)?);
                match stack.last_mut() {
                    Some((_, content, _)) => content.push(item),
                    None => root = Some(item),
                }
            }
            Event::End(_) => {
                flush(&mut stack);
                let (mut object, mut content, _) = stack.pop().ok_or("unexpected end tag")?;
                match content.len() {
                    0 => {}
                    1 => {
                        object.insert("$".into(), content.pop().unwrap());
                    }
                    _ => {
                        object.insert("$".into(), Value::Array(content));
                    }
                }
                let item = Value::Object(object);
                match stack.last_mut() {
                    Some((_, content, _)) => content.push(item),
                    None => root = Some(item),
                }
            }
            Event::Text(text) => {
                if let Some((_, _, pending)) = stack.last_mut() {
                    pending.push_str(&text.xml10_content());
                }
            }
            Event::CData(data) => {
                if let Some((_, _, pending)) = stack.last_mut() {
                    pending.push_str(&data.into_inner());
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref().map_err(|e| e.to_string())? {
                    Some(c) => c.to_string(),
                    None => {
                        let name = reference.into_inner();
                        quick_xml::escape::resolve_predefined_entity(&name)
                            .ok_or_else(|| format!("unknown entity &{name};"))?
                            .to_owned()
                    }
                };
                if let Some((_, _, pending)) = stack.last_mut() {
                    pending.push_str(&resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some((object, _, _)) = stack.last() {
        return Err(format!("missing closing tag for <{}>", object.get("@").and_then(Value::as_str).unwrap_or("")));
    }
    root.ok_or_else(|| "empty XML document".to_owned())
}

fn element(start: &BytesStart, auto_convert: bool) -> std::result::Result<Map<String, Value>, String> {
    let mut object = Map::new();
    object.insert("@".into(), Value::String(start.name().as_ref().to_owned()));
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        let value = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|e| e.to_string())?;
        object.insert(format!("@{}", attribute.key.as_ref()), scalar(&value, auto_convert));
    }
    Ok(object)
}

/// Text as a number or boolean when it prints back the same (or is in
/// scientific notation), else a string.
fn scalar(text: &str, auto_convert: bool) -> Value {
    if !auto_convert {
        return Value::String(text.to_owned());
    }
    let integer = |s: &str| {
        let digits = s.strip_prefix('-').unwrap_or(s);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && (digits == "0" || !digits.starts_with('0'))
    };
    let decimal = |s: &str| match s.split_once('.') {
        Some((whole, fraction)) => {
            integer(whole) && !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    };
    let scientific = |s: &str| match s.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            let mantissa = mantissa.strip_prefix('-').unwrap_or(mantissa);
            let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, "0"));
            [whole, fraction, exponent].iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    };
    match text {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ if integer(text) || decimal(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()))
        }
        _ if scientific(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(text.into()), Value::Number),
        _ => Value::String(text.to_owned()),
    }
}

/// An XML element value (an object with a `"@"` tag name).
fn is_element(value: &Value) -> bool {
    value.get("@").is_some_and(Value::is_string)
}

/// The XML text of an element: compact, or indented two spaces per level as
/// Snowflake displays XML values.
pub fn render(value: &Value, pretty: bool) -> Option<String> {
    if !is_element(value) {
        return None;
    }
    let mut out = String::new();
    write_element(value, pretty, 0, &mut out);
    Some(out)
}

fn write_element(value: &Value, pretty: bool, depth: usize, out: &mut String) {
    let tag = value["@"].as_str().unwrap_or_default();
    out.push('<');
    out.push_str(tag);
    if let Some(object) = value.as_object() {
        for (key, attribute) in object.iter().filter(|(k, _)| k.len() > 1 && k.starts_with('@')) {
            out.push_str(&format!(" {}=\"{}\"", &key[1..], escape(&text_of(attribute), true)));
        }
    }
    out.push('>');
    let items: Vec<&Value> = match value.get("$") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(items)) => items.iter().collect(),
        Some(item) => vec![item],
    };
    let nested = items.iter().any(|item| is_element(item)) || items.len() > 1;
    if pretty && nested {
        for item in &items {
            out.push('\n');
            out.push_str(&"  ".repeat(depth + 1));
            write_item(item, pretty, depth + 1, out);
        }
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    } else {
        for item in &items {
            write_item(item, pretty, depth + 1, out);
        }
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn write_item(item: &Value, pretty: bool, depth: usize, out: &mut String) {
    if is_element(item) {
        write_element(item, pretty, depth, out);
    } else {
        out.push_str(&escape(&text_of(item), false));
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn escape(text: &str, attribute: bool) -> String {
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    if attribute { escaped.replace('"', "&quot;") } else { escaped }
}

/// `TO_XML`: XML values as they are; anything else in a `SnowflakeData`
/// element with each value's type.
fn to_xml(value: &Value) -> String {
    if let Some(xml) = render(value, false) {
        return xml;
    }
    let mut out = String::new();
    write_typed("SnowflakeData", value, &mut out);
    out
}

fn write_typed(tag: &str, value: &Value, out: &mut String) {
    let kind = match value {
        Value::Object(_) => "OBJECT",
        Value::Array(_) => "ARRAY",
        Value::String(_) => "VARCHAR",
        Value::Bool(_) => "BOOLEAN",
        Value::Number(n) if n.is_i64() || n.is_u64() => "INTEGER",
        Value::Number(n) if n.to_string().contains(['e', 'E']) => "DOUBLE",
        Value::Number(_) => "DECIMAL",
        Value::Null => "NULL_VALUE",
    };
    out.push_str(&format!("<{tag} type=\"{kind}\">"));
    match value {
        Value::Object(object) => object.iter().for_each(|(k, v)| write_typed(k, v, out)),
        Value::Array(items) => items.iter().for_each(|v| write_typed("e", v, out)),
        other => out.push_str(&escape(&text_of(other), false)),
    }
    out.push_str(&format!("</{tag}>"));
}

/// The `instance`th child element named `tag` (0-based), as XMLGET finds it.
fn xmlget(value: &Value, tag: &str, instance: usize) -> Option<Value> {
    let items: Vec<&Value> = match value.get("$")? {
        Value::Array(items) => items.iter().collect(),
        item => vec![item],
    };
    items.into_iter().filter(|item| item.get("@").and_then(Value::as_str) == Some(tag)).nth(instance).cloned()
}

fn json(text: &str) -> Result<Value> {
    Ok(serde_json::from_str(text)?)
}

/// `sf_parse_xml(text, disable_auto_convert) -> VARCHAR` (JSON).
pub struct ParseXml;

impl VArrowScalar for ParseXml {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let texts = input.column(0).as_string::<i32>();
        let disable = input.column(1).as_boolean();
        let out = (0..texts.len())
            .map(|i| -> Result<Option<String>> {
                if texts.is_null(i) {
                    return Ok(None);
                }
                let parsed = parse(texts.value(i), !(disable.is_valid(i) && disable.value(i)))
                    .map_err(|e| format!("Error parsing XML: {e}"))?;
                Ok(Some(parsed.to_string()))
            })
            .collect::<Result<StringArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Boolean], DataType::Utf8)]
    }
}

/// `sf_check_xml(text) -> VARCHAR`: NULL for valid XML, else the parse error.
pub struct CheckXml;

impl VArrowScalar for CheckXml {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let texts = input.column(0).as_string::<i32>();
        let out: StringArray = texts.iter().map(|text| text.and_then(|t| parse(t, true).err())).collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8], DataType::Utf8)]
    }
}

/// `sf_xmlget(object_json, tag, instance) -> VARCHAR` (JSON).
pub struct XmlGet;

impl VArrowScalar for XmlGet {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let objects = input.column(0).as_string::<i32>();
        let tags = input.column(1).as_string::<i32>();
        let instances = input.column(2).as_primitive::<Int64Type>();
        let out = (0..objects.len())
            .map(|i| -> Result<Option<String>> {
                if objects.is_null(i) || tags.is_null(i) || instances.is_null(i) || instances.value(i) < 0 {
                    return Ok(None);
                }
                let found = xmlget(&json(objects.value(i))?, tags.value(i), instances.value(i) as usize);
                Ok(found.map(|v| v.to_string()))
            })
            .collect::<Result<StringArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Utf8, DataType::Int64], DataType::Utf8)]
    }
}

/// `sf_to_xml(json) -> VARCHAR`.
pub struct ToXml;

impl VArrowScalar for ToXml {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let values = input.column(0).as_string::<i32>();
        let out = values
            .iter()
            .map(|v| v.map(|v| json(v).map(|v| to_xml(&v))).transpose())
            .collect::<Result<StringArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8], DataType::Utf8)]
    }
}

/// `sf_xml_text(json) -> VARCHAR`: compact XML for XML values, else NULL
/// (for converting OBJECTs to text).
pub struct XmlText;

impl VArrowScalar for XmlText {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let values = input.column(0).as_string::<i32>();
        let out: StringArray = values
            .iter()
            .map(|v| v.and_then(|v| serde_json::from_str(v).ok()).and_then(|v| render(&v, false)))
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8], DataType::Utf8)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_renders() {
        let v = parse("<a attr=\"123\">text</a>", true).unwrap();
        assert_eq!(v, serde_json::json!({"@": "a", "@attr": 123, "$": "text"}));
        assert_eq!(render(&v, true).unwrap(), "<a attr=\"123\">text</a>");
        let v = parse("<a><b>X</b><b>Y</b></a>", true).unwrap();
        assert_eq!(render(&v, true).unwrap(), "<a>\n  <b>X</b>\n  <b>Y</b>\n</a>");
        assert_eq!(render(&v, false).unwrap(), "<a><b>X</b><b>Y</b></a>");
        assert_eq!(render(&parse("<a/>", true).unwrap(), false).unwrap(), "<a></a>");
    }

    #[test]
    fn reports_mismatched_tags() {
        assert_eq!(
            parse("<name> Invalid </WRONG_CLOSING_TAG>", true).unwrap_err(),
            "no opening tag for </WRONG_CLOSING_TAG>, pos 35"
        );
    }
}
