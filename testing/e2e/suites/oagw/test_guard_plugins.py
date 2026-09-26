"""E2E tests for OAGW guard plugins (required headers guard).

Each allow-path test also sends a request without the header to the same
upstream and expects the guard's 400, which proves the guard is bound: a
missing binding would let both requests through.

Guards currently check the outbound header map, after passthrough filtering
and auth injection (PLG-05). Under `passthrough: all` the two maps agree, so
the `passthrough-all` variants pass today; the `default-passthrough` variants
are what a client sees with the default config and fail until PLG-05 lands.
"""
import httpx
import pytest

from .helpers import (
    REQUIRED_HEADERS_GUARD_PLUGIN_ID,
    assert_problem,
    create_route,
    create_upstream,
    unique_alias,
    update_upstream,
)

PLG_05 = pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="PLG-05: request guards see the outbound headers, so under the "
           "default passthrough a header the client sent is reported missing",
)
PASSTHROUGH = [
    pytest.param({"request": {"passthrough": "all"}}, id="passthrough-all"),
    pytest.param(None, id="default-passthrough", marks=PLG_05),
]


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


async def _guarded_echo(client, base, headers, mock_url, cleanup, prefix, required, upstream_headers):
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url,
        alias=alias,
        plugins=_required_headers_plugins(request_headers=required),
        upstream_headers=upstream_headers,
    ))
    await create_route(client, base, headers, upstream["id"], ["POST"], "/echo")
    return alias, upstream


async def _post_echo(client, base, headers, alias, extra=None):
    return await client.post(
        f"{base}/oagw/v1/proxy/{alias}/echo",
        headers={**headers, "content-type": "application/json", **(extra or {})},
        json={"guard": "test"},
    )


def _assert_header_missing(resp):
    assert_problem(resp, 400, reason="REQUIRED_HEADER_MISSING")


@pytest.mark.asyncio
@pytest.mark.parametrize("upstream_headers", PASSTHROUGH)
async def test_required_headers_allows_when_present(
    upstream_headers, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A request carrying the required header passes; one without it is rejected."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _guarded_echo(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "guard-hdr-ok", ["x-correlation-id"], upstream_headers,
        )

        resp = await _post_echo(
            client, oagw_base_url, oagw_headers, alias, {"x-correlation-id": "test-123"},
        )
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        if upstream_headers is not None:
            assert resp.json()["headers"].get("x-correlation-id") == "test-123"

        _assert_header_missing(await _post_echo(client, oagw_base_url, oagw_headers, alias))


@pytest.mark.asyncio
async def test_required_headers_rejects_when_missing(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Request without the required header is rejected with 400 REQUIRED_HEADER_MISSING."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _guarded_echo(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "guard-hdr-miss", ["x-correlation-id"], {"request": {"passthrough": "all"}},
        )
        _assert_header_missing(await _post_echo(client, oagw_base_url, oagw_headers, alias))


@pytest.mark.asyncio
async def test_required_headers_allows_unconfigured(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A guard with an empty config is stored and fails open; configuring it makes it enforce."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, upstream = await _guarded_echo(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "guard-hdr-open", None, None,
        )

        stored = (await client.get(
            f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}", headers=oagw_headers,
        )).json()
        refs = [p["plugin_ref"] for p in stored["plugins"]["items"]]
        assert refs == [REQUIRED_HEADERS_GUARD_PLUGIN_ID]

        resp = await _post_echo(client, oagw_base_url, oagw_headers, alias)
        assert resp.status_code == 200, (
            f"Expected 200 (unconfigured = fail-open), got {resp.status_code}: {resp.text[:500]}"
        )

        # The same binding, now configured, rejects: the binding is live.
        await update_upstream(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, plugins=_required_headers_plugins(request_headers=["x-foo"]),
        )
        _assert_header_missing(await _post_echo(client, oagw_base_url, oagw_headers, alias))


@pytest.mark.asyncio
@pytest.mark.parametrize("upstream_headers", PASSTHROUGH)
async def test_required_headers_case_insensitive(
    upstream_headers, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Required header names match case-insensitively.

    hyper lowercases inbound names, so what this pins is that a mixed-case
    name in the guard config matches the header on the wire.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _guarded_echo(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "guard-hdr-case", ["X-Correlation-ID"], upstream_headers,
        )

        resp = await _post_echo(
            client, oagw_base_url, oagw_headers, alias, {"X-CORRELATION-ID": "case-test"},
        )
        assert resp.status_code == 200, (
            f"Expected 200 (case-insensitive match), got {resp.status_code}: {resp.text[:500]}"
        )
        _assert_header_missing(await _post_echo(client, oagw_base_url, oagw_headers, alias))


@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="PLG-05 (F-3): `authorization` is stripped before guards run, so a "
           "guard requiring it rejects every client",
)
async def test_required_authorization_header_allows_client_that_sends_it(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A guard requiring `authorization` passes a client that sent one."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _guarded_echo(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "guard-hdr-authz", ["authorization"], None,
        )
        resp = await _post_echo(client, oagw_base_url, oagw_headers, alias)
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
