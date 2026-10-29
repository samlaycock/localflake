// Runs the shared driver conformance spec (tests/conformance) through the
// Node.js driver, which receives JSON results.
//
// snowflake-sdk differences the runner bridges:
// - It can't send arbitrary session parameters at login, so a case's
//   `session_parameters` are applied with ALTER SESSION right after connecting.
// - Numbers are fetched as strings (`fetchAsString: ["Number"]`): JavaScript
//   numbers can't hold NUMBER(38,0) exactly.
// - TIME values are text in TIME_OUTPUT_FORMAT, so the runner sets a format
//   with nanoseconds.

import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import snowflake from "snowflake-sdk";

const repoRoot = path.resolve(import.meta.dirname, "../..");
const spec = JSON.parse(fs.readFileSync(path.join(repoRoot, "tests/conformance/cases.json"), "utf8"));
let server;
let port;

snowflake.configure({ logLevel: "OFF" });

before(async () => {
  port = await new Promise((resolve) => {
    const srv = net.createServer().listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
  const binary = process.env.LOCALFLAKE_BIN ?? path.join(repoRoot, "target/debug/localflake");
  server = spawn(binary, ["--addr", `127.0.0.1:${port}`], { stdio: "inherit" });
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const ok = await new Promise((resolve) => {
      const sock = net.connect(port, "127.0.0.1", () => resolve(sock.end() && true));
      sock.on("error", () => resolve(false));
    });
    if (ok) return;
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error("localflake did not start");
});

after(() => server.kill());

for (const testCase of spec.cases) {
  // Node.js always receives JSON results.
  const skip = testCase.skip?.node ?? testCase.skip?.["node/JSON"];
  test(testCase.name, { skip }, async () => {
    const tmpdir = fs.mkdtempSync(path.join(os.tmpdir(), "localflake-conformance-"));
    for (const [name, content] of Object.entries(testCase.files ?? {})) {
      fs.writeFileSync(path.join(tmpdir, name), content);
    }
    const options = testCase.connect ?? {};
    const conn = snowflake.createConnection({
      account: "localflake",
      username: "conformance",
      password: "conformance",
      accessUrl: `http://127.0.0.1:${port}`,
      database: options.database ?? "CONFORMANCE",
      schema: options.schema,
      role: options.role,
      warehouse: options.warehouse,
    });
    await conn.connectAsync();
    try {
      // TIME values arrive as text in TIME_OUTPUT_FORMAT, which hides fractions by default.
      await execute(conn, { sqlText: "alter session set time_output_format = 'HH24:MI:SS.FF9'" });
      for (const [key, value] of Object.entries(options.session_parameters ?? {})) {
        await execute(conn, { sqlText: `alter session set ${key} = '${value}'` });
      }
      for (const [index, step] of testCase.steps.entries()) {
        try {
          await runStep(conn, step, tmpdir);
        } catch (err) {
          err.message = `step ${index + 1} (${step.sql}): ${err.message}`;
          throw err;
        }
      }
    } finally {
      await new Promise((resolve) => conn.destroy(resolve));
      fs.rmSync(tmpdir, { recursive: true, force: true });
    }
  });
}

/** Executes a statement; resolves with `{ stmt, rows }` or rejects with the driver's error. */
function execute(conn, options) {
  return new Promise((resolve, reject) => {
    conn.execute({
      fetchAsString: ["Number"],
      rowMode: "array",
      ...options,
      complete: (err, stmt, rows) => (err ? reject(err) : resolve({ stmt, rows })),
    });
  });
}

/** Every result of a multi-statement request, in order. */
function executeMulti(conn, options) {
  return new Promise((resolve, reject) => {
    const results = [];
    conn.execute({
      fetchAsString: ["Number"],
      rowMode: "array",
      ...options,
      complete: (err, stmt, rows) => {
        if (err) return reject(err);
        results.push({ stmt, rows });
        if (stmt.hasNext()) stmt.NextResult();
        else resolve(results);
      },
    });
  });
}

function collect(stream) {
  return new Promise((resolve, reject) => {
    const rows = [];
    stream.on("data", (row) => rows.push(row));
    stream.on("end", () => resolve(rows));
    stream.on("error", reject);
  });
}

async function runStep(conn, step, tmpdir) {
  const sqlText = step.sql.replaceAll("{tmpdir}", tmpdir);
  for (const name of Object.keys(step.files_after ?? {})) {
    fs.mkdirSync(path.dirname(path.join(tmpdir, name)), { recursive: true });
  }
  let outcome;
  try {
    if (step.describe) {
      outcome = await execute(conn, { sqlText, describeOnly: true });
    } else if (step.bind_rows) {
      outcome = await execute(conn, { sqlText, binds: step.bind_rows.map((row) => row.map(bind)) });
    } else if (step.async) {
      const { stmt } = await execute(conn, { sqlText, asyncExec: true, binds: (step.binds ?? []).map(bind) });
      const result = await conn.getResultsFromQueryId({ queryId: stmt.getQueryId(), fetchAsString: ["Number"], rowMode: "array" });
      outcome = { stmt: result, rows: await collect(result.streamRows()) };
    } else if (step.statements) {
      outcome = await executeMulti(conn, { sqlText, parameters: { MULTI_STATEMENT_COUNT: step.statements } });
    } else {
      outcome = await execute(conn, { sqlText, binds: step.binds?.map(bind) });
    }
  } catch (err) {
    assert.ok(step.error, `unexpected error ${err.code}: ${err.message}`);
    assert.equal(err.code, step.error.code, `error code (${err.message})`);
    if (step.error.sqlstate) assert.equal(err.sqlState, step.error.sqlstate, `sqlstate (${err.message})`);
    // Async errors come from the query status; the caller already has the query ID.
    if (!step.async) assert.ok(err.data?.queryId, "errors carry a query ID");
    return;
  }
  assert.ok(!step.error, `expected error ${JSON.stringify(step.error)}`);

  if (step.results) {
    assert.equal(outcome.length, step.results.length, "number of results");
    step.results.forEach((expected, i) => checkResult(outcome[i].stmt, outcome[i].rows, expected));
  } else if (step.describe) {
    checkColumns(outcome.stmt.getColumns(), step.columns);
  } else {
    checkResult(outcome.stmt, outcome.rows, step);
  }

  for (const [name, content] of Object.entries(step.files_after ?? {})) {
    const file = path.join(tmpdir, name);
    assert.ok(fs.existsSync(file), `${name} was not downloaded`);
    assert.equal(fs.readFileSync(file, "utf8"), content, name);
  }
}

function checkResult(stmt, rows, expected) {
  const columns = stmt.getColumns() ?? [];
  if (expected.columns) checkColumns(columns, expected.columns);
  if (expected.rowcount !== undefined) assert.equal(stmt.getNumUpdatedRows(), expected.rowcount, "rowcount");
  if (expected.row_count !== undefined) assert.equal(rows.length, expected.row_count, "number of rows");
  if (expected.rows) {
    const types = columns.map(columnType);
    const actual = rows.map((row) => row.map((value, i) => canonical(value, types[i])));
    assert.equal(actual.length, expected.rows.length, `rows ${JSON.stringify(actual)}`);
    actual.forEach((row, i) => {
      const want = expected.rows[i];
      const same = row.length === want.length && row.every((v, j) => valuesEqual(v, want[j]));
      assert.ok(same, `row ${JSON.stringify(row)}, expected ${JSON.stringify(want)}`);
    });
  }
}

function columnType(column) {
  const type = column.getType().toUpperCase();
  return type === "FIXED" ? `FIXED(${column.getPrecision()},${column.getScale()})` : type;
}

function checkColumns(columns, expected) {
  const actual = columns.map((c) => [c.getName(), columnType(c)]);
  assert.equal(actual.length, expected.length, `columns ${JSON.stringify(actual)}`);
  actual.forEach(([name, type], i) => {
    const [wantName, wantType] = expected[i];
    assert.equal(name, wantName, "column name");
    const matches = wantType.startsWith("FIXED(*,")
      ? type.startsWith("FIXED(") && type.split(",")[1] === wantType.split(",")[1]
      : type === wantType;
    assert.ok(matches, `column ${name} type ${type}, expected ${wantType}`);
  });
}

function valuesEqual(actual, expected) {
  if (typeof expected === "number") return typeof actual === "number" && actual === expected;
  return actual === expected;
}

const pad = (n, width = 2) => String(n).padStart(width, "0");

function fraction(seconds, nanos) {
  let text = pad(seconds);
  if (nanos) text += `.${pad(nanos, 9)}`.replace(/0+$/, "");
  return text;
}

/** The fixed-point string with exactly `scale` fractional digits. */
function fixed(text, scale) {
  const negative = text.startsWith("-");
  const [whole, frac = ""] = text.replace(/^-/, "").split(".");
  const digits = scale > 0 ? `${whole}.${frac.padEnd(scale, "0").slice(0, scale)}` : whole;
  return negative ? `-${digits}` : digits;
}

function canonical(value, type) {
  if (value === null || value === undefined) return null;
  const base = type.split("(")[0];
  // snowflake-sdk renders NULL as the text "NULL" for values it returns as text
  // (TIME, and numbers under fetchAsString).
  if (value === "NULL" && ["FIXED", "REAL", "TIME"].includes(base)) return null;
  switch (base) {
    case "FIXED":
      return fixed(String(value), Number(type.slice(0, -1).split(",")[1]));
    case "REAL": {
      const n = typeof value === "number" ? value : Number(value);
      if (Number.isNaN(n)) return "NaN";
      if (!Number.isFinite(n)) return n > 0 ? "Infinity" : "-Infinity";
      return n;
    }
    case "DATE":
      return `${value.getUTCFullYear()}-${pad(value.getUTCMonth() + 1)}-${pad(value.getUTCDate())}`;
    case "TIME": {
      const [hms, frac = ""] = String(value).split(".");
      const trimmed = frac.replace(/0+$/, "");
      return trimmed ? `${hms}.${trimmed}` : hms;
    }
    case "TIMESTAMP_NTZ":
    case "TIMESTAMP_LTZ":
    case "TIMESTAMP_TZ":
      return timestamp(value, base);
    case "BINARY":
      return Buffer.from(value).toString("hex");
    case "VARIANT":
    case "OBJECT":
    case "ARRAY":
    case "VECTOR":
      return stableJson(typeof value === "string" ? JSON.parse(value) : value);
    default:
      return value;
  }
}

/**
 * snowflake-sdk returns timestamps as Date objects extended with
 * the value's nanoseconds (`getNanoSeconds`) and, for TIMESTAMP_TZ, its
 * offset in minutes (`getTimezone`).
 */
function timestamp(value, base) {
  const nanos = value.getNanoSeconds();
  let offset = 0;
  if (base === "TIMESTAMP_TZ") offset = value.getTimezone();
  // Wall time: UTC for NTZ, UTC for LTZ (canonical form), UTC + offset for TZ.
  const wall = new Date(value.getTime() + offset * 60_000);
  const time = `${pad(wall.getUTCHours())}:${pad(wall.getUTCMinutes())}:${fraction(wall.getUTCSeconds(), nanos)}`;
  const date = `${pad(wall.getUTCFullYear(), 4)}-${pad(wall.getUTCMonth() + 1)}-${pad(wall.getUTCDate())}`;
  if (base === "TIMESTAMP_LTZ") return `${date} ${time}Z`;
  if (base === "TIMESTAMP_TZ") {
    const sign = offset >= 0 ? "+" : "-";
    return `${date} ${time}${sign}${pad(Math.floor(Math.abs(offset) / 60))}:${pad(Math.abs(offset) % 60)}`;
  }
  return `${date} ${time}`;
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stableJson(value[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

/**
 * Binds use the closest JavaScript type: Date for TIMESTAMP_LTZ; other dates
 * and times go as strings the SQL casts.
 */
function bind({ type, value }) {
  switch (type) {
    case "NULL":
      return null;
    case "FIXED":
      return value.includes(".") ? value : Number(value);
    case "REAL":
      return Number(value);
    case "TIMESTAMP_LTZ":
      return new Date(value.replace(" ", "T"));
    default:
      return value;
  }
}
