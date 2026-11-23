"""E2E tests for HEAD and OPTIONS routing (#4979)."""
import httpx
import pytest

from .helpers import create_route, create_upstream, unique_alias


async def _upstream_with_route(client, base, headers, mock_url, cleanup, prefix, methods, path):
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias,
    ))
    await create_route(client, base, headers, upstream["id"], methods, path)
    return alias


@pytest.mark.scenario("positive-12.8-head-request-returns-upstream-head", part="A")
@pytest.mark.asyncio
async def test_head_returns_the_upstream_head(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A HEAD reaches the upstream; the client gets its headers, the length
    of the representation included, and no body."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "head", ["GET", "HEAD"], "/echo-any",
        )
        resp = await client.head(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-any", headers=oagw_headers,
        )
        assert resp.status_code == 200, resp.headers
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        assert resp.headers.get("x-echo-method") == "HEAD"
        assert resp.headers.get("content-type") == "application/json"
        assert int(resp.headers["content-length"]) > 0
        assert resp.content == b""

        # The same route answers GET with a body of that kind.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-any", headers=oagw_headers,
        )
        assert resp.status_code == 200
        assert resp.json()["method"] == "GET"


@pytest.mark.scenario("positive-12.8-head-request-returns-upstream-head", part="B")
@pytest.mark.asyncio
async def test_head_on_an_event_stream_ends_at_its_head(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A HEAD on an event stream ends at its head instead of waiting for
    events (the client's timeout would fail the test)."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=5.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "head-sse", ["HEAD"], "/sse/events",
        )
        resp = await client.head(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/sse/events", headers=oagw_headers,
        )
        assert resp.status_code == 200, resp.headers
        assert resp.headers.get("content-type") == "text/event-stream"
        assert resp.content == b""


@pytest.mark.scenario("positive-12.8-head-request-returns-upstream-head", part="C")
@pytest.mark.scenario("positive-12.9-options-routed-preflight-answered-gateway", part="C")
@pytest.mark.asyncio
async def test_methods_must_be_listed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A GET route answers neither HEAD nor OPTIONS: both are 404 from the
    gateway."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "get-only", ["GET"], "/echo-any",
        )
        url = f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-any"
        for method in ("HEAD", "OPTIONS"):
            resp = await client.request(method, url, headers=oagw_headers)
            assert resp.status_code == 404, (method, resp.text[:300])
            assert resp.headers.get("x-oagw-error-source") == "gateway", method
        resp = await client.get(url, headers=oagw_headers)
        assert resp.status_code == 200


@pytest.mark.scenario("positive-12.9-options-routed-preflight-answered-gateway", part="A")
@pytest.mark.asyncio
async def test_options_is_proxied(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """OPTIONS reaches the upstream when the route lists it, also with an
    `Origin` but no `Access-Control-Request-Method`."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "options", ["POST", "OPTIONS"], "/echo-any",
        )
        url = f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-any"
        for extra in ({}, {"origin": "https://app.example.com"}):
            resp = await client.options(url, headers={**oagw_headers, **extra})
            assert resp.status_code == 200, (extra, resp.text[:300])
            assert resp.headers.get("x-oagw-error-source") == "upstream"
            assert resp.headers.get("allow") == "GET, HEAD, OPTIONS, POST"
            assert resp.json()["method"] == "OPTIONS"


@pytest.mark.scenario("positive-12.9-options-routed-preflight-answered-gateway", part="B")
@pytest.mark.asyncio
async def test_preflight_is_answered_by_the_gateway(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A CORS preflight gets the gateway's 204 and never reaches the
    upstream, even on a route that lists OPTIONS. It carries no credentials,
    as a browser's would."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "preflight", ["POST", "OPTIONS"], "/echo-any",
        )
        resp = await client.options(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo-any",
            headers={
                "origin": "https://app.example.com",
                "access-control-request-method": "POST",
            },
        )
        assert resp.status_code == 204, resp.text[:300]
        assert resp.headers.get("access-control-allow-origin") == "https://app.example.com"
        assert resp.headers.get("access-control-allow-methods") == "POST"
        assert "allow" not in resp.headers
        assert "x-echo-method" not in resp.headers
        assert resp.content == b""
