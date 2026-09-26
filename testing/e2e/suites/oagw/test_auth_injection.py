"""E2E tests for OAGW auth injection (API key plugin)."""
import httpx
import pytest

from .helpers import APIKEY_AUTH_PLUGIN_ID, create_route, create_upstream, unique_alias


@pytest.mark.scenario("positive-9.2-api-key-injection")
@pytest.mark.asyncio
async def test_apikey_auth_injects_bearer_header(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 9.2: the apikey plugin injects `<prefix><resolved secret>`.

    The secret is provisioned by conftest (a failure there errors every test),
    so any non-200 here is a plugin or pipeline failure, never a skip.
    """
    alias = unique_alias("auth-key")
    auth_config = {
        "type": APIKEY_AUTH_PLUGIN_ID,
        "sharing": "private",
        "config": {
            "header": "authorization",
            "prefix": "Bearer ",
            "secret_ref": "cred://openai-key",
        },
    }

    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, auth=auth_config,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"test": True},
        )
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        assert resp.headers.get("x-oagw-error-source") == "upstream"

        echoed = resp.json()["headers"]
        # The client's own gateway token must be replaced, not forwarded.
        assert echoed.get("authorization") == "Bearer sk-test-e2e-fake-key"
