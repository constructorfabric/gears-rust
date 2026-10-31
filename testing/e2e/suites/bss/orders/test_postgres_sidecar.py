"""Real Docker checks for the Orders suite's local PostgreSQL contract."""

import json
import socket
import subprocess

import pytest

from . import postgres_sidecar


@pytest.mark.timeout(720, func_only=True)
def test_postgres_sidecar_tcp_sql_errors_and_cleanup():
    pg = postgres_sidecar.PostgresSidecar()
    container_id = None
    try:
        pg.start()
        container_id = pg.container_id
        with socket.create_connection(("127.0.0.1", int(pg.dsn_port)), timeout=5):
            pass
        inspected = json.loads(subprocess.check_output(["docker", "inspect", container_id]))[0]
        assert inspected["HostConfig"]["PortBindings"]["5432/tcp"][0]["HostIp"] == "127.0.0.1"
        assert pg.psql("SELECT 1") == "1"
        # Multiple provisioning statements must retain psql's per-statement autocommit.
        pg.psql("CREATE ROLE sidecar_owner;\nCREATE DATABASE sidecar_check OWNER sidecar_owner;")
        assert pg.psql("SELECT current_database()", db="sidecar_check") == "sidecar_check"
        with pytest.raises(RuntimeError, match="does_not_exist"):
            pg.psql("SELECT * FROM does_not_exist; SELECT 1")
    finally:
        pg.stop()
    assert pg.port is None
    assert pg.container_id is None
    assert subprocess.run(["docker", "inspect", container_id], capture_output=True).returncode != 0


@pytest.mark.parametrize("override", ["", "  ", "18-alpine"])
def test_postgres_image_pin(monkeypatch, override):
    monkeypatch.setenv("GEARS_TEST_PG_TAG", override)
    assert postgres_sidecar.PostgresSidecar().IMAGE == "postgres:18-alpine"


def test_postgres_image_override(monkeypatch):
    monkeypatch.setenv("GEARS_TEST_PG_TAG", "  18.1-alpine  ")
    assert postgres_sidecar.PostgresSidecar().IMAGE == "postgres:18.1-alpine"
