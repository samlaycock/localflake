//! Wire types for the Snowflake driver protocol (`/session/v1/login-request`,
//! `/queries/v1/query-request`, ...). This protocol isn't formally documented;
//! the shapes here follow the open-source drivers, e.g. snowflake-connector-python
//! and gosnowflake.

use std::collections::BTreeMap;

use localflake_engine::{Bind, Column, EngineError, QueryResult, StatementKind, Transfer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// "Asynchronous execution in progress": returned for `asyncExec` requests.
pub const QUERY_IN_PROGRESS_ASYNC: &str = "333334";

/// `statementTypeId` of a multi-statement parent; drivers then fetch `resultIds`.
const MULTI_STATEMENT_TYPE_ID: u32 = 0xA000;

/// Every response is wrapped in `{data, code, message, success}`.
#[derive(Serialize)]
pub struct Envelope {
    pub data: Value,
    pub code: Option<String>,
    pub message: Option<String>,
    pub success: bool,
}

impl Envelope {
    pub fn ok(data: Value) -> Self {
        Self { data, code: None, message: None, success: true }
    }

    pub fn error(code: &str, message: impl Into<String>, data: Value) -> Self {
        Self { data, code: Some(code.to_owned()), message: Some(message.into()), success: false }
    }

    pub fn engine_error(err: &EngineError, query_id: &str) -> Self {
        Self::error(
            err.code,
            err.message.clone(),
            json!({ "errorCode": err.code, "sqlState": err.sql_state, "queryId": query_id, "internalError": false }),
        )
    }
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub data: LoginData,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct LoginData {
    pub login_name: Option<String>,
    pub account_name: Option<String>,
    pub client_app_id: Option<String>,
    #[serde(default)]
    pub session_parameters: serde_json::Map<String, Value>,
}

/// Query string parameters on the login request.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginParams {
    pub database_name: Option<String>,
    pub schema_name: Option<String>,
    pub warehouse: Option<String>,
    pub role_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRequest {
    pub sql_text: String,
    #[serde(default)]
    pub async_exec: bool,
    #[serde(default)]
    pub describe_only: bool,
    pub bindings: Option<BTreeMap<String, Binding>>,
    #[serde(default)]
    pub parameters: serde_json::Map<String, Value>,
}

#[derive(Deserialize, Debug)]
pub struct Binding {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: Value,
}

/// Converts Snowflake bindings (`{"1": {"type": "FIXED", "value": "1"}, ...}`)
/// into rows of engine bind values. Array values mean one row per element.
pub fn bind_rows(bindings: &BTreeMap<String, Binding>) -> Result<Vec<Vec<Bind>>, String> {
    let mut ordered: Vec<(usize, &Binding)> = bindings
        .iter()
        .map(|(k, b)| {
            k.parse::<usize>().map(|i| (i, b)).map_err(|_| format!("named bind variable '{k}' is not supported"))
        })
        .collect::<Result<_, _>>()?;
    ordered.sort_by_key(|(i, _)| *i);
    let row_count = ordered.iter().find_map(|(_, b)| b.value.as_array().map(Vec::len)).unwrap_or(1);
    let mut rows = vec![Vec::with_capacity(ordered.len()); row_count];
    for (_, binding) in ordered {
        for (row, out) in rows.iter_mut().enumerate() {
            let value = match &binding.value {
                Value::Array(values) => values.get(row).unwrap_or(&Value::Null),
                single => single,
            };
            out.push(bind_value(&binding.kind, value)?);
        }
    }
    Ok(rows)
}

fn bind_value(kind: &str, value: &Value) -> Result<Bind, String> {
    let text = match value {
        Value::Null => return Ok(Bind::Null),
        Value::String(s) => s.clone(),
        Value::Bool(b) => return Ok(Bind::Bool(*b)),
        other => other.to_string(),
    };
    let int = |s: &str| {
        s.split_whitespace().next().unwrap_or("").parse::<i64>().map_err(|e| format!("invalid {kind} bind '{s}': {e}"))
    };
    Ok(match kind.to_uppercase().as_str() {
        "FIXED" => text.parse::<i64>().map(Bind::Int).unwrap_or(Bind::Decimal(text)),
        "REAL" => Bind::Float(text.parse().map_err(|e| format!("invalid REAL bind '{text}': {e}"))?),
        "BOOLEAN" => Bind::Bool(matches!(text.to_lowercase().as_str(), "true" | "1" | "t" | "yes")),
        // Dates are milliseconds since the epoch.
        "DATE" => Bind::Date(int(&text)?.div_euclid(86_400_000) as i32),
        // Times and timestamps are nanoseconds (TIMESTAMP_TZ appends an offset).
        "TIME" => Bind::Time(int(&text)?.div_euclid(1000)),
        "TIMESTAMP_NTZ" => Bind::Timestamp(int(&text)?.div_euclid(1000)),
        "TIMESTAMP_LTZ" | "TIMESTAMP_TZ" => Bind::TimestampTz(int(&text)?.div_euclid(1000)),
        "BINARY" => Bind::Binary(hex_decode(&text)?),
        _ => Bind::Text(text),
    })
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()).ok_or_else(|| format!("invalid hex '{s}'"))
        })
        .collect()
}

pub fn rowtype(column: &Column) -> Value {
    json!({
        "name": column.name,
        "database": "",
        "schema": "",
        "table": "",
        "type": column.sf_type.name(),
        "precision": column.precision,
        "scale": column.scale,
        "length": column.length,
        "byteLength": column.length,
        "nullable": column.nullable,
        "collation": null,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultFormat {
    Json,
    Arrow,
}

pub struct SessionInfo<'a> {
    pub database: Option<&'a str>,
    pub schema: Option<&'a str>,
    pub warehouse: Option<&'a str>,
    pub role: Option<&'a str>,
    pub parameters: Value,
}

pub fn query_response(result: &QueryResult, format: ResultFormat, session: &SessionInfo) -> Result<Value, EngineError> {
    // Snowflake only uses Arrow for query results; DML counts, DDL status and SHOW
    // output are always JSON, and drivers only read DML row counts from JSON rowsets.
    let format = if result.kind == StatementKind::Query { format } else { ResultFormat::Json };
    let mut data = json!({
        "parameters": session.parameters,
        "rowtype": result.columns.iter().map(rowtype).collect::<Vec<_>>(),
        "total": result.row_count(),
        "returned": result.row_count(),
        "queryId": result.query_id,
        "databaseProvider": null,
        "finalDatabaseName": session.database,
        "finalSchemaName": session.schema,
        "finalWarehouseName": session.warehouse,
        "finalRoleName": session.role,
        "numberOfBinds": 0,
        "arrayBindSupported": false,
        "statementTypeId": result.kind.type_id(),
        "version": 1,
        "sendResultTime": now_millis(),
        "queryResultFormat": match format { ResultFormat::Json => "json", ResultFormat::Arrow => "arrow" },
    });
    if let Some(stats) = dml_stats(result)? {
        data["stats"] = stats;
    }
    match format {
        ResultFormat::Json => data["rowset"] = json!(result.json_rows()?),
        ResultFormat::Arrow => {
            data["rowset"] = json!([]);
            data["rowsetBase64"] = json!(result.arrow_rowset()?);
        }
    }
    Ok(data)
}

/// The response to PUT and GET. Snowflake answers with a storage location
/// and the driver moves the files itself; localflake poses as a GCS stage
/// with presigned URLs pointing back at its own `/__localflake/files/` route,
/// which every driver supports and which works across containers.
pub fn transfer_response(result: &QueryResult, transfer: &Transfer, base_url: &str, session: &SessionInfo) -> Value {
    // The Rust-core drivers check that a URL "signs" the headers they send.
    let url = |relative: &str| {
        format!(
            "{base_url}{}{}?X-Goog-SignedHeaders=host%3Bx-goog-if-generation-match%3Bx-goog-meta-encryptiondata%3Bx-goog-meta-matdesc%3Bx-goog-meta-sfc-digest",
            encode_path(&transfer.stage_path),
            encode_path(relative)
        )
    };
    // PUT: the driver re-sends the command once per file, naming that file.
    let presigned = transfer.upload.then(|| {
        let local = transfer.src_locations.first().map(String::as_str).unwrap_or_default();
        let mut name = local.rsplit(['/', '\\']).next().unwrap_or(local).to_owned();
        // The driver gzips files that aren't compressed yet, adding `.gz`.
        const COMPRESSED: &[&str] =
            &[".gz", ".bz2", ".br", ".zst", ".deflate", ".raw_deflate", ".parquet", ".orc", ".avro"];
        let lower = name.to_lowercase();
        if transfer.auto_compress && !COMPRESSED.iter().any(|ext| lower.ends_with(ext)) {
            name.push_str(".gz");
        }
        url(&name)
    });
    let mut data = json!({
        "command": if transfer.upload { "UPLOAD" } else { "DOWNLOAD" },
        "src_locations": transfer.src_locations,
        "parallel": transfer.parallel,
        "threshold": 200 * 1024 * 1024,
        "autoCompress": transfer.auto_compress,
        "overwrite": transfer.overwrite,
        "sourceCompression": transfer.source_compression.to_lowercase(),
        "clientShowEncryptionParameter": false,
        "encryptionMaterial": null,
        "stageInfo": {
            "locationType": "GCS",
            "location": format!("localflake/{}", transfer.stage_path),
            "path": transfer.stage_path,
            "region": "local",
            "storageAccount": null,
            "isClientSideEncrypted": false,
            "creds": {},
            "presignedUrl": presigned,
            "endPoint": null,
            "useRegionalUrl": false,
            "useVirtualUrl": false,
        },
        "queryId": result.query_id,
        "parameters": session.parameters,
        "rowtype": [],
        "rowset": [],
        "total": 0,
        "returned": 0,
        "statementTypeId": StatementKind::Other.type_id(),
        "finalDatabaseName": session.database,
        "finalSchemaName": session.schema,
        "finalWarehouseName": session.warehouse,
        "finalRoleName": session.role,
        "version": 1,
        "queryResultFormat": "json",
    });
    if !transfer.upload {
        data["presignedUrls"] = json!(transfer.src_locations.iter().map(|f| url(f)).collect::<Vec<_>>());
        data["localLocation"] = json!(transfer.local_location);
    }
    data
}

/// Percent-encodes each segment of a `/`-separated path.
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// DML row counts as `stats`, which the Rust-core drivers read for `rowcount`.
fn dml_stats(result: &QueryResult) -> Result<Option<Value>, EngineError> {
    if !matches!(
        result.kind,
        StatementKind::Insert | StatementKind::Update | StatementKind::Delete | StatementKind::Merge
    ) {
        return Ok(None);
    }
    let rows = result.json_rows()?;
    let Some(row) = rows.first() else { return Ok(None) };
    let mut stats = json!({"numRowsInserted": 0, "numRowsUpdated": 0, "numRowsDeleted": 0, "numDmlDuplicates": 0});
    for (column, value) in result.columns.iter().zip(row) {
        // Multi-table inserts report "number of rows inserted into <table>" per target.
        let key = match column.name.as_str() {
            name if name.starts_with("number of rows inserted") => "numRowsInserted",
            "number of rows updated" => "numRowsUpdated",
            "number of rows deleted" => "numRowsDeleted",
            _ => continue,
        };
        let n = value.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        stats[key] = json!(stats[key].as_i64().unwrap_or(0) + n);
    }
    Ok(Some(stats))
}

/// The parent result of a multi-statement request, pointing at each statement's result.
pub fn multi_statement_response(
    parent_id: &str,
    results: &[std::sync::Arc<QueryResult>],
    session: &SessionInfo,
) -> Value {
    let ids: Vec<&str> = results.iter().map(|r| r.query_id.as_str()).collect();
    let types: Vec<String> = results.iter().map(|r| r.kind.type_id().to_string()).collect();
    json!({
        "parameters": session.parameters,
        "rowtype": [{
            "name": "multiple statement execution", "database": "", "schema": "", "table": "", "type": "text",
            "precision": null, "scale": null, "length": 16777216, "byteLength": 16777216, "nullable": false, "collation": null,
        }],
        "rowset": [["Multiple statements executed successfully."]],
        "total": 1,
        "returned": 1,
        "queryId": parent_id,
        "resultIds": ids.join(","),
        "resultTypes": types.join(","),
        "finalDatabaseName": session.database,
        "finalSchemaName": session.schema,
        "finalWarehouseName": session.warehouse,
        "finalRoleName": session.role,
        "statementTypeId": MULTI_STATEMENT_TYPE_ID,
        "version": 1,
        "queryResultFormat": "json",
    })
}

/// Session parameters as `[{name, value}]`, with booleans and numbers typed.
pub fn parameters_json(parameters: &[(String, String)]) -> Value {
    Value::Array(
        parameters
            .iter()
            .map(|(name, value)| {
                let value = match value.as_str() {
                    "true" => json!(true),
                    "false" => json!(false),
                    v if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) => json!(v.parse::<u64>().unwrap_or(0)),
                    v => json!(v),
                };
                json!({ "name": name, "value": value })
            })
            .collect(),
    )
}

pub fn now_millis() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bindings() {
        let bindings: BTreeMap<String, Binding> = serde_json::from_value(json!({
            "2": {"type": "TEXT", "value": "x"},
            "1": {"type": "FIXED", "value": "42"},
            "3": {"type": "DATE", "value": "86400000"},
            "4": {"type": "TIMESTAMP_NTZ", "value": "1500000000"},
            "5": {"type": "TEXT", "value": null},
        }))
        .unwrap();
        assert_eq!(
            bind_rows(&bindings).unwrap(),
            vec![vec![Bind::Int(42), Bind::Text("x".into()), Bind::Date(1), Bind::Timestamp(1_500_000), Bind::Null]]
        );
    }

    #[test]
    fn decodes_array_bindings() {
        let bindings: BTreeMap<String, Binding> = serde_json::from_value(json!({
            "1": {"type": "FIXED", "value": ["1", "2"]},
            "2": {"type": "TEXT", "value": ["a", null]},
        }))
        .unwrap();
        assert_eq!(
            bind_rows(&bindings).unwrap(),
            vec![vec![Bind::Int(1), Bind::Text("a".into())], vec![Bind::Int(2), Bind::Null]]
        );
    }
}
