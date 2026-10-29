//! Materialized views, dynamic tables and `ALTER VIEW`.
//!
//! Materialized views and dynamic tables are DuckDB views, so they're always
//! up to date: a dynamic table never lags behind its sources. Their kind (and
//! a dynamic table's target lag, warehouse and refresh mode) is kept in the
//! view's comment behind [`COMMENT_MARKER`], as declared column types are, so
//! metadata can report them as Snowflake does:
//!
//! ```text
//! <marker>DYNAMIC TABLE<sep>1 minute<sep>WH<sep>AUTO<marker>user comment
//! ```
//!
//! sqlparser doesn't parse `ALTER VIEW`, `ALTER MATERIALIZED VIEW` or
//! `DROP DYNAMIC TABLE`, so those are handled here at the token level.

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::ast::quote;
use crate::translate::{COMMENT_MARKER, Command, SqlCommand, StatementKind};
use crate::{Context, SqlError, meta};

/// Separates a dynamic table's options in its comment marker.
const OPTION_SEPARATOR: char = '\u{E003}';

/// The kinds of DuckDB view that are something else in Snowflake.
pub enum ViewKind<'a> {
    Materialized,
    Dynamic { target_lag: &'a str, warehouse: &'a str, refresh_mode: &'a str },
}

/// The DuckDB comment for a view of `kind` with the user's `comment`.
pub fn view_comment(kind: &ViewKind, comment: Option<&str>) -> String {
    let declared = match kind {
        ViewKind::Materialized => "MATERIALIZED VIEW".to_owned(),
        ViewKind::Dynamic { target_lag, warehouse, refresh_mode } => {
            ["DYNAMIC TABLE", target_lag, warehouse, refresh_mode].join(&OPTION_SEPARATOR.to_string())
        }
    };
    format!("{COMMENT_MARKER}{declared}{COMMENT_MARKER}{}", comment.unwrap_or_default())
}

/// Keeps the kind marker of a view's existing comment, replacing the user's comment.
pub fn replace_user_comment(existing: Option<&str>, comment: Option<&str>) -> Option<String> {
    let marker = existing.filter(|c| c.starts_with(COMMENT_MARKER)).and_then(|c| {
        let declared = c[COMMENT_MARKER.len_utf8()..].split(COMMENT_MARKER).next()?;
        Some(format!("{COMMENT_MARKER}{declared}{COMMENT_MARKER}"))
    });
    match (marker, comment) {
        (Some(marker), comment) => Some(format!("{marker}{}", comment.unwrap_or_default())),
        (None, comment) => comment.map(str::to_owned),
    }
}

/// Handles `DROP DYNAMIC TABLE` and `ALTER [MATERIALIZED] VIEW | DYNAMIC TABLE`.
pub fn parse(sql: &str, ctx: &Context) -> Result<Option<Command>, SqlError> {
    let tokens: Vec<Token> = Tokenizer::new(&SnowflakeDialect, sql)
        .tokenize()?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_) | Token::SemiColon))
        .collect();
    let word = |i: usize| match tokens.get(i) {
        Some(Token::Word(w)) if w.quote_style.is_none() => w.value.to_uppercase(),
        _ => String::new(),
    };
    let (verb, object, mut pos) = match (word(0).as_str(), word(1).as_str(), word(2).as_str()) {
        ("DROP", "DYNAMIC", "TABLE") => ("DROP", "Dynamic table", 3),
        ("ALTER", "VIEW", _) => ("ALTER", "View", 2),
        ("ALTER", "MATERIALIZED", "VIEW") => ("ALTER", "Materialized view", 3),
        ("ALTER", "DYNAMIC", "TABLE") => ("ALTER", "Dynamic table", 3),
        _ => return Ok(None),
    };
    let if_exists = word(pos) == "IF" && word(pos + 1) == "EXISTS";
    if if_exists {
        pos += 2;
    }
    let (parts, after) = object_name(&tokens, pos);
    if parts.is_empty() {
        return Err(SqlError::Unsupported(format!("{verb} {} without a name", object.to_uppercase())));
    }
    let name = parts.iter().map(|p| quote(p)).collect::<Vec<_>>().join(".");
    let if_exists_sql = if if_exists { "IF EXISTS " } else { "" };
    let bare = parts.last().unwrap().clone();

    if verb == "DROP" {
        let sql = format!("DROP VIEW {if_exists_sql}{name}");
        let status = format!("{bare} successfully dropped.");
        return Ok(Some(Command::Sql(SqlCommand::new(sql, StatementKind::Ddl).with_status(status))));
    }
    let done = "Statement executed successfully.";
    let command = match (word(after).as_str(), word(after + 1).as_str()) {
        ("RENAME", "TO") => {
            let (new_name, _) = object_name(&tokens, after + 2);
            let new_name = new_name.last().ok_or_else(|| SqlError::Unsupported("RENAME TO without a name".into()))?;
            let sql = format!("ALTER VIEW {if_exists_sql}{name} RENAME TO {}", quote(new_name));
            Command::Sql(SqlCommand::new(sql, StatementKind::Ddl).with_status(done))
        }
        ("SET", "COMMENT") | ("UNSET", "COMMENT") => {
            let comment = match tokens.get(after + 3) {
                Some(Token::SingleQuotedString(s)) if word(after) == "SET" => Some(s.clone()),
                Some(Token::DollarQuotedString(s)) if word(after) == "SET" => Some(s.value.clone()),
                _ if word(after) == "SET" => return Err(SqlError::Unsupported("SET COMMENT without a string".into())),
                _ => None,
            };
            let (database, schema, view) = meta::resolve(&parts, ctx)?;
            Command::CommentOnView { database, schema, view, comment, if_exists }
        }
        // Refreshing, suspending, resuming, clustering, security and policies have no effect on a view.
        _ => {
            let sql = if if_exists { "SELECT 1".to_owned() } else { format!("SELECT * FROM {name} LIMIT 0") };
            Command::Sql(SqlCommand::new(sql, StatementKind::Ddl).with_status(done))
        }
    };
    Ok(Some(command))
}

/// The parts of the object name starting at `pos` (already normalized, so
/// unquoted parts are upper case), and the position after it.
fn object_name(tokens: &[Token], mut pos: usize) -> (Vec<String>, usize) {
    let mut parts = Vec::new();
    while let Some(Token::Word(w)) = tokens.get(pos) {
        parts.push(w.value.clone());
        pos += 1;
        if !matches!(tokens.get(pos), Some(Token::Period)) {
            break;
        }
        pos += 1;
    }
    (parts, pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_user_comment_and_keeps_the_marker() {
        let dynamic = view_comment(
            &ViewKind::Dynamic { target_lag: "1 minute", warehouse: "WH", refresh_mode: "AUTO" },
            Some("old"),
        );
        let replaced = replace_user_comment(Some(&dynamic), Some("new")).unwrap();
        assert_eq!(replaced, dynamic.replace("old", "new"));
        assert_eq!(replace_user_comment(Some(&dynamic), None).unwrap(), dynamic.replace("old", ""));
        assert_eq!(replace_user_comment(Some("plain"), Some("new")).as_deref(), Some("new"));
        assert_eq!(replace_user_comment(None, None), None);
    }
}
