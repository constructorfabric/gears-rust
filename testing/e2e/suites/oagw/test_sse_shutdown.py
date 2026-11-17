"""E2E test for scenario 13.3: an open SSE stream ends when the server stops.

The shared server cannot be stopped, so the test runs a private server on its
own port, home directory and gRPC address, from the same binary and base
config, with the suite's mock as its upstream. It follows the private-server
setup of `suites/file_storage/private_env.py`.

The bound on the cut sits well under the runtime's 35 s HTTP drain deadline:
a stream that only the drain deadline ends fails the test.
"""
from __future__ import annotations

import os
import re
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import httpx
import pytest

from .helpers import create_route, create_upstream, unique_alias

# test_sse_shutdown.py -> oagw -> suites -> e2e -> testing -> repo root.
REPO_ROOT = Path(__file__).resolve().parents[4]

# From SIGTERM to the transport error on the caller's side.
CUT_WITHIN_SECS = 2.0
# From SIGTERM to the process exit, without a kill.
EXIT_WITHIN_SECS = 10.0


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


@pytest.fixture
def private_oagw():
    """A private server with OAGW; yields ``(process, base_url)``."""
    from lib.orchestrator import GearTestEnv, _log_path, _prepare_config

    binary = _server_binary()
    port = _free_port()
    base_url = f"http://127.0.0.1:{port}"
    home_dir = Path(tempfile.mkdtemp(prefix="cf-oagw-e2e-shutdown-"))
    # The shared server's config (only the suite's gears) when the launcher
    # passes it; the full e2e config otherwise.
    server_config = os.environ.get("E2E_SERVER_CONFIG")
    env = GearTestEnv(
        config_path=Path(server_config) if server_config else None,
        config_patch=lambda text, _env: _patch_config(text, port, home_dir),
        port=port,
        log_suffix="oagw-shutdown",
    )
    config_path = _prepare_config(env)
    log = _log_path(env)
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


@pytest.mark.scenario("positive-13.3-sse-stream-ends-on-gear-stop")
@pytest.mark.timeout(60, func_only=True)
async def test_sse_stream_is_cut_when_the_server_stops(
    private_oagw, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 13.3: SIGTERM aborts an open SSE stream at once, before the HTTP drain."""
    _ = mock_upstream
    proc, base_url = private_oagw
    alias = unique_alias("sse-stop")
    # Read timeout well past CUT_WITHIN_SECS: a stream that is not cut fails
    # on the bound below, not on a timeout of its own.
    async with httpx.AsyncClient(timeout=httpx.Timeout(10.0)) as client:
        # No cleanup: the private server keeps them in memory.
        upstream = await create_upstream(
            client, base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        await create_route(client, base_url, oagw_headers, upstream["id"], ["POST"], "/sse/hold")

        async with client.stream(
            "POST",
            f"{base_url}/oagw/v1/proxy/{alias}/sse/hold",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"stream": True},
        ) as resp:
            assert resp.status_code == 200
            assert resp.headers["content-type"].startswith("text/event-stream")
            chunks = resp.aiter_raw()
            first = b""
            while b"\n\n" not in first:
                first += await chunks.__anext__()
            assert first == b"data: held\n\n"

            proc.terminate()
            stopped_at = time.monotonic()
            # An incomplete chunked body (or a reset): the stream was cut, it
            # did not end. A read timeout is neither.
            with pytest.raises((httpx.RemoteProtocolError, httpx.ReadError)):
                async for chunk in chunks:
                    pytest.fail(f"data after the stop: {chunk!r}")
            cut_after = time.monotonic() - stopped_at

    assert cut_after < CUT_WITHIN_SECS, f"stream cut {cut_after:.1f}s after SIGTERM"
    try:
        proc.wait(timeout=EXIT_WITHIN_SECS)
    except subprocess.TimeoutExpired:
        pytest.fail(f"the server did not exit within {EXIT_WITHIN_SECS}s of SIGTERM")
