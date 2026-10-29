//! Snowflake identifier case normalization.
//!
//! Snowflake stores unquoted identifiers in upper case and treats quoted
//! identifiers as case-sensitive. DuckDB preserves whatever case it is given,
//! so we upper-case every unquoted word before handing SQL to DuckDB. This
//! happens at the token level, which is safe for keywords and function names
//! (both case-insensitive) and leaves string literals untouched.
//!
//! Quoted identifiers keep their case, but DuckDB resolves names
//! case-insensitively, so `"id"` and `ID` would be the same column. Quoted
//! identifiers containing lower-case letters are therefore encoded with a
//! marker after each lower-case letter ([`encode_identifier`]), which makes
//! case variants distinct names; results and metadata decode them again.
//!
//! The one exception is semi-structured path access: in `v:Foo.bar` the path
//! elements `Foo` and `bar` are case-sensitive keys, not identifiers.

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::SqlError;

pub fn normalize_identifiers(sql: &str) -> Result<String, SqlError> {
    let quoted = crate::stage::quote_file_uris(sql);
    let sql = quoted.as_str();
    let tokens = Tokenizer::new(&SnowflakeDialect, sql).tokenize_with_location()?;
    let offsets = LineOffsets::new(sql);
    // End of a stage location being copied verbatim (see `stage::normalize_location`).
    let mut skip_until = 0;

    let mut out = String::with_capacity(sql.len());
    let mut copied_to = 0;
    let mut in_path = false;
    let mut prev: Option<&Token> = None;

    for twl in &tokens {
        let token = &twl.token;
        if matches!(token, Token::Whitespace(_)) {
            continue;
        }
        let start = offsets.byte_offset(sql, twl.span.start.line, twl.span.start.column);
        if start < skip_until {
            continue;
        }
        if matches!(token, Token::AtSign)
            && let Some((location, end)) = crate::stage::normalize_location(sql, start)
        {
            out.push_str(&sql[copied_to..start]);
            out.push_str(&location);
            copied_to = end;
            skip_until = end;
            prev = Some(token);
            continue;
        }

        let path_element = match token {
            Token::Word(_) => matches!(prev, Some(Token::Colon)) || (in_path && matches!(prev, Some(Token::Period))),
            _ => false,
        };
        in_path = match token {
            Token::Word(_) => path_element,
            Token::Period | Token::LBracket | Token::RBracket | Token::Number(..) | Token::SingleQuotedString(_) => {
                in_path
            }
            Token::Colon => true,
            _ => false,
        };

        if let Token::Word(word) = token
            && !path_element
        {
            let start = offsets.byte_offset(sql, twl.span.start.line, twl.span.start.column);
            let end = offsets.byte_offset(sql, twl.span.end.line, twl.span.end.column);
            match word.quote_style {
                None => {
                    out.push_str(&sql[copied_to..start]);
                    out.push_str(&sql[start..end].to_uppercase());
                    copied_to = end;
                }
                Some('"') if word.value.chars().any(char::is_lowercase) => {
                    out.push_str(&sql[copied_to..start]);
                    out.push('"');
                    out.push_str(&encode_identifier(&word.value).replace('"', "\"\""));
                    out.push('"');
                    copied_to = end;
                }
                _ => {}
            }
        }
        prev = Some(token);
    }
    out.push_str(&sql[copied_to..]);
    Ok(out)
}

/// Follows each lower-case letter of a quoted identifier with this marker.
pub const CASE_MARKER: char = '\u{E002}';

/// The DuckDB name for a case-sensitive Snowflake identifier.
pub fn encode_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        out.push(c);
        if c.is_lowercase() {
            out.push(CASE_MARKER);
        }
    }
    out
}

/// The Snowflake name for a DuckDB name.
pub fn decode_identifier(name: &str) -> String {
    name.replace(CASE_MARKER, "")
}

/// Maps the tokenizer's 1-based (line, column-in-chars) locations to byte offsets.
pub(crate) struct LineOffsets(Vec<usize>);

impl LineOffsets {
    pub(crate) fn new(sql: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(sql.match_indices('\n').map(|(i, _)| i + 1));
        Self(starts)
    }

    pub(crate) fn byte_offset(&self, sql: &str, line: u64, column: u64) -> usize {
        let line_start = self.0[(line - 1) as usize];
        sql[line_start..].char_indices().nth((column - 1) as usize).map_or(sql.len(), |(i, _)| line_start + i)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_identifiers as n;
    use super::{decode_identifier, encode_identifier};

    #[test]
    fn uppercases_unquoted_identifiers() {
        assert_eq!(n("select a, b from t").unwrap(), "SELECT A, B FROM T");
    }

    #[test]
    fn encodes_quoted_identifiers_and_preserves_literals() {
        let encoded = n(r#"select "a", "B", 'x y' from "My Table""#).unwrap();
        assert_eq!(
            encoded,
            format!(r#"SELECT "{}", "B", 'x y' FROM "{}""#, encode_identifier("a"), encode_identifier("My Table"))
        );
        assert_eq!(decode_identifier(&encoded), r#"SELECT "a", "B", 'x y' FROM "My Table""#);
    }

    #[test]
    fn preserves_variant_path_elements() {
        assert_eq!(
            n("select v:Foo.bar[0].Baz, v::string from t").unwrap(),
            "SELECT V:Foo.bar[0].Baz, V::STRING FROM T"
        );
    }

    #[test]
    fn handles_multiline_and_unicode() {
        assert_eq!(
            n("select 'é', x\nfrom t -- note\nwhere y = 1").unwrap(),
            "SELECT 'é', X\nFROM T -- note\nWHERE Y = 1"
        );
    }
}
