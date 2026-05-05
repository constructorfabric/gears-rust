"""TimescaleDB Docker sidecar for usage-collector e2e tests.

Implements SidecarProtocol for a TimescaleDB container. SSL is enabled inside
the container so that the Rust TimescaleDbConfig validator (which requires
sslmode=require|verify-ca|verify-full) accepts the connection URL.
"""

from __future__ import annotations

import subprocess
import time


class TimescaleDbSidecar:
    """SidecarProtocol implementation for a TimescaleDB Docker container."""

    name: str = "timescaledb"

    def __init__(self, port: int) -> None:
        self.port = port
        self._container_id: str | None = None

    def start(self) -> None:
        result = subprocess.run(
            [
                "docker", "run", "-d", "--rm",
                "-p", f"{self.port}:5432",
                "-e", "POSTGRES_PASSWORD=password",
                "timescale/timescaledb:latest-pg16",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        self._container_id = result.stdout.strip()
        self._wait_ready()
        self._install_openssl()
        self._enable_ssl()

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + 60.0
        while time.monotonic() < deadline:
            result = subprocess.run(
                [
                    "docker", "exec", self._container_id,
                    "pg_isready", "-U", "postgres",
                ],
                capture_output=True,
            )
            if result.returncode == 0:
                return
            time.sleep(0.5)
        raise TimeoutError("TimescaleDB container did not become ready within 60s")

    def _install_openssl(self) -> None:
        subprocess.run(
            [
                "docker", "exec", self._container_id,
                "apk", "add", "--no-cache", "openssl",
            ],
            capture_output=True,
            check=True,
        )

    def _enable_ssl(self) -> None:
        pg_data = "/var/lib/postgresql/data"
        # Generate a self-signed certificate inside the container.
        subprocess.run(
            [
                "docker", "exec", self._container_id,
                "openssl", "req",
                "-new", "-x509", "-days", "3650",
                "-nodes", "-newkey", "rsa:2048",
                "-subj", "/CN=localhost",
                "-keyout", f"{pg_data}/server.key",
                "-out", f"{pg_data}/server.crt",
            ],
            capture_output=True,
            check=True,
        )
        subprocess.run(
            [
                "docker", "exec", self._container_id,
                "chmod", "600", f"{pg_data}/server.key",
            ],
            capture_output=True,
            check=True,
        )
        subprocess.run(
            [
                "docker", "exec", self._container_id,
                "chown", "postgres:postgres",
                f"{pg_data}/server.crt",
                f"{pg_data}/server.key",
            ],
            capture_output=True,
            check=True,
        )
        # Append SSL settings to postgresql.conf and reload.
        subprocess.run(
            [
                "docker", "exec", self._container_id,
                "bash", "-c",
                f"printf \"\\nssl = on\\nssl_cert_file = 'server.crt'\\nssl_key_file = 'server.key'\\n\""
                f" >> {pg_data}/postgresql.conf",
            ],
            capture_output=True,
            check=True,
        )
        subprocess.run(
            [
                "docker", "exec", "-u", "postgres", self._container_id,
                "pg_ctl", "reload", "-D", pg_data,
            ],
            capture_output=True,
            check=True,
        )
        # Poll until SSL is active rather than sleeping a fixed interval,
        # because pg_ctl reload issues SIGHUP which is processed asynchronously.
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            result = subprocess.run(
                [
                    "docker", "exec", self._container_id,
                    "psql", "-U", "postgres", "-c", "SHOW ssl;",
                ],
                capture_output=True,
                text=True,
            )
            if result.returncode == 0 and "on" in result.stdout:
                return
            time.sleep(0.3)
        raise TimeoutError("PostgreSQL SSL did not become active within 10s")

    def stop(self) -> None:
        if self._container_id:
            subprocess.run(
                ["docker", "stop", self._container_id],
                capture_output=True,
            )
            self._container_id = None
