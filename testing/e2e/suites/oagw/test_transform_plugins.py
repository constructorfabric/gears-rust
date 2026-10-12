"""E2E tests for OAGW transform plugins (request-id transform)."""
import re

import httpx
import pytest

from .helpers import (
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
    assert_problem,
    create_route,
    create_upstream,
    create_upstream_raw,
    unique_alias,
)

UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    re.IGNORECASE,
)


def _request_id_plugins() -> dict:
    """Build a plugins payload with the RequestIdTransformPlugin bound."""
    return {
        "sharing": "private",
        "items": [
            {
                "plugin_ref": REQUEST_ID_TRANSFORM_PLUGIN_ID,
                "config": {},
            },
        ],
    }


# ---------------------------------------------------------------------------
# Test E: transform injects x-request-id when absent
# ---------------------------------------------------------------------------


@pytest.mark.scenario("positive-7.6-request-correlation-headers-propagate-end-end")
@pytest.mark.asyncio
async def test_request_id_transform_injects_header(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """RequestIdTransformPlugin injects a UUID x-request-id when none is present."""
    _ = mock_upstream
    alias = unique_alias("xform-rid-inj")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_request_id_plugins(),
        ))
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"transform": "test"},
        )
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )

        echoed = resp.json().get("headers", {})
        request_id = echoed.get("x-request-id", "")
        assert request_id, "Expected x-request-id header to be injected"
        # After P-11 the api-gateway's id (a nanoid, not a UUID) is forwarded
        # instead; this check then becomes "equals the response x-request-id".
        assert UUID_RE.match(request_id), (
            f"Expected x-request-id to be a UUID, got: {request_id!r}"
        )


# ---------------------------------------------------------------------------
# Test F: transform does not clobber an existing x-request-id
# ---------------------------------------------------------------------------


@pytest.mark.scenario("positive-7.6-request-correlation-headers-propagate-end-end")
@pytest.mark.asyncio
async def test_request_id_transform_does_not_clobber_existing_id(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """RequestIdTransformPlugin leaves a forwarded client x-request-id alone.

    `passthrough: all` is what forwards the client's id here; the plugin's
    part is only not overwriting it. The default-passthrough behaviour is
    `test_correlation_headers_forwarded_by_default` below.
    """
    _ = mock_upstream
    alias = unique_alias("xform-rid-keep")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_request_id_plugins(),
            upstream_headers={"request": {"passthrough": "all"}},
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        custom_id = "e2e-trace-abc123"
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "x-request-id": custom_id,
            },
            json={"transform": "preserve"},
        )
        assert resp.status_code == 200, (
            f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        )

        request_id = resp.json()["headers"].get("x-request-id", "")
        assert request_id == custom_id, (
            f"Expected x-request-id to be preserved as {custom_id!r}, got: {request_id!r}"
        )


@pytest.mark.scenario("positive-7.6-request-correlation-headers-propagate-end-end")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="P-11: under the default passthrough the client's x-request-id, "
           "traceparent, tracestate and accept never reach the upstream",
)
async def test_correlation_headers_forwarded_by_default(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 7.6: correlation and Accept headers propagate end to end with no config."""
    _ = mock_upstream
    alias = unique_alias("xform-corr")
    sent = {
        "x-request-id": "e2e-trace-def456",
        "traceparent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        "tracestate": "e2e=1",
        "accept": "application/json",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json", **sent},
            json={"transform": "correlation"},
        )
        assert resp.status_code == 200, resp.text[:500]
        echoed = resp.json()["headers"]
        assert {k: echoed.get(k) for k in sent} == sent
        # The id the client sees is the one the upstream saw.
        assert resp.headers.get("x-request-id") == sent["x-request-id"]


# ---------------------------------------------------------------------------
# Test G: unknown plugin ids fail closed
# ---------------------------------------------------------------------------

UNKNOWN_PLUGINS = [
    pytest.param("gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.nonexistent.v1", id="transform"),
    pytest.param("gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.nonexistent.v1", id="guard"),
]


def _plugins(plugin_ref: str) -> dict:
    return {"sharing": "private", "items": [{"plugin_ref": plugin_ref, "config": {}}]}


@pytest.mark.scenario("positive-4.5-plugin-resolution-supports-builtin-named-ids-custom-uuid", part="B")
@pytest.mark.asyncio
@pytest.mark.parametrize("plugin_ref", UNKNOWN_PLUGINS)
@pytest.mark.xfail(strict=True, raises=AssertionError, reason="M-11: unknown plugin ids are accepted on write")
async def test_unknown_plugin_rejected_at_write(
    plugin_ref, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 4.5-B: an upstream binding an unknown plugin id is rejected with 400."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("plugin-unknown-w"), plugins=_plugins(plugin_ref),
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem(resp, 400, esrc=None)


@pytest.mark.scenario("positive-4.5-plugin-resolution-supports-builtin-named-ids-custom-uuid", part="B")
@pytest.mark.asyncio
@pytest.mark.parametrize("plugin_ref", UNKNOWN_PLUGINS)
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="PLG-03: an unknown transform is skipped (200) and an unknown guard "
           "gives 500; the decision is 503 plugin_not_found",
)
async def test_unknown_plugin_fails_closed_at_proxy(
    plugin_ref, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 4.5-B at proxy time: a binding that doesn't resolve blocks the request.

    Once M-11 rejects these ids on write, the setup's create fails with an
    HTTP error, which `raises=AssertionError` turns into a real failure: the
    check must then move to a unit test of the proxy pipeline.
    """
    _ = mock_upstream
    alias = unique_alias("plugin-unknown-p")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, plugins=_plugins(plugin_ref),
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"transform": "unknown-plugin"},
        )
        assert_problem(resp, 503)
