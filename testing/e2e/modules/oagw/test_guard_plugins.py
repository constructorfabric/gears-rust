"""E2E tests for OAGW guard plugins (required headers guard)."""
import httpx
import pytest

from .helpers import (
    REQUIRED_HEADERS_GUARD_PLUGIN_ID,
    create_route,
    create_upstream,
    delete_upstream,
    unique_alias,
)


def _required_headers_plugins(request_headers=None, response_headers=None):
    """Build a plugins payload with the RequiredHeadersGuardPlugin bound."""
    config = {}
    if request_headers:
        config["required_request_headers"] = ",".join(request_headers)
    if response_headers:
        config["required_response_headers"] = ",".join(response_headers)
    return {
        "sharing": "private",
        "items": [
            {
                "plugin_ref": REQUIRED_HEADERS_GUARD_PLUGIN_ID,
                "config": config,
            },
        ],
    }


# ---------------------------------------------------------------------------
# Test A: required header present → allowed
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_required_headers_allows_when_present(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Request with required header present passes the guard."""
    _ = mock_upstream
    alias = unique_alias("guard-hdr-ok")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_required_headers_plugins(request_headers=["x-correlation-id"]),
            upstream_headers={"request": {"passthrough": "all"}},
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
                "x-correlation-id": "test-123",
            },
            json={"guard": "headers-test"},
        )
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )
        body = resp.json()
        assert "headers" in body, "Expected echoed headers from /echo"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Test B: required header missing → rejected
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_required_headers_rejects_when_missing(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Request without required header is rejected (400)."""
    _ = mock_upstream
    alias = unique_alias("guard-hdr-miss")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_required_headers_plugins(request_headers=["x-correlation-id"]),
            upstream_headers={"request": {"passthrough": "all"}},
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        # Send WITHOUT x-correlation-id.
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"guard": "headers-missing"},
        )
        assert resp.status_code == 400, (
            f"Expected 400, got {resp.status_code}: {resp.text[:500]}"
        )
        assert resp.headers.get("x-oagw-error-source") == "gateway"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Test C: unconfigured guard allows all
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_required_headers_allows_unconfigured(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Guard with empty config (no required headers) allows all requests."""
    _ = mock_upstream
    alias = unique_alias("guard-hdr-open")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_required_headers_plugins(),
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"guard": "unconfigured"},
        )
        assert resp.status_code == 200, (
            f"Expected 200 (unconfigured = fail-open), got {resp.status_code}: {resp.text[:500]}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Test D: case-insensitive header matching
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_required_headers_case_insensitive(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Required header check is case-insensitive."""
    _ = mock_upstream
    alias = unique_alias("guard-hdr-case")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_required_headers_plugins(request_headers=["X-Correlation-ID"]),
            upstream_headers={"request": {"passthrough": "all"}},
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        # Send with lowercase header name (HTTP normalizes to lowercase).
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "x-correlation-id": "case-test",
            },
            json={"guard": "case-insensitive"},
        )
        assert resp.status_code == 200, (
            f"Expected 200 (case-insensitive match), got {resp.status_code}: {resp.text[:500]}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)
