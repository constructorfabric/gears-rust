"""E2E tests for OAGW WebSocket proxy support.

Verifies that WebSocket upgrade requests are proxied through OAGW to the
upstream, and that bidirectional frame forwarding works correctly.
"""
import asyncio

import pytest
import websockets

from .helpers import create_route, create_upstream, delete_upstream, unique_alias


@pytest.mark.asyncio
async def test_websocket_echo_text(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """WebSocket text frames are proxied bidirectionally (echo round-trip)."""
    _ = mock_upstream
    alias = unique_alias("ws-echo")

    # OAGW management API uses httpx (HTTP), WS client uses websockets library.
    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    # Build WebSocket URL from the OAGW base URL.
    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"

    extra_headers = {k: v for k, v in oagw_headers.items()}

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
        # Send a text message and verify the echo.
        await ws.send("hello from e2e")
        reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
        assert reply == "hello from e2e", f"unexpected echo: {reply}"

        # Send another message to confirm the connection stays open.
        await ws.send("second message")
        reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
        assert reply == "second message", f"unexpected echo: {reply}"

        # Close cleanly.
        await ws.close()

    # Cleanup.
    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_echo_binary(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """WebSocket binary frames are proxied bidirectionally."""
    _ = mock_upstream
    alias = unique_alias("ws-bin")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
        payload = bytes(range(256))
        await ws.send(payload)
        reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
        assert reply == payload, f"binary echo mismatch: got {len(reply)} bytes"

        await ws.close()

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_upgrade_rejected_by_upstream(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """WebSocket upgrade to a non-WS upstream path returns an error."""
    _ = mock_upstream
    alias = unique_alias("ws-reject")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        # Route to /echo (a normal HTTP endpoint, not WebSocket).
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    # The upstream will return a non-101 response (likely 404 or 405),
    # which should cause the WebSocket handshake to fail.
    with pytest.raises(websockets.exceptions.InvalidStatus) as exc_info:
        async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
            pass
    # The proxy should not return a 101 for a non-WS upstream endpoint.
    assert exc_info.value.response.status_code != 101

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_echo_large_payload(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """WebSocket frames larger than the proxy's internal buffer are forwarded correctly."""
    _ = mock_upstream
    alias = unique_alias("ws-large")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
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

        await ws.close()

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_concurrent_bidirectional(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Multiple messages sent concurrently are all echoed back correctly."""
    _ = mock_upstream
    alias = unique_alias("ws-bidir")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    message_count = 20

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
        # Fire all messages without waiting for replies — exercises the
        # bidirectional copy loop under concurrent read/write pressure.
        messages = [f"msg-{i}" for i in range(message_count)]
        for msg in messages:
            await ws.send(msg)

        # Collect all replies.
        replies = []
        for _ in range(message_count):
            reply = await asyncio.wait_for(ws.recv(), timeout=10.0)
            replies.append(reply)

        # Echo server preserves order, so replies should match 1:1.
        assert replies == messages, (
            f"expected {messages}, got {replies}"
        )

        await ws.close()

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_mixed_text_binary_interleaved(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Text and binary frames interleaved in a single session are echoed with correct opcodes."""
    _ = mock_upstream
    alias = unique_alias("ws-mixed")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
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

            if kind == "text":
                assert isinstance(reply, str), (
                    f"expected str reply for text frame, got {type(reply)}"
                )
                assert reply == payload, f"text mismatch: {reply!r} != {payload!r}"
            else:
                assert isinstance(reply, bytes), (
                    f"expected bytes reply for binary frame, got {type(reply)}"
                )
                assert reply == payload, (
                    f"binary mismatch: {len(reply)} bytes != {len(payload)} bytes"
                )

        await ws.close()

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_rapid_small_message_burst(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """150 small messages fired rapidly are all echoed back without loss or reorder."""
    _ = mock_upstream
    alias = unique_alias("ws-burst")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    message_count = 150

    async with websockets.connect(
        ws_uri, additional_headers=extra_headers, max_size=2**20,
    ) as ws:
        # Fire all messages without waiting — exercises the relay loop under
        # sustained write pressure with small frames (< 100 bytes each).
        messages = [f'{{"seq":{i},"data":"msg-{i}"}}' for i in range(message_count)]
        for msg in messages:
            await ws.send(msg)

        # Collect all replies.
        replies = []
        for _ in range(message_count):
            reply = await asyncio.wait_for(ws.recv(), timeout=15.0)
            replies.append(reply)

        assert len(replies) == message_count, (
            f"expected {message_count} replies, got {len(replies)}"
        )
        # Echo server preserves order.
        assert replies == messages, (
            f"message order/content mismatch at first diff: "
            f"{next((i, r, m) for i, (r, m) in enumerate(zip(replies, messages, strict=True)) if r != m)}"
        )

        await ws.close()

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_websocket_utf8_multibyte_integrity(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Multi-byte UTF-8 (emoji, CJK, combining chars) survives the proxy without corruption."""
    _ = mock_upstream
    alias = unique_alias("ws-utf8")

    import httpx

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/ws/echo",
        )

    ws_url = oagw_base_url.replace("http://", "ws://").replace("https://", "wss://")
    ws_uri = f"{ws_url}/oagw/v1/proxy/{alias}/ws/echo"
    extra_headers = {k: v for k, v in oagw_headers.items()}

    # JSON payloads with progressively challenging UTF-8 content.
    payloads = [
        # CJK + emoji
        '{"msg":"こんにちは世界 🌍🔥 café résumé naïve"}',
        # 4-byte UTF-8: mathematical bold script, family emoji with ZWJ
        '{"msg":"𝓗𝓮𝓵𝓵𝓸 𝕋𝕖𝕤𝕥","emoji":"👨\u200d👩\u200d👧\u200d👦"}',
        # Mixed scripts: Cyrillic, Arabic, Chinese
        '{"content":"Привет мир • مرحبا بالعالم • 你好世界","ok":true}',
        # Combining characters
        '{"text":"e\u0301 n\u0303 o\u0308","flag":"\U0001f3f3\ufe0f\u200d\U0001f308"}',
    ]

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
        for i, payload in enumerate(payloads):
            await ws.send(payload)
            reply = await asyncio.wait_for(ws.recv(), timeout=5.0)
            assert isinstance(reply, str), f"payload {i}: expected str, got {type(reply)}"
            assert reply == payload, (
                f"payload {i}: UTF-8 corruption through proxy\n"
                f"  sent: {payload!r}\n"
                f"  got:  {reply!r}"
            )
            # Verify JSON round-trip integrity.
            import json
            assert json.loads(reply) == json.loads(payload), (
                f"payload {i}: JSON semantic mismatch"
            )

        await ws.close()

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

    async with websockets.connect(ws_uri, additional_headers=extra_headers) as ws:
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

        await ws.close()

    async with httpx.AsyncClient(timeout=10.0) as client:
        await delete_upstream(client, oagw_base_url, oagw_headers, uid)