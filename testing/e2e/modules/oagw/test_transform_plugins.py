"""E2E tests for OAGW transform plugins (request-id transform)."""
import re

import httpx
import pytest

from .helpers import (
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
    create_route,
    create_upstream,
    delete_upstream,
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


@pytest.mark.asyncio
async def test_request_id_transform_injects_header(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """RequestIdTransformPlugin injects a UUID x-request-id when none is present."""
    _ = mock_upstream
    alias = unique_alias("xform-rid-inj")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_request_id_plugins(),
        )
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
        assert UUID_RE.match(request_id), (
            f"Expected x-request-id to be a UUID, got: {request_id!r}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Test F: transform preserves existing x-request-id
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_request_id_transform_preserves_existing(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """RequestIdTransformPlugin preserves an existing x-request-id from the client."""
    _ = mock_upstream
    alias = unique_alias("xform-rid-keep")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins=_request_id_plugins(),
            upstream_headers={"request": {"passthrough": "all"}},
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
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

        echoed = resp.json().get("headers", {})
        request_id = echoed.get("x-request-id", "")
        assert request_id == custom_id, (
            f"Expected x-request-id to be preserved as {custom_id!r}, got: {request_id!r}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)


# ---------------------------------------------------------------------------
# Test G: unknown transform plugin does not block the pipeline
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_unknown_transform_does_not_block_pipeline(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """An unresolvable transform plugin is logged and skipped — the request succeeds."""
    _ = mock_upstream
    alias = unique_alias("xform-unknown")
    fake_plugin = "gts.x.core.oagw.transform_plugin.v1~x.core.oagw.nonexistent.v1"
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            plugins={
                "sharing": "private",
                "items": [
                    {"plugin_ref": fake_plugin, "config": {}},
                ],
            },
        )
        uid = upstream["id"]
        await create_route(
            client, oagw_base_url, oagw_headers, uid, ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json={"transform": "unknown-plugin"},
        )
        assert resp.status_code == 200, (
            f"Expected 200 (unknown transform should be skipped), "
            f"got {resp.status_code}: {resp.text[:500]}"
        )

        await delete_upstream(client, oagw_base_url, oagw_headers, uid)
