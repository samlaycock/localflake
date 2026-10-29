# Driver conformance spec

`cases.json` is shared by every driver suite (`tests/python`, `tests/node`,
`tests/go`). Each suite has a small runner that executes every case through its
driver and compares the results in a canonical form, so all drivers get the
same coverage, and a new case covers every driver at once.

Driver-specific APIs that SQL can't express (bind styles, cursors, streaming,
file streams) are tested in each suite's own tests; see `tests/README.md` for
the checklist every suite covers.

## Cases

```jsonc
{
  "name": "types/fixed",            // unique; runners use it as the test name
  "connect": {                      // optional login options (defaults: database CONFORMANCE)
    "database": "X", "schema": "Y", "role": "R", "warehouse": "W",
    "session_parameters": { "TIMEZONE": "UTC" }
  },
  "files": { "a.csv": "1,x\n" },    // optional: written to a temp dir before the case
  "skip": { "node": "why" },        // optional: drivers (or "driver/FORMAT", e.g. "go/JSON") that can't run it, and why
  "steps": [ /* run in order, on one connection */ ]
}
```

Each case runs on a new connection. Cases must not depend on each other, so
they create their objects with `CREATE OR REPLACE`.

## Steps

| Field | Meaning |
|---|---|
| `sql` | The statement. `{tmpdir}` is replaced with the case's temp directory. |
| `binds` | Positional bind values for `?` / `:n` (see Values). |
| `bind_rows` | Rows of bind values, executed as one array-bound statement. |
| `statements` | Run as a multi-statement request with this statement count. |
| `async` | Submit asynchronously and fetch the result by query ID. |
| `describe` | Describe only: check `columns`, fetch no rows. |
| `columns` | Expected result columns: `[name, type]`, e.g. `["N", "FIXED(10,2)"]`. A precision of `*` isn't checked. |
| `rows` | Expected rows, in order, as canonical values. |
| `row_count` | Expected number of rows, without checking them. |
| `rowcount` | Expected affected-row count of DML. |
| `results` | Multi-statement: one `{columns, rows, rowcount}` per statement. |
| `error` | Expected failure: `{"code": "002003", "sqlstate": "42S02"}`. `sqlstate` is optional (drivers build async errors from the query status, which has none). |
| `files_after` | Files expected in the temp dir afterwards, with their content (after `GET`). Runners create their directories before the step. |

A step with no expectations only has to succeed.

## Types and canonical values

Types are Snowflake's: `FIXED(p,s)`, `REAL`, `TEXT`, `BOOLEAN`, `DATE`, `TIME`,
`TIMESTAMP_NTZ`, `TIMESTAMP_LTZ`, `TIMESTAMP_TZ`, `BINARY`, `VARIANT`,
`OBJECT`, `ARRAY`, `VECTOR`. Runners convert each driver's native value, using the
column's type, to:

| Type | Canonical value |
|---|---|
| any SQL NULL | `null` |
| `FIXED` | string with exactly the column's scale: `"1.50"`, `"-7"` |
| `REAL` | number, or `"NaN"`, `"Infinity"`, `"-Infinity"` |
| `TEXT` | string |
| `BOOLEAN` | `true` / `false` |
| `DATE` | `"2024-02-29"` |
| `TIME` | `"10:11:12.5"` (fraction without trailing zeros, omitted if zero) |
| `TIMESTAMP_NTZ` | `"2024-01-02 03:04:05.5"` |
| `TIMESTAMP_LTZ` | the instant in UTC: `"2024-07-01 12:00:00Z"` |
| `TIMESTAMP_TZ` | wall time and offset: `"2024-03-15 10:11:12+02:00"` |
| `BINARY` | lower-case hex: `"6162"` |
| `VARIANT`, `OBJECT`, `ARRAY` | compact JSON text with sorted keys: `"{\"a\":[1,2]}"`; JSON null is `"null"` |
| `VECTOR` | compact JSON array of the elements: `"[0.5,-2.5]"` |

Bind values are `{"type": "<TYPE>", "value": <canonical value>}` (or
`{"type": "NULL"}`); each runner binds the driver's native type for it.
