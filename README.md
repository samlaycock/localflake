# localflake

A local Snowflake emulator, written in Rust on top of DuckDB. It implements the
HTTP protocol Snowflake's official drivers speak, so applications connect by
pointing the driver at localflake instead of `<account>.snowflakecomputing.com`.

Scope: core storage and querying with Snowflake SQL semantics, plus stages and
data loading. Streams, tasks, time travel, non-SQL UDFs and Snowpark are out of
scope for now.

## Running

With Docker (images for linux/amd64 and linux/arm64):

```sh
docker run --rm -p 8080:8080 ghcr.io/samlaycock/localflake            # in-memory
docker run --rm -p 8080:8080 -v localflake:/data \
  -e LOCALFLAKE_DATA_DIR=/data ghcr.io/samlaycock/localflake          # persistent
```

Every flag has an environment variable (`LOCALFLAKE_ADDR`, `LOCALFLAKE_DATA_DIR`,
`LOCALFLAKE_RESULT_FORMAT`, `LOCALFLAKE_MISSING_DATABASE`); see `--help`.

From source:

```sh
cargo run -- --addr 127.0.0.1:8080                 # in-memory
cargo run -- --data-dir ./data                     # one DuckDB file per database
RUST_LOG=localflake_engine=debug cargo run         # log translated SQL
```

Time zones use DuckDB's ICU extension, which is embedded in the binary at
build time, so localflake never needs the network at runtime. The build takes
it from `LOCALFLAKE_ICU_EXTENSION` (a path, for offline builds), DuckDB's own
cache in `~/.duckdb/extensions`, or a one-off download from
extensions.duckdb.org. If none is available the build still succeeds and
localflake installs ICU on first start instead; `LOCALFLAKE_NO_ICU=1` runs
everything in UTC.

Connect with any credentials:

```python
import snowflake.connector

conn = snowflake.connector.connect(
    account="localflake", user="dev", password="dev",
    host="127.0.0.1", port=8080, protocol="http",
    database="MY_DB",  # created on first login
)
```

## Layout

| Crate | Role |
|---|---|
| `localflake-server` | Driver protocol over HTTP (axum): login, sessions, `query-request`, response encoding |
| `localflake-engine` | DuckDB sessions, Snowflake databases as attached DuckDB catalogs, result encoding, error code mapping |
| `localflake-sql` | Snowflake SQL → DuckDB: identifier case normalization, parsing (sqlparser-rs), statement classification and rewrites |

Requests flow server → engine → sql → DuckDB, and the Arrow results go back
through the engine's encoder into Snowflake's `rowtype`/`rowset` format.

## Testing

```sh
cargo test --workspace                              # unit tests and the conformance corpus, no server
cargo build                                         # the end-to-end suites start target/debug/localflake
(cd tests/python && uv run pytest)                  # official Python connector (Arrow, via Snowflake's Rust core)
(cd tests/node && npm install && npm test)          # official Node.js driver (JSON)
(cd tests/go && go test ./...)                      # gosnowflake (Arrow, its own decoder)
```

Set `LOCALFLAKE_BIN` to test a different binary.

### Conformance corpus

`crates/localflake-engine/tests/corpus/*.test` holds Snowflake SQL with the
results Snowflake gives, grouped by area (numeric, string, date/time,
semi-structured, DML, metadata, UDFs, ...). Each file runs in a fresh session:

```text
query rowsort types
select region, sum(amount) from sales group by 1
----
VARCHAR|NUMBER(*,2)
east|30.50
```

Cases marked `gap` are known differences: they're reported but don't fail the
run, and a gap that starts passing is flagged so the marker can be removed.
There are currently none. `CORPUS_SLOW=<ms>` lists slow cases.

```sh
cargo test -p localflake-engine --test corpus -- --nocapture   # summary
CORPUS_FILTER=07 CORPUS_SHOW_GAPS=1 cargo test -p localflake-engine --test corpus -- --nocapture
```

The expected results come from Snowflake's documented behaviour. Cases that
real Snowflake disagrees with are bugs in the corpus, so please fix them.

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
  `ARRAY`, each reported with its Snowflake type (see "How Snowflake types are
  stored"), JSON null distinct from SQL NULL
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
  `APPROX_TOP_K`, `RATIO_TO_REPORT`, `CONDITIONAL_TRUE_EVENT`, `IGNORE NULLS`)
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
- DDL: `CREATE [OR REPLACE] [TRANSIENT] TABLE`, `CTAS`, `LIKE`, `CLONE`,
  `AUTOINCREMENT`/`IDENTITY`, comments, `ALTER TABLE ... ADD/DROP/RENAME
  COLUMN`, `SWAP WITH`, views, sequences. Snowflake-only clauses (`CLUSTER BY`,
  `DATA_RETENTION_TIME_IN_DAYS`, ...) are accepted and ignored, and
  PRIMARY KEY/UNIQUE/FOREIGN KEY are not enforced (`NOT NULL` is)
- Metadata: `SHOW DATABASES|SCHEMAS|TABLES|VIEWS|OBJECTS|COLUMNS|SEQUENCES|
  PARAMETERS|...` and `DESCRIBE TABLE|VIEW|DATABASE|SCHEMA` with Snowflake's
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

## Roadmap

1. **Record the corpus against real Snowflake:** the expected results are
   written from Snowflake's documentation; running them against a Snowflake
   account would catch the cases where the documentation and behaviour differ.
2. **Chunked results:** large results are returned inline; Snowflake splits them
   into chunks the driver downloads separately.
3. **More drivers in CI:** JDBC, ODBC, .NET.

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
- Timestamps have microsecond precision (Snowflake: nanosecond). TIMESTAMP_TZ
  offsets are kept in 15-minute steps.
- When a statement inside a transaction fails, DuckDB aborts the transaction
  while Snowflake keeps it open. localflake rolls back and replays the
  transaction's earlier changes into a fresh one, so it carries on as in
  Snowflake, but volatile values in those statements (`CURRENT_TIMESTAMP`,
  `RANDOM()`, sequences) are evaluated again. If a replay fails, the next
  `COMMIT` fails (SQLSTATE 40000) rather than losing writes silently.
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
- Not implemented: JavaScript/Python/Java UDFs, stored procedures, Snowflake
  Scripting, time travel, streams and tasks.
- No authentication, roles or access control.

## Releases

Releases are managed with [changesets](https://github.com/changesets/changesets).
`package.json` holds the version; `npm run version` copies it into the Cargo
workspace.

1. Every pull request includes a changeset (`npm run changeset`, or
   `npm run changeset -- --empty` for changes users won't notice). CI checks
   for one.
2. On `main`, the release workflow keeps a "chore: release" pull request open
   that applies the pending changesets: the version bump and `CHANGELOG.md`.
3. Merging that pull request builds the image natively for amd64 and arm64,
   pushes `ghcr.io/samlaycock/localflake:<version>`, `:<major>.<minor>` (and
   `:<major>` from 1.0) and `:latest`, then tags `v<version>` and creates the
   GitHub release from the changelog.

## Not affiliated with Snowflake

"Snowflake" is a trademark of Snowflake Inc. This project is independent and
interoperates with the open-source Snowflake drivers.
