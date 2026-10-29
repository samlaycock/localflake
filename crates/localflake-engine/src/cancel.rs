//! Cancelling running statements: driver abort requests, `SYSTEM$CANCEL_QUERY`
//! and `STATEMENT_TIMEOUT_IN_SECONDS`.
//!
//! A DuckDB interrupt only stops the query running at that moment, and DuckDB
//! clears the flag when the next autocommit transaction starts. A statement
//! runs several DuckDB queries (type probes, stage reads, the statement
//! itself), so an interrupt landing between two of them would be lost. Instead,
//! a watchdog thread interrupts each cancelled statement's connection every few
//! milliseconds until the statement ends.

use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use duckdb::InterruptHandle;
use duckdb::arrow::array::{Array, ArrayRef, AsArray, StringArray};
use duckdb::arrow::datatypes::{DataType, Int64Type};
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};

use crate::EngineError;

/// How often the watchdog interrupts cancelled statements and checks deadlines.
const TICK: Duration = Duration::from_millis(10);
/// How long the watchdog sleeps when nothing is running, so it notices the engine closing.
const IDLE: Duration = Duration::from_millis(100);

const RUNNING: u8 = 0;
const CANCELED: u8 = 1;
const TIMED_OUT: u8 = 2;

/// Cancels a request from another thread. Each statement of the request
/// checks it before running, and a running statement is interrupted.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicU8>);

impl CancelToken {
    pub fn cancel(&self) {
        let _ = self.0.compare_exchange(RUNNING, CANCELED, Ordering::Relaxed, Ordering::Relaxed);
    }

    fn time_out(&self) {
        let _ = self.0.compare_exchange(RUNNING, TIMED_OUT, Ordering::Relaxed, Ordering::Relaxed);
    }

    fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed) != RUNNING
    }

    /// The error a stopped statement fails with, if the request was stopped.
    pub(crate) fn error(&self, timeout_seconds: u64) -> Option<EngineError> {
        match self.0.load(Ordering::Relaxed) {
            CANCELED => Some(EngineError::canceled()),
            TIMED_OUT => Some(EngineError::timed_out(timeout_seconds)),
            _ => None,
        }
    }
}

struct Statement {
    session_id: u64,
    token: CancelToken,
    interrupt: Arc<InterruptHandle>,
    deadline: Option<Instant>,
}

/// The statements running across all sessions, by query ID.
#[derive(Default)]
pub(crate) struct Running {
    statements: Mutex<HashMap<String, Statement>>,
    wake: Condvar,
}

impl Running {
    /// Starts the watchdog, which stops once the registry is dropped.
    pub(crate) fn open() -> Arc<Self> {
        let running = Arc::new(Self::default());
        let weak = Arc::downgrade(&running);
        std::thread::Builder::new()
            .name("localflake-cancel".into())
            .spawn(move || watchdog(weak))
            .expect("cannot start the cancellation watchdog");
        running
    }

    /// Registers a statement until the returned guard drops.
    pub(crate) fn start(
        self: &Arc<Self>,
        query_id: &str,
        session_id: u64,
        token: &CancelToken,
        interrupt: Arc<InterruptHandle>,
        timeout: Option<Duration>,
    ) -> RunningGuard {
        let deadline = timeout.map(|t| Instant::now() + t);
        let statement = Statement { session_id, token: token.clone(), interrupt, deadline };
        self.statements.lock().unwrap().insert(query_id.to_owned(), statement);
        self.wake.notify_one();
        RunningGuard { running: self.clone(), query_id: query_id.to_owned() }
    }

    /// Cancels a running statement, returning whether it was running.
    pub(crate) fn cancel_query(&self, query_id: &str) -> bool {
        let statements = self.statements.lock().unwrap();
        let Some(statement) = statements.get(query_id) else { return false };
        statement.token.cancel();
        statement.interrupt.interrupt();
        true
    }

    /// Cancels every running statement of a session, returning how many there were.
    pub(crate) fn cancel_session(&self, session_id: u64) -> usize {
        let statements = self.statements.lock().unwrap();
        let mut cancelled = 0;
        for statement in statements.values().filter(|s| s.session_id == session_id) {
            statement.token.cancel();
            statement.interrupt.interrupt();
            cancelled += 1;
        }
        cancelled
    }

    #[cfg(test)]
    pub(crate) fn query_ids(&self) -> Vec<String> {
        self.statements.lock().unwrap().keys().cloned().collect()
    }
}

/// Deregisters a statement. Once it drops, the watchdog no longer interrupts the connection.
pub(crate) struct RunningGuard {
    running: Arc<Running>,
    query_id: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.running.statements.lock().unwrap().remove(&self.query_id);
    }
}

fn watchdog(running: Weak<Running>) {
    while let Some(running) = running.upgrade() {
        let statements = running.statements.lock().unwrap();
        let now = Instant::now();
        for statement in statements.values() {
            if statement.deadline.is_some_and(|d| d <= now) {
                statement.token.time_out();
            }
            if statement.token.is_stopped() {
                statement.interrupt.interrupt();
            }
        }
        let wait = if statements.is_empty() { IDLE } else { TICK };
        drop(running.wake.wait_timeout(statements, wait).unwrap());
    }
}

/// `sf_cancel_all_queries(session_id, caller_session_id, nonce)`:
/// `SYSTEM$CANCEL_ALL_QUERIES`. The caller's own statement is the one running
/// it, so cancelling the caller's session cancels nothing.
pub(crate) struct CancelAllQueries;

impl VArrowScalar for CancelAllQueries {
    type State = Arc<Running>;

    fn invoke(running: &Arc<Running>, input: duckdb::arrow::array::RecordBatch) -> Result<ArrayRef, Box<dyn Error>> {
        let sessions = input.column(0).as_primitive::<Int64Type>();
        let callers = input.column(1).as_primitive::<Int64Type>();
        let out: StringArray = (0..input.num_rows())
            .map(|i| {
                (!sessions.is_null(i)).then(|| {
                    let session = sessions.value(i);
                    let cancelled =
                        if session == callers.value(i) { 0 } else { running.cancel_session(session as u64) };
                    format!("{cancelled} cancelled.")
                })
            })
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Int64, DataType::Int64, DataType::Float64], DataType::Utf8)]
    }
}

/// `sf_cancel_query(query_id, nonce)`: `SYSTEM$CANCEL_QUERY`. Arrow scalars
/// can't be marked volatile, so the translation passes a volatile `nonce`
/// to stop DuckDB from folding the call (and cancelling) when it only prepares
/// the statement.
pub(crate) struct CancelQuery;

impl VArrowScalar for CancelQuery {
    type State = Arc<Running>;

    fn invoke(running: &Arc<Running>, input: duckdb::arrow::array::RecordBatch) -> Result<ArrayRef, Box<dyn Error>> {
        let ids = input.column(0).as_string::<i32>();
        let out: StringArray = ids
            .iter()
            .map(|id| {
                id.map(|id| {
                    if running.cancel_query(id) {
                        format!("query [{id}] terminated.")
                    } else {
                        "Identified SQL statement is not currently executing.".to_owned()
                    }
                })
            })
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Float64], DataType::Utf8)]
    }
}
