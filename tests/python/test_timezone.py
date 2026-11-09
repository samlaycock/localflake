"""The TIMEZONE parameter at each level: account (LOCALFLAKE_TIMEZONE),
session (login or ALTER SESSION) and query (the request's parameters)."""

import pytest
from snowflake.connector.errors import ProgrammingError

from conftest import connector, start_server, stop_server

# (A) 2-argument CONVERT_TIMEZONE of an NTZ; (B) a TIMESTAMP_TZ column against
# offset-less text; (C) the same through DATE_TRUNC.
QUERIES = [
    """select to_varchar(convert_timezone('UTC', ntz), 'YYYY-MM-DD"T"HH24:MI:SS.FF3') from t""",
    "select count(*) from t where tz >= '2026-06-23T17:48:00.000'",
    "select count(*) from t where date_trunc('millisecond', tz) = '2026-06-23T17:48:31.518'",
]
LOS_ANGELES = ["2026-06-23T17:48:31.518", 0, 0]
UTC = ["2026-06-23T10:48:31.518", 1, 1]


@pytest.fixture(scope="module")
def utc_server():
    proc, port = start_server({"LOCALFLAKE_TIMEZONE": "UTC"})
    yield port
    stop_server(proc)


@pytest.fixture
def connect_utc(utc_server):
    connections = []
    yield connector(utc_server, connections)
    for conn in connections:
        conn.close()


def setup(conn):
    cur = conn.cursor()
    cur.execute("create or replace table t (tz timestamp_tz, ntz timestamp_ntz)")
    cur.execute(
        "insert into t select '2026-06-23T10:48:31.518-07:00'::timestamp_tz, '2026-06-23T10:48:31.518'::timestamp_ntz"
    )
    return cur


def results(cur, timezone=None):
    params = {"TIMEZONE": timezone} if timezone else None
    return [cur.execute(sql, _statement_params=params).fetchone()[0] for sql in QUERIES]


def timezone_parameter(cur, scope="in session"):
    row = cur.execute(f"show parameters like 'TIMEZONE' {scope}").fetchone()
    return row[1], row[3]


def test_default_is_los_angeles(connect):
    cur = setup(connect(database="TZ_DEFAULT"))
    assert results(cur) == LOS_ANGELES
    assert timezone_parameter(cur) == ("America/Los_Angeles", "")
    assert cur.execute("select right(to_varchar(current_timestamp()), 5)").fetchone()[0] in ("-0700", "-0800")


def test_session_level(connect):
    cur = setup(connect(database="TZ_SESSION"))
    cur.execute("alter session set timezone = 'UTC'")
    assert results(cur) == UTC
    assert timezone_parameter(cur) == ("UTC", "SESSION")
    assert cur.execute("select right(to_varchar(current_timestamp()), 5)").fetchone()[0] == "+0000"
    cur.execute("alter session unset timezone")
    assert results(cur) == LOS_ANGELES
    assert timezone_parameter(cur) == ("America/Los_Angeles", "")


def test_login_parameter(connect):
    cur = setup(connect(database="TZ_LOGIN", session_parameters={"TIMEZONE": "UTC"}))
    assert results(cur) == UTC
    assert timezone_parameter(cur) == ("UTC", "SESSION")


def test_query_level_applies_to_that_query_only(connect):
    cur = setup(connect(database="TZ_QUERY"))
    assert results(cur, "UTC") == UTC
    assert results(cur) == LOS_ANGELES
    tz = cur.execute(
        "select right(to_varchar(current_timestamp()), 5)", _statement_params={"TIMEZONE": "Asia/Tokyo"}
    ).fetchone()[0]
    assert tz == "+0900"
    cur.execute("alter session set timezone = 'UTC'")
    assert results(cur, "America/Los_Angeles") == LOS_ANGELES
    assert results(cur) == UTC
    assert timezone_parameter(cur) == ("UTC", "SESSION")


def test_account_level(connect_utc):
    cur = setup(connect_utc(database="TZ_ACCOUNT"))
    assert results(cur) == UTC
    assert timezone_parameter(cur) == ("UTC", "ACCOUNT")
    assert timezone_parameter(cur, "in account") == ("UTC", "ACCOUNT")
    assert results(cur, "America/Los_Angeles") == LOS_ANGELES
    assert results(cur) == UTC
    cur.execute("alter session set timezone = 'America/Los_Angeles'")
    assert results(cur) == LOS_ANGELES
    assert timezone_parameter(cur) == ("America/Los_Angeles", "SESSION")
    assert timezone_parameter(cur, "in account") == ("UTC", "ACCOUNT")
    cur.execute("alter session unset timezone")
    assert results(cur) == UTC


def test_explicit_offsets_are_unchanged(connect):
    cur = setup(connect(database="TZ_EXPLICIT"))
    sql = "select to_varchar(tz), to_varchar('2024-01-15 12:00:00 +02:00'::timestamp_tz) from t"
    expected = ("2026-06-23 10:48:31.518 -0700", "2024-01-15 12:00:00.000 +0200")
    assert cur.execute(sql).fetchone() == expected
    assert cur.execute(sql, _statement_params={"TIMEZONE": "UTC"}).fetchone() == expected
    cur.execute("alter session set timezone = 'Asia/Tokyo'")
    assert cur.execute(sql).fetchone() == expected


def test_invalid_timezones(connect):
    cur = connect().cursor()
    cur.execute("alter session set timezone = 'UTC'")
    with pytest.raises(ProgrammingError) as err:
        cur.execute("alter session set timezone = 'Not/AZone'")
    assert err.value.errno == 2043
    assert timezone_parameter(cur) == ("UTC", "SESSION")
    with pytest.raises(ProgrammingError) as err:
        cur.execute("select 1", _statement_params={"TIMEZONE": "Not/AZone"})
    assert err.value.errno == 2043


def test_invalid_account_timezone_fails_startup():
    with pytest.raises(RuntimeError):
        proc, _ = start_server({"LOCALFLAKE_TIMEZONE": "Not/AZone"})
        stop_server(proc)
