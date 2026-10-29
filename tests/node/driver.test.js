// Drives localflake with the official Node.js driver (snowflake-sdk), which
// implements the protocol itself rather than through Snowflake's Rust core.
// Build first with `cargo build`, or point LOCALFLAKE_BIN at a binary.

import { spawn } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import snowflake from "snowflake-sdk";

const repoRoot = path.resolve(import.meta.dirname, "../..");
let server;
let port;

snowflake.configure({ logLevel: "OFF" });

function freePort() {
  return new Promise((resolve) => {
    const srv = net.createServer().listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

async function waitForPort(port) {
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
}

before(async () => {
  port = await freePort();
  const binary = process.env.LOCALFLAKE_BIN ?? path.join(repoRoot, "target/debug/localflake");
  server = spawn(binary, ["--addr", `127.0.0.1:${port}`], { stdio: "inherit" });
  await waitForPort(port);
});

after(() => server.kill());

async function connect(database = "NODE_DB") {
  const conn = snowflake.createConnection({
    account: "localflake",
    username: "test",
    password: "test",
    accessUrl: `http://127.0.0.1:${port}`,
    database,
  });
  await conn.connectAsync();
  return conn;
}

function exec(conn, sqlText, binds) {
  return new Promise((resolve, reject) => {
    conn.execute({
      sqlText,
      binds,
      complete: (err, stmt, rows) => (err ? reject(err) : resolve({ stmt, rows })),
    });
  });
}

test("select with types", async () => {
  const conn = await connect();
  const { rows } = await exec(
    conn,
    "select 1 as i, 1.5::number(10,2) as n, 'x' as s, true as b, '2024-02-29'::date as d, parse_json('{\"a\": 1}') as v",
  );
  assert.equal(rows.length, 1);
  const row = rows[0];
  assert.equal(row.I, 1);
  assert.equal(row.N, 1.5);
  assert.equal(row.S, "x");
  assert.equal(row.B, true);
  assert.equal(new Date(row.D).toISOString().slice(0, 10), "2024-02-29");
  assert.deepEqual(row.V, { a: 1 });
});

test("dml, binds and row counts", async () => {
  const conn = await connect("NODE_DML");
  await exec(conn, "create table t (id int, name varchar)");
  const { stmt } = await exec(conn, "insert into t values (?, ?), (?, ?)", [1, "a", 2, null]);
  assert.equal(stmt.getNumUpdatedRows(), 2);
  const { rows } = await exec(conn, "select id, name from t where id = :1", [1]);
  assert.deepEqual(rows, [{ ID: 1, NAME: "a" }]);
  const update = await exec(conn, "update t set name = 'b'");
  assert.equal(update.stmt.getNumUpdatedRows(), 2);
});

test("array binds", async () => {
  const conn = await connect("NODE_ARRAY");
  await exec(conn, "create table t (id int, name varchar)");
  const rows = Array.from({ length: 20 }, (_, i) => [i, `n${i}`]);
  await exec(conn, "insert into t values (?, ?)", rows);
  const { rows: result } = await exec(conn, "select count(*) as c from t");
  assert.equal(result[0].C, 20);
});

test("errors carry Snowflake codes", async () => {
  const conn = await connect();
  await assert.rejects(exec(conn, "select * from missing_table"), (err) => {
    assert.equal(err.code, "002003");
    assert.equal(err.sqlState, "42S02");
    return true;
  });
});

test("session context", async () => {
  const conn = await connect("NODE_CTX");
  await exec(conn, "create schema staging");
  const { rows } = await exec(conn, "select current_database() as d, current_schema() as s");
  assert.deepEqual(rows, [{ D: "NODE_CTX", S: "STAGING" }]);
});

test("large result streams", async () => {
  const conn = await connect();
  const { rows } = await exec(conn, "select seq4() as i from table(generator(rowcount => 50000))");
  assert.equal(rows.length, 50000);
  assert.equal(rows.at(-1).I, 49999);
});

test("timestamp_tz keeps its offset", async () => {
  const conn = await connect();
  const { rows } = await exec(conn, "select '2024-03-15 10:11:12 +02:00'::timestamp_tz as t, to_varchar('2024-03-15 10:11:12 +02:00'::timestamp_tz) as s");
  assert.equal(new Date(rows[0].T).toISOString(), "2024-03-15T08:11:12.000Z");
  assert.equal(rows[0].S, "2024-03-15 10:11:12.000 +0200");
});

test("put, copy and get through a stage", async () => {
  const conn = await connect("NODE_STAGES");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "localflake-node-"));
  const source = path.join(dir, "people.csv");
  fs.writeFileSync(source, "1,Ada\n2,Grace\n");
  await exec(conn, "create or replace stage fixtures");
  const put = await exec(conn, `put file://${source} @fixtures/in/`);
  assert.equal(put.rows[0].status, "UPLOADED");
  const list = await exec(conn, "list @fixtures");
  assert.deepEqual(list.rows.map((r) => r.name), ["fixtures/in/people.csv.gz"]);
  await exec(conn, "create or replace table people (id int, name varchar)");
  const copy = await exec(conn, "copy into people from @fixtures/in/");
  assert.equal(copy.rows[0].status, "LOADED");
  const { rows } = await exec(conn, "select name from people order by id");
  assert.deepEqual(rows, [{ NAME: "Ada" }, { NAME: "Grace" }]);
  await exec(conn, "copy into @fixtures/out/ from people file_format = (type = csv compression = none) single = true");
  const out = path.join(dir, "out");
  fs.mkdirSync(out);
  const get = await exec(conn, `get @fixtures/out/ file://${out}/`);
  assert.equal(get.rows[0].status, "DOWNLOADED");
  assert.equal(fs.readFileSync(path.join(out, "data"), "utf8"), "1,Ada\n2,Grace\n");
});

// Runs for minutes unless the server stops it.
const endless =
  "select count(*) from table(generator(rowcount => 100000000000)) where uniform(1, 10, random()) > 0";

test("cancel a running statement", async () => {
  const conn = await connect();
  let statement;
  const finished = new Promise((resolve) => {
    statement = conn.execute({ sqlText: endless, complete: (err) => resolve(err) });
  });
  await new Promise((r) => setTimeout(r, 300));
  await new Promise((resolve, reject) => statement.cancel((err) => (err ? reject(err) : resolve())));
  const err = await finished;
  assert.ok(err, "the statement should fail");
  // The session runs one statement at a time, so this only returns quickly if
  // the cancel stopped the endless statement on the server.
  const started = Date.now();
  const { rows } = await exec(conn, "select 1 as one");
  assert.equal(rows[0].ONE, 1);
  assert.ok(Date.now() - started < 5_000);
});

test("statement timeout", async () => {
  const conn = await connect();
  await exec(conn, "alter session set statement_timeout_in_seconds = 1");
  await assert.rejects(exec(conn, endless), (err) => {
    assert.equal(err.code, "000630");
    assert.equal(err.sqlState, "57014");
    return true;
  });
});
