# localflake

[![CI](https://github.com/samlaycock/localflake/actions/workflows/ci.yml/badge.svg)](https://github.com/samlaycock/localflake/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/samlaycock/localflake)](https://github.com/samlaycock/localflake/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A local Snowflake emulator for offline development and testing, written in Rust
on top of DuckDB. It implements the HTTP protocol Snowflake's official drivers
speak, so your application connects by pointing its driver at localflake instead
of `<account>.snowflakecomputing.com`. No Snowflake account, network access or
code changes are needed.

- **Works with the official drivers.** Python (`snowflake-connector-python`),
  Node.js (`snowflake-sdk`) and Go (`gosnowflake`) are tested in CI against a
  shared conformance spec, with both Arrow and JSON results.
- **Snowflake SQL semantics, not just syntax.** Identifier case rules, `NUMBER`
  scales, decimal division, NULL ordering, `VARIANT` paths, `TIMESTAMP_TZ`
  offsets and session time zones behave as they do in Snowflake, and errors
  carry Snowflake's error codes and SQLSTATEs.
- **Several hundred functions and the query syntax apps use**, including
  `QUALIFY`, `FLATTEN`, `PIVOT`, `MATCH_RECOGNIZE`, `CONNECT BY`, `MERGE`,
  `ASOF JOIN` and SQL UDFs.
- **Stages and data loading.** `PUT`/`GET` through the drivers, `COPY INTO`
  tables and stages for CSV, JSON and Parquet, and queries over staged files.
- **Transactions, multi-statement requests, async queries, bind variables and
  `RESULT_SCAN`.**
- **One binary or Docker image, in memory or persistent.** Optionally keeps one
  DuckDB file per database, and never needs the network at runtime.

> [!WARNING]
> localflake is for development and testing only. It accepts any credentials,
> has no access control, and is not a database for real data. It aims to behave
> like Snowflake for the features it implements, but it is not Snowflake: test
> against a real account before relying on behaviour in production.

## Contents

- [Installation](#installation)
- [Quick start](#quick-start)
- [Supported drivers](#supported-drivers)
- [What works](#what-works)
- [Stages and file transfer](#stages-and-file-transfer)
- [How Snowflake types are stored](#how-snowflake-types-are-stored)
- [Known differences](#known-differences)
- [Configuration](#configuration)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)

## Installation

**Docker** (linux/amd64 and linux/arm64):

```sh
docker run --rm -p 8080:8080 ghcr.io/samlaycock/localflake            # in memory
docker run --rm -p 8080:8080 -v localflake:/data \
  -e LOCALFLAKE_DATA_DIR=/data ghcr.io/samlaycock/localflake          # persistent
```

The image is tagged `:<version>`, `:<major>.<minor>` (and `:<major>` from 1.0)
and `:latest`. It runs as a non-root user and listens on `0.0.0.0:8080`. Pass
flags after the image name, e.g. `docker run … ghcr.io/samlaycock/localflake
--missing-database ignore`.

**From source** (needs a recent stable Rust toolchain and a C++ compiler):

```sh
cargo install --git https://github.com/samlaycock/localflake localflake-server --locked
localflake
```

This compiles DuckDB from source, which takes several minutes. Or clone the
repository and run `cargo run --release`. [CONTRIBUTING.md](CONTRIBUTING.md)
explains how to link DuckDB's prebuilt library instead.

## Quick start

Start localflake with either command above. It serves `http://127.0.0.1:8080`.

Point your driver at it with `host`, `port` and `protocol`, and use any account,
user and password. The database you log in with is created if it doesn't exist.

**Python** (`snowflake-connector-python`):

```python
import snowflake.connector

conn = snowflake.connector.connect(
    account="localflake", user="dev", password="dev",
    host="127.0.0.1", port=8080, protocol="http",
    database="MY_DB",
)
print(conn.cursor().execute("select current_database(), 1 / 3").fetchone())
# ('MY_DB', Decimal('0.333333'))
```

**Node.js** (`snowflake-sdk`):

```js
import snowflake from "snowflake-sdk";

const conn = snowflake.createConnection({
  account: "localflake", username: "dev", password: "dev",
  accessUrl: "http://127.0.0.1:8080",
  database: "MY_DB",
});
await conn.connectAsync();
```

**Go** (`gosnowflake`):

```go
// import sf "github.com/snowflakedb/gosnowflake/v2"
dsn, _ := sf.DSN(&sf.Config{
	Account: "localflake", User: "dev", Password: "dev",
	Host: "127.0.0.1", Port: 8080, Protocol: "http",
	Database: "MY_DB",
})
db, _ := sql.Open("snowflake", dsn)
```

In Docker Compose or CI, run the image as a service and use its hostname as
`host`.

## Supported drivers

| Driver | Tested version | Result formats |
|---|---|---|
| Python: `snowflake-connector-python` | 5.x | Arrow, JSON |
| Node.js: `snowflake-sdk` | 2.x | JSON (the driver's only format) |
| Go: `gosnowflake` | v2 | Arrow, JSON |

These run in CI against every change. Other drivers that speak the same
protocol (JDBC, ODBC, .NET) may work but aren't tested yet. See
[`tests/README.md`](tests/README.md) for what the suites cover and the known
differences between drivers.

## What works

**Protocol**
- Login with any credentials, token renewal, heartbeat, logout, telemetry
- Results as JSON or Arrow (`rowsetBase64`). Arrow is used for drivers that
  request it from Snowflake (Python, Go, JDBC, ODBC, .NET); override with
  `--result-format json|arrow`. DML, DDL and `SHOW` results are always JSON,
  as with Snowflake
- Bind variables (qmark `?`, numeric `:1`) including array binds for
  `executemany`, with DML row counts in both `rowset` and `stats`
- Multi-statement requests (`MULTI_STATEMENT_COUNT` / `num_statements`) with
  per-statement child results
- Async queries (`asyncExec`, `/monitoring/queries/{id}`, `/queries/{id}/result`)
- Cancellation: driver abort requests (by request ID or query ID),
  `SYSTEM$CANCEL_QUERY` and `STATEMENT_TIMEOUT_IN_SECONDS`, failing with
  Snowflake's 000604 and 000630 errors
- `describeOnly` requests, `RESULT_SCAN(LAST_QUERY_ID())`

**SQL**
- Snowflake identifier rules: unquoted names upper-cased, quoted names keep
  case, VARIANT path keys (`v:Foo.bar`) case-sensitive
- Databases as DuckDB catalogs with a `PUBLIC` schema; `CREATE/DROP/USE
  DATABASE|SCHEMA`, `CLONE`, three-part names; `--data-dir` persists one DuckDB
  file per database
- Types: `NUMBER(p,s)`, `INT` family, `FLOAT`, `VARCHAR(n)`/`STRING`/`CHAR(n)`,
  `BINARY(n)`, `BOOLEAN`, `DATE`, `TIME`, `TIMESTAMP_NTZ/LTZ/TZ` (honouring
  `TIMESTAMP_TYPE_MAPPING` and the session `TIMEZONE`), `VARIANT`, `OBJECT`,
  `ARRAY`, `VECTOR(INT|FLOAT, n)`, each reported with its Snowflake type (see
  "How Snowflake types are stored"), JSON null distinct from SQL NULL
- Vectors: casts from and to `ARRAY`, and `VECTOR_COSINE_SIMILARITY`,
  `VECTOR_INNER_PRODUCT`, `VECTOR_L1_DISTANCE` and `VECTOR_L2_DISTANCE`
- Collations (`COLLATE 'en-ci'`, `'en-ai'`, `COLLATE(...)`) for case- and
  accent-insensitive comparison, `COMPRESS`/`DECOMPRESS_*`, `SYSTEM$TYPEOF`
- `MATCH_RECOGNIZE` (see "Known differences" for its scope),
  `CONDITIONAL_CHANGE_EVENT`, `NTH_VALUE ... FROM LAST`
- Semi-structured data: `:` and `[]` paths, `::` casts, `GET_PATH`,
  `PARSE_JSON`, `OBJECT_CONSTRUCT`, `ARRAY_CONSTRUCT`, `ARRAY_AGG`,
  `LATERAL FLATTEN` / `TABLE(FLATTEN(...))` with Snowflake's output columns,
  `OUTER`, `PATH` and `MODE`
- Several hundred functions across every category of Snowflake's function
  reference: conditional (`IFF`, `DECODE`, `NVL2`, `BOOLAND`, ...), numeric
  (`DIV0`, `WIDTH_BUCKET`, `HAVERSINE`, bitwise, trigonometric), string
  (`REGEXP_*` with positions, occurrences and flags, `SPLIT_PART`, `STRTOK`,
  `INITCAP` with Snowflake's delimiters, `SOUNDEX`, `JAROWINKLER_SIMILARITY`,
  `SHA2`, `PARSE_URL`, ...), date/time (`DATEADD`/`DATEDIFF` on week
  boundaries, `MONTHS_BETWEEN`, `NEXT_DAY`, `TIME_SLICE`, `*_FROM_PARTS` with
  carry-over, `CONVERT_TIMEZONE` with the IANA database), semi-structured
  (`ARRAY_*`, `OBJECT_*`, `ARRAYS_ZIP`, `GET_IGNORE_CASE`, recursive
  `FLATTEN`), aggregate and window (`MEDIAN`, `MODE`, `MAX_BY`,
  `APPROX_TOP_K`, `RATIO_TO_REPORT`, `CONDITIONAL_TRUE_EVENT`,
  `INTERPOLATE_FFILL|BFILL|LINEAR`, `IGNORE NULLS`)
- Sketches and their states: `HLL_ACCUMULATE|COMBINE|ESTIMATE|EXPORT|IMPORT`,
  `APPROX_PERCENTILE_*` and `APPROX_TOP_K_*` states, `MINHASH`,
  `MINHASH_COMBINE` and `APPROXIMATE_SIMILARITY`, and the bitmap functions
  (`BITMAP_CONSTRUCT_AGG`, `BITMAP_OR_AGG`, `BITMAP_AND_AGG`, `BITMAP_COUNT`,
  ...) in Snowflake's documented bitmap layout
- XML (`PARSE_XML`, `XMLGET`, `CHECK_XML`, `TO_XML`, with XML values printing
  as XML), `PARSE_IP`, `SEARCH_IP` and `SEARCH` (all three analyzers and
  search modes), `ENCRYPT_RAW`/`DECRYPT_RAW` (standard AES in GCM, CBC, ECB,
  CTR, OFB and CFB modes) and `ENCRYPT`/`DECRYPT`
- Context functions such as `CURRENT_STATEMENT`, `CURRENT_TRANSACTION`,
  `LAST_TRANSACTION`, `GETVARIABLE` and `IS_ROLE_IN_SESSION`, and
  `SYSTEM$WAIT`, `SYSTEM$CANCEL_ALL_QUERIES` and `SYSTEM$GET_TZDB_VERSION`
- `TO_CHAR`/`TO_NUMBER` numeric format models (`9,999.99`, `$`, `S`, `MI`,
  `FM`) and date/time formats; implicit conversions to text follow the session's
  `*_OUTPUT_FORMAT` parameters (timestamps print as `2024-01-02 03:04:05.000`)
- Snowflake typing: decimal division and `AVG` use Snowflake's result scale
  (`10.00 / 3` is `3.33333333`, `AVG` of integers is `NUMBER(38,6)`), `MEDIAN`
  adds 3 to the scale, rounding is half away from zero. These depend on operand
  types, which a prepare-only type probe asks DuckDB for before translating
- Snowflake semantics: division by zero errors (100051), NULLs sort high,
  `CONCAT`/`GREATEST` with NULL are NULL, `SELECT a + 1` names its column
  `A + 1`, `FROM VALUES` columns are `COLUMN1..n` (and `$1`), `TOP n`, `MINUS`,
  `QUALIFY`, `ILIKE`, `LIKE ANY|ALL`, `$$strings$$`, lateral column aliases
- Query syntax: `PIVOT`/`UNPIVOT [INCLUDE NULLS]`, `START WITH ... CONNECT BY`
  (with `LEVEL`, `SYS_CONNECT_BY_PATH`, `CONNECT_BY_ROOT`), `ASOF JOIN ...
  MATCH_CONDITION`, `SAMPLE`/`TABLESAMPLE`, `UNION [ALL] BY NAME`,
  `SELECT * EXCLUDE|RENAME|REPLACE|ILIKE`, `GROUP BY ALL|ROLLUP|CUBE|GROUPING
  SETS`, recursive CTEs, `OBJECT_CONSTRUCT(*)`
- DML: `MERGE` with per-action row counts, `UPDATE ... FROM`, `DELETE ...
  USING`, `INSERT OVERWRITE`, multi-table `INSERT ALL|FIRST` with `WHEN`/`ELSE`
- SQL user-defined functions, scalar and table (`RETURNS TABLE`), with default
  arguments, stored as DuckDB macros
- Snowflake Scripting: anonymous blocks (`DECLARE ... BEGIN ... EXCEPTION ...
  END`) and `EXECUTE IMMEDIATE` (strings, variables, `$session_variables`,
  `USING`), with typed variables, `LET`, `IF`, `CASE`, `FOR`, `WHILE`,
  `REPEAT`, `LOOP`, labelled `BREAK`/`CONTINUE`, `RETURN`, `SELECT ... INTO`,
  `:variable` binds in SQL, `SQLROWCOUNT`/`SQLFOUND`/`SQLNOTFOUND`/
  `ACTIVITY_COUNT`, declared exceptions, `RAISE`, `EXIT` and `CONTINUE`
  handlers with `SQLCODE`/`SQLERRM`/`SQLSTATE`, and Snowflake's "Uncaught
  exception ... on line N at position M" errors
- DDL: `CREATE [OR REPLACE] [TRANSIENT] TABLE`, `CTAS`, `LIKE`, `CLONE`,
  `AUTOINCREMENT`/`IDENTITY`, comments, `ALTER TABLE ... ADD/DROP/RENAME
  COLUMN`, `SWAP WITH`, views, materialized views, dynamic tables, `ALTER
  [MATERIALIZED] VIEW | DYNAMIC TABLE` (rename, comments; refresh, suspend and
  resume are accepted), sequences. Snowflake-only clauses (`CLUSTER BY`,
  `DATA_RETENTION_TIME_IN_DAYS`, ...) are accepted and ignored, and
  PRIMARY KEY/UNIQUE/FOREIGN KEY are not enforced (`NOT NULL` is)
- Metadata: `SHOW DATABASES|SCHEMAS|TABLES|VIEWS|MATERIALIZED VIEWS|DYNAMIC
  TABLES|OBJECTS|COLUMNS|SEQUENCES|PARAMETERS|...` and `DESCRIBE
  TABLE|VIEW|DATABASE|SCHEMA` with Snowflake's
  column layout, `INFORMATION_SCHEMA.TABLES|COLUMNS|SCHEMATA|...`
- Session: `ALTER SESSION SET/UNSET`, session variables (`SET x = ...`,
  `$x`), `CURRENT_DATABASE()` and friends
- Transactions: `BEGIN`/`COMMIT`/`ROLLBACK`, `AUTOCOMMIT = FALSE` with
  implicit transactions, DDL committing the open transaction
- Stages: named internal stages (`CREATE [TEMPORARY] STAGE`, `ALTER`, `DROP`,
  `SHOW STAGES`, `DESCRIBE STAGE`), user (`@~`) and table (`@%t`) stages, named
  file formats, `LIST`/`REMOVE` with `PATTERN`, and `PUT`/`GET` through the
  official drivers (see below)
- `COPY INTO <table>` from CSV, JSON and Parquet: file format options (delimiters,
  enclosures, escapes, `SKIP_HEADER`, `NULL_IF`, `TRIM_SPACE`, date/time
  formats, gzip/deflate), `FILES`, `PATTERN`, `ON_ERROR` (`ABORT_STATEMENT`,
  `CONTINUE`, `SKIP_FILE[_n[%]]`), `FORCE`, `PURGE`, `TRUNCATECOLUMNS`,
  `MATCH_BY_COLUMN_NAME`, `VALIDATION_MODE`, transformations
  (`FROM (SELECT $1, t.$2 ... FROM @s t)`), Snowflake's per-file result rows and
  load history (unchanged files aren't loaded twice)
- `COPY INTO @stage` unloads to CSV, JSON or Parquet (`HEADER`, `SINGLE`,
  `OVERWRITE`, `DETAILED_OUTPUT`, gzip by default), and `SELECT $1, metadata$filename
  FROM @stage (FILE_FORMAT => 'f', PATTERN => '...')` queries staged files
- Warehouse, role, grant and other account-level statements are accepted as
  no-ops
- DuckDB errors mapped to Snowflake error codes and SQLSTATEs

## Stages and file transfer

Staged files live under `<data_dir>/stages` (or a temp directory for in-memory
runs, removed on exit). For `PUT` and `GET`, Snowflake tells the driver where
the files go and the driver moves them itself. localflake answers as a GCS stage
with presigned URLs that point back at its own `/__localflake/files/` endpoint,
because every official driver supports that and it works when localflake runs in
a container. Presigned URLs use the `Host` the driver connected with.

A named stage with `URL = 'file:///some/dir'` reads and writes that local
directory, which is handy for fixtures (a localflake extension; other external
URLs such as `s3://` are rejected).

## How Snowflake types are stored

| Snowflake | DuckDB storage | Notes |
|---|---|---|
| `VARIANT` | `VARIANT` | JSON null is stored as a reserved marker string, so it stays distinct from SQL NULL; SQL NULL array elements print as `undefined` |
| `OBJECT` | `JSON` | normalized through VARIANT, so keys are sorted |
| `ARRAY` | `VARIANT[]` | |
| `VECTOR(INT, n)`, `VECTOR(FLOAT, n)` | `INTEGER[n]`, `FLOAT[n]` | the only 32-bit numbers and fixed-size arrays, which is how results recognise them; sent to drivers as JSON arrays or Arrow fixed-size lists |
| `TIMESTAMP_LTZ` | `TIMESTAMPTZ` | follows the session `TIMEZONE` (default `America/Los_Angeles`) through ICU |
| `TIMESTAMP_TZ` | `TIMESTAMP_NS` | the sub-microsecond digits hold the UTC offset; registered DuckDB casts convert to and from it |
| `VARCHAR(n)`, `CHAR(n)`, `BINARY(n)` | `VARCHAR`, `BLOB` | the declared type lives in the column comment; lengths are enforced with a CHECK |
| quoted `"mixedCase"` names | encoded names | a marker after each lower-case letter makes case variants distinct, since DuckDB matches names case-insensitively |

Some results depend on operand types the translator can't see (decimal
division scales, text conversion of timestamps, TIMESTAMP_TZ arithmetic). For
those, the engine prepares the statement once with `sf_probe` markers, whose
bind callbacks record each operand's type, then translates it again with the
types known.

## Known differences

- Logging in with an unknown database creates it, which suits tests. Run with
  `--missing-database ignore` to log in without a current database instead, as
  Snowflake does.
- Result metadata reports precision 38 for `NUMBER(p,0)` columns, which are
  stored as `BIGINT` (Snowflake reports the declared precision). Scales are
  always right.
- Timestamps have microsecond precision (Snowflake: nanosecond). TIMESTAMP_TZ
  offsets are kept in 15-minute steps.
- When a statement inside a transaction fails, DuckDB aborts the transaction
  while Snowflake keeps it open. localflake rolls back and replays the
  transaction's earlier changes into a fresh one, so it carries on as in
  Snowflake, but volatile values in those statements (`CURRENT_TIMESTAMP`,
  `RANDOM()`, sequences) are evaluated again. If a replay fails, the next
  `COMMIT` fails (SQLSTATE 40000) rather than losing writes silently.
- Async queries (`asyncExec`) run to completion before localflake responds,
  so by the time a driver can abort one by query ID it has already finished.
- Statements from sessions with different TIMEZONE settings take turns (the
  TIMESTAMP_TZ casts need the zone); sessions sharing a zone run concurrently.
- Declared lengths of columns added with `ALTER TABLE ... ADD COLUMN` are
  reported but not enforced (DuckDB can't add a column with a CHECK).
- `MATCH_RECOGNIZE` covers PARTITION/ORDER BY, DEFINE with the current row and
  `PREV`/`NEXT`, greedy patterns with alternation, grouping, quantifiers,
  anchors and `PERMUTE`, `ONE ROW`/`ALL ROWS PER MATCH`, and MEASURES with
  `MATCH_NUMBER()`, `CLASSIFIER()`, `FIRST`/`LAST` and (symbol-qualified)
  aggregates. Inside DEFINE, other symbols' columns read the current row.
- Stages: files are not encrypted, `PUT` doesn't skip files whose content is
  unchanged (`OVERWRITE = FALSE` skips any existing name, and only the Rust-core
  Python driver asks), unloads write a single file regardless of
  `MAX_FILE_SIZE`, `PARTITION BY` unloads, Avro/ORC/XML and zstd/bz2/brotli
  compression aren't supported, `COPY` reports only each file's first error, and
  load history is kept in memory (lost on restart). External stages (S3, GCS,
  Azure), storage integrations, pipes and directory tables aren't emulated.
- Vector functions compute in double precision and accept vectors of different
  element types (Snowflake requires the same type and dimension; different
  dimensions are an error in both). gosnowflake can't decode `VECTOR` columns in
  Arrow results, so Go applications need JSON results
  (`GO_QUERY_RESULT_FORMAT = 'JSON'`) to read them.
- Sketch states are exact, so estimates (`HLL_ESTIMATE`,
  `APPROX_PERCENTILE_ESTIMATE`, `APPROX_TOP_K_ESTIMATE`) are exact too, and the
  states use localflake's own formats (HyperLogLog states and `HLL_EXPORT`
  output differ from Snowflake's). MinHash uses different hash functions, so
  `APPROXIMATE_SIMILARITY` estimates differ slightly. `HLL_ACCUMULATE(*)`,
  `MINHASH(k, *)` and the `*` form of `SEARCH`/`SEARCH_IP` aren't supported.
- `ENCRYPT`/`DECRYPT` derive their key differently from Snowflake (which uses an
  internal seed), so they only decrypt what localflake encrypted;
  `ENCRYPT_RAW`/`DECRYPT_RAW` are standard AES and interoperate.
- BINARY values inside a VARIANT (such as `ENCRYPT_RAW`'s fields) print as
  escaped bytes rather than hex, though `AS_BINARY(GET(...))` reads them back.
- With no access control, `IS_ROLE_IN_SESSION`, `IS_DATABASE_ROLE_IN_SESSION`
  and `IS_GRANTED_TO_INVOKER_ROLE` are always TRUE. `SEARCH`'s
  `UNICODE_ANALYZER` approximates Unicode word segmentation, and
  `SYSTEM$WAIT` can't be cancelled while it waits.
- Functions not implemented: `GET_DDL`, `MAP_*` (no MAP type), the
  `GEOGRAPHY`/`GEOMETRY` and `ST_*`/`H3_*` functions, `DECFLOAT`,
  `TO_IPADDRESS`, Cortex and AI functions, and account-level `SYSTEM$`
  functions.
- Materialized views and dynamic tables are views, so they're always up to
  date: a dynamic table never lags behind its sources, and `TARGET_LAG`,
  `WAREHOUSE` and `REFRESH_MODE` are only recorded for `SHOW DYNAMIC TABLES`
  (which reports `REFRESH_MODE = AUTO` as given, where Snowflake reports the
  mode it chose). `SHOW TABLES`, `SHOW OBJECTS` and `INFORMATION_SCHEMA.TABLES`
  list dynamic tables as tables.
- Snowflake Scripting doesn't support cursors, `RESULTSET`s or `RETURN
  TABLE(...)` yet. Expressions are evaluated as queries, so a block's
  variables round-trip through SQL literals; `SELECT ... INTO` with no rows
  sets the variables to NULL.
- Not implemented: JavaScript/Python/Java UDFs, stored procedures, time
  travel, streams and tasks.
- No authentication, roles or access control.

## Configuration

Every flag has an environment variable. Run `localflake --help` for the full
list.

| Flag | Environment variable | Default | |
|---|---|---|---|
| `--addr` | `LOCALFLAKE_ADDR` | `127.0.0.1:8080` | Address to listen on (`0.0.0.0:8080` in the Docker image) |
| `--data-dir` | `LOCALFLAKE_DATA_DIR` | unset (in memory) | Directory for persistent databases, one DuckDB file each, and staged files |
| `--result-format` | `LOCALFLAKE_RESULT_FORMAT` | `auto` | `auto` uses Arrow for drivers that request it from Snowflake, `json` or `arrow` forces one |
| `--missing-database` | `LOCALFLAKE_MISSING_DATABASE` | `create` | Logging in with an unknown database creates it, or `ignore` logs in without a current database, as Snowflake does |

Logging uses `RUST_LOG`. For example, `RUST_LOG=localflake_engine=debug` logs
the DuckDB SQL each statement is translated to.

### Time zones

Time zones use DuckDB's ICU extension, which is embedded in the binary at build
time, so localflake never needs the network at runtime. The build takes it from
`LOCALFLAKE_ICU_EXTENSION` (a path, for offline builds), DuckDB's own cache in
`~/.duckdb/extensions`, or a one-off download from extensions.duckdb.org. If
none is available the build still succeeds and localflake installs ICU on first
start instead. `LOCALFLAKE_NO_ICU=1` runs everything in UTC.

## Roadmap

1. **Record the corpus against real Snowflake:** the expected results are
   written from Snowflake's documentation; running them against a Snowflake
   account would catch the cases where the documentation and behaviour differ.
2. **Chunked results:** large results are returned inline; Snowflake splits them
   into chunks the driver downloads separately.
3. **More drivers in CI:** JDBC, ODBC, .NET.

## Contributing

Issues and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for
how to build, test and release, and how the SQL conformance corpus works.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

## Not affiliated with Snowflake

"Snowflake" is a trademark of Snowflake Inc. This project is independent and
interoperates with the open-source Snowflake drivers.
