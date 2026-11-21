"""E2E tests for OAGW OAuth2 Client Credentials auth plugin.

The mock `/oauth2/token` endpoint issues `mock-e2e-token-form` or
`mock-e2e-token-basic` depending on how the client authenticated, and
rejects wrong credentials, so each test pins which variant ran.
"""
import uuid

import httpx
import pytest

from .helpers import (
    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
    UPSTREAM_SCHEMA,
    assert_gateway_problem_body,
    assert_problem,
    assert_problem_body,
    create_route,
    create_upstream,
    create_upstream_raw,
    credential_not_found,
    err_type,
    service_unavailable,
    traced,
    unique_alias,
)


def _oauth2_auth(
    plugin_id: str, mock_upstream_url: str, token_path: str = "/oauth2/token", **refs,
) -> dict:
    return {
        "type": plugin_id,
        "sharing": "private",
        "config": {
            "token_endpoint": f"{mock_upstream_url}{token_path}",
            "client_id_ref": refs.get("client_id_ref", "cred://test-oauth2-client-id"),
            "client_secret_ref": refs.get("client_secret_ref", "cred://test-oauth2-client-secret"),
            # A per-test scope makes the token cache key unique, so every run
            # really calls the token endpoint.
            "scopes": f"read write e2e-{uuid.uuid4().hex[:8]}",
        },
    }


async def _proxy_echo(client, oagw_base_url, oagw_headers, alias):
    return await client.post(
        f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
        headers={**oagw_headers, "content-type": "application/json"},
        json={"test": True},
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("plugin_id", "expected_token"),
    [
        # Client credentials sent as form parameters.
        pytest.param(
            OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, "mock-e2e-token-form", id="form",
            marks=pytest.mark.scenario("positive-9.5-oauth2-client-credentials"),
        ),
        # Client authenticated with HTTP Basic, not form params.
        pytest.param(
            OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID, "mock-e2e-token-basic", id="basic",
            marks=pytest.mark.scenario("positive-9.6-oauth2-client-credentials"),
        ),
    ],
)
async def test_oauth2_client_cred_injects_bearer(
    plugin_id, expected_token,
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """The plugin fetches a token with the right client auth and injects it."""
    alias = unique_alias("oauth2")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, auth=_oauth2_auth(plugin_id, mock_upstream_url),
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await _proxy_echo(client, oagw_base_url, oagw_headers, alias)
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        assert resp.json()["headers"].get("authorization") == f"Bearer {expected_token}"


@pytest.mark.scenario("negative-9.7-secret-access-control-cred-store", part="B")
@pytest.mark.asyncio
async def test_oauth2_client_cred_missing_secret_rejected_at_write(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 9.7-B: an upstream whose OAuth2 refs don't resolve is rejected on create."""
    alias = unique_alias("oauth2-nosecret")
    auth = _oauth2_auth(
        OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, mock_upstream_url,
        client_id_ref="cred://nonexistent-client-id",
        client_secret_ref="cred://nonexistent-client-secret",
    )
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url, alias=alias, auth=auth,
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        # The refs are checked in key order; the violation names the first one
        # and does not quote it.
        assert_problem_body(resp, {
            "type": err_type("failed_precondition"),
            "title": "Failed Precondition",
            "status": 400,
            "detail": "Operation precondition not met",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "violations": [{
                    "type": "STATE",
                    "subject": "auth.config.client_id_ref",
                    "description": (
                        "credential reference is not accessible to this tenant "
                        "(not provisioned yet, or not shared)"
                    ),
                }],
                "resource_type": UPSTREAM_SCHEMA,
            },
        })


@pytest.mark.scenario("negative-9.7-secret-access-control-cred-store", part="B")
@pytest.mark.asyncio
async def test_oauth2_client_cred_secret_deleted_returns_401(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 9.7-B at proxy time: the secret existed at write time, then was
    deleted. 401 `AUTH_CREDENTIAL_NOT_FOUND`, as for the API-key plugin: one
    mapping for every auth plugin."""
    alias = unique_alias("oauth2-gone")
    ref = f"e2e-oauth2-gone-{uuid.uuid4().hex[:8]}"
    secrets_url = f"{oagw_base_url}/credstore/v1/secrets"
    async with httpx.AsyncClient(timeout=10.0) as client:
        created = await client.post(
            secrets_url, headers=oagw_headers,
            json={"reference": ref, "value": "test-client-secret", "sharing": "tenant"},
        )
        if created.status_code not in (200, 201):
            pytest.fail(f"could not create secret {ref!r}: HTTP {created.status_code}")
        try:
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
                auth=_oauth2_auth(
                    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, mock_upstream_url,
                    client_secret_ref=f"cred://{ref}",
                ),
            ))
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
            )
        finally:
            deleted = await client.delete(
                f"{secrets_url}/{ref}", headers={**oagw_headers, "If-Match": "*"},
            )
        if deleted.status_code != 204:
            pytest.fail(f"could not delete secret {ref!r}: HTTP {deleted.status_code}")

        resp = await _proxy_echo(client, oagw_base_url, traced(oagw_headers), alias)
        assert_gateway_problem_body(resp, credential_not_found(f"/oagw/v1/proxy/{alias}/echo"))
        assert ref not in resp.text, resp.text[:500]


@pytest.mark.scenario("negative-9.11-oauth2-identity-provider-unavailable")
@pytest.mark.asyncio
async def test_oauth2_identity_provider_unavailable_returns_503(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """The token endpoint answers 503: 503 `IDENTITY_PROVIDER_UNAVAILABLE`
    with the transient retry hint, not the credential store's reason. The
    upstream is not called."""
    alias = unique_alias("oauth2-idp-down")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            auth=_oauth2_auth(
                OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, mock_upstream_url,
                token_path="/oauth2/token-unavailable",
            ),
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await _proxy_echo(client, oagw_base_url, traced(oagw_headers), alias)
        assert_gateway_problem_body(resp, service_unavailable(
            503, f"/oagw/v1/proxy/{alias}/echo", 5, "IDENTITY_PROVIDER_UNAVAILABLE",
        ))
        assert resp.headers.get("retry-after") == "5", resp.headers
