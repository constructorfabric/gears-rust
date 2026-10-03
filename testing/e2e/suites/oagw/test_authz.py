"""E2E tests for OAGW proxy authorization enforcement."""
import os

import httpx
import pytest

from .helpers import (
    PROXY_SCHEMA,
    assert_problem,
    create_route,
    create_upstream,
    unique_alias,
)


@pytest.mark.asyncio
async def test_proxy_authz_allowed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Regression: valid tenant passes authz and proxy request succeeds."""
    _ = mock_upstream
    alias = unique_alias("authz-allow")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1/models",
        )

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200
        assert resp.headers.get("x-oagw-error-source") == "upstream"


@pytest.mark.asyncio
async def test_proxy_authz_forbidden_nil_tenant(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """OAGW's proxy PEP check turns an authz deny into a 403 Problem Details.

    The deny decision itself is static-authz's nil-tenant special case, not a
    missing proxy-invoke permission, so this covers OAGW's enforcement and
    error mapping rather than scenario 5.1. The tenant comes from the token.
    """
    _ = mock_upstream
    alias = unique_alias("authz-deny")
    nil_tenant_token = os.getenv("E2E_AUTH_TOKEN_NIL_TENANT", "e2e-token-nil-tenant")
    denied_headers = {"Authorization": f"Bearer {nil_tenant_token}"}
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1/models",
        )

        # The check runs before upstream resolution, so an alias that does
        # not exist is denied identically.
        for target in (alias, unique_alias("authz-missing")):
            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{target}/v1/models",
                headers=denied_headers,
            )
            body = assert_problem(
                resp, 403,
                category="permission_denied",
                reason="AUTHZ_DENIED",
                resource_type=PROXY_SCHEMA,
            )
            assert body["title"] == "Permission Denied"
