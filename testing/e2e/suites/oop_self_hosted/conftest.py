"""Self-managed lifecycle for the OoP self-hosted (Profile 2) E2E suite.

Each test boots its own throwaway flight-control with the real user-facing
`config/oop-self-hosted.yaml`, which links the control-plane gears in-process
and spawns `hello` as an OoP worker child (`runtime.type: oop` →
`config/oop-hello.yaml`). The suite verifies the host-side half of the OoP
machinery: config-driven spawn, env injection, edge proxying to the spawned
worker, and host shutdown stopping the child — on the exact config `make
oop-example` demos.

Sibling of `testing/e2e/suites/oop`, whose cluster launches the same worker
binaries manually (the Profile 3-shaped path). Run it via
`make e2e-oop-self-hosted`.
"""
from __future__ import annotations

import os
import signal
import socket
import subprocess
import time
from pathlib import Path

import httpx
import pytest

# testing/e2e/suites/oop_self_hosted/conftest.py -> repo root
ROOT = Path(__file__).resolve().parents[4]
TARGET_DIR = ROOT / "target" / "debug"
LOG_DIR = ROOT / "testing" / "e2e" / "logs"

EDGE_PORT = 8087
DIRECTORY_PORT = 50051
WORKER_PORT = 9091  # spawned hello's oop_http.listen_addr (config/oop-hello.yaml)
BASE_URL = f"http://127.0.0.1:{EDGE_PORT}"

REQUEST_TIMEOUT = 5.0
BUILD_TIMEOUT = int(os.environ.get("OOP_SELF_HOSTED_E2E_BUILD_TIMEOUT", "1800"))
BOOT_TIMEOUT = int(os.environ.get("OOP_SELF_HOSTED_E2E_BOOT_TIMEOUT", "180"))

BINARIES = [
    # (cargo bin name, cargo package, cargo features)
    ("flight-control", "cf-gears-flight-control", ""),
    ("hello-oop", "hello", "oop_module"),
]

HOST_CONFIG = "config/oop-self-hosted.yaml"
HOST_LOG = LOG_DIR / "oop-self-hosted-flight-control.log"


def _port_in_use(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.5)
        return s.connect_ex(("127.0.0.1", port)) == 0


def _ensure_built() -> None:
    force = os.environ.get("OOP_E2E_FORCE_BUILD") == "1"
    for name, package, features in BINARIES:
        path = TARGET_DIR / name
        if force or not path.exists():
            cmd = ["cargo", "build", "-p", package, "--bin", name]
            if features:
                cmd += ["--features", features]
            print(f"[oop-self-hosted-e2e] building {name} ({' '.join(cmd)})")
            subprocess.run(cmd, cwd=str(ROOT), check=True, timeout=BUILD_TIMEOUT)
        if not path.exists():
            pytest.fail(f"binary not produced: {path}")


def _tail(n: int = 60) -> str:
    if not HOST_LOG.exists():
        return "(no log)"
    return "".join(HOST_LOG.read_text(errors="replace").splitlines(keepends=True)[-n:])


@pytest.fixture(scope="session", autouse=True)
def _require_oop_optin():
    """Skip unless the e2e-oop umbrella opted in (`make e2e-oop`)."""
    if os.environ.get("OOP_E2E") != "1":
        pytest.skip(
            "OOP_E2E not set — run via: make e2e-oop-self-hosted",
            allow_module_level=True,
        )


@pytest.fixture
def self_hosted_host(tmp_path_factory):
    """Boot a throwaway flight-control with the spawn config, wait for the
    spawned worker's route at its edge, yield the host Popen. Tears the host
    (and therefore its spawned child) down after the test."""
    for port in (EDGE_PORT, DIRECTORY_PORT, WORKER_PORT):
        if _port_in_use(port):
            pytest.fail(f"required port {port} is already in use — another server is running")

    _ensure_built()
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_fh = open(HOST_LOG, "w")
    env = {
        **os.environ,
        "HOME": str(tmp_path_factory.mktemp("oop-self-hosted-home")),
        "RUST_LOG": os.environ.get("RUST_LOG", "info"),
    }
    popen = subprocess.Popen(
        [str(TARGET_DIR / "flight-control"), "--config", HOST_CONFIG, "run"],
        cwd=str(ROOT),
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )

    try:
        deadline = time.monotonic() + BOOT_TIMEOUT
        while time.monotonic() < deadline:
            if popen.poll() is not None:
                pytest.fail(
                    f"spawn host exited (code {popen.returncode}) during boot.\n"
                    f"--- log ---\n{_tail()}"
                )
            try:
                if httpx.get(f"{BASE_URL}/hello/v1/ping", timeout=3).status_code == 200:
                    break
            except httpx.HTTPError:
                pass
            time.sleep(1)
        else:
            pytest.fail(f"timed out after {BOOT_TIMEOUT}s waiting for /hello/v1/ping.\n--- log ---\n{_tail()}")

        yield popen
    finally:
        if popen.poll() is None:
            popen.terminate()
            try:
                popen.wait(timeout=10)
            except subprocess.TimeoutExpired:
                popen.kill()
                popen.wait(timeout=3)
        try:
            os.killpg(popen.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        log_fh.close()
