//! `SHOW ...`, `DESCRIBE ...` and `UNSET ...`, parsed by hand (Snowflake has far
//! more SHOW variants than sqlparser knows) and answered from DuckDB's catalog
//! with Snowflake's output columns.

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::ast::quote_literal;
use crate::information_schema::HIDDEN_SCHEMAS;
use crate::{Context, SqlError};

#[derive(Debug, Clone, PartialEq)]
pub enum Meta {
    Show(Show),
    Describe { kind: String, name: Vec<String> },
    Unset(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Show {
    /// The object kind in upper case, e.g. `TABLES` or `PRIMARY KEYS`.
    pub kind: String,
    pub terse: bool,
    pub like: Option<String>,
    pub scope: Scope,
    pub starts_with: Option<String>,
    pub limit: Option<u64>,
    pub from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Scope {
    #[default]
    Default,
    Account,
    Database(Option<String>),
    Schema(Option<Vec<String>>),
    Table(Vec<String>),
    /// `IN name` without a keyword.
    Unqualified(Vec<String>),
}

/// Returns `Some` if `sql` is a statement handled here.
pub fn parse(sql: &str) -> Result<Option<Meta>, SqlError> {
    let tokens: Vec<Token> = Tokenizer::new(&SnowflakeDialect, sql)
        .tokenize()?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_) | Token::SemiColon))
        .collect();
    let mut p = Tokens { tokens, pos: 0 };
    match p.word().as_deref() {
        Some("SHOW") => parse_show(&mut p).map(|s| Some(Meta::Show(s))),
        Some("DESC" | "DESCRIBE") => {
            let mut kind = "TABLE".to_owned();
            if let Some(w) = p.peek_word()
                && matches!(
                    w.as_str(),
                    "TABLE" | "VIEW" | "DATABASE" | "SCHEMA" | "MATERIALIZED" | "EXTERNAL" | "DYNAMIC"
                )
            {
                p.pos += 1;
                kind = match w.as_str() {
                    "MATERIALIZED" | "EXTERNAL" | "DYNAMIC" => p.word().unwrap_or_default(),
                    _ => w,
                };
            }
            let name =
                p.object_name().ok_or_else(|| SqlError::Unsupported("DESCRIBE without an object name".into()))?;
            Ok(Some(Meta::Describe { kind, name }))
        }
        Some("UNSET") => {
            let mut names = Vec::new();
            while let Some(token) = p.next() {
                if let Token::Word(w) = token {
                    names.push(if w.quote_style.is_some() { w.value } else { w.value.to_uppercase() });
                }
            }
            Ok(Some(Meta::Unset(names)))
        }
        _ => Ok(None),
    }
}

fn parse_show(p: &mut Tokens) -> Result<Show, SqlError> {
    let mut show = Show::default();
    if p.peek_word().as_deref() == Some("TERSE") {
        p.pos += 1;
        show.terse = true;
    }
    let mut kind = Vec::new();
    while let Some(w) = p.peek_word() {
        if matches!(w.as_str(), "LIKE" | "IN" | "STARTS" | "LIMIT" | "HISTORY") {
            break;
        }
        kind.push(w);
        p.pos += 1;
    }
    show.kind = kind.join(" ");
    while let Some(w) = p.word() {
        match w.as_str() {
            "HISTORY" => {}
            "LIKE" => show.like = p.string(),
            "STARTS" => {
                p.word();
                show.starts_with = p.string();
            }
            "LIMIT" => {
                show.limit = p.number();
                if p.peek_word().as_deref() == Some("FROM") {
                    p.pos += 1;
                    show.from = p.string();
                }
            }
            "IN" => {
                show.scope = match p.peek_word().as_deref() {
                    Some("ACCOUNT") => {
                        p.pos += 1;
                        Scope::Account
                    }
                    Some("DATABASE") => {
                        p.pos += 1;
                        Scope::Database(p.object_name().and_then(|n| n.into_iter().last()))
                    }
                    Some("SCHEMA") => {
                        p.pos += 1;
                        Scope::Schema(p.object_name())
                    }
                    Some("TABLE" | "VIEW") => {
                        p.pos += 1;
                        Scope::Table(p.object_name().unwrap_or_default())
                    }
                    _ => Scope::Unqualified(p.object_name().unwrap_or_default()),
                };
            }
            _ => {}
        }
    }
    Ok(show)
}

struct Tokens {
    tokens: Vec<Token>,
    pos: usize,
}

impl Tokens {
    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn peek_word(&self) -> Option<String> {
        match self.tokens.get(self.pos) {
            Some(Token::Word(w)) if w.quote_style.is_none() => Some(w.value.to_uppercase()),
            _ => None,
        }
    }

    fn word(&mut self) -> Option<String> {
        let w = self.peek_word();
        if w.is_some() {
            self.pos += 1;
        }
        w
    }

    fn string(&mut self) -> Option<String> {
        match self.next() {
            Some(Token::SingleQuotedString(s)) => Some(s),
            Some(Token::DollarQuotedString(d)) => Some(d.value),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<u64> {
        match self.next() {
            Some(Token::Number(n, _)) => n.parse().ok(),
            _ => None,
        }
    }

    /// Parses `a.b."C"`, keeping quoted parts as-is (unquoted parts are already upper case).
    fn object_name(&mut self) -> Option<Vec<String>> {
        let mut parts = Vec::new();
        loop {
            match self.tokens.get(self.pos) {
                Some(Token::Word(w))
                    if parts.is_empty() || matches!(self.tokens.get(self.pos - 1), Some(Token::Period)) =>
                {
                    if w.quote_style.is_none() && parts.is_empty() && is_clause_keyword(&w.value) {
                        break;
                    }
                    parts.push(if w.quote_style.is_some() { w.value.clone() } else { w.value.to_uppercase() });
                    self.pos += 1;
                }
                Some(Token::Period) if !parts.is_empty() => self.pos += 1,
                _ => break,
            }
        }
        (!parts.is_empty()).then_some(parts)
    }
}

fn is_clause_keyword(word: &str) -> bool {
    matches!(word.to_uppercase().as_str(), "LIKE" | "STARTS" | "LIMIT" | "TYPE")
}

/// Resolves a 1–3 part object name against the session's current database and schema.
pub fn resolve(name: &[String], ctx: &Context) -> Result<(String, String, String), SqlError> {
    let current_db = || ctx.database.clone().ok_or(SqlError::NoDatabase);
    let current_schema = || ctx.schema.clone().ok_or(SqlError::NoDatabase);
    Ok(match name {
        [object] => (current_db()?, current_schema()?, object.clone()),
        [schema, object] => (current_db()?, schema.clone(), object.clone()),
        [db, schema, object] => (db.clone(), schema.clone(), object.clone()),
        _ => return Err(SqlError::Unsupported(format!("object name {}", name.join(".")))),
    })
}

/// Builds the DuckDB query answering a SHOW command.
pub fn show_sql(show: &Show, ctx: &Context) -> Result<String, SqlError> {
    // Catalog names are compared decoded (see `normalize::encode_identifier`).
    let lit = |s: &str| quote_literal(&crate::decode_identifier(s));
    let current_db = ctx.database.as_deref().map_or("NULL".to_owned(), lit);
    let user_dbs = "NOT internal AND database_name NOT IN ('memory', 'temp', 'system')";
    let created = "current_timestamp AS created_on";

    let (source, columns, terse_kind): (String, Vec<String>, &str) = match show.kind.as_str() {
        "DATABASES" => (
            format!(
                "SELECT database_name AS name, database_name, CAST(NULL AS VARCHAR) AS schema_name, comment FROM sf_databases() WHERE {user_dbs}"
            ),
            cols(&[
                created,
                "name",
                "'N' AS is_default",
                &format!("CASE WHEN name = {current_db} THEN 'Y' ELSE 'N' END AS is_current"),
                "'' AS origin",
                "'ACCOUNTADMIN' AS owner",
                "coalesce(comment, '') AS comment",
                "'' AS options",
                "'1' AS retention_time",
                "'STANDARD' AS kind",
                "CAST(NULL AS VARCHAR) AS budget",
                "'ROLE' AS owner_role_type",
                "CAST(NULL AS VARCHAR) AS object_visibility",
            ]),
            "DATABASE",
        ),
        "SCHEMAS" => (
            format!(
                "SELECT schema_name AS name, database_name, schema_name, comment FROM sf_schemas() \
                 WHERE database_name IN (SELECT database_name FROM sf_databases() WHERE {user_dbs}) AND schema_name NOT IN {HIDDEN_SCHEMAS} \
                 UNION ALL SELECT 'INFORMATION_SCHEMA', database_name, 'INFORMATION_SCHEMA', 'Views describing the contents of schemas in this database' \
                 FROM sf_databases() WHERE {user_dbs}"
            ),
            cols(&[
                created,
                "name",
                "'N' AS is_default",
                &format!(
                    "CASE WHEN database_name = {current_db} AND name = {} THEN 'Y' ELSE 'N' END AS is_current",
                    ctx.schema.as_deref().map_or("NULL".to_owned(), lit)
                ),
                "database_name",
                "'ACCOUNTADMIN' AS owner",
                "coalesce(comment, '') AS comment",
                "'' AS options",
                "'1' AS retention_time",
                "'ROLE' AS owner_role_type",
                "CAST(NULL AS VARCHAR) AS budget",
                "CAST(NULL AS VARCHAR) AS object_visibility",
            ]),
            "SCHEMA",
        ),
        "TABLES" | "TRANSIENT TABLES" | "HYBRID TABLES" => (
            format!("{} UNION ALL BY NAME {}", tables_source(), dynamic_tables_source()),
            cols(&[
                created,
                "name",
                "database_name",
                "schema_name",
                "kind",
                "coalesce(comment, '') AS comment",
                "'' AS cluster_by",
                "\"rows\"",
                "CAST(0 AS BIGINT) AS bytes",
                "'ACCOUNTADMIN' AS owner",
                "'1' AS retention_time",
                "'OFF' AS automatic_clustering",
                "'OFF' AS change_tracking",
                "'OFF' AS search_optimization",
                "CAST(NULL AS VARCHAR) AS search_optimization_progress",
                "CAST(NULL AS BIGINT) AS search_optimization_bytes",
                "'N' AS is_external",
                "'N' AS enable_schema_evolution",
                "'ROLE' AS owner_role_type",
                "'N' AS is_event",
                "CAST(NULL AS VARCHAR) AS budget",
                "'N' AS is_hybrid",
                "'N' AS is_iceberg",
                "is_dynamic",
                "'N' AS is_immutable",
            ]),
            "TABLE",
        ),
        "VIEWS" => (
            views_source(),
            cols(&[
                created,
                "name",
                "'' AS reserved",
                "database_name",
                "schema_name",
                "'ACCOUNTADMIN' AS owner",
                "coalesce(comment, '') AS comment",
                "text",
                "'false' AS is_secure",
                "CASE WHEN kind = 'MATERIALIZED_VIEW' THEN 'true' ELSE 'false' END AS is_materialized",
                "'ROLE' AS owner_role_type",
                "'OFF' AS change_tracking",
            ]),
            "VIEW",
        ),
        "OBJECTS" => (
            format!("{} UNION ALL {} UNION ALL BY NAME {}", tables_source(), views_source(), dynamic_tables_source()),
            cols(&[
                created,
                "name",
                "database_name",
                "schema_name",
                "kind",
                "coalesce(comment, '') AS comment",
                "'' AS cluster_by",
                "\"rows\"",
                "CAST(0 AS BIGINT) AS bytes",
                "'ACCOUNTADMIN' AS owner",
                "'1' AS retention_time",
                "'ROLE' AS owner_role_type",
                "CAST(NULL AS VARCHAR) AS budget",
                "'N' AS is_hybrid",
                "is_dynamic",
                "'N' AS is_iceberg",
            ]),
            "",
        ),
        "MATERIALIZED VIEWS" => (
            format!("SELECT * FROM ({}) WHERE kind = 'MATERIALIZED_VIEW'", views_source()),
            cols(&[
                created,
                "name",
                "'' AS reserved",
                "database_name",
                "schema_name",
                "'' AS cluster_by",
                "CAST(0 AS BIGINT) AS \"rows\"",
                "CAST(0 AS BIGINT) AS bytes",
                "CAST(NULL AS VARCHAR) AS source_database_name",
                "CAST(NULL AS VARCHAR) AS source_schema_name",
                "CAST(NULL AS VARCHAR) AS source_table_name",
                "current_timestamp AS refreshed_on",
                "current_timestamp AS compacted_on",
                "'ACCOUNTADMIN' AS owner",
                "'false' AS invalid",
                "'' AS invalid_reason",
                "'0s' AS behind_by",
                "coalesce(comment, '') AS comment",
                "text",
                "'false' AS is_secure",
                "'OFF' AS automatic_clustering",
                "'ROLE' AS owner_role_type",
                "CAST(NULL AS VARCHAR) AS budget",
            ]),
            "MATERIALIZED_VIEW",
        ),
        // Dynamic tables are views, so they're always up to date (see `views::create_dynamic_table`).
        "DYNAMIC TABLES" => (
            dynamic_tables_source(),
            cols(&[
                created,
                "name",
                "'' AS reserved",
                "database_name",
                "schema_name",
                "'' AS cluster_by",
                "CAST(0 AS BIGINT) AS \"rows\"",
                "CAST(0 AS BIGINT) AS bytes",
                "'ACCOUNTADMIN' AS owner",
                "target_lag",
                "refresh_mode",
                "CAST(NULL AS VARCHAR) AS refresh_mode_reason",
                "warehouse",
                "coalesce(comment, '') AS comment",
                "text",
                "'OFF' AS automatic_clustering",
                "'RUNNING' AS scheduling_state",
                "CAST(NULL AS TIMESTAMP WITH TIME ZONE) AS last_suspended_on",
                "'false' AS is_clone",
                "'false' AS is_replica",
                "'false' AS is_iceberg",
                "current_timestamp AS data_timestamp",
                "'ROLE' AS owner_role_type",
                "CAST(NULL AS VARCHAR) AS budget",
            ]),
            "TABLE",
        ),
        "COLUMNS" => (
            format!(
                "SELECT column_name AS name, database_name, schema_name, table_name, column_index, data_type, numeric_precision, \
                 numeric_scale, is_nullable, column_default, comment FROM sf_columns() \
                 WHERE database_name IN (SELECT database_name FROM sf_databases() WHERE {user_dbs}) AND schema_name NOT IN {HIDDEN_SCHEMAS}"
            ),
            cols(&[
                "table_name",
                "schema_name",
                "name AS column_name",
                "sf_type_json(data_type, numeric_precision, numeric_scale, CASE WHEN is_nullable THEN 'true' ELSE 'false' END) AS data_type",
                "CASE WHEN is_nullable THEN 'true' ELSE 'false' END AS \"null?\"",
                "coalesce(column_default, '') AS \"default\"",
                "'COLUMN' AS kind",
                "'' AS expression",
                "coalesce(comment, '') AS comment",
                "database_name",
                "CASE WHEN column_default LIKE 'nextval(%' THEN 'IDENTITY START 1 INCREMENT 1 ORDER' ELSE '' END AS autoincrement",
                "CAST(NULL AS VARCHAR) AS schema_evolution_record",
            ]),
            "",
        ),
        "SEQUENCES" => (
            format!(
                "SELECT sequence_name AS name, database_name, schema_name, start_value, increment_by, comment FROM sf_sequences() s \
                 WHERE schema_name NOT IN {HIDDEN_SCHEMAS} \
                 AND NOT EXISTS (SELECT 1 FROM sf_columns() c WHERE c.database_name = s.database_name \
                 AND contains(c.column_default, '\"' || s.schema_name || '\".\"' || s.sequence_name || '\"'))"
            ),
            cols(&[
                created,
                "name",
                "schema_name",
                "database_name",
                "start_value AS next_value",
                "increment_by AS interval",
                "'ACCOUNTADMIN' AS owner",
                "'ROLE' AS owner_role_type",
                "coalesce(comment, '') AS comment",
                "'N' AS ordered",
            ]),
            "SEQUENCE",
        ),
        "PRIMARY KEYS" | "UNIQUE KEYS" | "IMPORTED KEYS" | "EXPORTED KEYS" => {
            return Ok(empty(&[
                "created_on",
                "database_name",
                "schema_name",
                "table_name",
                "column_name",
                "key_sequence",
                "constraint_name",
                "rely",
                "comment",
            ]));
        }
        "PARAMETERS" => return Ok(parameters_sql(show, ctx)),
        "WAREHOUSES" => {
            let name = lit(ctx.warehouse.as_deref().unwrap_or("COMPUTE_WH"));
            return Ok(format!(
                "SELECT * FROM (SELECT {name} AS name, 'STARTED' AS state, 'STANDARD' AS type, 'X-Small' AS size, 1 AS min_cluster_count, \
                 1 AS max_cluster_count, 1 AS started_clusters, 0 AS running, 0 AS queued, 'Y' AS is_default, 'Y' AS is_current, \
                 600 AS auto_suspend, 'true' AS auto_resume, '100' AS available, '0' AS provisioning, '0' AS quiescing, '0' AS other, \
                 {created}, current_timestamp AS resumed_on, current_timestamp AS updated_on, 'ACCOUNTADMIN' AS owner, '' AS comment, \
                 'false' AS enable_query_acceleration, 8 AS query_acceleration_max_scale_factor, 'null' AS resource_monitor, \
                 'STANDARD' AS scaling_policy, CAST(NULL AS VARCHAR) AS budget, 'ROLE' AS owner_role_type) {}",
                like_clause(show, "name")
            ));
        }
        "ROLES" => {
            let current = ctx.role.as_deref().unwrap_or("ACCOUNTADMIN");
            let roles: Vec<String> = ["ACCOUNTADMIN", "PUBLIC", "SYSADMIN", "SECURITYADMIN", "USERADMIN"]
                .iter()
                .map(|r| format!("({}, {})", lit(r), if *r == current { "'Y'" } else { "'N'" }))
                .collect();
            return Ok(format!(
                "SELECT {created}, name, 'N' AS is_default, is_current, 'Y' AS is_inherited, 1 AS assigned_to_users, \
                 0 AS granted_to_roles, 0 AS granted_roles, '' AS owner, '' AS comment, 'ROLE' AS owner_role_type \
                 FROM (VALUES {}) AS r(name, is_current) {} ORDER BY name",
                roles.join(", "),
                like_clause(show, "name")
            ));
        }
        "USERS" => {
            let user = lit(&ctx.user);
            return Ok(format!(
                "SELECT * FROM (SELECT {user} AS name, {created}, {user} AS login_name, {user} AS display_name, '' AS first_name, \
                 '' AS last_name, '' AS email, '' AS mins_to_unlock, '' AS days_to_expiry, '' AS comment, 'false' AS disabled, \
                 'false' AS must_change_password, 'false' AS snowflake_lock, {} AS default_warehouse, {} AS default_namespace, \
                 {} AS default_role, '' AS default_secondary_roles, 'false' AS ext_authn_duo, '' AS ext_authn_uid, \
                 '' AS mins_to_bypass_mfa, 'ACCOUNTADMIN' AS owner, current_timestamp AS last_success_login, \
                 CAST(NULL AS TIMESTAMPTZ) AS expires_at_time, CAST(NULL AS TIMESTAMPTZ) AS locked_until_time, \
                 'true' AS has_password, 'false' AS has_rsa_public_key, 'PERSON' AS type, 'false' AS has_mfa) {}",
                lit(ctx.warehouse.as_deref().unwrap_or("")),
                lit(ctx.database.as_deref().unwrap_or("")),
                lit(ctx.role.as_deref().unwrap_or("ACCOUNTADMIN")),
                like_clause(show, "name")
            ));
        }
        "GRANTS" => {
            return Ok(empty(&[
                "created_on",
                "privilege",
                "granted_on",
                "name",
                "granted_to",
                "grantee_name",
                "grant_option",
                "granted_by",
            ]));
        }
        "FUNCTIONS" | "USER FUNCTIONS" => (
            format!(
                "SELECT function_name AS name, database_name, schema_name, function_type, parameters FROM duckdb_functions() \
                 WHERE database_name IN (SELECT database_name FROM sf_databases() WHERE {user_dbs}) AND NOT internal"
            ),
            cols(&[
                created,
                "name",
                "schema_name",
                "'N' AS is_builtin",
                "'N' AS is_aggregate",
                "'N' AS is_ansi",
                "len(parameters) AS min_num_arguments",
                "len(parameters) AS max_num_arguments",
                "name || '(' || array_to_string(parameters, ', ') || ')' AS arguments",
                "'user-defined function' AS description",
                "database_name AS catalog_name",
                "CASE WHEN function_type = 'table_macro' THEN 'Y' ELSE 'N' END AS is_table_function",
                "'N' AS valid_for_clustering",
                "'N' AS is_secure",
                "'' AS secrets",
                "'' AS external_access_integrations",
                "'false' AS is_external_function",
                "'SQL' AS language",
                "'N' AS is_memoizable",
                "'N' AS is_data_metric",
            ]),
            "FUNCTION",
        ),
        // `__lf_procedures` is replaced by the stored procedures of every database (see the engine's `procedures`).
        "PROCEDURES" | "USER PROCEDURES" => (
            "SELECT * FROM __lf_procedures".to_owned(),
            cols(&[
                "created_on",
                "name",
                "schema_name",
                "'N' AS is_builtin",
                "'N' AS is_aggregate",
                "'N' AS is_ansi",
                "min_num_arguments",
                "max_num_arguments",
                "arguments",
                "coalesce(comment, 'user-defined procedure') AS description",
                "database_name AS catalog_name",
                "CASE WHEN starts_with(returns, 'TABLE') THEN 'Y' ELSE 'N' END AS is_table_function",
                "'N' AS valid_for_clustering",
                "CASE WHEN is_secure THEN 'Y' ELSE 'N' END AS is_secure",
                "'' AS secrets",
                "'' AS external_access_integrations",
            ]),
            "PROCEDURE",
        ),
        _ => return Ok(empty(&["created_on", "name", "database_name", "schema_name", "kind", "comment", "owner"])),
    };

    let mut filters = scope_filters(show, ctx)?;
    if let Some(like) = &show.like {
        filters.push(format!("name ILIKE {}", lit(like)));
    }
    if let Some(prefix) = &show.starts_with {
        filters.push(format!("starts_with(name, {})", lit(prefix)));
    }
    if let Some(from) = &show.from {
        filters.push(format!("name >= {}", lit(from)));
    }
    let projection = if show.terse && !terse_kind.is_empty() || show.terse && show.kind == "OBJECTS" {
        let kind = if terse_kind.is_empty() { "kind".to_owned() } else { format!("{} AS kind", lit(terse_kind)) };
        let schema = if show.kind == "DATABASES" { "CAST(NULL AS VARCHAR) AS schema_name" } else { "schema_name" };
        format!("{created}, name, {kind}, database_name, {schema}")
    } else {
        columns.join(", ")
    };
    let order = if show.kind == "COLUMNS" {
        "database_name, schema_name, table_name, column_index"
    } else {
        "database_name, schema_name, name"
    };
    let where_clause = if filters.is_empty() { String::new() } else { format!(" WHERE {}", filters.join(" AND ")) };
    let limit = show.limit.map_or(String::new(), |n| format!(" LIMIT {n}"));
    Ok(format!("SELECT {projection} FROM ({source}) AS __show{where_clause} ORDER BY {order}{limit}"))
}

fn tables_source() -> String {
    format!(
        "SELECT table_name AS name, database_name, schema_name, CASE WHEN temporary THEN 'TEMPORARY' ELSE 'TABLE' END AS kind, \
         comment, CAST(estimated_size AS BIGINT) AS \"rows\", CAST(NULL AS VARCHAR) AS text, 'N' AS is_dynamic FROM sf_tables() \
         WHERE database_name NOT IN ('temp', 'system') AND schema_name NOT IN {HIDDEN_SCHEMAS}"
    )
}

/// Views and materialized views (both DuckDB views; see `views`).
fn views_source() -> String {
    format!(
        "SELECT view_name AS name, database_name, schema_name, \
         CASE WHEN sf_view_kind(comment) = 'MATERIALIZED VIEW' THEN 'MATERIALIZED_VIEW' ELSE 'VIEW' END AS kind, \
         sf_user_comment(comment) AS comment, CAST(NULL AS BIGINT) AS \"rows\", sql AS text, 'N' AS is_dynamic \
         FROM sf_views() WHERE NOT internal AND database_name NOT IN ('temp', 'system') AND schema_name NOT IN {HIDDEN_SCHEMAS} \
         AND sf_view_kind(comment) <> 'DYNAMIC TABLE'"
    )
}

/// Dynamic tables, which are DuckDB views (see `views`), as tables.
fn dynamic_tables_source() -> String {
    format!(
        "SELECT view_name AS name, database_name, schema_name, 'TABLE' AS kind, sf_user_comment(comment) AS comment, \
         CAST(NULL AS BIGINT) AS \"rows\", sql AS text, 'Y' AS is_dynamic, sf_view_option(comment, 2) AS target_lag, \
         sf_view_option(comment, 3) AS warehouse, sf_view_option(comment, 4) AS refresh_mode \
         FROM sf_views() WHERE NOT internal AND database_name NOT IN ('temp', 'system') AND schema_name NOT IN {HIDDEN_SCHEMAS} \
         AND sf_view_kind(comment) = 'DYNAMIC TABLE'"
    )
}

fn scope_filters(show: &Show, ctx: &Context) -> Result<Vec<String>, SqlError> {
    // Catalog names are compared decoded (see `normalize::encode_identifier`).
    let lit = |s: &str| quote_literal(&crate::decode_identifier(s));
    let db_filter = |db: &str| format!("database_name = {}", lit(db));
    let schema_filter =
        |db: &str, schema: &str| format!("database_name = {} AND schema_name = {}", lit(db), lit(schema));
    let account_level = show.kind == "DATABASES";
    let schema_level = show.kind == "SCHEMAS";
    Ok(match &show.scope {
        Scope::Account => vec![],
        _ if account_level => vec![],
        Scope::Default => match (&ctx.database, &ctx.schema) {
            (Some(db), _) if schema_level => vec![db_filter(db)],
            (Some(db), Some(schema)) => vec![schema_filter(db, schema)],
            (Some(db), None) => vec![db_filter(db)],
            (None, _) => vec![],
        },
        Scope::Database(name) => {
            let db = name.clone().or_else(|| ctx.database.clone()).ok_or(SqlError::NoDatabase)?;
            vec![db_filter(&db)]
        }
        Scope::Schema(name) => match name.as_deref() {
            None => vec![schema_filter(
                ctx.database.as_deref().ok_or(SqlError::NoDatabase)?,
                ctx.schema.as_deref().ok_or(SqlError::NoDatabase)?,
            )],
            Some([schema]) => vec![schema_filter(ctx.database.as_deref().ok_or(SqlError::NoDatabase)?, schema)],
            Some([db, schema, ..]) => vec![schema_filter(db, schema)],
            Some([]) => vec![],
        },
        Scope::Table(name) => table_filters(name, ctx)?,
        Scope::Unqualified(name) if matches!(show.kind.as_str(), "COLUMNS") => table_filters(name, ctx)?,
        Scope::Unqualified(name) => match name.as_slice() {
            [one] if schema_level => vec![db_filter(one)],
            [one] => {
                // A schema in the current database if one exists, otherwise a database.
                let db = ctx.database.as_deref().map_or("NULL".to_owned(), lit);
                vec![format!(
                    "CASE WHEN EXISTS (SELECT 1 FROM sf_schemas() WHERE database_name = {db} AND schema_name = {one}) \
                     THEN database_name = {db} AND schema_name = {one} ELSE database_name = {one} END",
                    one = lit(one)
                )]
            }
            [db, schema, ..] => vec![schema_filter(db, schema)],
            [] => vec![],
        },
    })
}

fn table_filters(name: &[String], ctx: &Context) -> Result<Vec<String>, SqlError> {
    let (db, schema, table) = resolve(name, ctx)?;
    Ok(vec![format!(
        "database_name = {} AND schema_name = {} AND table_name = {}",
        quote_literal(&db),
        quote_literal(&schema),
        quote_literal(&table)
    )])
}

fn parameters_sql(show: &Show, ctx: &Context) -> String {
    let mut params: Vec<(String, String)> =
        crate::context::DEFAULT_PARAMETERS.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    for (key, value) in &ctx.parameters {
        match params.iter_mut().find(|(k, _)| k == key) {
            Some(existing) => existing.1 = value.clone(),
            None => params.push((key.clone(), value.clone())),
        }
    }
    let rows: Vec<String> = params
        .iter()
        .map(|(k, v)| {
            let default = crate::context::DEFAULT_PARAMETERS.iter().find(|(d, _)| *d == k).map_or("", |(_, d)| *d);
            let level = if ctx.parameters.contains_key(k) { "SESSION" } else { "" };
            format!(
                "({}, {}, {}, {}, '', 'STRING')",
                quote_literal(k),
                quote_literal(v),
                quote_literal(default),
                quote_literal(level)
            )
        })
        .collect();
    format!(
        "SELECT key, value, \"default\", level, description, type FROM (VALUES {}) AS p(key, value, \"default\", level, description, type) {} ORDER BY key",
        rows.join(", "),
        like_clause(show, "key")
    )
}

fn like_clause(show: &Show, column: &str) -> String {
    show.like.as_ref().map_or(String::new(), |like| format!("WHERE {column} ILIKE {}", quote_literal(like)))
}

fn cols(columns: &[&str]) -> Vec<String> {
    columns.iter().map(|c| (*c).to_owned()).collect()
}

fn empty(columns: &[&str]) -> String {
    let projection: Vec<String> = columns
        .iter()
        .map(|c| {
            if *c == "created_on" {
                "CAST(NULL AS TIMESTAMPTZ) AS created_on".to_owned()
            } else {
                format!("CAST(NULL AS VARCHAR) AS \"{c}\"")
            }
        })
        .collect();
    format!("SELECT {} WHERE false", projection.join(", "))
}

/// Builds the query answering `DESCRIBE TABLE|VIEW`. Returns no rows if the object doesn't exist.
pub fn describe_sql(kind: &str, name: &[String], ctx: &Context) -> Result<String, SqlError> {
    // Catalog names are compared decoded (see `normalize::encode_identifier`).
    let lit = |s: &str| quote_literal(&crate::decode_identifier(s));
    match kind {
        "TABLE" | "VIEW" => {
            let (db, schema, table) = resolve(name, ctx)?;
            Ok(format!(
                "SELECT column_name AS \"name\", sf_type_desc(data_type, numeric_precision, numeric_scale) AS \"type\", \
                 'COLUMN' AS \"kind\", CASE WHEN is_nullable THEN 'Y' ELSE 'N' END AS \"null?\", column_default AS \"default\", \
                 'N' AS \"primary key\", 'N' AS \"unique key\", CAST(NULL AS VARCHAR) AS \"check\", \
                 CAST(NULL AS VARCHAR) AS \"expression\", comment AS \"comment\", CAST(NULL AS VARCHAR) AS \"policy name\", \
                 CAST(NULL AS VARCHAR) AS \"privacy domain\", CAST(NULL AS VARCHAR) AS \"schema_evolution_record\" \
                 FROM sf_columns() WHERE database_name = {} AND schema_name = {} AND table_name = {} ORDER BY column_index",
                lit(&db),
                lit(&schema),
                lit(&table)
            ))
        }
        "DATABASE" => {
            let db = name.last().cloned().unwrap_or_default();
            Ok(format!(
                "SELECT current_timestamp AS created_on, schema_name AS name, 'SCHEMA' AS kind FROM sf_schemas() \
                 WHERE database_name = {} AND schema_name NOT IN {HIDDEN_SCHEMAS} ORDER BY name",
                lit(&db)
            ))
        }
        "SCHEMA" => {
            let (db, schema) = match name {
                [schema] => (ctx.database.clone().ok_or(SqlError::NoDatabase)?, schema.clone()),
                [db, schema, ..] => (db.clone(), schema.clone()),
                [] => return Err(SqlError::NoDatabase),
            };
            Ok(format!(
                "SELECT current_timestamp AS created_on, table_name AS name, 'TABLE' AS kind FROM sf_tables() \
                 WHERE database_name = {db} AND schema_name = {schema} \
                 UNION ALL SELECT current_timestamp, view_name, 'VIEW' FROM sf_views() \
                 WHERE NOT internal AND database_name = {db} AND schema_name = {schema} ORDER BY name",
                db = lit(&db),
                schema = lit(&schema)
            ))
        }
        other => Err(SqlError::Unsupported(format!("DESCRIBE {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_show_variants() {
        let Some(Meta::Show(show)) =
            parse("SHOW TERSE TABLES LIKE 'A%' IN SCHEMA DB.S STARTS WITH 'A' LIMIT 5 FROM 'B'").unwrap()
        else {
            panic!()
        };
        assert_eq!(show.kind, "TABLES");
        assert!(show.terse);
        assert_eq!(show.like.as_deref(), Some("A%"));
        assert_eq!(show.scope, Scope::Schema(Some(vec!["DB".into(), "S".into()])));
        assert_eq!(show.starts_with.as_deref(), Some("A"));
        assert_eq!((show.limit, show.from.as_deref()), (Some(5), Some("B")));

        let Some(Meta::Show(show)) = parse("SHOW PRIMARY KEYS IN TABLE T").unwrap() else { panic!() };
        assert_eq!((show.kind.as_str(), show.scope), ("PRIMARY KEYS", Scope::Table(vec!["T".into()])));
    }

    #[test]
    fn parses_describe_and_unset() {
        assert_eq!(parse("DESC T").unwrap(), Some(Meta::Describe { kind: "TABLE".into(), name: vec!["T".into()] }));
        assert_eq!(
            parse(r#"DESCRIBE VIEW DB.S."v""#).unwrap(),
            Some(Meta::Describe { kind: "VIEW".into(), name: vec!["DB".into(), "S".into(), "v".into()] })
        );
        assert_eq!(parse("UNSET (A, B)").unwrap(), Some(Meta::Unset(vec!["A".into(), "B".into()])));
        assert_eq!(parse("SELECT 1").unwrap(), None);
    }
}
