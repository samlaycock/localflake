//! Stages: internal stages kept as directories, file formats, PUT/GET
//! descriptors for the drivers, LIST/REMOVE, COPY INTO (load and unload)
//! and queries over staged files.
//!
//! Layout under the stage root (`<data_dir>/stages`, or a temp directory):
//!
//! ```text
//! users/<USER>/                         user stage (@~)
//! db/<DB>/<SCHEMA>/tables/<TABLE>/       table stages (@%t)
//! db/<DB>/<SCHEMA>/stages/<NAME>/        named internal stages (@s)
//! db/<DB>/<SCHEMA>/stages/<NAME>.json    stage definitions
//! db/<DB>/<SCHEMA>/formats/<NAME>.json   named file formats
//! ```
//!
//! Names are percent-encoded so that case-insensitive file systems keep
//! `"a"` and `A` apart.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use localflake_sql::stage::{CopySource, Location, OptValue, Options, StageCommand, StageName, option};
use localflake_sql::{Command, Scope, Show, StatementKind, encode_identifier};
use serde_json::{Value, json};

use crate::files::{self, Format, Record};
use crate::{EngineError, QueryResult, Session, SfType, literal};

/// The directory tree holding every stage's files and definitions.
pub(crate) struct StageStore {
    root: PathBuf,
    /// Remove the tree when the engine closes (in-memory engines).
    temporary: bool,
    /// Files loaded into each table: table key → (file path → content MD5).
    load_history: Mutex<HashMap<String, HashMap<String, String>>>,
}

impl Drop for StageStore {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl StageStore {
    pub fn open(data_dir: Option<&Path>) -> std::io::Result<Self> {
        let (root, temporary) = match data_dir {
            Some(dir) => (dir.join("stages"), false),
            None => (
                std::env::temp_dir().join(format!(
                    "localflake/stages-{}-{}",
                    std::process::id(),
                    uuid::Uuid::new_v4().simple()
                )),
                true,
            ),
        };
        std::fs::create_dir_all(&root)?;
        Ok(Self { root, temporary, load_history: Default::default() })
    }

    /// The file for a stage-root-relative path from a presigned URL, if it is safe.
    pub fn file(&self, relative: &str) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for part in relative.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                return None;
            }
            path.push(part);
        }
        Some(path)
    }

    pub fn drop_database(&self, database: &str) {
        let _ = std::fs::remove_dir_all(self.root.join("db").join(fs_name(database)));
    }

    fn schema_dir(&self, database: &str, schema: &str) -> PathBuf {
        self.root.join("db").join(fs_name(database)).join(fs_name(schema))
    }
}

/// Percent-encodes everything but upper-case letters, digits and `_`.
fn fs_name(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn fs_unname(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = name.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A stage resolved to a directory.
struct Stage {
    /// Where the stage's files live.
    dir: PathBuf,
    /// `dir` relative to the stage root, for presigned URLs (internal stages only).
    url_path: Option<String>,
    /// Prefix of file names in LIST and COPY output (`mystage/` for named stages).
    display: String,
    /// The stage's own file format and copy options (named stages).
    file_format: BTreeMap<String, Value>,
    copy_options: BTreeMap<String, Value>,
    /// The fully qualified name, for messages.
    name: String,
}

/// A file in a stage.
struct StagedFile {
    /// Path relative to the stage directory, `/`-separated.
    relative: String,
    path: PathBuf,
    size: u64,
    modified: std::time::SystemTime,
}

impl StagedFile {
    fn md5(&self) -> String {
        use md5::Digest;
        let bytes = std::fs::read(&self.path).unwrap_or_default();
        md5::Md5::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Recursively lists the files under `dir`, sorted by path.
fn walk(dir: &Path) -> Vec<StagedFile> {
    fn go(base: &Path, dir: &Path, out: &mut Vec<StagedFile>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                go(base, &path, out);
            } else if let Ok(relative) = path.strip_prefix(base) {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                // Partial uploads are written next to their target first.
                if name.ends_with(".lf-partial") {
                    continue;
                }
                out.push(StagedFile {
                    relative: relative
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                    path: path.clone(),
                    size: meta.len(),
                    modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                });
            }
        }
    }
    let mut out = vec![];
    go(dir, dir, &mut out);
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    out
}

fn anchored(pattern: &str) -> Result<regex::Regex, EngineError> {
    regex::Regex::new(&format!("^(?:{pattern})$"))
        .map_err(|e| EngineError::load("100097", format!("Invalid regular expression '{pattern}': {e}")))
}

/// What the driver must do for PUT and GET: Snowflake answers these with a
/// storage location, and the driver moves the files itself.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub upload: bool,
    /// PUT: the local path or glob from the command. GET: stage files relative to `stage_path`.
    pub src_locations: Vec<String>,
    /// The stage directory relative to the stage root (`/`-terminated), for presigned URLs.
    pub stage_path: String,
    /// GET: the local directory to download into.
    pub local_location: Option<String>,
    pub auto_compress: bool,
    pub overwrite: bool,
    pub source_compression: String,
    pub parallel: u64,
}

/// A value for a result built from Rust data.
enum Cell {
    Text(Option<String>),
    Int(Option<i64>),
    Bool(bool),
    /// Milliseconds since the epoch.
    Time(i64),
}

impl Cell {
    fn sql(&self) -> String {
        match self {
            Cell::Text(Some(s)) => literal(s),
            Cell::Text(None) => "NULL".into(),
            Cell::Int(Some(n)) => n.to_string(),
            Cell::Int(None) => "NULL".into(),
            Cell::Bool(b) => b.to_string(),
            Cell::Time(ms) => format!("make_timestamp({}::BIGINT)::TIMESTAMPTZ", ms * 1000),
        }
    }
}

fn text(s: impl Into<String>) -> Cell {
    Cell::Text(Some(s.into()))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A Snowflake identifier for generated SQL (already normalized form).
fn q(name: &str) -> String {
    format!("\"{}\"", encode_identifier(name).replace('"', "\"\""))
}

/// Raw rows of one staged file, ready for the load table.
struct FileRows {
    file: StagedFile,
    display: String,
    records: Vec<Record>,
    /// Parse errors by record index: (message, code).
    errors: HashMap<usize, (String, &'static str)>,
}

/// A temp table holding staged rows: `COLUMN1..n` text plus metadata columns.
struct LoadTable {
    name: String,
    columns: usize,
    /// The values are JSON documents (JSON and Parquet files), not text.
    json: bool,
}

const LOAD_COLUMNS: &str = "\"METADATA$FILENAME\" VARCHAR, \"METADATA$FILE_ROW_NUMBER\" BIGINT, \
    \"METADATA$FILE_CONTENT_KEY\" VARCHAR, \"METADATA$FILE_LAST_MODIFIED\" TIMESTAMP, \
    \"METADATA$START_SCAN_TIME\" TIMESTAMPTZ, __LF_FILE BIGINT, __LF_ROW BIGINT, __LF_LINE BIGINT, \
    __LF_ERROR VARCHAR, __LF_ERROR_CODE VARCHAR, __LF_ERROR_COLUMN VARCHAR";

impl Session {
    pub(crate) fn run_stage_command(
        &mut self,
        command: StageCommand,
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        let id = query_id.to_owned();
        let status = |message: String| Ok(QueryResult::status(id.clone(), &message, StatementKind::Ddl));
        match command {
            StageCommand::CreateStage { name, or_replace, if_not_exists, temporary, options } => {
                let (db, schema, stage) = self.qualify(&name)?;
                let definition = self.stage_definition_path(&db, &schema, &stage);
                if definition.exists() {
                    if if_not_exists {
                        return status(format!("{stage} already exists, statement succeeded."));
                    }
                    if !or_replace {
                        return Err(EngineError::already_exists("Stage", &stage));
                    }
                    self.remove_stage(&db, &schema, &stage);
                }
                let mut def = json!({
                    "name": stage, "database": db, "schema": schema, "created_on": now_millis(),
                    "temporary": temporary, "owner": self.ctx.role.clone().unwrap_or_default(),
                    "url": null, "comment": null, "file_format": {}, "copy_options": {},
                });
                self.apply_stage_options(&mut def, &options)?;
                self.write_definition(&definition, &def)?;
                if temporary {
                    self.temporary_files.push(definition.clone());
                    self.temporary_files.push(definition.with_extension(""));
                }
                status(format!("Stage area {stage} successfully created."))
            }
            StageCommand::AlterStage { name, if_exists, rename, set } => {
                let (db, schema, stage) = self.qualify(&name)?;
                let definition = self.stage_definition_path(&db, &schema, &stage);
                let Some(mut def) = read_definition(&definition) else {
                    if if_exists {
                        return status("Statement executed successfully.".into());
                    }
                    return Err(EngineError::does_not_exist("Stage", &format!("{db}.{schema}.{stage}")));
                };
                self.apply_stage_options(&mut def, &set)?;
                self.write_definition(&definition, &def)?;
                if let Some(new_name) = rename {
                    let (ndb, nschema, nstage) = self.qualify(&new_name)?;
                    let target = self.stage_definition_path(&ndb, &nschema, &nstage);
                    if target.exists() {
                        return Err(EngineError::already_exists("Stage", &nstage));
                    }
                    def["name"] = json!(nstage);
                    def["database"] = json!(ndb);
                    def["schema"] = json!(nschema);
                    self.write_definition(&target, &def)?;
                    let _ = std::fs::remove_file(&definition);
                    let (from, to) = (definition.with_extension(""), target.with_extension(""));
                    if from.exists() {
                        std::fs::rename(from, to).map_err(EngineError::internal)?;
                    }
                }
                status("Statement executed successfully.".into())
            }
            StageCommand::DropStage { name, if_exists } => {
                let (db, schema, stage) = self.qualify(&name)?;
                if !self.stage_definition_path(&db, &schema, &stage).exists() {
                    if if_exists {
                        return status(format!("Drop statement executed successfully ({stage} already dropped)."));
                    }
                    return Err(EngineError::does_not_exist("Stage", &format!("{db}.{schema}.{stage}")));
                }
                self.remove_stage(&db, &schema, &stage);
                status(format!("{stage} successfully dropped."))
            }
            StageCommand::CreateFileFormat { name, or_replace, if_not_exists, temporary, options } => {
                let (db, schema, format) = self.qualify(&name)?;
                let path = self.format_path(&db, &schema, &format);
                if path.exists() {
                    if if_not_exists {
                        return status(format!("{format} already exists, statement succeeded."));
                    }
                    if !or_replace {
                        return Err(EngineError::already_exists("File format", &format));
                    }
                }
                let mut opts = files::options_json(&options);
                let comment = opts.remove("COMMENT");
                let kind = opts.get("TYPE").and_then(Value::as_str).unwrap_or("CSV").to_uppercase();
                if !matches!(kind.as_str(), "CSV" | "JSON" | "PARQUET" | "AVRO" | "ORC" | "XML" | "CUSTOM") {
                    return Err(EngineError::load(
                        "002001",
                        format!("SQL compilation error:\ninvalid value ['{kind}'] for parameter 'TYPE'"),
                    ));
                }
                opts.insert("TYPE".into(), json!(kind));
                let def = json!({
                    "name": format, "database": db, "schema": schema, "created_on": now_millis(),
                    "owner": self.ctx.role.clone().unwrap_or_default(), "comment": comment,
                    "temporary": temporary, "options": opts,
                });
                self.write_definition(&path, &def)?;
                if temporary {
                    self.temporary_files.push(path);
                }
                status(format!("File format {format} successfully created."))
            }
            StageCommand::AlterFileFormat { name, if_exists, rename, set } => {
                let (db, schema, format) = self.qualify(&name)?;
                let path = self.format_path(&db, &schema, &format);
                let Some(mut def) = read_definition(&path) else {
                    if if_exists {
                        return status("Statement executed successfully.".into());
                    }
                    return Err(EngineError::does_not_exist("File format", &format!("{db}.{schema}.{format}")));
                };
                let mut opts = files::options_json(&set);
                if let Some(comment) = opts.remove("COMMENT") {
                    def["comment"] = comment;
                }
                if let Some(Value::Object(existing)) = def.get_mut("options") {
                    existing.extend(opts);
                }
                match rename {
                    Some(new_name) => {
                        let (ndb, nschema, nformat) = self.qualify(&new_name)?;
                        def["name"] = json!(nformat);
                        self.write_definition(&self.format_path(&ndb, &nschema, &nformat), &def)?;
                        let _ = std::fs::remove_file(&path);
                    }
                    None => self.write_definition(&path, &def)?,
                }
                status("Statement executed successfully.".into())
            }
            StageCommand::DropFileFormat { name, if_exists } => {
                let (db, schema, format) = self.qualify(&name)?;
                let path = self.format_path(&db, &schema, &format);
                if !path.exists() {
                    if if_exists {
                        return status(format!("Drop statement executed successfully ({format} already dropped)."));
                    }
                    return Err(EngineError::does_not_exist("File format", &format!("{db}.{schema}.{format}")));
                }
                let _ = std::fs::remove_file(path);
                status(format!("{format} successfully dropped."))
            }
            StageCommand::ShowStages(show) => self.show_stages(&show, query_id),
            StageCommand::ShowFileFormats(show) => self.show_file_formats(&show, query_id),
            StageCommand::DescribeStage(name) => {
                let (db, schema, stage) = self.qualify(&name)?;
                let def = read_definition(&self.stage_definition_path(&db, &schema, &stage))
                    .ok_or_else(|| EngineError::does_not_exist("Stage", &format!("{db}.{schema}.{stage}")))?;
                let mut format = Format::default();
                format.apply(&self.resolve_format_options(object_map(&def["file_format"]))?);
                let mut rows: Vec<Vec<Cell>> = format
                    .described()
                    .into_iter()
                    .map(|(property, value, default)| {
                        vec![text("STAGE_FILE_FORMAT"), text(property), text("String"), text(value), text(default)]
                    })
                    .collect();
                let copy_options = object_map(&def["copy_options"]);
                for (property, default) in [
                    ("ON_ERROR", "ABORT_STATEMENT"),
                    ("SIZE_LIMIT", ""),
                    ("PURGE", "false"),
                    ("RETURN_FAILED_ONLY", "false"),
                    ("ENFORCE_LENGTH", "true"),
                    ("TRUNCATECOLUMNS", "false"),
                    ("FORCE", "false"),
                ] {
                    let value = copy_options.get(property).and_then(Value::as_str).unwrap_or(default);
                    rows.push(vec![
                        text("STAGE_COPY_OPTIONS"),
                        text(property),
                        text("String"),
                        text(value),
                        text(default),
                    ]);
                }
                let url = def["url"].as_str().map(|u| format!("[\"{u}\"]")).unwrap_or_default();
                rows.push(vec![text("STAGE_LOCATION"), text("URL"), text("String"), text(url), text("")]);
                self.rows_result(
                    query_id,
                    &[
                        ("parent_property", "VARCHAR"),
                        ("property", "VARCHAR"),
                        ("property_type", "VARCHAR"),
                        ("property_value", "VARCHAR"),
                        ("property_default", "VARCHAR"),
                    ],
                    rows,
                    StatementKind::Show,
                )
            }
            StageCommand::DescribeFileFormat(name) => {
                let (db, schema, format_name) = self.qualify(&name)?;
                let def = read_definition(&self.format_path(&db, &schema, &format_name)).ok_or_else(|| {
                    EngineError::does_not_exist("File format", &format!("{db}.{schema}.{format_name}"))
                })?;
                let mut format = Format::default();
                format.apply(&object_map(&def["options"]));
                let rows = format
                    .described()
                    .into_iter()
                    .map(|(property, value, default)| vec![text(property), text("String"), text(value), text(default)])
                    .collect();
                self.rows_result(
                    query_id,
                    &[
                        ("property", "VARCHAR"),
                        ("property_type", "VARCHAR"),
                        ("property_value", "VARCHAR"),
                        ("property_default", "VARCHAR"),
                    ],
                    rows,
                    StatementKind::Show,
                )
            }
            StageCommand::List { location, pattern } => {
                let stage = self.resolve_stage(&location)?;
                let files = self.select_files(&stage, &location.path, &[], pattern.as_deref())?;
                let rows = files
                    .iter()
                    .map(|f| {
                        let modified = chrono::DateTime::<chrono::Utc>::from(f.modified);
                        vec![
                            text(format!("{}{}", stage.display, f.relative)),
                            Cell::Int(Some(f.size as i64)),
                            text(f.md5()),
                            text(modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string()),
                        ]
                    })
                    .collect();
                self.rows_result(
                    query_id,
                    &[("name", "VARCHAR"), ("size", "BIGINT"), ("md5", "VARCHAR"), ("last_modified", "VARCHAR")],
                    rows,
                    StatementKind::Show,
                )
            }
            StageCommand::Remove { location, pattern } => {
                let stage = self.resolve_stage(&location)?;
                let files = self.select_files(&stage, &location.path, &[], pattern.as_deref())?;
                let mut rows = vec![];
                for f in files {
                    std::fs::remove_file(&f.path).map_err(EngineError::internal)?;
                    rows.push(vec![text(format!("{}{}", stage.display, f.relative)), text("removed")]);
                }
                self.rows_result(query_id, &[("name", "VARCHAR"), ("result", "VARCHAR")], rows, StatementKind::Other)
            }
            StageCommand::Put { local, location, options } => {
                let stage = self.resolve_stage(&location)?;
                let base = stage.url_path.clone().ok_or_else(|| {
                    EngineError::load(
                        "002003",
                        format!("PUT is only supported for internal stages, not {}", stage.name),
                    )
                })?;
                let mut directory = location.path.trim_matches('/').to_owned();
                if !directory.is_empty() {
                    directory.push('/');
                }
                std::fs::create_dir_all(stage.dir.join(&directory)).map_err(EngineError::internal)?;
                let flag =
                    |key: &str, default: bool| option(&options, key).and_then(OptValue::as_bool).unwrap_or(default);
                let transfer = Transfer {
                    upload: true,
                    src_locations: vec![local],
                    stage_path: format!("{base}{directory}"),
                    local_location: None,
                    auto_compress: flag("AUTO_COMPRESS", true),
                    overwrite: flag("OVERWRITE", false),
                    source_compression: option(&options, "SOURCE_COMPRESSION")
                        .and_then(OptValue::text)
                        .unwrap_or("AUTO_DETECT")
                        .to_uppercase(),
                    parallel: option(&options, "PARALLEL")
                        .and_then(OptValue::text)
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(4),
                };
                Ok(self.transfer_result(query_id, transfer))
            }
            StageCommand::Get { location, local, options } => {
                let stage = self.resolve_stage(&location)?;
                let base = stage.url_path.clone().ok_or_else(|| {
                    EngineError::load(
                        "002003",
                        format!("GET is only supported for internal stages, not {}", stage.name),
                    )
                })?;
                let pattern = option(&options, "PATTERN").and_then(OptValue::text);
                let files = self.select_files(&stage, &location.path, &[], pattern)?;
                let mut local = local;
                if !local.ends_with('/') {
                    local.push('/');
                }
                let transfer = Transfer {
                    upload: false,
                    src_locations: files.into_iter().map(|f| f.relative).collect(),
                    stage_path: base,
                    local_location: Some(local),
                    auto_compress: false,
                    overwrite: true,
                    source_compression: "NONE".into(),
                    parallel: option(&options, "PARALLEL")
                        .and_then(OptValue::text)
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(10),
                };
                Ok(self.transfer_result(query_id, transfer))
            }
            StageCommand::CopyIntoTable { table, columns, source, options } => {
                self.copy_into_table(&table, &columns, source, &options, query_id)
            }
            StageCommand::CopyIntoLocation { location, query, options } => {
                self.copy_into_location(&location, &query, &options, query_id)
            }
        }
    }

    fn transfer_result(&self, query_id: &str, transfer: Transfer) -> QueryResult {
        let mut result =
            QueryResult::status(query_id.to_owned(), "Statement executed successfully.", StatementKind::Other);
        result.transfer = Some(transfer);
        result
    }

    /// `[db.][schema.]name` with the session's current database and schema.
    fn qualify(&self, name: &[String]) -> Result<(String, String, String), EngineError> {
        let db = || self.ctx.database.clone().ok_or_else(EngineError::no_current_database);
        let schema = || self.ctx.schema.clone().ok_or_else(EngineError::no_current_database);
        Ok(match name {
            [object] => (db()?, schema()?, object.clone()),
            [s, object] => (db()?, s.clone(), object.clone()),
            [d, s, object] => (d.clone(), s.clone(), object.clone()),
            _ => return Err(EngineError::load("002003", format!("Invalid object name '{}'", name.join(".")))),
        })
    }

    fn stages(&self) -> &StageStore {
        &self.engine.inner.stages
    }

    fn stage_definition_path(&self, db: &str, schema: &str, stage: &str) -> PathBuf {
        self.stages().schema_dir(db, schema).join("stages").join(format!("{}.json", fs_name(stage)))
    }

    fn format_path(&self, db: &str, schema: &str, format: &str) -> PathBuf {
        self.stages().schema_dir(db, schema).join("formats").join(format!("{}.json", fs_name(format)))
    }

    fn write_definition(&self, path: &Path, def: &Value) -> Result<(), EngineError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(EngineError::internal)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(def).unwrap()).map_err(EngineError::internal)
    }

    fn remove_stage(&self, db: &str, schema: &str, stage: &str) {
        let definition = self.stage_definition_path(db, schema, stage);
        let _ = std::fs::remove_file(&definition);
        let _ = std::fs::remove_dir_all(definition.with_extension(""));
    }

    fn apply_stage_options(&self, def: &mut Value, options: &Options) -> Result<(), EngineError> {
        for (key, value) in options {
            match (key.as_str(), value) {
                ("URL", v) => {
                    let url = v.text().unwrap_or_default().to_owned();
                    def["url"] = json!(url);
                }
                ("COMMENT", v) => def["comment"] = json!(v.text()),
                ("FILE_FORMAT", OptValue::Nested(nested)) => def["file_format"] = json!(files::options_json(nested)),
                ("FILE_FORMAT", v) => def["file_format"] = json!({ "FORMAT_NAME": v.text() }),
                ("COPY_OPTIONS", OptValue::Nested(nested)) => def["copy_options"] = json!(files::options_json(nested)),
                // Credentials, encryption, directory tables and integrations have no effect locally.
                _ => {}
            }
        }
        Ok(())
    }

    /// Replaces `FORMAT_NAME` with the named format's options.
    fn resolve_format_options(&self, options: BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, EngineError> {
        let Some(name) = options.get("FORMAT_NAME").and_then(Value::as_str) else { return Ok(options) };
        let parts = split_name(name);
        let (db, schema, format) = self.qualify(&parts)?;
        let def = read_definition(&self.format_path(&db, &schema, &format))
            .ok_or_else(|| EngineError::does_not_exist("File format", &format!("{db}.{schema}.{format}")))?;
        let mut resolved = object_map(&def["options"]);
        resolved.extend(options.into_iter().filter(|(k, _)| k != "FORMAT_NAME"));
        Ok(resolved)
    }

    /// The format for reading a stage: the stage's own format overlaid with
    /// the statement's FILE_FORMAT (inline options or a format name).
    fn format_for(&self, stage: &Stage, statement: Option<&OptValue>) -> Result<Format, EngineError> {
        let mut format = Format::default();
        format.apply(&self.resolve_format_options(stage.file_format.clone())?);
        match statement {
            Some(OptValue::Nested(nested)) => {
                let options = self.resolve_format_options(files::options_json(nested))?;
                if options.contains_key("TYPE") {
                    format = Format::default();
                }
                format.apply(&options);
            }
            Some(other) => {
                let mut options = BTreeMap::new();
                options.insert("FORMAT_NAME".to_owned(), json!(other.text()));
                format = Format::default();
                format.apply(&self.resolve_format_options(options)?);
            }
            None => {}
        }
        Ok(format)
    }

    fn resolve_stage(&self, location: &Location) -> Result<Stage, EngineError> {
        let root = &self.stages().root;
        match &location.stage {
            StageName::User => {
                let user = fs_name(if self.ctx.user.is_empty() { "USER" } else { &self.ctx.user });
                let relative = format!("users/{user}/");
                Ok(Stage {
                    dir: root.join(&relative),
                    url_path: Some(relative),
                    display: String::new(),
                    file_format: BTreeMap::new(),
                    copy_options: BTreeMap::new(),
                    name: "@~".into(),
                })
            }
            StageName::Table(name) => {
                let (db, schema, table) = self.qualify(name)?;
                if self.table_oid(&db, &schema, &table)?.is_none() {
                    return Err(EngineError::does_not_exist("Table", &format!("{db}.{schema}.{table}")));
                }
                let relative = format!("db/{}/{}/tables/{}/", fs_name(&db), fs_name(&schema), fs_name(&table));
                Ok(Stage {
                    dir: root.join(&relative),
                    url_path: Some(relative),
                    display: String::new(),
                    file_format: BTreeMap::new(),
                    copy_options: BTreeMap::new(),
                    name: format!("@%{table}"),
                })
            }
            StageName::Named(name) => {
                let (db, schema, stage) = self.qualify(name)?;
                let definition = self.stage_definition_path(&db, &schema, &stage);
                let full = format!("{db}.{schema}.{stage}");
                let def = read_definition(&definition).ok_or_else(|| EngineError::does_not_exist("Stage", &full))?;
                let (dir, url_path) = match def["url"].as_str().filter(|u| !u.is_empty()) {
                    // A localflake extension: an external stage over a local directory.
                    Some(url) if url.to_lowercase().starts_with("file://") => (PathBuf::from(&url[7..]), None),
                    Some(url) => {
                        return Err(EngineError::load(
                            "091003",
                            format!(
                                "localflake cannot access the external location '{url}' of stage {full}; use an internal stage or a file:// URL"
                            ),
                        ));
                    }
                    None => {
                        let relative = format!("db/{}/{}/stages/{}/", fs_name(&db), fs_name(&schema), fs_name(&stage));
                        (root.join(&relative), Some(relative))
                    }
                };
                Ok(Stage {
                    dir,
                    url_path,
                    display: format!("{}/", stage.to_lowercase()),
                    file_format: object_map(&def["file_format"]),
                    copy_options: object_map(&def["copy_options"]),
                    name: full,
                })
            }
        }
    }

    /// Files under `path` in a stage, restricted to FILES (relative to `path`) or a PATTERN.
    fn select_files(
        &self,
        stage: &Stage,
        path: &str,
        names: &[String],
        pattern: Option<&str>,
    ) -> Result<Vec<StagedFile>, EngineError> {
        let all = walk(&stage.dir);
        if !names.is_empty() {
            let mut prefix = path.to_owned();
            if !prefix.is_empty() && !prefix.ends_with('/') {
                prefix.push('/');
            }
            let wanted: Vec<String> = names.iter().map(|n| format!("{prefix}{}", n.trim_start_matches('/'))).collect();
            let mut selected = vec![];
            for name in &wanted {
                match all.iter().position(|f| &f.relative == name) {
                    Some(i) => selected.push(i),
                    None => {
                        return Err(EngineError::load(
                            "100067",
                            format!(
                                "Remote file '{}{name}' was not found. There are several potential causes. The file might not exist. The required credentials may be missing or invalid.",
                                stage.display
                            ),
                        ));
                    }
                }
            }
            let mut all: Vec<Option<StagedFile>> = all.into_iter().map(Some).collect();
            return Ok(selected.into_iter().filter_map(|i| all[i].take()).collect());
        }
        let regex = pattern.map(anchored).transpose()?;
        Ok(all
            .into_iter()
            .filter(|f| f.relative.starts_with(path))
            .filter(|f| {
                regex
                    .as_ref()
                    .is_none_or(|r| r.is_match(&f.relative) || r.is_match(&format!("{}{}", stage.display, f.relative)))
            })
            .collect())
    }

    fn table_oid(&self, db: &str, schema: &str, table: &str) -> Result<Option<i64>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT table_oid FROM sf_tables() WHERE database_name = ? AND schema_name = ? AND table_name = ?",
        )?;
        let mut rows = stmt.query([db, schema, table])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    /// Builds a result from rows of literals by running it as a query.
    fn rows_result(
        &self,
        query_id: &str,
        columns: &[(&str, &str)],
        rows: Vec<Vec<Cell>>,
        kind: StatementKind,
    ) -> Result<QueryResult, EngineError> {
        let names: Vec<String> = columns.iter().map(|(n, _)| crate::ident(n)).collect();
        let sql = if rows.is_empty() {
            let cols: Vec<String> =
                columns.iter().map(|(n, t)| format!("CAST(NULL AS {t}) AS {}", crate::ident(n))).collect();
            format!("SELECT {} WHERE false", cols.join(", "))
        } else {
            let values: Vec<String> = rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> =
                        row.iter().zip(columns).map(|(c, (_, t))| format!("CAST({} AS {t})", c.sql())).collect();
                    format!("({})", cells.join(", "))
                })
                .collect();
            format!("SELECT * FROM (VALUES {}) AS __lf_rows({})", values.join(", "), names.join(", "))
        };
        self.run_query(&sql, &[], false, query_id, kind)
    }

    fn show_stages(&self, show: &Show, query_id: &str) -> Result<QueryResult, EngineError> {
        let mut rows = vec![];
        for (def, _) in self.scoped_definitions(show, "stages")? {
            let url = def["url"].as_str().unwrap_or_default().to_owned();
            let kind = match (url.is_empty(), def["temporary"].as_bool().unwrap_or(false)) {
                (false, _) => "EXTERNAL",
                (true, true) => "INTERNAL TEMPORARY",
                (true, false) => "INTERNAL",
            };
            rows.push(vec![
                Cell::Time(def["created_on"].as_i64().unwrap_or(0)),
                text(def["name"].as_str().unwrap_or_default()),
                text(def["database"].as_str().unwrap_or_default()),
                text(def["schema"].as_str().unwrap_or_default()),
                text(url.clone()),
                text("N"),
                text("N"),
                text(def["owner"].as_str().unwrap_or_default()),
                text(def["comment"].as_str().unwrap_or_default()),
                Cell::Text(None),
                text(kind),
                Cell::Text(if url.is_empty() { None } else { Some("LOCAL".into()) }),
                Cell::Text(None),
                Cell::Text(None),
                Cell::Text(None),
                text("ROLE"),
                Cell::Bool(false),
            ]);
        }
        self.rows_result(
            query_id,
            &[
                ("created_on", "TIMESTAMPTZ"),
                ("name", "VARCHAR"),
                ("database_name", "VARCHAR"),
                ("schema_name", "VARCHAR"),
                ("url", "VARCHAR"),
                ("has_credentials", "VARCHAR"),
                ("has_encryption_key", "VARCHAR"),
                ("owner", "VARCHAR"),
                ("comment", "VARCHAR"),
                ("region", "VARCHAR"),
                ("type", "VARCHAR"),
                ("cloud", "VARCHAR"),
                ("notification_channel", "VARCHAR"),
                ("storage_integration", "VARCHAR"),
                ("endpoint", "VARCHAR"),
                ("owner_role_type", "VARCHAR"),
                ("directory_enabled", "BOOLEAN"),
            ],
            rows,
            StatementKind::Show,
        )
    }

    fn show_file_formats(&self, show: &Show, query_id: &str) -> Result<QueryResult, EngineError> {
        let mut rows = vec![];
        for (def, _) in self.scoped_definitions(show, "formats")? {
            let options = def["options"].clone();
            rows.push(vec![
                Cell::Time(def["created_on"].as_i64().unwrap_or(0)),
                text(def["name"].as_str().unwrap_or_default()),
                text(def["database"].as_str().unwrap_or_default()),
                text(def["schema"].as_str().unwrap_or_default()),
                text(options["TYPE"].as_str().unwrap_or("CSV")),
                text(def["owner"].as_str().unwrap_or_default()),
                text(def["comment"].as_str().unwrap_or_default()),
                text(options.to_string()),
                text("ROLE"),
            ]);
        }
        self.rows_result(
            query_id,
            &[
                ("created_on", "TIMESTAMPTZ"),
                ("name", "VARCHAR"),
                ("database_name", "VARCHAR"),
                ("schema_name", "VARCHAR"),
                ("type", "VARCHAR"),
                ("owner", "VARCHAR"),
                ("comment", "VARCHAR"),
                ("format_options", "VARCHAR"),
                ("owner_role_type", "VARCHAR"),
            ],
            rows,
            StatementKind::Show,
        )
    }

    /// Stage or file format definitions within a SHOW command's scope.
    fn scoped_definitions(&self, show: &Show, kind: &str) -> Result<Vec<(Value, PathBuf)>, EngineError> {
        let (db, schema): (Option<String>, Option<String>) = match &show.scope {
            Scope::Account => (None, None),
            Scope::Database(name) => (name.clone().or_else(|| self.ctx.database.clone()), None),
            Scope::Schema(Some(name)) | Scope::Unqualified(name) => match name.as_slice() {
                [s] => (self.ctx.database.clone(), Some(s.clone())),
                [d, s, ..] => (Some(d.clone()), Some(s.clone())),
                [] => (self.ctx.database.clone(), self.ctx.schema.clone()),
            },
            Scope::Schema(None) | Scope::Default | Scope::Table(_) => {
                (self.ctx.database.clone(), self.ctx.schema.clone())
            }
        };
        let mut found = vec![];
        let databases = std::fs::read_dir(self.stages().root.join("db")).into_iter().flatten().flatten();
        for database in databases {
            let db_name = fs_unname(&database.file_name().to_string_lossy());
            if db.as_ref().is_some_and(|d| d != &db_name) {
                continue;
            }
            for schema_dir in std::fs::read_dir(database.path()).into_iter().flatten().flatten() {
                let schema_name = fs_unname(&schema_dir.file_name().to_string_lossy());
                if schema.as_ref().is_some_and(|s| s != &schema_name) {
                    continue;
                }
                for entry in std::fs::read_dir(schema_dir.path().join(kind)).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.extension().is_none_or(|e| e != "json") {
                        continue;
                    }
                    let Some(def) = read_definition(&path) else { continue };
                    let name = def["name"].as_str().unwrap_or_default();
                    if show.like.as_ref().is_some_and(|p| !files::like(p, name)) {
                        continue;
                    }
                    if show.starts_with.as_ref().is_some_and(|p| !name.starts_with(p.as_str())) {
                        continue;
                    }
                    found.push((def, path));
                }
            }
        }
        found.sort_by(|a, b| a.0["name"].as_str().cmp(&b.0["name"].as_str()));
        if let Some(limit) = show.limit {
            found.truncate(limit as usize);
        }
        Ok(found)
    }

    /// Reads staged files into rows, per the format.
    fn read_files(&self, stage: &Stage, files: Vec<StagedFile>, format: &Format) -> Result<Vec<FileRows>, EngineError> {
        let kind = format.kind();
        let mut out = vec![];
        for file in files {
            let display = format!("{}{}", stage.display, file.relative);
            let records = match kind.as_str() {
                "CSV" => {
                    let bytes = std::fs::read(&file.path).map_err(EngineError::internal)?;
                    let bytes = files::decompress(bytes, &format.text("COMPRESSION").unwrap_or_default(), &display)?;
                    let text = files::decode_text(bytes, format);
                    let skip = format.text("SKIP_HEADER").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0)
                        + usize::from(format.flag("PARSE_HEADER"));
                    files::parse_csv(&text, format).into_iter().skip(skip).collect()
                }
                "JSON" => {
                    let bytes = std::fs::read(&file.path).map_err(EngineError::internal)?;
                    let bytes = files::decompress(bytes, &format.text("COMPRESSION").unwrap_or_default(), &display)?;
                    let text = files::decode_text(bytes, format);
                    files::parse_json_documents(&text, format, &display)?
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| Record { line: i + 1, fields: vec![Some(v.to_string())] })
                        .collect()
                }
                "PARQUET" => {
                    let mut stmt =
                        self.conn.prepare("SELECT CAST(to_json(__t) AS VARCHAR) FROM read_parquet(?) AS __t")?;
                    let rows = stmt.query_map([file.path.to_string_lossy().as_ref()], |r| r.get::<_, String>(0))?;
                    let mut records = vec![];
                    for (i, row) in rows.enumerate() {
                        records.push(Record { line: i + 1, fields: vec![Some(row?)] });
                    }
                    records
                }
                other => {
                    return Err(EngineError::load(
                        "002040",
                        format!("localflake does not support the {other} file format"),
                    ));
                }
            };
            out.push(FileRows { file, display, records, errors: HashMap::new() });
        }
        Ok(out)
    }

    /// Creates the temp table holding staged rows.
    fn create_load_table(&mut self, files: &[FileRows], columns: usize, json: bool) -> Result<LoadTable, EngineError> {
        self.load_tables += 1;
        let name = format!("__LF_STAGE_{}", self.load_tables);
        let value_columns: Vec<String> = (1..=columns).map(|i| format!("COLUMN{i} VARCHAR")).collect();
        self.conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE {name} ({}{LOAD_COLUMNS})",
            value_columns.iter().map(|c| format!("{c}, ")).collect::<String>()
        ))?;
        let scan_time = chrono::Utc::now().timestamp_micros();
        let mut appender = self.conn.appender_to_db(&name, "main")?;
        let mut row_id = 0i64;
        for (file_index, file) in files.iter().enumerate() {
            let modified = chrono::DateTime::<chrono::Utc>::from(file.file.modified).timestamp_micros();
            let content_key = format!("{}-{}", file.file.size, modified);
            for (i, record) in file.records.iter().enumerate() {
                use duckdb::types::{TimeUnit, Value as V};
                row_id += 1;
                let mut values: Vec<V> = (0..columns)
                    .map(|c| match record.fields.get(c).cloned().flatten() {
                        Some(s) => V::Text(s),
                        None => V::Null,
                    })
                    .collect();
                let (error, code) = match file.errors.get(&i) {
                    Some((message, code)) => (V::Text(message.clone()), V::Text((*code).to_owned())),
                    None => (V::Null, V::Null),
                };
                values.extend([
                    V::Text(file.file.relative.clone()),
                    V::BigInt(i as i64 + 1),
                    V::Text(content_key.clone()),
                    V::Timestamp(TimeUnit::Microsecond, modified),
                    V::Timestamp(TimeUnit::Microsecond, scan_time),
                    V::BigInt(file_index as i64),
                    V::BigInt(row_id),
                    V::BigInt(record.line as i64),
                    error,
                    code,
                    V::Null,
                ]);
                appender.append_row(duckdb::appender_params_from_iter(values))?;
            }
        }
        appender.flush()?;
        drop(appender);
        self.load_table_names.push(name.clone());
        Ok(LoadTable { name, columns, json })
    }

    /// Drops the load tables a statement created.
    pub(crate) fn drop_load_tables(&mut self) {
        for name in std::mem::take(&mut self.load_table_names) {
            let _ = self.conn.execute_batch(&format!("DROP TABLE IF EXISTS temp.main.{name}"));
        }
    }

    /// The Snowflake SQL relation for a load table: text columns as they are,
    /// JSON documents parsed into VARIANT.
    fn load_relation(table: &LoadTable) -> String {
        if !table.json {
            return table.name.clone();
        }
        let values: Vec<String> = (1..=table.columns).map(|i| format!("PARSE_JSON(COLUMN{i}) AS COLUMN{i}")).collect();
        format!(
            "(SELECT {}, METADATA$FILENAME, METADATA$FILE_ROW_NUMBER, METADATA$FILE_CONTENT_KEY, \
             METADATA$FILE_LAST_MODIFIED, METADATA$START_SCAN_TIME FROM {})",
            values.join(", "),
            table.name
        )
    }

    /// Replaces stage references in a query with load tables of the staged
    /// files. `copy` supplies COPY's FILES, PATTERN and FILE_FORMAT.
    pub(crate) fn expand_stage_references(&mut self, sql: &str, copy: Option<&Options>) -> Result<String, EngineError> {
        let references = localflake_sql::stage::references(sql)?;
        if references.is_empty() {
            return Ok(sql.to_owned());
        }
        // `$7` reads the seventh field, so the load table needs that many columns.
        let max_position = regex::Regex::new(r"\$(\d+)")
            .unwrap()
            .captures_iter(sql)
            .filter_map(|c| c[1].parse::<usize>().ok())
            .max()
            .unwrap_or(1);
        let mut out = sql.to_owned();
        for reference in references.iter().rev() {
            let stage = self.resolve_stage(&reference.location)?;
            let file_format =
                copy.and_then(|o| option(o, "FILE_FORMAT")).or_else(|| option(&reference.options, "FILE_FORMAT"));
            let format = self.format_for(&stage, file_format)?;
            let names = copy.and_then(|o| option(o, "FILES")).map(OptValue::texts).unwrap_or_default();
            let pattern = copy
                .and_then(|o| option(o, "PATTERN"))
                .or_else(|| option(&reference.options, "PATTERN"))
                .and_then(OptValue::text);
            let selected = self.select_files(&stage, &reference.location.path, &names, pattern)?;
            let rows = self.read_files(&stage, selected, &format)?;
            let json = format.kind() != "CSV";
            let widest = rows.iter().flat_map(|f| f.records.iter().map(|r| r.fields.len())).max().unwrap_or(1);
            let columns = if json { 1 } else { widest.max(max_position) };
            let table = self.create_load_table(&rows, columns, json)?;
            out.replace_range(reference.start..reference.end, &Self::load_relation(&table));
        }
        Ok(out)
    }

    /// Translates generated Snowflake SQL and runs it, returning the affected row count.
    fn execute_generated(&mut self, sql: &str) -> Result<usize, EngineError> {
        let Command::Sql(command) = self.translate(sql)? else {
            return Err(EngineError::internal(format!("unexpected command for generated SQL: {sql}")));
        };
        for pre in &command.pre {
            self.conn.execute_batch(pre)?;
        }
        Ok(self.conn.execute(&command.sql, [])?)
    }

    /// Translates a generated Snowflake query and reads every value as text.
    fn query_generated(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, EngineError> {
        let Command::Sql(command) = self.translate(sql)? else {
            return Err(EngineError::internal(format!("unexpected command for generated SQL: {sql}")));
        };
        for pre in &command.pre {
            self.conn.execute_batch(pre)?;
        }
        let mut stmt = self.conn.prepare(&command.sql)?;
        let mut rows = stmt.query([])?;
        let mut out = vec![];
        while let Some(row) = rows.next()? {
            let n = row.as_ref().column_count();
            out.push((0..n).map(|i| row.get::<_, Option<String>>(i)).collect::<Result<_, _>>()?);
        }
        Ok(out)
    }

    /// `(name, declared type, nullable)` of a table's columns.
    fn table_columns(&self, db: &str, schema: &str, table: &str) -> Result<Vec<(String, String, bool)>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT column_name, sf_type_desc(data_type, numeric_precision, numeric_scale), is_nullable FROM sf_columns() \
             WHERE database_name = ? AND schema_name = ? AND table_name = ? ORDER BY column_index",
        )?;
        let rows = stmt.query_map([db, schema, table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn copy_into_table(
        &mut self,
        table: &[String],
        columns: &[String],
        source: CopySource,
        options: &Options,
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        let (db, schema, table_name) = self.qualify(table)?;
        let full_name = format!("{db}.{schema}.{table_name}");
        let oid = self
            .table_oid(&db, &schema, &table_name)?
            .ok_or_else(|| EngineError::does_not_exist("Table", &full_name))?;
        let table_columns = self.table_columns(&db, &schema, &table_name)?;
        let targets: Vec<(String, String, bool)> = if columns.is_empty() {
            table_columns.clone()
        } else {
            columns
                .iter()
                .map(|c| {
                    table_columns.iter().find(|(n, _, _)| n == c).cloned().ok_or_else(|| {
                        EngineError::load(
                            "000904",
                            format!("SQL compilation error: error line 1 at position 0\ninvalid identifier '{c}'"),
                        )
                    })
                })
                .collect::<Result<_, _>>()?
        };

        // The stage and its options.
        let (location, transform) = match &source {
            CopySource::Location(location) => (location.clone(), None),
            CopySource::Query(sql) => {
                let references = localflake_sql::stage::references(sql)?;
                let [reference] = references.as_slice() else {
                    return Err(EngineError::load("002040", "COPY transformations must read exactly one stage"));
                };
                (reference.location.clone(), Some(sql.clone()))
            }
        };
        let stage = self.resolve_stage(&location)?;
        let mut options = options.clone();
        for (key, value) in &stage.copy_options {
            if option(&options, key).is_none()
                && let Some(text) = value.as_str()
            {
                options.insert(0, (key.clone(), OptValue::Word(text.to_owned())));
            }
        }
        let flag = |key: &str| option(&options, key).and_then(OptValue::as_bool).unwrap_or(false);
        let format = self.format_for(&stage, option(&options, "FILE_FORMAT"))?;
        let names = option(&options, "FILES").map(OptValue::texts).unwrap_or_default();
        let pattern = option(&options, "PATTERN").and_then(OptValue::text);
        let mut selected = self.select_files(&stage, &location.path, &names, pattern)?;

        // Files already loaded into this table (and unchanged) are skipped.
        let table_key = format!("{full_name}#{oid}");
        let loaded_before = self.stages().load_history.lock().unwrap().get(&table_key).cloned().unwrap_or_default();
        if !flag("FORCE") && !flag("LOAD_UNCERTAIN_FILES") {
            selected.retain(|f| loaded_before.get(&f.relative).is_none_or(|md5| *md5 != f.md5()));
        }
        if selected.is_empty() {
            return Ok(QueryResult::status(
                query_id.to_owned(),
                "Copy executed with 0 files processed.",
                StatementKind::Other,
            ));
        }

        let kind = format.kind();
        let json = kind != "CSV";
        let match_mode = option(&options, "MATCH_BY_COLUMN_NAME")
            .and_then(OptValue::text)
            .map(str::to_uppercase)
            .filter(|m| m != "NONE");
        let mut files = self.read_files(&stage, selected, &format)?;
        if transform.is_none() {
            if let Some(mode) = &match_mode {
                let header = if kind == "CSV" { self.csv_headers(&files, &format)? } else { vec![] };
                for (i, file) in files.iter_mut().enumerate() {
                    map_columns_by_name(file, &targets, mode, header.get(i));
                }
            } else if json && targets.len() != 1 {
                return Err(EngineError::load(
                    "002019",
                    format!(
                        "SQL compilation error:\n{kind} file format can produce one and only one column of type variant, object, or array. Load data into separate columns using the MATCH_BY_COLUMN_NAME copy option or copy with transformation."
                    ),
                ));
            } else if !json
                && format.text("ERROR_ON_COLUMN_COUNT_MISMATCH").is_none_or(|v| !v.eq_ignore_ascii_case("false"))
            {
                for file in &mut files {
                    for (i, record) in file.records.iter().enumerate() {
                        if record.fields.len() != targets.len() {
                            let message = format!(
                                "Number of columns in file ({}) does not match that of the corresponding table ({}), use file format option error_on_column_count_mismatch=false to ignore this error",
                                record.fields.len(),
                                targets.len()
                            );
                            file.errors.insert(i, (message, "100080"));
                        }
                    }
                }
            }
        }
        // MATCH_BY_COLUMN_NAME turns every value into JSON text.
        let values_are_json = json || match_mode.is_some();
        let width = if transform.is_some() {
            files.iter().flat_map(|f| f.records.iter().map(|r| r.fields.len())).max().unwrap_or(1)
        } else {
            targets.len()
        };
        let load = self.create_load_table(&files, width.max(1), json && match_mode.is_none())?;

        // The SELECT producing target rows from the load table.
        let truncate =
            flag("TRUNCATECOLUMNS") || option(&options, "ENFORCE_LENGTH").and_then(OptValue::as_bool) == Some(false);
        let conversions: Vec<String>;
        let source_sql = match &transform {
            None => {
                conversions = targets
                    .iter()
                    .enumerate()
                    .map(|(i, (_, ty, _))| {
                        conversion(&format!("COLUMN{}", i + 1), ty, values_are_json, &format, false, truncate)
                    })
                    .collect();
                load.name.clone()
            }
            Some(sql) => {
                let expanded = self.expand_query_over(sql, &load)?;
                let aliases: Vec<String> = (1..=targets.len()).map(|i| format!("COLUMN{i}")).collect();
                conversions = targets
                    .iter()
                    .enumerate()
                    .map(|(i, (_, ty, _))| format!("CAST(COLUMN{} AS {ty})", i + 1))
                    .collect();
                format!("({expanded}) AS __LF_COPY({})", aliases.join(", "))
            }
        };

        // Row errors: values that don't convert, and NULLs in NOT NULL columns.
        if transform.is_none() {
            for (i, (name, ty, nullable)) in targets.iter().enumerate() {
                let input = format!("COLUMN{}", i + 1);
                let attempt = conversion(&input, ty, values_are_json, &format, true, truncate);
                let mut failed = format!("({input} IS NOT NULL AND {attempt} IS NULL)");
                if !nullable {
                    failed = format!("({failed} OR {input} IS NULL)");
                }
                self.execute_generated(&format!(
                    "UPDATE {} SET __LF_ERROR_COLUMN = {} WHERE __LF_ERROR IS NULL AND __LF_ERROR_COLUMN IS NULL AND {failed}",
                    load.name,
                    literal(name)
                ))?;
            }
        }

        // Per file: rows, errors and the first error.
        let on_error =
            option(&options, "ON_ERROR").and_then(OptValue::text).unwrap_or("ABORT_STATEMENT").to_uppercase();
        let mut stats = vec![];
        for (index, file) in files.iter().enumerate() {
            let (rows, errors): (i64, i64) = self.conn.query_row(
                &format!(
                    "SELECT count(*), count(*) FILTER (WHERE __LF_ERROR IS NOT NULL OR __LF_ERROR_COLUMN IS NOT NULL) FROM temp.main.{} WHERE __LF_FILE = ?",
                    load.name
                ),
                [index as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let first = if errors > 0 {
                Some(self.first_load_error(&load, index, &targets, &values_are_json, &format, truncate)?)
            } else {
                None
            };
            stats.push((file.display.clone(), rows, errors, first));
        }

        let validation = option(&options, "VALIDATION_MODE").and_then(OptValue::text).map(str::to_uppercase);
        if let Some(mode) = &validation {
            return self.validation_result(
                query_id,
                mode,
                &load,
                &conversions,
                &targets,
                &source_sql,
                &stats,
                &table_name,
            );
        }

        // Decide which files load.
        let mut skipped_files = vec![];
        for (index, (_, rows, errors, first)) in stats.iter().enumerate() {
            if *errors == 0 {
                continue;
            }
            let skip = match on_error.as_str() {
                "CONTINUE" => *errors == *rows,
                "SKIP_FILE" => true,
                other if other.starts_with("SKIP_FILE_") => {
                    let limit = &other["SKIP_FILE_".len()..];
                    match limit.strip_suffix('%') {
                        Some(pct) => {
                            (*errors as f64) * 100.0 / (*rows).max(1) as f64 >= pct.parse::<f64>().unwrap_or(0.0)
                        }
                        None => *errors >= limit.parse::<i64>().unwrap_or(1),
                    }
                }
                _ => {
                    let error = first.clone().unwrap();
                    return Err(EngineError::load(
                        error.code,
                        format!(
                            "{}\n  File '{}', line {}, character {}\n  Row {} starts at line {}, column \"{}\"[\"{}\":{}]\n  If you would like to continue loading when an error is encountered, use other values such as 'SKIP_FILE' or 'CONTINUE' for the ON_ERROR option. For more information on loading options, please run 'info loading_data' in a SQL client.",
                            error.message,
                            stats[index].0,
                            error.line,
                            error.character,
                            error.row,
                            error.line,
                            table_name,
                            error.column.as_deref().unwrap_or(""),
                            error.column_index,
                        ),
                    ));
                }
            };
            if skip {
                skipped_files.push(index as i64);
            }
        }

        // Insert the good rows.
        let column_list: Vec<String> = targets.iter().map(|(n, _, _)| q(n)).collect();
        let filter = if transform.is_none() {
            let mut filter = "WHERE __LF_ERROR IS NULL AND __LF_ERROR_COLUMN IS NULL".to_owned();
            if !skipped_files.is_empty() {
                let list: Vec<String> = skipped_files.iter().map(i64::to_string).collect();
                filter.push_str(&format!(" AND __LF_FILE NOT IN ({})", list.join(", ")));
            }
            filter
        } else {
            String::new()
        };
        let insert = format!(
            "INSERT INTO {}.{}.{} ({}) SELECT {} FROM {source_sql} {filter}",
            q(&db),
            q(&schema),
            q(&table_name),
            column_list.join(", "),
            conversions.join(", ")
        );
        let own_transaction = !self.in_transaction;
        if own_transaction {
            self.conn.execute_batch("BEGIN TRANSACTION")?;
        }
        let outcome = self.execute_generated(&insert);
        match (&outcome, own_transaction) {
            (Ok(_), true) => self.conn.execute_batch("COMMIT")?,
            (Err(_), true) => self.conn.execute_batch("ROLLBACK")?,
            _ => {}
        }
        outcome?;

        // Results, load history and PURGE.
        let mut history = self.stages().load_history.lock().unwrap().remove(&table_key).unwrap_or_default();
        let purge = flag("PURGE");
        let mut rows = vec![];
        for (index, (display, parsed, errors, first)) in stats.into_iter().enumerate() {
            let skipped = skipped_files.contains(&(index as i64));
            let loaded = if skipped { 0 } else { parsed - errors };
            let status = if skipped || (parsed > 0 && loaded == 0 && errors > 0) {
                "LOAD_FAILED"
            } else if errors > 0 {
                "PARTIALLY_LOADED"
            } else {
                "LOADED"
            };
            if status != "LOAD_FAILED" {
                history.insert(files[index].file.relative.clone(), files[index].file.md5());
                if purge {
                    let _ = std::fs::remove_file(&files[index].file.path);
                }
            }
            let error_limit = match on_error.as_str() {
                "CONTINUE" => parsed,
                other if other.starts_with("SKIP_FILE_") && !other.ends_with('%') => {
                    other["SKIP_FILE_".len()..].parse().unwrap_or(1)
                }
                _ => 1,
            };
            rows.push(vec![
                text(display),
                text(status),
                Cell::Int(Some(parsed)),
                Cell::Int(Some(loaded)),
                Cell::Int(Some(error_limit)),
                Cell::Int(Some(errors)),
                Cell::Text(first.as_ref().map(|e| e.message.clone())),
                Cell::Int(first.as_ref().map(|e| e.line as i64)),
                Cell::Int(first.as_ref().map(|e| e.character as i64)),
                Cell::Text(
                    first.as_ref().and_then(|e| {
                        e.column.as_ref().map(|c| format!("\"{table_name}\"[\"{c}\":{}]", e.column_index))
                    }),
                ),
            ]);
        }
        self.stages().load_history.lock().unwrap().insert(table_key, history);
        self.rows_result(
            query_id,
            &[
                ("file", "VARCHAR"),
                ("status", "VARCHAR"),
                ("rows_parsed", "BIGINT"),
                ("rows_loaded", "BIGINT"),
                ("error_limit", "BIGINT"),
                ("errors_seen", "BIGINT"),
                ("first_error", "VARCHAR"),
                ("first_error_line", "BIGINT"),
                ("first_error_character", "BIGINT"),
                ("first_error_column_name", "VARCHAR"),
            ],
            rows,
            StatementKind::Other,
        )
    }

    /// Replaces the single stage reference of a COPY transformation with the load table.
    fn expand_query_over(&self, sql: &str, load: &LoadTable) -> Result<String, EngineError> {
        let references = localflake_sql::stage::references(sql)?;
        let mut out = sql.to_owned();
        for reference in references.iter().rev() {
            out.replace_range(reference.start..reference.end, &Self::load_relation(load));
        }
        Ok(out)
    }

    /// Header names of CSV files read with PARSE_HEADER, for MATCH_BY_COLUMN_NAME.
    fn csv_headers(&self, files: &[FileRows], format: &Format) -> Result<Vec<Vec<String>>, EngineError> {
        if !format.flag("PARSE_HEADER") {
            return Err(EngineError::load(
                "002019",
                "SQL compilation error:\nMATCH_BY_COLUMN_NAME with CSV requires the file format option PARSE_HEADER = TRUE",
            ));
        }
        let mut headers = vec![];
        for file in files {
            let bytes = std::fs::read(&file.file.path).map_err(EngineError::internal)?;
            let bytes = files::decompress(bytes, &format.text("COMPRESSION").unwrap_or_default(), &file.display)?;
            let text = files::decode_text(bytes, format);
            let header = files::parse_csv(&text, format).into_iter().next().map(|r| r.fields).unwrap_or_default();
            headers.push(header.into_iter().map(Option::unwrap_or_default).collect());
        }
        Ok(headers)
    }

    /// The first error in a file of the load table, with its message.
    fn first_load_error(
        &mut self,
        load: &LoadTable,
        file: usize,
        targets: &[(String, String, bool)],
        json: &bool,
        format: &Format,
        truncate: bool,
    ) -> Result<LoadError, EngineError> {
        let (row, line, error, code, column): (i64, i64, Option<String>, Option<String>, Option<String>) = self.conn.query_row(
            &format!(
                "SELECT \"METADATA$FILE_ROW_NUMBER\", __LF_LINE, __LF_ERROR, __LF_ERROR_CODE, __LF_ERROR_COLUMN FROM temp.main.{} \
                 WHERE __LF_FILE = ? AND (__LF_ERROR IS NOT NULL OR __LF_ERROR_COLUMN IS NOT NULL) ORDER BY __LF_ROW LIMIT 1",
                load.name
            ),
            [file as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        if let Some(message) = error {
            let code: &'static str = if code.as_deref() == Some("100080") { "100080" } else { "100069" };
            return Ok(LoadError {
                message,
                code,
                line: line as usize,
                character: 1,
                row,
                column: None,
                column_index: 0,
            });
        }
        let column = column.unwrap_or_default();
        let index = targets.iter().position(|(n, _, _)| *n == column).unwrap_or(0);
        let (_, ty, nullable) = &targets[index];
        let input = format!("COLUMN{}", index + 1);
        let probe = format!(
            "SELECT {} FROM {} WHERE __LF_FILE = {file} AND \"METADATA$FILE_ROW_NUMBER\" = {row}",
            conversion(&input, ty, *json, format, false, truncate),
            load.name
        );
        let (message, code) = match self.query_generated(&probe) {
            Err(err) => (err.message.lines().next().unwrap_or_default().to_owned(), err.code),
            Ok(_) if !nullable => ("NULL result in a non-nullable column".to_owned(), "100072"),
            Ok(_) => ("String is too long and would be truncated".to_owned(), "100074"),
        };
        Ok(LoadError {
            message,
            code,
            line: line as usize,
            character: 1,
            row,
            column: Some(column),
            column_index: index + 1,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn validation_result(
        &mut self,
        query_id: &str,
        mode: &str,
        load: &LoadTable,
        conversions: &[String],
        targets: &[(String, String, bool)],
        source_sql: &str,
        stats: &[(String, i64, i64, Option<LoadError>)],
        table_name: &str,
    ) -> Result<QueryResult, EngineError> {
        if let Some(n) = mode.strip_prefix("RETURN_").and_then(|m| m.strip_suffix("_ROWS")) {
            let limit = n.parse::<i64>().unwrap_or(0);
            if let Some((file, _, _, Some(error))) = stats.iter().find(|s| s.3.as_ref().is_some_and(|e| e.row <= limit))
            {
                return Err(EngineError::load(
                    error.code,
                    format!("{}\n  File '{file}', line {}", error.message, error.line),
                ));
            }
            let columns: Vec<String> =
                conversions.iter().zip(targets).map(|(c, (n, _, _))| format!("{c} AS {}", q(n))).collect();
            let sql = format!(
                "SELECT {} FROM {source_sql} WHERE __LF_ERROR IS NULL AND __LF_ERROR_COLUMN IS NULL ORDER BY __LF_ROW LIMIT {limit}",
                columns.join(", ")
            );
            let Command::Sql(command) = self.translate(&sql)? else { return Err(EngineError::internal("validation")) };
            return self.run_query(&command.sql, &[], false, query_id, StatementKind::Query);
        }
        let _ = load;
        // Only each file's first error is recorded, for RETURN_ERRORS and RETURN_ALL_ERRORS alike.
        let rows = stats
            .iter()
            .filter_map(|(file, _, _, error)| error.as_ref().map(|e| (file, e)))
            .map(|(file, e)| {
                vec![
                    text(e.message.clone()),
                    text(file.clone()),
                    Cell::Int(Some(e.line as i64)),
                    Cell::Int(Some(e.character as i64)),
                    Cell::Int(None),
                    text(if e.code == "100080" { "parsing" } else { "conversion" }),
                    Cell::Int(Some(e.code.parse().unwrap_or(0))),
                    text("22000"),
                    Cell::Text(e.column.as_ref().map(|c| format!("\"{table_name}\"[\"{c}\":{}]", e.column_index))),
                    Cell::Int(Some(e.row)),
                    Cell::Int(Some(e.line as i64)),
                    Cell::Text(None),
                ]
            })
            .collect();
        self.rows_result(
            query_id,
            &[
                ("ERROR", "VARCHAR"),
                ("FILE", "VARCHAR"),
                ("LINE", "BIGINT"),
                ("CHARACTER", "BIGINT"),
                ("BYTE_OFFSET", "BIGINT"),
                ("CATEGORY", "VARCHAR"),
                ("CODE", "BIGINT"),
                ("SQL_STATE", "VARCHAR"),
                ("COLUMN_NAME", "VARCHAR"),
                ("ROW_NUMBER", "BIGINT"),
                ("ROW_START_LINE", "BIGINT"),
                ("REJECTED_RECORD", "VARCHAR"),
            ],
            rows,
            StatementKind::Query,
        )
    }

    fn copy_into_location(
        &mut self,
        location: &Location,
        query: &str,
        options: &Options,
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        let stage = self.resolve_stage(location)?;
        let format = self.format_for(&stage, option(options, "FILE_FORMAT"))?;
        let flag = |key: &str| option(options, key).and_then(OptValue::as_bool).unwrap_or(false);
        if option(options, "PARTITION").is_some() {
            return Err(EngineError::load("002040", "localflake does not support PARTITION BY for unloading"));
        }
        let query = self.expand_stage_references(query, None)?;

        // Column names and types of the query.
        let Command::Sql(command) = self.translate(&query)? else {
            return Err(EngineError::load("002040", "COPY INTO a location needs a query"));
        };
        for pre in &command.pre {
            self.conn.execute_batch(pre)?;
        }
        let described = self.run_query(&command.sql, &[], true, query_id, StatementKind::Query)?;
        let names: Vec<String> = described.columns.iter().map(|c| c.name.clone()).collect();
        let types: Vec<SfType> = described.columns.iter().map(|c| c.sf_type).collect();
        let aliases: Vec<String> = (1..=names.len()).map(|i| format!("COLUMN{i}")).collect();
        let from = format!("({query}) AS __LF_UNLOAD({})", aliases.join(", "));
        let kind = format.kind();

        // File naming: SINGLE writes exactly the path; otherwise the path is a prefix.
        let single = flag("SINGLE");
        let compression = format.text("COMPRESSION").unwrap_or_else(|| "AUTO".into());
        let mut path = location.path.clone();
        let (content, rows): (Vec<u8>, usize) = match kind.as_str() {
            "CSV" | "JSON" => {
                let exprs: Vec<String> = if kind == "JSON" {
                    if names.len() != 1 || !types[0].is_semi_structured() {
                        return Err(EngineError::load(
                            "002019",
                            "SQL compilation error:\nJSON file format can produce one and only one column of type variant, object, or array. Use CSV file format if you want to load more than one column.",
                        ));
                    }
                    vec!["TO_JSON(COLUMN1)".into()]
                } else {
                    aliases
                        .iter()
                        .zip(&types)
                        .map(|(c, t)| {
                            let fmt = match t {
                                SfType::Date => format.explicit("DATE_FORMAT"),
                                SfType::Time => format.explicit("TIME_FORMAT"),
                                SfType::TimestampNtz | SfType::TimestampLtz | SfType::TimestampTz => {
                                    format.explicit("TIMESTAMP_FORMAT")
                                }
                                SfType::Binary => format.explicit("BINARY_FORMAT"),
                                _ => None,
                            };
                            match fmt {
                                Some(f) => format!("TO_VARCHAR({c}, {})", literal(&f)),
                                None => format!("TO_VARCHAR({c})"),
                            }
                        })
                        .collect()
                };
                let values = self.query_generated(&format!("SELECT {} FROM {from}", exprs.join(", ")))?;
                let rows = values.len();
                let body = if kind == "JSON" {
                    values.iter().filter_map(|r| r[0].clone()).map(|j| j + "\n").collect::<String>()
                } else {
                    let text_columns: Vec<bool> = types.iter().map(|t| *t == SfType::Text).collect();
                    let header = flag("HEADER").then_some(names.as_slice());
                    files::write_csv(header, &values, &text_columns, &format)
                };
                (body.into_bytes(), rows)
            }
            "PARQUET" => {
                let exprs: Vec<String> = aliases
                    .iter()
                    .zip(&types)
                    .map(|(c, t)| match t {
                        SfType::TimestampLtz | SfType::TimestampTz => {
                            format!("CONVERT_TIMEZONE('UTC', {c})::TIMESTAMP_NTZ")
                        }
                        t if t.is_semi_structured() => format!("TO_JSON({c})"),
                        _ => c.clone(),
                    })
                    .collect();
                let Command::Sql(select) = self.translate(&format!("SELECT {} FROM {from}", exprs.join(", ")))? else {
                    return Err(EngineError::internal("unload query"));
                };
                for pre in &select.pre {
                    self.conn.execute_batch(pre)?;
                }
                let rows: i64 =
                    self.conn.query_row(&format!("SELECT count(*) FROM ({})", select.sql), [], |r| r.get(0))?;
                let renamed: Vec<String> = names.iter().map(|n| crate::ident(n)).collect();
                let scratch =
                    std::env::temp_dir().join(format!("localflake-unload-{}.parquet", uuid::Uuid::new_v4().simple()));
                self.conn.execute_batch(&format!(
                    "COPY (SELECT * FROM ({}) AS __u({})) TO {} (FORMAT parquet)",
                    select.sql,
                    renamed.join(", "),
                    literal(&scratch.to_string_lossy())
                ))?;
                let bytes = std::fs::read(&scratch).map_err(EngineError::internal)?;
                let _ = std::fs::remove_file(&scratch);
                (bytes, rows as usize)
            }
            other => {
                return Err(EngineError::load(
                    "002040",
                    format!("localflake cannot unload to the {other} file format"),
                ));
            }
        };
        let input_bytes = content.len() as i64;
        let (content, extension): (Vec<u8>, String) = if kind == "PARQUET" {
            let extension = if compression.eq_ignore_ascii_case("NONE") { ".parquet" } else { ".snappy.parquet" };
            (content, extension.to_owned())
        } else {
            let (bytes, compressed) = files::compress(content, &compression)?;
            let base = format.text("FILE_EXTENSION").filter(|e| !e.is_empty()).map_or_else(
                || if kind == "JSON" { ".json".to_owned() } else { ".csv".to_owned() },
                |e| if e.starts_with('.') { e } else { format!(".{e}") },
            );
            (bytes, format!("{base}{compressed}"))
        };
        let file_name = if single {
            if path.is_empty() || path.ends_with('/') {
                path.push_str("data");
            }
            path.clone()
        } else {
            if path.is_empty() || path.ends_with('/') {
                path.push_str("data_");
            } else if !path.ends_with('_') {
                path.push('_');
            }
            format!("{path}0_0_0{extension}")
        };
        let target = stage.dir.join(&file_name);
        if flag("OVERWRITE") && !single {
            for f in walk(&stage.dir).into_iter().filter(|f| f.relative.starts_with(&path)) {
                let _ = std::fs::remove_file(f.path);
            }
        } else if target.exists() && !flag("OVERWRITE") {
            return Err(EngineError::load(
                "001030",
                format!(
                    "Files already existing at the unload destination: {}{file_name}. Use overwrite option to force unloading.",
                    stage.name.trim_start_matches('@')
                ),
            ));
        }
        let output_bytes = content.len() as i64;
        if rows > 0 {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(EngineError::internal)?;
            }
            std::fs::write(&target, &content).map_err(EngineError::internal)?;
        }
        if flag("DETAILED_OUTPUT") {
            let rows = if rows > 0 {
                vec![vec![text(file_name), Cell::Int(Some(output_bytes)), Cell::Int(Some(rows as i64))]]
            } else {
                vec![]
            };
            return self.rows_result(
                query_id,
                &[("FILE_NAME", "VARCHAR"), ("FILE_SIZE", "BIGINT"), ("ROW_COUNT", "BIGINT")],
                rows,
                StatementKind::Other,
            );
        }
        self.rows_result(
            query_id,
            &[("rows_unloaded", "BIGINT"), ("input_bytes", "BIGINT"), ("output_bytes", "BIGINT")],
            vec![vec![
                Cell::Int(Some(rows as i64)),
                Cell::Int(Some(if rows > 0 { input_bytes } else { 0 })),
                Cell::Int(Some(if rows > 0 { output_bytes } else { 0 })),
            ]],
            StatementKind::Other,
        )
    }
}

#[derive(Debug, Clone)]
struct LoadError {
    message: String,
    code: &'static str,
    line: usize,
    character: usize,
    row: i64,
    column: Option<String>,
    column_index: usize,
}

/// Rewrites a file's records to one JSON value per target column, matched by name.
fn map_columns_by_name(
    file: &mut FileRows,
    targets: &[(String, String, bool)],
    mode: &str,
    header: Option<&Vec<String>>,
) {
    let insensitive = mode == "CASE_INSENSITIVE";
    let matches = |key: &str, column: &str| if insensitive { key.eq_ignore_ascii_case(column) } else { key == column };
    for record in &mut file.records {
        let fields: Vec<Option<String>> = match header {
            // CSV: values are text, so encode them as JSON strings.
            Some(names) => targets
                .iter()
                .map(|(column, _, _)| {
                    let i = names.iter().position(|n| matches(n, column))?;
                    record.fields.get(i).cloned().flatten().map(|v| Value::String(v).to_string())
                })
                .collect(),
            None => {
                let document: Value = record
                    .fields
                    .first()
                    .cloned()
                    .flatten()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Null);
                let Value::Object(map) = document else {
                    record.fields = vec![None; targets.len()];
                    continue;
                };
                targets
                    .iter()
                    .map(|(column, _, _)| {
                        map.iter()
                            .find(|(k, _)| matches(k, column))
                            .map(|(_, v)| v)
                            .filter(|v| !v.is_null())
                            .map(Value::to_string)
                    })
                    .collect()
            }
        };
        record.fields = fields;
    }
}

/// Converts a staged value (text, or JSON text when `json`) to a column type,
/// as Snowflake SQL. With `attempt`, a failed conversion gives NULL.
fn conversion(input: &str, ty: &str, json: bool, format: &Format, attempt: bool, truncate: bool) -> String {
    let base = ty.split('(').next().unwrap_or(ty).trim().to_uppercase();
    let try_ = if attempt { "TRY_" } else { "" };
    if json {
        return match base.as_str() {
            "VARIANT" => format!("PARSE_JSON({input})"),
            "OBJECT" | "ARRAY" => format!("CAST(PARSE_JSON({input}) AS {base})"),
            _ => conversion(&format!("CAST(PARSE_JSON({input}) AS VARCHAR)"), ty, false, format, attempt, truncate),
        };
    }
    let with_format = |function: &str, key: &str| match format.explicit(key) {
        Some(f) => format!("{try_}{function}({input}, {})", literal(&f)),
        None if attempt => format!("{try_}{function}({input})"),
        None => format!("CAST({input} AS {ty})"),
    };
    match base.as_str() {
        "VARIANT" => format!("{try_}PARSE_JSON({input})"),
        "OBJECT" | "ARRAY" if attempt => format!("TRY_PARSE_JSON({input})"),
        "OBJECT" | "ARRAY" => format!("CAST(PARSE_JSON({input}) AS {base})"),
        "DATE" => with_format("TO_DATE", "DATE_FORMAT"),
        "TIME" => with_format("TO_TIME", "TIME_FORMAT"),
        "TIMESTAMP_NTZ" | "DATETIME" => with_format("TO_TIMESTAMP_NTZ", "TIMESTAMP_FORMAT"),
        "TIMESTAMP_LTZ" => with_format("TO_TIMESTAMP_LTZ", "TIMESTAMP_FORMAT"),
        "TIMESTAMP_TZ" => with_format("TO_TIMESTAMP_TZ", "TIMESTAMP_FORMAT"),
        "TIMESTAMP" => with_format("TO_TIMESTAMP", "TIMESTAMP_FORMAT"),
        "BINARY" | "VARBINARY" => {
            let f = format.explicit("BINARY_FORMAT").unwrap_or_else(|| "HEX".into());
            format!("{try_}TO_BINARY({input}, {})", literal(&f))
        }
        "VARCHAR" | "TEXT" | "STRING" | "CHAR" => {
            let length = ty.split_once('(').and_then(|(_, rest)| rest.trim_end_matches(')').parse::<u64>().ok());
            match length {
                Some(n) if truncate => format!("LEFT({input}, {n})"),
                Some(n) if attempt => format!("CASE WHEN LENGTH({input}) > {n} THEN NULL ELSE {input} END"),
                _ => input.to_owned(),
            }
        }
        _ if attempt => format!("TRY_CAST({input} AS {ty})"),
        _ => format!("CAST({input} AS {ty})"),
    }
}

fn read_definition(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn object_map(value: &Value) -> BTreeMap<String, Value> {
    match value {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => BTreeMap::new(),
    }
}

/// Splits `db.schema."Name"` into parts with identifier case rules.
fn split_name(name: &str) -> Vec<String> {
    let mut parts = vec![];
    let mut current = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => {
                quoted = !quoted;
                was_quoted = true;
            }
            '.' if !quoted => {
                parts.push(if was_quoted {
                    localflake_sql::decode_identifier(&current)
                } else {
                    current.to_uppercase()
                });
                current.clear();
                was_quoted = false;
            }
            c => current.push(c),
        }
    }
    parts.push(if was_quoted { localflake_sql::decode_identifier(&current) } else { current.to_uppercase() });
    parts
}
