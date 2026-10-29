"""Protocol features and Snowflake SQL semantics through the real connector."""

import datetime
import decimal
import json
import time

import pytest
from snowflake.connector import DictCursor
from snowflake.connector.errors import ProgrammingError


def test_arrow_types_round_trip(connect):
    cur = connect(database="TYPES_DB").cursor()
    cur.execute(
        "create table t (i int, n number(10, 2), f float, s varchar, b boolean, d date, "
        "ts timestamp_ntz, bin binary, v variant)"
    )
    cur.execute(
        "insert into t select 42, 12.34, 1.5, 'héllo', true, '2024-02-29', "
        "'2024-01-02 03:04:05.678901', to_binary('CAFE', 'HEX'), parse_json('{\"a\": [1, 2]}')"
    )
    cur.execute("insert into t (i) values (null)")
    rows = cur.execute("select * from t order by i nulls last").fetchall()
    assert rows[0][:8] == (
        42,
        decimal.Decimal("12.34"),
        1.5,
        "héllo",
        True,
        datetime.date(2024, 2, 29),
        datetime.datetime(2024, 1, 2, 3, 4, 5, 678901),
        b"\xca\xfe",
    )
    assert json.loads(rows[0][8]) == {"a": [1, 2]}
    assert rows[1] == (None,) * 9
    assert [d.type_code for d in cur.description][:2] == [0, 0]  # both NUMBER


def test_qmark_and_numeric_binds(connect):
    conn = connect(database="BIND_DB", paramstyle="qmark")
    cur = conn.cursor()
    cur.execute("create table t (id int, name varchar, d date, ts timestamp_ntz, amount number(10, 2))")
    cur.execute(
        "insert into t values (?, ?, ?, ?, ?)",
        (1, "ada", datetime.date(2024, 1, 2), datetime.datetime(2024, 1, 2, 3, 4, 5), decimal.Decimal("1.25")),
    )
    assert cur.rowcount == 1
    cur.execute("insert into t (id, name) values (?, ?)", (2, None))
    assert cur.execute("select name, d, ts, amount from t where id = ?", (1,)).fetchall() == [
        ("ada", datetime.date(2024, 1, 2), datetime.datetime(2024, 1, 2, 3, 4, 5), decimal.Decimal("1.25"))
    ]
    assert cur.execute("select count(*) from t where name is null").fetchone() == (1,)


def test_pyformat_binds(connect):
    cur = connect().cursor()
    assert cur.execute("select %s || 'b'", ("a",)).fetchone() == ("ab",)
    assert cur.execute("select %(x)s", {"x": "it's"}).fetchone() == ("it's",)


def test_executemany_array_binds(connect):
    conn = connect(database="MANY_DB", paramstyle="qmark")
    cur = conn.cursor()
    cur.execute("create table t (id int, name varchar)")
    cur.executemany("insert into t values (?, ?)", [(i, f"n{i}") for i in range(50)])
    assert cur.rowcount == 50
    assert cur.execute("select count(*), max(name) from t").fetchone() == (50, "n9")


def test_multiple_statements(connect):
    cur = connect(database="MULTI_DB").cursor()
    cur.execute(
        "create table t (a int); insert into t values (1), (2); select sum(a) from t",
        num_statements=3,
    )
    assert cur.fetchall() == [("Table T successfully created.",)]
    assert cur.nextset()
    assert cur.fetchall() == [(2,)]
    assert cur.nextset()
    assert cur.fetchall() == [(3,)]
    assert not cur.nextset()


def test_multiple_statements_rejected_by_default(connect):
    cur = connect().cursor()
    with pytest.raises(ProgrammingError) as err:
        cur.execute("select 1; select 2")
    assert err.value.errno == 8


def test_async_query(connect):
    conn = connect(database="ASYNC_DB")
    cur = conn.cursor()
    cur.execute_async("select count(*) from table(generator(rowcount => 1000))")
    query_id = cur.sfqid
    deadline = time.monotonic() + 10
    while conn.is_still_running(conn.get_query_status(query_id)):
        assert time.monotonic() < deadline
        time.sleep(0.05)
    cur.get_results_from_sfqid(query_id)
    assert cur.fetchall() == [(1000,)]


def test_large_result(connect):
    cur = connect().cursor()
    cur.execute("select seq4() as i, uuid_string() as u from table(generator(rowcount => 200000))")
    count = 0
    total = 0
    for row in cur:
        count += 1
        total += row[0]
    assert count == 200000
    assert total == sum(range(200000))


def test_result_scan_last_query(connect):
    cur = connect().cursor()
    cur.execute("select 7 as x union all select 8")
    assert cur.execute("select sum(x) from table(result_scan(last_query_id()))").fetchone() == (15,)


def test_show_and_describe(connect):
    cur = connect(database="META_DB").cursor(DictCursor)
    cur.execute("create table people (id int, name varchar(20) comment 'full name')")
    tables = cur.execute("show tables").fetchall()
    assert [(t["name"], t["database_name"], t["schema_name"]) for t in tables] == [("PEOPLE", "META_DB", "PUBLIC")]
    columns = cur.execute("describe table people").fetchall()
    assert [(c["name"], c["type"], c["comment"]) for c in columns] == [
        ("ID", "NUMBER(38,0)", None),
        ("NAME", "VARCHAR(20)", "full name"),
    ]
    assert ("META_DB",) in [(d["name"],) for d in cur.execute("show databases").fetchall()]
    info = cur.execute(
        "select column_name, data_type from information_schema.columns where table_name = 'PEOPLE' order by ordinal_position"
    ).fetchall()
    assert [(r["COLUMN_NAME"], r["DATA_TYPE"]) for r in info] == [("ID", "NUMBER"), ("NAME", "TEXT")]


def test_semi_structured(connect):
    cur = connect(database="VARIANT_DB").cursor()
    cur.execute("create table events (payload variant)")
    cur.execute(
        """insert into events select parse_json(column1) from values
        ('{"user": "ada", "tags": ["x", "y"]}'),
        ('{"user": "grace", "tags": []}')"""
    )
    rows = cur.execute(
        "select payload:user::string, f.value::string from events, "
        "lateral flatten(input => payload:tags, outer => true) f order by 1, 2"
    ).fetchall()
    assert rows == [("ada", "x"), ("ada", "y"), ("grace", None)]


def test_alter_session_and_query_tag(connect):
    cur = connect().cursor()
    cur.execute("alter session set query_tag = 'e2e'")
    assert cur.execute("show parameters like 'QUERY_TAG' in session").fetchone()[1] == "e2e"
    cur.execute("alter session set timezone = 'UTC'")
    assert cur.execute("show parameters like 'TIMEZONE' in session").fetchone() is not None


def test_transactions(connect):
    conn = connect(database="TXN_DB")
    cur = conn.cursor()
    cur.execute("create table t (a int)")
    cur.execute("begin")
    cur.execute("insert into t values (1)")
    cur.execute("rollback")
    assert cur.execute("select count(*) from t").fetchone() == (0,)
    conn.autocommit(False)
    cur.execute("insert into t values (2)")
    conn.commit()
    assert cur.execute("select count(*) from t").fetchone() == (1,)


def test_snowflake_division_and_errors(connect):
    cur = connect().cursor()
    assert cur.execute("select 1 / 3, iff(true, 'y', 'n'), nvl(null, 5)").fetchone() == (
        decimal.Decimal("0.333333"),
        "y",
        5,
    )
    with pytest.raises(ProgrammingError) as err:
        cur.execute("select 1 / 0")
    assert err.value.errno == 100051


def test_timestamp_tz(connect):
    cur = connect().cursor()
    value = cur.execute("select '2024-03-15 10:11:12 +02:00'::timestamp_tz").fetchone()[0]
    assert value.isoformat() == "2024-03-15T10:11:12+02:00"
    cur.execute("alter session set timezone = 'America/New_York'")
    assert cur.execute("select hour('2024-07-01 12:00:00 +00:00'::timestamp_ltz)").fetchone() == (8,)


def test_put_copy_get_roundtrip(connect, tmp_path):
    conn = connect(database="PY_STAGES")
    cur = conn.cursor()
    source = tmp_path / "people.csv"
    source.write_text("1,Ada\n2,Grace\n")
    cur.execute("create or replace stage fixtures")
    put = cur.execute(f"put file://{source} @fixtures/in/").fetchall()
    assert put[0][0] == "people.csv" and put[0][6] == "UPLOADED"
    again = cur.execute(f"put file://{source} @fixtures/in/").fetchall()
    assert again[0][6] == "SKIPPED"
    names = [r[0] for r in cur.execute("list @fixtures").fetchall()]
    assert names == ["fixtures/in/people.csv.gz"]

    cur.execute("create or replace table people (id int, name varchar)")
    loaded = cur.execute("copy into people from @fixtures/in/").fetchall()
    assert loaded[0][1] == "LOADED" and loaded[0][3] == 2
    assert cur.execute("select name from people order by id").fetchall() == [("Ada",), ("Grace",)]

    cur.execute("copy into @fixtures/out/ from people file_format = (type = csv compression = none) single = true")
    out = tmp_path / "out"
    out.mkdir()
    got = cur.execute(f"get @fixtures/out/ file://{out}/").fetchall()
    assert got[0][2] == "DOWNLOADED"
    assert (out / "data").read_text() == "1,Ada\n2,Grace\n"


# Runs for minutes unless the server stops it.
ENDLESS = "select count(*) from table(generator(rowcount => 100000000000)) where uniform(1, 10, random()) > 0"


def test_statement_timeout(connect):
    cur = connect().cursor()
    cur.execute("alter session set statement_timeout_in_seconds = 1")
    started = time.monotonic()
    with pytest.raises(ProgrammingError) as err:
        cur.execute(ENDLESS)
    assert err.value.errno == 630
    assert err.value.sqlstate == "57014"
    assert time.monotonic() - started < 10
    assert cur.execute("select 1").fetchone() == (1,)


def test_abort_query_not_running(connect):
    cur = connect().cursor()
    qid = cur.execute("select 1").sfqid
    assert cur.abort_query(qid) is False
