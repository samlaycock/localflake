//! Conformance corpus: `tests/corpus/*.test` files of Snowflake SQL with the
//! results Snowflake gives. Every file runs in a fresh session and all
//! mismatches are reported together.
//!
//! ```text
//! # comment
//! statement ok
//! create table t (a int)
//!
//! query [rowsort] [types]
//! select a, 'x' from t
//! ----
//! NUMBER(*,0)|VARCHAR         <- only with `types`
//! 1|x
//!
//! error 002003
//! select * from missing
//! ```
//!
//! Append `gap` to a directive (`query gap`, `error 002003 gap`) to mark a
//! known gap: it is reported but doesn't fail the run, and a gap that starts
//! passing is reported too so the marker can be removed.
//!
//! Values are shown the way Snowflake displays them: NULL, true/false, dates
//! as YYYY-MM-DD, timestamps as YYYY-MM-DD HH:MI:SS[.fraction], binary as hex,
//! semi-structured values as compact JSON. `(empty)` is the empty string.
//! Set `CORPUS_FILTER` to run only files whose name contains it, and
//! `CORPUS_SLOW=<ms>` to list cases slower than that.

use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, NaiveDate};
use localflake_engine::{Column, Engine, QueryResult, Request, Session, SessionOptions, SfType};

#[test]
fn corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let filter = std::env::var("CORPUS_FILTER").unwrap_or_default();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "test"))
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(&filter))
        .collect();
    files.sort();

    let engine = Engine::open(None).unwrap();
    let mut failures = Vec::new();
    let mut gaps = Vec::new();
    let mut fixed_gaps = Vec::new();
    let mut total = 0;
    let slow: Option<u128> = std::env::var("CORPUS_SLOW").ok().and_then(|v| v.parse().ok());
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(path).unwrap();
        let database =
            format!("CORPUS_{}", name.trim_end_matches(".test").replace(|c: char| !c.is_alphanumeric(), "_"));
        let mut session = engine
            .session(SessionOptions {
                user: "TESTER".into(),
                database: Some(database.to_uppercase()),
                ..Default::default()
            })
            .unwrap();
        for case in parse(&text) {
            total += 1;
            let location = format!("{name}:{}: {}", case.line, case.sql.trim());
            let started = std::time::Instant::now();
            let outcome = case.run(&mut session);
            if slow.is_some_and(|ms| started.elapsed().as_millis() >= ms) {
                eprintln!("slow ({} ms): {location}", started.elapsed().as_millis());
            }
            match (outcome, case.gap) {
                (Err(message), false) => failures.push(format!("{location}\n{message}")),
                (Err(_), true) => gaps.push(location),
                (Ok(()), true) => fixed_gaps.push(location),
                (Ok(()), false) => {}
            }
        }
    }
    eprintln!("corpus: {} of {total} cases pass, {} known gaps", total - failures.len() - gaps.len(), gaps.len());
    if std::env::var_os("CORPUS_SHOW_GAPS").is_some() {
        eprintln!("known gaps:\n  {}", gaps.join("\n  "));
    }
    if !fixed_gaps.is_empty() {
        eprintln!("gaps that now pass (remove the `gap` marker):\n  {}", fixed_gaps.join("\n  "));
    }
    if !failures.is_empty() {
        panic!("{} corpus failures:\n\n{}", failures.len(), failures.join("\n\n"));
    }
}

enum Expect {
    Ok,
    Error(String),
    Rows { rowsort: bool, types: bool, rows: Vec<String> },
}

struct Case {
    line: usize,
    sql: String,
    expect: Expect,
    gap: bool,
}

fn parse(text: &str) -> Vec<Case> {
    let lines: Vec<&str> = text.lines().collect();
    let mut cases = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let header = lines[i].trim();
        if header.is_empty() || header.starts_with('#') {
            i += 1;
            continue;
        }
        let line = i + 1;
        i += 1;
        let mut sql = String::new();
        while i < lines.len() && !lines[i].trim().is_empty() && lines[i].trim() != "----" {
            sql.push_str(lines[i]);
            sql.push('\n');
            i += 1;
        }
        let gap = header.split_whitespace().any(|w| w == "gap");
        let mut words = header.split_whitespace().filter(|w| *w != "gap");
        let expect = match words.next() {
            Some("statement") => Expect::Ok,
            Some("error") => Expect::Error(words.collect::<Vec<_>>().join(" ")),
            Some("query") => {
                let options: Vec<&str> = words.collect();
                let mut rows = Vec::new();
                if i < lines.len() && lines[i].trim() == "----" {
                    i += 1;
                    while i < lines.len() && !lines[i].trim().is_empty() {
                        rows.push(lines[i].trim_end().to_owned());
                        i += 1;
                    }
                }
                Expect::Rows { rowsort: options.contains(&"rowsort"), types: options.contains(&"types"), rows }
            }
            other => panic!("line {line}: unknown directive {other:?}"),
        };
        cases.push(Case { line, sql, expect, gap });
    }
    cases
}

impl Case {
    fn run(&self, session: &mut Session) -> Result<(), String> {
        let outcome = session.execute(&Request { sql: &self.sql, ..Default::default() });
        match (&self.expect, outcome) {
            (Expect::Ok, Ok(_)) => Ok(()),
            (Expect::Error(expected), Err(failure)) => {
                let err = failure.error;
                if err.code == expected || err.message.contains(expected.as_str()) || expected.is_empty() {
                    Ok(())
                } else {
                    Err(format!("  expected error {expected}\n  got error {}: {}", err.code, err.message))
                }
            }
            (Expect::Rows { rowsort, types, rows }, Ok(results)) => {
                let result = results.last().unwrap();
                let mut actual = render(result).map_err(|e| format!("  render failed: {e}"))?;
                let mut expected = rows.clone();
                if *rowsort {
                    actual.sort();
                    expected.sort();
                }
                if *types {
                    actual.insert(0, result.columns.iter().map(type_name).collect::<Vec<_>>().join("|"));
                }
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "  expected:\n    {}\n  actual:\n    {}",
                        expected.join("\n    "),
                        actual.join("\n    ")
                    ))
                }
            }
            (_, Err(failure)) => Err(format!("  failed: {}: {}", failure.error.code, failure.error.message)),
            (Expect::Error(expected), Ok(_)) => Err(format!("  expected error {expected}, but it succeeded")),
        }
    }
}

fn render(result: &QueryResult) -> Result<Vec<String>, String> {
    let rows = result.json_rows().map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|row| {
            row.iter()
                .zip(&result.columns)
                .map(|(value, column)| match value {
                    None => "NULL".to_owned(),
                    Some(v) => display(v, column),
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect())
}

fn display(value: &str, column: &Column) -> String {
    let out = match column.sf_type {
        SfType::Boolean => (if value == "1" { "true" } else { "false" }).to_owned(),
        SfType::Date => {
            let days: i64 = value.parse().unwrap();
            (NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() + chrono::Duration::days(days)).to_string()
        }
        SfType::Time => {
            let (secs, frac) = split_fraction(value);
            let secs: i64 = secs.parse().unwrap();
            format!("{:02}:{:02}:{:02}{frac}", secs / 3600, secs / 60 % 60, secs % 60)
        }
        SfType::TimestampNtz | SfType::TimestampLtz => {
            let (secs, frac) = split_fraction(value);
            let ts = DateTime::from_timestamp(secs.parse().unwrap(), 0).unwrap().naive_utc();
            let mut s = ts.format("%Y-%m-%d %H:%M:%S").to_string();
            s.push_str(&frac);
            if column.sf_type == SfType::TimestampLtz {
                s.push_str(" Z");
            }
            s
        }
        SfType::Real => match value.parse::<f64>() {
            Ok(f) if f.is_nan() => "NaN".to_owned(),
            Ok(f) if f.is_infinite() => (if f > 0.0 { "inf" } else { "-inf" }).to_owned(),
            Ok(f) => f.to_string(),
            Err(_) => value.to_owned(),
        },
        // "<epoch>.<nanos> <offset minutes + 1440>" → local time and offset.
        SfType::TimestampTz => {
            let (instant, zone) = value.split_once(' ').unwrap_or((value, "1440"));
            let offset = zone.parse::<i64>().unwrap_or(1440) - 1440;
            let (secs, frac) = split_fraction(instant);
            let local = DateTime::from_timestamp(secs.parse::<i64>().unwrap() + offset * 60, 0).unwrap().naive_utc();
            let sign = if offset < 0 { '-' } else { '+' };
            format!(
                "{}{frac} {sign}{:02}{:02}",
                local.format("%Y-%m-%d %H:%M:%S"),
                offset.abs() / 60,
                offset.abs() % 60
            )
        }
        SfType::Variant | SfType::Object | SfType::Array => match serde_json::from_str::<serde_json::Value>(value) {
            Ok(json) => json.to_string(),
            // Not strict JSON (e.g. `undefined` array elements): drop the pretty-printing.
            Err(_) => value.lines().map(str::trim).collect(),
        },
        _ => value.to_owned(),
    };
    if out.is_empty() { "(empty)".to_owned() } else { out }
}

/// `"12.500000000"` → (`"12"`, `".5"`); a zero fraction is dropped.
fn split_fraction(value: &str) -> (&str, String) {
    match value.split_once('.') {
        Some((whole, frac)) => {
            let frac = frac.trim_end_matches('0');
            (whole, if frac.is_empty() { String::new() } else { format!(".{frac}") })
        }
        None => (value, String::new()),
    }
}

fn type_name(column: &Column) -> String {
    let mut s = String::new();
    match column.sf_type {
        SfType::Fixed => {
            // Only the scale is compared: Snowflake reports exact precision for literals.
            write!(s, "NUMBER(*,{})", column.scale.unwrap_or(0)).unwrap();
        }
        SfType::Real => s.push_str("FLOAT"),
        SfType::Text => s.push_str("VARCHAR"),
        SfType::Boolean => s.push_str("BOOLEAN"),
        SfType::Date => s.push_str("DATE"),
        SfType::Time => s.push_str("TIME"),
        SfType::TimestampNtz => s.push_str("TIMESTAMP_NTZ"),
        SfType::TimestampLtz => s.push_str("TIMESTAMP_LTZ"),
        SfType::TimestampTz => s.push_str("TIMESTAMP_TZ"),
        SfType::Binary => s.push_str("BINARY"),
        SfType::Variant => s.push_str("VARIANT"),
        SfType::Object => s.push_str("OBJECT"),
        SfType::Array => s.push_str("ARRAY"),
        SfType::Vector => {
            let (element, dimension) = column.vector.unwrap_or((SfType::Real, 0));
            let element = if element == SfType::Fixed { "INT" } else { "FLOAT" };
            write!(s, "VECTOR({element}, {dimension})").unwrap();
        }
    }
    s
}
