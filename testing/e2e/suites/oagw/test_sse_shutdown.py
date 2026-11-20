"""E2E test for scenario 13.3: an open SSE stream ends when the server stops.

The shared server cannot be stopped, so the test runs a private server
(`private_server.py`) and stops it with SIGTERM.

The bound on the cut sits well under the runtime's 35 s HTTP drain deadline:
a stream that only the drain deadline ends fails the test.
"""
from __future__ import annotations

import subprocess
import time

import httpx
import pytest

from .helpers import create_route, create_upstream, unique_alias
from .private_server import private_oagw as start_private_oagw

# From SIGTERM to the transport error on the caller's side.
CUT_WITHIN_SECS = 2.0
# From SIGTERM to the process exit, without a kill.
EXIT_WITHIN_SECS = 10.0


@pytest.fixture
def private_oagw():
    """A private server with OAGW; yields ``(process, base_url)``."""
    with start_private_oagw(log_suffix="oagw-shutdown") as server:
        yield server


def _assert_exits(proc: subprocess.Popen) -> None:
    try:
        proc.wait(timeout=EXIT_WITHIN_SECS)
    except subprocess.TimeoutExpired:
        pytest.fail(f"the server did not exit within {EXIT_WITHIN_SECS}s of SIGTERM")


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
    _assert_exits(proc)
