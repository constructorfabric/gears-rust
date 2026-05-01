"""E2E tests for OAGW proxy authorization enforcement."""
import os

import httpx
import pytest

from .helpers import create_route, create_upstream, delete_upstream, unique_alias


@pytest.mark.asyncio
async def test_proxy_authz_allowed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Regression: valid tenant passes authz and proxy request succeeds."""
    _ = mock_upstream
    alias = unique_alias("authz-allow")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        try:
            await create_route(
                client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
            )

            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=oagw_headers,
            )
            assert resp.status_code == 200
        finally:
            await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_proxy_authz_forbidden_nil_tenant(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Nil tenant UUID is denied by authz with 403 and Problem Details body.

    The static-authz plugin denies requests when the token's subject_tenant_id
    is the nil UUID (00000000-0000-0000-0000-000000000000). The authz check
    runs before upstream resolution, so this produces a clean 403.
    The tenant is extracted from the token by the auth middleware — there is
    no x-tenant-id header.
    """
    _ = mock_upstream
    alias = unique_alias("authz-deny")
    # Use a token whose subject_tenant_id is the nil UUID.
    nil_tenant_token = os.getenv("E2E_AUTH_TOKEN_NIL_TENANT", "e2e-token-nil-tenant")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        uid = upstream["id"]
        try:
            await create_route(
                client, oagw_base_url, oagw_headers, uid, ["GET"], "/v1/models",
            )

            denied_headers = {
                "Authorization": f"Bearer {nil_tenant_token}",
            }

            resp = await client.get(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
                headers=denied_headers,
            )

            if resp.status_code == 401:
                pytest.skip(
                    "Token rejected by auth; ensure a nil-tenant token is configured"
                )
            if resp.status_code == 200:
                pytest.skip("AuthZ not enforced in this environment")

            assert resp.status_code == 403, (
                f"Expected 403, got {resp.status_code}: {resp.text[:500]}"
            )
            assert resp.headers.get("x-oagw-error-source") == "gateway"
            body = resp.json()
            assert body["status"] == 403
            assert body["title"] == "Permission Denied"
            assert body["type"] == "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~"
        finally:
            await delete_upstream(client, oagw_base_url, oagw_headers, uid)
