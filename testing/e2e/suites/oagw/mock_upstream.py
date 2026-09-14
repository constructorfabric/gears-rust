"""Standalone mock upstream HTTP server for OAGW E2E tests.

Provides endpoints that simulate an upstream service (OpenAI-compatible JSON,
SSE streaming, echo, configurable errors). Started as a session-scoped pytest
fixture so the OAGW service under test can proxy to it.

Uses only stdlib asyncio — no aiohttp dependency.
"""
from __future__ import annotations

import asyncio
import hashlib
import base64
import json
import re
import struct
from urllib.parse import parse_qs
import logging

# ---------------------------------------------------------------------------
# Request/response helpers
# ---------------------------------------------------------------------------

async def _read_request(reader: asyncio.StreamReader) -> tuple[str, str, dict, bytes]:
    """Parse a minimal HTTP/1.1 request from the stream."""
    header_data = b""
    while b"\r\n\r\n" not in header_data:
        chunk = await reader.read(4096)
        if not chunk:
            break
        header_data += chunk

    header_part, _, body_start = header_data.partition(b"\r\n\r\n")
    lines = header_part.decode("utf-8", errors="replace").split("\r\n")
    request_line = lines[0] if lines else ""
    parts = request_line.split(" ", 2)
    method = parts[0] if len(parts) > 0 else "GET"
    path = parts[1] if len(parts) > 1 else "/"

    headers: dict[str, str] = {}
    for line in lines[1:]:
        if ":" in line:
            k, _, v = line.partition(":")
            headers[k.strip().lower()] = v.strip()

    content_length = int(headers.get("content-length", "0"))
    body = body_start
    while len(body) < content_length:
        chunk = await reader.read(content_length - len(body))
        if not chunk:
            break
        body += chunk

    return method, path, headers, body


_HTTP_REASONS: dict[int, str] = {
    200: "OK", 201: "Created", 204: "No Content",
    400: "Bad Request", 401: "Unauthorized", 403: "Forbidden",
    404: "Not Found", 405: "Method Not Allowed", 409: "Conflict",
    500: "Internal Server Error", 502: "Bad Gateway", 503: "Service Unavailable",
}


def _json_response(data: object, status: int = 200) -> bytes:
    body = json.dumps(data).encode()
    reason = _HTTP_REASONS.get(status, "Unknown")
    return (
        f"HTTP/1.1 {status} {reason}\r\n"
        f"Content-Type: application/json\r\n"
        f"Content-Length: {len(body)}\r\n"
        f"Connection: close\r\n"
        f"\r\n"
    ).encode() + body


def _sse_header() -> bytes:
    return (
        "HTTP/1.1 200 OK\r\n"
        "Content-Type: text/event-stream\r\n"
        "Cache-Control: no-cache\r\n"
        "Transfer-Encoding: chunked\r\n"
        "Connection: close\r\n"
        "\r\n"
    ).encode()


def _sse_chunk(data: str) -> bytes:
    payload = f"data: {data}\n\n".encode()
    return f"{len(payload):x}\r\n".encode() + payload + b"\r\n"


def _sse_end() -> bytes:
    return b"0\r\n\r\n"


# ---------------------------------------------------------------------------
# Stateful counters for conditional-response endpoints
# ---------------------------------------------------------------------------

_endpoint_call_counts: dict[str, int] = {}


def _bump_count(key: str) -> int:
    """Increment and return the call count for *key* (1-indexed)."""
    _endpoint_call_counts[key] = _endpoint_call_counts.get(key, 0) + 1
    return _endpoint_call_counts[key]


# ---------------------------------------------------------------------------
# WebSocket helpers (RFC 6455)
# ---------------------------------------------------------------------------

_WS_MAGIC = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _ws_accept_key(key: str) -> str:
    """Compute Sec-WebSocket-Accept from the client's Sec-WebSocket-Key."""
    digest = hashlib.sha1(key.strip().encode() + _WS_MAGIC).digest()
    return base64.b64encode(digest).decode()


def _ws_upgrade_response(accept_key: str) -> bytes:
    return (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept_key}\r\n"
        "\r\n"
    ).encode()


async def _ws_read_frame(reader: asyncio.StreamReader) -> tuple[int, bytes] | None:
    """Read a single WebSocket frame. Returns (opcode, payload) or None on EOF."""
    hdr = await reader.readexactly(2)
    opcode = hdr[0] & 0x0F
    masked = bool(hdr[1] & 0x80)
    length = hdr[1] & 0x7F

    if length == 126:
        raw = await reader.readexactly(2)
        length = struct.unpack("!H", raw)[0]
    elif length == 127:
        raw = await reader.readexactly(8)
        length = struct.unpack("!Q", raw)[0]

    mask_key = await reader.readexactly(4) if masked else b"\x00" * 4
    payload = bytearray(await reader.readexactly(length))
    if masked:
        for i in range(length):
            payload[i] ^= mask_key[i % 4]

    return opcode, bytes(payload)


def _ws_write_frame(opcode: int, payload: bytes) -> bytes:
    """Build an unmasked WebSocket frame."""
    frame = bytearray()
    frame.append(0x80 | opcode)  # FIN + opcode
    length = len(payload)
    if length < 126:
        frame.append(length)
    elif length < 65536:
        frame.append(126)
        frame.extend(struct.pack("!H", length))
    else:
        frame.append(127)
        frame.extend(struct.pack("!Q", length))
    frame.extend(payload)
    return bytes(frame)


async def _ws_echo_loop(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    """WebSocket echo loop: read frames from client, echo text/binary back."""
    try:
        while True:
            result = await _ws_read_frame(reader)
            if result is None:
                break
            opcode, payload = result

            if opcode == 0x8:  # Close
                # Echo the close frame back and exit.
                writer.write(_ws_write_frame(0x8, payload))
                await writer.drain()
                break
            elif opcode == 0x9:  # Ping → Pong
                writer.write(_ws_write_frame(0xA, payload))
                await writer.drain()
            elif opcode in (0x1, 0x2):  # Text or Binary → echo
                writer.write(_ws_write_frame(opcode, payload))
                await writer.drain()
    except (asyncio.IncompleteReadError, ConnectionError):
        pass


# ---------------------------------------------------------------------------
# Route handlers
# ---------------------------------------------------------------------------

async def _handle(method: str, path: str, headers: dict, body: bytes, writer: asyncio.StreamWriter, reader: asyncio.StreamReader | None = None) -> None:
    # POST /reset-counters — reset all stateful endpoint call counts
    if method == "POST" and path == "/reset-counters":
        _endpoint_call_counts.clear()
        writer.write(_json_response({"reset": True}))

    # GET /health
    elif method == "GET" and path == "/health":
        writer.write(_json_response({"status": "ok"}))

    # POST /oauth2/token — mock OAuth2 token endpoint
    elif method == "POST" and path == "/oauth2/token":
        # Parse URL-encoded form body and validate grant_type=client_credentials.
        parsed = parse_qs(body.decode("utf-8", errors="replace"))
        form_params = {k: v[0] for k, v in parsed.items() if v}
        if form_params.get("grant_type") != "client_credentials":
            writer.write(_json_response(
                {"error": "unsupported_grant_type", "error_description": "grant_type must be client_credentials"},
                status=400,
            ))
        else:
            writer.write(_json_response({
                "access_token": "mock-e2e-token",
                "expires_in": 3600,
                "token_type": "Bearer",
            }))

    # POST /echo
    elif method == "POST" and path == "/echo":
        writer.write(_json_response({
            "headers": headers,
            "body": body.decode("utf-8", errors="replace"),
        }))

    # POST /v1/chat/completions/stream
    elif method == "POST" and path == "/v1/chat/completions/stream":
        writer.write(_sse_header())
        words = ["Hello", " from", " mock", " server"]
        for i, word in enumerate(words):
            delta: dict = {}
            if i == 0:
                delta["role"] = "assistant"
            delta["content"] = word
            chunk = {
                "id": "chatcmpl-mock-stream",
                "object": "chat.completion.chunk",
                "created": 1_234_567_890,
                "model": "gpt-4-mock",
                "choices": [{"index": 0, "delta": delta, "finish_reason": None}],
            }
            writer.write(_sse_chunk(json.dumps(chunk)))
            await writer.drain()
            await asyncio.sleep(0.01)
        final = {
            "id": "chatcmpl-mock-stream",
            "object": "chat.completion.chunk",
            "created": 1_234_567_890,
            "model": "gpt-4-mock",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        }
        writer.write(_sse_chunk(json.dumps(final)))
        writer.write(_sse_chunk("[DONE]"))
        writer.write(_sse_end())

    # POST /v1/chat/completions
    elif method == "POST" and path == "/v1/chat/completions":
        writer.write(_json_response({
            "id": "chatcmpl-mock-123",
            "object": "chat.completion",
            "created": 1_234_567_890,
            "model": "gpt-4-mock",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello from mock server"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30},
        }))

    # GET /v1/models
    elif method == "GET" and path == "/v1/models":
        writer.write(_json_response({
            "object": "list",
            "data": [
                {"id": "gpt-4", "object": "model", "created": 1_234_567_890, "owned_by": "openai"},
                {"id": "gpt-3.5-turbo", "object": "model", "created": 1_234_567_890, "owned_by": "openai"},
            ],
        }))

    # GET /error/timeout
    elif method == "GET" and path == "/error/timeout":
        await asyncio.sleep(30)
        writer.write(_json_response({"error": "timeout"}, status=200))

    # GET /error/{code}
    elif method == "GET" and (m := re.fullmatch(r"/error/(\d+)", path)):
        code = int(m.group(1))
        writer.write(_json_response(
            {"error": {"message": f"Simulated error {code}", "type": "server_error", "code": f"error_{code}"}},
            status=code,
        ))

    # GET /status/{code}
    elif method == "GET" and (m := re.fullmatch(r"/status/(\d+)", path)):
        code = int(m.group(1))
        writer.write(_json_response({"status": code, "description": f"Status {code}"}, status=code))

    # POST /echo-401-once — returns 401 on first call, 200 thereafter.
    # Used to exercise the OAGW 401-retry logic.
    elif method == "POST" and path == "/echo-401-once":
        call = _bump_count("echo-401-once")
        if call == 1:
            writer.write(_json_response(
                {"error": "invalid_token", "error_description": "token expired"},
                status=401,
            ))
        else:
            writer.write(_json_response({
                "headers": headers,
                "body": body.decode("utf-8", errors="replace"),
                "call_number": call,
            }))

    # GET /ws/echo — WebSocket echo endpoint
    elif method == "GET" and path == "/ws/echo" and "upgrade" in headers.get("connection", "").lower():
        upgrade_val = headers.get("upgrade", "").lower()
        ws_key = headers.get("sec-websocket-key", "")
        ws_version = headers.get("sec-websocket-version", "")
        if upgrade_val != "websocket" or not ws_key or not ws_version:
            writer.write(_json_response(
                {"error": "invalid WebSocket upgrade: missing required headers"},
                status=400,
            ))
            await writer.drain()
            return
        accept = _ws_accept_key(ws_key)
        writer.write(_ws_upgrade_response(accept))
        await writer.drain()
        # Run the echo loop; the caller must keep the connection open.
        await _ws_echo_loop(reader, writer)
        return  # Connection fully handled — don't close in _client.

    # 404 fallback
    else:
        writer.write(_json_response({"error": "not found"}, status=404))


# ---------------------------------------------------------------------------
# Server lifecycle (used by conftest.py fixture)
# ---------------------------------------------------------------------------

class MockUpstreamServer:
    """Manages the mock upstream lifecycle for pytest fixtures."""

    def __init__(self, host: str = "127.0.0.1", port: int = 19876):
        self.host = host
        self.port = port
        self._server: asyncio.AbstractServer | None = None

    async def start(self) -> None:
        async def _client(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            try:
                method, path, headers, body = await _read_request(reader)
                await _handle(method, path, headers, body, writer, reader)
                await writer.drain()
            except (asyncio.CancelledError, GeneratorExit):
                raise
            except Exception:
                pass
            finally:
                try:
                    writer.close()
                except RuntimeError:
                    # Event loop may already be closing/shutdown.
                    pass
                try:
                    await writer.wait_closed()
                except (ConnectionError, RuntimeError):
                    # Ignore connection/loop errors during shutdown; the peer may have disconnected.
                    logging.debug("Ignoring connection/loop error during writer.close()", exc_info=True)

        self._server = await asyncio.start_server(_client, self.host, self.port)

    async def stop(self) -> None:
        if self._server:
            self._server.close()
            await self._server.wait_closed()
            self._server = None

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="OAGW mock upstream server")
    parser.add_argument("--port", type=int, default=19876)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

    server = MockUpstreamServer(host=args.host, port=args.port)

    async def _main() -> None:
        await server.start()
        assert server._server is not None
        async with server._server:
            await server._server.serve_forever()

    asyncio.run(_main())
