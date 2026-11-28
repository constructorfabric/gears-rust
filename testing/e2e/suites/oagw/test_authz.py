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

# `DenyReason::INVALID_REQUEST` (authz-resolver-sdk).
INVALID_REQUEST = "gts.cf.core.errors.err.v1~x.authz.errors.invalid_request.v1"


@pytest.mark.asyncio
async def test_proxy_authz_forbidden_nil_tenant(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """OAGW's proxy PEP check turns an authz deny into a 403 Problem Details.

    The deny decision itself is static-authz's nil-tenant special case, not a
    missing proxy-invoke permission, so this covers OAGW's enforcement and
    error mapping rather than scenario 5.1. The tenant comes from the token.
    static-authz denies a nil tenant with the ``invalid_request`` error code,
    and OAGW passes the PDP's code through as the problem's ``reason``.
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
                reason=INVALID_REQUEST,
                resource_type=PROXY_SCHEMA,
            )
            assert body["title"] == "Permission Denied"
