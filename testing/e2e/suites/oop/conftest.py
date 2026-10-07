"""Self-managed lifecycle for the out-of-process (loopback) E2E suite.

Boots flight-control (edge + DirectoryService) and three OoP gear processes
on loopback, waits for the edge to sync their routes from the DirectoryService,
and yields the edge base URL. Everything is torn down on session teardown.

Topology (all on 127.0.0.1):
    flight-control (edge)        :8087   + DirectoryService :50051
    hello-oop                    :9091   (anonymous REST)
    api-contracts-oop            :9092   (PaymentApi REST provider)
    api-contracts-consumer-oop   :9093   (resolves PaymentApi from the provider)

This is a Profile 2 (Host + Workers) topology on loopback: flight-control runs
the DirectoryService + built-in edge, and the workers are separate OoP
processes discovered through the directory. It exercises the same OoP software
path (bootstrap, directory-resolved REST clients, edge reverse-proxy) that a
Profile 3 (Kubernetes) deployment shares — only with processes on 127.0.0.1
instead of pods behind an external gateway. Heavy work (cargo build, process
boot, route sync) happens in the session fixture; run pytest with
`-o timeout_func_only=true` so the per-test timeout does not count fixture setup
(the `make e2e-oop` target does this).

The suite is opt-in: it only runs when `OOP_E2E=1` is set (which `make e2e-oop`
does). Any other session that collects it — e.g. `make e2e-docker-smoke`, which
runs `pytest -m smoke` across the whole tree under the 10s `pytest.ini` timeout
— skips it gracefully instead of tripping that timeout inside the fixture build.
"""
from __future__ import annotations

import os
import socket
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

import httpx
import pytest

# testing/e2e/suites/oop/conftest.py -> repo root
ROOT = Path(__file__).resolve().parents[4]
TARGET_DIR = ROOT / "target" / "debug"
# Reuse the git-ignored shared E2E logs dir; prefix files so they are distinct.
LOG_DIR = ROOT / "testing" / "e2e" / "logs"
LOG_PREFIX = "oop-"

EDGE_PORT = 8087
DIRECTORY_PORT = 50051
DIRECTORY_ENDPOINT = f"http://127.0.0.1:{DIRECTORY_PORT}"
BASE_URL = f"http://127.0.0.1:{EDGE_PORT}"
# static-authn accept_all maps any non-empty bearer to the platform-root tenant.
TOKEN = os.environ.get("OOP_E2E_TOKEN", "oop-e2e-token")

# Per-request timeout for the test HTTP calls (distinct from the lifecycle
# build/boot/route-sync timeouts below). Imported by the test modules.
REQUEST_TIMEOUT = 5.0

BUILD_TIMEOUT = int(os.environ.get("OOP_E2E_BUILD_TIMEOUT", "1800"))
FLIGHT_CONTROL_HEALTH_TIMEOUT = int(os.environ.get("OOP_E2E_FLIGHT_CONTROL_TIMEOUT", "120"))
ROUTE_SYNC_TIMEOUT = int(os.environ.get("OOP_E2E_ROUTE_TIMEOUT", "60"))


@dataclass
class Binary:
    """A binary to build and (optionally) launch."""
    name: str            # cargo bin name == target/debug/<name>
    package: str         # cargo package (-p)
    features: str        # comma-separated cargo features ("" = none)


@dataclass
class Proc:
    """A launched process + its log file handle."""
    name: str
    popen: subprocess.Popen
    log_fh: object


# flight-control binary + the three OoP gear binaries.
FLIGHT_CONTROL = Binary(name="flight-control", package="cf-gears-flight-control", features="")
GEARS = [
    Binary(name="hello-oop", package="hello", features="oop_module"),
    Binary(name="api-contracts-oop", package="cf-api-contracts", features="oop_module"),
    Binary(
        name="api-contracts-consumer-oop",
        package="cf-api-contracts-consumer",
        features="oop_module",
    ),
]

# Launch specs: (binary name, config path, is_flight_control).
FLIGHT_CONTROL_CONFIG = "config/oop-flight-control.yaml"
GEAR_LAUNCH = [
    ("hello-oop", "config/oop-hello.yaml"),
    ("api-contracts-oop", "config/oop-api-contracts.yaml"),
    ("api-contracts-consumer-oop", "config/oop-api-contracts-consumer.yaml"),
]

# Each gear's own HTTP listener (oop_http.listen_addr in its config). Reaching a
# gear here bypasses the edge — needed only to probe framework endpoints the edge
# does not surface, such as the per-gear `/readyz` (ADR-0005). Kept in sync with
# the config files.
GEAR_PORTS = {
    "hello-oop": 9091,
    "api-contracts-oop": 9092,
    "api-contracts-consumer-oop": 9093,
}


# ── helpers ────────────────────────────────────────────────────────────────

def _port_in_use(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.5)
        return s.connect_ex(("127.0.0.1", port)) == 0


def _cargo_build(bin_: Binary) -> None:
    cmd = ["cargo", "build", "-p", bin_.package, "--bin", bin_.name]
    if bin_.features:
        cmd += ["--features", bin_.features]
    print(f"[oop-e2e] building {bin_.name} ({' '.join(cmd)})")
    subprocess.run(cmd, cwd=str(ROOT), check=True, timeout=BUILD_TIMEOUT)


def _ensure_built() -> None:
    force = os.environ.get("OOP_E2E_FORCE_BUILD") == "1"
    for b in [FLIGHT_CONTROL, *GEARS]:
        path = TARGET_DIR / b.name
        if force or not path.exists():
            _cargo_build(b)
        if not path.exists():
            pytest.fail(f"binary not produced: {path}")


def _launch(bin_name: str, config: str, *, is_flight_control: bool, child_home: str) -> Proc:
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / f"{LOG_PREFIX}{bin_name}.log"
    log_fh = open(log_path, "w")
    # Isolate on-disk state (SQLite DBs live under `~/.cf-gears-*`) into a
    # throwaway HOME so runs are repeatable and the real home stays clean.
    env = {**os.environ, "HOME": child_home, "RUST_LOG": os.environ.get("RUST_LOG", "info")}
    env["TOOLKIT_DIRECTORY_ENDPOINT"] = DIRECTORY_ENDPOINT
    cmd = [str(TARGET_DIR / bin_name), "--config", config]
    if is_flight_control:
        cmd.append("run")  # flight-control uses a `run` subcommand
    print(f"[oop-e2e] launching {bin_name} -> {log_path}")
    try:
        popen = subprocess.Popen(
            cmd, cwd=str(ROOT), stdout=log_fh, stderr=subprocess.STDOUT, env=env
        )
    except Exception:
        log_fh.close()
        raise
    return Proc(name=bin_name, popen=popen, log_fh=log_fh)


def _tail(bin_name: str, n: int = 60) -> str:
    p = LOG_DIR / f"{LOG_PREFIX}{bin_name}.log"
    if not p.exists():
        return "(no log)"
    return "".join(p.read_text(errors="replace").splitlines(keepends=True)[-n:])


def _poll(desc: str, fn, *, timeout: int, procs: list[Proc]) -> None:
    """Poll fn() until it returns True; fail with log tails on timeout/crash."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for pr in procs:
            if pr.popen.poll() is not None:
                pytest.fail(
                    f"process {pr.name} exited (code {pr.popen.returncode}) "
                    f"while waiting for {desc}.\n--- {pr.name} log ---\n{_tail(pr.name)}"
                )
        try:
            if fn():
                return
        except httpx.HTTPError:
            pass
        time.sleep(1)
    tails = "\n".join(f"--- {pr.name} log ---\n{_tail(pr.name)}" for pr in procs)
    pytest.fail(f"timed out after {timeout}s waiting for {desc}.\n{tails}")


def _status(method: str, path: str, **kw) -> int:
    try:
        r = httpx.request(method, f"{BASE_URL}{path}", timeout=3, **kw)
        return r.status_code
    except httpx.HTTPError:
        return 0


# ── cluster controller ───────────────────────────────────────────────────────

class Cluster:
    """Owns the launched processes and exposes per-gear start/stop.

    `oop_cluster` yields only `base_url` (back-compat for the steady-state
    tests); lifecycle tests take the `cluster` fixture to stop/start an
    individual gear and re-check the edge.
    """

    def __init__(self, child_home: str):
        self.base_url = BASE_URL
        self._child_home = child_home
        self._procs: dict[str, Proc] = {}
        # bin_name -> (config, is_flight_control), so a stopped gear can restart.
        self._specs: dict[str, tuple[str, bool]] = {}

    @property
    def procs(self) -> list[Proc]:
        """Currently-tracked (expected-alive) processes, for crash detection."""
        return list(self._procs.values())

    def launch(self, bin_name: str, config: str, *, is_flight_control: bool = False) -> Proc:
        self._specs[bin_name] = (config, is_flight_control)
        pr = _launch(
            bin_name, config, is_flight_control=is_flight_control, child_home=self._child_home
        )
        self._procs[bin_name] = pr
        return pr

    def is_running(self, bin_name: str) -> bool:
        pr = self._procs.get(bin_name)
        return pr is not None and pr.popen.poll() is None

    def stop_gear(self, bin_name: str) -> None:
        """Gracefully stop a gear (SIGTERM) so it drains + deregisters."""
        pr = self._procs.pop(bin_name, None)
        if pr is None:
            return
        pr.popen.terminate()
        try:
            pr.popen.wait(timeout=10)  # time to drain + deregister from the directory
        except subprocess.TimeoutExpired:
            pr.popen.kill()
            try:
                pr.popen.wait(timeout=3)
            except subprocess.TimeoutExpired:
                pass
        try:
            pr.log_fh.close()
        except Exception:
            pass

    def start_gear(self, bin_name: str) -> Proc:
        """Relaunch a previously-stopped gear with the same config."""
        config, is_flight_control = self._specs[bin_name]
        return self.launch(bin_name, config, is_flight_control=is_flight_control)

    def status(self, method: str, path: str, **kw) -> int:
        return _status(method, path, **kw)

    def gear_url(self, bin_name: str) -> str:
        """Base URL of a gear's own listener (bypasses the edge)."""
        return f"http://127.0.0.1:{GEAR_PORTS[bin_name]}"

    def wait(self, desc: str, fn, *, timeout: int = ROUTE_SYNC_TIMEOUT) -> None:
        _poll(desc, fn, timeout=timeout, procs=self.procs)

    def shutdown_all(self) -> None:
        procs = list(self._procs.values())
        for pr in reversed(procs):
            pr.popen.terminate()
        for pr in reversed(procs):
            try:
                pr.popen.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pr.popen.kill()
                try:
                    pr.popen.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
            try:
                pr.log_fh.close()
            except Exception:
                pass
        self._procs.clear()


# ── session fixtures ──────────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def _require_oop_optin():
    """Skip the whole suite unless it is opted in via `make e2e-oop`.

    This suite is self-managed: its `oop_cluster` fixture runs a `cargo build`
    and orchestrates several processes, work that only `make e2e-oop` sets up
    (it pre-builds the binaries and runs pytest with `-o timeout_func_only=true`
    so the per-test timeout does not count the heavy fixture setup). Any other
    session that collects this suite — notably `make e2e-docker-smoke`, which
    runs `pytest -m smoke` across the whole tree under `pytest.ini`'s 10s
    timeout — would trip that timeout during the fixture build. Gate collection
    on the opt-in env var `make e2e-oop` sets so those runs skip gracefully
    (mirrors mini-chat/usage-collector gating on `E2E_BINARY`).
    """
    if os.environ.get("OOP_E2E") != "1":
        pytest.skip(
            "OOP_E2E not set — run the OoP loopback suite via: make e2e-oop",
            allow_module_level=True,
        )


@pytest.fixture(scope="session")
def cluster(tmp_path_factory):
    """Build, boot, and tear down the loopback OoP cluster; yield its controller."""
    if os.environ.get("OOP_E2E_SKIP") == "1":
        pytest.skip("OOP_E2E_SKIP=1")
    for port in (EDGE_PORT, DIRECTORY_PORT, *GEAR_PORTS.values()):
        if _port_in_use(port):
            pytest.skip(
                f"port {port} already in use — another server is running; "
                f"stop it or set OOP_E2E_SKIP=1"
            )

    _ensure_built()
    c = Cluster(str(tmp_path_factory.mktemp("oop-home")))
    try:
        # 1) flight-control (edge + DirectoryService)
        c.launch("flight-control", FLIGHT_CONTROL_CONFIG, is_flight_control=True)
        c.wait(
            "edge /healthz",
            lambda: _status("GET", "/healthz") == 200,
            timeout=FLIGHT_CONTROL_HEALTH_TIMEOUT,
        )

        # 2) OoP gears (register with the DirectoryService)
        for bin_name, cfg in GEAR_LAUNCH:
            c.launch(bin_name, cfg)

        # 3) wait for the edge to sync each gear's routes from the directory
        c.wait("hello route synced at edge", lambda: _status("GET", "/hello/v1/ping") == 200)
        c.wait(
            "consumer route synced at edge",
            lambda: _status(
                "POST",
                "/api-contracts-consumer/v1/charge",
                headers={"Authorization": f"Bearer {TOKEN}"},
                json={"amount_cents": 1, "currency": "USD", "description": "warmup"},
            ) not in (0, 404),
        )

        yield c
    finally:
        c.shutdown_all()


@pytest.fixture(scope="session")
def oop_cluster(cluster):
    """Edge base URL — the steady-state tests only need this."""
    return cluster.base_url


@pytest.fixture(scope="session")
def auth():
    """Bearer header accepted by static-authn accept_all."""
    return {"Authorization": f"Bearer {TOKEN}"}
