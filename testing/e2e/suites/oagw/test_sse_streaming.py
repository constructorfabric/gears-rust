"""E2E tests for OAGW SSE streaming proxy.

Both tests read the response with `client.stream` and time each event's
arrival, so a proxy that buffers the whole stream before sending fails.
"""
import json
import time

import httpx
import pytest

from .helpers import create_route, create_upstream, unique_alias
from .mock_upstream import SSE_EVENT_GAP_SECS


async def _stream_events(client, url, headers) -> tuple[httpx.Response, list[tuple[float, str]]]:
    """POST to an SSE endpoint; return the response and (arrival_secs, raw_event) pairs."""
    started = time.perf_counter()
    arrivals: list[tuple[float, str]] = []
    buf = b""
    async with client.stream(
        "POST", url, headers={**headers, "content-type": "application/json"}, json={"stream": True},
    ) as resp:
        async for chunk in resp.aiter_raw():
            buf += chunk
            while b"\n\n" in buf:
                event, buf = buf.split(b"\n\n", 1)
                arrivals.append((time.perf_counter() - started, event.decode()))
    assert buf == b"", f"trailing bytes outside an SSE event: {buf[:200]!r}"
    return resp, arrivals


async def _sse_upstream(client, base, headers, mock_url, cleanup, path):
    alias = unique_alias("sse")
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias,
    ))
    await create_route(client, base, headers, upstream["id"], ["POST"], path)
    return f"{base}/oagw/v1/proxy/{alias}{path}"


@pytest.mark.scenario("positive-13.1-sse-stream-forwarded-buffering")
@pytest.mark.asyncio
async def test_sse_proxy_streams_chat_completion(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 13.1: a chat-completion stream is forwarded unbuffered and unchanged."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=15.0) as client:
        url = await _sse_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "/v1/chat/completions/stream",
        )
        resp, arrivals = await _stream_events(client, url, oagw_headers)

    assert resp.status_code == 200
    assert resp.headers["content-type"].startswith("text/event-stream")
    assert resp.headers.get("content-length") is None
    assert resp.headers.get("x-oagw-error-source") == "upstream"

    events = [e for _, e in arrivals]
    assert all(e.startswith("data: ") for e in events), events
    assert events[-1] == "data: [DONE]"
    chunks = [json.loads(e[len("data: "):]) for e in events[:-1]]
    assert len(chunks) == 5
    assert [c["choices"][0]["delta"].get("content") for c in chunks[:4]] == [
        "Hello", " from", " mock", " server",
    ]
    assert chunks[-1]["choices"][0]["finish_reason"] == "stop"
    # The mock sleeps 10 ms between the first four events.
    spread = arrivals[-1][0] - arrivals[0][0]
    assert spread >= 0.025, f"events arrived together (buffered?): spread={spread * 1000:.1f}ms"


@pytest.mark.scenario("positive-13.1-sse-stream-forwarded-buffering")
@pytest.mark.asyncio
async def test_sse_proxy_preserves_event_fields(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """F0007: `retry:`, `event:` and `id:` fields pass through, each event on arrival."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=15.0) as client:
        url = await _sse_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "/sse/events",
        )
        resp, arrivals = await _stream_events(client, url, oagw_headers)

    assert resp.status_code == 200
    assert [e for _, e in arrivals] == [
        "retry: 1500",
        'event: tick\nid: 1\ndata: {"n": 1}',
        'event: tick\nid: 2\ndata: {"n": 2}',
        'event: tick\nid: 3\ndata: {"n": 3}',
        "event: done\nid: 4\ndata: bye",
    ]
    # The mock sleeps between ticks; buffering would deliver them together.
    # The whole spread is checked, not each gap, so jitter on one tick is ok.
    ticks = [t for t, e in arrivals if e.startswith("event: tick")]
    spread = ticks[-1] - ticks[0]
    assert spread >= 2 * SSE_EVENT_GAP_SECS * 0.75, f"tick spread {spread:.3f}s: buffered?"
