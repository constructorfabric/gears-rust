"""E2E tests for OAGW error handling (gateway vs upstream error source)."""
import time

import httpx
import pytest

from .helpers import (
    UPSTREAM_SCHEMA,
    assert_problem,
    create_route,
    create_upstream,
    unique_alias,
    update_upstream,
)


@pytest.mark.scenario("negative-6.4-alias-not-found-returns-stable-404")
@pytest.mark.asyncio
async def test_nonexistent_alias_returns_404_gateway(
    oagw_base_url, oagw_headers, mock_upstream,
):
    """Scenario 6.4: an unknown alias is a gateway 404 that names the upstream."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/nonexistent-alias-xyz-{unique_alias()}/v1/test",
            headers=oagw_headers,
        )
        # resource_type tells this apart from route-not-found on a known alias.
        assert_problem(
            resp, 404,
            category="not_found",
            resource_type=UPSTREAM_SCHEMA,
            detail_contains="upstream not found",
        )


@pytest.mark.scenario("negative-2.6-disable-upstream-blocks-proxy-traffic")
@pytest.mark.asyncio
async def test_disabled_upstream_returns_503_gateway(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.6: disabling a working upstream blocks proxy traffic with 503.

    `Retry-After: 30` (the administrative delay) separates this from the
    transient 503 an unreachable endpoint gets (`Retry-After: 5`).
    """
    _ = mock_upstream
    alias = unique_alias("err-disabled")
    url = f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models"
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1/models",
        )
        resp = await client.get(url, headers=oagw_headers)
        assert resp.status_code == 200, resp.text[:300]

        await update_upstream(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, enabled=False,
        )
        resp = await client.get(url, headers=oagw_headers)
        assert_problem(resp, 503, category="service_unavailable")
        assert resp.headers.get("retry-after") == "30"


@pytest.mark.scenario("negative-2.6-disable-upstream-blocks-proxy-traffic")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="M-12: a disabled descendant upstream falls through to the "
           "ancestor's upstream (200) instead of returning 503",
)
async def test_disabled_descendant_upstream_does_not_fall_through(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.6 (descendant): the nearest upstream wins even when disabled."""
    _ = mock_upstream
    alias = unique_alias("err-disabled-child")
    async with httpx.AsyncClient(timeout=10.0) as client:
        # An inheritable field makes the root's upstream visible to l1a; a
        # generous limit keeps it out of the way.
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            rate_limit={
                "algorithm": "token_bucket",
                "sustained": {"rate": 1000, "window": "minute"},
                "burst": {"capacity": 1000},
                "scope": "tenant",
                "strategy": "reject",
                "sharing": "inherit",
            },
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["GET"], "/v1/models",
        )
        url = f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models"
        resp = await client.get(url, headers=hierarchy_l1a_headers)
        if resp.status_code != 200:
            # Precondition, not the behaviour under test: fail outright
            # rather than count as the expected (M-12) failure.
            pytest.fail(f"l1a must reach the root upstream first, got {resp.status_code}")

        cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
            alias=alias, enabled=False,
        ))

        resp = await client.get(
            url, headers=hierarchy_l1a_headers,
        )
        assert_problem(resp, 503, category="service_unavailable")


@pytest.mark.scenario("negative-12.2-upstream-error-passthrough-esrc-upstream")
@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["GET", "POST"])
@pytest.mark.parametrize("code", [400, 404, 429, 500, 503])
async def test_upstream_error_passthrough(
    code, method, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 12.2: an upstream error keeps its status, content type and body."""
    _ = mock_upstream
    alias = unique_alias("err-pass")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], [method], "/error",
        )

        resp = await client.request(
            method, f"{oagw_base_url}/oagw/v1/proxy/{alias}/error/{code}",
            headers=oagw_headers,
        )
        assert resp.status_code == code
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        assert resp.headers.get("content-type") == "application/json"
        assert resp.json() == {
            "error": {
                "message": f"Simulated error {code}",
                "type": "server_error",
                "code": f"error_{code}",
            },
        }


@pytest.mark.asyncio
async def test_upstream_timeout_returns_504_gateway(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """OAGW's own proxy timeout answers a stalled upstream with a gateway 504.

    The e2e config sets `proxy_timeout_secs: 2`; the mock sleeps 30 s. The
    elapsed-time window tells OAGW's timer apart from the api-gateway's 30 s
    timeout layer, which would also produce a 504 but without the error source.
    """
    _ = mock_upstream
    alias = unique_alias("err-timeout")
    async with httpx.AsyncClient(timeout=8.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/error",
        )

        started = time.monotonic()
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/error/timeout",
            headers=oagw_headers,
        )
        elapsed = time.monotonic() - started

        assert_problem(resp, 504, category="deadline_exceeded")
        assert 1.5 <= elapsed < 5, f"504 after {elapsed:.2f}s, expected OAGW's 2 s timer"
