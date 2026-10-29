"""Runs the shared driver conformance spec (tests/conformance) through the
Python connector, with both Arrow and JSON results."""

import datetime
import decimal
import json
import math
import time
from pathlib import Path

import pytest
import snowflake.connector
from snowflake.connector.constants import FIELD_ID_TO_NAME
from snowflake.connector.errors import Error

SPEC = json.loads((Path(__file__).resolve().parents[1] / "conformance" / "cases.json").read_text())
CASES = SPEC["cases"]
FORMATS = ["ARROW", "JSON"]


@pytest.mark.parametrize("result_format", FORMATS)
@pytest.mark.parametrize("case", CASES, ids=[c["name"] for c in CASES])
def test_conformance(case, result_format, server, tmp_path):
    skip = case.get("skip", {})
    for key in ("python", f"python/{result_format}"):
        if key in skip:
            pytest.skip(skip[key])
    for name, content in case.get("files", {}).items():
        (tmp_path / name).write_text(content)
    options = case.get("connect", {})
    parameters = {"PYTHON_CONNECTOR_QUERY_RESULT_FORMAT": result_format, **options.get("session_parameters", {})}
    conn = snowflake.connector.connect(
        account="localflake",
        user="conformance",
        password="conformance",
        host="127.0.0.1",
        port=server,
        protocol="http",
        paramstyle="qmark",
        database=options.get("database", "CONFORMANCE"),
        schema=options.get("schema"),
        role=options.get("role"),
        warehouse=options.get("warehouse"),
        session_parameters=parameters,
    )
    try:
        for index, step in enumerate(case["steps"]):
            try:
                run_step(conn, step, tmp_path)
            except AssertionError as err:
                raise AssertionError(f"step {index + 1} ({step['sql']}): {err}") from None
    finally:
        conn.close()


def run_step(conn, step, tmp_path):
    sql = step["sql"].replace("{tmpdir}", str(tmp_path))
    for name in step.get("files_after", {}):
        (tmp_path / name).parent.mkdir(parents=True, exist_ok=True)
    cur = conn.cursor()
    try:
        if step.get("describe"):
            cur.describe(sql)
        elif "bind_rows" in step:
            cur.executemany(sql, [[bind(b) for b in row] for row in step["bind_rows"]])
        elif step.get("async"):
            cur.execute_async(sql, [bind(b) for b in step.get("binds", [])])
            query_id = cur.sfqid
            deadline = time.monotonic() + 30
            while conn.is_still_running(conn.get_query_status(query_id)) and time.monotonic() < deadline:
                time.sleep(0.05)
            cur.get_results_from_sfqid(query_id)
        elif "statements" in step:
            cur.execute(sql, num_statements=step["statements"])
        else:
            cur.execute(sql, [bind(b) for b in step.get("binds", [])] or None)
        if step.get("async"):
            # Errors of async queries surface when the result is fetched.
            rows = cur.fetchall()
        else:
            rows = None
    except Error as err:
        assert "error" in step, f"unexpected error {err.errno}: {err.msg}"
        assert f"{err.errno:06d}" == step["error"]["code"], f"error code {err.errno} ({err.msg})"
        if "sqlstate" in step["error"]:
            assert err.sqlstate == step["error"]["sqlstate"], f"sqlstate {err.sqlstate} ({err.msg})"
        assert err.sfqid, "errors carry a query ID"
        return
    assert "error" not in step, f"expected error {step['error']}"

    if "results" in step:
        for i, expected in enumerate(step["results"]):
            if i > 0:
                assert cur.nextset(), f"statement {i + 1} has no result"
            check_result(cur, expected, None)
        assert not cur.nextset(), "more results than expected"
    elif step.get("describe"):
        check_columns(cur.description, step["columns"])
    else:
        check_result(cur, step, rows)

    for name, content in step.get("files_after", {}).items():
        path = tmp_path / name
        assert path.exists(), f"{name} was not downloaded"
        assert path.read_text() == content, f"{name}: {path.read_text()!r}"


def check_result(cur, expected, rows):
    if "columns" in expected:
        check_columns(cur.description, expected["columns"])
    if "rowcount" in expected:
        assert cur.rowcount == expected["rowcount"], f"rowcount {cur.rowcount}"
    if "rows" in expected or "row_count" in expected:
        rows = cur.fetchall() if rows is None else rows
        types = [column_type(d) for d in cur.description]
        if "row_count" in expected:
            assert len(rows) == expected["row_count"], f"{len(rows)} rows"
        if "rows" in expected:
            actual = [[canonical(v, t) for v, t in zip(row, types)] for row in rows]
            assert_rows(actual, expected["rows"])


def column_type(d):
    name = FIELD_ID_TO_NAME[d.type_code]
    if name == "FIXED":
        return f"FIXED({d.precision},{d.scale})"
    return name


def check_columns(description, expected):
    actual = [[d.name, column_type(d)] for d in description]
    assert len(actual) == len(expected), f"columns {actual}"
    for (name, typ), (want_name, want_type) in zip(actual, expected):
        assert name == want_name, f"column name {name!r}, expected {want_name!r}"
        assert type_matches(typ, want_type), f"column {name} type {typ}, expected {want_type}"


def type_matches(actual, expected):
    if expected.startswith("FIXED(*,") and actual.startswith("FIXED("):
        return actual.split(",")[1] == expected.split(",")[1]
    return actual == expected


def assert_rows(actual, expected):
    assert len(actual) == len(expected), f"rows {actual}"
    for row, want in zip(actual, expected):
        assert len(row) == len(want) and all(values_equal(a, b) for a, b in zip(row, want)), f"row {row}, expected {want}"


def values_equal(actual, expected):
    if isinstance(expected, float) or (isinstance(expected, int) and not isinstance(expected, bool)):
        return isinstance(actual, (int, float)) and not isinstance(actual, bool) and float(actual) == float(expected)
    return actual == expected


def fraction(seconds, microseconds):
    text = f"{seconds:02d}"
    if microseconds:
        text += f".{microseconds:06d}".rstrip("0")
    return text


def canonical(value, typ):
    if value is None:
        return None
    base = typ.split("(")[0]
    if base == "FIXED":
        scale = int(typ.rstrip(")").split(",")[1])
        return format(decimal.Decimal(str(value)), f".{scale}f")
    if base == "REAL":
        if math.isnan(value):
            return "NaN"
        if math.isinf(value):
            return "Infinity" if value > 0 else "-Infinity"
        return value
    if base == "DATE":
        return value.isoformat()
    if base == "TIME":
        return f"{value.hour:02d}:{value.minute:02d}:{fraction(value.second, value.microsecond)}"
    if base in ("TIMESTAMP_NTZ", "TIMESTAMP_LTZ", "TIMESTAMP_TZ"):
        if base == "TIMESTAMP_LTZ":
            value = value.astimezone(datetime.timezone.utc)
        text = value.strftime("%Y-%m-%d %H:%M:") + fraction(value.second, value.microsecond)
        if base == "TIMESTAMP_LTZ":
            return text + "Z"
        if base == "TIMESTAMP_TZ":
            offset = value.utcoffset()
            minutes = int(offset.total_seconds() // 60)
            sign = "+" if minutes >= 0 else "-"
            return text + f"{sign}{abs(minutes) // 60:02d}:{abs(minutes) % 60:02d}"
        return text
    if base == "BINARY":
        return bytes(value).hex()
    if base in ("VARIANT", "OBJECT", "ARRAY", "VECTOR"):
        parsed = json.loads(value) if isinstance(value, str) else value
        return json.dumps(parsed, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return value


def bind(spec):
    typ, value = spec["type"], spec.get("value")
    if typ == "NULL":
        return None
    if typ == "FIXED":
        return int(value) if "." not in value else decimal.Decimal(value)
    if typ == "REAL":
        return float(value)
    if typ == "DATE":
        return datetime.date.fromisoformat(value)
    if typ == "TIME":
        return datetime.time.fromisoformat(value)
    if typ == "TIMESTAMP_NTZ":
        return datetime.datetime.fromisoformat(value)
    if typ == "TIMESTAMP_LTZ":
        return ("TIMESTAMP_LTZ", datetime.datetime.fromisoformat(value.replace("Z", "+00:00")))
    if typ == "TIMESTAMP_TZ":
        return ("TIMESTAMP_TZ", datetime.datetime.fromisoformat(value))
    if typ == "BINARY":
        return bytes.fromhex(value)
    return value
