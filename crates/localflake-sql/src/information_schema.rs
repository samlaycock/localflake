//! `<db>.INFORMATION_SCHEMA` views, built over DuckDB's catalog functions with
//! Snowflake's column names and type names.

use crate::ast::quote_literal;

/// SQL helpers for describing DuckDB columns in Snowflake terms.
pub const MACROS: &str = r#"
CREATE OR REPLACE TEMP MACRO sf_declared_type(c) AS
    CASE WHEN starts_with(c, chr(57345)) THEN split_part(c, chr(57345), 2) END;
CREATE OR REPLACE TEMP MACRO sf_user_comment(c) AS
    CASE WHEN starts_with(c, chr(57345)) THEN nullif(split_part(c, chr(57345), 3), '') ELSE c END;
CREATE OR REPLACE TEMP MACRO sf_dename(x) AS replace(x, chr(57346), '');
CREATE OR REPLACE TEMP MACRO sf_view_kind(c) AS split_part(coalesce(sf_declared_type(c), 'VIEW'), chr(57347), 1);
CREATE OR REPLACE TEMP MACRO sf_view_option(c, n) AS nullif(split_part(sf_declared_type(c), chr(57347), n), '');
CREATE OR REPLACE TEMP MACRO sf_columns() AS TABLE
    SELECT * REPLACE (coalesce(sf_declared_type(comment), data_type) AS data_type, sf_user_comment(comment) AS comment,
        sf_dename(database_name) AS database_name, sf_dename(schema_name) AS schema_name,
        sf_dename(table_name) AS table_name, sf_dename(column_name) AS column_name)
    FROM duckdb_columns();
CREATE OR REPLACE TEMP MACRO sf_databases() AS TABLE
    SELECT * REPLACE (sf_dename(database_name) AS database_name) FROM duckdb_databases();
CREATE OR REPLACE TEMP MACRO sf_schemas() AS TABLE
    SELECT * REPLACE (sf_dename(database_name) AS database_name, sf_dename(schema_name) AS schema_name) FROM duckdb_schemas();
CREATE OR REPLACE TEMP MACRO sf_tables() AS TABLE
    SELECT * REPLACE (sf_dename(database_name) AS database_name, sf_dename(schema_name) AS schema_name,
        sf_dename(table_name) AS table_name) FROM duckdb_tables();
CREATE OR REPLACE TEMP MACRO sf_views() AS TABLE
    SELECT * REPLACE (sf_dename(database_name) AS database_name, sf_dename(schema_name) AS schema_name,
        sf_dename(view_name) AS view_name) FROM duckdb_views();
CREATE OR REPLACE TEMP MACRO sf_sequences() AS TABLE
    SELECT * REPLACE (sf_dename(database_name) AS database_name, sf_dename(schema_name) AS schema_name,
        sf_dename(sequence_name) AS sequence_name) FROM duckdb_sequences();
CREATE OR REPLACE TEMP MACRO sf_char_length(t) AS
    CASE WHEN t LIKE 'VARCHAR(%' OR t LIKE 'BINARY(%' THEN CAST(regexp_extract(t, '\((\d+)\)', 1) AS BIGINT)
        WHEN t = 'VARCHAR' THEN 16777216 WHEN t = 'BLOB' THEN 8388608 END;
CREATE OR REPLACE TEMP MACRO sf_data_type(t) AS
    CASE WHEN t LIKE 'VARCHAR(%' THEN 'TEXT'
        WHEN t LIKE 'BINARY(%' THEN 'BINARY'
        WHEN t LIKE 'TIMESTAMP_TZ%' OR t = 'TIMESTAMP_NS' THEN 'TIMESTAMP_TZ'
        WHEN t IN ('TINYINT', 'SMALLINT', 'INTEGER', 'BIGINT', 'HUGEINT', 'UTINYINT', 'USMALLINT', 'UINTEGER', 'UBIGINT', 'UHUGEINT')
            OR t LIKE 'DECIMAL%' THEN 'NUMBER'
        WHEN t IN ('FLOAT', 'DOUBLE') THEN 'FLOAT'
        WHEN t = 'VARCHAR' THEN 'TEXT'
        WHEN t = 'BOOLEAN' THEN 'BOOLEAN'
        WHEN t = 'DATE' THEN 'DATE'
        WHEN t LIKE 'TIME%' AND t NOT LIKE 'TIMESTAMP%' THEN 'TIME'
        WHEN t = 'TIMESTAMP WITH TIME ZONE' THEN 'TIMESTAMP_LTZ'
        WHEN t LIKE 'TIMESTAMP%' THEN 'TIMESTAMP_NTZ'
        WHEN t = 'BLOB' THEN 'BINARY'
        WHEN t = 'JSON' OR t LIKE 'STRUCT%' OR t LIKE 'MAP%' THEN 'OBJECT'
        WHEN regexp_matches(t, '^(INTEGER|FLOAT)\[\d+\]$') THEN 'VECTOR'
        WHEN t LIKE '%[]' THEN 'ARRAY'
        WHEN t = 'VARIANT' THEN 'VARIANT'
        ELSE t END;
CREATE OR REPLACE TEMP MACRO sf_numeric_precision(t, p) AS
    CASE WHEN sf_data_type(t) = 'NUMBER' THEN CASE WHEN t LIKE 'DECIMAL%' THEN p ELSE 38 END END;
CREATE OR REPLACE TEMP MACRO sf_numeric_scale(t, s) AS
    CASE WHEN sf_data_type(t) = 'NUMBER' THEN CASE WHEN t LIKE 'DECIMAL%' THEN s ELSE 0 END END;
CREATE OR REPLACE TEMP MACRO sf_type_desc(t, p, s) AS
    CASE sf_data_type(t)
        WHEN 'NUMBER' THEN 'NUMBER(' || sf_numeric_precision(t, p) || ',' || sf_numeric_scale(t, s) || ')'
        WHEN 'TEXT' THEN 'VARCHAR(' || sf_char_length(t) || ')'
        WHEN 'BINARY' THEN 'BINARY(' || sf_char_length(t) || ')'
        WHEN 'TIME' THEN 'TIME(9)'
        WHEN 'TIMESTAMP_NTZ' THEN 'TIMESTAMP_NTZ(9)'
        WHEN 'TIMESTAMP_LTZ' THEN 'TIMESTAMP_LTZ(9)'
        WHEN 'TIMESTAMP_TZ' THEN CASE WHEN t LIKE '%(%' THEN t ELSE 'TIMESTAMP_TZ(9)' END
        WHEN 'VECTOR' THEN 'VECTOR(' || CASE WHEN t LIKE 'FLOAT%' THEN 'FLOAT' ELSE 'INT' END || ', '
            || regexp_extract(t, '\[(\d+)\]', 1) || ')'
        ELSE sf_data_type(t) END;
CREATE OR REPLACE TEMP MACRO sf_type_json(t, p, s, nullable) AS
    CASE sf_data_type(t)
        WHEN 'NUMBER' THEN '{"type":"FIXED","precision":' || sf_numeric_precision(t, p) || ',"scale":' || sf_numeric_scale(t, s) || ',"nullable":' || nullable || '}'
        WHEN 'TEXT' THEN '{"type":"TEXT","length":' || sf_char_length(t) || ',"byteLength":' || least(sf_char_length(t) * 4, 16777216) || ',"nullable":' || nullable || ',"fixed":false}'
        WHEN 'FLOAT' THEN '{"type":"REAL","nullable":' || nullable || '}'
        WHEN 'TIMESTAMP_NTZ' THEN '{"type":"TIMESTAMP_NTZ","precision":0,"scale":9,"nullable":' || nullable || '}'
        WHEN 'TIMESTAMP_LTZ' THEN '{"type":"TIMESTAMP_LTZ","precision":0,"scale":9,"nullable":' || nullable || '}'
        WHEN 'TIME' THEN '{"type":"TIME","precision":0,"scale":9,"nullable":' || nullable || '}'
        WHEN 'TIMESTAMP_TZ' THEN '{"type":"TIMESTAMP_TZ","precision":0,"scale":9,"nullable":' || nullable || '}'
        WHEN 'BINARY' THEN '{"type":"BINARY","length":' || sf_char_length(t) || ',"byteLength":' || sf_char_length(t) || ',"nullable":' || nullable || ',"fixed":true}'
        ELSE '{"type":"' || sf_data_type(t) || '","nullable":' || nullable || '}' END;
"#;

/// Schemas DuckDB creates in every catalog that Snowflake doesn't have.
pub const HIDDEN_SCHEMAS: &str = "('main', 'pg_catalog', 'information_schema')";

pub fn view(database: &str, view: &str) -> Option<String> {
    let db = quote_literal(database);
    let hidden = HIDDEN_SCHEMAS;
    Some(match view {
        "TABLES" => format!(
            "SELECT database_name AS TABLE_CATALOG, schema_name AS TABLE_SCHEMA, table_name AS TABLE_NAME, \
             'ACCOUNTADMIN' AS TABLE_OWNER, CASE WHEN temporary THEN 'LOCAL TEMPORARY' ELSE 'BASE TABLE' END AS TABLE_TYPE, \
             'NO' AS IS_TRANSIENT, CAST(NULL AS VARCHAR) AS CLUSTERING_KEY, CAST(estimated_size AS BIGINT) AS ROW_COUNT, \
             CAST(0 AS BIGINT) AS BYTES, CAST(1 AS BIGINT) AS RETENTION_TIME, current_timestamp AS CREATED, \
             current_timestamp AS LAST_ALTERED, current_timestamp AS LAST_DDL, 'ACCOUNTADMIN' AS LAST_DDL_BY, \
             'NO' AS AUTO_CLUSTERING_ON, comment AS COMMENT, CASE WHEN temporary THEN 'YES' ELSE 'NO' END AS IS_TEMPORARY, \
             'NO' AS IS_ICEBERG, 'NO' AS IS_DYNAMIC, 'NO' AS IS_IMMUTABLE \
             FROM sf_tables() WHERE database_name = {db} AND schema_name NOT IN {hidden} \
             UNION ALL \
             SELECT database_name, schema_name, view_name, 'ACCOUNTADMIN', \
             CASE sf_view_kind(comment) WHEN 'DYNAMIC TABLE' THEN 'BASE TABLE' WHEN 'MATERIALIZED VIEW' THEN 'MATERIALIZED VIEW' ELSE 'VIEW' END, \
             'NO', NULL, NULL, NULL, NULL, current_timestamp, current_timestamp, current_timestamp, 'ACCOUNTADMIN', 'NO', \
             sf_user_comment(comment), 'NO', 'NO', CASE WHEN sf_view_kind(comment) = 'DYNAMIC TABLE' THEN 'YES' ELSE 'NO' END, 'NO' \
             FROM sf_views() WHERE NOT internal AND database_name = {db} AND schema_name NOT IN {hidden}"
        ),
        "COLUMNS" => format!(
            "SELECT database_name AS TABLE_CATALOG, schema_name AS TABLE_SCHEMA, table_name AS TABLE_NAME, \
             column_name AS COLUMN_NAME, CAST(column_index AS BIGINT) AS ORDINAL_POSITION, column_default AS COLUMN_DEFAULT, \
             CASE WHEN is_nullable THEN 'YES' ELSE 'NO' END AS IS_NULLABLE, sf_data_type(data_type) AS DATA_TYPE, \
             CASE WHEN sf_data_type(data_type) IN ('TEXT', 'BINARY') THEN sf_char_length(data_type) END AS CHARACTER_MAXIMUM_LENGTH, \
             CASE WHEN sf_data_type(data_type) = 'TEXT' THEN least(sf_char_length(data_type) * 4, 16777216)
                  WHEN sf_data_type(data_type) = 'BINARY' THEN sf_char_length(data_type) END AS CHARACTER_OCTET_LENGTH, \
             CAST(sf_numeric_precision(data_type, numeric_precision) AS BIGINT) AS NUMERIC_PRECISION, \
             CASE WHEN sf_data_type(data_type) = 'NUMBER' THEN 10 END AS NUMERIC_PRECISION_RADIX, \
             CAST(sf_numeric_scale(data_type, numeric_scale) AS BIGINT) AS NUMERIC_SCALE, \
             CASE WHEN sf_data_type(data_type) IN ('TIME', 'TIMESTAMP_NTZ', 'TIMESTAMP_LTZ', 'TIMESTAMP_TZ') THEN 9 END AS DATETIME_PRECISION, \
             CAST(NULL AS VARCHAR) AS INTERVAL_TYPE, CAST(NULL AS BIGINT) AS INTERVAL_PRECISION, \
             CAST(NULL AS VARCHAR) AS CHARACTER_SET_CATALOG, CAST(NULL AS VARCHAR) AS CHARACTER_SET_SCHEMA, \
             CAST(NULL AS VARCHAR) AS CHARACTER_SET_NAME, CAST(NULL AS VARCHAR) AS COLLATION_CATALOG, \
             CAST(NULL AS VARCHAR) AS COLLATION_SCHEMA, CAST(NULL AS VARCHAR) AS COLLATION_NAME, \
             CAST(NULL AS VARCHAR) AS DOMAIN_CATALOG, CAST(NULL AS VARCHAR) AS DOMAIN_SCHEMA, CAST(NULL AS VARCHAR) AS DOMAIN_NAME, \
             CAST(NULL AS VARCHAR) AS UDT_CATALOG, CAST(NULL AS VARCHAR) AS UDT_SCHEMA, CAST(NULL AS VARCHAR) AS UDT_NAME, \
             CAST(NULL AS VARCHAR) AS SCOPE_CATALOG, CAST(NULL AS VARCHAR) AS SCOPE_SCHEMA, CAST(NULL AS VARCHAR) AS SCOPE_NAME, \
             CAST(NULL AS BIGINT) AS MAXIMUM_CARDINALITY, CAST(NULL AS VARCHAR) AS DTD_IDENTIFIER, \
             'NO' AS IS_SELF_REFERENCING, CASE WHEN column_default LIKE 'nextval(%' THEN 'YES' ELSE 'NO' END AS IS_IDENTITY, \
             CAST(NULL AS VARCHAR) AS IDENTITY_GENERATION, CAST(NULL AS VARCHAR) AS IDENTITY_START, \
             CAST(NULL AS VARCHAR) AS IDENTITY_INCREMENT, CAST(NULL AS VARCHAR) AS IDENTITY_MAXIMUM, \
             CAST(NULL AS VARCHAR) AS IDENTITY_MINIMUM, CAST(NULL AS VARCHAR) AS IDENTITY_CYCLE, \
             CAST(NULL AS VARCHAR) AS IDENTITY_ORDERED, CAST(NULL AS VARCHAR) AS SCHEMA_EVOLUTION_RECORD, comment AS COMMENT \
             FROM sf_columns() WHERE database_name = {db} AND schema_name NOT IN {hidden}"
        ),
        "SCHEMATA" => format!(
            "SELECT database_name AS CATALOG_NAME, schema_name AS SCHEMA_NAME, 'ACCOUNTADMIN' AS SCHEMA_OWNER, \
             'NO' AS IS_TRANSIENT, 'NO' AS IS_MANAGED_ACCESS, CAST(1 AS BIGINT) AS RETENTION_TIME, \
             CAST(NULL AS VARCHAR) AS DEFAULT_CHARACTER_SET_CATALOG, CAST(NULL AS VARCHAR) AS DEFAULT_CHARACTER_SET_SCHEMA, \
             CAST(NULL AS VARCHAR) AS DEFAULT_CHARACTER_SET_NAME, CAST(NULL AS VARCHAR) AS SQL_PATH, \
             current_timestamp AS CREATED, current_timestamp AS LAST_ALTERED, comment AS COMMENT, \
             CAST(NULL AS VARCHAR) AS REPLICABLE_WITH_FAILOVER_GROUPS, CAST(NULL AS VARCHAR) AS FAILOVER_GROUPS, \
             'ROLE' AS OWNER_ROLE_TYPE \
             FROM (SELECT database_name, schema_name, comment FROM sf_schemas() \
                   WHERE database_name = {db} AND schema_name NOT IN {hidden} \
                   UNION ALL SELECT {db}, 'INFORMATION_SCHEMA', 'Views describing the contents of schemas in this database')"
        ),
        "VIEWS" => format!(
            "SELECT database_name AS TABLE_CATALOG, schema_name AS TABLE_SCHEMA, view_name AS TABLE_NAME, \
             'ACCOUNTADMIN' AS TABLE_OWNER, sql AS VIEW_DEFINITION, 'NONE' AS CHECK_OPTION, 'NO' AS IS_UPDATABLE, \
             'NO' AS INSERTABLE_INTO, 'NO' AS IS_SECURE, current_timestamp AS CREATED, current_timestamp AS LAST_ALTERED, \
             current_timestamp AS LAST_DDL, 'ACCOUNTADMIN' AS LAST_DDL_BY, sf_user_comment(comment) AS COMMENT \
             FROM sf_views() WHERE NOT internal AND database_name = {db} AND schema_name NOT IN {hidden} \
             AND sf_view_kind(comment) <> 'DYNAMIC TABLE'"
        ),
        "DATABASES" => {
            "SELECT database_name AS DATABASE_NAME, 'ACCOUNTADMIN' AS DATABASE_OWNER, 'NO' AS IS_TRANSIENT, \
             comment AS COMMENT, current_timestamp AS CREATED, current_timestamp AS LAST_ALTERED, \
             CAST(1 AS BIGINT) AS RETENTION_TIME, 'STANDARD' AS TYPE, 'ROLE' AS OWNER_ROLE_TYPE \
             FROM sf_databases() WHERE NOT internal AND database_name NOT IN ('memory', 'temp', 'system')"
                .to_owned()
        }
        "SEQUENCES" => format!(
            "SELECT database_name AS SEQUENCE_CATALOG, schema_name AS SEQUENCE_SCHEMA, sequence_name AS SEQUENCE_NAME, \
             'ACCOUNTADMIN' AS SEQUENCE_OWNER, 'NUMBER' AS DATA_TYPE, 38 AS NUMERIC_PRECISION, 10 AS NUMERIC_PRECISION_RADIX, \
             0 AS NUMERIC_SCALE, CAST(start_value AS VARCHAR) AS START_VALUE, CAST(min_value AS VARCHAR) AS MINIMUM_VALUE, \
             CAST(max_value AS VARCHAR) AS MAXIMUM_VALUE, CAST(increment_by AS VARCHAR) AS INCREMENT, \
             CASE WHEN cycle THEN 'YES' ELSE 'NO' END AS CYCLE_OPTION, current_timestamp AS CREATED, \
             current_timestamp AS LAST_ALTERED, 'NO' AS ORDERED, comment AS COMMENT \
             FROM sf_sequences() WHERE database_name = {db} AND schema_name NOT IN {hidden}"
        ),
        _ => return None,
    })
}
