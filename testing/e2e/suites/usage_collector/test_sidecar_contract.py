"""Contract test for TimescaleDbSidecar — verifies the Docker lifecycle itself.

Not an E2E test: no server, no gear. It exists because every other test in this
directory depends on this class behaving, and debugging a container problem
through a failed server boot is far slower than failing here.

It lives in this package rather than next to lib/sidecars.py so that it
inherits this package's conftest skip gate (Task 4's `_require_dedicated_binary`
autouse fixture): the shared `make e2e-local` run DOES collect this file (it's
part of `testpaths = .`), but every test in it is skipped unless `E2E_BINARY`
is set, same as the rest of this package.

Isolation note: `reap_orphans()` matches on Docker label alone, via
`docker ps -aq`, which includes RUNNING containers — not just dead ones. If
these self-tests used the shared `TimescaleDbSidecar.LABEL`, planting an
orphan under that label (or `start()`'s own reap-before-run) would delete the
TimescaleDB container the real `test_env` session fixture has pooled
connections against. So every sidecar/label reference below uses a distinct
`*-selftest` label instead of the shared one — do not "simplify" this back to
`TimescaleDbSidecar.LABEL`.
"""
import shutil
import socket
import subprocess

import pytest

from lib.sidecars import TimescaleDbSidecar

SELFTEST_LABEL = "cf-gears-e2e=usage-collector-selftest"


def _docker_available() -> bool:
    if shutil.which("docker") is None:
        return False
    return subprocess.run(
        ["docker", "info"], capture_output=True, timeout=30, check=False
    ).returncode == 0


pytestmark = pytest.mark.skipif(
    not _docker_available(), reason="Docker is not available"
)

# pytest.ini imposes a 10s per-test hard kill. Pulling an image and waiting for
# Postgres to accept connections takes far longer, so both tests below opt out
# explicitly. The package conftest (Task 4) leaves an existing timeout marker
# alone, so these values survive.


@pytest.mark.timeout(600)
def test_sidecar_starts_exposes_a_reachable_port_and_cleans_up():
    sidecar = TimescaleDbSidecar(label=SELFTEST_LABEL)
    assert sidecar.name == "timescaledb"
    assert sidecar.port is None, "port must be unknown before start()"

    sidecar.start()
    try:
        assert sidecar.port is not None
        assert sidecar.port != 5432, "must use a mapped port, not the container's own"
        with socket.create_connection(("127.0.0.1", sidecar.port), timeout=5):
            pass
        assert sidecar.dsn_port == str(sidecar.port)
    finally:
        container_id = sidecar.container_id
        sidecar.stop()

    assert container_id is not None
    remaining = subprocess.run(
        ["docker", "ps", "-aq", "--filter", f"id={container_id}"],
        capture_output=True, text=True, timeout=30, check=True,
    ).stdout.strip()
    assert remaining == "", f"container {container_id} survived stop()"


@pytest.mark.timeout(600)
def test_reap_removes_orphans_from_a_previous_run():
    # `docker run` below carries a 120s timeout, which an implicit pull would
    # overrun on a cold cache — and a TimeoutExpired there fires BEFORE the
    # `try`, so the orphan would leak instead of being removed. The sibling
    # test above pulls via start(), but this test must stand alone under
    # `-k test_reap`. See TimescaleDbSidecar.pull.
    TimescaleDbSidecar.pull()

    orphan = subprocess.run(
        ["docker", "run", "-d", "--label", SELFTEST_LABEL,
         "--entrypoint", "sleep", TimescaleDbSidecar.IMAGE, "300"],
        capture_output=True, text=True, timeout=120, check=True,
    ).stdout.strip()
    try:
        TimescaleDbSidecar.reap_orphans(SELFTEST_LABEL)
        remaining = subprocess.run(
            ["docker", "ps", "-aq", "--filter", f"id={orphan}"],
            capture_output=True, text=True, timeout=30, check=True,
        ).stdout.strip()
        assert remaining == "", "reap_orphans() left a labelled container behind"
    finally:
        subprocess.run(["docker", "rm", "-f", orphan],
                       capture_output=True, timeout=60, check=False)
