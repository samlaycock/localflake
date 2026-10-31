//! A record of the statements every session runs (drivers and the web UI
//! alike), newest last, with the DuckDB SQL each was translated to. This is
//! for debugging: the web UI shows it as the query history.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use localflake_sql::StatementKind;

use crate::EngineError;

/// Statements kept in the history.
const CAPACITY: usize = 2000;
/// Most memory the history's SQL text may take: the oldest statements are
/// forgotten sooner when statements are big (bulk inserts with literal rows).
const MAX_BYTES: usize = 64 * 1024 * 1024;
/// Longest SQL text kept per statement, in bytes.
const MAX_SQL_LENGTH: usize = 64 * 1024;
/// Most DuckDB SQL kept per statement, in bytes (scripts and procedures run many statements).
const MAX_DUCKDB_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone)]
pub struct QueryRecord {
    pub query_id: String,
    /// Increases whenever the record changes, so clients can poll for changes.
    pub sequence: u64,
    pub session_id: u64,
    pub user: String,
    /// The client that opened the session, e.g. the driver's application ID.
    pub client: String,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub query_tag: Option<String>,
    /// The session's TIMEZONE parameter, which TIMESTAMP_LTZ results display in.
    pub timezone: Option<String>,
    /// The statement as the client sent it.
    pub sql: String,
    pub status: QueryStatus,
    pub kind: Option<StatementKind>,
    /// Rows returned, or for DML the rows changed.
    pub rows: Option<usize>,
    pub error: Option<EngineError>,
    /// Milliseconds since the Unix epoch.
    pub started_at: u64,
    pub duration_micros: Option<u64>,
    /// The DuckDB SQL the statement ran, in order. Only filled in by
    /// [`History::get`]: pages of the history leave it empty.
    pub duckdb_sql: Vec<String>,
}

/// What the session knows about a statement when it starts.
pub(crate) struct Started<'a> {
    pub query_id: &'a str,
    pub session_id: u64,
    pub user: &'a str,
    pub client: &'a str,
    pub database: Option<&'a str>,
    pub schema: Option<&'a str>,
    pub query_tag: Option<&'a str>,
    pub timezone: Option<&'a str>,
    pub sql: &'a str,
}

#[derive(Default)]
pub(crate) struct History {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    /// The SQL text the entries hold, in bytes.
    bytes: usize,
    sequence: u64,
    /// Bumped when the history is cleared, so clients polling for changes start over.
    generation: u64,
}

struct Entry {
    record: QueryRecord,
    duckdb_sql: Vec<String>,
    /// When a running statement started.
    started: Option<Instant>,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.record.sql.len() + self.duckdb_sql.iter().map(String::len).sum::<usize>()
    }
}

/// A page of the history: records changed after a sequence number.
#[derive(Debug, Clone)]
pub struct HistoryPage {
    pub records: Vec<QueryRecord>,
    /// The latest sequence number; pass it back to get later changes.
    pub sequence: u64,
    pub generation: u64,
}

impl History {
    pub(crate) fn start(&self, started: Started) {
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        let record = QueryRecord {
            query_id: started.query_id.to_owned(),
            sequence: state.sequence,
            session_id: started.session_id,
            user: started.user.to_owned(),
            client: started.client.to_owned(),
            database: started.database.map(str::to_owned),
            schema: started.schema.map(str::to_owned),
            query_tag: started.query_tag.filter(|t| !t.is_empty()).map(str::to_owned),
            timezone: started.timezone.map(str::to_owned),
            sql: truncate(started.sql, MAX_SQL_LENGTH),
            status: QueryStatus::Running,
            kind: None,
            rows: None,
            error: None,
            started_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            duration_micros: None,
            duckdb_sql: Vec::new(),
        };
        let entry = Entry { record, duckdb_sql: Vec::new(), started: Some(Instant::now()) };
        state.bytes += entry.bytes();
        state.entries.push_back(entry);
        state.evict();
    }

    pub(crate) fn finish(
        &self,
        query_id: &str,
        outcome: Result<(StatementKind, usize), &EngineError>,
        duckdb_sql: Vec<String>,
    ) {
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        let sequence = state.sequence;
        let Some(entry) = state.entries.iter_mut().rev().find(|e| e.record.query_id == query_id) else {
            return;
        };
        let added = duckdb_sql.iter().map(String::len).sum::<usize>();
        entry.duckdb_sql = duckdb_sql;
        let record = &mut entry.record;
        record.sequence = sequence;
        record.duration_micros = entry.started.take().map(|s| s.elapsed().as_micros() as u64);
        match outcome {
            Ok((kind, rows)) => {
                record.status = QueryStatus::Succeeded;
                record.kind = Some(kind);
                record.rows = Some(rows);
            }
            Err(error) => {
                record.status = QueryStatus::Failed;
                record.error = Some(error.clone());
            }
        }
        state.bytes += added;
        state.evict();
    }

    /// Records changed after `since` (all of them for 0), oldest first.
    pub(crate) fn page(&self, since: u64) -> HistoryPage {
        let state = self.state.lock().unwrap();
        HistoryPage {
            records: state.entries.iter().filter(|e| e.record.sequence > since).map(|e| e.record.clone()).collect(),
            sequence: state.sequence,
            generation: state.generation,
        }
    }

    pub(crate) fn get(&self, query_id: &str) -> Option<QueryRecord> {
        let state = self.state.lock().unwrap();
        let entry = state.entries.iter().rev().find(|e| e.record.query_id == query_id)?;
        Some(QueryRecord { duckdb_sql: entry.duckdb_sql.clone(), ..entry.record.clone() })
    }

    /// Forgets finished statements; running ones stay so they can still finish.
    pub(crate) fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.entries.retain(|e| e.record.status == QueryStatus::Running);
        state.bytes = state.entries.iter().map(Entry::bytes).sum();
        state.generation += 1;
    }
}

impl State {
    /// Forgets the oldest statements while there are too many or they're too big.
    fn evict(&mut self) {
        while self.entries.len() > CAPACITY || (self.bytes > MAX_BYTES && self.entries.len() > 1) {
            let Some(oldest) = self.entries.pop_front() else { break };
            self.bytes -= oldest.bytes();
        }
    }
}

/// Collects the DuckDB SQL a statement runs, up to `MAX_DUCKDB_BYTES`.
#[derive(Default)]
pub(crate) struct DuckdbLog(Option<(Vec<String>, usize)>);

impl DuckdbLog {
    pub(crate) fn start(&mut self) {
        self.0 = Some((Vec::new(), 0));
    }

    pub(crate) fn push(&mut self, sql: &str) {
        if let Some((log, bytes)) = &mut self.0
            && *bytes < MAX_DUCKDB_BYTES
        {
            let sql = truncate(sql, (MAX_DUCKDB_BYTES - *bytes).min(MAX_SQL_LENGTH));
            *bytes += sql.len();
            log.push(sql);
        }
    }

    pub(crate) fn take(&mut self) -> Vec<String> {
        self.0.take().map(|(log, _)| log).unwrap_or_default()
    }
}

fn truncate(sql: &str, limit: usize) -> String {
    if sql.len() <= limit {
        return sql.to_owned();
    }
    let end = sql.floor_char_boundary(limit);
    format!("{}… [{} more bytes]", &sql[..end], sql.len() - end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(query_id: &str) -> Started<'_> {
        Started {
            query_id,
            session_id: 1,
            user: "U",
            client: "test",
            database: Some("DB"),
            schema: None,
            query_tag: Some(""),
            timezone: None,
            sql: "select 1",
        }
    }

    #[test]
    fn pages_changes_since_a_sequence() {
        let history = History::default();
        history.start(started("a"));
        history.start(started("b"));
        let first = history.page(0);
        assert_eq!(first.records.len(), 2);
        assert_eq!(first.records[0].status, QueryStatus::Running);
        assert_eq!(first.records[0].query_tag, None);

        history.finish("a", Ok((StatementKind::Query, 3)), vec!["SELECT 1".into()]);
        let next = history.page(first.sequence);
        assert_eq!(next.records.len(), 1);
        let a = &next.records[0];
        assert_eq!((a.query_id.as_str(), a.status, a.rows), ("a", QueryStatus::Succeeded, Some(3)));
        assert!(a.duration_micros.is_some());
        assert!(a.duckdb_sql.is_empty(), "pages leave out the DuckDB SQL");
        assert_eq!(history.get("a").unwrap().duckdb_sql, vec!["SELECT 1"]);

        history.clear();
        let cleared = history.page(0);
        assert_eq!(cleared.records.iter().map(|r| r.query_id.as_str()).collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(cleared.generation, first.generation + 1);
    }

    #[test]
    fn truncates_long_sql_on_a_char_boundary() {
        let long = "é".repeat(MAX_SQL_LENGTH);
        let kept = truncate(&long, MAX_SQL_LENGTH);
        assert!(kept.len() < long.len());
        assert!(kept.ends_with("more bytes]"));
    }

    #[test]
    fn forgets_the_oldest_statements_beyond_the_memory_budget() {
        let history = History::default();
        let big = "x".repeat(MAX_SQL_LENGTH);
        let fits = MAX_BYTES / MAX_SQL_LENGTH;
        for i in 0..fits + 10 {
            let id = i.to_string();
            history.start(Started { sql: &big, ..started(&id) });
        }
        let page = history.page(0);
        assert_eq!(page.records.len(), fits);
        assert_eq!(page.records[0].query_id, "10");
    }
}
