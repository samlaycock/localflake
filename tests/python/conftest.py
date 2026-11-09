"""Starts a localflake server for the test session.

Build first with `cargo build`, or point LOCALFLAKE_BIN at a binary.
"""

import os
import socket
import subprocess
import time
from pathlib import Path

import pytest
import snowflake.connector

REPO_ROOT = Path(__file__).resolve().parents[2]


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start_server(env=None):
    """Starts a localflake server, with extra environment variables; returns `(process, port)`."""
    binary = os.environ.get("LOCALFLAKE_BIN", REPO_ROOT / "target" / "debug" / "localflake")
    port = _free_port()
    proc = subprocess.Popen([str(binary), "--addr", f"127.0.0.1:{port}"], env={**os.environ, **(env or {})})
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            break
        except OSError:
            if proc.poll() is not None:
                raise RuntimeError(f"localflake exited with status {proc.returncode}") from None
            time.sleep(0.05)
    else:
        proc.kill()
        raise RuntimeError("localflake did not start")
    return proc, port


def stop_server(proc):
    proc.terminate()
    proc.wait(timeout=5)


@pytest.fixture(scope="session")
def server():
    proc, port = start_server()
    yield port
    stop_server(proc)


def connector(port, connections):
    """A `connect(**kwargs)` for the server on `port`, recording connections to close."""

    def _connect(**kwargs):
        params = dict(
            account="localflake",
            user="test",
            password="test",
            host="127.0.0.1",
            port=port,
            protocol="http",
            database="TEST_DB",
        )
        params.update(kwargs)
        conn = snowflake.connector.connect(**params)
        connections.append(conn)
        return conn

    return _connect


@pytest.fixture
def connect(server):
    connections = []
    yield connector(server, connections)
    for conn in connections:
        conn.close()
