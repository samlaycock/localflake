//! SQL user-defined functions as DuckDB macros.
//!
//! ```sql
//! CREATE FUNCTION add_one(x NUMBER) RETURNS NUMBER AS 'x + 1';
//! CREATE FUNCTION evens(n INT) RETURNS TABLE (v INT) AS 'SELECT ...';
//! ```
//! sqlparser doesn't parse Snowflake's CREATE FUNCTION, so the statement is
//! read here token by token. The body is Snowflake SQL like any other: it is
//! normalized, parsed and rewritten, then becomes `CREATE MACRO` (scalar) or
//! `CREATE MACRO ... AS TABLE` (table function).

use sqlparser::ast::{DataType, Expr, Statement};
use sqlparser::dialect::SnowflakeDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::ast::cast;
use crate::rewrite::{rewrite_expr, rewrite_statement};
use crate::{Context, SqlError, normalize_identifiers, types};

pub struct Udf {
    pub or_replace: bool,
    pub if_not_exists: bool,
    pub name: String,
    /// `(name, default)`; names are upper-cased like other unquoted identifiers.
    pub args: Vec<(String, Option<String>)>,
    pub returns: Returns,
    pub language: String,
    pub body: String,
}

pub enum Returns {
    Scalar(String),
    Table(Vec<(String, String)>),
}

/// Parses `CREATE [OR REPLACE] [SECURE] [TEMP] FUNCTION ...`, or returns `None`
/// for other statements.
pub fn parse(sql: &str) -> Result<Option<Udf>, SqlError> {
    let tokens: Vec<Token> = Tokenizer::new(&SnowflakeDialect, sql)
        .tokenize()?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect();
    let word = |i: usize| match tokens.get(i) {
        Some(Token::Word(w)) if w.quote_style.is_none() => w.value.to_uppercase(),
        _ => String::new(),
    };
    if word(0) != "CREATE" {
        return Ok(None);
    }
    let mut i = 1;
    let mut or_replace = false;
    while matches!(word(i).as_str(), "OR" | "REPLACE" | "SECURE" | "TEMP" | "TEMPORARY") {
        or_replace |= word(i) == "REPLACE";
        i += 1;
    }
    if word(i) != "FUNCTION" {
        return Ok(None);
    }
    i += 1;
    let mut if_not_exists = false;
    if word(i) == "IF" && word(i + 1) == "NOT" && word(i + 2) == "EXISTS" {
        if_not_exists = true;
        i += 3;
    }
    let mut name = String::new();
    while i < tokens.len() && !matches!(tokens[i], Token::LParen) {
        name.push_str(&tokens[i].to_string());
        i += 1;
    }
    let (arg_tokens, next) = parenthesized(&tokens, i)?;
    i = next;
    let mut args = vec![];
    for arg in split_commas(arg_tokens) {
        let Some(Token::Word(arg_name)) = arg.first() else { continue };
        let default = arg.iter().position(|t| matches!(t, Token::Word(w) if w.value.eq_ignore_ascii_case("DEFAULT")));
        let default = default.map(|d| join(&arg[d + 1..]));
        let arg_name =
            if arg_name.quote_style.is_some() { arg_name.value.clone() } else { arg_name.value.to_uppercase() };
        args.push((arg_name, default));
    }
    if word(i) != "RETURNS" {
        return Err(SqlError::Unsupported("CREATE FUNCTION without RETURNS".into()));
    }
    i += 1;
    let returns = if word(i) == "TABLE" {
        let (columns, next) = parenthesized(&tokens, i + 1)?;
        i = next;
        Returns::Table(
            split_commas(columns)
                .into_iter()
                .filter(|c| !c.is_empty())
                .map(|c| (c[0].to_string().trim_matches('"').to_uppercase(), join(&c[1..])))
                .collect(),
        )
    } else {
        let start = i;
        while i < tokens.len() && !is_option_start(&word(i)) {
            i += 1;
        }
        Returns::Scalar(join(&tokens[start..i]))
    };
    let mut language = "SQL".to_owned();
    let mut body = None;
    while i < tokens.len() {
        match word(i).as_str() {
            "LANGUAGE" => {
                language = word(i + 1);
                i += 2;
            }
            "AS" => {
                body = match tokens.get(i + 1) {
                    Some(Token::SingleQuotedString(s)) => Some(s.clone()),
                    Some(Token::DollarQuotedString(s)) => Some(s.value.clone()),
                    _ => None,
                };
                i += 2;
            }
            _ => i += 1,
        }
    }
    let body = body.ok_or_else(|| SqlError::Unsupported("CREATE FUNCTION without a body".into()))?;
    Ok(Some(Udf { or_replace, if_not_exists, name, args, returns, language, body }))
}

fn is_option_start(word: &str) -> bool {
    matches!(
        word,
        "LANGUAGE"
            | "AS"
            | "COMMENT"
            | "IMMUTABLE"
            | "VOLATILE"
            | "MEMOIZABLE"
            | "STRICT"
            | "CALLED"
            | "RETURNS"
            | "NOT"
            | "NULL"
            | "RUNTIME_VERSION"
            | "HANDLER"
            | "PACKAGES"
            | "IMPORTS"
    )
}

fn parenthesized(tokens: &[Token], start: usize) -> Result<(&[Token], usize), SqlError> {
    if !matches!(tokens.get(start), Some(Token::LParen)) {
        return Err(SqlError::Unsupported("CREATE FUNCTION syntax".into()));
    }
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&tokens[start + 1..i], i + 1));
                }
            }
            _ => {}
        }
    }
    Err(SqlError::Unsupported("CREATE FUNCTION syntax".into()))
}

fn split_commas(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = vec![];
    let (mut depth, mut start) = (0, 0);
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            Token::Comma if depth == 0 => {
                parts.push(&tokens[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < tokens.len() {
        parts.push(&tokens[start..]);
    }
    parts
}

fn join(tokens: &[Token]) -> String {
    tokens.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
}

fn data_type(text: &str, ctx: &Context) -> Result<DataType, SqlError> {
    let parsed = Parser::new(&SnowflakeDialect).try_with_sql(text)?.parse_data_type()?;
    types::map_type(&parsed, ctx)
}

/// The DuckDB statement creating the macro.
pub fn to_macro(udf: &Udf, ctx: &Context) -> Result<String, SqlError> {
    if !udf.language.eq_ignore_ascii_case("SQL") {
        return Err(SqlError::Unsupported(format!("{} user-defined functions", udf.language)));
    }
    let body = normalize_identifiers(&udf.body)?;
    let params = udf
        .args
        .iter()
        .map(|(name, default)| {
            let name = crate::ast::quote(name);
            Ok(match default {
                Some(default) => {
                    let mut value = Parser::new(&SnowflakeDialect).try_with_sql(default)?.parse_expr()?;
                    rewrite_expr(&mut value, ctx)?;
                    format!("{name} := {value}")
                }
                None => name,
            })
        })
        .collect::<Result<Vec<_>, SqlError>>()?
        .join(", ");
    let create = match (udf.or_replace, udf.if_not_exists) {
        (true, _) => "CREATE OR REPLACE MACRO",
        (false, true) => "CREATE MACRO IF NOT EXISTS",
        _ => "CREATE MACRO",
    };
    Ok(match &udf.returns {
        Returns::Scalar(returns) => {
            let mut expr = Parser::new(&SnowflakeDialect).try_with_sql(&body)?.parse_expr()?;
            rewrite_expr(&mut expr, ctx)?;
            let expr: Expr = cast(expr, data_type(returns, ctx)?);
            format!("{create} {}({params}) AS {expr}", udf.name)
        }
        Returns::Table(columns) => {
            let query = Parser::new(&SnowflakeDialect).try_with_sql(&body)?.parse_query()?;
            let mut statement = Statement::Query(query);
            rewrite_statement(&mut statement, ctx, &body)?;
            let names: Vec<String> = (0..columns.len()).map(|i| format!("__c{i}")).collect();
            let projection = columns
                .iter()
                .zip(&names)
                .map(|((name, type_), c)| {
                    Ok(format!("CAST({c} AS {}) AS {}", data_type(type_, ctx)?, crate::ast::quote(name)))
                })
                .collect::<Result<Vec<_>, SqlError>>()?;
            format!(
                "{create} {}({params}) AS TABLE SELECT {} FROM ({statement}) AS __udtf({})",
                udf.name,
                projection.join(", "),
                names.join(", ")
            )
        }
    })
}
