use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, CommentDef, CreateTableLikeKind, IdentityPropertyFormatKind,
    IdentityPropertyKind, ObjectName, ObjectType, Set, Statement, TableConstraint, TableObject, Use,
};
use sqlparser::dialect::SnowflakeDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::ast::{quote, quote_literal};
use crate::meta::{self, Meta};
use crate::rewrite::{rewrite_expr, rewrite_statement};
use crate::{Context, SqlError, normalize_identifiers, types};

/// What the engine should do for one Snowflake statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Use(UseTarget),
    CreateDatabase {
        name: String,
        if_not_exists: bool,
        or_replace: bool,
        clone: Option<String>,
    },
    DropDatabase {
        name: String,
        if_exists: bool,
    },
    /// `CREATE SCHEMA` also makes the new schema current.
    CreateSchema {
        sql: String,
        database: Option<String>,
        schema: String,
    },
    AlterSession {
        set: Vec<(String, String)>,
        unset: Vec<String>,
    },
    /// `SET name = expr`, with each value as a DuckDB expression to evaluate.
    SetVariables(Vec<(String, String)>),
    UnsetVariables(Vec<String>),
    /// `DESCRIBE`: `sql` returns no rows if the object doesn't exist.
    Describe {
        sql: String,
        object: String,
    },
    /// Statements accepted for compatibility but without effect (grants, warehouses, ...).
    Ignored {
        kind: StatementKind,
    },
    Sql(SqlCommand),
    /// `INSERT ALL | FIRST`: `setup` materializes the source rows as
    /// `__LF_INSERT_SOURCE`, then each insert runs in order and reports its own
    /// row count. `clear` empties the targets first (`INSERT OVERWRITE`).
    MultiInsert {
        setup: String,
        clear: Vec<String>,
        inserts: Vec<(String, String)>,
    },
    /// Stages, file formats and data loading, run by the engine.
    Stage(crate::stage::StageCommand),
    /// `DROP FUNCTION`: the engine drops the scalar or table macro of that name.
    DropFunction {
        name: String,
        if_exists: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum UseTarget {
    Database(String),
    Schema { database: Option<String>, schema: String },
    Warehouse(String),
    Role(String),
    SecondaryRoles,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SqlCommand {
    /// Statements to run first (e.g. sequences for identity columns).
    pub pre: Vec<String>,
    pub sql: String,
    /// Statements to run after (e.g. `COMMENT ON`).
    pub post: Vec<String>,
    pub kind: StatementKind,
    /// The status message for DDL (`Table T successfully created.`).
    pub status: Option<String>,
    /// Query IDs whose stored results must be available as temp tables.
    pub result_scans: Vec<String>,
    /// For `MERGE`: the count columns to report, from the actions it has. The
    /// SQL returns one `merge_action` row per affected row.
    pub merge_counts: Vec<(&'static str, &'static str)>,
    /// For `INSERT`: the target and source, so the engine can apply
    /// Snowflake's conversions for the target column types.
    pub insert: Option<InsertParts>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InsertParts {
    pub table: String,
    /// The column list as written (empty: all columns).
    pub columns: Vec<String>,
    /// The source query (`VALUES ...` or `SELECT ...`).
    pub source: String,
}

impl InsertParts {
    fn of(insert: &sqlparser::ast::Insert) -> Option<Self> {
        let TableObject::TableName(name) = &insert.table else { return None };
        let source = insert.source.as_ref()?;
        if insert.on.is_some() || insert.returning.is_some() {
            return None;
        }
        Some(Self {
            table: name.to_string(),
            columns: insert.columns.iter().map(ToString::to_string).collect(),
            source: source.to_string(),
        })
    }
}

impl SqlCommand {
    fn new(sql: impl Into<String>, kind: StatementKind) -> Self {
        Self {
            pre: vec![],
            sql: sql.into(),
            post: vec![],
            kind,
            status: None,
            result_scans: vec![],
            merge_counts: vec![],
            insert: None,
        }
    }

    fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }
}

/// Mirrors the statement type categories Snowflake reports to drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Query,
    Show,
    Insert,
    Update,
    Delete,
    Merge,
    Ddl,
    Transaction,
    Other,
}

impl StatementKind {
    /// The `statementTypeId` returned in query responses. Drivers use the
    /// 0x3000 range to decide that a result holds DML row counts.
    pub fn type_id(self) -> u32 {
        match self {
            Self::Query => 0x1000,
            Self::Insert => 0x3100,
            Self::Update => 0x3200,
            Self::Delete => 0x3300,
            Self::Merge => 0x3400,
            Self::Show => 0x4000,
            Self::Transaction => 0x5000,
            Self::Ddl => 0x6000,
            Self::Other => 0x2000,
        }
    }

    pub fn is_dml(self) -> bool {
        matches!(self, Self::Insert | Self::Update | Self::Delete | Self::Merge)
    }
}

/// Splits Snowflake SQL text into statements, normalizing identifier case.
pub fn split(sql: &str) -> Result<Vec<String>, SqlError> {
    let normalized = normalize_identifiers(sql)?;
    let tokens = Tokenizer::new(&SnowflakeDialect, &normalized).tokenize_with_location()?;
    let offsets = crate::normalize::LineOffsets::new(&normalized);
    let mut statements = Vec::new();
    let mut start = 0;
    let mut has_content = false;
    for twl in &tokens {
        match &twl.token {
            Token::SemiColon => {
                let end = offsets.byte_offset(&normalized, twl.span.start.line, twl.span.start.column);
                if has_content {
                    statements.push(normalized[start..end].trim().to_owned());
                }
                start = end + 1;
                has_content = false;
            }
            Token::Whitespace(_) => {}
            _ => has_content = true,
        }
    }
    if has_content {
        statements.push(normalized[start..].trim().to_owned());
    }
    if statements.is_empty() {
        return Err(SqlError::Empty);
    }
    Ok(statements)
}

/// Translates one (already split and normalized) statement.
pub fn translate_statement(sql: &str, ctx: &Context) -> Result<Command, SqlError> {
    if let Some(command) = crate::stage::parse(sql)? {
        return Ok(Command::Stage(command));
    }
    if let Some(meta) = meta::parse(sql)? {
        return Ok(match meta {
            Meta::Show(show) if show.kind == "STAGES" => Command::Stage(crate::stage::StageCommand::ShowStages(show)),
            Meta::Show(show) if show.kind == "FILE FORMATS" => {
                Command::Stage(crate::stage::StageCommand::ShowFileFormats(show))
            }
            Meta::Show(show) => Command::Sql(SqlCommand::new(meta::show_sql(&show, ctx)?, StatementKind::Show)),
            Meta::Describe { kind, name } => {
                Command::Describe { sql: meta::describe_sql(&kind, &name, ctx)?, object: name.join(".") }
            }
            Meta::Unset(names) => Command::UnsetVariables(names),
        });
    }
    if let Some(command) = account_level(sql)? {
        return Ok(command);
    }

    if let Some(udf) = crate::udf::parse(sql)? {
        let status = format!(
            "Function {} successfully created.",
            udf.name.rsplit('.').next().unwrap_or(&udf.name).trim_matches('"')
        );
        let sql = crate::udf::to_macro(&udf, ctx)?;
        return Ok(Command::Sql(SqlCommand::new(sql, StatementKind::Ddl).with_status(status)));
    }
    if let Some(command) = drop_function(sql)? {
        return Ok(command);
    }
    let sql = &strip_sequence_equals(sql)?;
    let sql = &preprocess_tokens(sql)?;
    let mut statements = Parser::parse_sql(&SnowflakeDialect, sql)?;
    let mut statement = match statements.len() {
        0 => return Err(SqlError::Empty),
        1 => statements.remove(0),
        _ => return Err(SqlError::Unsupported("multiple statements in one request".into())),
    };
    let result_scans = rewrite_statement(&mut statement, ctx, sql)?;
    let mut command = translate_parsed(statement, ctx)?;
    if let Command::Sql(sql) = &mut command {
        sql.result_scans = result_scans;
        // Each SEQ call restarts its sequence at 0.
        let restarts = (0..ctx.types.sequences())
            .map(|i| format!("CREATE OR REPLACE TEMP SEQUENCE __lf_seq{i} MINVALUE 0 START 0"));
        sql.pre.splice(0..0, restarts);
    }
    Ok(command)
}

/// `INSERT ALL | FIRST [WHEN ... THEN] INTO ... SELECT ...` as one insert per
/// `INTO` clause over the materialized source rows.
fn multi_table_insert(insert: &sqlparser::ast::Insert) -> Result<Command, SqlError> {
    use sqlparser::ast::{MultiTableInsertIntoClause, MultiTableInsertType, MultiTableInsertValue};
    const SOURCE: &str = "__LF_INSERT_SOURCE";
    let source = insert.source.as_ref().ok_or_else(|| SqlError::Unsupported("INSERT ALL without a query".into()))?;
    let setup = format!("CREATE OR REPLACE TEMP TABLE {SOURCE} AS {source}");
    let first = matches!(insert.multi_table_insert_type, Some(MultiTableInsertType::First));
    let matched = |cond: &sqlparser::ast::Expr| format!("coalesce(({cond}), false)");

    // (INTO clause, row filter) in execution order.
    let mut targets: Vec<(&MultiTableInsertIntoClause, Option<String>)> =
        insert.multi_table_into_clauses.iter().map(|into| (into, None)).collect();
    let mut earlier: Vec<String> = vec![];
    for when in &insert.multi_table_when_clauses {
        let mut filter = matched(&when.condition);
        if first && !earlier.is_empty() {
            filter = format!("{filter} AND NOT ({})", earlier.join(" OR "));
        }
        earlier.push(matched(&when.condition));
        targets.extend(when.into_clauses.iter().map(|into| (into, Some(filter.clone()))));
    }
    if let Some(else_clauses) = &insert.multi_table_else_clause {
        let filter = if earlier.is_empty() { None } else { Some(format!("NOT ({})", earlier.join(" OR "))) };
        targets.extend(else_clauses.iter().map(|into| (into, filter.clone())));
    }

    let mut inserts = vec![];
    let mut clear: Vec<String> = vec![];
    for (into, filter) in targets {
        let delete = format!("DELETE FROM {}", into.table_name);
        if insert.overwrite && !clear.contains(&delete) {
            clear.push(delete);
        }
        let columns = if into.columns.is_empty() {
            String::new()
        } else {
            format!(" ({})", into.columns.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
        };
        let values = match &into.values {
            Some(values) => values
                .values
                .iter()
                .map(|v| match v {
                    MultiTableInsertValue::Expr(e) => Ok(e.to_string()),
                    MultiTableInsertValue::Default => Err(SqlError::Unsupported("DEFAULT in INSERT ALL values".into())),
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(", "),
            None => "*".to_owned(),
        };
        let filter = filter.map(|f| format!(" WHERE {f}")).unwrap_or_default();
        let sql = format!("INSERT INTO {}{columns} SELECT {values} FROM {SOURCE}{filter}", into.table_name);
        inserts.push((last_part(&into.table_name), sql));
    }
    Ok(Command::MultiInsert { setup, clear, inserts })
}

fn translate_parsed(statement: Statement, ctx: &Context) -> Result<Command, SqlError> {
    let kind = match &statement {
        Statement::Use(u) => return Ok(Command::Use(translate_use(u)?)),
        Statement::CreateDatabase { db_name, if_not_exists, or_replace, clone, .. } => {
            return Ok(Command::CreateDatabase {
                name: last_part(db_name),
                if_not_exists: *if_not_exists,
                or_replace: *or_replace,
                clone: clone.as_ref().map(last_part),
            });
        }
        Statement::Drop { object_type: ObjectType::Database, names, if_exists, .. } => {
            return Ok(Command::DropDatabase { name: last_part(&names[0]), if_exists: *if_exists });
        }
        Statement::Drop { object_type: ObjectType::Schema, names, if_exists, .. } => {
            let sql = format!("DROP SCHEMA {}{} CASCADE", if *if_exists { "IF EXISTS " } else { "" }, names[0]);
            let status = format!("{} successfully dropped.", last_part(&names[0]));
            return Ok(Command::Sql(SqlCommand::new(sql, StatementKind::Ddl).with_status(status)));
        }
        Statement::Drop { names, .. } => {
            let status = format!("{} successfully dropped.", last_part(&names[0]));
            return Ok(Command::Sql(SqlCommand::new(statement.to_string(), StatementKind::Ddl).with_status(status)));
        }
        Statement::CreateSchema { schema_name, if_not_exists, or_replace, .. } => {
            let name = schema_name.to_string();
            let parts: Vec<String> = name.split('.').map(|p| p.trim_matches('"').to_owned()).collect();
            let (database, schema) = match parts.as_slice() {
                [db, schema] => (Some(db.clone()), schema.clone()),
                _ => (None, parts.last().cloned().unwrap_or_default()),
            };
            let mut sql = format!("CREATE SCHEMA {}{name}", if *if_not_exists { "IF NOT EXISTS " } else { "" });
            if *or_replace {
                sql = format!("DROP SCHEMA IF EXISTS {name} CASCADE; {sql}");
            }
            return Ok(Command::CreateSchema { sql, database, schema });
        }
        Statement::CreateTable(_) => return render_create_table(statement, ctx).map(Command::Sql),
        Statement::CreateView(_) => return render_create_view(statement).map(Command::Sql),
        Statement::AlterTable(_) => return render_alter_table(statement, ctx),
        Statement::AlterSession { set, session_params } => {
            let options: Vec<(String, String)> = session_params
                .options
                .iter()
                .map(|o| {
                    let value = match &o.option_value {
                        sqlparser::ast::helpers::key_value_options::KeyValueOptionKind::Single(v) => match &v.value {
                            sqlparser::ast::Value::SingleQuotedString(s)
                            | sqlparser::ast::Value::DoubleQuotedString(s) => s.clone(),
                            other => other.to_string(),
                        },
                        other => format!("{other:?}"),
                    };
                    (o.option_name.to_uppercase(), value)
                })
                .collect();
            return Ok(if *set {
                Command::AlterSession { set: options, unset: vec![] }
            } else {
                Command::AlterSession { set: vec![], unset: options.into_iter().map(|(k, _)| k).collect() }
            });
        }
        Statement::Set(set) => return translate_set(set, ctx),
        Statement::Insert(insert) if insert.multi_table_insert_type.is_some() => return multi_table_insert(insert),
        Statement::Merge(merge) => {
            use sqlparser::ast::MergeAction;
            let has = |f: fn(&MergeAction) -> bool| merge.clauses.iter().any(|c| f(&c.action));
            let mut counts = vec![];
            if has(|a| matches!(a, MergeAction::Insert(_))) {
                counts.push(("number of rows inserted", "INSERT"));
            }
            if has(|a| matches!(a, MergeAction::Update { .. })) {
                counts.push(("number of rows updated", "UPDATE"));
            }
            if has(|a| matches!(a, MergeAction::Delete { .. })) {
                counts.push(("number of rows deleted", "DELETE"));
            }
            let mut sql = SqlCommand::new(format!("{statement} RETURNING merge_action"), StatementKind::Merge);
            sql.merge_counts = counts;
            return Ok(Command::Sql(sql));
        }
        Statement::Insert(insert) if insert.overwrite => {
            let mut insert = insert.clone();
            insert.overwrite = false;
            let table = match &insert.table {
                TableObject::TableName(name) => name.to_string(),
                other => return Err(SqlError::Unsupported(format!("INSERT OVERWRITE INTO {other}"))),
            };
            let parts = InsertParts::of(&insert);
            let mut sql = SqlCommand::new(Statement::Insert(insert).to_string(), StatementKind::Insert);
            sql.pre.push(format!("DELETE FROM {table}"));
            sql.insert = parts;
            return Ok(Command::Sql(sql));
        }
        Statement::Insert(insert) => {
            let mut sql = SqlCommand::new(statement.to_string(), StatementKind::Insert);
            sql.insert = InsertParts::of(insert);
            return Ok(Command::Sql(sql));
        }
        Statement::Query(_) | Statement::Explain { .. } | Statement::ExplainTable { .. } => StatementKind::Query,
        Statement::Update(_) => StatementKind::Update,
        Statement::Delete(_) => StatementKind::Delete,
        Statement::CreateSequence { name, .. } => {
            let status = format!("Sequence {} successfully created.", last_part(name));
            return Ok(Command::Sql(SqlCommand::new(statement.to_string(), StatementKind::Ddl).with_status(status)));
        }
        Statement::Truncate(truncate) => {
            let sql =
                truncate.table_names.iter().map(|t| format!("DELETE FROM {}", t.name)).collect::<Vec<_>>().join("; ");
            return Ok(Command::Sql(
                SqlCommand::new(sql, StatementKind::Ddl).with_status("Statement executed successfully."),
            ));
        }
        Statement::Comment { .. } => StatementKind::Ddl,
        Statement::StartTransaction { .. } => {
            return Ok(Command::Sql(SqlCommand::new("BEGIN TRANSACTION", StatementKind::Transaction)));
        }
        Statement::Commit { .. } => return Ok(Command::Sql(SqlCommand::new("COMMIT", StatementKind::Transaction))),
        Statement::Rollback { .. } => return Ok(Command::Sql(SqlCommand::new("ROLLBACK", StatementKind::Transaction))),
        Statement::Grant(_) | Statement::Revoke(_) | Statement::CreateRole(_) => {
            return Ok(Command::Ignored { kind: StatementKind::Other });
        }
        Statement::Call(_) => return Err(SqlError::Unsupported("stored procedures".into())),
        Statement::CopyIntoSnowflake { .. }
        | Statement::Copy { .. }
        | Statement::Put { .. }
        | Statement::CreateStage { .. } => {
            return Err(SqlError::Unsupported("stages and data loading".into()));
        }
        _ => StatementKind::Other,
    };
    Ok(Command::Sql(SqlCommand::new(statement.to_string(), kind)))
}

/// Account-level statements sqlparser may not parse, handled by their leading keywords.
/// Token-level rewrites for syntax sqlparser doesn't accept:
/// - `NTH_VALUE(x, n) FROM LAST|FIRST OVER (...)` → `NTH_VALUE_FROM_LAST(x, n) OVER (...)`;
/// - the `COLLATE(s, spec)` function → `SF_COLLATE(s, spec)` (COLLATE is a keyword).
fn preprocess_tokens(sql: &str) -> Result<String, SqlError> {
    let mut tokens = Tokenizer::new(&SnowflakeDialect, sql).tokenize()?;
    let is_word = |t: &Token, w: &str| matches!(t, Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case(w));
    let next_significant =
        |tokens: &[Token], from: usize| (from..tokens.len()).find(|&j| !matches!(tokens[j], Token::Whitespace(_)));
    let mut changed = false;
    // Number positional `?` binds, so a rewrite that repeats an operand (a
    // cast becomes `CASE WHEN f(x) ... ELSE g(x) END`) still reads one bind.
    let mut binds = 0;
    for token in tokens.iter_mut() {
        if matches!(token, Token::Placeholder(p) if p == "?") {
            binds += 1;
            *token = Token::Placeholder(format!(":{binds}"));
            changed = true;
        }
    }
    let mut i = 0;
    while i < tokens.len() {
        // `t.$1`: a qualified positional column (see `rewrite`'s placeholder handling).
        if let Token::Placeholder(p) = &tokens[i]
            && i > 0
            && matches!(tokens[i - 1], Token::Period)
            && let Some(n) = p.strip_prefix('$').filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        {
            tokens[i] = Token::make_word(&format!("COLUMN{n}"), None);
            changed = true;
        }
        if is_word(&tokens[i], "COLLATE")
            && next_significant(&tokens, i + 1).is_some_and(|j| matches!(tokens[j], Token::LParen))
        {
            tokens[i] = Token::make_word("SF_COLLATE", None);
            changed = true;
        }
        if is_word(&tokens[i], "NTH_VALUE")
            && next_significant(&tokens, i + 1).is_some_and(|j| matches!(tokens[j], Token::LParen))
        {
            // Find the closing parenthesis of the call, then FROM FIRST|LAST.
            let open = next_significant(&tokens, i + 1).unwrap();
            let mut depth = 0;
            let mut close = None;
            for (j, t) in tokens.iter().enumerate().skip(open) {
                match t {
                    Token::LParen => depth += 1,
                    Token::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(close) = close
                && let Some(from) = next_significant(&tokens, close + 1)
                && is_word(&tokens[from], "FROM")
                && let Some(which) = next_significant(&tokens, from + 1)
                && (is_word(&tokens[which], "LAST") || is_word(&tokens[which], "FIRST"))
            {
                if is_word(&tokens[which], "LAST") {
                    tokens[i] = Token::make_word("NTH_VALUE_FROM_LAST", None);
                }
                tokens.drain(close + 1..=which);
                changed = true;
            }
        }
        i += 1;
    }
    Ok(if changed { tokens.iter().map(ToString::to_string).collect() } else { sql.to_owned() })
}

/// `DROP FUNCTION [IF EXISTS] name(arg types)`.
fn drop_function(sql: &str) -> Result<Option<Command>, SqlError> {
    let tokens: Vec<Token> = Tokenizer::new(&SnowflakeDialect, sql)
        .tokenize()?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect();
    let is_word =
        |i: usize, w: &str| matches!(tokens.get(i), Some(Token::Word(word)) if word.value.eq_ignore_ascii_case(w));
    if !is_word(0, "DROP") || !is_word(1, "FUNCTION") {
        return Ok(None);
    }
    let if_exists = is_word(2, "IF") && is_word(3, "EXISTS");
    let start = if if_exists { 4 } else { 2 };
    let name: String = tokens[start..]
        .iter()
        .take_while(|t| !matches!(t, Token::LParen | Token::SemiColon))
        .map(ToString::to_string)
        .collect();
    Ok(Some(Command::DropFunction { name, if_exists }))
}

/// `CREATE SEQUENCE s START = 10 INCREMENT = 5`: Snowflake allows `=` in
/// sequence options, sqlparser doesn't.
fn strip_sequence_equals(sql: &str) -> Result<String, SqlError> {
    let tokens = Tokenizer::new(&SnowflakeDialect, sql).tokenize()?;
    let words: Vec<String> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(w.value.to_uppercase()),
            _ => None,
        })
        .take(6)
        .collect();
    if words.first().map(String::as_str) != Some("CREATE") || !words.iter().any(|w| w == "SEQUENCE") {
        return Ok(sql.to_owned());
    }
    let mut kept: Vec<Token> = vec![];
    let mut previous_word = String::new();
    for token in tokens {
        match &token {
            Token::Eq if matches!(previous_word.as_str(), "START" | "INCREMENT" | "WITH" | "BY") => continue,
            Token::Word(w) => previous_word = w.value.to_uppercase(),
            Token::Whitespace(_) => {}
            _ => previous_word.clear(),
        }
        kept.push(token);
    }
    // sqlparser expects INCREMENT before START, so both move to canonical form.
    let mut clauses = vec![];
    let mut insert_at = None;
    for keyword in ["INCREMENT", "START"] {
        let Some(start) =
            kept.iter().position(|t| matches!(t, Token::Word(w) if w.value.eq_ignore_ascii_case(keyword)))
        else {
            continue;
        };
        let mut end = start + 1;
        let mut value = None;
        while end < kept.len() {
            match &kept[end] {
                Token::Whitespace(_) => {}
                Token::Word(w) if w.value.eq_ignore_ascii_case("WITH") || w.value.eq_ignore_ascii_case("BY") => {}
                Token::Minus => value = Some("-".to_owned()),
                Token::Number(n, _) => {
                    value = Some(format!("{}{n}", value.unwrap_or_default()));
                    end += 1;
                    break;
                }
                _ => break,
            }
            end += 1;
        }
        let Some(value) = value else { continue };
        kept.drain(start..end);
        insert_at = Some(insert_at.map_or(start, |i: usize| i.min(start)));
        clauses.push(if keyword == "INCREMENT" {
            format!("INCREMENT BY {value} ")
        } else {
            format!("START WITH {value} ")
        });
    }
    let mut out: String = match insert_at {
        Some(i) => {
            let head: String = kept[..i].iter().map(ToString::to_string).collect();
            let tail: String = kept[i..].iter().map(ToString::to_string).collect();
            format!("{head}{}{tail}", clauses.concat())
        }
        None => kept.iter().map(ToString::to_string).collect(),
    };
    out.truncate(out.trim_end().len());
    Ok(out)
}

fn account_level(sql: &str) -> Result<Option<Command>, SqlError> {
    let words: Vec<String> = Tokenizer::new(&SnowflakeDialect, sql)
        .tokenize()?
        .into_iter()
        .filter_map(|t| match t {
            Token::Word(w) if w.quote_style.is_none() => Some(w.value.to_uppercase()),
            Token::Whitespace(_) => None,
            _ => Some(String::new()),
        })
        .take(8)
        .collect();
    let first = words.first().map(String::as_str).unwrap_or_default();
    let object = words
        .iter()
        .skip(1)
        .find(|w| !matches!(w.as_str(), "OR" | "REPLACE" | "IF" | "NOT" | "EXISTS" | "SECURE" | "TEMPORARY" | "TEMP"))
        .map(String::as_str)
        .unwrap_or_default();
    Ok(match (first, object) {
        ("GRANT" | "REVOKE", _) => Some(Command::Ignored { kind: StatementKind::Other }),
        ("ALTER", "SESSION") => None,
        (
            "CREATE" | "ALTER" | "DROP",
            "WAREHOUSE" | "ROLE" | "USER" | "RESOURCE" | "INTEGRATION" | "NETWORK" | "STORAGE" | "API" | "NOTIFICATION"
            | "SECURITY" | "MASKING" | "ROW" | "TAG" | "PIPE" | "STREAM" | "TASK" | "SHARE" | "SECRET"
            | "AUTHENTICATION" | "PASSWORD" | "SESSION" | "PACKAGES",
        ) => Some(Command::Ignored { kind: StatementKind::Ddl }),
        ("CALL" | "EXECUTE", _) => return Err(SqlError::Unsupported("stored procedures and tasks".into())),
        ("UNDROP", _) => return Err(SqlError::Unsupported("UNDROP (time travel)".into())),
        // `BEGIN NAME t` / `START TRANSACTION NAME t`: transaction names have no effect.
        ("BEGIN" | "START", _) if words.iter().any(|w| w == "NAME") => {
            Some(Command::Sql(SqlCommand::new("BEGIN TRANSACTION", StatementKind::Transaction)))
        }
        _ => None,
    })
}

fn translate_use(u: &Use) -> Result<UseTarget, SqlError> {
    let parts =
        |name: &ObjectName| name.0.iter().map(|p| p.to_string().trim_matches('"').to_owned()).collect::<Vec<_>>();
    Ok(match u {
        Use::Database(name) | Use::Catalog(name) => UseTarget::Database(parts(name).pop().unwrap_or_default()),
        Use::Schema(name) | Use::Object(name) => {
            let mut parts = parts(name);
            if parts.len() == 1 && matches!(u, Use::Object(_)) {
                UseTarget::Database(parts.remove(0))
            } else {
                let schema = parts.pop().unwrap_or_default();
                UseTarget::Schema { database: parts.pop(), schema }
            }
        }
        Use::Warehouse(name) => UseTarget::Warehouse(parts(name).pop().unwrap_or_default()),
        Use::Role(name) => UseTarget::Role(parts(name).pop().unwrap_or_default()),
        Use::SecondaryRoles(_) => UseTarget::SecondaryRoles,
        Use::Default => return Err(SqlError::Unsupported("USE DEFAULT".into())),
    })
}

fn translate_set(set: &Set, ctx: &Context) -> Result<Command, SqlError> {
    let (names, values): (Vec<&ObjectName>, Vec<sqlparser::ast::Expr>) = match set {
        Set::SingleAssignment { variable, values, .. } => (vec![variable], values.clone()),
        Set::ParenthesizedAssignments { variables, values } => (variables.iter().collect(), values.clone()),
        other => return Err(SqlError::Unsupported(format!("{other}"))),
    };
    if names.len() != values.len() {
        return Err(SqlError::Unsupported("SET with mismatched names and values".into()));
    }
    let mut assignments = Vec::new();
    for (name, mut value) in names.into_iter().zip(values) {
        rewrite_expr(&mut value, ctx)?;
        assignments.push((last_part(name).to_uppercase(), value.to_string()));
    }
    Ok(Command::SetVariables(assignments))
}

fn render_create_table(statement: Statement, ctx: &Context) -> Result<SqlCommand, SqlError> {
    let Statement::CreateTable(ct) = statement else { unreachable!() };
    let table_name = last_part(&ct.name);
    let temporary = ct.temporary || ct.volatile;
    // DuckDB temp tables live in their own catalog and can't be qualified.
    let name = if temporary { quote(&table_name) } else { ct.name.to_string() };
    let prefix = format!(
        "CREATE {}{}TABLE {}{name}",
        if ct.or_replace { "OR REPLACE " } else { "" },
        if temporary { "TEMP " } else { "" },
        if ct.if_not_exists { "IF NOT EXISTS " } else { "" },
    );
    let mut command = SqlCommand::new(String::new(), StatementKind::Ddl)
        .with_status(format!("Table {table_name} successfully created."));

    if let Some(source) = &ct.clone {
        command.sql = format!("{prefix} AS SELECT * FROM {source}");
        return Ok(command);
    }
    if let Some(like) = &ct.like {
        let source = match like {
            CreateTableLikeKind::Parenthesized(l) | CreateTableLikeKind::Plain(l) => l.name.to_string(),
        };
        command.sql = format!("{prefix} AS SELECT * FROM {source} LIMIT 0");
        return Ok(command);
    }

    let pk_columns: Vec<String> = ct
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::PrimaryKey(pk) => {
                Some(pk.columns.iter().map(|c| c.column.expr.to_string()).collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect();
    let mut columns = Vec::new();
    for column in &ct.columns {
        let (definition, pre) = render_column(column, &ct.name, &pk_columns, ct.or_replace, ctx)?;
        columns.push(definition);
        command.pre.extend(pre);
        if let Some(comment) = column_comment(column) {
            let mut column_name = column.name.clone();
            crate::ast::duckdb_ident(&mut column_name);
            command.post.push(format!("COMMENT ON COLUMN {name}.{column_name} IS {}", quote_literal(&comment)));
        }
    }
    if let Some(comment) = table_comment(&ct) {
        command.post.push(format!("COMMENT ON TABLE {name} IS {}", quote_literal(&comment)));
    }
    if !command.pre.is_empty() && ct.or_replace {
        // Replacing the sequence of a table that still exists would fail on the dependency.
        command.pre.insert(0, format!("DROP TABLE IF EXISTS {name}"));
    }

    command.sql = match (&ct.query, columns.is_empty()) {
        (Some(query), true) => format!("{prefix} AS {query}"),
        (Some(query), false) => {
            command.post.insert(0, format!("INSERT INTO {name} {query}"));
            format!("{prefix} ({})", columns.join(", "))
        }
        (None, _) => format!("{prefix} ({})", columns.join(", ")),
    };
    Ok(command)
}

/// Renders a column for DuckDB. Snowflake doesn't enforce PRIMARY KEY, UNIQUE
/// or FOREIGN KEY constraints, so they're dropped (PRIMARY KEY still implies NOT NULL).
fn render_column(
    column: &ColumnDef,
    table: &ObjectName,
    pk_columns: &[String],
    or_replace: bool,
    ctx: &Context,
) -> Result<(String, Vec<String>), SqlError> {
    render_column_with(column, table, pk_columns, or_replace, ctx, true)
}

/// `enforce_length` adds the CHECK for declared lengths; DuckDB can't add a
/// column with a CHECK constraint to an existing table.
fn render_column_with(
    column: &ColumnDef,
    table: &ObjectName,
    pk_columns: &[String],
    or_replace: bool,
    ctx: &Context,
    enforce_length: bool,
) -> Result<(String, Vec<String>), SqlError> {
    let mut column_name = column.name.clone();
    crate::ast::duckdb_ident(&mut column_name);
    let mut parts = vec![column_name.to_string(), types::map_type(&column.data_type, ctx)?.to_string()];
    let mut pre = Vec::new();
    let mut not_null = pk_columns.contains(&column.name.to_string());
    for option in &column.options {
        match &option.option {
            ColumnOption::NotNull | ColumnOption::PrimaryKey(_) => not_null = true,
            ColumnOption::Default(expr) => parts.push(format!("DEFAULT {expr}")),
            ColumnOption::Collation(name) => {
                let spec = name.to_string().trim_matches(['\'', '"']).to_owned();
                let spec = sqlparser::ast::Expr::Value(sqlparser::ast::Value::SingleQuotedString(spec).into());
                // COLLATE goes right after the type.
                parts.insert(2, format!("COLLATE {}", crate::functions::collation_name(&spec)?));
            }
            ColumnOption::Identity(identity) => {
                let property = match identity {
                    IdentityPropertyKind::Autoincrement(p) | IdentityPropertyKind::Identity(p) => p,
                };
                let (start, increment) = match &property.parameters {
                    Some(
                        IdentityPropertyFormatKind::FunctionCall(p) | IdentityPropertyFormatKind::StartAndIncrement(p),
                    ) => (p.seed.to_string(), p.increment.to_string()),
                    None => ("1".to_owned(), "1".to_owned()),
                };
                let sequence = sequence_name(table, &column.name.value, ctx)?;
                let create = if or_replace { "CREATE OR REPLACE SEQUENCE" } else { "CREATE SEQUENCE IF NOT EXISTS" };
                pre.push(format!("{create} {sequence} START {start} INCREMENT {increment}"));
                parts.push(format!("DEFAULT nextval({})", quote_literal(&sequence)));
            }
            _ => {}
        }
    }
    if not_null {
        parts.push("NOT NULL".to_owned());
    }
    // Snowflake rejects strings longer than the declared length.
    if enforce_length
        && let Some(length) = declared_type(&column.data_type).as_deref().and_then(|t| t.strip_prefix("VARCHAR("))
    {
        parts.push(format!("CHECK (length({column_name}) <= {})", length.trim_end_matches(')')));
    }
    Ok((parts.join(" "), pre))
}

/// Separates the declared type from the user's comment in a column comment.
const COMMENT_MARKER: char = '\u{E001}';

/// The DuckDB comment for a column: its declared Snowflake type when DuckDB
/// can't represent it (see [`declared_type`]), then the user's comment.
fn column_comment(column: &ColumnDef) -> Option<String> {
    let comment = column.options.iter().find_map(|o| match &o.option {
        ColumnOption::Comment(c) => Some(c.clone()),
        _ => None,
    });
    match declared_type(&column.data_type) {
        Some(declared) => Some(format!("{COMMENT_MARKER}{declared}{COMMENT_MARKER}{}", comment.unwrap_or_default())),
        None => comment,
    }
}

/// The Snowflake type to report for a column when the DuckDB type loses
/// information: string and binary lengths, and TIMESTAMP_TZ (stored as
/// TIMESTAMP WITH TIME ZONE, which otherwise reports as TIMESTAMP_LTZ).
fn declared_type(data_type: &sqlparser::ast::DataType) -> Option<String> {
    let text = data_type.to_string().to_uppercase();
    let (base, length) = match text.split_once('(') {
        Some((base, rest)) => {
            (base.trim().to_owned(), rest.trim_end_matches(')').split(',').next().map(|n| n.trim().to_owned()))
        }
        None => (text.trim().to_owned(), None),
    };
    match base.as_str() {
        "VARCHAR" | "STRING" | "TEXT" | "NVARCHAR" | "NVARCHAR2" | "CHAR VARYING" | "CHARACTER VARYING"
        | "NCHAR VARYING" => length.filter(|n| n != "16777216").map(|n| format!("VARCHAR({n})")),
        "CHAR" | "CHARACTER" | "NCHAR" => Some(format!("VARCHAR({})", length.unwrap_or_else(|| "1".to_owned()))),
        "BINARY" | "VARBINARY" => length.filter(|n| n != "8388608").map(|n| format!("BINARY({n})")),
        "TIMESTAMP_TZ" | "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => {
            Some(format!("TIMESTAMP_TZ({})", length.unwrap_or_else(|| "9".to_owned())))
        }
        _ => None,
    }
}

fn sequence_name(table: &ObjectName, column: &str, ctx: &Context) -> Result<String, SqlError> {
    let parts: Vec<String> = table.0.iter().map(|p| p.to_string().trim_matches('"').to_owned()).collect();
    let (db, schema, table) = meta::resolve(&parts, ctx)?;
    Ok(format!("{}.{}.{}", quote(&db), quote(&schema), quote(&format!("{table}_{column}_SEQ"))))
}

fn render_create_view(statement: Statement) -> Result<SqlCommand, SqlError> {
    let Statement::CreateView(view) = statement else { unreachable!() };
    if view.materialized {
        return Err(SqlError::Unsupported("materialized views".into()));
    }
    let columns = if view.columns.is_empty() {
        String::new()
    } else {
        format!(" ({})", view.columns.iter().map(|c| c.name.to_string()).collect::<Vec<_>>().join(", "))
    };
    let name = view.name.to_string();
    let sql = format!(
        "CREATE {}{}VIEW {}{name}{columns} AS {}",
        if view.or_replace { "OR REPLACE " } else { "" },
        if view.temporary { "TEMP " } else { "" },
        if view.if_not_exists { "IF NOT EXISTS " } else { "" },
        view.query
    );
    let status = format!("View {} successfully created.", last_part(&view.name));
    let mut command = SqlCommand::new(sql, StatementKind::Ddl).with_status(status);
    if let Some(comment) = &view.comment {
        command.post.push(format!("COMMENT ON VIEW {name} IS {}", quote_literal(comment)));
    }
    Ok(command)
}

fn render_alter_table(statement: Statement, ctx: &Context) -> Result<Command, SqlError> {
    let Statement::AlterTable(alter) = statement else { unreachable!() };
    let name = alter.name.to_string();
    let if_exists = if alter.if_exists { "IF EXISTS " } else { "" };
    let mut statements = Vec::new();
    for operation in &alter.operations {
        let rendered = match operation {
            AlterTableOperation::SwapWith { table_name } => {
                let temp = quote("__lf_swap");
                let schema_prefix: Vec<String> =
                    alter.name.0.iter().take(alter.name.0.len() - 1).map(|p| p.to_string()).collect();
                let qualified_temp =
                    if schema_prefix.is_empty() { temp.clone() } else { format!("{}.{temp}", schema_prefix.join(".")) };
                statements.push(format!("ALTER TABLE {name} RENAME TO {temp}"));
                statements.push(format!("ALTER TABLE {table_name} RENAME TO {}", quote(&last_part(&alter.name))));
                statements.push(format!("ALTER TABLE {qualified_temp} RENAME TO {}", quote(&last_part(table_name))));
                continue;
            }
            AlterTableOperation::AddColumn { column_def, if_not_exists, .. } => {
                let (definition, _) = render_column_with(column_def, &alter.name, &[], false, ctx, false)?;
                statements.push(format!(
                    "ALTER TABLE {name} ADD COLUMN {}{definition}",
                    if *if_not_exists { "IF NOT EXISTS " } else { "" }
                ));
                if let Some(comment) = column_comment(column_def) {
                    statements.push(format!(
                        "COMMENT ON COLUMN {name}.{} IS {}",
                        column_def.name,
                        quote_literal(&comment)
                    ));
                }
                continue;
            }
            AlterTableOperation::AlterColumn {
                column_name,
                op: sqlparser::ast::AlterColumnOperation::SetDataType { data_type, .. },
            } => format!("ALTER COLUMN {column_name} SET DATA TYPE {}", types::map_type(data_type, ctx)?),
            AlterTableOperation::RenameTable { table_name } => {
                let target = match table_name {
                    sqlparser::ast::RenameTableNameKind::As(n) | sqlparser::ast::RenameTableNameKind::To(n) => {
                        last_part(n)
                    }
                };
                format!("RENAME TO {}", quote(&target))
            }
            AlterTableOperation::ClusterBy { .. }
            | AlterTableOperation::DropClusteringKey
            | AlterTableOperation::SuspendRecluster
            | AlterTableOperation::ResumeRecluster
            | AlterTableOperation::SetTblProperties { .. }
            | AlterTableOperation::SetOptionsParens { .. }
            | AlterTableOperation::AddConstraint { .. }
            | AlterTableOperation::DropConstraint { .. } => continue,
            other => other.to_string(),
        };
        statements.push(format!("ALTER TABLE {if_exists}{name} {rendered}"));
    }
    let Some(sql) = statements.pop() else {
        return Ok(Command::Ignored { kind: StatementKind::Ddl });
    };
    let mut command = SqlCommand::new(sql, StatementKind::Ddl).with_status("Statement executed successfully.");
    command.pre = statements;
    Ok(Command::Sql(command))
}

fn table_comment(ct: &sqlparser::ast::CreateTable) -> Option<String> {
    use sqlparser::ast::{CreateTableOptions, SqlOption};
    if let Some(comment) = &ct.comment {
        return Some(comment_text(comment));
    }
    let options = match &ct.table_options {
        CreateTableOptions::With(o)
        | CreateTableOptions::Options(o)
        | CreateTableOptions::Plain(o)
        | CreateTableOptions::TableProperties(o) => o,
        CreateTableOptions::None => return None,
    };
    options.iter().find_map(|o| match o {
        SqlOption::Comment(c) => Some(comment_text(c)),
        SqlOption::KeyValue { key, value } if key.value.eq_ignore_ascii_case("COMMENT") => {
            crate::ast::string_literal(value).map(str::to_owned)
        }
        _ => None,
    })
}

fn comment_text(comment: &CommentDef) -> String {
    match comment {
        CommentDef::WithEq(s) | CommentDef::WithoutEq(s) => s.clone(),
    }
}

fn last_part(name: &ObjectName) -> String {
    name.0.last().map(|p| p.to_string().trim_matches('"').to_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context { database: Some("DB".into()), schema: Some("PUBLIC".into()), user: "ME".into(), ..Default::default() }
    }

    fn command(statement: &str) -> Command {
        translate_statement(&split(statement).unwrap()[0], &ctx()).unwrap()
    }

    fn sql(statement: &str) -> String {
        match command(statement) {
            Command::Sql(c) => c.sql,
            other => panic!("expected SQL, got {other:?}"),
        }
    }

    #[test]
    fn splits_statements() {
        assert_eq!(split("select 1; select ';'; ").unwrap(), vec!["SELECT 1", "SELECT ';'"]);
        assert!(matches!(split(" ; -- nothing"), Err(SqlError::Empty)));
    }

    #[test]
    fn names_expression_columns_like_snowflake() {
        assert_eq!(sql("select a+1, b, count(*) from t"), r#"SELECT A + 1 AS "A+1", B, COUNT(*) AS "COUNT(*)" FROM T"#);
    }

    #[test]
    fn names_values_columns() {
        assert_eq!(
            sql("select column2 from values (1, 'a')"),
            r#"SELECT COLUMN2 FROM (VALUES (1, 'a')) __VALUES (COLUMN1, COLUMN2)"#
        );
        assert_eq!(sql("select x from values (1) t(x)"), "SELECT X FROM (VALUES (1)) T (X)");
    }

    #[test]
    fn rewrites_functions() {
        assert_eq!(sql("select iff(a > 1, 'x', 'y') as r"), "SELECT CASE WHEN A > 1 THEN 'x' ELSE 'y' END AS R");
        assert_eq!(sql("select nvl(a, 0) as r"), "SELECT coalesce(A, 0) AS R");
        assert_eq!(sql("select a / b as r"), "SELECT sf_div(A, B) AS R");
        assert_eq!(
            sql("select dateadd(day, 1, d) as r"),
            "SELECT sf_cast_like(D + to_days(CAST(1 AS BIGINT)), D) AS R"
        );
        assert_eq!(sql("select datediff('month', a, b) as r"), "SELECT date_diff('month', A, B) AS R");
    }

    #[test]
    fn rewrites_variant_paths() {
        assert_eq!(
            sql("select v:a.B[0] as r from t"),
            "SELECT variant_extract(CAST(variant_extract(CAST(variant_extract(CAST(CAST(V AS VARIANT) AS VARIANT), 'a') AS VARIANT), 'B') AS VARIANT), CAST(1 AS UINTEGER)) AS R FROM T"
        );
        assert_eq!(
            sql("select v:a::string as r from t"),
            "SELECT sf_to_varchar(variant_extract(CAST(CAST(V AS VARIANT) AS VARIANT), 'a')) AS R FROM T"
        );
    }

    #[test]
    fn renders_create_table() {
        let Command::Sql(c) = command(
            "CREATE OR REPLACE TRANSIENT TABLE T (ID INT AUTOINCREMENT, A NUMBER(10,2) NOT NULL COMMENT 'x', V VARIANT, PRIMARY KEY (ID)) CLUSTER BY (A) COMMENT = 'hi'",
        ) else {
            panic!()
        };
        assert_eq!(
            c.sql,
            r#"CREATE OR REPLACE TABLE T (ID BIGINT DEFAULT nextval('"DB"."PUBLIC"."T_ID_SEQ"') NOT NULL, A DECIMAL(10,2) NOT NULL, V VARIANT)"#
        );
        assert_eq!(
            c.pre,
            vec![
                "DROP TABLE IF EXISTS T".to_owned(),
                r#"CREATE OR REPLACE SEQUENCE "DB"."PUBLIC"."T_ID_SEQ" START 1 INCREMENT 1"#.to_owned()
            ]
        );
        assert_eq!(c.post, vec!["COMMENT ON COLUMN T.A IS 'x'".to_owned(), "COMMENT ON TABLE T IS 'hi'".to_owned()]);
    }

    #[test]
    fn classifies_commands() {
        assert_eq!(
            command("use schema db1.s"),
            Command::Use(UseTarget::Schema { database: Some("DB1".into()), schema: "S".into() })
        );
        assert_eq!(command("use warehouse wh"), Command::Use(UseTarget::Warehouse("WH".into())));
        assert!(matches!(command("create warehouse wh"), Command::Ignored { .. }));
        assert!(matches!(command("grant select on table t to role r"), Command::Ignored { .. }));
        assert_eq!(
            command("create database if not exists analytics"),
            Command::CreateDatabase { name: "ANALYTICS".into(), if_not_exists: true, or_replace: false, clone: None }
        );
        assert_eq!(
            command("alter session set timezone = 'UTC'"),
            Command::AlterSession { set: vec![("TIMEZONE".into(), "UTC".into())], unset: vec![] }
        );
        assert!(
            matches!(command("set x = 1"), Command::SetVariables(v) if v == vec![("X".to_owned(), "1".to_owned())])
        );
    }
}
