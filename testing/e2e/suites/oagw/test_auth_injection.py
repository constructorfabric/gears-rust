"""E2E tests for OAGW auth injection (API key plugin)."""
import uuid

import httpx
import pytest

from .helpers import (
    APIKEY_AUTH_PLUGIN_ID,
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
    assert_gateway_problem_body,
    assert_problem,
    create_route,
    create_upstream,
    create_upstream_raw,
    credential_not_found,
    traced,
    unique_alias,
)


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


@pytest.mark.asyncio
async def test_missing_secret_ref_is_not_echoed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A `secret_ref` that does not resolve is a 400 `failed_precondition`
    whose body does not repeat the reference (#4989, F0008).
    """
    _ = mock_upstream
    secret_ref = f"cred://{unique_alias('e2e-missing')}"
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("auth-missing"),
            auth={
                "type": APIKEY_AUTH_PLUGIN_ID,
                "sharing": "private",
                "config": {"header": "authorization", "secret_ref": secret_ref},
            },
        )
        if resp.status_code < 300:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem(resp, 400, esrc=None, category="failed_precondition")
        bare = secret_ref.removeprefix("cred://")
        assert bare not in resp.text, resp.text[:500]


_APIKEY_OPENAI = {
    "type": APIKEY_AUTH_PLUGIN_ID,
    "sharing": "private",
    "config": {
        "header": "authorization",
        "prefix": "Bearer ",
        "secret_ref": "cred://openai-key",
    },
}


@pytest.mark.scenario("positive-7.2-hop-hop-headers-stripped", part="C")
@pytest.mark.asyncio
async def test_apikey_auth_keeps_repeated_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 7.2-C: the auth plugin replaces only its own header; a header
    the client repeats reaches the upstream once per line (#4982).
    """
    _ = mock_upstream
    alias = unique_alias("auth-multi")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, auth=_APIKEY_OPENAI,
            upstream_headers={"request": {"passthrough": "all"}},
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo-lines",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-lines",
            headers=[
                *oagw_headers.items(),
                ("x-multi", "a"),
                ("x-multi", "b"),
            ],
        )
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        lines = resp.json()["header_lines"]
        assert [v for k, v in lines if k == "x-multi"] == ["a", "b"], lines
        assert [v for k, v in lines if k == "authorization"] == [
            "Bearer sk-test-e2e-fake-key",
        ], lines


@pytest.mark.scenario("positive-7.2-hop-hop-headers-stripped", part="C")
@pytest.mark.asyncio
async def test_apikey_auth_keeps_headers_through_transform(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 7.2-C with the request_id transform bound: the transform adds
    `x-request-id` and changes nothing else. The credential, an obs-text
    value the transform cannot see as a string, still reaches the upstream,
    and the repeated client header keeps both lines.
    """
    _ = mock_upstream
    alias = unique_alias("auth-xform")
    ref = f"e2e-apikey-obs-{uuid.uuid4().hex[:8]}"
    secrets_url = f"{oagw_base_url}/credstore/v1/secrets"
    async with httpx.AsyncClient(timeout=10.0) as client:
        created = await client.post(
            secrets_url, headers=oagw_headers,
            json={"reference": ref, "value": "sk-café", "sharing": "tenant"},
        )
        if created.status_code not in (200, 201):
            pytest.fail(f"could not create secret {ref!r}: HTTP {created.status_code}")
        try:
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias,
                auth={
                    "type": APIKEY_AUTH_PLUGIN_ID,
                    "sharing": "private",
                    "config": {
                        "header": "authorization",
                        "prefix": "Bearer ",
                        "secret_ref": f"cred://{ref}",
                    },
                },
                upstream_headers={"request": {"passthrough": "all"}},
                plugins={
                    "sharing": "private",
                    "items": [{"plugin_ref": REQUEST_ID_TRANSFORM_PLUGIN_ID, "config": {}}],
                },
            ))
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo-lines",
            )

            resp = await client.post(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-lines",
                headers=[
                    *oagw_headers.items(),
                    ("x-multi", "a"),
                    ("x-multi", "b"),
                ],
            )
        finally:
            await client.delete(
                f"{secrets_url}/{ref}", headers={**oagw_headers, "If-Match": "*"},
            )
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        lines = resp.json()["header_lines"]
        # The mock decodes header bytes as UTF-8.
        assert [v for k, v in lines if k == "authorization"] == ["Bearer sk-café"], lines
        assert [v for k, v in lines if k == "x-multi"] == ["a", "b"], lines
        assert len([v for k, v in lines if k == "x-request-id"]) == 1, lines


@pytest.mark.scenario("negative-9.7-secret-access-control-cred-store", part="B")
@pytest.mark.asyncio
async def test_apikey_secret_deleted_does_not_reveal_the_ref(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 9.7-B at proxy time: the secret existed at write time, then
    was deleted. The credential is missing: 401 with reason
    `AUTH_CREDENTIAL_NOT_FOUND` (it was 500), and the body contains neither
    the reference (#4989) nor the config key. That the server log names the
    key and not the reference is covered by Rust tests: E2E does not see the
    server log.
    """
    _ = mock_upstream
    alias = unique_alias("auth-gone")
    ref = f"e2e-apikey-gone-{uuid.uuid4().hex[:8]}"
    secrets_url = f"{oagw_base_url}/credstore/v1/secrets"
    async with httpx.AsyncClient(timeout=10.0) as client:
        created = await client.post(
            secrets_url, headers=oagw_headers,
            json={"reference": ref, "value": "sk-gone", "sharing": "tenant"},
        )
        if created.status_code not in (200, 201):
            pytest.fail(f"could not create secret {ref!r}: HTTP {created.status_code}")
        try:
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
                auth={
                    "type": APIKEY_AUTH_PLUGIN_ID,
                    "sharing": "private",
                    "config": {"header": "authorization", "secret_ref": f"cred://{ref}"},
                },
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

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**traced(oagw_headers), "content-type": "application/json"},
            json={"test": True},
        )
        assert_gateway_problem_body(resp, credential_not_found(f"/oagw/v1/proxy/{alias}/echo"))
        assert ref not in resp.text, resp.text[:500]
        assert "secret_ref" not in resp.text, resp.text[:500]
