"""A private OAGW server for tests that stop it or need non-default gear config.

The shared server cannot be stopped or reconfigured, so these tests run a
private server on their own port, home directory and gRPC address, from the
same binary and base config, with the suite's mock as their upstream. It
follows the private-server setup of `suites/file_storage/private_env.py`.
"""
from __future__ import annotations

import contextlib
import os
import re
import socket
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Iterator

import httpx
import pytest

# private_server.py -> oagw -> suites -> e2e -> testing -> repo root.
REPO_ROOT = Path(__file__).resolve().parents[4]


def _server_binary() -> str:
    if os.environ.get("E2E_DOCKER_MODE"):
        pytest.skip("needs a local server binary; the Docker mode runs the server in a container")
    binary = os.environ.get("E2E_SERVER_BINARY") or os.environ.get("E2E_BINARY")
    if not binary:
        pytest.skip("E2E_SERVER_BINARY not set -- run via `make e2e-local SUITE=oagw`")
    if not Path(binary).exists():
        pytest.fail(f"server binary {binary!r} does not exist")
    return binary


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _patch_config(config_text: str, port: int, home_dir: Path) -> str:
    """Move the server off everything the shared server owns.

    Regex on the YAML text, as in `file_storage/private_env.py` (``pyyaml``
    is not an e2e dependency).
    """
    replacements = [
        (r"(home_dir\s*:\s*).*", f'"{home_dir.as_posix()}"'),
        # The shared server owns the base config's gRPC UDS path.
        (r"(listen_addr\s*:\s*).*", '"127.0.0.1:0"'),
        (r"(bind_addr\s*:\s*).*", f'"127.0.0.1:{port}"'),
    ]
    for pattern, value in replacements:
        config_text, count = re.subn(
            pattern, lambda m, v=value: m.group(1) + v, config_text, count=1
        )
        assert count == 1, f"{pattern!r} not found in the base config"
    return config_text


def _log_tail(path: Path, n: int = 3000) -> str:
    return path.read_text()[-n:] if path.exists() else ""


@contextlib.contextmanager
def private_oagw(
    oagw_config: dict[str, object] | None = None, log_suffix: str = "oagw-private",
) -> Iterator[tuple[subprocess.Popen, str]]:
    """Run a private server; yields ``(process, base_url)``.

    ``oagw_config`` overrides keys of the OAGW gear config (e.g.
    ``{"streaming_idle_timeout_secs": 1}``) through ``APP__`` variables.
    """
    from lib.orchestrator import GearTestEnv, _log_path, _prepare_config

    binary = _server_binary()
    port = _free_port()
    base_url = f"http://127.0.0.1:{port}"
    home_dir = Path(tempfile.mkdtemp(prefix=f"cf-{log_suffix}-"))
    # The shared server's config (only the suite's gears) when the launcher
    # passes it; the full e2e config otherwise.
    server_config = os.environ.get("E2E_SERVER_CONFIG")
    env = GearTestEnv(
        config_path=Path(server_config) if server_config else None,
        config_patch=lambda text, _env: _patch_config(text, port, home_dir),
        port=port,
        log_suffix=log_suffix,
    )
    config_path = _prepare_config(env)
    log = _log_path(env)
    gear_env = {
        f"APP__GEARS__OAGW__CONFIG__{key.upper()}": str(value)
        for key, value in (oagw_config or {}).items()
    }
    proc = None
    with open(log, "w") as log_fh:
        try:
            proc = subprocess.Popen(
                [binary, "--config", str(config_path), "run"],
                cwd=str(REPO_ROOT),
                stdout=log_fh,
                stderr=subprocess.STDOUT,
                env={
                    **os.environ,
                    # The suite manifest points the shared server's home here.
                    "APP__SERVER__HOME_DIR": str(home_dir),
                    "APP__GEARS__API_GATEWAY__CONFIG__ADVERTISE_URI": base_url,
                    **gear_env,
                },
            )
            deadline = time.monotonic() + env.health_timeout
            while True:
                if proc.poll() is not None:
                    pytest.fail(
                        f"private server exited (rc={proc.returncode}).\n"
                        f"Log tail:\n{_log_tail(log)}"
                    )
                try:
                    if httpx.get(f"{base_url}{env.health_path}", timeout=3).status_code == 200:
                        break
                except httpx.TransportError:
                    pass
                if time.monotonic() >= deadline:
                    pytest.fail(
                        f"private server not ready within {env.health_timeout}s.\n"
                        f"Log tail:\n{_log_tail(log)}"
                    )
                time.sleep(0.5)
            yield proc, base_url
        finally:
            if proc is not None and proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
