"""E2E tests for OAGW built-in CORS handling."""
import httpx
import pytest

from .helpers import create_route, create_upstream, delete_upstream, unique_alias


CORS_CONFIG = {
    "enabled": True,
    "allowed_origins": ["https://app.example.com"],
    "allowed_methods": ["GET", "POST"],
    "allowed_headers": ["content-type", "authorization"],
    "expose_headers": ["x-request-id"],
    "max_age": 3600,
    "allow_credentials": False,
}


# ---------------------------------------------------------------------------
# Preflight tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cors_preflight_allowed_origin(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Preflight with allowed origin returns 204 with CORS headers."""
    _ = mock_upstream
    alias = unique_alias("cors-pre-ok")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.request(
            "OPTIONS",
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "origin": "https://app.example.com",
                "access-control-request-method": "POST",
                "access-control-request-headers": "content-type",
            },
        )
        assert resp.status_code == 204, (
            f"Expected 204, got {resp.status_code}: {resp.text[:500]}"
        )
        assert resp.headers["access-control-allow-origin"] == "https://app.example.com"
        assert "POST" in resp.headers["access-control-allow-methods"]
        assert "content-type" in resp.headers.get("access-control-allow-headers", "")
        assert resp.headers.get("access-control-max-age") == "3600"
        assert "Origin" in resp.headers.get("vary", "")

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_cors_preflight_rejected_origin(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Preflight with disallowed origin returns 403."""
    _ = mock_upstream
    alias = unique_alias("cors-pre-bad")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.request(
            "OPTIONS",
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "origin": "https://evil.com",
                "access-control-request-method": "POST",
            },
        )
        assert resp.status_code == 403, (
            f"Expected 403, got {resp.status_code}: {resp.text[:500]}"
        )
        assert "access-control-allow-origin" not in resp.headers

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_cors_preflight_rejected_method(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Preflight with disallowed method returns 403."""
    _ = mock_upstream
    alias = unique_alias("cors-pre-meth")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,  # only GET, POST allowed
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["DELETE"], "/echo",
        )

        resp = await client.request(
            "OPTIONS",
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "origin": "https://app.example.com",
                "access-control-request-method": "DELETE",
            },
        )
        assert resp.status_code == 403, (
            f"Expected 403, got {resp.status_code}: {resp.text[:500]}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_cors_preflight_rejected_header(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Preflight with disallowed request header returns 403."""
    _ = mock_upstream
    alias = unique_alias("cors-pre-hdr")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,  # only content-type, authorization allowed
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.request(
            "OPTIONS",
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "origin": "https://app.example.com",
                "access-control-request-method": "POST",
                "access-control-request-headers": "x-custom-secret",
            },
        )
        assert resp.status_code == 403, (
            f"Expected 403, got {resp.status_code}: {resp.text[:500]}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_cors_preflight_with_credentials(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Preflight with allow_credentials reflects origin and includes credentials header."""
    _ = mock_upstream
    alias = unique_alias("cors-pre-cred")
    cors_with_creds = {
        **CORS_CONFIG,
        "allow_credentials": True,
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=cors_with_creds,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.request(
            "OPTIONS",
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "origin": "https://app.example.com",
                "access-control-request-method": "POST",
            },
        )
        assert resp.status_code == 204, (
            f"Expected 204, got {resp.status_code}: {resp.text[:500]}"
        )
        assert resp.headers["access-control-allow-origin"] == "https://app.example.com"
        assert resp.headers.get("access-control-allow-credentials") == "true"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Actual request tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cors_actual_request_includes_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Actual cross-origin request includes CORS response headers."""
    _ = mock_upstream
    alias = unique_alias("cors-actual")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "origin": "https://app.example.com",
            },
            json={"test": "cors"},
        )
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )
        assert resp.headers["access-control-allow-origin"] == "https://app.example.com"
        assert "x-request-id" in resp.headers.get("access-control-expose-headers", "")
        assert "Origin" in resp.headers.get("vary", "")

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_cors_actual_request_disallowed_origin_no_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Actual request with disallowed origin gets no CORS headers (browser blocks)."""
    _ = mock_upstream
    alias = unique_alias("cors-actual-bad")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "origin": "https://evil.com",
            },
            json={"test": "cors"},
        )
        # Request still succeeds (upstream proxied), but no CORS headers
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )
        assert "access-control-allow-origin" not in resp.headers

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# CORS disabled by default
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cors_disabled_no_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Without CORS config, no CORS headers are returned."""
    _ = mock_upstream
    alias = unique_alias("cors-off")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,  # No cors config
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "origin": "https://app.example.com",
            },
            json={"test": "no-cors"},
        )
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )
        assert "access-control-allow-origin" not in resp.headers

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Config validation
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cors_credentials_with_wildcard_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Creating upstream with allow_credentials + wildcard origin is rejected."""
    _ = mock_upstream
    alias = unique_alias("cors-cred-wild")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/upstreams",
            headers={**oagw_headers, "content-type": "application/json"},
            json={
                "server": {
                    "endpoints": [
                        {"host": "127.0.0.1", "port": 19876, "scheme": "http"},
                    ],
                },
                "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
                "alias": alias,
                "enabled": True,
                "cors": {
                    "enabled": True,
                    "allowed_origins": ["*"],
                    "allowed_methods": ["GET"],
                    "allow_credentials": True,
                },
            },
        )
        assert resp.status_code == 400, (
            f"Expected 400, got {resp.status_code}: {resp.text[:500]}"
        )


# ---------------------------------------------------------------------------
# Wildcard origin
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cors_wildcard_origin(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Wildcard origin returns '*' as Access-Control-Allow-Origin."""
    _ = mock_upstream
    alias = unique_alias("cors-wild")
    cors_wildcard = {
        **CORS_CONFIG,
        "allowed_origins": ["*"],
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=cors_wildcard,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "origin": "https://any-origin.com",
            },
            json={"test": "wildcard"},
        )
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )
        assert resp.headers["access-control-allow-origin"] == "*"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)
