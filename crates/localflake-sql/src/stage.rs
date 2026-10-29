//! Stages and data loading: `CREATE STAGE`, `CREATE FILE FORMAT`, `PUT`,
//! `GET`, `LIST`, `REMOVE` and `COPY INTO`, parsed by hand (sqlparser only
//! knows part of this syntax) into [`StageCommand`]s for the engine, plus the
//! scanner for stage locations (`@stage/path`) used by the normalizer and by
//! queries over staged files.

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::meta::Show;
use crate::{SqlError, decode_identifier};

/// A stage, as written after `@`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageName {
    /// `@~`: the user's stage.
    User,
    /// `@%table`: a table's stage.
    Table(Vec<String>),
    /// `@name`: a named stage.
    Named(Vec<String>),
}

/// `@stage/path`: a stage and a path prefix within it (no leading slash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub stage: StageName,
    pub path: String,
}

/// An option value: `'text'`, `123`, `WORD`, `('a', 'b')` or `(KEY = value ...)`.
#[derive(Debug, Clone, PartialEq)]
pub enum OptValue {
    Str(String),
    Num(String),
    Word(String),
    List(Vec<OptValue>),
    Nested(Options),
}

impl OptValue {
    /// The value as text: strings and words as written (words upper case).
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Str(s) | Self::Num(s) | Self::Word(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.text()?.to_uppercase().as_str() {
            "TRUE" => Some(true),
            "FALSE" => Some(false),
            _ => None,
        }
    }

    /// A list's items as text (a single value is a list of one).
    pub fn texts(&self) -> Vec<String> {
        match self {
            Self::List(items) => items.iter().filter_map(|i| i.text().map(str::to_owned)).collect(),
            other => other.text().map(|t| vec![t.to_owned()]).unwrap_or_default(),
        }
    }
}

/// Options in the order written, keys upper case.
pub type Options = Vec<(String, OptValue)>;

/// Finds an option by key.
pub fn option<'a>(options: &'a Options, key: &str) -> Option<&'a OptValue> {
    options.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

#[derive(Debug, Clone, PartialEq)]
pub enum StageCommand {
    CreateStage {
        name: Vec<String>,
        or_replace: bool,
        if_not_exists: bool,
        temporary: bool,
        options: Options,
    },
    AlterStage {
        name: Vec<String>,
        if_exists: bool,
        rename: Option<Vec<String>>,
        set: Options,
    },
    DropStage {
        name: Vec<String>,
        if_exists: bool,
    },
    CreateFileFormat {
        name: Vec<String>,
        or_replace: bool,
        if_not_exists: bool,
        temporary: bool,
        options: Options,
    },
    AlterFileFormat {
        name: Vec<String>,
        if_exists: bool,
        rename: Option<Vec<String>>,
        set: Options,
    },
    DropFileFormat {
        name: Vec<String>,
        if_exists: bool,
    },
    ShowStages(Show),
    ShowFileFormats(Show),
    DescribeStage(Vec<String>),
    DescribeFileFormat(Vec<String>),
    List {
        location: Location,
        pattern: Option<String>,
    },
    Remove {
        location: Location,
        pattern: Option<String>,
    },
    /// `PUT 'file://...' @stage`: the driver uploads the files.
    Put {
        local: String,
        location: Location,
        options: Options,
    },
    /// `GET @stage 'file://...'`: the driver downloads the files.
    Get {
        location: Location,
        local: String,
        options: Options,
    },
    CopyIntoTable {
        table: Vec<String>,
        columns: Vec<String>,
        source: CopySource,
        options: Options,
    },
    CopyIntoLocation {
        location: Location,
        /// Snowflake SQL for the rows to unload.
        query: String,
        options: Options,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CopySource {
    Location(Location),
    /// `FROM (SELECT ... FROM @stage)`: Snowflake SQL with stage references.
    Query(String),
}

/// Returns `Some` for the statements handled here.
pub fn parse(sql: &str) -> Result<Option<StageCommand>, SqlError> {
    let mut p = Parser::new(sql)?;
    let Some(first) = p.peek_word() else { return Ok(None) };
    let command = match first.as_str() {
        "CREATE" => {
            p.pos += 1;
            let or_replace = p.eat_words(&["OR", "REPLACE"]);
            let temporary = p.eat_any(&["TEMP", "TEMPORARY", "VOLATILE"]);
            let file_format = if p.eat_word("STAGE") {
                false
            } else if p.eat_words(&["FILE", "FORMAT"]) {
                true
            } else {
                return Ok(None);
            };
            let if_not_exists = p.eat_words(&["IF", "NOT", "EXISTS"]);
            let name = p.required_name()?;
            let options = p.options()?;
            if file_format {
                StageCommand::CreateFileFormat { name, or_replace, if_not_exists, temporary, options }
            } else {
                StageCommand::CreateStage { name, or_replace, if_not_exists, temporary, options }
            }
        }
        "ALTER" | "DROP" => {
            p.pos += 1;
            let file_format = if p.eat_word("STAGE") {
                false
            } else if p.eat_words(&["FILE", "FORMAT"]) {
                true
            } else {
                return Ok(None);
            };
            let if_exists = p.eat_words(&["IF", "EXISTS"]);
            let name = p.required_name()?;
            if first == "DROP" {
                if file_format {
                    StageCommand::DropFileFormat { name, if_exists }
                } else {
                    StageCommand::DropStage { name, if_exists }
                }
            } else {
                let rename = if p.eat_words(&["RENAME", "TO"]) { Some(p.required_name()?) } else { None };
                let set = if p.eat_word("SET") { p.options()? } else { vec![] };
                if rename.is_none() && set.is_empty() && !p.at_end() {
                    return Err(SqlError::Unsupported(format!(
                        "this ALTER {}",
                        if file_format { "FILE FORMAT" } else { "STAGE" }
                    )));
                }
                if file_format {
                    StageCommand::AlterFileFormat { name, if_exists, rename, set }
                } else {
                    StageCommand::AlterStage { name, if_exists, rename, set }
                }
            }
        }
        "DESC" | "DESCRIBE" => {
            p.pos += 1;
            if p.eat_word("STAGE") {
                StageCommand::DescribeStage(p.required_name()?)
            } else if p.eat_words(&["FILE", "FORMAT"]) {
                StageCommand::DescribeFileFormat(p.required_name()?)
            } else {
                return Ok(None);
            }
        }
        "LIST" | "LS" | "REMOVE" | "RM" => {
            p.pos += 1;
            let location = p.required_location()?;
            let options = p.options()?;
            let pattern = option(&options, "PATTERN").and_then(OptValue::text).map(str::to_owned);
            if matches!(first.as_str(), "LIST" | "LS") {
                StageCommand::List { location, pattern }
            } else {
                StageCommand::Remove { location, pattern }
            }
        }
        "PUT" => {
            p.pos += 1;
            let local = p.local_path()?;
            let location = p.required_location()?;
            StageCommand::Put { local, location, options: p.options()? }
        }
        "GET" => {
            p.pos += 1;
            let location = p.required_location()?;
            let local = p.local_path()?;
            StageCommand::Get { location, local, options: p.options()? }
        }
        "COPY" => {
            p.pos += 1;
            if !p.eat_word("INTO") {
                return Ok(None);
            }
            if let Some(location) = p.location()? {
                if !p.eat_word("FROM") {
                    return Err(SqlError::Unsupported("COPY INTO a location without FROM".into()));
                }
                let query = match p.parenthesized()? {
                    Some(query) => query,
                    None => format!("SELECT * FROM {}", render_name(&p.required_name()?)),
                };
                let options = p.options()?;
                StageCommand::CopyIntoLocation { location, query, options }
            } else {
                if p.peek_string().is_some() {
                    return Err(SqlError::Unsupported("COPY INTO an external location".into()));
                }
                let table = p.required_name()?;
                let mut columns = vec![];
                if p.eat(&Token::LParen) {
                    loop {
                        match p.next() {
                            Some(Token::Word(w)) => columns.push(word_ident(&w)),
                            Some(Token::Comma) => {}
                            Some(Token::RParen) => break,
                            _ => return Err(SqlError::Unsupported("COPY INTO column list".into())),
                        }
                    }
                }
                if !p.eat_word("FROM") {
                    return Err(SqlError::Unsupported("COPY INTO without FROM".into()));
                }
                let source = if let Some(location) = p.location()? {
                    CopySource::Location(location)
                } else if let Some(query) = p.parenthesized()? {
                    CopySource::Query(query)
                } else {
                    return Err(SqlError::Unsupported("COPY INTO from an external location".into()));
                };
                StageCommand::CopyIntoTable { table, columns, source, options: p.options()? }
            }
        }
        _ => return Ok(None),
    };
    if !p.at_end() {
        return Err(SqlError::Unsupported(format!("unexpected text after {first}: {}", p.rest())));
    }
    Ok(Some(command))
}

/// A stage reference in a query: `@stage/path [(FILE_FORMAT => 'f', PATTERN => '...')]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    /// Byte range of the reference (including its options) in the SQL.
    pub start: usize,
    pub end: usize,
    pub location: Location,
    pub options: Options,
}

/// Finds the stage references in (normalized) SQL.
pub fn references(sql: &str) -> Result<Vec<Reference>, SqlError> {
    if !sql.contains('@') {
        return Ok(vec![]);
    }
    let mut p = Parser::new(sql)?;
    let mut found = vec![];
    while p.pos < p.tokens.len() {
        let Some((start, _)) = p.spans.get(p.pos).copied() else { break };
        if matches!(p.tokens[p.pos], Token::AtSign)
            && let Some(location) = p.location()?
        {
            let mut options = vec![];
            let mut end = p.prev_end();
            // `(FILE_FORMAT => 'f', PATTERN => '...')`
            if matches!(p.tokens.get(p.pos), Some(Token::LParen))
                && matches!(p.tokens.get(p.pos + 2), Some(Token::RArrow))
            {
                p.pos += 1;
                loop {
                    match p.next() {
                        Some(Token::Word(w)) => {
                            let key = w.value.to_uppercase();
                            if !p.eat(&Token::RArrow) {
                                return Err(SqlError::Unsupported("stage reference options".into()));
                            }
                            options.push((key, p.value()?));
                        }
                        Some(Token::Comma) => {}
                        Some(Token::RParen) => break,
                        _ => return Err(SqlError::Unsupported("stage reference options".into())),
                    }
                }
                end = p.prev_end();
            }
            found.push(Reference { start, end, location, options });
        } else {
            p.pos += 1;
        }
    }
    Ok(found)
}

/// A stage location as scanned from SQL text, before case normalization.
pub(crate) struct RawLocation {
    pub kind: char,
    /// Name parts with whether each was quoted (quoted parts keep their quotes' content).
    pub parts: Vec<(String, bool)>,
    pub path: String,
    pub end: usize,
}

/// Scans a stage location starting at the `@` at byte `at`. Paths are
/// case-sensitive, so they run verbatim to the next space, `)`, `,`, `;` or quote.
pub(crate) fn scan_location(sql: &str, at: usize) -> Option<RawLocation> {
    let bytes = sql.as_bytes();
    let mut i = at + 1;
    let kind = match bytes.get(i) {
        Some(b'~') => {
            i += 1;
            '~'
        }
        Some(b'%') => {
            i += 1;
            '%'
        }
        _ => '@',
    };
    let mut parts = vec![];
    if kind != '~' {
        loop {
            if bytes.get(i) == Some(&b'"') {
                let mut j = i + 1;
                let mut value = String::new();
                loop {
                    let c = sql[j..].chars().next()?;
                    if c == '"' {
                        if bytes.get(j + 1) == Some(&b'"') {
                            value.push('"');
                            j += 2;
                            continue;
                        }
                        j += 1;
                        break;
                    }
                    value.push(c);
                    j += c.len_utf8();
                }
                parts.push((value, true));
                i = j;
            } else {
                let len: usize = sql[i..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .map(char::len_utf8)
                    .sum();
                if len == 0 {
                    return None;
                }
                parts.push((sql[i..i + len].to_owned(), false));
                i += len;
            }
            if bytes.get(i) == Some(&b'.') {
                i += 1;
            } else {
                break;
            }
        }
    }
    let mut path = String::new();
    if bytes.get(i) == Some(&b'/') {
        let len: usize = sql[i..]
            .chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, ')' | ',' | ';' | '\'' | '('))
            .map(char::len_utf8)
            .sum();
        path = sql[i..i + len].trim_start_matches('/').to_owned();
        i += len;
    }
    Some(RawLocation { kind, parts, path, end: i })
}

impl RawLocation {
    /// The location with case rules applied: unquoted name parts upper case,
    /// quoted ones as written.
    fn location(&self) -> Location {
        let name: Vec<String> = self
            .parts
            .iter()
            .map(|(part, quoted)| if *quoted { decode_identifier(part) } else { part.to_uppercase() })
            .collect();
        let stage = match self.kind {
            '~' => StageName::User,
            '%' => StageName::Table(name),
            _ => StageName::Named(name),
        };
        Location { stage, path: self.path.clone() }
    }
}

fn render_name(name: &[String]) -> String {
    name.iter()
        .map(|part| {
            let plain = part.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && !part.starts_with(|c: char| c.is_ascii_digit());
            if plain { part.clone() } else { format!("\"{}\"", crate::encode_identifier(part).replace('"', "\"\"")) }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn word_ident(w: &sqlparser::tokenizer::Word) -> String {
    if w.quote_style.is_some() { decode_identifier(&w.value) } else { w.value.to_uppercase() }
}

struct Parser<'a> {
    sql: &'a str,
    tokens: Vec<Token>,
    /// Byte range of each token.
    spans: Vec<(usize, usize)>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(sql: &'a str) -> Result<Self, SqlError> {
        let offsets = crate::normalize::LineOffsets::new(sql);
        let mut tokens = vec![];
        let mut spans = vec![];
        for twl in Tokenizer::new(&SnowflakeDialect, sql).tokenize_with_location()? {
            if matches!(twl.token, Token::Whitespace(_) | Token::SemiColon) {
                continue;
            }
            let start = offsets.byte_offset(sql, twl.span.start.line, twl.span.start.column);
            let end = offsets.byte_offset(sql, twl.span.end.line, twl.span.end.column);
            tokens.push(twl.token);
            spans.push((start, end));
        }
        Ok(Self { sql, tokens, spans, pos: 0 })
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn rest(&self) -> &str {
        self.spans.get(self.pos).map_or("", |(start, _)| &self.sql[*start..])
    }

    fn prev_end(&self) -> usize {
        self.spans.get(self.pos.wrapping_sub(1)).map_or(self.sql.len(), |(_, end)| *end)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.tokens.get(self.pos) == Some(token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek_word(&self) -> Option<String> {
        match self.tokens.get(self.pos) {
            Some(Token::Word(w)) if w.quote_style.is_none() => Some(w.value.to_uppercase()),
            _ => None,
        }
    }

    fn peek_string(&self) -> Option<String> {
        match self.tokens.get(self.pos) {
            Some(Token::SingleQuotedString(s)) => Some(s.clone()),
            Some(Token::DollarQuotedString(d)) => Some(d.value.clone()),
            _ => None,
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.peek_word().as_deref() == Some(word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_any(&mut self, words: &[&str]) -> bool {
        words.iter().any(|w| self.eat_word(w))
    }

    /// Consumes `words` only if they all follow.
    fn eat_words(&mut self, words: &[&str]) -> bool {
        let start = self.pos;
        if words.iter().all(|w| self.eat_word(w)) {
            return true;
        }
        self.pos = start;
        false
    }

    fn required_name(&mut self) -> Result<Vec<String>, SqlError> {
        let mut parts = vec![];
        loop {
            match self.tokens.get(self.pos) {
                Some(Token::Word(w))
                    if parts.is_empty() || matches!(self.tokens.get(self.pos - 1), Some(Token::Period)) =>
                {
                    parts.push(word_ident(w));
                    self.pos += 1;
                }
                Some(Token::Period) if !parts.is_empty() => self.pos += 1,
                _ => break,
            }
        }
        if parts.is_empty() {
            return Err(SqlError::Unsupported(format!("expected an object name at: {}", self.rest())));
        }
        Ok(parts)
    }

    /// A stage location at the current token, if there is one.
    fn location(&mut self) -> Result<Option<Location>, SqlError> {
        if !matches!(self.tokens.get(self.pos), Some(Token::AtSign)) {
            return Ok(None);
        }
        let at = self.spans[self.pos].0;
        let raw = scan_location(self.sql, at)
            .ok_or_else(|| SqlError::Unsupported(format!("stage location at: {}", &self.sql[at..])))?;
        while self.pos < self.tokens.len() && self.spans[self.pos].0 < raw.end {
            self.pos += 1;
        }
        Ok(Some(raw.location()))
    }

    fn required_location(&mut self) -> Result<Location, SqlError> {
        if let Some(s) = self.peek_string() {
            // `LIST '@stage/path'` is allowed too.
            if s.starts_with('@')
                && let Some(raw) = scan_location(&s, 0)
            {
                self.pos += 1;
                return Ok(raw.location());
            }
        }
        self.location()?.ok_or_else(|| SqlError::Unsupported(format!("expected a stage location at: {}", self.rest())))
    }

    /// `'file:///path'` (the normalizer quotes unquoted file URIs), as a local path.
    fn local_path(&mut self) -> Result<String, SqlError> {
        let Some(Token::SingleQuotedString(uri)) = self.next() else {
            return Err(SqlError::Unsupported("expected a file:// URI".into()));
        };
        let path = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("FILE://"))
            .ok_or_else(|| SqlError::Unsupported(format!("expected a file:// URI, got '{uri}'")))?;
        Ok(path.to_owned())
    }

    /// The text inside `( ... )`, if the current token opens one.
    fn parenthesized(&mut self) -> Result<Option<String>, SqlError> {
        if !matches!(self.tokens.get(self.pos), Some(Token::LParen)) {
            return Ok(None);
        }
        let start = self.spans[self.pos].1;
        let mut depth = 0;
        while let Some(token) = self.next() {
            match token {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.spans[self.pos - 1].0;
                        return Ok(Some(self.sql[start..end].trim().to_owned()));
                    }
                }
                _ => {}
            }
        }
        Err(SqlError::Unsupported("unbalanced parentheses".into()))
    }

    /// `KEY = value` pairs, optionally comma-separated, until the end or `)`.
    fn options(&mut self) -> Result<Options, SqlError> {
        let mut options = vec![];
        loop {
            match self.tokens.get(self.pos) {
                Some(Token::Comma) => self.pos += 1,
                Some(Token::Word(w)) if w.quote_style.is_none() => {
                    let key = w.value.to_uppercase();
                    self.pos += 1;
                    // Flags written without a value (`PURGE`, `FORCE`).
                    if !self.eat(&Token::Eq) {
                        options.push((key, OptValue::Word("TRUE".into())));
                        continue;
                    }
                    options.push((key, self.value()?));
                }
                _ => break,
            }
        }
        Ok(options)
    }

    fn value(&mut self) -> Result<OptValue, SqlError> {
        Ok(match self.next() {
            Some(Token::SingleQuotedString(s)) => OptValue::Str(s),
            Some(Token::DollarQuotedString(d)) => OptValue::Str(d.value),
            Some(Token::Number(n, _)) => OptValue::Num(n),
            Some(Token::Minus) => match self.next() {
                Some(Token::Number(n, _)) => OptValue::Num(format!("-{n}")),
                _ => return Err(SqlError::Unsupported("option value".into())),
            },
            Some(Token::Word(w)) => {
                // `FORMAT_NAME = db.schema.name`
                let mut name = vec![word_ident(&w)];
                while self.eat(&Token::Period) {
                    match self.next() {
                        Some(Token::Word(w)) => name.push(word_ident(&w)),
                        _ => return Err(SqlError::Unsupported("option value".into())),
                    }
                }
                OptValue::Word(name.join("."))
            }
            Some(Token::LParen) => {
                // Either `(KEY = value ...)` or `('a', 'b')`.
                if matches!(self.tokens.get(self.pos), Some(Token::Word(_)))
                    && matches!(self.tokens.get(self.pos + 1), Some(Token::Eq))
                {
                    let nested = self.options()?;
                    if !self.eat(&Token::RParen) {
                        return Err(SqlError::Unsupported(format!("option list at: {}", self.rest())));
                    }
                    OptValue::Nested(nested)
                } else {
                    let mut items = vec![];
                    loop {
                        if self.eat(&Token::RParen) {
                            break;
                        }
                        if self.eat(&Token::Comma) {
                            continue;
                        }
                        items.push(self.value()?);
                    }
                    OptValue::List(items)
                }
            }
            other => return Err(SqlError::Unsupported(format!("option value {other:?}"))),
        })
    }
}

/// Quotes unquoted `file://` URIs (as in `PUT file:///tmp/x.csv @s`), which
/// the tokenizer would otherwise read as `file:` and a `//` comment.
pub(crate) fn quote_file_uris(sql: &str) -> String {
    if !sql.to_ascii_lowercase().contains("file://") {
        return sql.to_owned();
    }
    let mut out = String::with_capacity(sql.len() + 8);
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < sql.len() {
        let c = bytes[i];
        // Skip literals, quoted identifiers and comments.
        let skip_to = match c {
            b'\'' | b'"' => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' && c == b'\'' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == c {
                        if bytes.get(j + 1) == Some(&c) {
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                Some((j + 1).min(bytes.len()))
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => Some(sql[i..].find('\n').map_or(sql.len(), |n| i + n)),
            b'/' if bytes.get(i + 1) == Some(&b'*') => Some(sql[i + 2..].find("*/").map_or(sql.len(), |n| i + n + 4)),
            _ => None,
        };
        if let Some(end) = skip_to {
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        let at_word_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if at_word_start && sql[i..].len() >= 7 && sql[i..i + 7].eq_ignore_ascii_case("file://") {
            let len = sql[i..].find(|c: char| c.is_whitespace() || c == ';').unwrap_or(sql.len() - i);
            out.push('\'');
            out.push_str(&sql[i..i + len].replace('\'', "''"));
            out.push('\'');
            i += len;
            continue;
        }
        let ch = sql[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Normalizes a stage location for [`crate::normalize_identifiers`]: name
/// parts follow identifier rules, the path is kept verbatim. Returns the
/// replacement text and the end of the location.
pub(crate) fn normalize_location(sql: &str, at: usize) -> Option<(String, usize)> {
    let raw = scan_location(sql, at)?;
    let mut out = String::from("@");
    match raw.kind {
        '~' => out.push('~'),
        '%' => out.push('%'),
        _ => {}
    }
    let parts: Vec<String> = raw
        .parts
        .iter()
        .map(|(part, quoted)| {
            if *quoted {
                format!("\"{}\"", crate::encode_identifier(part).replace('"', "\"\""))
            } else {
                part.to_uppercase()
            }
        })
        .collect();
    out.push_str(&parts.join("."));
    if !raw.path.is_empty() || sql[..raw.end].ends_with('/') {
        out.push('/');
        out.push_str(&raw.path);
    }
    Some((out, raw.end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize_identifiers as n;

    fn parse_n(sql: &str) -> StageCommand {
        parse(&n(sql).unwrap()).unwrap().unwrap()
    }

    #[test]
    fn normalizes_locations_and_file_uris() {
        assert_eq!(n("list @my_stage/Dir/File-1.csv").unwrap(), "LIST @MY_STAGE/Dir/File-1.csv");
        assert_eq!(n("put file:///tmp/My.csv @~/In").unwrap(), "PUT 'file:///tmp/My.csv' @~/In");
        assert_eq!(n("select $1 from @%t (pattern => 'a.*') x").unwrap(), "SELECT $1 FROM @%T (PATTERN => 'a.*') X");
    }

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse_n("put file:///tmp/a.csv @s/in/ auto_compress = false"),
            StageCommand::Put {
                local: "/tmp/a.csv".into(),
                location: Location { stage: StageName::Named(vec!["S".into()]), path: "in/".into() },
                options: vec![("AUTO_COMPRESS".into(), OptValue::Word("FALSE".into()))],
            }
        );
        let StageCommand::CopyIntoTable { table, columns, source, options } = parse_n(
            "copy into db.t (a, b) from @\"My Stage\"/x files = ('a.csv', 'b.csv') file_format = (type = csv skip_header = 1 null_if = ('', 'NA')) on_error = continue purge",
        ) else {
            panic!()
        };
        assert_eq!(table, vec!["DB", "T"]);
        assert_eq!(columns, vec!["A", "B"]);
        assert_eq!(
            source,
            CopySource::Location(Location { stage: StageName::Named(vec!["My Stage".into()]), path: "x".into() })
        );
        assert_eq!(option(&options, "FILES").unwrap().texts(), vec!["a.csv", "b.csv"]);
        let Some(OptValue::Nested(format)) = option(&options, "FILE_FORMAT") else { panic!() };
        assert_eq!(option(format, "SKIP_HEADER"), Some(&OptValue::Num("1".into())));
        assert_eq!(option(format, "NULL_IF").unwrap().texts(), vec!["", "NA"]);
        assert_eq!(option(&options, "PURGE").and_then(OptValue::as_bool), Some(true));

        let StageCommand::CopyIntoTable { source: CopySource::Query(q), .. } =
            parse_n("copy into t from (select $1, $2:x from @s t) file_format = (format_name = 'f')")
        else {
            panic!()
        };
        assert_eq!(q, "SELECT $1, $2:x FROM @S T");

        let StageCommand::CopyIntoLocation { location, query, .. } = parse_n("copy into @~/out/ from t header = true")
        else {
            panic!()
        };
        assert_eq!(location, Location { stage: StageName::User, path: "out/".into() });
        assert_eq!(query, "SELECT * FROM T");
    }

    #[test]
    fn finds_references() {
        let sql = n("select $1 from @s/a.csv (file_format => 'csv1', pattern => '.*') t join @~ u on 1=1").unwrap();
        let refs = references(&sql).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(&sql[refs[0].start..refs[0].end], "@S/a.csv (FILE_FORMAT => 'csv1', PATTERN => '.*')");
        assert_eq!(refs[1].location.stage, StageName::User);
    }
}
