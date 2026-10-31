//! Stored procedures: each database keeps its procedures in a table in the
//! hidden `__localflake` schema, so they persist, clone and drop with the
//! database. `CALL` runs a procedure's body with the Snowflake Scripting
//! interpreter (see `script`).

use localflake_sql::procedure::{self, Procedure, Returns};
use localflake_sql::{StatementKind, decode_identifier, encode_identifier};

use crate::{DEFAULT_SCHEMA, EngineError, QueryResult, Session, ident, literal};

const STORE_SCHEMA: &str = "__localflake";

/// The columns of a database's procedure table (all VARCHAR unless noted).
/// Names are Snowflake names (decoded); `definition` is the normalized
/// `CREATE PROCEDURE` statement, parsed again to run the procedure.
const STORE_COLUMNS: &str = "schema_name VARCHAR, name VARCHAR, signature VARCHAR, argument_signature VARCHAR, \
     arguments VARCHAR, min_num_arguments INTEGER, max_num_arguments INTEGER, returns VARCHAR, language VARCHAR, \
     body VARCHAR, comment VARCHAR, execute_as VARCHAR, null_input VARCHAR, is_secure BOOLEAN, \
     created_on TIMESTAMPTZ, definition VARCHAR";

/// Where a procedure lives: the DuckDB database and schema names, and its Snowflake name.
pub(crate) struct Location {
    database: String,
    schema: String,
    name: String,
}

impl Location {
    fn table(&self) -> String {
        format!("{}.{}.procedures", ident(&self.database), ident(STORE_SCHEMA))
    }

    /// The schema-qualified name, as errors print it.
    fn display(&self) -> String {
        format!("{}.{}", decode_identifier(&self.schema), self.name)
    }
}

/// A stored procedure, parsed from its definition.
pub(crate) struct Stored {
    pub(crate) procedure: Procedure,
    /// The procedure's Snowflake name, for its result column.
    pub(crate) name: String,
}

/// `(NUMBER, VARCHAR)`: the argument types that identify an overload.
fn signature(types: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let families: Vec<&str> = types.into_iter().map(|t| procedure::type_family(t.as_ref())).collect();
    format!("({})", families.join(", "))
}

/// The return type as SHOW PROCEDURES and INFORMATION_SCHEMA print it.
fn returns_display(returns: &Returns) -> String {
    match returns {
        Returns::Scalar { data_type, .. } => data_type.clone(),
        Returns::Table(columns) => {
            let columns: Vec<String> = columns.iter().map(|(n, t)| format!("{} {t}", decode_identifier(n))).collect();
            format!("TABLE ({})", columns.join(", "))
        }
    }
}

impl Session {
    /// Resolves `[[db.]schema.]name` against the current database and schema.
    fn procedure_location(&self, name: &[String]) -> Result<Location, EngineError> {
        let current_db = || self.ctx.database.clone().ok_or_else(EngineError::no_current_database);
        let current_schema = || self.ctx.schema.clone().unwrap_or_else(|| DEFAULT_SCHEMA.to_owned());
        let (database, schema, name) = match name {
            [name] => (current_db()?, current_schema(), name),
            [schema, name] => (current_db()?, schema.clone(), name),
            [database, schema, name, ..] => (database.clone(), schema.clone(), name),
            [] => return Err(EngineError::no_current_database()),
        };
        Ok(Location { database, schema, name: decode_identifier(name) })
    }

    fn store_exists(&self, database: &str) -> Result<bool, EngineError> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE database_name = ? AND schema_name = ? AND table_name = 'procedures'",
            [database, STORE_SCHEMA],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    /// Forgets a dropped (or replaced) schema's procedures.
    pub(crate) fn drop_schema_procedures(&self, database: &str, schema: &str) -> Result<(), EngineError> {
        if self.store_exists(database)? {
            let sql =
                format!("DELETE FROM {}.{}.procedures WHERE schema_name = ?", ident(database), ident(STORE_SCHEMA));
            self.conn.execute(&sql, [decode_identifier(schema)])?;
        }
        Ok(())
    }

    /// The stored procedures named `name` in its schema, as (signature, definition).
    fn stored_overloads(&self, location: &Location) -> Result<Vec<(String, String)>, EngineError> {
        if !self.store_exists(&location.database)? {
            return Ok(vec![]);
        }
        let sql = format!("SELECT signature, definition FROM {} WHERE schema_name = ? AND name = ?", location.table());
        let mut stmt = self.conn.prepare(&sql)?;
        let schema = decode_identifier(&location.schema);
        let rows = stmt.query_map([schema.as_str(), location.name.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The overloads of a procedure, for `CALL` to choose from.
    pub(crate) fn procedure_overloads(&self, name: &[String]) -> Result<Vec<Stored>, EngineError> {
        let location = self.procedure_location(name)?;
        let overloads = self.stored_overloads(&location)?;
        if overloads.is_empty() {
            return Err(EngineError::unknown_procedure(&location.display()));
        }
        overloads
            .into_iter()
            .map(|(_, definition)| {
                let procedure = procedure::parse_create(&definition)?
                    .ok_or_else(|| EngineError::internal(format!("bad stored procedure: {definition}")))?;
                Ok(Stored { procedure, name: location.name.clone() })
            })
            .collect()
    }

    pub(crate) fn create_procedure(
        &mut self,
        procedure: Procedure,
        definition: &str,
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        let location = self.procedure_location(&procedure.name)?;
        if !self.schema_exists(&location.database, &location.schema)? {
            let schema = format!("{}.{}", decode_identifier(&location.database), decode_identifier(&location.schema));
            return Err(EngineError::does_not_exist("Schema", &schema));
        }
        // DDL commits the open transaction.
        if self.in_transaction {
            self.end_transaction("COMMIT")?;
        }
        let signature = signature(procedure.arguments.iter().map(|a| &a.data_type));
        let exists = self.stored_overloads(&location)?.iter().any(|(s, _)| *s == signature);
        if exists && procedure.if_not_exists {
            let message = format!("{} already exists, statement succeeded.", location.name);
            return Ok(QueryResult::status(query_id.to_owned(), &message, StatementKind::Ddl));
        }
        if exists && !procedure.or_replace {
            return Err(EngineError::already_exists("Function", &format!("{}{signature}", location.display())));
        }

        // Optional arguments (which come last) are bracketed: `P(NUMBER [, VARCHAR, VARCHAR])`.
        let family = |a: &procedure::Argument| procedure::type_family(&a.data_type);
        let (optional, required): (Vec<_>, Vec<_>) = procedure.arguments.iter().partition(|a| a.default.is_some());
        let mut types = required.iter().map(|a| family(a)).collect::<Vec<_>>().join(", ");
        if !optional.is_empty() {
            let optional = optional.iter().map(|a| family(a)).collect::<Vec<_>>().join(", ");
            types = if types.is_empty() { format!("[{optional}]") } else { format!("{types} [, {optional}]") };
        }
        let returns = returns_display(&procedure.returns);
        let return_family = match &procedure.returns {
            Returns::Scalar { data_type, .. } => procedure::type_family(data_type).to_owned(),
            Returns::Table(_) => returns.clone(),
        };
        let arguments = format!("{}({types}) RETURN {return_family}", location.name);
        let argument_signature: Vec<String> =
            procedure.arguments.iter().map(|a| format!("{} {}", decode_identifier(&a.name), a.data_type)).collect();
        let body = if procedure.body_normalized { decode_identifier(&procedure.body) } else { procedure.body.clone() };

        let table = location.table();
        self.conn.execute_batch(&format!(
            "CREATE SCHEMA IF NOT EXISTS {}.{}; CREATE TABLE IF NOT EXISTS {table} ({STORE_COLUMNS});",
            ident(&location.database),
            ident(STORE_SCHEMA)
        ))?;
        let schema = decode_identifier(&location.schema);
        self.conn.execute(
            &format!("DELETE FROM {table} WHERE schema_name = ? AND name = ? AND signature = ?"),
            [schema.as_str(), location.name.as_str(), signature.as_str()],
        )?;
        self.conn.execute(
            &format!("INSERT INTO {table} VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, current_timestamp, ?)"),
            duckdb::params![
                schema,
                location.name,
                signature,
                format!("({})", argument_signature.join(", ")),
                arguments,
                required.len() as i64,
                procedure.arguments.len() as i64,
                returns,
                procedure.language,
                body,
                procedure.comment,
                procedure.execute_as,
                procedure.null_input,
                procedure.secure,
                definition,
            ],
        )?;
        let message = format!("Function {} successfully created.", location.name);
        Ok(QueryResult::status(query_id.to_owned(), &message, StatementKind::Ddl))
    }

    pub(crate) fn drop_procedure(
        &mut self,
        name: &[String],
        types: &[String],
        if_exists: bool,
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        let location = self.procedure_location(name)?;
        let signature = signature(types);
        let exists = self.stored_overloads(&location)?.iter().any(|(s, _)| *s == signature);
        if !exists {
            if if_exists {
                let message = format!("Drop statement executed successfully ({} already dropped).", location.name);
                return Ok(QueryResult::status(query_id.to_owned(), &message, StatementKind::Ddl));
            }
            return Err(EngineError::does_not_exist("Function", &format!("{}{signature}", location.display())));
        }
        if self.in_transaction {
            self.end_transaction("COMMIT")?;
        }
        let schema = decode_identifier(&location.schema);
        self.conn.execute(
            &format!("DELETE FROM {} WHERE schema_name = ? AND name = ? AND signature = ?", location.table()),
            [schema.as_str(), location.name.as_str(), signature.as_str()],
        )?;
        let message = format!("{} successfully dropped.", location.name);
        Ok(QueryResult::status(query_id.to_owned(), &message, StatementKind::Ddl))
    }

    /// `DESCRIBE PROCEDURE`: one row per property.
    pub(crate) fn describe_procedure(
        &mut self,
        name: &[String],
        types: &[String],
        query_id: &str,
    ) -> Result<QueryResult, EngineError> {
        let location = self.procedure_location(name)?;
        let signature = signature(types);
        let not_found = || EngineError::does_not_exist("Function", &format!("{}{signature}", location.display()));
        if !self.store_exists(&location.database)? {
            return Err(not_found());
        }
        let sql = format!(
            "SELECT argument_signature, returns, language, null_input, execute_as, body FROM {} \
             WHERE schema_name = ? AND name = ? AND signature = ?",
            location.table()
        );
        let schema = decode_identifier(&location.schema);
        let row: Option<[String; 6]> = self
            .conn
            .query_row(&sql, [schema.as_str(), location.name.as_str(), signature.as_str()], |r| {
                Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?])
            })
            .ok();
        let Some([arguments, returns, language, null_input, execute_as, body]) = row else {
            return Err(not_found());
        };
        let properties = [
            ("signature", arguments),
            ("returns", returns),
            ("language", language),
            ("null handling", null_input),
            ("volatility", "VOLATILE".to_owned()),
            ("execute as", execute_as),
            ("body", body),
        ];
        let rows: Vec<String> =
            properties.iter().map(|(property, value)| format!("({}, {})", literal(property), literal(value))).collect();
        let sql = format!("SELECT * FROM (VALUES {}) AS p(\"property\", \"value\")", rows.join(", "));
        self.run_query(&sql, &[], false, query_id, StatementKind::Show)
    }

    /// Replaces `__lf_procedures` (in SHOW PROCEDURES and INFORMATION_SCHEMA.PROCEDURES)
    /// with the stored procedures of every database.
    pub(crate) fn expand_procedures(&self, sql: &str) -> Result<String, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT database_name FROM duckdb_tables() WHERE schema_name = ? AND table_name = 'procedures' ORDER BY 1",
        )?;
        let databases: Vec<String> = stmt.query_map([STORE_SCHEMA], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let empty: Vec<String> = STORE_COLUMNS
            .split(", ")
            .map(|c| {
                let (name, data_type) = c.split_once(' ').unwrap_or((c, "VARCHAR"));
                format!("CAST(NULL AS {data_type}) AS {name}")
            })
            .collect();
        let mut parts =
            vec![format!("SELECT CAST(NULL AS VARCHAR) AS database_name, {} WHERE false", empty.join(", "))];
        for database in &databases {
            parts.push(format!(
                "SELECT {} AS database_name, * FROM {}.{}.procedures",
                literal(&decode_identifier(database)),
                ident(database),
                ident(STORE_SCHEMA)
            ));
        }
        Ok(sql.replace("__lf_procedures", &format!("({})", parts.join(" UNION ALL BY NAME "))))
    }
}

/// The Snowflake name of a procedure as a quoted DuckDB column alias.
pub(crate) fn column_alias(name: &str) -> String {
    ident(&encode_identifier(name))
}
