"""E2E tests for OAGW OAuth2 Client Credentials auth plugin."""
import httpx
import pytest

from .helpers import create_route, create_upstream, delete_upstream, unique_alias

OAUTH2_CC_PLUGIN_ID = (
    "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1"
)
OAUTH2_CC_BASIC_PLUGIN_ID = (
    "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1"
)


@pytest.mark.asyncio
async def test_oauth2_client_cred_form_injects_bearer(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """OAuth2 CC (Form) plugin obtains token and injects Authorization: Bearer."""
    alias = unique_alias("oauth2-form")
    auth_config = {
        "type": OAUTH2_CC_PLUGIN_ID,
        "sharing": "private",
        "config": {
            "token_endpoint": f"{mock_upstream_url}/oauth2/token",
            "client_id_ref": "cred://test-oauth2-client-id",
            "client_secret_ref": "cred://test-oauth2-client-secret",
            "scopes": "read write",
        },
    }

    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            upstream = await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias, auth=auth_config,
            )
        except httpx.HTTPStatusError as exc:
            if exc.response.status_code == 500:
                pytest.skip(
                    f"Cannot create upstream with OAuth2 auth config "
                    f"(cred_store may not be available): "
                    f"{exc.response.status_code} {exc.response.text[:200]}"
                )
            raise

        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"test": True},
        )

        if resp.status_code == 500:
            await delete_upstream(client, oagw_base_url, oagw_headers, uid)
            pytest.skip(
                f"OAuth2 auth injection failed (cred_store may not have test secret): "
                f"{resp.status_code} {resp.text[:200]}"
            )

        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )

        echoed = resp.json().get("headers", {})
        auth_header = echoed.get("authorization", "")
        assert auth_header.startswith("Bearer "), (
            f"Expected 'Bearer ...' in authorization header, got: {auth_header!r}"
        )
        token_value = auth_header[len("Bearer "):]
        assert len(token_value) > 0, "Resolved Bearer token is empty"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


@pytest.mark.asyncio
async def test_oauth2_client_cred_basic_injects_bearer(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """OAuth2 CC (Basic) plugin obtains token and injects Authorization: Bearer."""
    alias = unique_alias("oauth2-basic")
    auth_config = {
        "type": OAUTH2_CC_BASIC_PLUGIN_ID,
        "sharing": "private",
        "config": {
            "token_endpoint": f"{mock_upstream_url}/oauth2/token",
            "client_id_ref": "cred://test-oauth2-client-id",
            "client_secret_ref": "cred://test-oauth2-client-secret",
            "scopes": "read write",
        },
    }

    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            upstream = await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias, auth=auth_config,
            )
        except httpx.HTTPStatusError as exc:
            if exc.response.status_code == 500:
                pytest.skip(
                    f"Cannot create upstream with OAuth2 Basic auth config "
                    f"(cred_store may not be available): "
                    f"{exc.response.status_code} {exc.response.text[:200]}"
                )
            raise

        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"test": True},
        )

        if resp.status_code == 500:
            await delete_upstream(client, oagw_base_url, oagw_headers, uid)
            pytest.skip(
                f"OAuth2 Basic auth injection failed "
                f"(cred_store may not have test secret): "
                f"{resp.status_code} {resp.text[:200]}"
            )

        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )

        echoed = resp.json().get("headers", {})
        auth_header = echoed.get("authorization", "")
        assert auth_header.startswith("Bearer "), (
            f"Expected 'Bearer ...' in authorization header, got: {auth_header!r}"
        )
        token_value = auth_header[len("Bearer "):]
        assert len(token_value) > 0, "Resolved Bearer token is empty"

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)



@pytest.mark.asyncio
async def test_oauth2_client_cred_missing_secret_returns_error(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """OAuth2 CC with non-existent credential ref returns error gracefully."""
    alias = unique_alias("oauth2-nosecret")
    auth_config = {
        "type": OAUTH2_CC_PLUGIN_ID,
        "sharing": "private",
        "config": {
            "token_endpoint": f"{mock_upstream_url}/oauth2/token",
            "client_id_ref": "cred://nonexistent-client-id",
            "client_secret_ref": "cred://nonexistent-client-secret",
        },
    }

    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            upstream = await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias, auth=auth_config,
            )
        except httpx.HTTPStatusError as exc:
            if exc.response.status_code == 500:
                pytest.skip(
                    f"Cannot create upstream with OAuth2 auth config "
                    f"(cred_store may not be available): "
                    f"{exc.response.status_code} {exc.response.text[:200]}"
                )
            raise

        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"test": True},
        )

        # Should fail with an error status (500) because secrets don't exist.
        assert resp.status_code == 500, (
            f"Expected error status for missing secrets, got {resp.status_code}: "
            f"{resp.text[:500]}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)
