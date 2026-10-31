//! The Snowflake Scripting interpreter: runs blocks and `EXECUTE IMMEDIATE`
//! (parsed by `localflake_sql::scripting`) in a session.
//!
//! Variables hold typed values as Snowflake SQL literals (`(12.50)::NUMBER(10,2)`,
//! `'2024-01-02'::DATE`, ...). Expressions are evaluated as `SELECT <expr>`
//! with variables substituted, and SQL statements run through the usual
//! translation, so they behave exactly as they do outside a block.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta};
use localflake_sql::StatementKind;
use localflake_sql::procedure::{self, Call, Returns};
use localflake_sql::scripting::{Block, CursorSource, Declaration, Handler, Statement, Stmt};

use crate::procedures::{Stored, column_alias};
use crate::{Column, EngineError, QueryResult, Session, SfType, new_query_id};

/// How deeply procedures may call procedures.
const MAX_CALL_DEPTH: usize = 50;

/// A variable's value: a SQL literal and its Snowflake type.
#[derive(Debug, Clone)]
struct Value {
    literal: String,
    data_type: String,
}

impl Value {
    fn null(data_type: &str) -> Self {
        Self { literal: format!("NULL::{data_type}"), data_type: data_type.to_owned() }
    }

    fn text(text: &str) -> Self {
        Self { literal: text_literal(text), data_type: "VARCHAR".into() }
    }

    fn integer(n: i64) -> Self {
        Self { literal: format!("({n})::NUMBER(38,0)"), data_type: "NUMBER(38,0)".into() }
    }

    fn boolean(b: Option<bool>) -> Self {
        let literal = b.map_or("NULL::BOOLEAN".to_owned(), |b| if b { "TRUE" } else { "FALSE" }.to_owned());
        Self { literal, data_type: "BOOLEAN".into() }
    }
}

struct Variable {
    /// The declared type, or the type of the first value assigned.
    data_type: Option<String>,
    value: Value,
}

#[derive(Default)]
struct Scope {
    variables: HashMap<String, Variable>,
    exceptions: HashMap<String, (i64, String)>,
    /// RESULTSETs, with their result once a query is assigned.
    resultsets: HashMap<String, Option<Arc<QueryResult>>>,
    cursors: HashMap<String, Cursor>,
}

struct Cursor {
    source: CursorSource,
    /// The rows, once opened, and the next one FETCH reads.
    open: Option<(Arc<QueryResult>, usize)>,
}

/// An exception in flight: its type name (`STATEMENT_ERROR`, `EXPRESSION_ERROR`
/// or a declared exception), SQLCODE, SQLERRM and SQLSTATE, and where it happened.
#[derive(Debug, Clone)]
struct Raised {
    name: String,
    code: i64,
    message: String,
    sql_state: String,
    line: u64,
    position: u64,
    /// For STATEMENT_ERROR and EXPRESSION_ERROR, the underlying error.
    cause: Option<EngineError>,
}

enum Failure {
    Raised(Box<Raised>),
    /// Cancellation and timeouts, which handlers can't catch.
    Fatal(EngineError),
}

impl From<EngineError> for Failure {
    fn from(err: EngineError) -> Self {
        Failure::Fatal(err)
    }
}

enum Flow {
    Next,
    Break(Option<String>),
    Continue(Option<String>),
    Return(Value),
    /// `RETURN TABLE(...)`.
    ReturnTable(Arc<QueryResult>),
}

type Run<T> = Result<T, Failure>;

/// The result of a script: a value (a block's RETURN), or a statement's result
/// (`EXECUTE IMMEDIATE` of SQL).
enum Outcome {
    Value(Option<Value>),
    Result(QueryResult),
}

struct Interpreter<'s> {
    session: &'s mut Session,
    scopes: Vec<Scope>,
    /// The handlers of the enclosing blocks, innermost last.
    handlers: Vec<Vec<Handler>>,
    /// Where the statement being run starts.
    line: u64,
    position: u64,
    /// How many procedure calls deep this is.
    depth: usize,
}

impl Session {
    /// Runs a Snowflake Scripting statement. A block returns one row in a
    /// column named `anonymous block`; `EXECUTE IMMEDIATE` returns its statement's result.
    pub(crate) fn run_script(&mut self, stmt: Stmt, query_id: &str) -> Result<QueryResult, EngineError> {
        let mut interpreter = Interpreter::new(self);
        let outcome = interpreter.top_level(&stmt);
        let value = match outcome {
            Ok(Outcome::Result(mut result)) => {
                result.query_id = query_id.to_owned();
                return Ok(result);
            }
            Ok(Outcome::Value(value)) => value.unwrap_or_else(|| Value::null("VARCHAR")),
            Err(Failure::Fatal(err)) => return Err(err),
            Err(Failure::Raised(raised)) => return Err(uncaught(&raised)),
        };
        let sql = format!("SELECT {} AS \"anonymous block\"", value.literal);
        self.run_translated(&sql, query_id, &[], false)
    }
}

impl Session {
    /// Runs `CALL`. A scalar result is one row in a column named after the procedure.
    pub(crate) fn run_call(&mut self, call: &Call, query_id: &str) -> Result<QueryResult, EngineError> {
        let mut interpreter = Interpreter::new(self);
        let outcome = interpreter.call(call);
        match outcome {
            Ok((Outcome::Result(mut result), _)) => {
                result.query_id = query_id.to_owned();
                Ok(result)
            }
            Ok((Outcome::Value(value), name)) => {
                let value = value.unwrap_or_else(|| Value::null("VARCHAR"));
                let sql = format!("SELECT {} AS {}", value.literal, column_alias(&name));
                self.run_translated(&sql, query_id, &[], false)
            }
            Err(Failure::Fatal(err)) => Err(err),
            Err(Failure::Raised(raised)) => Err(raised.cause.clone().unwrap_or_else(|| uncaught(&raised))),
        }
    }
}

/// Snowflake's error for an exception nothing handled.
fn uncaught(raised: &Raised) -> EngineError {
    let mut message =
        format!("Uncaught exception of type '{}' on line {} at position {}", raised.name, raised.line, raised.position);
    if !raised.message.is_empty() {
        message.push_str(" : ");
        message.push_str(&raised.message);
    }
    match &raised.cause {
        Some(cause) => EngineError { message, ..cause.clone() },
        None => EngineError::scripting(error_code(raised.code), "P0001", message),
    }
}

/// A Snowflake error code (`-20002`, `002003`) as a static string.
fn error_code(code: i64) -> &'static str {
    static CODES: OnceLock<Mutex<HashMap<i64, &'static str>>> = OnceLock::new();
    let mut codes = CODES.get_or_init(Default::default).lock().unwrap();
    codes.entry(code).or_insert_with(|| Box::leak(code.to_string().into_boxed_str()))
}

impl<'s> Interpreter<'s> {
    fn new(session: &'s mut Session) -> Self {
        let mut interpreter =
            Interpreter { session, scopes: vec![Scope::default()], handlers: vec![], line: 1, position: 0, depth: 0 };
        interpreter.reset_status();
        interpreter
    }

    fn top_level(&mut self, stmt: &Stmt) -> Run<Outcome> {
        match stmt {
            Stmt::Block(block) => match self.block(block)? {
                Flow::Return(value) => Ok(Outcome::Value(Some(value))),
                Flow::ReturnTable(result) => Ok(Outcome::Result(Arc::unwrap_or_clone(result))),
                _ => Ok(Outcome::Value(None)),
            },
            Stmt::ExecuteImmediate { sql, using } => self.execute_immediate(sql, using),
            other => {
                let statement = Statement { stmt: other.clone(), line: 1, position: 0 };
                match self.statement(&statement)? {
                    Flow::Return(value) => Ok(Outcome::Value(Some(value))),
                    Flow::ReturnTable(result) => Ok(Outcome::Result(Arc::unwrap_or_clone(result))),
                    _ => Ok(Outcome::Value(None)),
                }
            }
        }
    }

    fn check_cancelled(&self) -> Run<()> {
        if let Some(cancel) = &self.session.cancel
            && let Some(err) = cancel.error(self.session.statement_timeout())
        {
            return Err(Failure::Fatal(err));
        }
        Ok(())
    }

    // Variables.

    fn lookup(&self, name: &str) -> Option<&Variable> {
        self.scopes.iter().rev().find_map(|s| s.variables.get(name))
    }

    fn literal_of(&self, name: &str) -> Option<String> {
        self.lookup(name).map(|v| v.value.literal.clone())
    }

    fn declare(&mut self, name: &str, data_type: Option<String>, value: Value) {
        let scope = self.scopes.last_mut().unwrap();
        scope.variables.insert(name.to_owned(), Variable { data_type, value });
    }

    fn assign(&mut self, name: &str, value: Value) -> Run<()> {
        let variable = self.scopes.iter_mut().rev().find_map(|s| s.variables.get_mut(name));
        match variable {
            Some(variable) => {
                variable.value = value;
                Ok(())
            }
            None => Err(self.expression_error(EngineError::undeclared_variable(name))),
        }
    }

    fn set_builtin(&mut self, name: &str, value: Value) {
        let data_type = value.data_type.clone();
        self.scopes[0].variables.insert(name.to_owned(), Variable { data_type: Some(data_type), value });
    }

    /// SQLCODE, SQLERRM, SQLSTATE and the DML status variables before anything runs.
    fn reset_status(&mut self) {
        self.set_builtin("SQLCODE", Value::integer(0));
        self.set_builtin("SQLERRM", Value::null("VARCHAR"));
        self.set_builtin("SQLSTATE", Value::text("00000"));
        self.set_dml_status(None, None);
    }

    /// After a statement: DML sets the row count; anything else leaves SQLROWCOUNT,
    /// SQLFOUND and SQLNOTFOUND NULL. ACTIVITY_COUNT also counts rows a query returns.
    fn set_dml_status(&mut self, dml_rows: Option<i64>, activity: Option<i64>) {
        let rows = dml_rows.map_or(Value::null("NUMBER(38,0)"), Value::integer);
        self.set_builtin("SQLROWCOUNT", rows);
        self.set_builtin("SQLFOUND", Value::boolean(dml_rows.map(|n| n > 0)));
        self.set_builtin("SQLNOTFOUND", Value::boolean(dml_rows.map(|n| n == 0)));
        self.set_builtin("ACTIVITY_COUNT", activity.map_or(Value::null("NUMBER(38,0)"), Value::integer));
    }

    // Errors.

    fn raised(&self, name: &str, err: EngineError) -> Failure {
        if err.code == "000604" || err.code == "000630" {
            return Failure::Fatal(err);
        }
        Failure::Raised(Box::new(Raised {
            name: name.to_owned(),
            code: err.code.parse().unwrap_or(-1),
            message: err.message.clone(),
            sql_state: err.sql_state.to_owned(),
            line: self.line,
            position: self.position,
            cause: Some(err),
        }))
    }

    fn statement_error(&self, err: EngineError) -> Failure {
        self.raised("STATEMENT_ERROR", err)
    }

    fn expression_error(&self, err: EngineError) -> Failure {
        self.raised("EXPRESSION_ERROR", err)
    }

    fn user_exception(&self, name: &str) -> Run<Failure> {
        let found = self.scopes.iter().rev().find_map(|s| s.exceptions.get(name));
        let Some((code, message)) = found else {
            return Err(self.expression_error(EngineError::undeclared_exception(name)));
        };
        Ok(Failure::Raised(Box::new(Raised {
            name: name.to_owned(),
            code: *code,
            message: message.clone(),
            sql_state: "P0001".into(),
            line: self.line,
            position: self.position,
            cause: None,
        })))
    }

    // Evaluation.

    /// Runs one SQL statement in the session, as its own query (for
    /// LAST_QUERY_ID and RESULT_SCAN).
    fn run_sql(&mut self, sql: &str) -> Result<QueryResult, EngineError> {
        let query_id = new_query_id();
        let result = self.session.run_statement(sql, &query_id, &[], false).map(Arc::new);
        self.session.engine.store(&query_id, result.clone());
        self.session.ctx.query_history.push(query_id);
        match result {
            Ok(result) => {
                if self.session.in_transaction && result.kind.is_dml() {
                    self.session.transaction_log.push((sql.to_owned(), vec![]));
                }
                Ok(Arc::unwrap_or_clone(result))
            }
            Err(err) => {
                self.session.recover_transaction();
                Err(err)
            }
        }
    }

    /// Runs an expression's query, which (unlike a statement) isn't a query of its own.
    fn run_expression(&mut self, sql: &str) -> Result<QueryResult, EngineError> {
        let result = self.session.run_statement(sql, &new_query_id(), &[], false);
        if result.is_err() {
            self.session.recover_transaction();
        }
        result
    }

    fn substitute(&self, sql: &str, bare: bool) -> Result<String, EngineError> {
        Ok(localflake_sql::scripting::substitute(sql, &|name| self.literal_of(name), bare)?)
    }

    /// Evaluates an expression, cast to `data_type` if given.
    fn evaluate(&mut self, expr: &str, data_type: Option<&str>) -> Run<Value> {
        self.check_cancelled()?;
        let substituted = self.substitute(expr, true).map_err(|e| self.expression_error(e))?;
        let value = match data_type {
            Some(t) => format!("CAST(({substituted}) AS {t})"),
            None => format!("({substituted})"),
        };
        let result = self.run_expression(&format!("SELECT {value}")).map_err(|e| self.expression_error(e))?;
        let rows = result.json_rows().map_err(|e| self.expression_error(e))?;
        let column = &result.columns[0];
        let cell = rows.first().and_then(|r| r[0].as_deref());
        Ok(to_value(column, cell))
    }

    fn condition(&mut self, expr: &str) -> Run<bool> {
        let value = self.evaluate(expr, Some("BOOLEAN"))?;
        Ok(value.literal == "TRUE")
    }

    fn integer(&mut self, expr: &str) -> Run<i64> {
        let value = self.evaluate(expr, Some("NUMBER(38,0)"))?;
        let digits = value.literal.trim_start_matches('(').split(')').next().unwrap_or_default().to_owned();
        digits.parse().map_err(|_| self.expression_error(EngineError::invalid_loop_bound(expr)))
    }

    // Blocks and statements.

    fn block(&mut self, block: &Block) -> Run<Flow> {
        self.scopes.push(Scope::default());
        let flow = self.block_body(block);
        self.scopes.pop();
        flow
    }

    fn block_body(&mut self, block: &Block) -> Run<Flow> {
        for declaration in &block.declarations {
            self.declare_other(declaration)?;
        }
        self.handlers.push(block.handlers.clone());
        let result = self.statements(&block.body);
        self.handlers.pop();
        match result {
            Err(Failure::Raised(raised)) => match find_handler(&block.handlers, &raised) {
                Some(handler) => {
                    let handler = handler.clone();
                    let flow = self.handle(&handler, &raised)?;
                    Ok(match flow {
                        Flow::Return(value) => Flow::Return(value),
                        Flow::ReturnTable(result) => Flow::ReturnTable(result),
                        _ => Flow::Next,
                    })
                }
                None => Err(Failure::Raised(raised)),
            },
            other => other,
        }
    }

    /// Runs a handler with SQLCODE, SQLERRM and SQLSTATE describing the exception.
    fn handle(&mut self, handler: &Handler, raised: &Raised) -> Run<Flow> {
        let saved: Vec<Value> =
            ["SQLCODE", "SQLERRM", "SQLSTATE"].iter().map(|n| self.lookup(n).unwrap().value.clone()).collect();
        self.set_builtin("SQLCODE", Value::integer(raised.code));
        self.set_builtin("SQLERRM", Value::text(&raised.message));
        self.set_builtin("SQLSTATE", Value::text(&raised.sql_state));
        // `RAISE;` in the handler re-raises this exception.
        self.scopes.push(Scope::default());
        self.scopes.last_mut().unwrap().exceptions.insert("\u{0}current".into(), (raised.code, raised.name.clone()));
        let reraise = Raised { ..raised.clone() };
        let flow = match self.statements(&handler.body) {
            Err(Failure::Raised(r)) if r.name == "\u{0}reraise" => Err(Failure::Raised(Box::new(reraise))),
            other => other,
        };
        self.scopes.pop();
        for (name, value) in ["SQLCODE", "SQLERRM", "SQLSTATE"].iter().zip(saved) {
            self.set_builtin(name, value);
        }
        flow
    }

    fn statements(&mut self, statements: &[Statement]) -> Run<Flow> {
        for statement in statements {
            match self.statement(statement) {
                Ok(Flow::Next) => {}
                Ok(flow) => return Ok(flow),
                Err(Failure::Raised(raised)) => {
                    // The innermost handler for this exception decides: CONTINUE resumes here.
                    let handler = self.handlers.iter().rev().find_map(|h| find_handler(h, &raised)).cloned();
                    match handler {
                        Some(handler) if handler.resume => match self.handle(&handler, &raised)? {
                            Flow::Next => {}
                            flow => return Ok(flow),
                        },
                        _ => return Err(Failure::Raised(raised)),
                    }
                }
                Err(fatal) => return Err(fatal),
            }
        }
        Ok(Flow::Next)
    }

    fn statement(&mut self, statement: &Statement) -> Run<Flow> {
        self.check_cancelled()?;
        (self.line, self.position) = (statement.line, statement.position);
        match &statement.stmt {
            Stmt::Sql(sql) => {
                self.sql_statement(sql)?;
            }
            Stmt::Block(block) => return self.block(block),
            Stmt::Assign { name, value, .. } if self.find_resultset(name).is_some() => {
                let result = self.run_query_expression(value)?;
                *self.find_resultset(name).unwrap() = Some(result);
            }
            Stmt::Assign { name, data_type, value, declare } => {
                if *declare {
                    let value = self.evaluate(value, data_type.as_deref())?;
                    let data_type = data_type.clone().or_else(|| Some(inferred_type(&value)));
                    self.declare(name, data_type, value);
                } else {
                    let declared = self.lookup(name).and_then(|v| v.data_type.clone());
                    if self.lookup(name).is_none() {
                        return Err(self.expression_error(EngineError::undeclared_variable(name)));
                    }
                    let value = self.evaluate(value, declared.as_deref())?;
                    self.assign(name, value)?;
                }
            }
            Stmt::If { branches, otherwise } => {
                for (condition, body) in branches {
                    if self.condition(condition)? {
                        return self.statements(body);
                    }
                }
                if let Some(body) = otherwise {
                    return self.statements(body);
                }
            }
            Stmt::Case { operand, branches, otherwise } => {
                for (value, body) in branches {
                    let test = match operand {
                        Some(operand) => format!("({operand}) = ({value})"),
                        None => value.clone(),
                    };
                    if self.condition(&test)? {
                        return self.statements(body);
                    }
                }
                if let Some(body) = otherwise {
                    return self.statements(body);
                }
            }
            Stmt::For { label, variable, reverse, start, end, body } => {
                let (start, end) = (self.integer(start)?, self.integer(end)?);
                let counters: Box<dyn Iterator<Item = i64>> =
                    if *reverse { Box::new((start..=end).rev()) } else { Box::new(start..=end) };
                for i in counters {
                    self.scopes.push(Scope::default());
                    self.declare(variable, Some("NUMBER(38,0)".into()), Value::integer(i));
                    let flow = self.statements(body);
                    self.scopes.pop();
                    match loop_flow(flow?, label) {
                        LoopFlow::Next => {}
                        LoopFlow::Stop => break,
                        LoopFlow::Exit(flow) => return Ok(flow),
                    }
                }
            }
            Stmt::While { label, condition, body } => {
                while self.condition(condition)? {
                    match loop_flow(self.statements(body)?, label) {
                        LoopFlow::Next => {}
                        LoopFlow::Stop => break,
                        LoopFlow::Exit(flow) => return Ok(flow),
                    }
                }
            }
            Stmt::Repeat { label, body, until } => loop {
                match loop_flow(self.statements(body)?, label) {
                    LoopFlow::Next => {}
                    LoopFlow::Stop => break,
                    LoopFlow::Exit(flow) => return Ok(flow),
                }
                if self.condition(until)? {
                    break;
                }
            },
            Stmt::Loop { label, body } => loop {
                match loop_flow(self.statements(body)?, label) {
                    LoopFlow::Next => {}
                    LoopFlow::Stop => break,
                    LoopFlow::Exit(flow) => return Ok(flow),
                }
            },
            Stmt::Break(label) => return Ok(Flow::Break(label.clone())),
            Stmt::Continue(label) => return Ok(Flow::Continue(label.clone())),
            Stmt::Return(expr) => {
                let value = match expr {
                    Some(expr) => self.evaluate(expr, None)?,
                    None => Value::null("VARCHAR"),
                };
                return Ok(Flow::Return(value));
            }
            Stmt::Raise(Some(name)) => return Err(self.user_exception(name)?),
            Stmt::Raise(None) => {
                let handling = self.scopes.iter().rev().any(|s| s.exceptions.contains_key("\u{0}current"));
                if !handling {
                    return Err(self.expression_error(EngineError::raise_outside_handler()));
                }
                return Err(Failure::Raised(Box::new(Raised {
                    name: "\u{0}reraise".into(),
                    code: 0,
                    message: String::new(),
                    sql_state: String::new(),
                    line: self.line,
                    position: self.position,
                    cause: None,
                })));
            }
            Stmt::ExecuteImmediate { sql, using } => {
                if let Outcome::Value(Some(value)) = self.execute_immediate(sql, using)? {
                    // A nested block's RETURN doesn't return from this one.
                    let _ = value;
                }
            }
            Stmt::Null => {}
            Stmt::Declare(declaration) => self.declare_other(declaration)?,
            Stmt::Open { cursor, using } => {
                let values: Vec<String> = using
                    .iter()
                    .map(|n| {
                        self.literal_of(n).ok_or_else(|| self.expression_error(EngineError::undeclared_variable(n)))
                    })
                    .collect::<Run<_>>()?;
                let rows = self.cursor_rows(cursor, &values)?;
                self.cursor_mut(cursor)?.open = Some((rows, 0));
            }
            Stmt::Fetch { cursor, into } => {
                let (result, position) = match &self.cursor_mut(cursor)?.open {
                    Some((result, position)) => (result.clone(), *position),
                    None => return Err(self.statement_error(EngineError::cursor_not_open(cursor))),
                };
                self.cursor_mut(cursor)?.open = Some((result.clone(), position + 1));
                let rows = result.json_rows().map_err(|e| self.statement_error(e))?;
                // Past the last row, the variables become NULL.
                for (i, name) in into.iter().enumerate() {
                    let Some(column) = result.columns.get(i) else { break };
                    let cell = rows.get(position).and_then(|r| r[i].as_deref());
                    self.assign_converted(name, to_value(column, cell))?;
                }
            }
            Stmt::Close(cursor) => self.cursor_mut(cursor)?.open = None,
            Stmt::ForCursor { label, record, cursor, body } => {
                let result = self.cursor_rows(cursor, &[])?;
                let rows = result.json_rows().map_err(|e| self.statement_error(e))?;
                for row in rows {
                    self.scopes.push(Scope::default());
                    for (column, cell) in result.columns.iter().zip(&row) {
                        let field = format!("{record}.{}", column.name.to_uppercase());
                        let value = to_value(column, cell.as_deref());
                        self.declare(&field, Some(value.data_type.clone()), value);
                    }
                    let flow = self.statements(body);
                    self.scopes.pop();
                    match loop_flow(flow?, label) {
                        LoopFlow::Next => {}
                        LoopFlow::Stop => break,
                        LoopFlow::Exit(flow) => return Ok(flow),
                    }
                }
            }
            Stmt::ReturnTable(source) => {
                let source = source.trim();
                let upper = source.to_uppercase();
                let result = match upper.strip_prefix("RESULTSET_FROM_CURSOR") {
                    Some(rest) => {
                        let cursor = rest.trim().trim_start_matches('(').trim_end_matches(')').trim().to_uppercase();
                        self.cursor_rows(&cursor, &[])?
                    }
                    None => self.resultset(&upper)?,
                };
                return Ok(Flow::ReturnTable(result));
            }
        }
        Ok(Flow::Next)
    }

    /// Declares a cursor, RESULTSET or exception in the current scope.
    fn declare_other(&mut self, declaration: &Declaration) -> Run<()> {
        match declaration {
            Declaration::Variable { name, data_type, default } => {
                let value = match default {
                    Some(expr) => self.evaluate(expr, data_type.as_deref())?,
                    None => Value::null(data_type.as_deref().unwrap_or("VARCHAR")),
                };
                let data_type = data_type.clone().or_else(|| Some(inferred_type(&value)));
                self.declare(name, data_type, value);
            }
            Declaration::Exception { name, code, message } => {
                self.scopes.last_mut().unwrap().exceptions.insert(name.clone(), (*code, message.clone()));
            }
            Declaration::Cursor { name, source } => {
                let cursor = Cursor { source: source.clone(), open: None };
                self.scopes.last_mut().unwrap().cursors.insert(name.clone(), cursor);
            }
            Declaration::ResultSet { name, query } => {
                let result = match query {
                    Some(query) => Some(self.run_query_expression(query)?),
                    None => None,
                };
                self.scopes.last_mut().unwrap().resultsets.insert(name.clone(), result);
            }
        }
        Ok(())
    }

    fn find_resultset(&mut self, name: &str) -> Option<&mut Option<Arc<QueryResult>>> {
        self.scopes.iter_mut().rev().find_map(|s| s.resultsets.get_mut(name))
    }

    fn resultset(&mut self, name: &str) -> Run<Arc<QueryResult>> {
        match self.find_resultset(name) {
            Some(Some(result)) => Ok(result.clone()),
            Some(None) => Err(self.statement_error(EngineError::resultset_not_assigned(name))),
            None => Err(self.expression_error(EngineError::undeclared_variable(name))),
        }
    }

    fn cursor_mut(&mut self, name: &str) -> Run<&mut Cursor> {
        let error = self.expression_error(EngineError::undeclared_variable(name));
        self.scopes.iter_mut().rev().find_map(|s| s.cursors.get_mut(name)).ok_or(error)
    }

    /// A cursor's rows: those it was opened with, or its query (with `?` bound
    /// to `values`) or RESULTSET's.
    fn cursor_rows(&mut self, name: &str, values: &[String]) -> Run<Arc<QueryResult>> {
        if values.is_empty()
            && let Some((result, _)) = &self.cursor_mut(name)?.open
        {
            return Ok(result.clone());
        }
        match self.cursor_mut(name)?.source.clone() {
            CursorSource::ResultSet(resultset) => self.resultset(&resultset),
            CursorSource::Query(query) => {
                let sql = localflake_sql::scripting::bind_placeholders(&query, values)
                    .map_err(|e| self.statement_error(e.into()))?;
                Ok(Arc::new(self.sql_statement(&sql)?))
            }
        }
    }

    /// `(SELECT ...)` or `(EXECUTE IMMEDIATE ...)` assigned to a RESULTSET.
    fn run_query_expression(&mut self, expr: &str) -> Run<Arc<QueryResult>> {
        let mut text = expr.trim();
        // ASYNC queries run straight away.
        if text.to_uppercase().starts_with("ASYNC") {
            text = text[5..].trim();
        }
        let inner = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')).unwrap_or(text).trim();
        if inner.to_uppercase().starts_with("EXECUTE") {
            let parsed = localflake_sql::scripting::parse(inner).map_err(|e| self.statement_error(e.into()))?;
            let Stmt::ExecuteImmediate { sql, using } = parsed else {
                return Err(self.statement_error(EngineError::resultset_query(inner)));
            };
            return match self.execute_immediate(&sql, &using)? {
                Outcome::Result(result) => Ok(Arc::new(result)),
                Outcome::Value(_) => Err(self.statement_error(EngineError::resultset_query(inner))),
            };
        }
        Ok(Arc::new(self.sql_statement(inner)?))
    }

    /// Assigns a value, converting it to the variable's declared type.
    fn assign_converted(&mut self, name: &str, value: Value) -> Run<()> {
        let declared = self.lookup(name).and_then(|v| v.data_type.clone());
        let value = match declared {
            Some(t) if t != value.data_type => self.evaluate(&value.literal, Some(&t))?,
            _ => value,
        };
        self.assign(name, value)
    }

    /// A SQL statement with `:variable` references, and `SELECT ... INTO :a, :b`.
    fn sql_statement(&mut self, sql: &str) -> Run<QueryResult> {
        if sql.trim_start().get(..4).is_some_and(|w| w.eq_ignore_ascii_case("CALL"))
            && let Some(call) = procedure::parse_call(sql).map_err(|e| self.statement_error(e.into()))?
        {
            return self.call_statement(&call);
        }
        let into = localflake_sql::scripting::select_into(sql).map_err(|e| self.statement_error(e.into()))?;
        let (sql, targets) = match into {
            Some((sql, targets)) => (sql, targets),
            None => (sql.to_owned(), vec![]),
        };
        let substituted = self.substitute(&sql, false).map_err(|e| self.statement_error(e))?;
        let result = self.run_sql(&substituted).map_err(|e| self.statement_error(e))?;
        let dml_rows = result.kind.is_dml().then(|| dml_row_count(&result));
        let activity = match result.kind {
            StatementKind::Query => Some(result.row_count() as i64),
            _ => dml_rows,
        };
        self.set_dml_status(dml_rows, activity);
        if !targets.is_empty() {
            let rows = result.json_rows().map_err(|e| self.statement_error(e))?;
            if rows.len() > 1 {
                return Err(self.statement_error(EngineError::select_into_rows()));
            }
            for (i, name) in targets.iter().enumerate() {
                let Some(column) = result.columns.get(i) else { break };
                let cell = rows.first().and_then(|r| r[i].as_deref());
                self.assign_converted(name, to_value(column, cell))?;
            }
        }
        Ok(result)
    }

    /// `CALL` in a block: a query of its own, whose value `INTO :var` assigns.
    fn call_statement(&mut self, call: &Call) -> Run<QueryResult> {
        let (outcome, name) = self.call(call)?;
        let result = match outcome {
            Outcome::Result(mut result) => {
                let query_id = new_query_id();
                result.query_id = query_id.clone();
                self.session.engine.store(&query_id, Ok(Arc::new(result.clone())));
                self.session.ctx.query_history.push(query_id);
                result
            }
            Outcome::Value(value) => {
                let value = value.unwrap_or_else(|| Value::null("VARCHAR"));
                if let Some(into) = &call.into {
                    self.assign_converted(into, value.clone())?;
                }
                let sql = format!("SELECT {} AS {}", value.literal, column_alias(&name));
                self.run_sql(&sql).map_err(|e| self.statement_error(e))?
            }
        };
        self.set_dml_status(None, Some(result.row_count() as i64));
        Ok(result)
    }

    /// Runs a stored procedure with its arguments as its only variables. Returns
    /// its value (converted to its return type) or table, and its name.
    fn call(&mut self, call: &Call) -> Run<(Outcome, String)> {
        self.check_cancelled()?;
        if self.depth >= MAX_CALL_DEPTH {
            return Err(self.statement_error(EngineError::call_depth()));
        }
        let overloads = self.session.procedure_overloads(&call.name).map_err(|e| self.statement_error(e))?;
        let mut values = Vec::new();
        for (name, expr) in &call.arguments {
            values.push((name.clone(), self.evaluate(expr, None)?));
        }
        let Some((stored, bound)) = choose_overload(overloads, &values) else {
            let name = localflake_sql::decode_identifier(call.name.last().map_or("", String::as_str));
            return Err(self.statement_error(EngineError::procedure_arguments(&name)));
        };
        let procedure = &stored.procedure;
        let mut arguments = Vec::new();
        for (argument, slot) in procedure.arguments.iter().zip(&bound) {
            let value = match (slot, &argument.default) {
                (Some(i), _) => values[*i].1.clone(),
                (None, Some(default)) => self.evaluate(default, None)?,
                (None, None) => Value::null(&argument.data_type),
            };
            arguments.push(self.evaluate(&value.literal, Some(&argument.data_type))?);
        }
        let return_type = match &procedure.returns {
            Returns::Scalar { data_type, .. } => data_type.as_str(),
            Returns::Table(_) => "VARCHAR",
        };
        if procedure.null_input == "RETURNS NULL ON NULL INPUT"
            && arguments.iter().any(|v| v.literal.starts_with("NULL"))
        {
            return Ok((Outcome::Value(Some(Value::null(return_type))), stored.name));
        }
        let body = match procedure.body_normalized {
            true => procedure.body.clone(),
            false => {
                localflake_sql::normalize_identifiers(&procedure.body).map_err(|e| self.statement_error(e.into()))?
            }
        };
        if !localflake_sql::scripting::is_script(&body) {
            let err =
                localflake_sql::SqlError::Unsupported("procedure bodies that aren't Snowflake Scripting blocks".into());
            return Err(self.statement_error(err.into()));
        }
        let parsed = localflake_sql::scripting::parse(&body).map_err(|e| self.statement_error(e.into()))?;

        let saved = std::mem::replace(&mut self.scopes, vec![Scope::default()]);
        let saved_handlers = std::mem::take(&mut self.handlers);
        let (line, position) = (self.line, self.position);
        self.depth += 1;
        self.reset_status();
        for (argument, value) in procedure.arguments.iter().zip(arguments) {
            self.declare(&argument.name, Some(argument.data_type.clone()), value);
        }
        // Each nested call takes a few hundred KB of stack in debug builds.
        let outcome = stacker::maybe_grow(512 * 1024, 4 * 1024 * 1024, || self.top_level(&parsed));
        // OUT arguments pass their final values back to the caller's variables.
        let outs: Vec<(String, Value)> = procedure
            .arguments
            .iter()
            .zip(&bound)
            .filter(|(argument, _)| argument.out)
            .filter_map(|(argument, slot)| {
                let target = call.arguments[(*slot)?].1.trim().trim_start_matches(':').trim();
                let target = match target.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                    Some(quoted) => quoted.to_owned(),
                    None => target.to_uppercase(),
                };
                Some((target, self.lookup(&argument.name)?.value.clone()))
            })
            .collect();
        self.scopes = saved;
        self.handlers = saved_handlers;
        (self.line, self.position) = (line, position);
        self.depth -= 1;

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(Failure::Raised(raised)) => return Err(self.statement_error(uncaught(&raised))),
            Err(fatal) => return Err(fatal),
        };
        for (target, value) in outs {
            if self.lookup(&target).is_some() {
                self.assign_converted(&target, value)?;
            }
        }
        let outcome = match (outcome, &procedure.returns) {
            (Outcome::Value(value), Returns::Scalar { data_type, not_null }) => {
                let value = match value {
                    Some(value) => self.evaluate(&value.literal, Some(data_type))?,
                    None => Value::null(data_type),
                };
                if *not_null && value.literal.starts_with("NULL") {
                    return Err(self.statement_error(EngineError::null_result()));
                }
                Outcome::Value(Some(value))
            }
            (Outcome::Result(mut result), Returns::Table(columns)) => {
                for (column, (name, _)) in result.columns.iter_mut().zip(columns) {
                    column.name = localflake_sql::decode_identifier(name);
                }
                Outcome::Result(result)
            }
            (outcome, _) => outcome,
        };
        Ok((outcome, stored.name))
    }

    /// `EXECUTE IMMEDIATE <expr> [USING (...)]`: runs the text as a statement or block.
    fn execute_immediate(&mut self, expr: &str, using: &[String]) -> Run<Outcome> {
        let text = self.evaluate(expr, Some("VARCHAR"))?;
        let Some(text) = string_value(&text.literal) else {
            return Err(self.expression_error(EngineError::execute_immediate_null()));
        };
        let values: Vec<String> = using
            .iter()
            .map(|name| {
                self.literal_of(name).ok_or_else(|| self.expression_error(EngineError::undeclared_variable(name)))
            })
            .collect::<Run<_>>()?;
        let normalized = localflake_sql::normalize_identifiers(&text).map_err(|e| self.statement_error(e.into()))?;
        let sql = localflake_sql::scripting::bind_placeholders(&normalized, &values)
            .map_err(|e| self.statement_error(e.into()))?;
        let sql = sql.trim().trim_end_matches(';').trim();
        if localflake_sql::scripting::is_script(sql) {
            let parsed = localflake_sql::scripting::parse(sql).map_err(|e| self.statement_error(e.into()))?;
            // A block runs with its own variables.
            let saved = std::mem::replace(&mut self.scopes, vec![Scope::default()]);
            let saved_handlers = std::mem::take(&mut self.handlers);
            let (line, position) = (self.line, self.position);
            self.reset_status();
            let outcome = self.top_level(&parsed);
            self.scopes = saved;
            self.handlers = saved_handlers;
            (self.line, self.position) = (line, position);
            return outcome;
        }
        Ok(Outcome::Result(self.sql_statement(sql)?))
    }
}

enum LoopFlow {
    Next,
    Stop,
    Exit(Flow),
}

/// What a loop does after its body: BREAK and CONTINUE without a label, or
/// with this loop's label, apply here; others (and RETURN) leave the loop.
fn loop_flow(flow: Flow, label: &Option<String>) -> LoopFlow {
    let ours = |l: &Option<String>| l.is_none() || l == label;
    match flow {
        Flow::Next => LoopFlow::Next,
        Flow::Continue(l) if ours(&l) => LoopFlow::Next,
        Flow::Break(l) if ours(&l) => LoopFlow::Stop,
        other => LoopFlow::Exit(other),
    }
}

/// The overload a CALL's arguments fit, preferring the one whose argument types
/// match the values' best, and which argument (if any) each parameter takes.
/// Arguments are all positional or all named (`name => value`).
fn choose_overload(overloads: Vec<Stored>, values: &[(Option<String>, Value)]) -> Option<(Stored, Vec<Option<usize>>)> {
    let named = values.iter().any(|(name, _)| name.is_some());
    let mut best: Option<(usize, Stored, Vec<Option<usize>>)> = None;
    for stored in overloads {
        let parameters = &stored.procedure.arguments;
        let bound: Vec<Option<usize>> = if named {
            if values.iter().any(|(name, _)| name.as_ref().is_none_or(|n| !parameters.iter().any(|p| &p.name == n))) {
                continue;
            }
            parameters.iter().map(|p| values.iter().position(|(name, _)| name.as_ref() == Some(&p.name))).collect()
        } else {
            if values.len() > parameters.len() {
                continue;
            }
            (0..parameters.len()).map(|i| (i < values.len()).then_some(i)).collect()
        };
        if parameters.iter().zip(&bound).any(|(p, slot)| slot.is_none() && p.default.is_none()) {
            continue;
        }
        let score = parameters
            .iter()
            .zip(&bound)
            .filter(|(p, slot)| {
                slot.is_some_and(|i| {
                    procedure::type_family(&p.data_type) == procedure::type_family(&values[i].1.data_type)
                })
            })
            .count();
        if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
            best = Some((score, stored, bound));
        }
    }
    best.map(|(_, stored, bound)| (stored, bound))
}

fn find_handler<'h>(handlers: &'h [Handler], raised: &Raised) -> Option<&'h Handler> {
    handlers.iter().find(|h| h.names.iter().any(|n| n == &raised.name || n == "OTHER"))
}

/// Rows a DML statement affected: the sum of its row-count columns.
fn dml_row_count(result: &QueryResult) -> i64 {
    let rows = result.json_rows().unwrap_or_default();
    rows.first().map_or(0, |row| row.iter().flatten().filter_map(|c| c.parse::<i64>().ok()).sum())
}

/// The type an untyped variable takes from its first value: decimals become
/// FLOAT, as Snowflake infers.
fn inferred_type(value: &Value) -> String {
    match value.data_type.strip_prefix("NUMBER(") {
        Some(rest) if !rest.ends_with(",0)") => "FLOAT".into(),
        _ => value.data_type.clone(),
    }
}

/// A Snowflake string literal. A plain literal (rather than an expression)
/// gets Snowflake's implicit conversions, as in `DATEADD(day, 1, '2024-05-08')`.
fn text_literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

/// The text of a VARCHAR value's literal, or None for NULL.
fn string_value(literal: &str) -> Option<String> {
    let inner = literal.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace("''", "'").replace("\\\\", "\\"))
}

/// A result value (in JSON wire encoding) as a typed literal.
fn to_value(column: &Column, cell: Option<&str>) -> Value {
    let data_type = type_name(column);
    let Some(cell) = cell else { return Value::null(&data_type) };
    let literal = match column.sf_type {
        SfType::Fixed => format!("({cell})::{data_type}"),
        SfType::Real => format!("'{cell}'::FLOAT"),
        SfType::Text => text_literal(cell),
        SfType::Boolean => if cell == "1" { "TRUE" } else { "FALSE" }.to_owned(),
        SfType::Date => {
            let days: i64 = cell.parse().unwrap_or_default();
            let date = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() + TimeDelta::days(days);
            format!("'{date}'::DATE")
        }
        SfType::Time => {
            let nanos = nanos_of(cell);
            let time = NaiveTime::MIN + TimeDelta::nanoseconds(nanos as i64);
            format!("'{}'::TIME", time.format("%H:%M:%S%.9f"))
        }
        SfType::TimestampNtz | SfType::TimestampLtz => {
            let instant = DateTime::from_timestamp_nanos(nanos_of(cell) as i64);
            let zone = if column.sf_type == SfType::TimestampLtz { " +00:00" } else { "" };
            format!("'{}{zone}'::{data_type}", instant.naive_utc().format("%Y-%m-%d %H:%M:%S%.9f"))
        }
        SfType::TimestampTz => {
            let (instant, code) = cell.split_once(' ').unwrap_or((cell, "1440"));
            let offset_minutes = code.parse::<i64>().unwrap_or(1440) - 1440;
            let local = DateTime::from_timestamp_nanos(nanos_of(instant) as i64) + TimeDelta::minutes(offset_minutes);
            let sign = if offset_minutes < 0 { '-' } else { '+' };
            format!(
                "'{} {sign}{:02}:{:02}'::TIMESTAMP_TZ",
                local.naive_utc().format("%Y-%m-%d %H:%M:%S%.9f"),
                offset_minutes.abs() / 60,
                offset_minutes.abs() % 60
            )
        }
        SfType::Binary => format!("TO_BINARY('{cell}', 'HEX')"),
        SfType::Variant => format!("PARSE_JSON({})", text_literal(cell)),
        SfType::Object => format!("PARSE_JSON({})::OBJECT", text_literal(cell)),
        SfType::Array => format!("PARSE_JSON({})::ARRAY", text_literal(cell)),
        SfType::Vector => format!("{cell}::{data_type}"),
    };
    Value { literal, data_type }
}

/// `seconds.fraction` as nanoseconds.
fn nanos_of(text: &str) -> i128 {
    let negative = text.starts_with('-');
    let (whole, fraction) = text.trim_start_matches('-').split_once('.').unwrap_or((text.trim_start_matches('-'), ""));
    let fraction = format!("{fraction:0<9}");
    let nanos =
        whole.parse::<i128>().unwrap_or_default() * 1_000_000_000 + fraction[..9].parse::<i128>().unwrap_or_default();
    if negative { -nanos } else { nanos }
}

fn type_name(column: &Column) -> String {
    match column.sf_type {
        SfType::Fixed => format!("NUMBER({},{})", column.precision.unwrap_or(38), column.scale.unwrap_or(0)),
        SfType::Real => "FLOAT".into(),
        SfType::Text => "VARCHAR".into(),
        SfType::Boolean => "BOOLEAN".into(),
        SfType::Date => "DATE".into(),
        SfType::Time => "TIME".into(),
        SfType::TimestampNtz => "TIMESTAMP_NTZ".into(),
        SfType::TimestampLtz => "TIMESTAMP_LTZ".into(),
        SfType::TimestampTz => "TIMESTAMP_TZ".into(),
        SfType::Binary => "BINARY".into(),
        SfType::Variant => "VARIANT".into(),
        SfType::Object => "OBJECT".into(),
        SfType::Array => "ARRAY".into(),
        SfType::Vector => {
            let (element, dimension) = column.vector.unwrap_or((SfType::Real, 0));
            let element = if element == SfType::Fixed { "INT" } else { "FLOAT" };
            format!("VECTOR({element}, {dimension})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_round_trip_text() {
        assert_eq!(string_value(&text_literal("it's a $ \\ test")).unwrap(), "it's a $ \\ test");
        assert_eq!(string_value(&text_literal("cost $$5")).unwrap(), "cost $$5");
        assert_eq!(nanos_of("-1.5"), -1_500_000_000);
    }
}
