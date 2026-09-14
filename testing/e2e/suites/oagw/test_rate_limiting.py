"""E2E tests for OAGW rate limiting — token bucket, sliding window, scoping."""
import httpx
import pytest

from .helpers import create_route, create_upstream, delete_upstream, unique_alias


@pytest.mark.asyncio
async def test_rate_limit_first_request_succeeds(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """First request within rate limit succeeds."""
    alias = unique_alias("rl-ok")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 1, "window": "minute"},
        "burst": {"capacity": 1},
        "scope": "tenant",
        "strategy": "reject",
        "cost": 1,
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, rate_limit=rate_limit,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
        )

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_rate_limit_exceeded_returns_429(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Second request exceeding burst returns 429 with Retry-After."""
    alias = unique_alias("rl-429")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 1, "window": "minute"},
        "burst": {"capacity": 1},
        "scope": "tenant",
        "strategy": "reject",
        "cost": 1,
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, rate_limit=rate_limit,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
        )

        # First request consumes the single token.
        resp1 = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp1.status_code == 200

        # Second request should be rate-limited.
        resp2 = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp2.status_code == 429, (
            f"Expected 429 on second request, got {resp2.status_code}: {resp2.text[:500]}"
        )
        assert resp2.headers.get("x-oagw-error-source") == "gateway"
        assert "retry-after" in resp2.headers, "Missing Retry-After header on 429"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_token_bucket_burst_capacity_and_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 18.1: burst allows requests up to capacity; response includes
    X-RateLimit-* headers; 11th request returns 429 with Retry-After."""
    alias = unique_alias("rl-burst")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 5, "window": "hour"},
        "burst": {"capacity": 10},
        "scope": "tenant",
        "strategy": "reject",
        "response_headers": True,
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, rate_limit=rate_limit,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
        )

        # Send 10 requests — all should succeed (burst capacity = 10).
        for i in range(10):
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=oagw_headers,
            )
            assert resp.status_code == 200, (
                f"Request {i+1}/10 should succeed, got {resp.status_code}"
            )

        # Verify rate limit headers on the last success.
        assert "x-ratelimit-limit" in resp.headers, "Missing X-RateLimit-Limit"
        assert "x-ratelimit-remaining" in resp.headers, "Missing X-RateLimit-Remaining"
        assert "x-ratelimit-reset" in resp.headers, "Missing X-RateLimit-Reset"

        # 11th request — should be rate limited.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 429, (
            f"Expected 429 on 11th request, got {resp.status_code}"
        )
        assert "retry-after" in resp.headers, "Missing Retry-After on 429"
        assert resp.headers.get("x-oagw-error-source") == "gateway"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_response_headers_disabled(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 18.1.1: response_headers=false suppresses X-RateLimit-* on
    success but Retry-After is still sent on 429."""
    alias = unique_alias("rl-nohdr")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 1, "window": "minute"},
        "burst": {"capacity": 1},
        "scope": "tenant",
        "strategy": "reject",
        "response_headers": False,
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, rate_limit=rate_limit,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
        )

        # First request succeeds — no X-RateLimit-* headers.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200
        assert "x-ratelimit-limit" not in resp.headers, (
            "X-RateLimit-Limit should be absent when response_headers=false"
        )
        assert "x-ratelimit-remaining" not in resp.headers
        assert "x-ratelimit-reset" not in resp.headers

        # Second request — 429 with Retry-After (always present).
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 429
        assert "retry-after" in resp.headers, (
            "Retry-After must be present on 429 even with response_headers=false"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_sliding_window_basic_enforcement(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 18.2 (adapted): sliding window algorithm enforces rate limit.
    Uses minute-scale window to avoid sub-second timing sensitivity."""
    alias = unique_alias("rl-sw")
    rate_limit = {
        "algorithm": "sliding_window",
        "sustained": {"rate": 2, "window": "minute"},
        "scope": "tenant",
        "strategy": "reject",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, rate_limit=rate_limit,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
        )

        # First two requests succeed.
        for i in range(2):
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=oagw_headers,
            )
            assert resp.status_code == 200, (
                f"Request {i+1}/2 should succeed, got {resp.status_code}"
            )

        # Third request — rejected by sliding window.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 429, (
            f"Expected 429 on 3rd request, got {resp.status_code}"
        )
        assert resp.headers.get("x-oagw-error-source") == "gateway"
        assert "retry-after" in resp.headers

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_scope_global(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 18.3: scope=global is accepted and enforces a shared counter.

    Global scope means the counter key omits tenant/user/IP, so all requests
    to this upstream share one bucket regardless of caller identity.
    Proved by exhausting the bucket with requests from two different tenants.
    """
    alias = unique_alias("rl-global")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 2, "window": "minute"},
        "burst": {"capacity": 2},
        "scope": "global",
        "strategy": "reject",
        "sharing": "inherit",
        "budget": {"mode": "shared", "total": 2},
    }
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            # Parent upstream with global-scoped rate limit.
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias, rate_limit=rate_limit,
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # Children bind to the same alias (inherit parent rate limit).
            child_a = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
            )
            uids.append((hierarchy_l1a_headers, child_a["id"]))

            child_b = await create_upstream(
                client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url,
                alias=alias,
            )
            uids.append((hierarchy_l1b_headers, child_b["id"]))

            await create_route(
                client, oagw_base_url, hierarchy_root_headers,
                parent["id"], ["GET"], "/v1/models",
            )

            # Tenant l1a: first request succeeds (1 of 2 tokens consumed).
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=hierarchy_l1a_headers,
            )
            assert resp.status_code == 200

            # Tenant l1b: request succeeds (2 of 2 tokens consumed).
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=hierarchy_l1b_headers,
            )
            assert resp.status_code == 200

            # Tenant l1a again: 429 — proves the global bucket is shared
            # across tenants, not isolated per-tenant.
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=hierarchy_l1a_headers,
            )
            assert resp.status_code == 429, (
                f"Global scope should share one bucket across tenants, got {resp.status_code}"
            )
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_scope_route_isolation(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 18.3: scope=route gives each route its own rate-limit bucket."""
    alias = unique_alias("rl-route")
    route_rl = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 1, "window": "minute"},
        "burst": {"capacity": 1},
        "scope": "route",
        "strategy": "reject",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        # Upstream without rate limit; rate limit lives on each route.
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/v1/models", rate_limit=route_rl,
        )
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/health", rate_limit=route_rl,
        )

        # Route A — allowed (own bucket).
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200

        # Route B — allowed (separate bucket).
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/health",
            headers=oagw_headers,
        )
        assert resp.status_code == 200

        # Route A again — rejected (exhausted).
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 429, "Route A should be rate-limited"

        # Route B again — rejected (exhausted).
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/health",
            headers=oagw_headers,
        )
        assert resp.status_code == 429, "Route B should be rate-limited"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_scope_tenant_isolation(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream,
):
    """Tenant-scoped rate limits give each tenant an independent bucket."""
    alias = unique_alias("rl-tenant-iso")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 1, "window": "minute"},
        "burst": {"capacity": 1},
        "scope": "tenant",
        "strategy": "reject",
        "sharing": "inherit",
    }
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            # Parent upstream with inheritable rate limit.
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias, rate_limit=rate_limit,
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # Children bind to the same alias (inherit parent rate limit).
            child_a = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
            )
            uids.append((hierarchy_l1a_headers, child_a["id"]))

            child_b = await create_upstream(
                client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url,
                alias=alias,
            )
            uids.append((hierarchy_l1b_headers, child_b["id"]))

            await create_route(
                client, oagw_base_url, hierarchy_root_headers,
                parent["id"], ["GET"], "/v1/models",
            )

            # Tenant l1a: first request succeeds, second is rate-limited.
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=hierarchy_l1a_headers,
            )
            assert resp.status_code == 200

            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=hierarchy_l1a_headers,
            )
            assert resp.status_code == 429, (
                f"Tenant l1a should be rate-limited, got {resp.status_code}"
            )

            # Tenant l1b: first request still succeeds (independent bucket).
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=hierarchy_l1b_headers,
            )
            assert resp.status_code == 200, (
                f"Tenant l1b should have its own bucket, got {resp.status_code}"
            )
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_weighted_cost(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Scenario 18.4: cost=10 consumes entire 10-token budget in one request."""
    alias = unique_alias("rl-cost")
    rate_limit = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 10, "window": "minute"},
        "burst": {"capacity": 10},
        "scope": "tenant",
        "strategy": "reject",
        "cost": 10,
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, rate_limit=rate_limit,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
        )

        # First request consumes all 10 tokens.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200

        # Second request — no tokens left.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 429

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_route_level_rate_limit(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Rate limit configured on the route (not upstream) is enforced."""
    alias = unique_alias("rl-rtlvl")
    route_rl = {
        "algorithm": "token_bucket",
        "sustained": {"rate": 1, "window": "minute"},
        "burst": {"capacity": 1},
        "scope": "tenant",
        "strategy": "reject",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        # Upstream has no rate limit.
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid,
            ["GET"], "/v1/models", rate_limit=route_rl,
        )

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 429, "Route-level rate limit should enforce"
        assert resp.headers.get("x-oagw-error-source") == "gateway"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)
