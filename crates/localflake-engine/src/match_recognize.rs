//! The row pattern matcher behind MATCH_RECOGNIZE.
//!
//! The translator evaluates each DEFINE condition per row and passes, per
//! partition, the rows' symbol bitmasks (in ORDER BY order) plus the pattern
//! in a small prefix notation:
//!
//! ```text
//! #2             symbol 2           ^ $        partition start / end
//! (c p q ...)    concatenation      (| p q)    alternation
//! (r min max g p) repetition; max -1 = unbounded, g = 1 greedy / 0 reluctant
//! ```
//!
//! The result lists every matched row as `row:match:symbol`, `;`-separated.
//! Matching is leftmost, with regex preference order (greedy quantifiers
//! longest first, alternatives left to right); empty matches are skipped.

use std::sync::Arc;

use duckdb::arrow::array::{Array, ArrayRef, AsArray, RecordBatch, StringArray};
use duckdb::arrow::datatypes::DataType;
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
enum Node {
    Symbol(usize),
    Start,
    End,
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, usize, Option<usize>, bool),
}

fn parse(text: &str) -> Result<Node> {
    let tokens: Vec<String> =
        text.replace('(', " ( ").replace(')', " ) ").split_whitespace().map(str::to_owned).collect();
    let mut pos = 0;
    let node = parse_node(&tokens, &mut pos)?;
    Ok(node)
}

fn parse_node(tokens: &[String], pos: &mut usize) -> Result<Node> {
    let token = tokens.get(*pos).ok_or("unexpected end of pattern")?.clone();
    *pos += 1;
    Ok(match token.as_str() {
        "^" => Node::Start,
        "$" => Node::End,
        t if t.starts_with('#') => Node::Symbol(t[1..].parse()?),
        "(" => {
            let kind = tokens.get(*pos).ok_or("bad pattern")?.clone();
            *pos += 1;
            let node = match kind.as_str() {
                "c" | "|" => {
                    let mut items = vec![];
                    while tokens.get(*pos).map(String::as_str) != Some(")") {
                        items.push(parse_node(tokens, pos)?);
                    }
                    if kind == "c" { Node::Concat(items) } else { Node::Alt(items) }
                }
                "r" => {
                    let min: usize = tokens[*pos].parse()?;
                    let max: i64 = tokens[*pos + 1].parse()?;
                    let greedy = tokens[*pos + 2] == "1";
                    *pos += 3;
                    let inner = parse_node(tokens, pos)?;
                    Node::Repeat(Box::new(inner), min, (max >= 0).then_some(max as usize), greedy)
                }
                other => return Err(format!("bad pattern node {other}").into()),
            };
            if tokens.get(*pos).map(String::as_str) != Some(")") {
                return Err("unbalanced pattern".into());
            }
            *pos += 1;
            node
        }
        other => return Err(format!("bad pattern token {other}").into()),
    })
}

/// Tries to match `node` at `pos`, then the continuation `k`. Records the
/// symbol of each consumed row in `labels`.
fn matches(
    node: &Node,
    rows: &[u64],
    pos: usize,
    labels: &mut Vec<usize>,
    k: &mut dyn FnMut(usize, &mut Vec<usize>) -> bool,
) -> bool {
    match node {
        Node::Symbol(s) => {
            if pos < rows.len() && rows[pos] & (1 << s) != 0 {
                labels.push(*s);
                if k(pos + 1, labels) {
                    return true;
                }
                labels.pop();
            }
            false
        }
        Node::Start => pos == 0 && k(pos, labels),
        Node::End => pos == rows.len() && k(pos, labels),
        Node::Concat(items) => concat(items, rows, pos, labels, k),
        Node::Alt(items) => items.iter().any(|item| matches(item, rows, pos, labels, k)),
        Node::Repeat(inner, min, max, greedy) => repeat(inner, *min, *max, *greedy, 0, rows, pos, labels, k),
    }
}

fn concat(
    items: &[Node],
    rows: &[u64],
    pos: usize,
    labels: &mut Vec<usize>,
    k: &mut dyn FnMut(usize, &mut Vec<usize>) -> bool,
) -> bool {
    match items.split_first() {
        None => k(pos, labels),
        Some((first, rest)) => matches(first, rows, pos, labels, &mut |p, l| concat(rest, rows, p, l, k)),
    }
}

#[allow(clippy::too_many_arguments)]
fn repeat(
    inner: &Node,
    min: usize,
    max: Option<usize>,
    greedy: bool,
    count: usize,
    rows: &[u64],
    pos: usize,
    labels: &mut Vec<usize>,
    k: &mut dyn FnMut(usize, &mut Vec<usize>) -> bool,
) -> bool {
    let can_stop = count >= min;
    let can_continue = max.is_none_or(|m| count < m);
    let more = |labels: &mut Vec<usize>, k: &mut dyn FnMut(usize, &mut Vec<usize>) -> bool| {
        can_continue
            && matches(inner, rows, pos, labels, &mut |p, l| {
                // An iteration that consumes nothing can't make progress.
                p > pos && repeat(inner, min, max, greedy, count + 1, rows, p, l, k)
            })
    };
    // Greedy tries another iteration before stopping; reluctant the reverse.
    if greedy && more(labels, k) {
        return true;
    }
    if can_stop && k(pos, labels) {
        return true;
    }
    !greedy && more(labels, k)
}

/// All matches in one partition, as `(row index, match number, symbol)`.
fn find_matches(pattern: &Node, rows: &[u64], skip_to_next_row: bool) -> Vec<(usize, usize, usize)> {
    let mut out = vec![];
    let mut start = 0;
    let mut number = 0;
    while start < rows.len() {
        let mut found: Option<(usize, Vec<usize>)> = None;
        let mut labels = vec![];
        matches(pattern, rows, start, &mut labels, &mut |end, l| {
            if end > start {
                found = Some((end, l.clone()));
                true
            } else {
                false
            }
        });
        match found {
            Some((end, labels)) => {
                number += 1;
                out.extend(labels.iter().enumerate().map(|(i, s)| (start + i, number, *s)));
                start = if skip_to_next_row { start + 1 } else { end };
            }
            None => start += 1,
        }
    }
    out
}

/// `sf_match_recognize(bitmasks, pattern, skip)`: bitmasks is the partition's
/// comma-separated row bitmasks; skip is `PAST LAST ROW` or `TO NEXT ROW`.
pub struct MatchRecognize;

impl VArrowScalar for MatchRecognize {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (masks, patterns, skips) = (
            input.column(0).as_string::<i32>(),
            input.column(1).as_string::<i32>(),
            input.column(2).as_string::<i32>(),
        );
        let out: StringArray = (0..masks.len())
            .map(|i| {
                if masks.is_null(i) {
                    return Ok(None);
                }
                let rows: Vec<u64> = masks
                    .value(i)
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::parse)
                    .collect::<std::result::Result<_, _>>()?;
                let pattern = parse(patterns.value(i))?;
                let found = find_matches(&pattern, &rows, skips.value(i) == "TO NEXT ROW");
                Ok(Some(found.iter().map(|(row, m, s)| format!("{}:{m}:{s}", row + 1)).collect::<Vec<_>>().join(";")))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Utf8, DataType::Utf8], DataType::Utf8)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pattern: &str, rows: &[u64]) -> Vec<(usize, usize, usize)> {
        find_matches(&parse(pattern).unwrap(), rows, false)
    }

    #[test]
    fn matches_rising_runs() {
        // A = start (any row), B = price up: pattern A B+
        let rows = [0b01, 0b11, 0b11, 0b01, 0b11];
        assert_eq!(run("(c #0 (r 1 -1 1 #1))", &rows), vec![(0, 1, 0), (1, 1, 1), (2, 1, 1), (3, 2, 0), (4, 2, 1)]);
    }

    #[test]
    fn alternation_and_anchors() {
        assert_eq!(run("(c ^ (| #0 #1))", &[0b10, 0b01]), vec![(0, 1, 1)]);
        // Empty matches are skipped, so a reluctant star takes the shortest non-empty match.
        assert_eq!(run("(r 0 -1 0 #0)", &[1, 1]), vec![(0, 1, 0), (1, 2, 0)]);
    }
}
