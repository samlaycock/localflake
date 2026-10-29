//! DuckDB-backed storage and query execution with Snowflake semantics.
//!
//! Each Snowflake database is a DuckDB catalog attached to a shared instance,
//! either in memory or as `<data_dir>/<NAME>.duckdb`. Each Snowflake session
//! gets its own DuckDB connection, so `USE` and session settings stay local.

mod cancel;
mod encode;
mod error;
mod files;
mod functions;
mod match_recognize;
mod probe;
mod stage;
mod tz;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use duckdb::Connection;
use duckdb::arrow::datatypes::DataType;
use duckdb::core::LogicalTypeId;
use duckdb::types::Value;
pub use localflake_sql::StatementKind;
use localflake_sql::{Command, Context, SqlCommand, TimestampMapping, UseTarget};

pub use cancel::CancelToken;
pub use encode::{Column, QueryResult, SfType};
pub use error::EngineError;
pub use stage::Transfer;

const DEFAULT_SCHEMA: &str = "PUBLIC";
/// Snowflake's default for the TIMEZONE parameter.
const DEFAULT_TIMEZONE: &str = "America/Los_Angeles";

/// Loads DuckDB's ICU extension (time zone support): the copy embedded at
/// build time if there is one (see `build.rs`), else DuckDB's runtime install.
/// Without it, localflake runs in UTC only.
fn load_icu(conn: &Connection) -> bool {
    if std::env::var_os("LOCALFLAKE_NO_ICU").is_some() {
        return false;
    }
    let loaded = match embedded_icu() {
        Ok(Some(path)) => {
            conn.execute_batch(&format!("LOAD {}", literal(&path.to_string_lossy()))).inspect_err(|err| {
                tracing::warn!(%err, path = %path.display(), "cannot load the embedded ICU extension");
            })
        }
        Ok(None) => conn.execute_batch("LOAD icu").or_else(|_| conn.execute_batch("INSTALL icu; LOAD icu")),
        Err(err) => {
            tracing::warn!(%err, "cannot unpack the embedded ICU extension");
            conn.execute_batch("LOAD icu").or_else(|_| conn.execute_batch("INSTALL icu; LOAD icu"))
        }
    };
    if let Err(err) = &loaded {
        tracing::warn!(%err, "ICU extension unavailable: TIMESTAMP_LTZ will use UTC regardless of TIMEZONE");
    }
    loaded.is_ok()
}

mod icu {
    include!(concat!(env!("OUT_DIR"), "/icu_extension.rs"));
}

/// Unpacks the embedded ICU extension into the temp directory, once per
/// version and content, and returns its path.
fn embedded_icu() -> std::io::Result<Option<PathBuf>> {
    use std::hash::{Hash, Hasher};
    use std::io::Read;
    let Some(gzipped) = icu::ICU_EXTENSION else { return Ok(None) };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    gzipped.hash(&mut hasher);
    // DuckDB names an extension after its file, so the file must be `icu.duckdb_extension`.
    let dir = std::env::temp_dir().join(format!("localflake/icu-{}-{:016x}", icu::DUCKDB_VERSION, hasher.finish()));
    let path = dir.join("icu.duckdb_extension");
    if !path.exists() {
        std::fs::create_dir_all(&dir)?;
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(gzipped).read_to_end(&mut bytes)?;
        // Write then rename, so concurrent starts never load a partial file.
        let partial = path.with_extension(format!("{}.part", std::process::id()));
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(Some(path))
}
/// Results kept for `RESULT_SCAN`, async queries and multi-statement requests.
const STORED_RESULTS: usize = 500;

pub type StoredResult = Result<Arc<QueryResult>, EngineError>;

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    root: Mutex<Connection>,
    /// The database every connection shares, opened through the C API so
    /// `sf_probe` can be registered with a bind callback. Held to close it last.
    _db: RawDatabase,
    /// Whether DuckDB's ICU extension is loaded. With it, TIMESTAMP_LTZ follows
    /// the session's TIMEZONE; without it everything runs in UTC.
    icu: bool,
    data_dir: Option<PathBuf>,
    results: Mutex<ResultStore>,
    next_session: AtomicU64,
    /// Whether logging in with an unknown database creates it (the default,
    /// convenient for tests) or logs in without a current database, as Snowflake does.
    create_missing_databases: std::sync::atomic::AtomicBool,
    stages: stage::StageStore,
    running: Arc<cancel::Running>,
}

struct RawDatabase(duckdb::ffi::duckdb_database);

// The handle is only used to open connections and to close the database once.
unsafe impl Send for RawDatabase {}
unsafe impl Sync for RawDatabase {}

impl Drop for RawDatabase {
    fn drop(&mut self) {
        unsafe { duckdb::ffi::duckdb_close(&mut self.0) };
    }
}

#[derive(Default)]
struct ResultStore {
    results: HashMap<String, StoredResult>,
    order: VecDeque<String>,
}

impl ResultStore {
    fn insert(&mut self, query_id: String, result: StoredResult) {
        if self.order.len() >= STORED_RESULTS
            && let Some(oldest) = self.order.pop_front()
        {
            self.results.remove(&oldest);
        }
        self.order.push_back(query_id.clone());
        self.results.insert(query_id, result);
    }
}

/// Who is connecting and with what initial context.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub account: String,
    pub user: String,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub warehouse: Option<String>,
    pub role: Option<String>,
    pub parameters: Vec<(String, String)>,
}

/// A bind value from the driver.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Null,
    Bool(bool),
    Int(i64),
    /// A number with a fractional part or beyond 64 bits, as text.
    Decimal(String),
    Float(f64),
    Text(String),
    /// Days since the epoch.
    Date(i32),
    /// Microseconds since midnight.
    Time(i64),
    /// Microseconds since the epoch, wall-clock.
    Timestamp(i64),
    /// Microseconds since the epoch, UTC instant (TIMESTAMP_LTZ).
    TimestampLtz(i64),
    /// An instant (microseconds since the epoch) and its UTC offset (TIMESTAMP_TZ).
    TimestampTz {
        micros: i64,
        offset_minutes: i32,
    },
    Binary(Vec<u8>),
}

/// Converts placeholders whose binds can't be bound with their own type
/// (see `Bind::to_value`): `$n` becomes `make_timestamp_ns($n)` for
/// TIMESTAMP_TZ and the instant of that for TIMESTAMP_LTZ.
fn typed_placeholders(sql: &str, binds: &[Bind]) -> String {
    if !binds.iter().any(|b| matches!(b, Bind::TimestampLtz(_) | Bind::TimestampTz { .. })) {
        return sql.to_owned();
    }
    map_placeholders(sql, |n| match binds.get(n.wrapping_sub(1)) {
        Some(Bind::TimestampLtz(_)) => Some(format!("sf_tz_instant(make_timestamp_ns(${n}))")),
        Some(Bind::TimestampTz { .. }) => Some(format!("make_timestamp_ns(${n})")),
        _ => None,
    })
}

/// Replaces the placeholders of type-probe SQL with NULLs of their binds'
/// types. DuckDB defers binding a statement to execution when it can't type a
/// parameter (`parse_json($1)`, or one inside a lambda such as FLATTEN's, even
/// when cast), so preparing the probe would record nothing. Probe SQL is only
/// prepared, never run.
///
/// Text binds become string literals instead: like an untyped parameter, a
/// literal takes its type from its context, whereas a VARCHAR can't be
/// range-compared with a date or time (`d >= ?`), which fails the probe.
fn probe_placeholders(sql: &str, binds: &[Bind]) -> String {
    if binds.is_empty() {
        return sql.to_owned();
    }
    map_placeholders(sql, |n| {
        let bind = binds.get(n.wrapping_sub(1))?;
        let type_name = match bind {
            Bind::Null => return Some("NULL".to_owned()),
            Bind::Text(s) => return Some(format!("'{}'", s.replace('\'', "''"))),
            Bind::Bool(_) => "BOOLEAN",
            Bind::Int(_) | Bind::TimestampLtz(_) | Bind::TimestampTz { .. } => "BIGINT",
            Bind::Decimal(_) => "VARCHAR",
            Bind::Float(_) => "DOUBLE",
            Bind::Date(_) => "DATE",
            Bind::Time(_) => "TIME",
            Bind::Timestamp(_) => "TIMESTAMP",
            Bind::Binary(_) => "BLOB",
        };
        let typed = format!("CAST(NULL AS {type_name})");
        Some(match bind {
            Bind::TimestampLtz(_) => format!("sf_tz_instant(make_timestamp_ns({typed}))"),
            Bind::TimestampTz { .. } => format!("make_timestamp_ns({typed})"),
            _ => typed,
        })
    })
}

/// Replaces each `$n` placeholder outside quotes with `wrap(n)`, if any.
fn map_placeholders(sql: &str, wrap: impl Fn(usize) -> Option<String>) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 32);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                out.push_str(&sql[i..(j + 1).min(bytes.len())]);
                i = j + 1;
            }
            b'$' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let len = bytes[i + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                let n: usize = sql[i + 1..i + 1 + len].parse().unwrap_or(0);
                match wrap(n) {
                    Some(text) => out.push_str(&text),
                    None => out.push_str(&sql[i..i + 1 + len]),
                }
                i += 1 + len;
            }
            _ => {
                let c = sql[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

impl Bind {
    fn to_value(&self) -> Value {
        use duckdb::types::TimeUnit::Microsecond;
        match self {
            Bind::Null => Value::Null,
            Bind::Bool(b) => Value::Boolean(*b),
            Bind::Int(i) => Value::BigInt(*i),
            Bind::Decimal(s) | Bind::Text(s) => Value::Text(s.clone()),
            Bind::Float(f) => Value::Double(*f),
            Bind::Date(d) => Value::Date32(*d),
            Bind::Time(t) => Value::Time64(Microsecond, *t),
            Bind::Timestamp(t) => Value::Timestamp(Microsecond, *t),
            // duckdb-rs binds timestamps as microseconds, so these go as the
            // encoded TIMESTAMP_TZ value and `typed_placeholders` converts them.
            Bind::TimestampLtz(t) => Value::BigInt(tz::encode(*t, 0)),
            Bind::TimestampTz { micros, offset_minutes } => Value::BigInt(tz::encode(*micros, offset_minutes * 60)),
            Bind::Binary(b) => Value::Blob(b.clone()),
        }
    }
}

/// One request from a driver: SQL text plus options.
#[derive(Debug, Default)]
pub struct Request<'a> {
    pub sql: &'a str,
    /// Rows of bind values; more than one row means "execute for each row" (array binding).
    pub bindings: Vec<Vec<Bind>>,
    pub describe_only: bool,
    /// `MULTI_STATEMENT_COUNT` for this request (0 = any number).
    pub statement_count: Option<usize>,
    /// Cancels the request from another thread.
    pub cancel: CancelToken,
}

impl Engine {
    /// Opens an engine. With a `data_dir`, databases persist as one DuckDB
    /// file each and existing files are re-attached; otherwise everything
    /// lives in memory.
    pub fn open(data_dir: Option<&Path>) -> Result<Self, EngineError> {
        let mut raw = std::ptr::null_mut();
        if unsafe { duckdb::ffi::duckdb_open(std::ptr::null(), &mut raw) } != duckdb::ffi::duckdb_state_DuckDBSuccess {
            return Err(EngineError::internal("cannot open DuckDB"));
        }
        let db = RawDatabase(raw);
        let root = unsafe { Connection::open_from_raw(db.0) }?;
        functions::register(&root)?;
        let running = cancel::Running::open();
        root.register_scalar_function_with_state::<cancel::CancelQuery>("sf_cancel_query", &running)?;
        unsafe { probe::register(db.0) }.map_err(EngineError::internal)?;
        let icu = load_icu(&root);
        unsafe { tz::register(db.0) }.map_err(EngineError::internal)?;
        if let Some(dir) = data_dir {
            std::fs::create_dir_all(dir).map_err(EngineError::internal)?;
            for entry in std::fs::read_dir(dir).map_err(EngineError::internal)? {
                let path = entry.map_err(EngineError::internal)?.path();
                if path.extension().is_some_and(|e| e == "duckdb") {
                    let name = path.file_stem().unwrap().to_string_lossy();
                    root.execute_batch(&format!("ATTACH {} AS {};", literal(&path.to_string_lossy()), ident(&name)))?;
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                root: Mutex::new(root),
                _db: db,
                icu,
                data_dir: data_dir.map(Path::to_owned),
                results: Default::default(),
                next_session: AtomicU64::new(1),
                create_missing_databases: std::sync::atomic::AtomicBool::new(true),
                stages: stage::StageStore::open(data_dir).map_err(EngineError::internal)?,
                running,
            }),
        })
    }

    /// Starts a session. Unknown databases are created on the fly, which is
    /// friendlier than Snowflake (which logs in with no current database).
    pub fn session(&self, options: SessionOptions) -> Result<Session, EngineError> {
        let conn = self.inner.root.lock().unwrap().try_clone()?;
        conn.execute_batch("SET default_null_order = 'nulls_last_on_asc_first_on_desc'; SET TimeZone = 'UTC';")?;
        let mut parameters = options.parameters;
        // Snowflake's default TIMEZONE, when time zones can be honoured.
        if self.inner.icu && !parameters.iter().any(|(k, _)| k.eq_ignore_ascii_case("TIMEZONE")) {
            parameters.insert(0, ("TIMEZONE".to_owned(), DEFAULT_TIMEZONE.to_owned()));
        }
        conn.execute_batch(&localflake_sql::macros())?;
        let ctx = Context {
            account: options.account,
            user: options.user,
            role: Some(options.role.unwrap_or_else(|| "ACCOUNTADMIN".to_owned())),
            warehouse: options.warehouse,
            session_id: self.inner.next_session.fetch_add(1, Ordering::Relaxed),
            ..Default::default()
        };
        let mut session = Session {
            engine: self.clone(),
            interrupt: conn.interrupt_handle(),
            conn,
            ctx,
            in_transaction: false,
            transaction_failed: false,
            transaction_log: vec![],
            temporary_files: vec![],
            load_tables: 0,
            load_table_names: vec![],
        };
        session.set_parameters(parameters)?;
        if let Some(database) = options.database {
            let exists = session.database_exists(&database)?;
            if !exists && self.inner.create_missing_databases.load(Ordering::Relaxed) {
                session.create_database(&database, None)?;
            }
            if exists || self.inner.create_missing_databases.load(Ordering::Relaxed) {
                let schema = options.schema.filter(|s| session.schema_exists(&database, s).unwrap_or(false));
                session.use_database(&database, schema.as_deref())?;
            }
        }
        Ok(session)
    }

    /// See `Inner::create_missing_databases`.
    pub fn set_create_missing_databases(&self, create: bool) {
        self.inner.create_missing_databases.store(create, Ordering::Relaxed);
    }

    /// A staged file for a presigned URL path (relative to the stage root),
    /// or `None` if the path is not safe.
    pub fn stage_file(&self, relative: &str) -> Option<PathBuf> {
        self.inner.stages.file(relative)
    }

    /// Cancels a running statement by query ID, returning whether it was running.
    pub fn cancel_query(&self, query_id: &str) -> bool {
        self.inner.running.cancel_query(query_id)
    }

    /// A stored result by query ID.
    pub fn result(&self, query_id: &str) -> Option<StoredResult> {
        self.inner.results.lock().unwrap().results.get(query_id).cloned()
    }

    fn store(&self, query_id: &str, result: StoredResult) {
        self.inner.results.lock().unwrap().insert(query_id.to_owned(), result);
    }
}

pub struct Session {
    conn: Connection,
    /// Interrupts `conn`, for cancelling its statements (see `cancel`).
    interrupt: Arc<duckdb::InterruptHandle>,
    engine: Engine,
    ctx: Context,
    /// Whether a DuckDB transaction is open (duckdb-rs's `is_autocommit` is a stub).
    in_transaction: bool,
    /// A statement failed and DuckDB aborted the open transaction, so it must not report a successful COMMIT.
    transaction_failed: bool,
    /// The open transaction's successful data changes, replayed into a fresh
    /// transaction when DuckDB aborts it (see `recover_transaction`).
    transaction_log: Vec<(String, Vec<Vec<Bind>>)>,
    /// Temporary stages and file formats, removed when the session ends.
    temporary_files: Vec<PathBuf>,
    /// Counter naming the temp tables that hold staged files during a statement.
    load_tables: u64,
    load_table_names: Vec<String>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for path in &self.temporary_files {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// A failed statement and the query ID it was recorded under.
#[derive(Debug, Clone)]
pub struct Failure {
    pub query_id: String,
    pub error: EngineError,
}

impl Session {
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Session parameters drivers care about, with any session overrides.
    pub fn client_parameters(&self) -> Vec<(String, String)> {
        const REPORTED: &[&str] = &[
            "AUTOCOMMIT",
            "BINARY_OUTPUT_FORMAT",
            "CLIENT_TIMESTAMP_TYPE_MAPPING",
            "DATE_OUTPUT_FORMAT",
            "QUERY_TAG",
            "TIMESTAMP_LTZ_OUTPUT_FORMAT",
            "TIMESTAMP_NTZ_OUTPUT_FORMAT",
            "TIMESTAMP_OUTPUT_FORMAT",
            "TIMESTAMP_TYPE_MAPPING",
            "TIMESTAMP_TZ_OUTPUT_FORMAT",
            "TIMEZONE",
            "TIME_OUTPUT_FORMAT",
        ];
        localflake_sql::DEFAULT_PARAMETERS
            .iter()
            .filter(|(k, _)| REPORTED.contains(k))
            .map(|(k, v)| ((*k).to_owned(), self.ctx.parameters.get(*k).cloned().unwrap_or_else(|| (*v).to_owned())))
            .collect()
    }

    /// Runs a request, returning one result per statement. Results (and
    /// failures) are stored under their query IDs.
    pub fn execute(&mut self, request: &Request) -> Result<Vec<Arc<QueryResult>>, Failure> {
        let statements = localflake_sql::split(request.sql).map_err(|e| self.fail(e.into()))?;
        let desired = request.statement_count.unwrap_or_else(|| {
            self.ctx.parameters.get("MULTI_STATEMENT_COUNT").and_then(|v| v.parse().ok()).unwrap_or(1)
        });
        if desired != 0 && statements.len() != desired {
            return Err(self.fail(EngineError::statement_count(statements.len(), desired)));
        }
        let timeout = self.statement_timeout();
        let mut results = Vec::with_capacity(statements.len());
        for statement in &statements {
            let query_id = new_query_id();
            let bindings = if statements.len() == 1 { request.bindings.as_slice() } else { &[] };
            let result = match request.cancel.error(timeout) {
                Some(error) => Err(error),
                None => self.run_cancellable(statement, &query_id, bindings, request, timeout).map(Arc::new),
            };
            self.engine.store(&query_id, result.clone());
            self.ctx.query_history.push(query_id.clone());
            match result {
                Ok(result) => {
                    if self.in_transaction && result.kind.is_dml() && !request.describe_only {
                        self.transaction_log.push((statement.clone(), bindings.to_vec()));
                    } else if !self.in_transaction {
                        self.transaction_log.clear();
                    }
                    results.push(result)
                }
                Err(error) => {
                    tracing::debug!(%query_id, %statement, %error, "statement failed");
                    self.recover_transaction();
                    return Err(Failure { query_id, error });
                }
            }
        }
        Ok(results)
    }

    /// Runs a statement that the request's cancel token, `SYSTEM$CANCEL_QUERY`
    /// or the statement timeout can interrupt. An interrupted statement fails
    /// with Snowflake's cancellation error.
    fn run_cancellable(
        &mut self,
        statement: &str,
        query_id: &str,
        bindings: &[Vec<Bind>],
        request: &Request,
        timeout: u64,
    ) -> Result<QueryResult, EngineError> {
        let limit = (timeout > 0).then(|| std::time::Duration::from_secs(timeout));
        let running = self.engine.inner.running.start(query_id, &request.cancel, self.interrupt.clone(), limit);
        let result = self.run_statement(statement, query_id, bindings, request.describe_only);
        drop(running);
        let Some(error) = request.cancel.error(timeout) else { return result };
        // An interrupt that lands after the statement's last DuckDB query stays
        // pending inside a transaction; this query takes it instead of the next statement.
        let _ = self.conn.execute_batch("SELECT 1");
        result.map_err(|_| error)
    }

    /// `STATEMENT_TIMEOUT_IN_SECONDS`, where 0 means no limit.
    fn statement_timeout(&self) -> u64 {
        self.ctx.parameters.get("STATEMENT_TIMEOUT_IN_SECONDS").and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// Translates a statement, running a type probe first when its SQL has
    /// type-directed sites (see `localflake_sql::SiteTypes`).
    fn translate(&self, sql: &str) -> Result<Command, EngineError> {
        self.translate_bound(sql, &[])
    }

    /// [`Self::translate`] for a statement with `binds`, which type its placeholders in the probe.
    fn translate_bound(&self, sql: &str, binds: &[Bind]) -> Result<Command, EngineError> {
        let types = &self.ctx.types;
        types.probe();
        let command = localflake_sql::translate_statement(sql, &self.ctx);
        let command = match command {
            Ok(Command::Sql(probe)) if types.sites() > 0 => {
                // Each prepare only types the innermost sites (see `probe`), so
                // refine until every site has a type or a round learns nothing.
                let mut known = probe::site_types(&self.conn, &probe_placeholders(&probe.sql, binds));
                for _ in 0..8 {
                    types.refine(known.clone());
                    let Ok(Command::Sql(next)) = localflake_sql::translate_statement(sql, &self.ctx) else { break };
                    if types.unknown() == 0 {
                        break;
                    }
                    let learned = probe::site_types(&self.conn, &probe_placeholders(&next.sql, binds));
                    let before = known.len();
                    known.extend(learned);
                    if known.len() == before {
                        break;
                    }
                }
                types.known(known);
                localflake_sql::translate_statement(sql, &self.ctx)
            }
            other => other,
        };
        types.off();
        Ok(command?)
    }

    fn end_transaction(&mut self, sql: &str) -> Result<(), EngineError> {
        self.in_transaction = false;
        self.transaction_log.clear();
        if std::mem::take(&mut self.transaction_failed) {
            self.conn.execute_batch("ROLLBACK")?;
            return if sql == "COMMIT" { Err(EngineError::transaction_rolled_back()) } else { Ok(()) };
        }
        Ok(self.conn.execute_batch(sql)?)
    }

    /// Snowflake keeps a transaction open when one of its statements fails, but
    /// DuckDB aborts it. Roll back, open a fresh transaction and replay the
    /// transaction's earlier changes, so the transaction carries on as in
    /// Snowflake. If a replay fails, the next COMMIT fails instead.
    fn recover_transaction(&mut self) {
        if !self.in_transaction || self.conn.execute_batch("SELECT 1").is_ok() {
            return;
        }
        if self.conn.execute_batch("ROLLBACK; BEGIN TRANSACTION").is_err() {
            self.in_transaction = false;
            self.transaction_failed = true;
            return;
        }
        for (sql, bindings) in std::mem::take(&mut self.transaction_log) {
            match self.run_statement(&sql, &new_query_id(), &bindings, false) {
                Ok(_) => self.transaction_log.push((sql, bindings)),
                Err(err) => {
                    tracing::warn!(%err, %sql, "replaying the aborted transaction failed");
                    self.transaction_failed = true;
                    let _ = self.conn.execute_batch("ROLLBACK; BEGIN TRANSACTION");
                    self.transaction_log.clear();
                    return;
                }
            }
        }
    }

    fn autocommit(&self) -> bool {
        self.ctx.parameters.get("AUTOCOMMIT").is_none_or(|v| !v.eq_ignore_ascii_case("false"))
    }

    fn fail(&mut self, error: EngineError) -> Failure {
        let query_id = new_query_id();
        self.engine.store(&query_id, Err(error.clone()));
        Failure { query_id, error }
    }

    fn run_statement(
        &mut self,
        sql: &str,
        query_id: &str,
        bindings: &[Vec<Bind>],
        describe_only: bool,
    ) -> Result<QueryResult, EngineError> {
        // TIMESTAMP_TZ casts read the time zone of the statement being executed.
        let _zone = tz::enter(self.ctx.parameters.get("TIMEZONE").map_or("UTC", String::as_str));
        let result = self.run_translated(sql, query_id, bindings, describe_only);
        self.drop_load_tables();
        result
    }

    fn run_translated(
        &mut self,
        sql: &str,
        query_id: &str,
        bindings: &[Vec<Bind>],
        describe_only: bool,
    ) -> Result<QueryResult, EngineError> {
        let id = || query_id.to_owned();
        let ok = || Ok(QueryResult::status(id(), "Statement executed successfully.", StatementKind::Other));
        // Queries over staged files (`SELECT $1 FROM @stage`) read them into temp tables first.
        let expanded;
        let sql = if sql.contains('@') && localflake_sql::stage::parse(sql)?.is_none() {
            expanded = self.expand_stage_references(sql, None)?;
            expanded.as_str()
        } else {
            sql
        };
        match self.translate_bound(sql, bindings.first().map(Vec::as_slice).unwrap_or_default())? {
            Command::Stage(command) => self.run_stage_command(command, query_id),
            Command::Use(target) => {
                self.apply_use(target)?;
                ok()
            }
            Command::CreateDatabase { name, if_not_exists, or_replace, clone } => {
                if self.database_exists(&name)? {
                    if if_not_exists {
                        let message = format!("{name} already exists, statement succeeded.");
                        return Ok(QueryResult::status(id(), &message, StatementKind::Ddl));
                    }
                    if !or_replace {
                        return Err(EngineError::already_exists("Database", &name));
                    }
                    self.drop_database(&name)?;
                }
                self.create_database(&name, clone.as_deref())?;
                self.use_database(&name, None)?;
                Ok(QueryResult::status(id(), &format!("Database {name} successfully created."), StatementKind::Ddl))
            }
            Command::DropDatabase { name, if_exists } => {
                if !self.database_exists(&name)? {
                    if if_exists {
                        let message = format!("Drop statement executed successfully ({name} already dropped).");
                        return Ok(QueryResult::status(id(), &message, StatementKind::Ddl));
                    }
                    return Err(EngineError::does_not_exist("Database", &name));
                }
                self.drop_database(&name)?;
                Ok(QueryResult::status(id(), &format!("{name} successfully dropped."), StatementKind::Ddl))
            }
            Command::CreateSchema { sql, database, schema } => {
                let database =
                    database.or_else(|| self.ctx.database.clone()).ok_or_else(EngineError::no_current_database)?;
                self.conn.execute_batch(&sql)?;
                self.use_database(&database, Some(&schema))?;
                Ok(QueryResult::status(id(), &format!("Schema {schema} successfully created."), StatementKind::Ddl))
            }
            Command::AlterSession { set, unset } => {
                self.set_parameters(set)?;
                // Turning autocommit back on commits the open transaction.
                if self.autocommit() && self.in_transaction {
                    self.end_transaction("COMMIT")?;
                }
                for key in unset {
                    self.ctx.parameters.remove(&key);
                    if key == "TIMESTAMP_TYPE_MAPPING" {
                        self.ctx.timestamp_type_mapping = TimestampMapping::Ntz;
                    }
                    if key == "TIMEZONE" && self.engine.inner.icu {
                        self.set_parameters(vec![("TIMEZONE".to_owned(), DEFAULT_TIMEZONE.to_owned())])?;
                    }
                }
                ok()
            }
            Command::SetVariables(assignments) => {
                for (name, expr) in assignments {
                    let literal = self.evaluate_literal(&expr)?;
                    self.ctx.variables.insert(name, literal);
                }
                ok()
            }
            Command::UnsetVariables(names) => {
                for name in names {
                    self.ctx.variables.remove(&name);
                }
                ok()
            }
            Command::Describe { sql, object } => {
                let result = self.run_query(&sql, &[], false, query_id, StatementKind::Show)?;
                if result.row_count() == 0 {
                    return Err(EngineError::does_not_exist("Table", &object));
                }
                Ok(result)
            }
            Command::Ignored { kind } => Ok(QueryResult::status(id(), "Statement executed successfully.", kind)),
            Command::DropFunction { name, if_exists } => {
                let bare = name.rsplit('.').next().unwrap_or(&name).trim_matches('"').to_owned();
                let kind: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT function_type FROM duckdb_functions() WHERE function_name = ? AND function_type LIKE '%macro' LIMIT 1",
                        [&bare],
                        |r| r.get(0),
                    )
                    .ok();
                match kind.as_deref() {
                    Some("table_macro") => self.conn.execute_batch(&format!("DROP MACRO TABLE {name}"))?,
                    Some(_) => self.conn.execute_batch(&format!("DROP MACRO {name}"))?,
                    None if if_exists => {}
                    None => return Err(EngineError::does_not_exist("Function", &bare)),
                }
                Ok(QueryResult::status(id(), &format!("{bare} successfully dropped."), StatementKind::Ddl))
            }
            Command::MultiInsert { setup, clear, inserts } => {
                let labels: Vec<String> =
                    inserts.iter().map(|(table, _)| format!("number of rows inserted into {table}")).collect();
                if describe_only {
                    let zeros: Vec<_> = labels.iter().map(|l| (l.as_str(), 0)).collect();
                    return Ok(QueryResult::row_counts(id(), &zeros, StatementKind::Insert));
                }
                self.conn.execute_batch(&setup)?;
                let outcome = (|| -> Result<Vec<i64>, EngineError> {
                    for sql in &clear {
                        self.conn.execute_batch(sql)?;
                    }
                    inserts.iter().map(|(_, sql)| Ok(self.conn.execute(sql, [])? as i64)).collect()
                })();
                self.conn.execute_batch("DROP TABLE IF EXISTS __LF_INSERT_SOURCE")?;
                let counts = outcome?;
                let pairs: Vec<_> = labels.iter().map(String::as_str).zip(counts).collect();
                Ok(QueryResult::row_counts(id(), &pairs, StatementKind::Insert))
            }
            Command::Sql(command) => self.run_sql(&command, bindings, describe_only, query_id),
        }
    }

    fn run_sql(
        &mut self,
        command: &SqlCommand,
        bindings: &[Vec<Bind>],
        describe_only: bool,
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        tracing::debug!(sql = %command.sql, kind = ?command.kind, "duckdb");
        for query_id in &command.result_scans {
            self.materialize_result(query_id)?;
        }
        let id = query_id.to_owned();
        if !describe_only {
            let begin = command.sql == "BEGIN TRANSACTION";
            match command.kind {
                StatementKind::Transaction => {
                    // Snowflake ignores BEGIN inside a transaction and COMMIT/ROLLBACK outside one.
                    if begin != self.in_transaction {
                        if begin {
                            self.conn.execute_batch(&command.sql)?;
                            self.in_transaction = true;
                        } else {
                            self.end_transaction(&command.sql)?;
                        }
                    }
                    return Ok(QueryResult::status(id, "Statement executed successfully.", command.kind));
                }
                // DDL commits the open transaction, then runs on its own.
                StatementKind::Ddl if self.in_transaction => self.end_transaction("COMMIT")?,
                // With AUTOCOMMIT off, the first query or DML opens a transaction.
                kind if (kind == StatementKind::Query || kind.is_dml())
                    && !self.in_transaction
                    && !self.autocommit() =>
                {
                    self.conn.execute_batch("BEGIN TRANSACTION")?;
                    self.in_transaction = true;
                }
                _ => {}
            }
        }
        match command.kind {
            StatementKind::Query | StatementKind::Show | StatementKind::Other => {
                for pre in &command.pre {
                    self.conn.execute_batch(pre)?;
                }
                let params = bindings.first().map(Vec::as_slice).unwrap_or_default();
                self.run_query(&command.sql, params, describe_only, query_id, command.kind)
            }
            StatementKind::Merge if !command.merge_counts.is_empty() => {
                if describe_only {
                    let zeros: Vec<_> = command.merge_counts.iter().map(|(column, _)| (*column, 0)).collect();
                    return Ok(QueryResult::row_counts(id, &zeros, StatementKind::Merge));
                }
                for pre in &command.pre {
                    self.conn.execute_batch(pre)?;
                }
                let sql = typed_placeholders(&command.sql, bindings.first().map(Vec::as_slice).unwrap_or_default());
                let mut stmt = self.conn.prepare(&sql)?;
                let mut actions: HashMap<String, i64> = HashMap::new();
                let rows: Vec<Vec<Bind>> = if bindings.is_empty() { vec![vec![]] } else { bindings.to_vec() };
                for row in &rows {
                    let mut result = stmt.query(duckdb::params_from_iter(row.iter().map(Bind::to_value)))?;
                    while let Some(r) = result.next()? {
                        *actions.entry(r.get::<_, String>(0)?).or_default() += 1;
                    }
                }
                let counts: Vec<_> = command
                    .merge_counts
                    .iter()
                    .map(|(column, action)| (*column, actions.get(*action).copied().unwrap_or(0)))
                    .collect();
                Ok(QueryResult::row_counts(id, &counts, StatementKind::Merge))
            }
            kind if kind.is_dml() => {
                if describe_only {
                    return Ok(QueryResult::row_counts(id, &dml_counts(kind, 0), kind));
                }
                for pre in &command.pre {
                    self.conn.execute_batch(pre)?;
                }
                let sql = match &command.insert {
                    Some(parts) => self.convert_insert(parts, &command.sql, bindings.first())?,
                    None => command.sql.clone(),
                };
                let sql = typed_placeholders(&sql, bindings.first().map(Vec::as_slice).unwrap_or_default());
                let mut stmt = self.conn.prepare(&sql)?;
                let mut total = 0;
                if bindings.is_empty() {
                    total += stmt.execute([])?;
                } else {
                    for row in bindings {
                        total += stmt.execute(duckdb::params_from_iter(row.iter().map(Bind::to_value)))?;
                    }
                }
                Ok(QueryResult::row_counts(id, &dml_counts(kind, total as i64), kind))
            }
            kind => {
                if !describe_only {
                    let statements: Vec<&str> =
                        command.pre.iter().chain([&command.sql]).chain(&command.post).map(String::as_str).collect();
                    self.conn.execute_batch(&statements.join(";\n"))?;
                }
                let message = command.status.as_deref().unwrap_or("Statement executed successfully.");
                Ok(QueryResult::status(id, message, kind))
            }
        }
    }

    fn run_query(
        &self,
        sql: &str,
        params: &[Bind],
        describe_only: bool,
        query_id: &str,
        kind: StatementKind,
    ) -> Result<QueryResult, EngineError> {
        let values: Vec<Value> = params.iter().map(Bind::to_value).collect();
        let typed = typed_placeholders(sql, params);
        let sql = typed.as_str();
        let mut stmt = self.conn.prepare(sql)?;

        // duckdb-rs can't decode VARIANT, and nested types have no Snowflake
        // equivalent, so those columns are re-selected as JSON text.
        let mut semi = HashMap::new();
        let mut casts = HashMap::new();
        for i in 0..stmt.column_count() {
            let logical = stmt.column_logical_type(i);
            match logical.id() {
                LogicalTypeId::Variant => {
                    semi.insert(i, SfType::Variant);
                    casts.insert(i, "JSON");
                }
                LogicalTypeId::List | LogicalTypeId::Array => {
                    semi.insert(i, SfType::Array);
                    casts.insert(i, "JSON");
                }
                LogicalTypeId::Struct | LogicalTypeId::Map => {
                    semi.insert(i, SfType::Object);
                    casts.insert(i, "JSON");
                }
                // OBJECT values are stored as JSON (see localflake_sql's type mapping).
                LogicalTypeId::Varchar if logical.get_alias().as_deref() == Some("JSON") => {
                    semi.insert(i, SfType::Object);
                }
                LogicalTypeId::Uuid
                | LogicalTypeId::Interval
                | LogicalTypeId::Union
                | LogicalTypeId::Bit
                | LogicalTypeId::TimeTZ
                | LogicalTypeId::Bignum
                | LogicalTypeId::Geometry
                | LogicalTypeId::UHugeint
                | LogicalTypeId::TimeNs
                | LogicalTypeId::Unsupported => {
                    casts.insert(i, "VARCHAR");
                }
                _ => {}
            }
        }

        let mut sql = sql.to_owned();
        let mut rewritten = false;
        if !casts.is_empty() {
            let names = self.column_names(&sql, &values, stmt.column_count())?;
            let projection: Vec<String> = names
                .iter()
                .enumerate()
                .map(|(i, name)| match casts.get(&i) {
                    Some(target) => format!("CAST(__c{i} AS {target}) AS {}", ident(name)),
                    None => format!("__c{i} AS {}", ident(name)),
                })
                .collect();
            let aliases: Vec<String> = (0..names.len()).map(|i| format!("__c{i}")).collect();
            sql = format!("SELECT {} FROM ({sql}) AS __lf({})", projection.join(", "), aliases.join(", "));
            rewritten = true;
        }
        if describe_only {
            sql = format!("SELECT * FROM ({sql}) AS __lf_describe LIMIT 0");
            rewritten = true;
        }
        if rewritten {
            drop(stmt);
            stmt = self.conn.prepare(&sql)?;
        }
        let arrow = stmt.query_arrow(duckdb::params_from_iter(values))?;
        let schema = arrow.get_schema();
        let batches: Vec<_> = arrow.collect();
        Ok(QueryResult::from_arrow(query_id.to_owned(), &schema, batches, &semi, kind))
    }

    /// Applies Snowflake's conversions to inserted values where DuckDB's
    /// differ: text going into TIMESTAMP_LTZ columns is parsed like
    /// `::TIMESTAMP_LTZ` (DuckDB reads `... 03:04:05 +00:00` as a zone name).
    fn convert_insert(
        &self,
        parts: &localflake_sql::InsertParts,
        sql: &str,
        params: Option<&Vec<Bind>>,
    ) -> Result<String, EngineError> {
        let describe = |query: &str, params: Option<&Vec<Bind>>| -> Result<Vec<(String, String)>, EngineError> {
            let values: Vec<Value> = params.map(|p| p.iter().map(Bind::to_value).collect()).unwrap_or_default();
            let query = typed_placeholders(query, params.map(Vec::as_slice).unwrap_or_default());
            let mut stmt = self.conn.prepare(&format!("DESCRIBE {query}"))?;
            let rows = stmt.query_map(duckdb::params_from_iter(values), |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        let table = describe(&parts.table, None)?;
        let unquote = |name: &str| name.trim_matches('"').replace("\"\"", "\"");
        let targets: Vec<(String, String)> = if parts.columns.is_empty() {
            table.clone()
        } else {
            parts
                .columns
                .iter()
                .map(|c| {
                    let name = unquote(c);
                    let ty = table.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)).map(|(_, t)| t.clone());
                    (name, ty.unwrap_or_default())
                })
                .collect()
        };
        if !targets.iter().any(|(_, ty)| ty == "TIMESTAMP WITH TIME ZONE") {
            return Ok(sql.to_owned());
        }
        let source = describe(&parts.source, params)?;
        if source.len() != targets.len() {
            return Ok(sql.to_owned());
        }
        let aliases: Vec<String> = (0..targets.len()).map(|i| format!("__lf_c{i}")).collect();
        let values: Vec<String> = targets
            .iter()
            .zip(&source)
            .zip(&aliases)
            .map(|(((_, target), (_, source)), alias)| {
                if target == "TIMESTAMP WITH TIME ZONE" && source == "VARCHAR" {
                    format!("sf_ltz({alias})")
                } else {
                    alias.clone()
                }
            })
            .collect();
        let columns: Vec<String> = targets.iter().map(|(n, _)| ident(n)).collect();
        Ok(format!(
            "INSERT INTO {} ({}) SELECT {} FROM ({}) AS __lf_src({})",
            parts.table,
            columns.join(", "),
            values.join(", "),
            parts.source,
            aliases.join(", ")
        ))
    }

    /// Result column names without executing the query.
    fn column_names(&self, sql: &str, values: &[Value], count: usize) -> Result<Vec<String>, EngineError> {
        let mut describe = self.conn.prepare(&format!("DESCRIBE {sql}"))?;
        let names: Vec<String> = describe
            .query_map(duckdb::params_from_iter(values.iter()), |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        if names.len() == count { Ok(names) } else { Ok((1..=count).map(|i| format!("C{i}")).collect()) }
    }

    /// Makes a stored result readable as `temp.main."__lf_result_<id>"` for `RESULT_SCAN`.
    fn materialize_result(&self, query_id: &str) -> Result<(), EngineError> {
        let table = localflake_sql::result_table(query_id);
        let exists: i64 = self.conn.query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE database_name = 'temp' AND table_name = ?",
            [&table],
            |r| r.get(0),
        )?;
        if exists > 0 {
            return Ok(());
        }
        let result = self.engine.result(query_id).ok_or_else(|| EngineError::unknown_query(query_id))??;
        let fields = result.batches.first().map(|b| b.schema().fields().to_vec());
        let columns: Vec<String> = result
            .columns
            .iter()
            .enumerate()
            .map(|(i, column)| {
                let duck_type = match (&fields, column.sf_type.is_semi_structured()) {
                    (_, true) => "JSON".to_owned(),
                    (Some(fields), false) => duckdb_type(fields[i].data_type()).to_owned(),
                    (None, false) => "VARCHAR".to_owned(),
                };
                format!("{} {duck_type}", ident(&localflake_sql::encode_identifier(&column.name)))
            })
            .collect();
        self.conn.execute_batch(&format!("CREATE TEMP TABLE {} ({})", ident(&table), columns.join(", ")))?;
        if !result.batches.is_empty() {
            let mut appender = self.conn.appender_to_db(&table, "main")?;
            for batch in &result.batches {
                appender.append_record_batch(batch.clone())?;
            }
            appender.flush()?;
        }
        Ok(())
    }

    /// Evaluates a `SET` value into a DuckDB literal expression.
    fn evaluate_literal(&self, expr: &str) -> Result<String, EngineError> {
        let (text, type_name): (Option<String>, String) = self.conn.query_row(
            &format!(
                "SELECT CASE WHEN typeof(__v) = 'VARIANT' THEN CAST(CAST(__v AS JSON) AS VARCHAR) ELSE CAST(__v AS VARCHAR) END, \
                 typeof(__v) FROM (SELECT ({expr}) AS __v)"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(match (text, type_name.as_str()) {
            (None, _) => "NULL".to_owned(),
            (Some(json), "VARIANT") => format!("CAST(CAST({} AS JSON) AS VARIANT)", literal(&json)),
            (Some(text), type_name) => format!("CAST({} AS {type_name})", literal(&text)),
        })
    }

    fn set_parameters(&mut self, parameters: Vec<(String, String)>) -> Result<(), EngineError> {
        for (key, value) in parameters {
            let key = key.to_uppercase();
            if key == "TIMEZONE" && self.engine.inner.icu {
                self.conn
                    .execute_batch(&format!("SET TimeZone = {}", literal(&value)))
                    .map_err(|_| EngineError::invalid_parameter("TIMEZONE", &value))?;
            }
            if key == "TIMESTAMP_TYPE_MAPPING" {
                self.ctx.timestamp_type_mapping = match value.to_uppercase().as_str() {
                    "TIMESTAMP_LTZ" => TimestampMapping::Ltz,
                    "TIMESTAMP_TZ" => TimestampMapping::Tz,
                    _ => TimestampMapping::Ntz,
                };
            }
            self.ctx.parameters.insert(key, value);
        }
        Ok(())
    }

    fn apply_use(&mut self, target: UseTarget) -> Result<(), EngineError> {
        match target {
            UseTarget::Database(database) => {
                if !self.database_exists(&database)? {
                    return Err(EngineError::does_not_exist("Database", &database));
                }
                self.use_database(&database, None)
            }
            UseTarget::Schema { database, schema } => {
                let database =
                    database.or_else(|| self.ctx.database.clone()).ok_or_else(EngineError::no_current_database)?;
                if !self.database_exists(&database)? {
                    return Err(EngineError::does_not_exist("Database", &database));
                }
                if !self.schema_exists(&database, &schema)? {
                    return Err(EngineError::does_not_exist("Schema", &format!("{database}.{schema}")));
                }
                self.use_database(&database, Some(&schema))
            }
            UseTarget::Warehouse(name) => {
                self.ctx.warehouse = Some(name);
                Ok(())
            }
            UseTarget::Role(name) => {
                self.ctx.role = Some(name);
                Ok(())
            }
            UseTarget::SecondaryRoles => Ok(()),
        }
    }

    /// Switches database (and schema). Without a schema, PUBLIC is used if it exists.
    fn use_database(&mut self, database: &str, schema: Option<&str>) -> Result<(), EngineError> {
        let schema = match schema {
            Some(s) => Some(s.to_owned()),
            None if self.schema_exists(database, DEFAULT_SCHEMA)? => Some(DEFAULT_SCHEMA.to_owned()),
            None => None,
        };
        match &schema {
            Some(s) => self.conn.execute_batch(&format!("USE {}.{};", ident(database), ident(s)))?,
            None => self.conn.execute_batch(&format!("USE {};", ident(database)))?,
        }
        self.ctx.database = Some(database.to_owned());
        self.ctx.schema = schema;
        Ok(())
    }

    fn database_exists(&self, name: &str) -> Result<bool, EngineError> {
        let count: i64 =
            self.conn
                .query_row("SELECT count(*) FROM duckdb_databases() WHERE database_name = ?", [name], |r| r.get(0))?;
        Ok(count > 0)
    }

    fn schema_exists(&self, database: &str, schema: &str) -> Result<bool, EngineError> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM duckdb_schemas() WHERE database_name = ? AND schema_name = ?",
            [database, schema],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    /// Creates a database with a PUBLIC schema, optionally copying another database.
    fn create_database(&mut self, name: &str, clone: Option<&str>) -> Result<(), EngineError> {
        if let Some(source) = clone
            && !self.database_exists(source)?
        {
            return Err(EngineError::does_not_exist("Database", source));
        }
        let location = match &self.engine.inner.data_dir {
            Some(dir) => dir.join(format!("{name}.duckdb")).to_string_lossy().into_owned(),
            None => ":memory:".to_owned(),
        };
        self.conn.execute_batch(&format!("ATTACH {} AS {};", literal(&location), ident(name)))?;
        match clone {
            Some(source) => {
                self.conn.execute_batch(&format!("COPY FROM DATABASE {} TO {};", ident(source), ident(name)))?
            }
            None => self.conn.execute_batch(&format!(
                "CREATE SCHEMA IF NOT EXISTS {}.{};",
                ident(name),
                ident(DEFAULT_SCHEMA)
            ))?,
        }
        Ok(())
    }

    fn drop_database(&mut self, name: &str) -> Result<(), EngineError> {
        if self.ctx.database.as_deref() == Some(name) {
            // DuckDB refuses to detach the connection's current catalog.
            self.conn.execute_batch("USE memory;")?;
            self.ctx.database = None;
            self.ctx.schema = None;
        }
        self.conn.execute_batch(&format!("DETACH {};", ident(name)))?;
        self.engine.inner.stages.drop_database(name);
        if let Some(dir) = &self.engine.inner.data_dir {
            for suffix in ["duckdb", "duckdb.wal"] {
                let _ = std::fs::remove_file(dir.join(format!("{name}.{suffix}")));
            }
        }
        Ok(())
    }
}

fn dml_counts(kind: StatementKind, n: i64) -> Vec<(&'static str, i64)> {
    match kind {
        StatementKind::Update => vec![("number of rows updated", n), ("number of multi-joined rows updated", 0)],
        StatementKind::Delete => vec![("number of rows deleted", n)],
        _ => vec![("number of rows inserted", n)],
    }
}

/// The DuckDB type for storing an Arrow column (used to materialize results).
fn duckdb_type(data_type: &DataType) -> String {
    match data_type {
        DataType::Boolean => "BOOLEAN".into(),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => "BIGINT".into(),
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 => "BIGINT".into(),
        DataType::UInt64 => "UBIGINT".into(),
        DataType::Float16 | DataType::Float32 | DataType::Float64 => "DOUBLE".into(),
        DataType::Decimal128(p, s) => format!("DECIMAL({p},{s})"),
        DataType::Date32 => "DATE".into(),
        DataType::Time64(_) => "TIME".into(),
        DataType::Timestamp(_, Some(_)) => "TIMESTAMPTZ".into(),
        DataType::Timestamp(_, None) => "TIMESTAMP".into(),
        DataType::Binary | DataType::LargeBinary => "BLOB".into(),
        _ => "VARCHAR".into(),
    }
}

fn new_query_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests;
