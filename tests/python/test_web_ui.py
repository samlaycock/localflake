"""The web UI's page and JSON API (`/__localflake/ui/`, `/__localflake/api/`).

These use the standard library's HTTP client, plus the Python connector where a
driver's statements and sessions should show up in the UI.
"""

import json
import urllib.error
import urllib.request

import pytest
from snowflake.connector.errors import DatabaseError


@pytest.fixture
def ui(server):
    base = f"http://127.0.0.1:{server}"

    def call(path, body=None, method=None, headers=None, raw=None):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        request = urllib.request.Request(
            base + path,
            data=data,
            method=method or ("POST" if data is not None else "GET"),
            headers={"Content-Type": "application/octet-stream" if raw is not None else "application/json", **(headers or {})},
        )
        try:
            with urllib.request.urlopen(request) as response:
                payload = response.read()
                return response.status, response.headers, payload
        except urllib.error.HTTPError as err:
            return err.code, err.headers, err.read()

    def api(path, body=None, method=None, **kwargs):
        status, _, payload = call("/__localflake/api" + path, body, method, **kwargs)
        return status, json.loads(payload) if payload else None

    def session(database=None):
        status, data = api("/sessions", {"database": database})
        assert status == 200, data
        return data["session"]

    def query(token, sql, **kwargs):
        status, data = api("/query", {"session": token, "sql": sql, **kwargs})
        assert status == 200, data
        return data

    call.api = api
    call.session = session
    call.query = query
    return call


def test_serves_the_page(ui):
    status, headers, _ = ui("/")
    assert status == 200  # urllib follows the redirect to the page
    status, headers, body = ui("/__localflake/ui/")
    assert status == 200
    assert headers["Content-Type"].startswith("text/html")
    assert b"app.js" in body
    status, headers, _ = ui("/__localflake/ui/app.js")
    assert status == 200 and headers["Content-Type"].startswith("text/javascript")
    assert ui("/__localflake/ui/nope.js")[0] == 404


def test_refuses_cross_origin_requests(ui):
    status, data = ui.api("/sessions", {}, headers={"Origin": "http://evil.example"})
    assert status == 403
    status, data = ui.api("/history", headers={"Sec-Fetch-Site": "cross-site"})
    assert status == 403


def test_runs_statements_and_reports_the_failing_one(ui):
    token = ui.session("UI_DB")
    data = ui.query(
        token,
        "create table t (id int, name varchar(10), ts timestamp_ltz); "
        "insert into t values (1, 'a', '2024-01-02 03:04:05'), (2, null, null); "
        "select * from t order by id; "
        "select 1 / 0; "
        "select 'never runs'",
    )
    kinds = [r["kind"] for r in data["results"]]
    assert kinds == ["ddl", "insert", "query"]
    rows = data["results"][2]
    assert [c["name"] for c in rows["columns"]] == ["ID", "NAME", "TS"]
    assert [c["type"] for c in rows["columns"]] == ["fixed", "text", "timestamp_ltz"]
    assert rows["rows"][1][1] is None
    assert rows["total"] == 2
    assert rows["timezone"] == "America/Los_Angeles"
    assert rows["durationMicros"] >= 0
    assert data["error"]["code"] == "100051"
    assert data["error"]["queryId"]
    assert data["context"]["database"] == "UI_DB"


def test_pages_through_results(ui):
    token = ui.session()
    data = ui.query(token, "select seq4() as n from table(generator(rowcount => 2500)) order by n", rowLimit=100)
    result = data["results"][0]
    assert result["total"] == 2500
    assert len(result["rows"]) == 100
    status, page = ui.api(f"/results/{result['queryId']}?offset=2490&limit=50")
    assert status == 200
    assert [r[0] for r in page["rows"]] == [str(n) for n in range(2490, 2500)]
    assert ui.api("/results/no-such-query")[0] == 404


def test_splits_statements_in_utf16_offsets(ui):
    sql = "select 'é😀'; begin let x := 1; return x; end; select 2"
    status, data = ui.api("/split", {"sql": sql})
    assert status == 200
    # JavaScript strings index UTF-16 code units: the emoji takes two.
    units = sql.encode("utf-16-le")
    texts = [units[a * 2 : b * 2].decode("utf-16-le") for a, b in data["ranges"]]
    assert texts == ["select 'é😀'", "begin let x := 1; return x; end", "select 2"]
    assert ui.api("/split", {"sql": "select 'unclosed"})[1]["error"]


def test_history_records_driver_statements(ui, connect):
    status, before = ui.api("/history")
    cur = connect(database="HISTORY_DB", session_parameters={"QUERY_TAG": "from-test"}).cursor()
    cur.execute("create table h (a int)")
    cur.execute("insert into h values (1), (2)")
    with pytest.raises(DatabaseError):
        cur.execute("select nope from h")

    status, page = ui.api(f"/history?since={before['sequence']}")
    records = [r for r in page["records"] if r["database"] == "HISTORY_DB"]
    assert [(r["sql"], r["status"]) for r in records] == [
        ("create table h (a int)", "succeeded"),
        ("insert into h values (1), (2)", "succeeded"),
        ("select nope from h", "failed"),
    ]
    insert = records[1]
    assert insert["rows"] == 2
    assert insert["client"] == "PythonConnector"
    assert insert["queryTag"] == "from-test"
    assert records[2]["error"]["code"] == "000904"

    status, detail = ui.api(f"/history/{insert['queryId']}")
    assert any("INSERT INTO" in sql for sql in detail["duckdbSql"])
    assert detail["resultAvailable"] is True

    # Polling from the latest sequence number returns nothing new.
    assert ui.api(f"/history?since={page['sequence']}")[1]["records"] == []


def test_catalog_describes_databases(ui):
    token = ui.session("CATALOG_DB")
    ui.query(
        token,
        "create table people (id int not null, name varchar(20) comment 'full name', v variant); "
        "create view adults as select * from people; "
        "create stage files; create sequence ids; "
        "create procedure p(x number) returns number language sql as 'begin return x; end'",
    )
    status, data = ui.api("/catalog")
    assert "CATALOG_DB" in [d["name"] for d in data["databases"]]
    status, data = ui.api("/catalog/CATALOG_DB")
    public = next(s for s in data["schemas"] if s["name"] == "PUBLIC")
    objects = {o["name"]: o for o in public["objects"]}
    assert objects["PEOPLE"]["kind"] == "TABLE"
    assert objects["ADULTS"]["kind"] == "VIEW"
    columns = objects["PEOPLE"]["columns"]
    assert [(c["name"], c["type"], c["nullable"]) for c in columns] == [
        ("ID", "NUMBER(38,0)", False),
        ("NAME", "VARCHAR(20)", True),
        ("V", "VARIANT", True),
    ]
    assert columns[1]["comment"] == "full name"
    assert [s["name"] for s in public["stages"]] == ["FILES"]
    assert [s["name"] for s in public["sequences"]] == ["IDS"]
    assert [p["name"] for p in public["procedures"]] == ["P"]

    # The explorer's own queries stay out of the history.
    status, page = ui.api("/history")
    assert not any(r["client"].endswith("(explorer)") for r in page["records"])


def test_uploads_downloads_and_loads_files(ui):
    token = ui.session("UPLOAD_DB")
    ui.query(token, "create table people (name varchar, active boolean, age int)")
    csv = b"age,name,active\n36,Ada,true\n85,Grace,false\n"
    status, data = ui.api(
        f"/stage-files?session={token}&stage=@~/uploads&name=people.csv", method="PUT", raw=csv
    )
    assert status == 200 and data["size"] == len(csv)
    data = ui.query(
        token,
        "copy into people from @~/uploads files = ('people.csv') "
        "file_format = (type = csv parse_header = true) match_by_column_name = case_insensitive",
    )
    assert data["error"] is None, data["error"]
    rows = ui.query(token, "select * from people order by age")["results"][0]["rows"]
    assert rows == [["Ada", "1", "36"], ["Grace", "0", "85"]]

    status, headers, body = ui(f"/__localflake/api/stage-files?session={token}&stage=@~&name=uploads/people.csv")
    assert status == 200 and body == csv
    assert 'filename="people.csv"' in headers["Content-Disposition"]
    assert ui(f"/__localflake/api/stage-files?session={token}&stage=@~&name=uploads/people")[0] == 404

    status, data = ui.api(f"/stage-files?session={token}&stage=@~&name=../x", method="PUT", raw=b"x")
    assert status == 400


def test_ending_a_session_from_the_ui(ui, connect):
    conn = connect(database="END_DB")
    conn.cursor().execute("select 1")
    status, data = ui.api("/sessions")
    driver = next(s for s in data["sessions"] if s["database"] == "END_DB" and not s["webUi"])
    assert driver["client"] == "PythonConnector"
    assert driver["requests"] >= 1

    status, _ = ui.api(f"/sessions/{driver['id']}", method="DELETE")
    assert status == 200
    with pytest.raises(DatabaseError) as err:
        conn.cursor().execute("select 1")
    assert err.value.errno == 390111

    # A worksheet whose session ended is told to start a new one.
    token = ui.session()
    status, data = ui.api("/sessions")
    worksheet = max((s for s in data["sessions"] if s["webUi"]), key=lambda s: s["id"])
    ui.api(f"/sessions/{worksheet['id']}", method="DELETE")
    status, data = ui.api("/query", {"session": token, "sql": "select 1"})
    assert status == 401 and data["error"]["code"] == "390111"
