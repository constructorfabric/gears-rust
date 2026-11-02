"""Gear test environment orchestrator.

Each test gear declares its infrastructure needs via a `GearTestEnv`.
The `test_env` session fixture reads it, starts sidecars, builds/configures/
starts the server, and yields a `RunningTestEnv` for tests to use.
"""

from __future__ import annotations

import atexit
import os
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Protocol, runtime_checkable

import pytest


# ── Defaults ──────────────────────────────────────────────────────────────

PROJECT_ROOT = Path(__file__).resolve().parents[3]  # testing/e2e/lib -> repo root
DEFAULT_CONFIG = PROJECT_ROOT / "config" / "e2e-local.yaml"


# ── Sidecar protocol ─────────────────────────────────────────────────────

@runtime_checkable
class SidecarProtocol(Protocol):
    name: str
    port: int | None

    def start(self) -> None: ...
    def stop(self) -> None: ...


# ── GearTestEnv ─────────────────────────────────────────────────────────

@dataclass
class GearTestEnv:
    """Declarative description of what a test gear needs from the server."""

    # Binary — path or name. Resolved via E2E_BINARY env var, then this field.
    binary: str = "cf-gears-example-server"

    # Config
    config_path: Path | None = None  # None = config/e2e-local.yaml
    config_patch: Callable[[str, "GearTestEnv"], str] | None = None

    # Server
    port: int = 8086
    health_path: str = "/healthz"
    # The application group's readiness (`tools/scripts/lib/readiness.py`);
    # under the gateway's `prefix_path` like every route.
    readiness_path: str = "/readyz"
    health_timeout: int = 60
    env: dict[str, str] = field(default_factory=dict)

    # Sidecars
    sidecars: list[Any] = field(default_factory=list)  # list[SidecarProtocol]

    # Logging — default uses port to avoid collisions between parallel runs.
    log_suffix: str | None = None  # e.g. "mini-chat" → cf-gears-e2e-8087-mini-chat.log


# ── RunningTestEnv ────────────────────────────────────────────────────────

@dataclass
class RunningTestEnv:
    """Yielded by the test_env fixture — provides access to the running server."""
    base_url: str
    env: GearTestEnv
    sidecars: dict[str, Any]  # name -> sidecar handle


# ── Server lifecycle helpers ──────────────────────────────────────────────

_server_proc: subprocess.Popen | None = None


def _stop_own_server() -> None:
    """Stop only the server process we started. Never pkill — other servers may be running."""
    global _server_proc
    if _server_proc is not None:
        _server_proc.terminate()
        try:
            _server_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            _server_proc.kill()
            _server_proc.wait(timeout=3)
        _server_proc = None


atexit.register(_stop_own_server)


def _resolve_binary(env: GearTestEnv) -> Path:
    """Resolve the server binary path.

    Priority:
    1. E2E_BINARY env var (explicit path — CI or developer override)
    2. env.binary as absolute path (if it is one)
    3. env.binary looked up in PATH
    """
    from shutil import which

    env_binary = os.environ.get("E2E_BINARY")
    if env_binary:
        p = Path(env_binary)
        if p.exists():
            print(f"[orchestrator] Using E2E_BINARY={p}")
            return p
        pytest.fail(f"E2E_BINARY={env_binary} does not exist")

    # Absolute or relative path
    p = Path(env.binary)
    if p.exists():
        print(f"[orchestrator] Using binary: {p}")
        return p

    # Search PATH
    found = which(env.binary)
    if found:
        print(f"[orchestrator] Found in PATH: {found}")
        return Path(found)

    pytest.fail(
        f"Binary not found: {env.binary}\n"
        f"Set E2E_BINARY env var or build first:\n"
        f"  make build"
    )


def _prepare_config(env: GearTestEnv) -> Path:
    """Load config, apply patches, write to temp file."""
    config_src = env.config_path or DEFAULT_CONFIG
    if not config_src.exists():
        pytest.fail(f"Config not found: {config_src}")

    config_text = config_src.read_text()

    if env.config_patch:
        config_text = env.config_patch(config_text, env)

    tmp = tempfile.NamedTemporaryFile(
        prefix="e2e-config-", suffix=".yaml", mode="w", delete=False,
    )
    tmp.write(config_text)
    tmp.close()
    return Path(tmp.name)


def _log_path(env: GearTestEnv) -> Path:
    """Derive log file path — uses port to avoid collisions between parallel runs."""
    logs_dir = PROJECT_ROOT / "testing" / "e2e" / "logs"
    logs_dir.mkdir(parents=True, exist_ok=True)
    suffix = f"-{env.log_suffix}" if env.log_suffix else ""
    return logs_dir / f"cf-gears-e2e-{env.port}{suffix}.log"


def _start_server(binary: Path, config: Path, env: GearTestEnv) -> subprocess.Popen:
    """Start the server process."""
    global _server_proc

    log = _log_path(env)
    log_fh = open(log, "w")
    proc_env = {**os.environ, **env.env}

    proc = subprocess.Popen(
        [str(binary), "--config", str(config), "run"],
        cwd=str(PROJECT_ROOT),
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        env=proc_env,
    )
    _server_proc = proc
    print(f"[orchestrator] Server started (pid={proc.pid}, port={env.port}, log={log})")
    return proc


def _load_readiness():
    """`tools/scripts/lib/readiness.py`: the launchers' shared startup criterion."""
    import importlib.util

    path = PROJECT_ROOT / "tools" / "scripts" / "lib" / "readiness.py"
    spec = importlib.util.spec_from_file_location("cf_gears_e2e_readiness", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


_readiness = _load_readiness()


def _wait_healthy(env: GearTestEnv, *, poll_interval: float = 1.0) -> None:
    """Poll `health_path`, then `readiness_path` until the application group
    started (`tools/scripts/lib/readiness.py`), or fail at `health_timeout`.

    A probe that times out or fails is retried until the deadline.
    """
    import httpx

    base = f"http://localhost:{env.port}"
    url, readyz = f"{base}{env.health_path}", f"{base}{env.readiness_path}"
    deadline = time.monotonic() + env.health_timeout
    last = "no answer"

    while time.monotonic() < deadline:
        try:
            r = httpx.get(url, timeout=3)
            if r.status_code != 200:
                last = f"{url}: {r.status_code}"
            else:
                ready = httpx.get(readyz, timeout=3)
                if _readiness.application_started(ready.status_code, ready.content):
                    print(f"[orchestrator] Server healthy: {url}, application started: {readyz}")
                    return
                last = f"{readyz}: {ready.status_code} {ready.content[:300]!r}"
        except httpx.HTTPError as e:
            last = f"{type(e).__name__}: {e}"
        time.sleep(poll_interval)

    log = _log_path(env)
    log_tail = ""
    if log.exists():
        log_tail = log.read_text()[-3000:]
    pytest.fail(
        f"Server did not become healthy within {env.health_timeout}s.\n"
        f"Health URL: {url}\n"
        f"Readiness URL: {readyz}\n"
        f"Last observation: {last}\n"
        f"Log tail:\n{log_tail}"
    )


# ── test_env fixture ──────────────────────────────────────────────────────

@pytest.fixture(scope="session")
def test_env(gear_test_env: GearTestEnv):
    """Orchestrate the full server lifecycle for a test gear.

    1. Start sidecars (so ports are known)
    2. Prepare config (with sidecar ports injected via config_patch)
    3. Resolve binary (from E2E_BINARY or PATH)
    4. Start server
    5. Wait for health
    6. Yield RunningTestEnv
    7. Teardown: stop server, stop sidecars
    """
    env = gear_test_env

    # 1. Start sidecars
    sidecar_handles: dict[str, Any] = {}
    for sc in env.sidecars:
        sc.start()
        sidecar_handles[sc.name] = sc

    # 2. Prepare config
    config_path = _prepare_config(env)

    # 3. Resolve binary
    binary_path = _resolve_binary(env)

    # 4. Start server
    proc = _start_server(binary_path, config_path, env)

    # 5. Health check
    _wait_healthy(env)

    yield RunningTestEnv(
        base_url=f"http://localhost:{env.port}",
        env=env,
        sidecars=sidecar_handles,
    )

    # 6. Teardown — only kill OUR server, not the shared CI server
    _stop_own_server()
    for sc in reversed(env.sidecars):
        sc.stop()


@pytest.fixture(scope="session")
def gear_test_env() -> GearTestEnv:
    """Default gear test environment. Override in gear conftest for custom needs."""
    return GearTestEnv()
