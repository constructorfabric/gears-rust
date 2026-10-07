"""E2E tests for OAGW proxy HTTP round-trip (passthrough, headers)."""
import json
from urllib.parse import urlparse

import httpx
import pytest

from .helpers import create_route, create_upstream, unique_alias

PASSTHROUGH_ALL = {"request": {"passthrough": "all"}}


@pytest.mark.scenario("positive-12.1-plain-http-request-response-passthrough")
@pytest.mark.asyncio
async def test_post_proxy_returns_upstream_response(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 12.1: a POST body reaches the upstream unchanged and its answer comes back."""
    _ = mock_upstream
    alias = unique_alias("proxy-post")
    payload = {"model": "gpt-4", "messages": [{"role": "user", "content": "Hello ✓"}]}
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "content-type": "application/json"},
            json=payload,
        )
        assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        echo = resp.json()
        assert echo["method"] == "POST"
        assert echo["path"] == "/echo"
        assert json.loads(echo["body"]) == payload
        assert echo["headers"]["content-type"] == "application/json"


@pytest.mark.scenario("positive-12.1-plain-http-request-response-passthrough")
@pytest.mark.asyncio
async def test_get_proxy_returns_upstream_response(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Proxy GET to /v1/models returns the mock model list unchanged."""
    _ = mock_upstream
    alias = unique_alias("proxy-get")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1/models",
        )

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models",
            headers=oagw_headers,
        )
        assert resp.status_code == 200
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        assert resp.headers.get("content-type") == "application/json"
        body = resp.json()
        assert body["object"] == "list"
        assert [m["id"] for m in body["data"]] == ["gpt-4", "gpt-3.5-turbo"]


@pytest.mark.scenario("positive-7.2-hop-hop-headers-stripped")
# ---------------------------------------------------------------------------
# Header verification via /echo
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_hop_by_hop_headers_stripped(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 7.2: hop-by-hop headers are stripped even under `passthrough: all`.

    Without `passthrough: all` every client header is dropped anyway, so the
    absence checks would pass whether or not stripping works. The control
    header proves client headers do reach the upstream in this config, and
    `x-conn-nominated` is hop-by-hop only because `Connection` names it.
    """
    _ = mock_upstream
    alias = unique_alias("proxy-hop")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, upstream_headers=PASSTHROUGH_ALL,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "connection": "keep-alive, x-conn-nominated",
                "x-conn-nominated": "x",
                "keep-alive": "timeout=5",
                "te": "trailers",
                "trailer": "X-Checksum",
                "proxy-authorization": "Basic dGVzdDp0ZXN0",
                "x-e2e-control": "arrives",
            },
            json={"test": True},
        )
        assert resp.status_code == 200
        echoed = resp.json()["headers"]

        assert echoed.get("x-e2e-control") == "arrives"
        for h in ("keep-alive", "te", "trailer", "x-conn-nominated", "proxy-authorization"):
            assert h not in echoed, f"hop-by-hop header {h!r} was forwarded upstream"
        # OAGW may set its own Connection, but not relay the client's.
        assert "x-conn-nominated" not in echoed.get("connection", "").lower()


@pytest.mark.scenario("positive-7.3-host-header-replaced-upstream-host")
@pytest.mark.asyncio
async def test_host_header_replaced(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 7.3: a client-supplied Host is replaced with the upstream's host."""
    _ = mock_upstream
    alias = unique_alias("proxy-host")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, upstream_headers=PASSTHROUGH_ALL,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={
                **oagw_headers,
                "content-type": "application/json",
                "host": "evil.example.com",
            },
            json={"test": True},
        )
        assert resp.status_code == 200
        assert resp.json()["headers"].get("host") == urlparse(mock_upstream_url).netloc
