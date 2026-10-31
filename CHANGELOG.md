# localflake

## 0.2.0

### Minor Changes

- [#17](https://github.com/samlaycock/localflake/pull/17) [`45ecaff`](https://github.com/samlaycock/localflake/commit/45ecaff529168ce4b350c65a89cba7daea026fd9) Thanks [@samlaycock](https://github.com/samlaycock)! - Fill most remaining gaps in Snowflake's function reference: `REGEXP_SUBSTR_ALL`, `GREATEST_IGNORE_NULLS`/`LEAST_IGNORE_NULLS`, `REGR_VALX`/`REGR_VALY`, `ARRAY_REMOVE_AT`, `ARRAY_REPEAT`, `ARRAYS_TO_OBJECT`, `SOUNDEX_P123`, `IS_TIME`/`IS_TIMESTAMP_TZ`/`IS_DATE_VALUE`, `TIMESTAMP_LTZ_FROM_PARTS`/`TIMESTAMP_TZ_FROM_PARTS`, context functions (`CURRENT_STATEMENT`, `CURRENT_TRANSACTION`, `LAST_TRANSACTION`, `GETVARIABLE`, `INVOKER_ROLE`, `IS_ROLE_IN_SESSION`, ...), `SYSTEM$WAIT`, `SYSTEM$CANCEL_ALL_QUERIES`, `SYSTEM$GET_TZDB_VERSION`, `SYSTEM$GET_TAG`, the vector aggregates with `VECTOR_NORMALIZE` and `VECTOR_TRUNCATE`, `INTERPOLATE_FFILL|BFILL|LINEAR`, HyperLogLog, percentile, top-k and MinHash states, the bitmap functions, XML (`PARSE_XML`, `XMLGET`, `CHECK_XML`, `TO_XML`), `PARSE_IP`, `SEARCH_IP`, `SEARCH`, and `ENCRYPT`/`DECRYPT`/`ENCRYPT_RAW`/`DECRYPT_RAW`. Also fixes `TYPEOF` of a VARIANT holding a TIMESTAMP_TZ, `SUBSTR` with a start of 0 or a BINARY value, and `TO_VARCHAR(binary, 'UTF-8' | 'HEX' | 'BASE64')`.

- [#12](https://github.com/samlaycock/localflake/pull/12) [`3771ede`](https://github.com/samlaycock/localflake/commit/3771ede076b35831339a2f2675c79fcceff9c1f9) Thanks [@samlaycock](https://github.com/samlaycock)! - Support cancelling queries: driver abort requests (`/queries/v1/abort-request` by request ID, as sent on Go context cancellation, Node's `statement.cancel()` and client-side timeouts, and `/queries/<id>/abort-request`), `SYSTEM$CANCEL_QUERY` and the `STATEMENT_TIMEOUT_IN_SECONDS` parameter. Cancelled statements fail with Snowflake's 000604 error and timed-out ones with 000630, and a cancelled statement leaves an open transaction usable.

- [#18](https://github.com/samlaycock/localflake/pull/18) [`6fef5a5`](https://github.com/samlaycock/localflake/commit/6fef5a5f610bdc93c82e80209f54fb1b8f7a1032) Thanks [@samlaycock](https://github.com/samlaycock)! - Support Snowflake Scripting anonymous blocks and `EXECUTE IMMEDIATE`: typed variables (`DECLARE`, `LET`, `:=`), `IF`, `CASE`, `FOR`, `WHILE`, `REPEAT` and `LOOP` with labelled `BREAK`/`CONTINUE`, `RETURN`, SQL statements with `:variable` binds and `SELECT ... INTO`, `SQLROWCOUNT`/`SQLFOUND`/`SQLNOTFOUND`/`ACTIVITY_COUNT`, declared exceptions with `RAISE`, `EXIT` and `CONTINUE` handlers (`SQLCODE`, `SQLERRM`, `SQLSTATE`), and Snowflake's uncaught-exception errors. Blocks sent by drivers are kept whole rather than split at their semicolons, and cancellation and statement timeouts stop a running block.

- [#19](https://github.com/samlaycock/localflake/pull/19) [`ecf87cc`](https://github.com/samlaycock/localflake/commit/ecf87cc11d958ec9aa3fa019d92facbc045bbe1f) Thanks [@samlaycock](https://github.com/samlaycock)! - Support stored procedures written in Snowflake Scripting: `CREATE [OR REPLACE] PROCEDURE` with scalar or `RETURNS TABLE` results, default, named and `OUT` arguments, overloads, `STRICT` and `NOT NULL` returns; `CALL` (including `CALL ... INTO :variable` and recursive calls from blocks), `DROP PROCEDURE`, `DESCRIBE PROCEDURE`, `SHOW PROCEDURES` and `INFORMATION_SCHEMA.PROCEDURES`. Procedures are stored in each database's hidden `__localflake` schema, so they persist, clone and drop with the database. Snowflake Scripting also gains cursors (`OPEN`, `FETCH`, `CLOSE`, `FOR row IN cursor`), `RESULTSET`s and `RETURN TABLE(...)`, and `SELECT ... INTO` accepts variables without colons.

- [#15](https://github.com/samlaycock/localflake/pull/15) [`ad2ae2a`](https://github.com/samlaycock/localflake/commit/ad2ae2aac7e1c63930a584521b5965609672a44d) Thanks [@samlaycock](https://github.com/samlaycock)! - Support the `VECTOR(INT, n)` and `VECTOR(FLOAT, n)` types: columns, casts from and to `ARRAY`, `DESCRIBE` and `INFORMATION_SCHEMA` metadata, and the `VECTOR_COSINE_SIMILARITY`, `VECTOR_INNER_PRODUCT`, `VECTOR_L1_DISTANCE` and `VECTOR_L2_DISTANCE` functions. Results carry Snowflake's `vector` column metadata, as JSON arrays or Arrow fixed-size lists.

- [#16](https://github.com/samlaycock/localflake/pull/16) [`d2da9d5`](https://github.com/samlaycock/localflake/commit/d2da9d516ac110d80a08dd3d36aa471a408f5b24) Thanks [@samlaycock](https://github.com/samlaycock)! - Support materialized views and dynamic tables, emulated as views that are always up to date: `CREATE`, `DROP`, `ALTER ... RENAME TO | SET COMMENT | UNSET COMMENT` (refresh, suspend, resume and the like are accepted), `DESCRIBE`, `SHOW MATERIALIZED VIEWS`, `SHOW DYNAMIC TABLES` (with the target lag, warehouse and refresh mode given), and dynamic tables listed as tables by `SHOW TABLES`, `SHOW OBJECTS` (`is_dynamic`) and `INFORMATION_SCHEMA.TABLES`. `ALTER VIEW` (rename, comments, and accepted no-ops such as `SET SECURE`) now works too.

- [#20](https://github.com/samlaycock/localflake/pull/20) [`9db8757`](https://github.com/samlaycock/localflake/commit/9db875726c96edf4c1606ebf311931b3e3726174) Thanks [@samlaycock](https://github.com/samlaycock)! - Add a web UI at `http://127.0.0.1:8080/` (`/__localflake/ui/`) for looking inside localflake while you develop. It has SQL worksheets (statement-at-cursor runs, highlighting, name completion, cancellation), a results grid with sorting, filtering and CSV/TSV/JSON export, and an object explorer that can preview, describe, drop and truncate objects, upload files to stages, download them, and load files into tables. It also has saved queries, a live history of every statement from every client with the DuckDB SQL each was translated to, and a list of connected sessions that can end a session to test reconnects. It's embedded in the binary and works offline. Turn it off with `--ui false` (`LOCALFLAKE_UI=false`).

### Patch Changes

- [#20](https://github.com/samlaycock/localflake/pull/20) [`040becb`](https://github.com/samlaycock/localflake/commit/040becb4711f5acf04db39cef40b8c1f637f9d88) Thanks [@samlaycock](https://github.com/samlaycock)! - Fix `TRY_CAST(... AS BOOLEAN)`, which failed with "TRY can not be used in combination with a volatile function". This also fixes `COPY INTO` with `MATCH_BY_COLUMN_NAME` into tables with `BOOLEAN` columns.

## 0.1.3

### Patch Changes

- [#10](https://github.com/samlaycock/localflake/pull/10) [`96b424e`](https://github.com/samlaycock/localflake/commit/96b424e0563d9080d345dd440dd645a2a853fa60) Thanks [@samlaycock](https://github.com/samlaycock)! - Fix `TO_VARCHAR` with a format on `TIMESTAMP_TZ` values printing `+00` instead of `+00:00` for `TZH:TZM` when a text bind was range-compared with a date or timestamp column (`WHERE D >= ?`).

## 0.1.2

### Patch Changes

- [#8](https://github.com/samlaycock/localflake/pull/8) [`0e3668e`](https://github.com/samlaycock/localflake/commit/0e3668e00d8e3d76d53d2a0f115aab2056f345ac) Thanks [@samlaycock](https://github.com/samlaycock)! - Fix two bugs in statements with `?` binds: `TO_VARCHAR` with a format on `TIMESTAMP_TZ` values printed `+00` instead of `+00:00` for `TZH:TZM` when a bind fed a `FLATTEN` subquery (`... IN (SELECT VALUE FROM TABLE(FLATTEN(INPUT => PARSE_JSON(?))))`), and `ESCAPE '\\'` failed with "Unterminated string literal".

## 0.1.1

### Patch Changes

- [#6](https://github.com/samlaycock/localflake/pull/6) [`f20eb41`](https://github.com/samlaycock/localflake/commit/f20eb418ee974d2f40d211d704c5a19ab45daa5e) Thanks [@samlaycock](https://github.com/samlaycock)! - Fix `TO_VARCHAR`/`TO_CHAR` with a format on `TIMESTAMP_TZ` and `TIMESTAMP_LTZ` values: date and time parts now print in the value's own offset instead of UTC, `TZH:TZM` keeps its minutes (`+00:00`, not `+00`), and formats made only of `TZH`/`TZM` no longer fail with a `TIMESTAMP_NS -> DOUBLE` cast error.

## 0.1.0

### Minor Changes

- [#1](https://github.com/samlaycock/localflake/pull/1) [`c8c95c9`](https://github.com/samlaycock/localflake/commit/c8c95c903ec208b2acb1b5d513bd96bbf29aa4e0) Thanks [@samlaycock](https://github.com/samlaycock)! - First release of localflake, a local Snowflake emulator on DuckDB, published as a Docker image (`ghcr.io/samlaycock/localflake`) for linux/amd64 and linux/arm64. It speaks the protocol of Snowflake's official drivers and covers Snowflake SQL, semi-structured data, transactions, stages, `PUT`/`GET` and `COPY INTO`.

### Patch Changes

- [`dbd2f0c`](https://github.com/samlaycock/localflake/commit/dbd2f0cf9efca75838374614edff48afbe2f4848) Thanks [@samlaycock](https://github.com/samlaycock)! - Fix driver-facing bugs found by the new shared driver conformance suite: `?` binds inside casts, text with a UTC offset inserted into `TIMESTAMP_LTZ` columns, `TIMESTAMP_LTZ` and `TIMESTAMP_TZ` binds (types and offsets), `CREATE OR REPLACE SCHEMA` on an existing schema, the error code for objects that already exist (002002), async query failures as reported to the Node.js driver, and server logging, which was silent by default.
