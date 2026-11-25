"""Standalone mock upstream HTTP server for OAGW E2E tests.

Provides endpoints that simulate an upstream service (OpenAI-compatible JSON,
SSE streaming, echo, configurable errors). Started as a session-scoped pytest
fixture so the OAGW service under test can proxy to it.

Uses only stdlib asyncio — no aiohttp dependency. `/oauth2/token-jwt` verifies
client assertion signatures with `cryptography`, imported on first use.
"""
from __future__ import annotations

import asyncio
import calendar
import hashlib
import hmac
import base64
import json
import re
import struct
import time
from urllib.parse import parse_qs, quote, unquote_to_bytes
import logging

# ---------------------------------------------------------------------------
# Request/response helpers
# ---------------------------------------------------------------------------

class _Headers(dict):
    """Request headers by lowercase name (the last line wins), plus every
    line in arrival order in ``lines`` so a repeated header can be seen."""

    def __init__(self) -> None:
        super().__init__()
        self.lines: list[tuple[str, str]] = []


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

    headers = _Headers()
    for line in lines[1:]:
        if ":" in line:
            k, _, v = line.partition(":")
            headers[k.strip().lower()] = v.strip()
            headers.lines.append((k.strip().lower(), v.strip()))

    if "chunked" in headers.get("transfer-encoding", "").lower():
        return method, path, headers, await _read_chunked(reader, body_start)

    content_length = int(headers.get("content-length", "0"))
    body = body_start
    while len(body) < content_length:
        chunk = await reader.read(content_length - len(body))
        if not chunk:
            break
        body += chunk

    return method, path, headers, body


class MalformedRequest(Exception):
    """The request the proxy sent upstream is not valid HTTP/1.1."""


async def _read_chunked(reader: asyncio.StreamReader, buffered: bytes) -> bytes:
    """Decode a chunked request body; ``buffered`` is what followed the headers."""
    buf = bytearray(buffered)

    async def fill(n: int) -> None:
        while len(buf) < n:
            more = await reader.read(4096)
            if not more:
                raise asyncio.IncompleteReadError(bytes(buf), n)
            buf.extend(more)

    body = bytearray()
    while True:
        while b"\r\n" not in buf:
            await fill(len(buf) + 1)
        line, _, rest = bytes(buf).partition(b"\r\n")
        try:
            size = int(line.split(b";", 1)[0], 16)
        except ValueError:
            raise MalformedRequest(f"bad chunk size line {line[:40]!r}") from None
        buf[:] = rest
        if size == 0:
            # Skip trailers up to the final empty line.
            while not (buf.startswith(b"\r\n") or b"\r\n\r\n" in buf):
                await fill(len(buf) + 1)
            return bytes(body)
        await fill(size + 2)
        # Strict on purpose: a lenient parser would let a proxy that breaks
        # chunk framing still produce the expected echo.
        if buf[size:size + 2] != b"\r\n":
            raise MalformedRequest("chunk data not followed by CRLF")
        body.extend(buf[:size])
        del buf[:size + 2]


_HTTP_REASONS: dict[int, str] = {
    200: "OK", 201: "Created", 204: "No Content",
    400: "Bad Request", 401: "Unauthorized", 403: "Forbidden",
    404: "Not Found", 405: "Method Not Allowed", 409: "Conflict",
    429: "Too Many Requests",
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


def _with_headers(response: bytes, extra: dict[str, str]) -> bytes:
    """``response`` with ``extra`` header lines added after the status line."""
    status_line, _, rest = response.partition(b"\r\n")
    lines = "".join(f"{k}: {v}\r\n" for k, v in extra.items()).encode()
    return status_line + b"\r\n" + lines + rest


def _head_only(response: bytes) -> bytes:
    """The head of ``response``: what a server answers to HEAD, its
    ``Content-Length`` included."""
    head, _, _ = response.partition(b"\r\n\r\n")
    return head + b"\r\n\r\n"


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
    return _sse_raw(f"data: {data}\n\n")


def _sse_raw(event: str) -> bytes:
    """Wrap one already-framed SSE event in an HTTP chunk."""
    payload = event.encode()
    return f"{len(payload):x}\r\n".encode() + payload + b"\r\n"


def _sse_end() -> bytes:
    return b"0\r\n\r\n"


# ---------------------------------------------------------------------------
# Stateful counters for conditional-response endpoints
# ---------------------------------------------------------------------------

_endpoint_call_counts: dict[str, int] = {}

# Credentials /oauth2/token accepts; they match the secrets conftest provisions.
_OAUTH2_CLIENT = ("test-client-id", "test-client-secret")

# private_key_jwt (RFC 7523 §2.2): public keys registered through
# /oauth2/jwt-keys, by `kid`, as (alg, PEM, expected aud or None for this
# endpoint's URL), and the `jti`s already used.
_JWT_BEARER = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
_JWT_CLIENT_KEYS: dict[str, tuple[str, bytes, str | None]] = {}
_JWT_SEEN_JTI: set[str] = set()

# Gap between SSE events (/sse/events, /v1/chat/completions/stream): wide enough
# that buffering is visible over a busy CI runner's scheduling jitter.
SSE_EVENT_GAP_SECS = 0.2

# How long /sse/hold keeps a stream open without an event: past every bound a
# test waits for, short enough that a leaked stream does not outlive the run.
SSE_HOLD_SECS = 60


def _bump_count(key: str) -> int:
    """Increment and return the call count for *key* (1-indexed)."""
    _endpoint_call_counts[key] = _endpoint_call_counts.get(key, 0) + 1
    return _endpoint_call_counts[key]


# ---------------------------------------------------------------------------
# AWS Signature Version 4 verification (/aws/*)
# ---------------------------------------------------------------------------

# Access keys and session tokens /aws/* accepts; they match the secrets
# conftest provisions. The verifier is written from the AWS SigV4 spec with
# stdlib hmac/hashlib, independently of the gateway's signer.
AWS_ACCESS_KEYS = {"AKIDEXAMPLE": "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"}
AWS_SESSION_TOKENS = {"FwoGZXIvYXdzEXAMPLEsessionTOKEN+/="}
# How far `X-Amz-Date` may be from the mock's clock, as AWS allows.
AWS_MAX_SKEW_SECS = 15 * 60


class SigV4Error(Exception):
    """The request's signature does not verify; the message says why."""


def _aws_encode(raw: bytes, keep_slash: bool) -> str:
    return quote(raw, safe="-_.~/" if keep_slash else "-_.~")


def _aws_normalize_path(path: str) -> str:
    """Dot segments and repeated slashes removed, as AWS services do."""
    out: list[str] = []
    for segment in path.split("/"):
        if segment in ("", "."):
            continue
        if segment == "..":
            if out:
                out.pop()
        else:
            out.append(segment)
    normalized = "/" + "/".join(out)
    if path.endswith("/") and out:
        normalized += "/"
    return normalized


def _hmac(key: bytes, msg: str) -> bytes:
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


def verify_sigv4(method: str, target: str, headers, body: bytes) -> dict:
    """Verify a SigV4-signed request; return what was verified or raise SigV4Error.

    The canonical request is rebuilt from the request line as received, the
    header lines the `Authorization` header names and the body. Every
    `x-amz-*` header received must be signed, the payload hash must be the
    body's (or `UNSIGNED-PAYLOAD`) and `X-Amz-Date` within the skew.
    """
    lines = getattr(headers, "lines", list(headers.items()))
    auth = [v for k, v in lines if k == "authorization"]
    if len(auth) != 1 or not auth[0].startswith("AWS4-HMAC-SHA256 "):
        raise SigV4Error(f"expected one AWS4-HMAC-SHA256 authorization header, got {len(auth)}")
    fields = dict(
        part.split("=", 1) for part in auth[0][len("AWS4-HMAC-SHA256 "):].split(", ") if "=" in part
    )
    try:
        akid, date, region, service, terminator = fields["Credential"].split("/")
        signed = fields["SignedHeaders"].split(";")
        signature = fields["Signature"]
    except (KeyError, ValueError) as exc:
        raise SigV4Error(f"malformed authorization header: {exc}") from None
    if terminator != "aws4_request":
        raise SigV4Error("credential scope does not end in aws4_request")
    secret = AWS_ACCESS_KEYS.get(akid)
    if secret is None:
        raise SigV4Error("unknown access key id")
    if signed != sorted(signed) or "host" not in signed or "x-amz-date" not in signed:
        raise SigV4Error(f"bad SignedHeaders {signed}")
    unsigned_amz = sorted({k for k, _ in lines if k.startswith("x-amz-") and k not in signed})
    if unsigned_amz:
        raise SigV4Error(f"x-amz-* headers not signed: {unsigned_amz}")
    tokens = [v for k, v in lines if k == "x-amz-security-token"]
    if tokens and (len(tokens) != 1 or tokens[0] not in AWS_SESSION_TOKENS):
        raise SigV4Error("unknown session token")

    amz_dates = [v for k, v in lines if k == "x-amz-date"]
    if len(amz_dates) != 1 or not re.fullmatch(r"\d{8}T\d{6}Z", amz_dates[0]):
        raise SigV4Error(f"bad x-amz-date {amz_dates}")
    amz_date = amz_dates[0]
    if amz_date[:8] != date:
        raise SigV4Error("credential scope date is not the x-amz-date date")
    signed_at = calendar.timegm(time.strptime(amz_date, "%Y%m%dT%H%M%SZ"))
    if abs(time.time() - signed_at) > AWS_MAX_SKEW_SECS:
        raise SigV4Error("x-amz-date is outside the allowed skew")

    body_hash = hashlib.sha256(body).hexdigest()
    payload_hashes = [v for k, v in lines if k == "x-amz-content-sha256"]
    if len(payload_hashes) > 1:
        raise SigV4Error("repeated x-amz-content-sha256")
    payload_hash = payload_hashes[0] if payload_hashes else body_hash
    if payload_hash not in (body_hash, "UNSIGNED-PAYLOAD"):
        raise SigV4Error("x-amz-content-sha256 is not the body's hash")

    path, _, query = target.partition("?")
    if service == "s3":
        # S3 decodes the path and encodes it again before it checks.
        canonical_uri = _aws_encode(unquote_to_bytes(path), keep_slash=True) or "/"
    else:
        canonical_uri = _aws_encode(_aws_normalize_path(path).encode(), keep_slash=True)
    pairs = []
    for pair in filter(None, query.split("&")):
        name, _, value = pair.partition("=")
        pairs.append((
            _aws_encode(unquote_to_bytes(name), False),
            _aws_encode(unquote_to_bytes(value), False),
        ))
    canonical_query = "&".join(f"{n}={v}" for n, v in sorted(pairs))
    canonical_headers = ""
    for name in signed:
        values = [" ".join(v.split()) for k, v in lines if k == name]
        if not values:
            raise SigV4Error(f"signed header {name} not received")
        canonical_headers += f"{name}:{','.join(values)}\n"
    canonical = "\n".join([
        method, canonical_uri, canonical_query, canonical_headers, ";".join(signed), payload_hash,
    ])
    string_to_sign = "\n".join([
        "AWS4-HMAC-SHA256", amz_date, f"{date}/{region}/{service}/aws4_request",
        hashlib.sha256(canonical.encode()).hexdigest(),
    ])
    key = _hmac(_hmac(_hmac(_hmac(("AWS4" + secret).encode(), date), region), service), "aws4_request")
    expected = hmac.new(key, string_to_sign.encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, signature):
        raise SigV4Error("signature does not match")
    return {
        "region": region,
        "service": service,
        "signed_headers": ";".join(signed),
        "payload_hash": payload_hash,
        "session_token": bool(tokens),
    }


# ---------------------------------------------------------------------------
# WebSocket helpers (RFC 6455)
# ---------------------------------------------------------------------------

_WS_MAGIC = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _ws_accept_key(key: str) -> str:
    """Compute Sec-WebSocket-Accept from the client's Sec-WebSocket-Key."""
    digest = hashlib.sha1(key.strip().encode() + _WS_MAGIC).digest()
    return base64.b64encode(digest).decode()


def _ws_upgrade_response(accept_key: str, subprotocol: str | None = None) -> bytes:
    proto = f"Sec-WebSocket-Protocol: {subprotocol}\r\n" if subprotocol else ""
    return (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept_key}\r\n"
        f"{proto}"
        "\r\n"
    ).encode()


async def _ws_read_frame(reader: asyncio.StreamReader) -> tuple[bool, int, bytes] | None:
    """Read a single WebSocket frame. Returns (fin, opcode, payload) or None on EOF."""
    hdr = await reader.readexactly(2)
    fin = bool(hdr[0] & 0x80)
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

    return fin, opcode, bytes(payload)


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
    """WebSocket echo loop: read frames from client, echo text/binary back.

    A fragmented message (non-FIN data frame plus continuation frames) is
    reassembled and echoed as one frame with the first fragment's opcode.
    Control frames may arrive between fragments. A data frame while a message
    is open, or a continuation with none open, breaks message boundaries
    (RFC 6455 §5.4): the connection is closed with 1002 instead of guessing,
    so a proxy that re-frames messages can't pass the echo tests.
    """
    fragments: list[bytes] = []
    fragment_opcode = 0
    try:
        while True:
            result = await _ws_read_frame(reader)
            if result is None:
                break
            fin, opcode, payload = result

            stray_continuation = opcode == 0x0 and not fragments
            interleaved_message = opcode in (0x1, 0x2) and bool(fragments)
            if stray_continuation or interleaved_message:
                writer.write(_ws_write_frame(0x8, struct.pack("!H", 1002) + b"fragmentation error"))
                await writer.drain()
                break
            if opcode in (0x0, 0x1, 0x2) and (fragments or not fin):
                if opcode != 0x0:
                    fragment_opcode = opcode
                fragments.append(payload)
                if not fin:
                    continue
                opcode, payload = fragment_opcode, b"".join(fragments)
                fragments = []

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


def _b64url_decode(part: str) -> bytes:
    return base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))


def _check_client_assertion(headers: dict, path: str, form: dict) -> str:
    """Check a private_key_jwt token request. Returns the error description,
    or "" when the assertion is valid: form fields, the JWS header (alg and a
    registered kid), the signature with the registered public key, and the
    claims (iss = sub = the client, aud = the audience registered with the
    key or else this endpoint, exp, iat, a fresh jti)."""
    import time

    if form.get("client_assertion_type") != _JWT_BEARER:
        return "client_assertion_type is not jwt-bearer"
    if "client_secret" in form or headers.get("authorization"):
        return "a client secret was sent along with the assertion"
    parts = form.get("client_assertion", "").split(".")
    if len(parts) != 3:
        return "client_assertion is not a compact JWS"
    try:
        header = json.loads(_b64url_decode(parts[0]))
        claims = json.loads(_b64url_decode(parts[1]))
        signature = _b64url_decode(parts[2])
    except ValueError:
        return "client_assertion does not decode"
    alg, kid = header.get("alg"), header.get("kid")
    registered = _JWT_CLIENT_KEYS.get(kid or "")
    if registered is None:
        return "unknown kid"
    if alg != registered[0]:
        return f"alg {alg} does not match the key registered for the kid"
    signing_input = f"{parts[0]}.{parts[1]}".encode()
    # Imported here so the other endpoints work without `cryptography`.
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec, padding
    from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature

    public_key = serialization.load_pem_public_key(registered[1])
    try:
        if alg == "ES256":
            if len(signature) != 64:
                return "ES256 signature is not 64 bytes"
            der = encode_dss_signature(
                int.from_bytes(signature[:32], "big"), int.from_bytes(signature[32:], "big"),
            )
            public_key.verify(der, signing_input, ec.ECDSA(hashes.SHA256()))
        elif alg == "RS256":
            public_key.verify(signature, signing_input, padding.PKCS1v15(), hashes.SHA256())
        elif alg == "PS256":
            public_key.verify(
                signature, signing_input,
                padding.PSS(mgf=padding.MGF1(hashes.SHA256()), salt_length=padding.PSS.DIGEST_LENGTH),
                hashes.SHA256(),
            )
        else:
            return f"unsupported alg {alg}"
    except InvalidSignature:
        return "signature does not verify"
    client_id = _OAUTH2_CLIENT[0]
    if claims.get("iss") != client_id or claims.get("sub") != client_id:
        return "iss and sub must be the client id"
    if form.get("client_id", client_id) != client_id:
        return "client_id does not match the assertion"
    expected_aud = registered[2] or f"http://{headers.get('host', '')}{path}"
    if claims.get("aud") != expected_aud:
        return "aud is not the expected audience"
    now = time.time()
    exp, iat = claims.get("exp"), claims.get("iat")
    if not isinstance(exp, int) or not isinstance(iat, int):
        return "exp and iat must be numbers"
    if exp <= now or iat > now + 60 or exp - iat > 300:
        return "assertion is expired or too long-lived"
    jti = claims.get("jti")
    if not isinstance(jti, str) or not jti or jti in _JWT_SEEN_JTI:
        return "jti is missing or replayed"
    _JWT_SEEN_JTI.add(jti)
    return ""


# ---------------------------------------------------------------------------
# Route handlers
# ---------------------------------------------------------------------------

async def _handle(method: str, target: str, headers: dict, body: bytes, writer: asyncio.StreamWriter, reader: asyncio.StreamReader | None = None) -> None:
    path, _, query = target.partition("?")
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
        # The issued token names the client-authentication mode, so a test can
        # tell a Basic-auth client (RFC 6749 §2.3.1 header) from a form one.
        parsed = parse_qs(body.decode("utf-8", errors="replace"))
        form_params = {k: v[0] for k, v in parsed.items() if v}
        basic = headers.get("authorization", "")
        if basic.startswith("Basic ") and "client_id" not in form_params:
            mode = "basic"
            try:
                client_id, _, secret = base64.b64decode(basic[6:]).decode().partition(":")
            except ValueError:
                client_id, secret = "", ""
        elif not basic and "client_id" in form_params:
            mode = "form"
            client_id = form_params.get("client_id", "")
            secret = form_params.get("client_secret", "")
        else:
            mode = None
            client_id, secret = "", ""
        if form_params.get("grant_type") != "client_credentials":
            writer.write(_json_response(
                {"error": "unsupported_grant_type", "error_description": "grant_type must be client_credentials"},
                status=400,
            ))
        elif mode is None or (client_id, secret) != _OAUTH2_CLIENT:
            writer.write(_json_response(
                {"error": "invalid_client", "error_description": "client authentication failed"},
                status=401,
            ))
        else:
            writer.write(_json_response({
                "access_token": f"mock-e2e-token-{mode}",
                "expires_in": 3600,
                "token_type": "Bearer",
            }))

    # GET /counters/{key} — the call count of a stateful endpoint.
    elif method == "GET" and path.startswith("/counters/"):
        key = path[len("/counters/"):]
        writer.write(_json_response({"key": key, "count": _endpoint_call_counts.get(key, 0)}))

    # POST /oauth2/jwt-keys — register the public key a private_key_jwt
    # client signs with: {"kid", "alg", "public_key_pem"}, and optionally the
    # "aud" its assertions must carry instead of the token endpoint URL.
    elif method == "POST" and path == "/oauth2/jwt-keys":
        key = json.loads(body)
        _JWT_CLIENT_KEYS[key["kid"]] = (
            key["alg"], key["public_key_pem"].encode(), key.get("aud"),
        )
        writer.write(_json_response({"registered": key["kid"]}))

    # POST /oauth2/token-jwt — token endpoint for private_key_jwt clients.
    # Issues `mock-e2e-token-jwt-<kid>` for a valid assertion, else 401;
    # counts the tokens issued per kid under `token-jwt:<kid>`.
    elif method == "POST" and path == "/oauth2/token-jwt":
        parsed = parse_qs(body.decode("utf-8", errors="replace"))
        form_params = {k: v[0] for k, v in parsed.items() if v}
        if form_params.get("grant_type") != "client_credentials":
            problem = "grant_type must be client_credentials"
        else:
            problem = _check_client_assertion(headers, path, form_params)
        if problem:
            writer.write(_json_response(
                {"error": "invalid_client", "error_description": problem}, status=401,
            ))
        else:
            kid = json.loads(_b64url_decode(form_params["client_assertion"].split(".")[0]))["kid"]
            _bump_count(f"token-jwt:{kid}")
            writer.write(_json_response({
                "access_token": f"mock-e2e-token-jwt-{kid}",
                "expires_in": 3600,
                "token_type": "Bearer",
            }))

    # POST /oauth2/token-unavailable — a token endpoint whose identity
    # provider is down.
    elif method == "POST" and path == "/oauth2/token-unavailable":
        writer.write(_json_response({"error": "temporarily_unavailable"}, status=503))

    # GET|POST /aws/... — verifies the AWS SigV4 signature; 403 like AWS
    # when it does not, else echoes the request and what was verified.
    elif method in ("GET", "POST") and path.startswith("/aws/"):
        try:
            verified = verify_sigv4(method, target, headers, body)
        except SigV4Error as exc:
            writer.write(_json_response(
                {"error": "SignatureDoesNotMatch", "message": str(exc)}, status=403,
            ))
        else:
            writer.write(_json_response({
                "verified": verified,
                "method": method,
                "path": path,
                "query": query,
                "headers": headers,
                "header_lines": [[k, v] for k, v in getattr(headers, "lines", headers.items())],
                "body": body.decode("utf-8", errors="replace"),
            }))

    # POST /echo
    elif method == "POST" and path == "/echo":
        writer.write(_json_response({
            "method": method,
            "path": path,
            "query": query,
            "headers": headers,
            "body": body.decode("utf-8", errors="replace"),
        }))

    # Any method on /echo-any: the request as received, for routing tests of
    # methods other than POST. HEAD gets the GET answer's head and no body.
    elif path == "/echo-any":
        echoed = _with_headers(_json_response({
            "method": method,
            "path": path,
            "query": query,
            "headers": headers,
            "body": body.decode("utf-8", errors="replace"),
        }), {"Allow": "GET, HEAD, OPTIONS, POST", "X-Echo-Method": method})
        writer.write(_head_only(echoed) if method == "HEAD" else echoed)

    # HEAD /sse/events — the event stream's head; no events follow.
    elif method == "HEAD" and path == "/sse/events":
        writer.write(_sse_header())

    # POST /echo-lines — every request header line, repeated ones included
    elif method == "POST" and path == "/echo-lines":
        writer.write(_json_response({
            "header_lines": [[k, v] for k, v in getattr(headers, "lines", headers.items())],
        }))

    # POST /sse/events — spaced events carrying every SSE field
    elif method == "POST" and path == "/sse/events":
        writer.write(_sse_header())
        writer.write(_sse_raw("retry: 1500\n\n"))
        for i in range(1, 4):
            writer.write(_sse_raw(f"event: tick\nid: {i}\ndata: {{\"n\": {i}}}\n\n"))
            await writer.drain()
            await asyncio.sleep(SSE_EVENT_GAP_SECS)
        writer.write(_sse_raw("event: done\nid: 4\ndata: bye\n\n"))
        writer.write(_sse_end())

    # POST /sse/hold — one event, then the stream stays open with nothing to
    # send until the caller closes it (or SSE_HOLD_SECS pass).
    elif method == "POST" and path == "/sse/hold":
        writer.write(_sse_header())
        writer.write(_sse_chunk("held"))
        await writer.drain()
        if reader is not None:
            try:
                # b"" once the caller closes: nothing more is read from it.
                await asyncio.wait_for(reader.read(), timeout=SSE_HOLD_SECS)
            except asyncio.TimeoutError:
                pass
        writer.write(_sse_end())

    # POST /sse/fail — one event, then the connection closes without the
    # body's terminating chunk (an upstream failing mid-stream). A FIN, not a
    # reset: a reset may discard the event before the gateway reads it.
    elif method == "POST" and path == "/sse/fail":
        writer.write(_sse_header())
        writer.write(_sse_chunk("one"))
        await writer.drain()
        writer.close()
        await writer.wait_closed()

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
            await asyncio.sleep(SSE_EVENT_GAP_SECS)
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

    # GET|POST /error/{code}
    elif method in ("GET", "POST") and (m := re.fullmatch(r"/error/(\d+)", path)):
        code = int(m.group(1))
        writer.write(_json_response(
            {"error": {"message": f"Simulated error {code}", "type": "server_error", "code": f"error_{code}"}},
            status=code,
        ))

    # GET /status/{code}
    elif method == "GET" and (m := re.fullmatch(r"/status/(\d+)", path)):
        code = int(m.group(1))
        writer.write(_json_response({"status": code, "description": f"Status {code}"}, status=code))

    # GET /flaky/{key}/{n} — 503 for the first n calls with this key, 200
    # after: an upstream that recovers. Keys are per test, the mock is shared.
    elif method == "GET" and (m := re.fullmatch(r"/flaky/([\w-]+)/(\d+)", path)):
        call = _bump_count(f"flaky:{m.group(1)}")
        status = 503 if call <= int(m.group(2)) else 200
        writer.write(_json_response({"call": call, "status": status}, status=status))

    # GET /drop — reads the request and closes the connection without an
    # answer: the request reached the upstream, which then went away.
    elif method == "GET" and path == "/drop":
        return

    # GET /busy — 503 with the upstream's own Retry-After.
    elif method == "GET" and path == "/busy":
        writer.write(_with_headers(
            _json_response({"error": "busy"}, status=503), {"Retry-After": "120"},
        ))

    # GET /challenge — 401 with the upstream's own WWW-Authenticate.
    elif method == "GET" and path == "/challenge":
        writer.write(_with_headers(
            _json_response({"error": "invalid_token"}, status=401),
            {"WWW-Authenticate": 'Bearer realm="mock", error="invalid_token"'},
        ))

    # GET /ws/echo — WebSocket echo endpoint
    # GET /ws/handshake — same, but first sends the handshake request headers
    # as a JSON text frame
    elif method == "GET" and path in ("/ws/echo", "/ws/handshake") and "upgrade" in headers.get("connection", "").lower():
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
        offered = [p.strip() for p in headers.get("sec-websocket-protocol", "").split(",") if p.strip()]
        writer.write(_ws_upgrade_response(accept, offered[0] if offered else None))
        if path == "/ws/handshake":
            writer.write(_ws_write_frame(0x1, json.dumps({"headers": headers}).encode()))
        await writer.drain()
        # Run the echo loop; the caller must keep the connection open.
        await _ws_echo_loop(reader, writer)
        return  # Connection fully handled — don't close in _client.

    # 404 fallback; names the path and query as received, so a test can
    # check the outbound URL of a route the mock has no handler for.
    else:
        writer.write(_json_response(
            {"error": "not found", "method": method, "path": path, "query": query}, status=404,
        ))


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
                try:
                    method, path, headers, body = await _read_request(reader)
                except MalformedRequest as exc:
                    writer.write(_json_response({"error": f"malformed request: {exc}"}, status=400))
                    await writer.drain()
                    return
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
