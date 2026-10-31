//! Stored procedures written in Snowflake Scripting: parsing `CREATE
//! PROCEDURE`, `CALL` and `DROP PROCEDURE`. The engine stores procedures in
//! each database's hidden `__localflake` schema and runs their bodies with
//! its Snowflake Scripting interpreter.

use sqlparser::tokenizer::Token;

use crate::SqlError;
use crate::scripting::{tokens_with_offsets, word_of};

#[derive(Debug, Clone, PartialEq)]
pub struct Procedure {
    /// The name as written: `[db.][schema.]name`, normalized.
    pub name: Vec<String>,
    pub or_replace: bool,
    pub if_not_exists: bool,
    pub arguments: Vec<Argument>,
    pub returns: Returns,
    pub language: String,
    /// The body: a Snowflake Scripting block, as written.
    pub body: String,
    /// Whether the body was part of the (normalized) statement rather than a string literal.
    pub body_normalized: bool,
    pub comment: Option<String>,
    pub execute_as: String,
    pub null_input: String,
    pub secure: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    pub name: String,
    /// `OUT`: the procedure's final value is passed back to the caller's variable.
    pub out: bool,
    pub data_type: String,
    /// The default value's expression, if the argument is optional.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Returns {
    Scalar {
        data_type: String,
        not_null: bool,
    },
    /// `RETURNS TABLE (name type, ...)`; empty for `RETURNS TABLE ()`.
    Table(Vec<(String, String)>),
}

impl Returns {
    /// As SHOW PROCEDURES and DESCRIBE print it.
    pub fn to_sql(&self) -> String {
        match self {
            Returns::Scalar { data_type, not_null } => {
                format!("{data_type}{}", if *not_null { " NOT NULL" } else { "" })
            }
            Returns::Table(columns) => {
                let columns: Vec<String> = columns.iter().map(|(n, t)| format!("{n} {t}")).collect();
                format!("TABLE ({})", columns.join(", "))
            }
        }
    }
}

/// `CALL name(args) [INTO :var]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: Vec<String>,
    /// Each argument's expression, with its name for `name => expr`.
    pub arguments: Vec<(Option<String>, String)>,
    pub into: Option<String>,
}

struct Cursor<'a> {
    sql: &'a str,
    tokens: Vec<(Token, usize, usize)>,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(sql: &'a str) -> Result<Self, SqlError> {
        Ok(Self { sql, tokens: tokens_with_offsets(sql)?, pos: 0 })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|t| &t.0)
    }

    fn word(&self) -> Option<String> {
        self.peek().and_then(word_of)
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let found = self.word().as_deref() == Some(word);
        if found {
            self.pos += 1;
        }
        found
    }

    fn eat(&mut self, token: &Token) -> bool {
        let found = self.peek() == Some(token);
        if found {
            self.pos += 1;
        }
        found
    }

    fn error(&self, what: &str) -> SqlError {
        let near = self.peek().map_or("end of input".to_owned(), |t| format!("'{t}'"));
        SqlError::Unsupported(format!("{what} near {near}"))
    }

    fn expect(&mut self, token: &Token) -> Result<(), SqlError> {
        if self.eat(token) { Ok(()) } else { Err(self.error(&format!("expected '{token}'"))) }
    }

    fn name_part(&mut self) -> Result<String, SqlError> {
        match self.peek() {
            Some(Token::Word(w)) => {
                let name = if w.quote_style.is_some() { w.value.clone() } else { w.value.to_uppercase() };
                self.pos += 1;
                Ok(name)
            }
            _ => Err(self.error("expected a name")),
        }
    }

    /// `a.b.c`.
    fn object_name(&mut self) -> Result<Vec<String>, SqlError> {
        let mut parts = vec![self.name_part()?];
        while self.eat(&Token::Period) {
            parts.push(self.name_part()?);
        }
        Ok(parts)
    }

    fn text(&self, from: usize, to: usize) -> String {
        if from >= to {
            return String::new();
        }
        self.sql[self.tokens[from].1..self.tokens[to - 1].2].trim().to_owned()
    }

    /// Text up to a `,` or `)` (or one of `stops`) outside parentheses.
    fn item(&mut self, stops: &[&str]) -> String {
        let start = self.pos;
        let mut depth = 0;
        while let Some(token) = self.peek() {
            match token {
                Token::LParen => depth += 1,
                Token::RParen if depth == 0 => break,
                Token::RParen => depth -= 1,
                Token::Comma if depth == 0 => break,
                _ if depth == 0 && self.word().is_some_and(|w| stops.contains(&w.as_str())) => break,
                _ => {}
            }
            self.pos += 1;
        }
        self.text(start, self.pos)
    }

    fn string(&mut self) -> Result<String, SqlError> {
        let value = match self.peek() {
            Some(Token::SingleQuotedString(s)) => s.replace("''", "'"),
            Some(Token::DollarQuotedString(s)) => s.value.clone(),
            _ => return Err(self.error("expected a string")),
        };
        self.pos += 1;
        Ok(value)
    }
}

/// Parses `CREATE [OR REPLACE] [SECURE] PROCEDURE ...`.
pub fn parse_create(sql: &str) -> Result<Option<Procedure>, SqlError> {
    let mut c = Cursor::new(sql)?;
    if !c.eat_word("CREATE") {
        return Ok(None);
    }
    let or_replace = c.eat_word("OR") && c.eat_word("REPLACE");
    let secure = c.eat_word("SECURE");
    c.eat_word("TEMPORARY");
    c.eat_word("TEMP");
    if !c.eat_word("PROCEDURE") {
        return Ok(None);
    }
    let if_not_exists = c.eat_word("IF") && c.eat_word("NOT") && c.eat_word("EXISTS");
    let name = c.object_name()?;
    c.expect(&Token::LParen)?;
    let mut arguments = Vec::new();
    while !c.eat(&Token::RParen) {
        let arg_name = c.name_part()?;
        let out = c.eat_word("OUT") || c.eat_word("OUTPUT");
        let _ = c.eat_word("IN") || c.eat_word("INPUT");
        let data_type = c.item(&["DEFAULT"]);
        let default = c.eat_word("DEFAULT").then(|| c.item(&[]));
        arguments.push(Argument { name: arg_name, out, data_type: data_type.to_uppercase(), default });
        if !c.eat(&Token::Comma) {
            c.expect(&Token::RParen)?;
            break;
        }
    }
    let (mut returns, mut language, mut comment) = (None, "SQL".to_owned(), None);
    let (mut execute_as, mut null_input) = ("OWNER".to_owned(), "CALLED ON NULL INPUT".to_owned());
    loop {
        match c.word().as_deref() {
            Some("RETURNS") if returns.is_none() => {
                c.pos += 1;
                if c.eat_word("TABLE") {
                    c.expect(&Token::LParen)?;
                    let mut columns = Vec::new();
                    while !c.eat(&Token::RParen) {
                        let column = c.name_part()?;
                        columns.push((column, c.item(&[]).to_uppercase()));
                        if !c.eat(&Token::Comma) {
                            c.expect(&Token::RParen)?;
                            break;
                        }
                    }
                    returns = Some(Returns::Table(columns));
                } else {
                    let data_type = c.item(&[
                        "NOT",
                        "NULL",
                        "LANGUAGE",
                        "AS",
                        "COMMENT",
                        "EXECUTE",
                        "CALLED",
                        "RETURNS",
                        "STRICT",
                        "VOLATILE",
                        "IMMUTABLE",
                        "COPY",
                    ]);
                    let not_null = c.eat_word("NOT") && c.eat_word("NULL");
                    c.eat_word("NULL");
                    returns = Some(Returns::Scalar { data_type: data_type.to_uppercase(), not_null });
                }
            }
            Some("RETURNS") => {
                // RETURNS NULL ON NULL INPUT
                c.pos += 1;
                for word in ["NULL", "ON", "NULL", "INPUT"] {
                    c.eat_word(word);
                }
                null_input = "RETURNS NULL ON NULL INPUT".into();
            }
            Some("LANGUAGE") => {
                c.pos += 1;
                language = c.name_part()?;
            }
            Some("CALLED") => {
                c.pos += 1;
                for word in ["ON", "NULL", "INPUT"] {
                    c.eat_word(word);
                }
                null_input = "CALLED ON NULL INPUT".into();
            }
            Some("STRICT") => {
                c.pos += 1;
                null_input = "RETURNS NULL ON NULL INPUT".into();
            }
            Some("VOLATILE" | "IMMUTABLE") => c.pos += 1,
            Some("COPY") => {
                c.pos += 1;
                c.eat_word("GRANTS");
            }
            Some("COMMENT") => {
                c.pos += 1;
                c.eat(&Token::Eq);
                comment = Some(c.string()?);
            }
            Some("EXECUTE") => {
                c.pos += 1;
                c.eat_word("AS");
                execute_as = if c.eat_word("RESTRICTED") { "RESTRICTED CALLER".into() } else { c.name_part()? };
            }
            Some("AS") => {
                c.pos += 1;
                break;
            }
            _ => return Err(c.error("unexpected text in CREATE PROCEDURE")),
        }
    }
    let returns = returns.ok_or_else(|| c.error("CREATE PROCEDURE without RETURNS"))?;
    let (body, body_normalized) = match c.peek() {
        Some(Token::SingleQuotedString(_) | Token::DollarQuotedString(_)) => (c.string()?, false),
        _ => (c.text(c.pos, c.tokens.len()), true),
    };
    if !language.eq_ignore_ascii_case("SQL") {
        return Err(SqlError::Unsupported(format!("{language} stored procedures")));
    }
    Ok(Some(Procedure {
        name,
        or_replace,
        if_not_exists,
        arguments,
        returns,
        language: language.to_uppercase(),
        body: body.trim().trim_end_matches(';').trim_end().to_owned(),
        body_normalized,
        comment,
        execute_as,
        null_input,
        secure,
    }))
}

/// Parses `CALL name(args) [INTO :var]`.
pub fn parse_call(sql: &str) -> Result<Option<Call>, SqlError> {
    let mut c = Cursor::new(sql)?;
    if !c.eat_word("CALL") {
        return Ok(None);
    }
    let name = c.object_name()?;
    c.expect(&Token::LParen)?;
    let mut arguments = Vec::new();
    while !c.eat(&Token::RParen) {
        let named = match (c.peek(), c.tokens.get(c.pos + 1).map(|t| &t.0)) {
            (Some(Token::Word(_)), Some(Token::RArrow)) => {
                let name = c.name_part()?;
                c.pos += 1;
                Some(name)
            }
            _ => None,
        };
        arguments.push((named, c.item(&[])));
        if !c.eat(&Token::Comma) {
            c.expect(&Token::RParen)?;
            break;
        }
    }
    let into = if c.eat_word("INTO") {
        c.eat(&Token::Colon);
        Some(c.name_part()?)
    } else {
        None
    };
    if c.peek().is_some() {
        return Err(c.error("unexpected text after CALL"));
    }
    Ok(Some(Call { name, arguments, into }))
}

/// A procedure overload as DROP and DESCRIBE name it: `name(type, ...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Overload {
    pub name: Vec<String>,
    pub types: Vec<String>,
}

/// Parses `DROP PROCEDURE [IF EXISTS] name(type, ...)`, and whether it has IF EXISTS.
pub fn parse_drop(sql: &str) -> Result<Option<(Overload, bool)>, SqlError> {
    let mut c = Cursor::new(sql)?;
    if !(c.eat_word("DROP") && c.eat_word("PROCEDURE")) {
        return Ok(None);
    }
    let if_exists = c.eat_word("IF") && c.eat_word("EXISTS");
    let name = c.object_name()?;
    let types = signature(&mut c)?;
    Ok(Some((Overload { name, types }, if_exists)))
}

/// Parses `DESC[RIBE] PROCEDURE name(type, ...)`.
pub fn parse_describe(sql: &str) -> Result<Option<Overload>, SqlError> {
    let mut c = Cursor::new(sql)?;
    if !((c.eat_word("DESC") || c.eat_word("DESCRIBE")) && c.eat_word("PROCEDURE")) {
        return Ok(None);
    }
    let name = c.object_name()?;
    Ok(Some(Overload { name, types: signature(&mut c)? }))
}

/// `(type, ...)` after a procedure name in DROP and DESCRIBE.
fn signature(c: &mut Cursor) -> Result<Vec<String>, SqlError> {
    let mut types = Vec::new();
    if c.eat(&Token::LParen) {
        while !c.eat(&Token::RParen) {
            types.push(c.item(&[]).to_uppercase());
            if !c.eat(&Token::Comma) {
                c.expect(&Token::RParen)?;
                break;
            }
        }
    }
    Ok(types)
}

/// The base type of an argument type, for matching signatures:
/// `NUMBER(12,2)` and `INT` are both NUMBER, `VARCHAR(10)` and `STRING` are VARCHAR.
pub fn type_family(data_type: &str) -> &'static str {
    let base = data_type.split('(').next().unwrap_or_default().trim().to_uppercase();
    match base.as_str() {
        "NUMBER" | "DECIMAL" | "NUMERIC" | "INT" | "INTEGER" | "BIGINT" | "SMALLINT" | "TINYINT" | "BYTEINT" => {
            "NUMBER"
        }
        "FLOAT" | "FLOAT4" | "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" | "REAL" => "FLOAT",
        "VARCHAR" | "STRING" | "TEXT" | "CHAR" | "CHARACTER" | "NCHAR" | "NVARCHAR" | "NVARCHAR2" | "CHAR VARYING" => {
            "VARCHAR"
        }
        "BINARY" | "VARBINARY" => "BINARY",
        "BOOLEAN" => "BOOLEAN",
        "DATE" => "DATE",
        "TIME" => "TIME",
        "TIMESTAMP_LTZ" => "TIMESTAMP_LTZ",
        "TIMESTAMP_TZ" => "TIMESTAMP_TZ",
        "TIMESTAMP" | "TIMESTAMP_NTZ" | "DATETIME" => "TIMESTAMP_NTZ",
        "VARIANT" => "VARIANT",
        "OBJECT" => "OBJECT",
        "ARRAY" => "ARRAY",
        "VECTOR" => "VECTOR",
        "GEOGRAPHY" => "GEOGRAPHY",
        "GEOMETRY" => "GEOMETRY",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_procedure() {
        let sql = crate::normalize_identifiers(
            "CREATE OR REPLACE PROCEDURE db.s.p(a NUMBER(12,2), b VARCHAR DEFAULT 'x')
             RETURNS TABLE (id INTEGER, price NUMBER(12, 2)) LANGUAGE SQL COMMENT = 'demo' EXECUTE AS CALLER
             AS $$ BEGIN RETURN 1; END $$",
        )
        .unwrap();
        let p = parse_create(&sql).unwrap().unwrap();
        assert_eq!(p.name, ["DB", "S", "P"]);
        assert!(p.or_replace);
        assert_eq!(
            p.arguments,
            vec![
                Argument { name: "A".into(), out: false, data_type: "NUMBER(12,2)".into(), default: None },
                Argument { name: "B".into(), out: false, data_type: "VARCHAR".into(), default: Some("'x'".into()) },
            ]
        );
        assert_eq!(
            p.returns,
            Returns::Table(vec![("ID".into(), "INTEGER".into()), ("PRICE".into(), "NUMBER(12, 2)".into())])
        );
        assert_eq!(
            (p.comment.as_deref(), p.execute_as.as_str(), p.body.as_str()),
            (Some("demo"), "CALLER", "BEGIN RETURN 1; END")
        );
    }

    #[test]
    fn parses_bare_bodies_and_scalar_returns() {
        let sql = crate::normalize_identifiers(
            "create procedure area() returns float not null language sql as declare r float; begin r := 3; return r; end",
        )
        .unwrap();
        let p = parse_create(&sql).unwrap().unwrap();
        assert_eq!(p.returns, Returns::Scalar { data_type: "FLOAT".into(), not_null: true });
        assert!(p.body.starts_with("DECLARE R FLOAT;"));
        assert!(parse_create("CREATE PROCEDURE P() RETURNS INT LANGUAGE JAVASCRIPT AS 'return 1'").is_err());
    }

    #[test]
    fn parses_calls_and_drops() {
        let call = parse_call("CALL S.P(1 + 2, B => 'x') INTO :R").unwrap().unwrap();
        assert_eq!(call.name, ["S", "P"]);
        assert_eq!(call.arguments, vec![(None, "1 + 2".into()), (Some("B".into()), "'x'".into())]);
        assert_eq!(call.into.as_deref(), Some("R"));
        assert_eq!(
            parse_drop("DROP PROCEDURE IF EXISTS P(NUMBER, VARCHAR)").unwrap(),
            Some((Overload { name: vec!["P".into()], types: vec!["NUMBER".into(), "VARCHAR".into()] }, true))
        );
    }
}
