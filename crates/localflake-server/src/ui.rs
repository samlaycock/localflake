//! The web UI: SQL worksheets, an object explorer, the query history and the
//! connected sessions, served at `/__localflake/ui/`, and the JSON API behind
//! it under `/__localflake/api/`.
//!
//! The page is static and embedded in the binary, so there's no build step
//! and localflake still never needs the network. Like the driver protocol,
//! the API has no authentication, but it refuses cross-origin requests, so
//! other websites open in the same browser can't use it.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request as HttpRequest, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use localflake_engine::{
    CancelToken, Engine, EngineError, QueryRecord, QueryResult, QueryStatus, Request, Session, SessionOptions,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::protocol::rowtype;
use crate::routes::{AppState, ClientSession, RequestGuard, SessionEntry, internal, open_session};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const APP_CSS: &str = include_str!("../ui/app.css");
const FAVICON: &str = include_str!("../ui/favicon.svg");

/// The user web UI sessions log in as, unless the page asks for another.
const DEFAULT_USER: &str = "WEB_UI";
/// Rows sent with a query's results; the page fetches more as needed.
const DEFAULT_ROWS: usize = 1000;
/// Most rows sent in one response (downloads page through bigger results).
const MAX_ROWS: usize = 100_000;
/// Longest SQL text sent in the history list; the detail view has all of it.
const HISTORY_SQL_PREVIEW: usize = 4096;

pub fn router() -> Router<AppState> {
    let api = Router::new()
        .route("/info", get(info))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", delete(close_session))
        .route("/query", post(run_query))
        .route("/cancel", post(cancel_request))
        .route("/split", post(split))
        .route("/results/{id}", get(result_page))
        .route("/history", get(history).delete(clear_history))
        .route("/history/{id}", get(history_record))
        .route("/queries/{id}/cancel", post(cancel_query))
        .route("/catalog", get(databases))
        .route("/catalog/{database}", get(database_catalog))
        .route("/stage-files", put(upload_stage_file).get(download_stage_file).layer(DefaultBodyLimit::disable()))
        .layer(middleware::from_fn(same_origin));
    Router::new()
        .route("/", get(|| async { Redirect::to("/__localflake/ui/") }))
        .route("/__localflake/ui", get(|| async { Redirect::permanent("/__localflake/ui/") }))
        .route("/__localflake/ui/", get(|| async { asset("index.html") }))
        .route("/__localflake/ui/{name}", get(|Path(name): Path<String>| async move { asset(&name) }))
        .nest("/__localflake/api", api)
}

/// An embedded file of the page. For working on the UI, `LOCALFLAKE_UI_DIR`
/// serves them from a directory instead, so changes show without a rebuild.
fn asset(name: &str) -> Response {
    let (embedded, content_type) = match name {
        "index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        "app.js" => (APP_JS, "text/javascript; charset=utf-8"),
        "app.css" => (APP_CSS, "text/css; charset=utf-8"),
        "favicon.svg" => (FAVICON, "image/svg+xml"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let body = std::env::var_os("LOCALFLAKE_UI_DIR")
        .and_then(|dir| std::fs::read_to_string(std::path::Path::new(&dir).join(name)).ok())
        .unwrap_or_else(|| embedded.to_owned());
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// Refuses requests from other origins: a page on another site could
/// otherwise run SQL here through the browser of someone visiting it.
async fn same_origin(request: HttpRequest, next: Next) -> Response {
    let headers = request.headers();
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    let cross_origin = headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .is_some_and(|origin| origin.split_once("://").map(|(_, authority)| authority) != host);
    let cross_site = headers.get("sec-fetch-site").is_some_and(|v| v != "same-origin" && v != "none");
    if cross_origin || cross_site {
        return api_error(StatusCode::FORBIDDEN, "cross-origin requests are not allowed");
    }
    next.run(request).await
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": { "message": message.into() } }))).into_response()
}

fn session_expired() -> Response {
    let error = json!({ "code": "390111", "message": "The worksheet's session has ended. Run the query again." });
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": error }))).into_response()
}

fn error_json(error: &EngineError, query_id: &str) -> Value {
    json!({ "code": error.code, "sqlState": error.sql_state, "message": error.message, "queryId": query_id })
}

/// Runs blocking engine work off the async runtime.
async fn blocking(work: impl FnOnce() -> Response + Send + 'static) -> Response {
    tokio::task::spawn_blocking(work).await.unwrap_or_else(internal)
}

async fn info(State(state): State<AppState>) -> Response {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "dataDir": state.engine.data_dir().map(|d| d.display().to_string()),
        "startedAt": state.started_at,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewSession {
    user: Option<String>,
    database: Option<String>,
    schema: Option<String>,
}

/// `POST /sessions`: starts a session for a worksheet.
async fn create_session(State(state): State<AppState>, Json(body): Json<NewSession>) -> Response {
    let non_empty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    let options = SessionOptions {
        account: "localflake".into(),
        user: non_empty(body.user).unwrap_or_else(|| DEFAULT_USER.into()),
        database: non_empty(body.database),
        schema: non_empty(body.schema),
        client: "localflake web UI".into(),
        ..Default::default()
    };
    match open_session(&state, options, true).await {
        Ok((token, entry)) => {
            let client = entry.client.lock().unwrap();
            Json(json!({ "session": token, "context": context_json(&client) })).into_response()
        }
        Err(response) => response,
    }
}

fn context_json(client: &ClientSession) -> Value {
    let ctx = client.session.context();
    json!({
        "sessionId": ctx.session_id,
        "user": ctx.user,
        "role": ctx.role,
        "warehouse": ctx.warehouse,
        "database": ctx.database,
        "schema": ctx.schema,
        "timezone": ctx.parameters.get("TIMEZONE"),
    })
}

async fn list_sessions(State(state): State<AppState>) -> Response {
    let mut sessions: Vec<Value> =
        state.sessions.lock().unwrap().values().map(|entry| json!(*entry.summary.lock().unwrap())).collect();
    sessions.sort_by_key(|s| s["id"].as_u64());
    Json(json!({ "sessions": sessions })).into_response()
}

/// `DELETE /sessions/{id}`: ends a session, cancelling what it's running. Its
/// client gets Snowflake's "session no longer exists" error on its next request.
async fn close_session(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let mut sessions = state.sessions.lock().unwrap();
    let Some(token) = sessions.iter().find(|(_, e)| e.summary.lock().unwrap().id == id).map(|(t, _)| t.clone()) else {
        return api_error(StatusCode::NOT_FOUND, format!("session {id} not found"));
    };
    sessions.remove(&token);
    let cancelled = state.engine.cancel_session(id);
    tracing::info!(session = id, cancelled, "session closed from the web UI");
    Json(json!({ "cancelled": cancelled })).into_response()
}

fn entry_for(state: &AppState, token: &str) -> Option<SessionEntry> {
    state.sessions.lock().unwrap().get(token).cloned()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryBody {
    session: String,
    sql: String,
    request_id: Option<String>,
    row_limit: Option<usize>,
}

/// `POST /query`: runs SQL in a worksheet session, any number of statements.
/// Responds with the statements' results, up to the failing one if any.
async fn run_query(State(state): State<AppState>, Json(body): Json<QueryBody>) -> Response {
    let Some(entry) = entry_for(&state, &body.session) else { return session_expired() };
    let cancel = CancelToken::default();
    let guard = body.request_id.map(|id| RequestGuard::start(&state.requests, id, body.session.clone(), &cancel));
    let limit = body.row_limit.unwrap_or(DEFAULT_ROWS).min(MAX_ROWS);
    let engine = state.engine.clone();
    blocking(move || {
        let _guard = guard;
        entry.begin();
        let mut client = entry.client.lock().unwrap();
        let outcome =
            client.session.execute(&Request { sql: &body.sql, statement_count: Some(0), cancel, ..Default::default() });
        entry.end(&client);
        let (results, error) = match outcome {
            Ok(results) => (results, Value::Null),
            Err(failure) => (failure.completed, error_json(&failure.error, &failure.query_id)),
        };
        let results: Result<Vec<Value>, EngineError> =
            results.iter().map(|r| result_json(&engine, r, 0, limit)).collect();
        match results {
            Ok(results) => Json(json!({ "results": results, "error": error, "context": context_json(&client) })),
            Err(err) => Json(json!({ "results": [], "error": error_json(&err, ""), "context": context_json(&client) })),
        }
        .into_response()
    })
    .await
}

/// A page of a result: its columns, `limit` rows from `offset` in the JSON
/// wire encoding (which the page formats), and how long the statement took.
fn result_json(engine: &Engine, result: &QueryResult, offset: usize, limit: usize) -> Result<Value, EngineError> {
    let record = engine.history_record(&result.query_id);
    let mut statement = record.as_ref().map(|r| r.sql.as_str()).unwrap_or_default().to_owned();
    statement.truncate(statement.floor_char_boundary(HISTORY_SQL_PREVIEW));
    Ok(json!({
        "queryId": result.query_id,
        "kind": kind_name(result.kind),
        "statement": statement,
        "columns": result.columns.iter().map(rowtype).collect::<Vec<_>>(),
        "rows": result.slice(offset, limit).json_rows()?,
        "total": result.row_count(),
        "offset": offset,
        "durationMicros": record.as_ref().and_then(|r| r.duration_micros),
        "timezone": record.and_then(|r| r.timezone),
        "transfer": result.transfer.is_some(),
    }))
}

fn kind_name(kind: localflake_engine::StatementKind) -> String {
    format!("{kind:?}").to_lowercase()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelBody {
    session: String,
    request_id: String,
}

/// `POST /cancel`: cancels a worksheet's request in progress.
async fn cancel_request(State(state): State<AppState>, Json(body): Json<CancelBody>) -> Response {
    let token = match state.requests.lock().unwrap().get(&body.request_id) {
        Some((owner, token)) if *owner == body.session => Some(token.clone()),
        _ => None,
    };
    if let Some(token) = &token {
        token.cancel();
    }
    Json(json!({ "cancelled": token.is_some() })).into_response()
}

/// `POST /queries/{id}/cancel`: cancels a running statement from any session.
async fn cancel_query(State(state): State<AppState>, Path(query_id): Path<String>) -> Response {
    Json(json!({ "cancelled": state.engine.cancel_query(&query_id) })).into_response()
}

#[derive(Deserialize)]
struct SplitBody {
    sql: String,
}

/// `POST /split`: where each statement starts and ends, in UTF-16 code units
/// (JavaScript string offsets), so the editor can run the one at the cursor.
async fn split(Json(body): Json<SplitBody>) -> Response {
    match localflake_engine::statement_ranges(&body.sql) {
        Ok(ranges) => {
            let mut units = 0;
            let mut bytes = 0;
            let mut utf16 = |byte: usize| {
                units += body.sql[bytes..byte].encode_utf16().count();
                bytes = byte;
                units
            };
            let ranges: Vec<[usize; 2]> = ranges.into_iter().map(|r| [utf16(r.start), utf16(r.end)]).collect();
            Json(json!({ "ranges": ranges })).into_response()
        }
        Err(err) => Json(json!({ "ranges": [], "error": err.message })).into_response(),
    }
}

#[derive(Deserialize)]
struct PageParams {
    offset: Option<usize>,
    limit: Option<usize>,
}

/// `GET /results/{id}`: a page of a stored result (the most recent few hundred are kept).
async fn result_page(
    State(state): State<AppState>,
    Path(query_id): Path<String>,
    Query(page): Query<PageParams>,
) -> Response {
    let engine = state.engine.clone();
    blocking(move || match engine.result(&query_id) {
        Some(Ok(result)) => {
            let limit = page.limit.unwrap_or(DEFAULT_ROWS).min(MAX_ROWS);
            match result_json(&engine, &result, page.offset.unwrap_or(0), limit) {
                Ok(json) => Json(json).into_response(),
                Err(err) => Json(json!({ "error": error_json(&err, &query_id) })).into_response(),
            }
        }
        Some(Err(err)) => Json(json!({ "error": error_json(&err, &query_id) })).into_response(),
        None => {
            api_error(StatusCode::NOT_FOUND, "This result is no longer stored: only the most recent results are kept.")
        }
    })
    .await
}

#[derive(Deserialize)]
struct HistoryParams {
    since: Option<u64>,
}

/// `GET /history?since=n`: statements that started or finished after
/// sequence number `n`, so the page can poll for changes.
async fn history(State(state): State<AppState>, Query(params): Query<HistoryParams>) -> Response {
    let page = state.engine.history(params.since.unwrap_or(0));
    let records: Vec<Value> = page.records.iter().map(|r| record_json(r, false)).collect();
    Json(json!({ "records": records, "sequence": page.sequence, "generation": page.generation })).into_response()
}

async fn history_record(State(state): State<AppState>, Path(query_id): Path<String>) -> Response {
    let Some(record) = state.engine.history_record(&query_id) else {
        return api_error(StatusCode::NOT_FOUND, format!("query {query_id} is not in the history"));
    };
    let mut json = record_json(&record, true);
    json["resultAvailable"] = json!(matches!(state.engine.result(&query_id), Some(Ok(_))));
    Json(json).into_response()
}

async fn clear_history(State(state): State<AppState>) -> Response {
    state.engine.clear_history();
    Json(json!({})).into_response()
}

fn record_json(record: &QueryRecord, full: bool) -> Value {
    let mut sql = record.sql.as_str();
    if !full && sql.len() > HISTORY_SQL_PREVIEW {
        sql = &sql[..sql.floor_char_boundary(HISTORY_SQL_PREVIEW)];
    }
    let mut json = json!({
        "queryId": record.query_id,
        "sequence": record.sequence,
        "sessionId": record.session_id,
        "user": record.user,
        "client": record.client,
        "database": record.database,
        "schema": record.schema,
        "queryTag": record.query_tag,
        "sql": sql,
        "sqlTruncated": sql.len() < record.sql.len(),
        "status": match record.status {
            QueryStatus::Running => "running",
            QueryStatus::Succeeded => "succeeded",
            QueryStatus::Failed => "failed",
        },
        "kind": record.kind.map(kind_name),
        "rows": record.rows,
        "error": record.error.as_ref().map(|e| error_json(e, &record.query_id)),
        "startedAt": record.started_at,
        "durationMicros": record.duration_micros,
    });
    if full {
        json["duckdbSql"] = json!(record.duckdb_sql);
        json["timezone"] = json!(record.timezone);
    }
    json
}

/// Runs catalog queries in an internal session, which keeps them out of the
/// query history and the result store.
fn with_explorer<T>(
    state: &AppState,
    work: impl FnOnce(&mut Session) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    let mut explorer = state.explorer.lock().unwrap();
    if explorer.is_none() {
        let options = SessionOptions {
            user: DEFAULT_USER.into(),
            client: "localflake web UI (explorer)".into(),
            internal: true,
            ..Default::default()
        };
        *explorer = Some(state.engine.session(options)?);
    }
    work(explorer.as_mut().unwrap())
}

type Row = HashMap<String, Option<String>>;

/// Runs a statement, returning its rows keyed by lower-case column name.
fn rows(session: &mut Session, sql: &str) -> Result<Vec<Row>, EngineError> {
    let results = session.execute(&Request { sql, ..Default::default() }).map_err(|f| f.error)?;
    let result = &results[0];
    let names: Vec<String> = result.columns.iter().map(|c| c.name.to_lowercase()).collect();
    Ok(result.json_rows()?.into_iter().map(|row| names.iter().cloned().zip(row).collect()).collect())
}

fn text(row: &Row, column: &str) -> Value {
    row.get(column).cloned().flatten().filter(|v| !v.is_empty()).map(Value::String).unwrap_or(Value::Null)
}

fn number(row: &Row, column: &str) -> Value {
    row.get(column).cloned().flatten().and_then(|v| v.parse::<u64>().ok()).map(Value::from).unwrap_or(Value::Null)
}

/// `"name"`, quoted for SQL.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `GET /catalog`: the databases.
async fn databases(State(state): State<AppState>) -> Response {
    let state2 = state.clone();
    blocking(move || {
        let databases = with_explorer(&state2, |s| rows(s, "SHOW DATABASES"));
        match databases {
            Ok(rows) => {
                let databases: Vec<Value> = rows
                    .iter()
                    .map(|r| json!({ "name": text(r, "name"), "comment": text(r, "comment"), "createdOn": text(r, "created_on") }))
                    .collect();
                Json(json!({ "databases": databases })).into_response()
            }
            Err(err) => Json(json!({ "error": error_json(&err, "") })).into_response(),
        }
    })
    .await
}

/// `GET /catalog/{database}`: a database's schemas and what's in them.
async fn database_catalog(State(state): State<AppState>, Path(database): Path<String>) -> Response {
    let state2 = state.clone();
    blocking(move || match with_explorer(&state2, |s| catalog(s, &database)) {
        Ok(catalog) => Json(catalog).into_response(),
        Err(err) => Json(json!({ "error": error_json(&err, "") })).into_response(),
    })
    .await
}

fn catalog(session: &mut Session, database: &str) -> Result<Value, EngineError> {
    let db = quote(database);
    let schemas = rows(session, &format!("SHOW SCHEMAS IN DATABASE {db}"))?;
    // The rest is best effort: a failure leaves that part of the tree empty.
    let mut optional = |sql: String| {
        rows(session, &sql).unwrap_or_else(|err| {
            tracing::warn!(%err, %sql, "web UI catalog query failed");
            vec![]
        })
    };
    let objects = optional(format!("SHOW OBJECTS IN DATABASE {db}"));
    let columns = optional(format!("SHOW COLUMNS IN DATABASE {db}"));
    let procedures = optional(format!("SHOW PROCEDURES IN DATABASE {db}"));
    let functions = optional(format!("SHOW USER FUNCTIONS IN DATABASE {db}"));
    let stages = optional(format!("SHOW STAGES IN DATABASE {db}"));
    let sequences = optional(format!("SHOW SEQUENCES IN DATABASE {db}"));
    let file_formats = optional(format!("SHOW FILE FORMATS IN DATABASE {db}"));

    let in_schema = |rows: &[Row], schema: &str| -> Vec<Row> {
        rows.iter().filter(|r| r.get("schema_name").cloned().flatten().as_deref() == Some(schema)).cloned().collect()
    };
    let schemas: Vec<Value> = schemas
        .iter()
        .filter_map(|schema_row| {
            let schema = schema_row.get("name").cloned().flatten()?;
            let schema_columns = in_schema(&columns, &schema);
            let objects: Vec<Value> = in_schema(&objects, &schema)
                .iter()
                .map(|o| {
                    let name = o.get("name").cloned().flatten().unwrap_or_default();
                    let kind = match (text(o, "kind").as_str(), text(o, "is_dynamic").as_str()) {
                        (_, Some("Y")) => "DYNAMIC_TABLE".to_owned(),
                        (Some(kind), _) => kind.to_owned(),
                        _ => "TABLE".to_owned(),
                    };
                    let columns: Vec<Value> = schema_columns
                        .iter()
                        .filter(|c| c.get("table_name").cloned().flatten().as_deref() == Some(name.as_str()))
                        .map(column_json)
                        .collect();
                    json!({
                        "name": name,
                        "kind": kind,
                        "rows": number(o, "rows"),
                        "bytes": number(o, "bytes"),
                        "comment": text(o, "comment"),
                        "columns": columns,
                    })
                })
                .collect();
            let routines = |rows: &[Row]| -> Vec<Value> {
                in_schema(rows, &schema)
                    .iter()
                    .map(|r| json!({ "name": text(r, "name"), "signature": text(r, "arguments"), "description": text(r, "description") }))
                    .collect()
            };
            let named = |rows: &[Row], extra: &str| -> Vec<Value> {
                in_schema(rows, &schema)
                    .iter()
                    .map(|r| json!({ "name": text(r, "name"), "detail": text(r, extra), "comment": text(r, "comment") }))
                    .collect()
            };
            Some(json!({
                "name": schema,
                "comment": text(schema_row, "comment"),
                "objects": objects,
                "procedures": routines(&procedures),
                "functions": routines(&functions),
                "stages": named(&stages, "url"),
                "sequences": named(&sequences, "next_value"),
                "fileFormats": named(&file_formats, "type"),
            }))
        })
        .collect();
    Ok(json!({ "name": database, "schemas": schemas }))
}

/// A column from `SHOW COLUMNS`, whose `data_type` is JSON such as
/// `{"type":"FIXED","precision":38,"scale":0,"nullable":true}`.
fn column_json(row: &Row) -> Value {
    let data_type: Value =
        row.get("data_type").cloned().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    json!({
        "name": text(row, "column_name"),
        "type": type_label(&data_type),
        "nullable": data_type["nullable"].as_bool().unwrap_or(true),
        "default": text(row, "default"),
        "autoincrement": text(row, "autoincrement"),
        "comment": text(row, "comment"),
    })
}

/// A Snowflake type name for a `SHOW COLUMNS` data type, as `DESCRIBE TABLE` writes it.
fn type_label(data_type: &Value) -> String {
    let kind = data_type["type"].as_str().unwrap_or("UNKNOWN");
    let number = |key: &str| data_type[key].as_u64();
    match kind {
        "FIXED" => format!("NUMBER({},{})", number("precision").unwrap_or(38), number("scale").unwrap_or(0)),
        "REAL" => "FLOAT".to_owned(),
        "TEXT" => match number("length") {
            Some(n) if n != 16_777_216 => format!("VARCHAR({n})"),
            _ => "VARCHAR".to_owned(),
        },
        "BINARY" => match number("length") {
            Some(n) if n != 8_388_608 => format!("BINARY({n})"),
            _ => "BINARY".to_owned(),
        },
        "TIMESTAMP_NTZ" | "TIMESTAMP_LTZ" | "TIMESTAMP_TZ" | "TIME" => {
            format!("{kind}({})", number("scale").unwrap_or(9))
        }
        other => other.to_owned(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StageFileParams {
    session: String,
    /// The stage location, e.g. `@~`, `@%"T"` or `@MY_STAGE/some/dir`.
    stage: String,
    /// Upload: the file name. Download: the file's path within the stage location.
    name: String,
}

/// Runs a PUT or GET in a worksheet session and returns the stage file path it resolves to.
fn transfer_path(client: &mut ClientSession, sql: &str) -> Result<(String, Vec<String>), Value> {
    let results = client
        .session
        .execute(&Request { sql, ..Default::default() })
        .map_err(|f| error_json(&f.error, &f.query_id))?;
    let Some(transfer) = &results[0].transfer else {
        return Err(json!({ "message": "the stage command did not describe a file transfer" }));
    };
    Ok((transfer.stage_path.clone(), transfer.src_locations.clone()))
}

fn valid_stage(stage: &str) -> bool {
    stage.starts_with('@') && !stage.contains([';', '\'', '\n'])
}

/// `PUT /stage-files?session&stage&name`: uploads the body to a stage, as a
/// driver's PUT would (the web UI loads files into tables this way).
async fn upload_stage_file(
    State(state): State<AppState>,
    Query(params): Query<StageFileParams>,
    body: Bytes,
) -> Response {
    let Some(entry) = entry_for(&state, &params.session) else { return session_expired() };
    if !valid_stage(&params.stage) || params.name.is_empty() || params.name.contains(['/', '\\', '\'']) {
        return api_error(StatusCode::BAD_REQUEST, "invalid stage or file name");
    }
    let engine = state.engine.clone();
    blocking(move || {
        entry.begin();
        let mut client = entry.client.lock().unwrap();
        let sql = format!("PUT 'file:///{}' {} AUTO_COMPRESS = FALSE OVERWRITE = TRUE", params.name, params.stage);
        let resolved = transfer_path(&mut client, &sql);
        entry.end(&client);
        let (stage_path, _) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response(),
        };
        let Some(target) = engine.stage_file(&format!("{stage_path}{}", params.name)) else {
            return api_error(StatusCode::BAD_REQUEST, "invalid stage path");
        };
        let written =
            target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&target, &body));
        match written {
            Ok(()) => Json(json!({ "size": body.len() })).into_response(),
            Err(err) => internal(err),
        }
    })
    .await
}

/// `GET /stage-files?session&stage&name`: downloads a staged file, as a driver's GET would.
async fn download_stage_file(State(state): State<AppState>, Query(params): Query<StageFileParams>) -> Response {
    let Some(entry) = entry_for(&state, &params.session) else { return session_expired() };
    if !valid_stage(&params.stage) || params.name.contains('\'') {
        return api_error(StatusCode::BAD_REQUEST, "invalid stage or file name");
    }
    let engine = state.engine.clone();
    blocking(move || {
        entry.begin();
        let mut client = entry.client.lock().unwrap();
        let location = format!("{}/{}", params.stage.trim_end_matches('/'), params.name.trim_start_matches('/'));
        let resolved = transfer_path(&mut client, &format!("GET {location} 'file:///tmp/'"));
        entry.end(&client);
        let (stage_path, files) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response(),
        };
        let wanted = params.name.trim_start_matches('/');
        let Some(file) = files.iter().find(|f| f.as_str() == wanted || f.ends_with(&format!("/{wanted}"))) else {
            return api_error(StatusCode::NOT_FOUND, "no such staged file");
        };
        let Some(path) = engine.stage_file(&format!("{stage_path}{file}")) else {
            return api_error(StatusCode::BAD_REQUEST, "invalid stage path");
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                let name = file.rsplit('/').next().unwrap_or(file).replace('"', "");
                let disposition = format!("attachment; filename=\"{name}\"");
                (
                    [
                        (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                        (header::CONTENT_DISPOSITION, disposition),
                    ],
                    bytes,
                )
                    .into_response()
            }
            Err(err) => api_error(StatusCode::NOT_FOUND, err.to_string()),
        }
    })
    .await
}
