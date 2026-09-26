"""E2E tests for OAGW built-in CORS handling."""
import httpx
import pytest

from .helpers import (
    assert_problem,
    create_route,
    create_upstream,
    create_upstream_raw,
    list_all,
    unique_alias,
)


CORS_CONFIG = {
    "enabled": True,
    "allowed_origins": ["https://app.example.com"],
    "allowed_methods": ["GET", "POST"],
    "expose_headers": ["x-request-id"],
    "allow_credentials": False,
}


# ---------------------------------------------------------------------------
# Preflight tests
# ---------------------------------------------------------------------------


@pytest.mark.scenario("positive-10.2-built-cors-handling")
@pytest.mark.asyncio
async def test_cors_preflight_fully_permissive(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Preflight with no auth headers returns fully permissive 204.

    The permissive preflight echoes origin, method, headers, enables
    credentials, and sets a long max-age — all without upstream resolution.
    The actual-request path enforces the real CORS policy.
    """
    _ = mock_upstream
    alias = unique_alias("cors-pre-noauth")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        ))
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        # Send preflight with ONLY Origin + ACRM — no Authorization, no tenant headers.
        resp = await client.request(
            "OPTIONS",
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
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
        assert resp.headers.get("access-control-allow-credentials") == "true"
        assert resp.headers.get("access-control-max-age") == "86400"
        assert "Origin" in resp.headers.get("vary", "")


@pytest.mark.scenario("positive-10.2-built-cors-handling")
@pytest.mark.asyncio
async def test_cors_preflight_permissive_echoes_any_method(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Preflight with method not in upstream CORS config still returns 204."""
    _ = mock_upstream
    alias = unique_alias("cors-pre-any")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,  # only GET, POST allowed
        ))
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
        assert resp.status_code == 204, (
            f"Expected 204 (permissive), got {resp.status_code}: {resp.text[:500]}"
        )
        assert "DELETE" in resp.headers["access-control-allow-methods"]


# ---------------------------------------------------------------------------
# Actual request tests
# ---------------------------------------------------------------------------


@pytest.mark.scenario("positive-10.2-built-cors-handling")
@pytest.mark.asyncio
async def test_cors_actual_request_includes_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Actual cross-origin request includes CORS response headers."""
    _ = mock_upstream
    alias = unique_alias("cors-actual")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        ))
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


@pytest.mark.scenario("positive-10.2-built-cors-handling")
@pytest.mark.asyncio
async def test_cors_actual_request_disallowed_origin_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Actual request with disallowed origin is rejected with 403 before reaching upstream."""
    _ = mock_upstream
    alias = unique_alias("cors-actual-bad")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=CORS_CONFIG,
        ))
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
        # Same status as an authz deny; the reason tells them apart.
        assert_problem(resp, 403, reason="CORS_ORIGIN_NOT_ALLOWED")


# ---------------------------------------------------------------------------
# CORS disabled by default
# ---------------------------------------------------------------------------


@pytest.mark.scenario("positive-10.2-built-cors-handling")
@pytest.mark.asyncio
async def test_cors_disabled_no_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Without CORS config, no CORS headers are returned."""
    _ = mock_upstream
    alias = unique_alias("cors-off")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,  # No cors config
        ))
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
        cors_headers = [h for h in resp.headers if h.lower().startswith("access-control-")]
        assert cors_headers == [], f"OAGW emitted CORS headers without config: {cors_headers}"


# ---------------------------------------------------------------------------
# Config validation
# ---------------------------------------------------------------------------


@pytest.mark.scenario("negative-10.3-cors-credentials-wildcard-rejected-config-validation")
@pytest.mark.asyncio
async def test_cors_credentials_with_wildcard_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
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
                "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
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
        body = assert_problem(resp, 400, esrc=None, reason="CORS_CREDENTIALS_WITH_WILDCARD")
        assert [v["field"] for v in body["context"]["field_violations"]] == ["cors.allow_credentials"]


# ---------------------------------------------------------------------------
# Wildcard origin
# ---------------------------------------------------------------------------


@pytest.mark.scenario("positive-10.2-built-cors-handling")
@pytest.mark.asyncio
async def test_cors_wildcard_origin(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Wildcard origin returns '*' as Access-Control-Allow-Origin."""
    _ = mock_upstream
    alias = unique_alias("cors-wild")
    cors_wildcard = {
        **CORS_CONFIG,
        "allowed_origins": ["*"],
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, cors=cors_wildcard,
        ))
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


# ---------------------------------------------------------------------------
# PLG-13: CORS moves out of OAGW
# ---------------------------------------------------------------------------

CORS_CONFIGS = [
    pytest.param(
        {"enabled": True, "allowed_origins": ["https://app.example.com"], "allowed_methods": ["GET"]},
        id="enabled",
    ),
    pytest.param({"enabled": False}, id="disabled"),
]


@pytest.mark.asyncio
@pytest.mark.parametrize("cors", CORS_CONFIGS)
@pytest.mark.xfail(strict=True, raises=AssertionError, reason="PLG-13: upstream `cors` is still accepted")
async def test_upstream_cors_field_rejected(
    cors, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """After PLG-13, an upstream carrying `cors` is rejected and nothing is stored."""
    _ = mock_upstream
    alias = unique_alias("cors-rejected")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias, cors=cors,
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        # PLG-13 promises only a 400; the error's shape is not decided.
        assert_problem(resp, 400, esrc=None)
        listed = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert alias not in [u["alias"] for u in listed]


@pytest.mark.asyncio
@pytest.mark.parametrize("cors", CORS_CONFIGS)
@pytest.mark.xfail(strict=True, raises=AssertionError, reason="PLG-13: route `cors` is still accepted")
async def test_route_cors_field_rejected(
    cors, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """After PLG-13, a route carrying `cors` is rejected too."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=unique_alias("cors-rt"),
        ))
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/routes",
            headers=oagw_headers,
            json={
                "upstream_id": upstream["id"],
                "match": {"http": {"methods": ["GET"], "path": "/v1/models"}},
                "enabled": True,
                "tags": [],
                "priority": 0,
                "cors": cors,
            },
        )
        assert_problem(resp, 400, esrc=None)
