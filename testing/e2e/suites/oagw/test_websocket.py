"""E2E tests for OAGW WebSocket proxy support.

Verifies that WebSocket upgrade requests are proxied through OAGW to the
upstream, and that bidirectional frame forwarding works correctly.
"""
import asyncio
import json

import httpx
import pytest
import websockets
from websockets.exceptions import InvalidStatus

from .helpers import APIKEY_AUTH_PLUGIN_ID, create_route, create_upstream, unique_alias


async def _ws_uri(base, headers, mock_url, cleanup, prefix, paths=("/ws/echo",), **upstream_kwargs):
    """Create an upstream with a GET route per path; return the WS proxy base URI."""
    alias = unique_alias(prefix)
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(headers, await create_upstream(
            client, base, headers, mock_url, alias=alias, **upstream_kwargs,
        ))
        for path in paths:
            await create_route(client, base, headers, upstream["id"], ["GET"], path)
    ws_base = base.replace("http://", "ws://").replace("https://", "wss://")
    return f"{ws_base}/oagw/v1/proxy/{alias}"


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
async def test_websocket_echo_text(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 14.1: WebSocket text frames are proxied bidirectionally (echo round-trip)."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-echo")

    async with websockets.connect(f"{base}/ws/echo", additional_headers=oagw_headers) as ws:
        assert ws.response.headers.get("x-oagw-error-source") == "upstream"
        await ws.send("hello from e2e")
        assert await asyncio.wait_for(ws.recv(), timeout=5.0) == "hello from e2e"

        # A second message confirms the connection stays open.
        await ws.send("second message")
        assert await asyncio.wait_for(ws.recv(), timeout=5.0) == "second message"

        await ws.close()
    assert ws.close_code == 1000


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
async def test_websocket_echo_binary(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """WebSocket binary frames are proxied bidirectionally."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-bin")

    async with websockets.connect(f"{base}/ws/echo", additional_headers=oagw_headers) as ws:
        payload = bytes(range(256))
        await ws.send(payload)
        reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
        assert reply == payload, f"binary echo mismatch: got {len(reply)} bytes"


@pytest.mark.asyncio
async def test_websocket_upgrade_rejected_by_upstream(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """An upgrade the upstream refuses is a gateway 502, not a 101.

    This pins today's behaviour. STR-06 will pass the upstream's own answer
    through as an upstream error, and this test changes with it (see
    `test_websocket_upgrade_refusal_status_passed_through`).

    The control connect to `/ws/echo` on the same alias proves the alias,
    route and token are fine, so the rejection can only come from the
    upstream refusing `/v1/models` (a plain HTTP endpoint).
    """
    _ = mock_upstream
    base = await _ws_uri(
        oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-reject",
        paths=("/ws/echo", "/v1/models"),
    )

    async with websockets.connect(f"{base}/ws/echo", additional_headers=oagw_headers) as ws:
        assert ws.response.headers.get("x-oagw-error-source") == "upstream"

    with pytest.raises(InvalidStatus) as exc_info:
        async with websockets.connect(f"{base}/v1/models", additional_headers=oagw_headers):
            pass
    resp = exc_info.value.response
    assert resp.status_code == 502
    assert resp.headers.get("x-oagw-error-source") == "gateway"
    assert resp.headers.get("content-type", "").startswith("application/problem+json")


@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="STR-06: every refused upgrade becomes the same gateway 502; the "
           "upstream's own status is lost",
)
async def test_websocket_upgrade_refusal_status_passed_through(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """An upstream's own non-101 answer (here 403) reaches the client as an upstream error."""
    _ = mock_upstream
    base = await _ws_uri(
        oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-refuse",
        paths=("/status",),
    )
    with pytest.raises(InvalidStatus) as exc_info:
        async with websockets.connect(f"{base}/status/403", additional_headers=oagw_headers):
            pass
    resp = exc_info.value.response
    assert resp.status_code == 403
    assert resp.headers.get("x-oagw-error-source") == "upstream"


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
async def test_websocket_echo_large_payload(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """WebSocket frames larger than the proxy's internal buffer are forwarded correctly."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-large")

    async with websockets.connect(f"{base}/ws/echo", additional_headers=oagw_headers) as ws:
        # 64 KiB payload — exceeds the 8192-byte internal copy buffer,
        # forcing multiple read/write cycles through the bridge.
        payload = bytes(i % 256 for i in range(65536))
        await ws.send(payload)
        reply = await asyncio.wait_for(ws.recv(), timeout=10.0)
        assert isinstance(reply, bytes), f"expected binary reply, got {type(reply)}"
        assert reply == payload, (
            f"large payload mismatch: sent {len(payload)} bytes, "
            f"got {len(reply)} bytes"
        )


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
@pytest.mark.timeout(30)
async def test_websocket_concurrent_bidirectional(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Sending and receiving at the same time with more than 1 MiB in flight.

    A half-duplex relay, or one that only drains after a write completes,
    deadlocks once the socket buffers fill in both directions.
    """
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-bidir")
    frame = 1024
    count = 2000  # ~2 MiB each way

    async with websockets.connect(
        f"{base}/ws/echo", additional_headers=oagw_headers, max_queue=None,
    ) as ws:
        sent = [f"{i:06d}".encode() + bytes(frame - 6) for i in range(count)]

        async def sender():
            for msg in sent:
                await ws.send(msg)

        async def receiver():
            return [await asyncio.wait_for(ws.recv(), timeout=10.0) for _ in range(count)]

        _, replies = await asyncio.gather(sender(), receiver())
        assert replies == sent


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
async def test_websocket_mixed_text_binary_interleaved(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Text and binary frames interleaved in a single session are echoed with correct opcodes."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-mixed")

    async with websockets.connect(f"{base}/ws/echo", additional_headers=oagw_headers) as ws:
        # Interleave text and binary frames — mimics APIs that use text for
        # JSON control messages and binary for audio/image data.
        messages = [
            ("text", '{"type":"control","action":"start"}'),
            ("binary", bytes(range(256))),
            ("text", '{"type":"data","seq":1,"content":"こんにちは 🌍"}'),
            ("binary", b"\x00\xff" * 500),
            ("text", '{"type":"control","action":"stop"}'),
            ("binary", bytes(i % 256 for i in range(1024))),
        ]

        for kind, payload in messages:
            await ws.send(payload)
            reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
            expected_type = str if kind == "text" else bytes
            assert isinstance(reply, expected_type), (
                f"expected {expected_type.__name__} reply for {kind} frame, got {type(reply)}"
            )
            assert reply == payload, f"{kind} mismatch: {reply!r} != {payload!r}"


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
@pytest.mark.timeout(30)
async def test_websocket_rapid_small_message_burst(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A burst far past the socket buffers survives a slow reader, without loss or reorder.

    The client writes ~4.5 MiB before reading anything, so the relay must
    hold the upstream back (backpressure) instead of dropping or reordering.
    """
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-burst")
    count = 4600
    messages = [json.dumps({"seq": i, "data": "x" * 1000}) for i in range(count)]

    async with websockets.connect(
        f"{base}/ws/echo", additional_headers=oagw_headers, max_queue=16,
    ) as ws:
        async def write_all():
            for m in messages:
                await ws.send(m)

        writer = asyncio.create_task(write_all())
        # Read slowly at first so replies back up through the relay.
        replies = []
        for i in range(count):
            if i < 50:
                await asyncio.sleep(0.01)
            replies.append(await asyncio.wait_for(ws.recv(), timeout=10.0))
        await writer

        assert len(replies) == count
        assert replies == messages, (
            f"order/content mismatch at first diff: "
            f"{next(i for i, (r, m) in enumerate(zip(replies, messages, strict=True)) if r != m)}"
        )


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
async def test_websocket_utf8_multibyte_integrity(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Multi-byte UTF-8 (emoji, CJK, combining chars) survives the proxy without corruption."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-utf8")
    ws_uri = f"{base}/ws/echo"

    # JSON payloads with progressively challenging UTF-8 content.
    payloads = [
        # CJK + emoji
        '{"msg":"こんにちは世界 🌍🔥 café résumé naïve"}',
        # 4-byte UTF-8: mathematical bold script, family emoji with ZWJ
        '{"msg":"𝓗𝓮𝓵𝓵𝓸 𝕋𝕖𝕤𝕥","emoji":"👨‍👩‍👧‍👦"}',
        # Mixed scripts: Cyrillic, Arabic, Chinese
        '{"content":"Привет мир • مرحبا بالعالم • 你好世界","ok":true}',
        # Combining characters
        '{"text":"é ñ ö","flag":"\U0001f3f3️‍\U0001f308"}',
    ]

    async with websockets.connect(ws_uri, additional_headers=oagw_headers) as ws:
        for i, payload in enumerate(payloads):
            await ws.send(payload)
            reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
            assert isinstance(reply, str), f"payload {i}: expected str, got {type(reply)}"
            assert reply == payload, (
                f"payload {i}: UTF-8 corruption through proxy\n"
                f"  sent: {payload!r}\n"
                f"  got:  {reply!r}"
            )
            assert json.loads(reply) == json.loads(payload), (
                f"payload {i}: JSON semantic mismatch"
            )

    # --- Invalid UTF-8 as binary frames (RFC 6455 §5.6: no UTF-8 requirement) ---
    # These byte sequences are invalid UTF-8 but must pass through cleanly
    # as binary frames without corruption or rejection.
    invalid_utf8_payloads = [
        # Truncated 2-byte sequence (C0 without continuation)
        b"\xc0",
        # Truncated 3-byte sequence (E0 80 without final byte)
        b"\xe0\x80",
        # Truncated 4-byte sequence (F0 90 80 without final byte)
        b"\xf0\x90\x80",
        # Overlong encoding of '/' (0x2F) — forbidden by RFC 3629
        b"\xc0\xaf",
        # Surrogate half (U+D800 encoded as CESU-8 — invalid in UTF-8)
        b"\xed\xa0\x80",
        # Valid ASCII mixed with invalid continuation bytes
        b"hello\x80world\xfe\xff",
        # 0xFE and 0xFF are never valid in UTF-8
        b"\xfe\xfe\xff\xff",
        # Mixed: valid JSON envelope wrapping broken bytes
        b'{"data":"' + b"\xc3\x28\xe2\x82" + b'"}',
    ]

    async with websockets.connect(ws_uri, additional_headers=oagw_headers) as ws:
        for i, payload in enumerate(invalid_utf8_payloads):
            await ws.send(payload)
            reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
            assert isinstance(reply, bytes), (
                f"invalid-utf8 payload {i}: expected bytes reply for binary frame, "
                f"got {type(reply)}"
            )
            assert reply == payload, (
                f"invalid-utf8 payload {i}: binary mismatch through proxy\n"
                f"  sent: {payload!r}\n"
                f"  got:  {reply!r}"
            )


@pytest.mark.scenario("positive-14.7-fragmented-message-relayed-without-size-limit")
@pytest.mark.asyncio
async def test_websocket_fragmented_message(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 14.7: a message split into continuation frames arrives whole."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-frag")

    async with websockets.connect(f"{base}/ws/echo", additional_headers=oagw_headers) as ws:
        # An iterable is sent as one fragmented message (FIN only on the last).
        await ws.send(["frag-a-", "frag-b-", "frag-c"])
        assert await asyncio.wait_for(ws.recv(), timeout=5.0) == "frag-a-frag-b-frag-c"


@pytest.mark.scenario("positive-14.1-websocket-upgrade-proxied")
@pytest.mark.asyncio
async def test_websocket_subprotocol_negotiated(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 14.1: `Sec-WebSocket-Protocol` is forwarded and the upstream's choice returned."""
    _ = mock_upstream
    base = await _ws_uri(oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-proto")

    async with websockets.connect(
        f"{base}/ws/echo", additional_headers=oagw_headers, subprotocols=["e2e.v1", "e2e.v2"],
    ) as ws:
        assert ws.subprotocol == "e2e.v1"


@pytest.mark.scenario("positive-14.2-auth-injected-during-handshake")
@pytest.mark.asyncio
async def test_websocket_handshake_auth_injected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 14.2: the auth plugin runs on the upgrade request."""
    _ = mock_upstream
    base = await _ws_uri(
        oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ws-auth",
        paths=("/ws/handshake",),
        auth={
            "type": APIKEY_AUTH_PLUGIN_ID,
            "sharing": "private",
            "config": {
                "header": "authorization",
                "prefix": "Bearer ",
                "secret_ref": "cred://openai-key",
            },
        },
    )

    async with websockets.connect(f"{base}/ws/handshake", additional_headers=oagw_headers) as ws:
        handshake = json.loads(await asyncio.wait_for(ws.recv(), timeout=5.0))
        assert handshake["headers"].get("authorization") == "Bearer sk-test-e2e-fake-key"
