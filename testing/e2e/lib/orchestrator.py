"""Module test environment orchestrator.

Each test module declares its infrastructure needs via a `ModuleTestEnv`.
The `test_env` session fixture reads it, starts sidecars, builds/configures/
starts the server, and yields a `RunningTestEnv` for tests to use.
"""

from __future__ import annotations

import atexit
import os
import subprocess
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


# ── ModuleTestEnv ─────────────────────────────────────────────────────────

@dataclass
class ModuleTestEnv:
    """Declarative description of what a test module needs from the server."""

    # Binary — path or name. Resolved via E2E_BINARY env var, then this field.
    binary: str = "hyperspot-server"

    # Config
    config_path: Path | None = None  # None = config/e2e-local.yaml
    config_patch: Callable[[str, "ModuleTestEnv"], str] | None = None

    # Server
    port: int = 8086
    health_path: str = "/healthz"
    health_timeout: int = 60
    env: dict[str, str] = field(default_factory=dict)

    # Sidecars
    sidecars: list[Any] = field(default_factory=list)  # list[SidecarProtocol]

    # Logging — default uses port to avoid collisions between parallel runs.
    log_file: Path | None = None  # None = /tmp/hyperspot-e2e-{port}.log


# ── RunningTestEnv ────────────────────────────────────────────────────────

@dataclass
class RunningTestEnv:
    """Yielded by the test_env fixture — provides access to the running server."""
    base_url: str
    env: ModuleTestEnv
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


def _resolve_binary(env: ModuleTestEnv) -> Path:
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


def _prepare_config(env: ModuleTestEnv) -> Path:
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


def _log_path(env: ModuleTestEnv) -> Path:
    """Derive log file path — uses port to avoid collisions between parallel runs."""
    if env.log_file is not None:
        return env.log_file
    return Path(f"/tmp/hyperspot-e2e-{env.port}.log")


def _start_server(binary: Path, config: Path, env: ModuleTestEnv) -> subprocess.Popen:
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


def _wait_healthy(env: ModuleTestEnv) -> None:
    """Poll health endpoint until success or timeout."""
    import httpx

    url = f"http://localhost:{env.port}{env.health_path}"
    deadline = time.monotonic() + env.health_timeout

    while time.monotonic() < deadline:
        try:
            r = httpx.get(url, timeout=3)
            if r.status_code == 200:
                print(f"[orchestrator] Server healthy: {url}")
                return
        except httpx.ConnectError:
            pass
        time.sleep(1)

    log = _log_path(env)
    log_tail = ""
    if log.exists():
        log_tail = log.read_text()[-3000:]
    pytest.fail(
        f"Server did not become healthy within {env.health_timeout}s.\n"
        f"Health URL: {url}\n"
        f"Log tail:\n{log_tail}"
    )


# ── test_env fixture ──────────────────────────────────────────────────────

@pytest.fixture(scope="session")
def test_env(module_test_env: ModuleTestEnv):
    """Orchestrate the full server lifecycle for a test module.

    1. Start sidecars (so ports are known)
    2. Prepare config (with sidecar ports injected via config_patch)
    3. Resolve binary (from E2E_BINARY or PATH)
    4. Start server
    5. Wait for health
    6. Yield RunningTestEnv
    7. Teardown: stop server, stop sidecars
    """
    env = module_test_env

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
def module_test_env() -> ModuleTestEnv:
    """Default module test environment. Override in module conftest for custom needs."""
    return ModuleTestEnv()
