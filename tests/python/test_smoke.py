import datetime
import decimal

import pytest
from snowflake.connector.errors import ProgrammingError


def test_select_one(connect):
    cur = connect().cursor()
    assert cur.execute("select 1").fetchall() == [(1,)]
    assert cur.description[0].name == "1"


def test_session_context(connect):
    conn = connect(database="CTX_DB", schema="PUBLIC")
    assert conn.database == "CTX_DB"
    assert conn.schema == "PUBLIC"


def test_create_insert_select(connect):
    cur = connect(database="CRUD_DB").cursor()
    cur.execute("create table people (id int, name varchar, score decimal(10, 2), joined date, active boolean)")
    cur.execute(
        "insert into people values "
        "(1, 'ada', 9.50, '2024-01-02', true), "
        "(2, 'grace', null, '1969-12-31', false)"
    )
    assert cur.rowcount == 2

    rows = cur.execute("select id, name, score, joined, active from people order by id").fetchall()
    assert [d.name for d in cur.description] == ["ID", "NAME", "SCORE", "JOINED", "ACTIVE"]
    assert rows == [
        (1, "ada", decimal.Decimal("9.50"), datetime.date(2024, 1, 2), True),
        (2, "grace", None, datetime.date(1969, 12, 31), False),
    ]

    cur.execute("update people set score = 1 where id = 2")
    assert cur.rowcount == 1


def test_timestamps(connect):
    cur = connect().cursor()
    row = cur.execute("select '2024-03-04 05:06:07.123456'::timestamp").fetchone()
    assert row[0] == datetime.datetime(2024, 3, 4, 5, 6, 7, 123456)


def test_quoted_identifiers_keep_case(connect):
    cur = connect(database="QUOTED_DB").cursor()
    cur.execute('create table "MixedCase" ("lower" int)')
    cur.execute('insert into "MixedCase" values (1)')
    cur.execute('select "lower" from "MixedCase"')
    assert cur.description[0].name == "lower"


def test_nulls_sort_last_ascending(connect):
    cur = connect(database="NULLS_DB").cursor()
    cur.execute("create table t (v int)")
    cur.execute("insert into t values (2), (null), (1)")
    assert [r[0] for r in cur.execute("select v from t order by v").fetchall()] == [1, 2, None]


def test_use_database(connect):
    conn = connect(database="USE_A")
    cur = conn.cursor()
    cur.execute("create database use_b")
    cur.execute("create table t (a int)")
    cur.execute("use database use_a")
    assert cur.execute("select count(*) from use_b.public.t").fetchone() == (0,)


def test_missing_table_error(connect):
    cur = connect().cursor()
    with pytest.raises(ProgrammingError) as exc:
        cur.execute("select * from does_not_exist")
    assert exc.value.errno == 2003
    assert exc.value.sqlstate == "42S02"


def test_syntax_error(connect):
    with pytest.raises(ProgrammingError) as exc:
        connect().cursor().execute("selec 1")
    assert exc.value.errno == 1003
