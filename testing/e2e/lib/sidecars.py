"""Container sidecars for the E2E orchestrator.

Each class here implements ``lib.orchestrator.SidecarProtocol`` (``name``,
``port``, ``start()``, ``stop()``) so it can be dropped into
``GearTestEnv.sidecars``. The orchestrator starts sidecars before it renders
the config, so ``config_patch`` can read the mapped port.

Deliberately hand-rolled rather than using testcontainers-python: this suite's
requirements.txt is minimal and Snyk-pinned, and testcontainers-rs 0.27 (used
by the Rust plugin tests) ships no ryuk either, so the library would buy no
cleanup guarantee the repo already relies on. Orphans are handled by the label
reaper below.
"""

from __future__ import annotations

import atexit
import shutil
import subprocess
import time

import pytest

DOCKER_TIMEOUT = 120
# Image pulls get their own, far more generous budget. See TimescaleDbSidecar.pull.
PULL_TIMEOUT = 600
READY_TIMEOUT = 60
READY_POLL_INTERVAL = 0.5
# Consecutive successful query probes required before a container is called
# ready. See TimescaleDbSidecar._wait_ready for why one success is not enough.
REQUIRED_CONSECUTIVE_OK = 3


class DockerUnavailable(RuntimeError):
    """Docker CLI missing, or the daemon is not reachable."""


def _run(args: list[str], *, timeout: int = DOCKER_TIMEOUT) -> str:
    proc = subprocess.run(
        args, capture_output=True, text=True, timeout=timeout, check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed: {' '.join(args)}\n"
            f"exit={proc.returncode}\nstdout={proc.stdout}\nstderr={proc.stderr}"
        )
    return proc.stdout.strip()


def require_docker() -> None:
    """Raise DockerUnavailable unless a reachable Docker daemon is present."""
    if shutil.which("docker") is None:
        raise DockerUnavailable("the `docker` CLI is not on PATH")
    proc = subprocess.run(
        ["docker", "info"], capture_output=True, text=True, timeout=30, check=False,
    )
    if proc.returncode != 0:
        raise DockerUnavailable(f"docker daemon unreachable: {proc.stderr.strip()}")


def skip_without_docker() -> None:
    """pytest.skip (module-level) unless Docker is usable."""
    try:
        require_docker()
    except DockerUnavailable as exc:
        pytest.skip(f"Docker required for this suite: {exc}", allow_module_level=True)


class TimescaleDbSidecar:
    """A throwaway TimescaleDB container with a dynamically mapped host port.

    Keep IMAGE in sync with TIMESCALEDB_IMAGE / TIMESCALEDB_TAG in
    libs/test-containers/src/lib.rs, which is where the Rust side pins every
    database image. A skew means the plugin's migrations are validated against
    a different PostgreSQL major than E2E runs; the Rust test
    `e2e_sidecar_pins_the_same_timescaledb_image` fails the build if this
    literal drifts from the constants.
    """

    IMAGE = "timescale/timescaledb:2.29.2-pg18"
    LABEL = "cf-gears-e2e=usage-collector"

    DB_USER = "uc"
    DB_PASSWORD = "uc"
    DB_NAME = "uc"

    name = "timescaledb"

    def __init__(self, label: str = LABEL) -> None:
        # Per-instance label (defaulting to the class constant) so a caller
        # that needs an isolated Docker namespace — e.g. the sidecar
        # self-tests in test_sidecar_contract.py, which plant and reap
        # orphans of their own — cannot collide with (and reap!) the label
        # a real session's container is running under.
        self.label = label
        self.port: int | None = None
        self.container_id: str | None = None

    @property
    def dsn_port(self) -> str:
        """Mapped host port as a string, for config substitution."""
        if self.port is None:
            raise RuntimeError("TimescaleDbSidecar.start() has not run")
        return str(self.port)

    @classmethod
    def reap_orphans(cls, label: str = LABEL) -> None:
        """Remove containers left by an earlier interrupted run.

        atexit does not fire on SIGKILL or a hard crash, so a leaked container
        is possible; this clears it on the next run. Same exposure as the Rust
        plugin tests, whose testcontainers version cleans up via Drop.

        `docker ps -aq` matches running containers too, so this removes ANY
        container carrying `label` — including one that is still in active
        use. Callers MUST pass the label of the namespace they own; never
        reap a label whose container might be a live session under test.
        """
        ids = _run(["docker", "ps", "-aq", "--filter", f"label={label}"])
        for container_id in ids.splitlines():
            container_id = container_id.strip()
            if container_id:
                subprocess.run(
                    ["docker", "rm", "-f", container_id],
                    capture_output=True, timeout=DOCKER_TIMEOUT, check=False,
                )

    @classmethod
    def pull(cls) -> None:
        """Fetch IMAGE with a timeout sized for a cold cache.

        Every `docker run` of IMAGE MUST be preceded by this, because a `run`
        that pulls implicitly inherits its caller's much shorter timeout.
        `_run`'s DOCKER_TIMEOUT (120s) is fine for starting an already-local
        image but too tight if it also has to pull first, and a
        `subprocess.TimeoutExpired` during `docker run` escapes as a bare
        SubprocessError instead of this module's RuntimeError — before
        `atexit.register(self.stop)` has run, so the container (if the daemon
        managed to create it before the timeout) leaks until the next reap.
        Pulling first keeps `docker run` itself fast, and this is idempotent
        when the image is already present.
        """
        _run(["docker", "pull", cls.IMAGE], timeout=PULL_TIMEOUT)

    def start(self) -> None:
        require_docker()
        self.reap_orphans(self.label)
        self.pull()

        self.container_id = _run([
            "docker", "run", "-d", "-P",
            "--label", self.label,
            "-e", f"POSTGRES_USER={self.DB_USER}",
            "-e", f"POSTGRES_PASSWORD={self.DB_PASSWORD}",
            "-e", f"POSTGRES_DB={self.DB_NAME}",
            self.IMAGE,
        ])
        atexit.register(self.stop)

        # Everything past `docker run` tears the container down on the way out.
        # `atexit` alone would leave it running for the rest of the session:
        # a raise here happens during session-fixture setup, so the
        # orchestrator's own `sc.stop()` — which sits after its `yield` — never
        # runs. BaseException, not Exception, so a Ctrl-C during the readiness
        # wait cleans up too.
        try:
            mapping = _run(["docker", "port", self.container_id, "5432/tcp"])
            # e.g. "0.0.0.0:32768" or two lines (IPv4 + IPv6). Take the first.
            self.port = int(mapping.splitlines()[0].rsplit(":", 1)[1])

            self._wait_ready()
        except BaseException:
            self.stop()
            raise

        print(f"[sidecar] timescaledb ready on 127.0.0.1:{self.port} "
              f"({self.container_id[:12]})")

    def _wait_ready(self) -> None:
        """Block until the container serves real queries over its mapped port.

        `pg_isready` alone is NOT sufficient, and using it alone caused
        intermittent "connection reset by peer" / "expected to read 5 bytes,
        got 0 bytes at EOF" failures in the gear under test. Two reasons:

        * Run over `docker exec`, it talks to the container's Unix socket. The
          official Postgres entrypoint starts a *temporary* local-only server
          to run initdb and any init scripts, then shuts it down and starts the
          real one. During that window `pg_isready` succeeds while nothing is
          listening on TCP yet.
        * Even once TCP is up, the restart boundary can accept a connection and
          immediately drop it.

        So readiness here means: a real query, executed via `docker exec` against
        the container's OWN loopback (`-h 127.0.0.1 -p 5432`, i.e. the container's
        internal TCP listener, not the host-mapped port), succeeding
        `REQUIRED_CONSECUTIVE_OK` times in a row. `-h 127.0.0.1` forces psql onto
        TCP rather than the Unix socket, and the repetition is what makes the
        init->restart boundary observable instead of a coin flip. Host-side
        reachability through the mapped port is a separate concern, covered by
        `test_sidecar_contract.py`'s plain `socket.create_connection` probe.
        """
        deadline = time.monotonic() + READY_TIMEOUT
        consecutive_ok = 0
        last_err = ""

        while time.monotonic() < deadline:
            probe = subprocess.run(
                ["docker", "exec", "-e", f"PGPASSWORD={self.DB_PASSWORD}",
                 self.container_id,
                 "psql", "-h", "127.0.0.1", "-p", "5432",
                 "-U", self.DB_USER, "-d", self.DB_NAME,
                 "-tAc", "select 1"],
                capture_output=True, text=True, timeout=30, check=False,
            )
            if probe.returncode == 0 and probe.stdout.strip() == "1":
                consecutive_ok += 1
                if consecutive_ok >= REQUIRED_CONSECUTIVE_OK:
                    return
            else:
                # Reset, not decrement: the restart boundary must be crossed
                # cleanly, not merely survived on balance.
                consecutive_ok = 0
                last_err = (probe.stderr or probe.stdout).strip()
            time.sleep(READY_POLL_INTERVAL)

        logs = subprocess.run(
            ["docker", "logs", "--tail", "80", self.container_id],
            capture_output=True, text=True, timeout=30, check=False,
        )
        raise RuntimeError(
            f"TimescaleDB not ready within {READY_TIMEOUT}s "
            f"(container {self.container_id}). Last probe error: {last_err}\n"
            f"--- docker logs ---\n{logs.stdout}\n{logs.stderr}"
        )

    def stop(self) -> None:
        if self.container_id is None:
            return
        subprocess.run(
            ["docker", "rm", "-f", self.container_id],
            capture_output=True, timeout=DOCKER_TIMEOUT, check=False,
        )
        self.container_id = None
        self.port = None
