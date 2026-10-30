//! Snowflake Scripting: parsing anonymous blocks and `EXECUTE IMMEDIATE`.
//!
//! ```text
//! DECLARE x NUMBER DEFAULT 1;
//! BEGIN
//!   FOR i IN 1 TO 3 DO x := x * 2; END FOR;
//!   RETURN x;
//! EXCEPTION WHEN OTHER THEN RETURN SQLERRM;
//! END
//! ```
//!
//! sqlparser only understands parts of the language, so blocks are parsed
//! here over its tokens. SQL statements and expressions inside a block stay
//! as their (already normalized) source text: the engine's interpreter (see
//! its `script` module) substitutes variables and runs them through the usual
//! translation.

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::SqlError;
use crate::normalize::LineOffsets;

/// A block: `[DECLARE ...] BEGIN ... [EXCEPTION ...] END`.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub declarations: Vec<Declaration>,
    pub body: Vec<Statement>,
    pub handlers: Vec<Handler>,
}

/// A statement and where it starts (1-based line, 0-based position), for
/// Snowflake's "on line N at position M" errors.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub stmt: Stmt,
    pub line: u64,
    pub position: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
    /// `name [type] [DEFAULT | := expr]`.
    Variable { name: String, data_type: Option<String>, default: Option<String> },
    /// `name EXCEPTION (code, 'message')`.
    Exception { name: String, code: i64, message: String },
}

/// `WHEN name [OR name ...] [EXIT | CONTINUE] THEN ...`; `OTHER` matches
/// every exception. A CONTINUE handler resumes after the failing statement.
#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub names: Vec<String>,
    pub resume: bool,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// A SQL statement, as normalized source text.
    Sql(String),
    Block(Block),
    /// `LET name [type] {DEFAULT | :=} expr`, or `name := expr` (`declare` false).
    Assign {
        name: String,
        data_type: Option<String>,
        value: String,
        declare: bool,
    },
    If {
        branches: Vec<(String, Vec<Statement>)>,
        otherwise: Option<Vec<Statement>>,
    },
    /// `CASE [operand] WHEN ... THEN ... [ELSE ...] END CASE`.
    Case {
        operand: Option<String>,
        branches: Vec<(String, Vec<Statement>)>,
        otherwise: Option<Vec<Statement>>,
    },
    For {
        label: Option<String>,
        variable: String,
        reverse: bool,
        start: String,
        end: String,
        body: Vec<Statement>,
    },
    While {
        label: Option<String>,
        condition: String,
        body: Vec<Statement>,
    },
    Repeat {
        label: Option<String>,
        body: Vec<Statement>,
        until: String,
    },
    Loop {
        label: Option<String>,
        body: Vec<Statement>,
    },
    Break(Option<String>),
    Continue(Option<String>),
    Return(Option<String>),
    /// `RAISE [name]`; without a name, re-raises the exception being handled.
    Raise(Option<String>),
    /// `EXECUTE IMMEDIATE expr [USING (var, ...)]`.
    ExecuteImmediate {
        sql: String,
        using: Vec<String>,
    },
    Null,
}

/// Whether `sql` (a normalized statement) is a block or `EXECUTE IMMEDIATE`.
pub fn is_script(sql: &str) -> bool {
    let words: Vec<String> = Tokenizer::new(&SnowflakeDialect, sql)
        .tokenize()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .take(2)
        .map(|t| word_of(&t).unwrap_or_default())
        .collect();
    match words.first().map(String::as_str) {
        Some("DECLARE") => true,
        Some("BEGIN") => !is_transaction_begin(words.get(1).map(String::as_str)),
        Some("EXECUTE") => words.get(1).is_some_and(|w| w == "IMMEDIATE"),
        _ => false,
    }
}

/// `BEGIN` starts a transaction when followed by nothing, `;`, `TRANSACTION`, `WORK` or `NAME`.
fn is_transaction_begin(next: Option<&str>) -> bool {
    matches!(next, None | Some("" | "TRANSACTION" | "WORK" | "NAME"))
}

fn word_of(token: &Token) -> Option<String> {
    match token {
        Token::Word(w) if w.quote_style.is_none() => Some(w.value.to_uppercase()),
        _ => None,
    }
}

/// Parses a block, or a single `EXECUTE IMMEDIATE`, from normalized SQL.
pub fn parse(sql: &str) -> Result<Stmt, SqlError> {
    let mut parser = Parser::new(sql)?;
    let stmt = match parser.peek_word().as_deref() {
        Some("EXECUTE") => parser.statement()?,
        _ => Stmt::Block(parser.block()?),
    };
    parser.eat(&Token::SemiColon);
    if parser.pos < parser.tokens.len() {
        return Err(parser.error("unexpected text after the end of the block"));
    }
    Ok(stmt)
}

struct Tok {
    token: Token,
    start: usize,
    end: usize,
    line: u64,
    column: u64,
}

struct Parser<'a> {
    sql: &'a str,
    tokens: Vec<Tok>,
    pos: usize,
}

/// Words that end a list of statements.
const TERMINATORS: &[&str] = &["END", "ELSE", "ELSEIF", "WHEN", "EXCEPTION", "UNTIL"];

impl<'a> Parser<'a> {
    fn new(sql: &'a str) -> Result<Self, SqlError> {
        let offsets = LineOffsets::new(sql);
        let tokens = Tokenizer::new(&SnowflakeDialect, sql)
            .with_unescape(false)
            .tokenize_with_location()?
            .into_iter()
            .filter(|t| !matches!(t.token, Token::Whitespace(_)))
            .map(|t| Tok {
                start: offsets.byte_offset(sql, t.span.start.line, t.span.start.column),
                end: offsets.byte_offset(sql, t.span.end.line, t.span.end.column),
                line: t.span.start.line,
                column: t.span.start.column,
                token: t.token,
            })
            .collect();
        Ok(Self { sql, tokens, pos: 0 })
    }

    fn error(&self, message: &str) -> SqlError {
        let near = self.tokens.get(self.pos).map_or("end of input".to_owned(), |t| format!("'{}'", t.token));
        SqlError::Unsupported(format!("Snowflake Scripting: {message} near {near}"))
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|t| &t.token)
    }

    fn peek_word(&self) -> Option<String> {
        self.peek().and_then(word_of)
    }

    fn word_at(&self, pos: usize) -> Option<String> {
        self.tokens.get(pos).and_then(|t| word_of(&t.token))
    }

    fn is_word(&self, word: &str) -> bool {
        self.peek_word().as_deref() == Some(word)
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let found = self.is_word(word);
        if found {
            self.pos += 1;
        }
        found
    }

    fn expect_word(&mut self, word: &str) -> Result<(), SqlError> {
        if self.eat_word(word) { Ok(()) } else { Err(self.error(&format!("expected {word}"))) }
    }

    fn eat(&mut self, token: &Token) -> bool {
        let found = self.peek() == Some(token);
        if found {
            self.pos += 1;
        }
        found
    }

    fn expect(&mut self, token: &Token) -> Result<(), SqlError> {
        if self.eat(token) { Ok(()) } else { Err(self.error(&format!("expected '{token}'"))) }
    }

    /// An identifier (variable, label or exception name), upper-cased unless quoted.
    fn name(&mut self) -> Result<String, SqlError> {
        match self.tokens.get(self.pos).map(|t| &t.token) {
            Some(Token::Word(w)) => {
                let name = if w.quote_style.is_some() { w.value.clone() } else { w.value.to_uppercase() };
                self.pos += 1;
                Ok(name)
            }
            _ => Err(self.error("expected a name")),
        }
    }

    /// Source text of tokens `from..to`.
    fn text(&self, from: usize, to: usize) -> String {
        if from >= to {
            return String::new();
        }
        self.sql[self.tokens[from].start..self.tokens[to - 1].end].trim().to_owned()
    }

    /// Text up to (not including) the first of `stops` outside parentheses
    /// and CASE expressions, or a `;`.
    fn until(&mut self, stops: &[&str]) -> Result<String, SqlError> {
        let start = self.pos;
        let (mut parens, mut cases) = (0i32, 0i32);
        while let Some(token) = self.peek() {
            match token {
                Token::LParen | Token::LBracket | Token::LBrace => parens += 1,
                Token::RParen | Token::RBracket | Token::RBrace => parens -= 1,
                Token::SemiColon if parens == 0 => break,
                _ => {}
            }
            if parens == 0 {
                match self.peek_word().as_deref() {
                    Some("CASE") => cases += 1,
                    Some("END") if cases > 0 => cases -= 1,
                    Some(w) if cases == 0 && stops.contains(&w) => break,
                    _ => {}
                }
            }
            self.pos += 1;
        }
        let text = self.text(start, self.pos);
        if text.is_empty() {
            return Err(self.error("expected an expression"));
        }
        Ok(text)
    }

    /// A condition: `(expr)` or an expression up to `stops`.
    fn condition(&mut self, stops: &[&str]) -> Result<String, SqlError> {
        self.until(stops)
    }

    fn block(&mut self) -> Result<Block, SqlError> {
        let mut declarations = Vec::new();
        if self.eat_word("DECLARE") {
            while !self.is_word("BEGIN") {
                if self.peek().is_none() {
                    return Err(self.error("expected BEGIN"));
                }
                declarations.push(self.declaration()?);
                self.expect(&Token::SemiColon)?;
            }
        }
        self.expect_word("BEGIN")?;
        let body = self.statements()?;
        let mut handlers = Vec::new();
        if self.eat_word("EXCEPTION") {
            while self.eat_word("WHEN") {
                let mut names = vec![self.name()?];
                while self.eat_word("OR") {
                    names.push(self.name()?);
                }
                let resume = self.eat_word("CONTINUE");
                self.eat_word("EXIT");
                self.expect_word("THEN")?;
                handlers.push(Handler { names, resume, body: self.statements()? });
            }
        }
        self.expect_word("END")?;
        Ok(Block { declarations, body, handlers })
    }

    fn declaration(&mut self) -> Result<Declaration, SqlError> {
        let name = self.name()?;
        if self.eat_word("EXCEPTION") {
            if !self.eat(&Token::LParen) {
                return Ok(Declaration::Exception { name, code: -20000, message: String::new() });
            }
            let negative = self.eat(&Token::Minus);
            let code = match self.tokens.get(self.pos).map(|t| &t.token) {
                Some(Token::Number(n, _)) => n.parse::<i64>().map_err(|_| self.error("invalid exception number"))?,
                _ => return Err(self.error("expected an exception number")),
            };
            self.pos += 1;
            self.expect(&Token::Comma)?;
            let message = match self.tokens.get(self.pos).map(|t| &t.token) {
                Some(Token::SingleQuotedString(s)) => s.replace("''", "'"),
                Some(Token::DollarQuotedString(s)) => s.value.clone(),
                _ => return Err(self.error("expected an exception message")),
            };
            self.pos += 1;
            self.expect(&Token::RParen)?;
            return Ok(Declaration::Exception { name, code: if negative { -code } else { code }, message });
        }
        if matches!(self.peek_word().as_deref(), Some("CURSOR" | "RESULTSET")) {
            return Err(self.error("cursors and RESULTSETs aren't supported yet"));
        }
        let (data_type, default) = self.typed_value(true)?;
        Ok(Declaration::Variable { name, data_type, default })
    }

    /// `[type] [{DEFAULT | :=} expr]`.
    fn typed_value(&mut self, optional_value: bool) -> Result<(Option<String>, Option<String>), SqlError> {
        let start = self.pos;
        while !self.is_word("DEFAULT") && !matches!(self.peek(), Some(Token::Assignment | Token::SemiColon) | None) {
            self.pos += 1;
        }
        let data_type = Some(self.text(start, self.pos)).filter(|t| !t.is_empty());
        let value = if self.eat_word("DEFAULT") || self.eat(&Token::Assignment) {
            Some(self.until(&[])?)
        } else if optional_value {
            None
        } else {
            return Err(self.error("expected DEFAULT or :="));
        };
        Ok((data_type, value))
    }

    /// Statements up to a terminating keyword (END, ELSE, WHEN, ...).
    fn statements(&mut self) -> Result<Vec<Statement>, SqlError> {
        let mut statements = Vec::new();
        loop {
            match self.peek_word() {
                Some(w) if TERMINATORS.contains(&w.as_str()) => break,
                _ if self.peek().is_none() => break,
                _ => {}
            }
            if self.eat(&Token::SemiColon) {
                continue;
            }
            let (line, position) = self.tokens.get(self.pos).map_or((0, 0), |t| (t.line, t.column - 1));
            statements.push(Statement { stmt: self.statement()?, line, position });
            if self.peek().is_some() {
                self.expect(&Token::SemiColon)?;
            }
        }
        Ok(statements)
    }

    /// An optional loop label after END FOR / END LOOP / ...
    fn end_label(&mut self) -> Option<String> {
        match self.peek() {
            Some(Token::Word(_)) if !self.peek_word().is_some_and(|w| TERMINATORS.contains(&w.as_str())) => {
                self.name().ok()
            }
            _ => None,
        }
    }

    fn statement(&mut self) -> Result<Stmt, SqlError> {
        let word = self.peek_word();
        // `name := expr`
        if matches!(self.tokens.get(self.pos + 1).map(|t| &t.token), Some(Token::Assignment)) {
            let name = self.name()?;
            self.pos += 1;
            return Ok(Stmt::Assign { name, data_type: None, value: self.until(&[])?, declare: false });
        }
        // `label: FOR ...` style labels aren't Snowflake syntax; labels follow END.
        Ok(match word.as_deref() {
            Some("DECLARE") => Stmt::Block(self.block()?),
            Some("BEGIN")
                if !is_transaction_begin(self.word_at(self.pos + 1).as_deref())
                    && !matches!(self.tokens.get(self.pos + 1).map(|t| &t.token), Some(Token::SemiColon)) =>
            {
                Stmt::Block(self.block()?)
            }
            Some("LET") => {
                self.pos += 1;
                let name = self.name()?;
                if matches!(self.peek_word().as_deref(), Some("CURSOR" | "RESULTSET")) {
                    return Err(self.error("cursors and RESULTSETs aren't supported yet"));
                }
                let (data_type, value) = self.typed_value(false)?;
                Stmt::Assign { name, data_type, value: value.unwrap_or_default(), declare: true }
            }
            Some("IF") => {
                self.pos += 1;
                let mut branches = vec![];
                let condition = self.condition(&["THEN"])?;
                self.expect_word("THEN")?;
                branches.push((condition, self.statements()?));
                let mut otherwise = None;
                loop {
                    if self.eat_word("ELSEIF") {
                        let condition = self.condition(&["THEN"])?;
                        self.expect_word("THEN")?;
                        branches.push((condition, self.statements()?));
                    } else if self.eat_word("ELSE") {
                        otherwise = Some(self.statements()?);
                    } else {
                        break;
                    }
                }
                self.expect_word("END")?;
                self.expect_word("IF")?;
                Stmt::If { branches, otherwise }
            }
            Some("CASE") => {
                self.pos += 1;
                let operand = if self.is_word("WHEN") { None } else { Some(self.until(&["WHEN"])?) };
                let mut branches = vec![];
                while self.eat_word("WHEN") {
                    let value = self.until(&["THEN"])?;
                    self.expect_word("THEN")?;
                    branches.push((value, self.statements()?));
                }
                let otherwise = if self.eat_word("ELSE") { Some(self.statements()?) } else { None };
                self.expect_word("END")?;
                self.eat_word("CASE");
                Stmt::Case { operand, branches, otherwise }
            }
            Some("FOR") => {
                self.pos += 1;
                let variable = self.name()?;
                self.expect_word("IN")?;
                let reverse = self.eat_word("REVERSE");
                let start = self.until(&["TO", "DO", "LOOP"])?;
                if !self.eat_word("TO") {
                    return Err(self.error("cursor FOR loops aren't supported yet"));
                }
                let end = self.until(&["DO", "LOOP"])?;
                let keyword = self.peek_word().unwrap_or_default();
                self.pos += 1;
                let body = self.statements()?;
                self.expect_word("END")?;
                self.expect_word(if keyword == "LOOP" { "LOOP" } else { "FOR" })?;
                Stmt::For { label: self.end_label(), variable, reverse, start, end, body }
            }
            Some("WHILE") => {
                self.pos += 1;
                let condition = self.condition(&["DO", "LOOP"])?;
                let keyword = self.peek_word().unwrap_or_default();
                if !matches!(keyword.as_str(), "DO" | "LOOP") {
                    return Err(self.error("expected DO"));
                }
                self.pos += 1;
                let body = self.statements()?;
                self.expect_word("END")?;
                self.expect_word(if keyword == "LOOP" { "LOOP" } else { "WHILE" })?;
                Stmt::While { label: self.end_label(), condition, body }
            }
            Some("REPEAT") => {
                self.pos += 1;
                let body = self.statements()?;
                self.expect_word("UNTIL")?;
                let until = self.condition(&["END"])?;
                self.expect_word("END")?;
                self.expect_word("REPEAT")?;
                Stmt::Repeat { label: self.end_label(), body, until }
            }
            Some("LOOP") => {
                self.pos += 1;
                let body = self.statements()?;
                self.expect_word("END")?;
                self.expect_word("LOOP")?;
                Stmt::Loop { label: self.end_label(), body }
            }
            Some("BREAK" | "EXIT") => {
                self.pos += 1;
                Stmt::Break(self.end_label())
            }
            Some("CONTINUE" | "ITERATE") => {
                self.pos += 1;
                Stmt::Continue(self.end_label())
            }
            Some("RETURN") => {
                self.pos += 1;
                if matches!(self.peek(), Some(Token::SemiColon) | None) {
                    Stmt::Return(None)
                } else {
                    Stmt::Return(Some(self.until(&[])?))
                }
            }
            Some("RAISE") => {
                self.pos += 1;
                if matches!(self.peek(), Some(Token::SemiColon) | None) {
                    Stmt::Raise(None)
                } else {
                    Stmt::Raise(Some(self.name()?))
                }
            }
            Some("NULL")
                if matches!(self.tokens.get(self.pos + 1).map(|t| &t.token), Some(Token::SemiColon) | None) =>
            {
                self.pos += 1;
                Stmt::Null
            }
            Some("EXECUTE") if self.word_at(self.pos + 1).as_deref() == Some("IMMEDIATE") => {
                self.pos += 2;
                let sql = self.until(&["USING"])?;
                let mut using = vec![];
                if self.eat_word("USING") {
                    self.expect(&Token::LParen)?;
                    loop {
                        using.push(self.name()?);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(&Token::RParen)?;
                }
                Stmt::ExecuteImmediate { sql, using }
            }
            _ => Stmt::Sql(self.until(&[])?),
        })
    }
}

/// Keywords after which `:name` is a variable rather than a VARIANT path.
const BEFORE_EXPRESSION: &[&str] = &[
    "SELECT",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "ON",
    "SET",
    "VALUES",
    "THEN",
    "ELSE",
    "WHEN",
    "IN",
    "BY",
    "RETURN",
    "LIKE",
    "ILIKE",
    "IS",
    "BETWEEN",
    "CASE",
    "INTO",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "TOP",
    "USING",
    "DISTINCT",
    "QUALIFY",
    "ALL",
    "ANY",
    "EXISTS",
    "AS",
    "DEFAULT",
    "IMMEDIATE",
    "UNTIL",
    "DO",
    "TO",
    "LOOP",
];

fn tokens_with_offsets(sql: &str) -> Result<Vec<(Token, usize, usize)>, SqlError> {
    let offsets = LineOffsets::new(sql);
    Ok(Tokenizer::new(&SnowflakeDialect, sql)
        .with_unescape(false)
        .tokenize_with_location()?
        .into_iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .map(|t| {
            let start = offsets.byte_offset(sql, t.span.start.line, t.span.start.column);
            let end = offsets.byte_offset(sql, t.span.end.line, t.span.end.column);
            (t.token, start, end)
        })
        .collect())
}

/// Replaces variable references in `sql` with `lookup`'s SQL literal for
/// them: `:name` (when not a VARIANT path such as `v:name`) and, if `bare`,
/// plain `name` outside subqueries (as in Snowflake Scripting expressions).
/// Names are upper-cased unless quoted; unknown names are left alone.
pub fn substitute(sql: &str, lookup: &dyn Fn(&str) -> Option<String>, bare: bool) -> Result<String, SqlError> {
    let tokens = tokens_with_offsets(sql)?;
    let mut out = String::with_capacity(sql.len());
    let mut copied = 0;
    // For each open parenthesis, whether it starts a subquery.
    let mut parens: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let (token, start, end) = &tokens[i];
        let prev = i.checked_sub(1).map(|p| &tokens[p].0);
        let next = tokens.get(i + 1).map(|t| &t.0);
        match token {
            Token::LParen => {
                let subquery = next.and_then(word_of).is_some_and(|w| w == "SELECT" || w == "WITH");
                parens.push(subquery);
            }
            Token::RParen => {
                parens.pop();
            }
            Token::Colon => {
                let path = match prev {
                    Some(Token::Word(w)) => {
                        w.quote_style.is_some() || !BEFORE_EXPRESSION.contains(&w.value.to_uppercase().as_str())
                    }
                    Some(Token::RParen | Token::RBracket | Token::Number(..) | Token::SingleQuotedString(_)) => true,
                    _ => false,
                };
                if !path
                    && let Some(Token::Word(w)) = next
                    && let Some(value) = lookup(&variable_name(w))
                {
                    out.push_str(&sql[copied..*start]);
                    out.push_str(&wrap(&value));
                    copied = tokens[i + 1].2;
                    i += 2;
                    continue;
                }
            }
            Token::Word(w) if bare && !parens.iter().any(|&q| q) => {
                let qualified = matches!(prev, Some(Token::Period | Token::Colon | Token::DoubleColon));
                let call_or_field = matches!(next, Some(Token::LParen | Token::Period));
                if !qualified
                    && !call_or_field
                    && let Some(value) = lookup(&variable_name(w))
                {
                    out.push_str(&sql[copied..*start]);
                    out.push_str(&wrap(&value));
                    copied = *end;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&sql[copied..]);
    Ok(out)
}

/// A substituted value: string literals as they are (so they keep Snowflake's
/// implicit conversions), anything else parenthesized.
fn wrap(value: &str) -> String {
    let plain_string = value.len() > 1
        && value.starts_with('\'')
        && value.ends_with('\'')
        && !value[1..value.len() - 1].replace("''", "").contains('\'');
    if plain_string { value.to_owned() } else { format!("({value})") }
}

fn variable_name(w: &sqlparser::tokenizer::Word) -> String {
    if w.quote_style.is_some() { w.value.clone() } else { w.value.to_uppercase() }
}

/// `SELECT ... INTO :a, :b FROM ...`: the statement without its INTO clause,
/// and the variables it assigns.
pub fn select_into(sql: &str) -> Result<Option<(String, Vec<String>)>, SqlError> {
    let tokens = tokens_with_offsets(sql)?;
    if tokens.first().and_then(|t| word_of(&t.0)).as_deref() != Some("SELECT") {
        return Ok(None);
    }
    let mut depth = 0;
    for (i, (token, start, _)) in tokens.iter().enumerate() {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ if depth == 0 && word_of(token).as_deref() == Some("INTO") => {
                let mut names = Vec::new();
                let mut j = i + 1;
                while let (Some(Token::Colon), Some(Token::Word(w))) =
                    (tokens.get(j).map(|t| &t.0), tokens.get(j + 1).map(|t| &t.0))
                {
                    names.push(variable_name(w));
                    j += 2;
                    if !matches!(tokens.get(j).map(|t| &t.0), Some(Token::Comma)) {
                        break;
                    }
                    j += 1;
                }
                if names.is_empty() {
                    return Ok(None);
                }
                let resume = tokens.get(j).map_or(sql.len(), |t| t.1);
                let without = format!("{} {}", &sql[..*start], &sql[resume..]);
                return Ok(Some((without.trim().to_owned(), names)));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Replaces each `?` placeholder in order with a value (EXECUTE IMMEDIATE ... USING).
pub fn bind_placeholders(sql: &str, values: &[String]) -> Result<String, SqlError> {
    let tokens = tokens_with_offsets(sql)?;
    let mut out = String::with_capacity(sql.len());
    let mut copied = 0;
    let mut values = values.iter();
    for (token, start, end) in &tokens {
        if matches!(token, Token::Placeholder(p) if p == "?")
            && let Some(value) = values.next()
        {
            out.push_str(&sql[copied..*start]);
            out.push_str(&format!("({value})"));
            copied = *end;
        }
    }
    out.push_str(&sql[copied..]);
    Ok(out)
}

/// Where each statement of a request ends, treating a block (from `DECLARE`
/// or a block `BEGIN` to its matching `END`) as one statement: returns, for
/// each token index, whether a `;` there ends a statement.
pub(crate) struct BlockTracker {
    in_block: bool,
    depth: i32,
    cases: i32,
    at_start: bool,
}

impl BlockTracker {
    pub(crate) fn new() -> Self {
        Self { in_block: false, depth: 0, cases: 0, at_start: true }
    }

    /// Feeds the next non-whitespace token (with the word after it, if any).
    /// Returns whether a `;` here ends the statement.
    pub(crate) fn feed(&mut self, token: &Token, next: Option<&Token>) -> bool {
        let word = word_of(token);
        let next_word = next.and_then(word_of);
        let starting = std::mem::take(&mut self.at_start);
        match word.as_deref() {
            Some("DECLARE") if starting => self.in_block = true,
            Some("BEGIN")
                if (starting || self.in_block)
                    && !is_transaction_begin(next_word.as_deref())
                    && !matches!(next, Some(Token::SemiColon) | None) =>
            {
                self.in_block = true;
                self.depth += 1;
            }
            Some("CASE") if self.in_block => self.cases += 1,
            Some("END") if self.in_block => match next_word.as_deref() {
                Some("IF" | "FOR" | "WHILE" | "LOOP" | "REPEAT") => {}
                Some("CASE") => self.cases -= 1,
                _ if self.cases > 0 => self.cases -= 1,
                _ => {
                    self.depth -= 1;
                    if self.depth <= 0 {
                        self.in_block = false;
                    }
                }
            },
            _ => {}
        }
        if matches!(token, Token::SemiColon) && !self.in_block {
            *self = Self::new();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(sql: &str) -> Block {
        match parse(&crate::normalize_identifiers(sql).unwrap()).unwrap() {
            Stmt::Block(b) => b,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_declarations_and_control_flow() {
        let b = block(
            "DECLARE x NUMBER(10,2) DEFAULT 1.5; e EXCEPTION (-20001, 'bad');
             BEGIN
               LET y := x + 1;
               IF (y > 2) THEN y := 0; ELSEIF (y = 2) THEN NULL; ELSE RAISE e; END IF;
               FOR i IN REVERSE 1 TO 3 DO INSERT INTO t VALUES (:i); END FOR;
               WHILE (y < 10) DO y := y + 1; END WHILE;
               RETURN y;
             EXCEPTION WHEN e OR statement_error THEN RETURN 'err';
             END",
        );
        assert_eq!(
            b.declarations,
            vec![
                Declaration::Variable {
                    name: "X".into(),
                    data_type: Some("NUMBER(10,2)".into()),
                    default: Some("1.5".into())
                },
                Declaration::Exception { name: "E".into(), code: -20001, message: "bad".into() },
            ]
        );
        assert_eq!(b.body.len(), 5);
        assert!(matches!(&b.body[2].stmt, Stmt::For { reverse: true, start, end, .. } if start == "1" && end == "3"));
        assert_eq!(b.body[4].stmt, Stmt::Return(Some("Y".into())));
        assert_eq!((b.body[1].line, b.body[1].position), (4, 15));
        assert_eq!(b.handlers[0].names, vec!["E", "STATEMENT_ERROR"]);
    }

    #[test]
    fn sql_statements_keep_their_text() {
        let b = block("BEGIN SELECT CASE WHEN a THEN 1 ELSE 2 END FROM t; BEGIN TRANSACTION; COMMIT; END");
        assert_eq!(
            b.body.into_iter().map(|s| s.stmt).collect::<Vec<_>>(),
            vec![
                Stmt::Sql("SELECT CASE WHEN A THEN 1 ELSE 2 END FROM T".into()),
                Stmt::Sql("BEGIN TRANSACTION".into()),
                Stmt::Sql("COMMIT".into())
            ]
        );
    }

    #[test]
    fn recognizes_scripts() {
        assert!(is_script("BEGIN RETURN 1; END"));
        assert!(is_script("DECLARE X INT; BEGIN RETURN X; END"));
        assert!(is_script("EXECUTE IMMEDIATE 'SELECT 1'"));
        assert!(!is_script("BEGIN"));
        assert!(!is_script("BEGIN TRANSACTION"));
        assert!(!is_script("BEGIN WORK"));
    }
}

#[cfg(test)]
mod substitution_tests {
    use super::*;

    fn lookup(name: &str) -> Option<String> {
        matches!(name, "X" | "ID").then(|| "42".to_owned())
    }

    #[test]
    fn substitutes_colon_and_bare_variables() {
        assert_eq!(
            substitute("SELECT V:X, :X FROM T WHERE ID = :ID", &lookup, false).unwrap(),
            "SELECT V:X, (42) FROM T WHERE ID = (42)"
        );
        assert_eq!(substitute("X + 1 > ID", &lookup, true).unwrap(), "(42) + 1 > (42)");
        assert_eq!(
            substitute("(SELECT X FROM T WHERE Y = :X) + X", &lookup, true).unwrap(),
            "(SELECT X FROM T WHERE Y = (42)) + (42)"
        );
        assert_eq!(substitute("T.X + X(1) + Y", &lookup, true).unwrap(), "T.X + X(1) + Y");
    }

    #[test]
    fn splits_select_into() {
        assert_eq!(
            select_into("SELECT ID, NAME INTO :ID, :NAME FROM T WHERE ID = 1").unwrap(),
            Some(("SELECT ID, NAME  FROM T WHERE ID = 1".to_owned(), vec!["ID".to_owned(), "NAME".to_owned()]))
        );
        assert_eq!(select_into("INSERT INTO T SELECT 1").unwrap(), None);
    }
}

#[cfg(test)]
mod split_tests {
    #[test]
    fn splits_blocks_as_single_statements() {
        let split = crate::split(
            "select 1; declare x int; begin x := 1; if (x = 1) then select case when x then 1 end; end if; \
             begin return x; end; end; begin; commit; begin transaction; execute immediate $$ begin return 1; end; $$; select 2",
        )
        .unwrap();
        assert_eq!(split.len(), 7, "{split:?}");
        assert!(split[1].starts_with("DECLARE") && split[1].ends_with("END"), "{}", split[1]);
        assert_eq!(
            &split[2..6],
            ["BEGIN", "COMMIT", "BEGIN TRANSACTION", "EXECUTE IMMEDIATE $$ begin return 1; end; $$"]
        );
    }
}
