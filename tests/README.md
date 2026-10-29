# Driver integration tests

localflake officially supports three Snowflake drivers, and each one has an
integration suite that runs against a real localflake server:

| Driver | Suite | Version | Result formats tested |
|---|---|---|---|
| Python: `snowflake-connector-python` | `tests/python` (pytest, uv) | 5.x (pinned in `uv.lock`) | Arrow, JSON |
| Node.js: `snowflake-sdk` | `tests/node` (`node --test`) | 2.x (pinned in `package-lock.json`) | JSON (the driver's only format) |
| Go: `gosnowflake` | `tests/go` (`go test`) | v2 (pinned in `go.mod`) | Arrow, JSON |

```sh
cargo build -p localflake-server            # the suites start target/debug/localflake
(cd tests/python && uv run pytest)
(cd tests/node && npm ci && npm test)
(cd tests/go && go test -count=1 ./...)   # -count=1: results depend on the server binary
```

Set `LOCALFLAKE_BIN` to test another binary.

## Shared conformance spec

Most coverage lives in `tests/conformance/cases.json`, which every suite runs
through its driver (`test_conformance.py`, `conformance.test.js`,
`conformance_test.go`). A case covers every driver at once, so the suites
can't drift apart. See `tests/conformance/README.md` for the format.

The spec covers:

- connecting with database, schema, role, warehouse and session parameters
- every Snowflake type, with its column metadata and edge values, in both result formats
- binds of every type: scalar, `?` and `:n` placeholders, and array binding
- DML row counts, `MERGE` counts and DDL status messages
- empty, large (100,000-row) and `RESULT_SCAN` results
- multi-statement requests, async queries and describe-only requests
- error codes, SQLSTATEs and query IDs, and that a session survives an error
- transactions (rollback, commit after a failed statement, `AUTOCOMMIT = FALSE`)
- `ALTER SESSION` and session variables
- stages: `PUT`, `LIST`, `COPY INTO`, unload, `GET`, and querying staged files

## Driver-specific tests

Driver APIs that SQL can't express are tested in each suite's own files
(`test_smoke.py` and `test_features.py`, `driver.test.js`, `driver_test.go`):

| Driver API | Python | Node.js | Go |
|---|---|---|---|
| Native type decoding | `test_arrow_types_round_trip`, `test_create_insert_select` | `select with types` | `TestArrowTypes` |
| Bind styles | qmark, numeric and pyformat (`test_qmark_and_numeric_binds`, `test_pyformat_binds`) | `?` and `:n` (spec) | `?` and `:n` (spec) |
| Typed date/time binds | spec (`datetime`, `(type, value)` tuples) | spec (`Date`) | spec (type markers) |
| Array binding | `test_executemany_array_binds` | `array binds` | spec (`sf.Array`) |
| Streaming large results | `test_large_result` | `large result streams` | `TestLargeResult` |
| Transaction API | `test_transactions` (`commit`, `rollback`, `autocommit`) | none (SQL only) | `TestTransactionAPI` (`BeginTx`) |
| Query IDs | spec (`sfqid`) | spec (`getQueryId`) | `TestQueryID` (`WithQueryIDChan`) |
| Async API | spec (`execute_async`) | spec (`asyncExec`) | spec (`WithAsyncMode`) |
| Multi-statement API | spec (`num_statements`, `nextset`) | spec (`MULTI_STATEMENT_COUNT`, `NextResult`) | spec (`WithMultiStatement`) |
| Describe-only | spec (`describe`) | spec (`describeOnly`) | spec (`WithDescribeOnly`) |
| `PUT`/`GET` | `test_put_copy_get_roundtrip` and spec | `put, copy and get through a stage` and spec | `TestStages` and spec |

A new driver API gets a test in every suite whose driver has it.

## Driver differences

These are differences between the drivers themselves, not localflake. The
spec skips the affected cases, with the reason, for those drivers:

- **Node.js** sends `Date` binds as text and can't bind `TIMESTAMP_TZ`. It
  reads Snowflake's `'inf'` FLOAT text as `NaN`, and can't tell JSON null from
  SQL NULL in VARIANT values. It renders NULL as the text `"NULL"` for values
  it returns as text: `TIME`, and numbers under `fetchAsString`.
- **Python** (the Rust-core connector) sends `TIMESTAMP_TZ` binds without
  their offset, and builds async errors without a SQLSTATE.
- **Go**'s JSON-format parser misreads timestamps before 1970 that have a
  fraction of a second; its Arrow format (the default) is right.

## Not yet covered

- Query cancellation and statement timeouts: localflake runs queries
  synchronously and ignores abort requests.
- Session keep-alive heartbeats and token renewal.
- Stream-based `PUT`/`GET` (Python's `file_stream`, Go's `WithFilePutStream`).
- Arrow batch and pandas APIs (`fetch_pandas_all`, Go's Arrow batches).
- Older driver versions: each suite tests one version, notably not Python's
  pre-5.x pure-Python connector.
