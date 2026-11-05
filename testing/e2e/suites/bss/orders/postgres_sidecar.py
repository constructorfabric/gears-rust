"""Orders-local PostgreSQL, using the shared Docker lifecycle only."""

from __future__ import annotations

import os
import subprocess
import uuid

from lib.sidecars import DOCKER_TIMEOUT, LABEL_KEY, PULL_TIMEOUT, RUN_ID, _DockerSidecar, _run


class PostgresSidecar(_DockerSidecar):
    """Disposable PostgreSQL with TCP readiness and fail-fast SQL execution."""

    LABEL = f"{LABEL_KEY}=bss-orders-pg"
    CONTAINER_PORT = "5432/tcp"
    name = "orders-postgres"
    DB_USER = "postgres"
    DB_PASSWORD = "orders-sidecar-e2e"
    DB_NAME = "postgres"

    def __init__(self) -> None:
        # Each instance owns its cleanup namespace, even within one pytest process.
        super().__init__(label=f"{self.LABEL}-{uuid.uuid4().hex[:8]}-{RUN_ID}")
        tag = os.environ.get("GEARS_TEST_PG_TAG", "").strip() or "18-alpine"
        self.IMAGE = f"postgres:{tag}"

    def pull(self) -> None:
        """Pull the instance image (the generic helper resolves a class constant)."""
        _run(["docker", "pull", self.IMAGE], timeout=PULL_TIMEOUT)

    def _env_args(self) -> list[str]:
        # The base lifecycle supplies -P; an explicit binding overrides it for 5432.
        return [
            "-p", "127.0.0.1::5432",
            "-e", f"POSTGRES_USER={self.DB_USER}",
            "-e", f"POSTGRES_PASSWORD={self.DB_PASSWORD}",
            "-e", f"POSTGRES_DB={self.DB_NAME}",
        ]

    def psql(self, sql: str, *, db: str = DB_NAME) -> str:
        """Execute as the sidecar admin, preserving per-statement autocommit.

        stdin avoids SQL quoting and a multi-statement ``-c`` transaction (CREATE
        DATABASE must run outside a transaction). ON_ERROR_STOP surfaces any SQL
        failure even when later statements would succeed.
        """
        if self.container_id is None:
            raise RuntimeError("PostgresSidecar.start() has not run")
        result = subprocess.run(
            ["docker", "exec", "-i", "-e", f"PGPASSWORD={self.DB_PASSWORD}",
             "-e", "PGCONNECT_TIMEOUT=5", self.container_id,
             "psql", "-X", "-h", "127.0.0.1", "-p", "5432", "-U", self.DB_USER,
             "-d", db, "-A", "-t", "-v", "ON_ERROR_STOP=1", "-f", "-"],
            input=sql, capture_output=True, text=True, timeout=DOCKER_TIMEOUT, check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"PostgreSQL SQL failed (database={db}, exit={result.returncode})\n"
                f"stdout={result.stdout}\nstderr={result.stderr}"
            )
        return result.stdout.strip()

    def _probe(self) -> tuple[bool, str]:
        # TCP avoids the entrypoint's temporary Unix-socket-only init server.
        try:
            result = self.psql("SELECT 1")
            return result == "1", result
        except (RuntimeError, subprocess.SubprocessError, OSError) as exc:
            return False, str(exc)
