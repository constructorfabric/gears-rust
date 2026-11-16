"""E2E tests for OAGW body validation guardrails.

Raw sockets are used throughout because httpx/h11 validates Content-Length
client-side. Two of these rejections are produced in front of OAGW (hyper and
the api-gateway body limit); they are kept, named as such, so the layering
stays visible and a change in it fails loudly.
"""
import asyncio
import json
import os
from urllib.parse import urlparse

import httpx
import pytest

from .helpers import create_route, create_upstream, unique_alias

# Must match `oagw.config.max_body_size_bytes` in config/e2e-local.yaml.
OAGW_MAX_BODY_BYTES = 1_048_576


async def _raw_http_request(
    host: str, port: int, raw_request: bytes, timeout: float = 10.0, half_close: bool = False,
) -> tuple[int, dict, bytes]:
    """Send a raw HTTP request and return (status, lowercased headers, body)."""
    reader, writer = await asyncio.wait_for(
        asyncio.open_connection(host, port), timeout=timeout,
    )
    writer.write(raw_request)
    await writer.drain()
    if half_close:
        writer.write_eof()
    try:
        head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=timeout)
        lines = head.decode("latin-1").split("\r\n")
        status = int(lines[0].split(" ", 2)[1])
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            if name:
                headers[name.strip().lower()] = value.strip()
        length = int(headers.get("content-length", "0"))
        body = await asyncio.wait_for(reader.readexactly(length), timeout=timeout)
    finally:
        writer.close()
    return status, headers, body


def _raw_post(base_url: str, path: str, headers: dict, content_length: str, body: str) -> tuple[str, int, bytes]:
    parsed = urlparse(base_url)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 80
    raw = (
        f"POST {path} HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        f"Content-Type: application/json\r\n"
        f"Content-Length: {content_length}\r\n"
        + "".join(f"{k}: {v}\r\n" for k, v in headers.items())
        + "\r\n"
        + body
    ).encode()
    return host, port, raw


async def _echo_upstream(client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, prefix):
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(oagw_headers, await create_upstream(
        client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
    ))
    await create_route(
        client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
    )
    return alias


@pytest.mark.asyncio
async def test_invalid_content_length_rejected_by_http_server(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A non-integer Content-Length gets hyper's bare 400, before OAGW runs.

    OAGW's own check for this (scenario 7.4-A) is unreachable over REST; it is
    covered in-process by `e2e_invalid_content_length_returns_400` in
    `oagw/tests/e2e_smoke_test.rs`.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _echo_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "body-cl",
        )

    status, headers, body = await _raw_http_request(*_raw_post(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/echo", oagw_headers, "not-a-number", '{"test": true}',
    ))
    assert status == 400
    assert "x-oagw-error-source" not in headers
    assert "content-type" not in headers
    assert headers.get("content-length") == "0" and body == b""


@pytest.mark.scenario("negative-7.4-well-known-header-validation-errors-400", part="B")
@pytest.mark.asyncio
async def test_content_length_mismatch_returns_400(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 7.4-B: a body that ends short of its Content-Length is a 400
    `BODY_READ_FAILED`, not labelled "too large": only the size limit is a
    413 (#4981).
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _echo_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "body-mismatch",
        )

    # Declare 999 bytes, send 7, then half-close so the body read ends early.
    status, headers, body = await _raw_http_request(*_raw_post(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/echo", oagw_headers, "999", '{"a":1}',
    ), half_close=True)
    assert status == 400, (status, body[:300])
    assert headers.get("content-type", "").startswith("application/problem+json")
    assert headers.get("x-oagw-error-source") == "gateway"
    problem = json.loads(body)
    assert problem["status"] == 400, problem
    reasons = [v["reason"] for v in problem["context"]["field_violations"]]
    assert reasons == ["BODY_READ_FAILED"], problem


@pytest.mark.asyncio
async def test_body_exceeding_gateway_limit_returns_413(oagw_base_url, mock_upstream):
    """The api-gateway body limit (64 MB) answers before authentication and OAGW.

    No token and no upstream are needed: the 413 is `about:blank` problem+json
    from the toolkit error middleware, without `x-oagw-error-source`.
    """
    _ = mock_upstream
    status, headers, body = await _raw_http_request(*_raw_post(
        oagw_base_url, f"/oagw/v1/proxy/{unique_alias('body-gw')}/echo", {}, "200000000", "small body",
    ))
    assert status == 413
    assert headers.get("content-type", "").startswith("application/problem+json")
    assert "x-oagw-error-source" not in headers
    problem = json.loads(body)
    assert problem["type"] == "about:blank" and problem["status"] == 413


@pytest.mark.scenario("negative-8.1-maximum-body-size-limit-enforced")
@pytest.mark.asyncio
async def test_body_exceeding_limit_returns_413(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 8.1: a declared body over OAGW's own cap is a gateway 413."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _echo_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "body-big",
        )

    status, headers, body = await _raw_http_request(*_raw_post(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/echo", oagw_headers,
        str(OAGW_MAX_BODY_BYTES + 1), "small body",
    ))
    assert status == 413, body[:300]
    assert headers.get("content-type", "").startswith("application/problem+json")
    assert headers.get("x-oagw-error-source") == "gateway"
    problem = json.loads(body)
    assert problem["status"] == 413
    reasons = [v["reason"] for v in problem["context"]["field_violations"]]
    assert reasons == ["PAYLOAD_TOO_LARGE"]
    assert str(OAGW_MAX_BODY_BYTES) in problem["detail"]


@pytest.mark.scenario("negative-5.1-proxy-invoke-permission-required")
@pytest.mark.asyncio
async def test_unauthorized_caller_gets_403_before_body_checks(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4981: `proxy:invoke` is checked before OAGW reads or validates the body.

    A caller the policy denies gets 403 for a body over the limit (413 for an
    allowed caller) and for a WebSocket upgrade with a body (400 for an
    allowed caller). The deny is static-authz's nil-tenant case. A
    non-numeric Content-Length never reaches OAGW (the HTTP server refuses
    it); `denied_caller_gets_403_before_body_checks` covers it.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _echo_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "body-authz",
        )
    nil_token = os.getenv("E2E_AUTH_TOKEN_NIL_TENANT", "e2e-token-nil-tenant")
    denied = {"Authorization": f"Bearer {nil_token}"}
    path = f"/oagw/v1/proxy/{alias}/echo"

    oversized = str(OAGW_MAX_BODY_BYTES + 1)
    status, headers, body = await _raw_http_request(*_raw_post(
        oagw_base_url, path, denied, oversized, "{}",
    ), half_close=True)
    assert status == 403, (status, body[:300])
    assert headers.get("x-oagw-error-source") == "gateway"

    # The same request from an allowed caller is a 413.
    status, _, body = await _raw_http_request(*_raw_post(
        oagw_base_url, path, oagw_headers, oversized, "{}",
    ), half_close=True)
    assert status == 413, (status, body[:300])

    def ws_with_body(headers: dict) -> tuple[str, int, bytes]:
        parsed = urlparse(oagw_base_url)
        host, port = parsed.hostname or "127.0.0.1", parsed.port or 80
        raw = (
            f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n"
            "Connection: Upgrade\r\nUpgrade: websocket\r\n"
            "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
            "Content-Length: 2\r\n"
            + "".join(f"{k}: {v}\r\n" for k, v in headers.items())
            + "\r\n{}"
        ).encode()
        return host, port, raw

    status, headers, body = await _raw_http_request(*ws_with_body(denied))
    assert status == 403, (status, body[:300])
    assert headers.get("x-oagw-error-source") == "gateway"
    status, _, body = await _raw_http_request(*ws_with_body(oagw_headers))
    assert status == 400, (status, body[:300])
    reasons = [v["reason"] for v in json.loads(body)["context"]["field_violations"]]
    assert reasons == ["WS_UPGRADE_BODY_FORBIDDEN"], body[:300]
