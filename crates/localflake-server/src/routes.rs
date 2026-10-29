use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use localflake_engine::{Engine, QueryResult, Request, Session, SessionOptions};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::protocol::{
    Envelope, LoginParams, LoginRequest, QUERY_IN_PROGRESS_ASYNC, QueryRequest, ResultFormat, SessionInfo, bind_rows,
    multi_statement_response, parameters_json, query_response, transfer_response,
};

/// Snowflake's "session no longer exists, new login required" code.
const SESSION_GONE: &str = "390111";
const SERVER_VERSION: &str = concat!("localflake-", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct AppState {
    engine: Engine,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<ClientSession>>>>>,
    /// Parent query ID of a multi-statement request → its statements' results.
    multi_statement: Arc<Mutex<HashMap<String, Vec<Arc<QueryResult>>>>>,
    forced_format: Option<ResultFormat>,
}

struct ClientSession {
    id: u64,
    master_token: String,
    app: String,
    session: Session,
}

impl ClientSession {
    fn info(&self) -> SessionInfo<'_> {
        let ctx = self.session.context();
        SessionInfo {
            database: ctx.database.as_deref(),
            schema: ctx.schema.as_deref(),
            warehouse: ctx.warehouse.as_deref(),
            role: ctx.role.as_deref(),
            parameters: parameters_json(&self.session.client_parameters()),
        }
    }

    /// The result format: a `*_QUERY_RESULT_FORMAT` session parameter if set,
    /// otherwise Arrow for drivers known to support it.
    fn format(&self, forced: Option<ResultFormat>) -> ResultFormat {
        if let Some(format) = forced {
            return format;
        }
        let requested = self
            .session
            .context()
            .parameters
            .iter()
            .find(|(k, _)| k.ends_with("QUERY_RESULT_FORMAT"))
            .map(|(_, v)| v.to_uppercase());
        match requested.as_deref() {
            Some("ARROW") | Some("ARROW_FORCE") => ResultFormat::Arrow,
            Some("JSON") => ResultFormat::Json,
            _ => match self.app.as_str() {
                "PythonConnector" | "PythonSnowpark" | "Go" | "JDBC" | "ODBC" | ".NET" | "C API" => ResultFormat::Arrow,
                _ => ResultFormat::Json,
            },
        }
    }
}

pub fn router(engine: Engine, forced_format: Option<ResultFormat>) -> Router {
    let state = AppState { engine, sessions: Default::default(), multi_statement: Default::default(), forced_format };
    Router::new()
        .route("/session/v1/login-request", post(login))
        .route("/session/token-request", post(renew_token))
        .route("/session/heartbeat", post(ok))
        .route("/session", post(delete_session))
        .route("/queries/v1/query-request", post(query))
        .route("/queries/v1/abort-request", post(ok))
        .route("/queries/{id}/result", get(query_result))
        .route("/monitoring/queries/{id}", get(query_status))
        // The Rust-core connectors join this URL with a doubled slash.
        .route("/monitoring/queries//{id}", get(query_status))
        .route("/telemetry/send", post(ok))
        .route("/telemetry/send/sessionless", post(ok))
        // Presigned URLs for PUT and GET (see `protocol::transfer_response`).
        .route(
            "/__localflake/files/{*path}",
            get(download_file).head(download_file).put(upload_file).layer(DefaultBodyLimit::disable()),
        )
        .fallback(unknown)
        .with_state(state)
}

async fn login(State(state): State<AppState>, Query(params): Query<LoginParams>, body: Bytes) -> Response {
    let request: LoginRequest = match decode(&body) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // Drivers send unset context fields as empty strings.
    let non_empty = |v: Option<String>| v.filter(|s| !s.is_empty());
    let data = request.data;
    let user = data.login_name.clone().unwrap_or_default();
    let app = data.client_app_id.clone().unwrap_or_default();
    let options = SessionOptions {
        account: data.account_name.clone().unwrap_or_default(),
        user: user.clone(),
        database: non_empty(params.database_name),
        schema: non_empty(params.schema_name),
        warehouse: non_empty(params.warehouse),
        role: non_empty(params.role_name),
        parameters: data
            .session_parameters
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())))
            .collect(),
    };
    let engine = state.engine.clone();
    let session = match tokio::task::spawn_blocking(move || engine.session(options)).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => return Json(Envelope::engine_error(&err, "")).into_response(),
        Err(err) => return internal(err),
    };

    let token = format!("ver:1-hint:0-ETMsDgAAA{}", Uuid::new_v4().simple());
    let master_token = format!("ver:1-hint:0-ETMsDgAAB{}", Uuid::new_v4().simple());
    let client = ClientSession { id: session.context().session_id, master_token: master_token.clone(), app, session };
    tracing::info!(session = client.id, %user, app = %client.app, database = ?client.session.context().database, "login");

    let info = client.info();
    let data = json!({
        "token": token,
        "masterToken": master_token,
        "validityInSeconds": 3600,
        "masterValidityInSeconds": 14400,
        "displayUserName": user,
        "serverVersion": SERVER_VERSION,
        "firstLogin": false,
        "remMeToken": null,
        "remMeValidityInSeconds": 0,
        "healthCheckInterval": 45,
        "newClientForUpgrade": null,
        "sessionId": client.id,
        "parameters": info.parameters,
        "sessionInfo": {
            "databaseName": info.database,
            "schemaName": info.schema,
            "warehouseName": info.warehouse,
            "roleName": info.role,
        },
        "idToken": null,
        "idTokenValidityInSeconds": 0,
        "responseData": null,
        "mfaToken": null,
        "mfaTokenValidityInSeconds": 0,
    });
    state.sessions.lock().unwrap().insert(token, Arc::new(Mutex::new(client)));
    Json(Envelope::ok(data)).into_response()
}

/// Drivers call this with the master token when the session token expires.
/// Tokens here never expire, so we hand back the existing session token.
async fn renew_token(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(master) = auth_token(&headers) else { return session_gone() };
    let sessions = state.sessions.lock().unwrap();
    let found = sessions.iter().find_map(|(token, s)| {
        let s = s.lock().unwrap();
        (s.master_token == master).then(|| (token.clone(), s.id))
    });
    let Some((token, id)) = found else { return session_gone() };
    Json(Envelope::ok(json!({
        "sessionToken": token,
        "validityInSecondsST": 3600,
        "masterToken": master,
        "validityInSecondsMT": 14400,
        "sessionId": id,
    })))
    .into_response()
}

async fn delete_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = auth_token(&headers)
        && let Some(client) = state.sessions.lock().unwrap().remove(&token)
    {
        tracing::info!(session = client.lock().unwrap().id, "logout");
    }
    Json(Envelope::ok(Value::Null)).into_response()
}

async fn query(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(client) = client_for(&state, &headers) else { return session_gone() };
    let request: QueryRequest = match decode(&body) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let bindings = match request.bindings.as_ref().map(bind_rows).transpose() {
        Ok(b) => b.unwrap_or_default(),
        Err(message) => return Json(Envelope::error("002049", message, Value::Null)).into_response(),
    };
    let statement_count = request.parameters.get("MULTI_STATEMENT_COUNT").and_then(|v| match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    });

    let forced = state.forced_format;
    let multi_statement = state.multi_statement.clone();
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost").to_owned();
    let base_url = format!("http://{host}/__localflake/files/");
    let response = tokio::task::spawn_blocking(move || {
        let mut client = client.lock().unwrap();
        tracing::info!(session = client.id, sql = %request.sql_text, "query");
        let outcome = client.session.execute(&Request {
            sql: &request.sql_text,
            bindings,
            describe_only: request.describe_only,
            statement_count: statement_count.map(|n| n as usize),
        });
        let results = match outcome {
            Ok(results) => results,
            Err(failure) => {
                tracing::info!(query_id = %failure.query_id, code = failure.error.code, message = %failure.error.message, "query failed");
                return Envelope::engine_error(&failure.error, &failure.query_id);
            }
        };
        let info = client.info();
        let query_id = if results.len() == 1 {
            results[0].query_id.clone()
        } else {
            let parent = Uuid::new_v4().to_string();
            multi_statement.lock().unwrap().insert(parent.clone(), results.clone());
            parent
        };
        if request.async_exec {
            // Queries run synchronously; the result is ready by the time the driver asks.
            return Envelope {
                data: json!({ "queryId": query_id, "getResultUrl": format!("/queries/{query_id}/result") }),
                code: Some(QUERY_IN_PROGRESS_ASYNC.to_owned()),
                message: Some("Asynchronous execution in progress. Use provided query id to perform query monitoring and management.".to_owned()),
                success: true,
            };
        }
        if results.len() > 1 {
            return Envelope::ok(multi_statement_response(&query_id, &results, &info));
        }
        if let Some(transfer) = &results[0].transfer {
            return Envelope::ok(transfer_response(&results[0], transfer, &base_url, &info));
        }
        match query_response(&results[0], client.format(forced), &info) {
            Ok(data) => Envelope::ok(data),
            Err(err) => Envelope::engine_error(&err, &query_id),
        }
    })
    .await;
    match response {
        Ok(envelope) => Json(envelope).into_response(),
        Err(err) => internal(err),
    }
}

/// `GET /queries/{id}/result`: results of async queries and of each statement in a multi-statement request.
async fn query_result(State(state): State<AppState>, headers: HeaderMap, Path(query_id): Path<String>) -> Response {
    let client = client_for(&state, &headers);
    let (format, info_parameters) = match &client {
        Some(c) => {
            let c = c.lock().unwrap();
            (c.format(state.forced_format), Some(c.session.client_parameters()))
        }
        None => (state.forced_format.unwrap_or(ResultFormat::Json), None),
    };
    let info = || SessionInfo {
        database: None,
        schema: None,
        warehouse: None,
        role: None,
        parameters: parameters_json(info_parameters.as_deref().unwrap_or_default()),
    };
    if let Some(results) = state.multi_statement.lock().unwrap().get(&query_id) {
        return Json(Envelope::ok(multi_statement_response(&query_id, results, &info()))).into_response();
    }
    let envelope = match state.engine.result(&query_id) {
        Some(Ok(result)) => match query_response(&result, format, &info()) {
            Ok(data) => Envelope::ok(data),
            Err(err) => Envelope::engine_error(&err, &query_id),
        },
        Some(Err(err)) => Envelope::engine_error(&err, &query_id),
        None => Envelope::error("000709", format!("Statement {query_id} not found"), json!({ "queryId": query_id })),
    };
    Json(envelope).into_response()
}

/// `GET /monitoring/queries/{id}`: status for async query polling.
async fn query_status(State(state): State<AppState>, Path(query_id): Path<String>) -> Response {
    let known_parent = state.multi_statement.lock().unwrap().contains_key(&query_id);
    let entry = match (known_parent, state.engine.result(&query_id)) {
        (true, _) | (_, Some(Ok(_))) => json!({ "id": query_id, "status": "SUCCESS", "state": "SUCCEEDED" }),
        (_, Some(Err(err))) => json!({
            "id": query_id, "status": "FAILED_WITH_ERROR", "state": "FAILED",
            "errorCode": err.code, "errorMessage": err.message,
        }),
        (_, None) => return Json(Envelope::ok(json!({ "queries": [] }))).into_response(),
    };
    Json(Envelope::ok(json!({ "queries": [entry] }))).into_response()
}

/// `PUT /__localflake/files/{path}`: a driver uploading a staged file.
async fn upload_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(target) = state.engine.stage_file(&path) else { return StatusCode::BAD_REQUEST.into_response() };
    // `OVERWRITE = FALSE`: GCS's "only if the object doesn't exist" precondition.
    if headers.get("x-goog-if-generation-match").is_some_and(|v| v == "0") && target.exists() {
        return StatusCode::PRECONDITION_FAILED.into_response();
    }
    let written = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write then rename, so readers never see a partial file.
        let partial = target.with_file_name(format!(
            "{}.{}.lf-partial",
            target.file_name().unwrap_or_default().to_string_lossy(),
            Uuid::new_v4().simple()
        ));
        std::fs::write(&partial, &body)?;
        std::fs::rename(&partial, &target)
    })
    .await;
    match written {
        Ok(Ok(())) => StatusCode::OK.into_response(),
        Ok(Err(err)) => internal(err),
        Err(err) => internal(err),
    }
}

/// `GET|HEAD /__localflake/files/{path}`: a driver downloading a staged file.
async fn download_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> Response {
    let Some(source) = state.engine.stage_file(&path) else { return StatusCode::BAD_REQUEST.into_response() };
    let Ok(bytes) = tokio::fs::read(&source).await else { return StatusCode::NOT_FOUND.into_response() };
    let total = bytes.len();
    // Drivers fetch large files in ranges.
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(start, end)| {
            let start: usize = start.parse().ok()?;
            let end: usize = if end.is_empty() {
                total.checked_sub(1)?
            } else {
                end.parse::<usize>().ok()?.min(total.checked_sub(1)?)
            };
            (start <= end).then_some((start, end))
        });
    let (status, body, content_range) = match range {
        Some((start, end)) => {
            (StatusCode::PARTIAL_CONTENT, bytes[start..=end].to_vec(), Some(format!("bytes {start}-{end}/{total}")))
        }
        None => (StatusCode::OK, bytes, None),
    };
    let length = body.len();
    let mut response = if method == axum::http::Method::HEAD {
        (status, Vec::new()).into_response()
    } else {
        (status, body).into_response()
    };
    let response_headers = response.headers_mut();
    response_headers.insert(header::CONTENT_LENGTH, length.into());
    response_headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    response_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    if let Some(content_range) = content_range {
        response_headers.insert(header::CONTENT_RANGE, content_range.parse().unwrap());
    }
    response
}

async fn ok() -> Json<Envelope> {
    Json(Envelope::ok(Value::Null))
}

/// Logs endpoints we haven't implemented, so it's obvious what a driver needs next.
async fn unknown(uri: Uri) -> Response {
    tracing::warn!(%uri, "unimplemented endpoint");
    let message = format!("localflake does not implement {}", uri.path());
    (StatusCode::NOT_FOUND, Json(Envelope::error("000404", message, Value::Null))).into_response()
}

fn client_for(state: &AppState, headers: &HeaderMap) -> Option<Arc<Mutex<ClientSession>>> {
    auth_token(headers).and_then(|t| state.sessions.lock().unwrap().get(&t).cloned())
}

/// Parses `Authorization: Snowflake Token="..."`.
fn auth_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Snowflake Token=")?.trim_matches('"');
    Some(token.to_owned())
}

/// Decodes a JSON request body. Drivers gzip request bodies, so detect that by
/// the magic bytes rather than trusting `Content-Encoding`.
#[allow(clippy::result_large_err)]
fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, Response> {
    let bad_request =
        |msg: String| (StatusCode::BAD_REQUEST, Json(Envelope::error("000400", msg, Value::Null))).into_response();
    let mut decompressed = Vec::new();
    let json = if body.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(body)
            .read_to_end(&mut decompressed)
            .map_err(|e| bad_request(format!("invalid gzip body: {e}")))?;
        &decompressed[..]
    } else {
        body
    };
    serde_json::from_slice(json).map_err(|e| bad_request(format!("invalid request body: {e}")))
}

fn session_gone() -> Response {
    let message = "Session no longer exists.  New login required to access the service.";
    Json(Envelope::error(SESSION_GONE, message, Value::Null)).into_response()
}

fn internal(err: impl std::fmt::Display) -> Response {
    tracing::error!(%err, "internal error");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(Envelope::error("000603", err.to_string(), Value::Null))).into_response()
}
