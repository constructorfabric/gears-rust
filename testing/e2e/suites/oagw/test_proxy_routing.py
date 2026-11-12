"""E2E tests for proxy path addressing and request header boundaries.

Raw sockets send the encoded paths: an HTTP client may normalize `%2e`
before the request leaves.
"""
import asyncio
import json
from urllib.parse import urlparse

import httpx
import pytest

from .helpers import (
    assert_problem, create_route, create_route_raw, create_upstream, list_all, unique_alias,
)

PASSTHROUGH_ALL = {"request": {"passthrough": "all"}}


async def _raw_get(base_url: str, path: str, headers: dict) -> tuple[int, dict, bytes]:
    parsed = urlparse(base_url)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 80
    raw = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        "Connection: close\r\n"
        + "".join(f"{k}: {v}\r\n" for k, v in headers.items())
        + "\r\n"
    ).encode()
    reader, writer = await asyncio.wait_for(asyncio.open_connection(host, port), timeout=10.0)
    writer.write(raw)
    await writer.drain()
    try:
        head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=10.0)
        lines = head.decode("latin-1").split("\r\n")
        status = int(lines[0].split(" ", 2)[1])
        resp_headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            if name:
                resp_headers[name.strip().lower()] = value.strip()
        length = int(resp_headers.get("content-length", "0"))
        body = await asyncio.wait_for(reader.readexactly(length), timeout=10.0)
    finally:
        writer.close()
    return status, resp_headers, body


async def _upstream_with_route(client, base, headers, mock_url, cleanup, prefix, method, path,
                               upstream_headers=None):
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias, upstream_headers=upstream_headers,
    ))
    await create_route(client, base, headers, upstream["id"], [method], path)
    return alias


async def _upstream_with_get_routes(client, base, headers, mock_url, cleanup, prefix, routes):
    """An upstream with a GET route per `(path, priority)`; returns the alias."""
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias,
    ))
    for path, priority in routes:
        await create_route(client, base, headers, upstream["id"], ["GET"], path, priority=priority)
    return alias


async def _outbound_path(base_url: str, alias: str, suffix: str, headers: dict) -> str:
    """The path the mock received for `suffix`, sent as is: the mock has no
    handler for these paths and its 404 names the path."""
    status, resp_headers, body = await _raw_get(base_url, f"/oagw/v1/proxy/{alias}/{suffix}", headers)
    assert status == 404, (suffix, status, body[:300])
    assert resp_headers.get("x-oagw-error-source") == "upstream", (suffix, body[:300])
    return json.loads(body)["path"]


def _assert_invalid_proxy_path(status: int, headers: dict, body: bytes, case) -> None:
    assert status == 400, (case, status, body[:300])
    assert headers.get("x-oagw-error-source") == "gateway", case
    problem = json.loads(body)
    assert problem["status"] == 400, problem
    assert problem["type"].endswith("invalid_argument.v1~"), problem
    violations = problem["context"]["field_violations"]
    assert [(v["field"], v["reason"]) for v in violations] == [("path", "INVALID_PROXY_PATH")], problem


@pytest.mark.scenario("negative-7.7-encoded-dot-segments-rejected-400")
@pytest.mark.asyncio
async def test_encoded_dot_segments_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: an encoded dot-segment is a 400 from OAGW, not forwarded.

    An upstream that decodes the path would resolve `%2e%2e` after OAGW
    matched the route and escape the route prefix.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "path-dots", "GET", "/v1/models",
        )
    for suffix in ("v1/models/%2e%2e/%2e%2e/admin", "v1/models/..%2f..%2fadmin", "v1/models/.%2E",
                   "v1/models/..;/admin", "v1/models/%252e%252e/admin"):
        status, headers, body = await _raw_get(
            oagw_base_url, f"/oagw/v1/proxy/{alias}/{suffix}", oagw_headers,
        )
        _assert_invalid_proxy_path(status, headers, body, suffix)


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="A")
@pytest.mark.asyncio
async def test_route_prefix_matches_whole_segments(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: route `/v1/models` covers `/v1/models`, not `/v1/modelsx`."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "path-segment", "GET", "/v1/models",
        )
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models", headers=oagw_headers,
        )
        # The mock answers 200 only on exactly `/v1/models`.
        assert resp.status_code == 200, resp.text[:300]

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/modelsx", headers=oagw_headers,
        )
        # No route matches: a gateway 404, not the upstream's.
        assert_problem(resp, 404)


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="B")
@pytest.mark.asyncio
async def test_encoded_unreserved_characters_match_their_route(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: `/v1/m%6fdels` is `/v1/models`: it matches that route rather
    than escaping to a broader one, and the upstream gets the decoded path.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "path-unreserved", "GET", "/v1/models",
        )
    status, headers, body = await _raw_get(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/v1/m%6fdels", oagw_headers,
    )
    # The mock answers 200 only on exactly `/v1/models`: the outbound path
    # is the decoded one.
    assert status == 200, (status, body[:300])
    assert headers.get("x-oagw-error-source") == "upstream"


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="C")
@pytest.mark.asyncio
async def test_bare_alias_matches_root_route(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: `/oagw/v1/proxy/{alias}` with no path matches route `/`."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "path-bare", "GET", "/",
        )
        resp = await client.get(f"{oagw_base_url}/oagw/v1/proxy/{alias}", headers=oagw_headers)
        # The mock has no `/` handler: the 404 is the upstream's, so the
        # request was routed.
        assert resp.status_code == 404, resp.text[:300]
        assert resp.headers.get("x-oagw-error-source") == "upstream"


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="F")
@pytest.mark.asyncio
async def test_encoded_separator_cannot_skip_a_narrower_route(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: with routes `/v1` and `/v1/models`, `/v1%2Fmodels` is a 400.

    As sent it is under `/v1`; an upstream that decodes `%2F` would serve
    `/v1/models` under the `/v1` route's policy.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = unique_alias("path-separator")
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        for path in ("/v1", "/v1/models"):
            await create_route(client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], path)
    for suffix in ("v1%2Fmodels", "v1%2fmodels", "v1%5Cmodels", "v1%EF%BC%8Fmodels"):
        status, headers, body = await _raw_get(
            oagw_base_url, f"/oagw/v1/proxy/{alias}/{suffix}", oagw_headers,
        )
        _assert_invalid_proxy_path(status, headers, body, suffix)
    # Sent decoded, it reaches the narrower route.
    status, headers, body = await _raw_get(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/v1/models", oagw_headers,
    )
    assert status == 200, (status, body[:300])
    assert headers.get("x-oagw-error-source") == "upstream"


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="F")
@pytest.mark.asyncio
async def test_encoded_separator_that_keeps_the_route_is_forwarded(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: the reverse bypass is a 400, and a `%2F` that selects the same
    route either way is forwarded as sent.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_get_routes(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "path-sep-keep",
            [("/v1", 0), ("/v1/admin", 0), ("/v3", 0), ("/v3/a%2Fb", 0)],
        )
    # As sent `/v3/a/b` is under `/v3`; decoded, route `/v3/a%2Fb` is longer.
    status, headers, body = await _raw_get(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/v3/a/b", oagw_headers,
    )
    _assert_invalid_proxy_path(status, headers, body, "/v3/a/b")
    for suffix in ("v1/admin%2Fx", "v1%2Fother"):
        assert await _outbound_path(oagw_base_url, alias, suffix, oagw_headers) == f"/{suffix}"


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="F")
@pytest.mark.asyncio
async def test_prefixes_equal_once_decoded_leave_one_route_unreachable(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: of `/v4/a/b` and `/v4/a%2Fb`, the decoded ranking picks one by
    priority; requests under the other are a 400.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_get_routes(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "path-sep-tie",
            [("/v4/a/b", 1), ("/v4/a%2Fb", 0)],
        )
    assert await _outbound_path(oagw_base_url, alias, "v4/a/b/x", oagw_headers) == "/v4/a/b/x"
    status, headers, body = await _raw_get(
        oagw_base_url, f"/oagw/v1/proxy/{alias}/v4/a%2Fb/x", oagw_headers,
    )
    _assert_invalid_proxy_path(status, headers, body, "/v4/a%2Fb/x")


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="D")
@pytest.mark.asyncio
async def test_unreachable_route_prefix_is_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: a route prefix no request could match is a 400 on create, and
    nothing is stored.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("path-prefix"),
        ))
        for prefix in ("/v1//chat", "/v1//", "//", "/v1/./chat", "/v1;x", "/v1/%2e%2e",
                       "/v1/%zz", "v1/chat"):
            resp = await create_route_raw(
                client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], prefix,
            )
            assert resp.status_code == 400, (prefix, resp.status_code, resp.text[:300])
            problem = resp.json()
            assert problem["status"] == 400, problem
            assert problem["type"].endswith("invalid_argument.v1~"), problem
        routes = await list_all(
            client, oagw_base_url, oagw_headers, "routes", upstream_id=upstream["id"],
        )
        assert routes == [], routes


@pytest.mark.scenario("negative-3.12-route-prefix-matches-whole-segments", part="E")
@pytest.mark.asyncio
async def test_trailing_slash_route_is_not_forwarded_before_a_segment_suffix(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4979: route `/v1/admin/` serves `/v1/admin.json` as `/v1/admin.json`,
    not `/v1/admin/.json`, and `/v1/admin/x` as `/v1/admin/x`.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_get_routes(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "path-slash",
            [("/v1/admin/", 0)],
        )
    for suffix in ("v1/admin.json", "v1/admin/x"):
        assert await _outbound_path(oagw_base_url, alias, suffix, oagw_headers) == f"/{suffix}"


@pytest.mark.scenario("positive-7.2-hop-hop-headers-stripped")
@pytest.mark.asyncio
async def test_nominated_header_is_stripped_under_an_allowlist(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4982: a header the allowlist names is not forwarded when the client
    nominates it in `Connection`.
    """
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "hop-allow", "POST", "/echo",
            upstream_headers={"request": {
                "passthrough": "allowlist",
                "passthrough_allowlist": ["x-hop", "x-e2e-control"],
            }},
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers={**oagw_headers, "connection": "x-hop", "x-hop": "1", "x-e2e-control": "arrives"},
            json={},
        )
        assert resp.status_code == 200, resp.text[:300]
        echoed = resp.json()["headers"]
        assert echoed.get("x-e2e-control") == "arrives"
        assert "x-hop" not in echoed, "a nominated header was forwarded upstream"


@pytest.mark.scenario("positive-7.2-hop-hop-headers-stripped")
@pytest.mark.asyncio
async def test_every_connection_line_is_honoured(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4982: names nominated by a second `Connection` line are not forwarded."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "hop-multi", "POST", "/echo", upstream_headers=PASSTHROUGH_ALL,
        )
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
            headers=[
                *oagw_headers.items(),
                ("content-type", "application/json"),
                ("connection", "x-first-nominated"),
                ("connection", "x-second-nominated"),
                ("x-first-nominated", "1"),
                ("x-second-nominated", "2"),
                ("x-e2e-control", "arrives"),
            ],
            json={"test": True},
        )
        assert resp.status_code == 200, resp.text[:300]
        echoed = resp.json()["headers"]
        assert echoed.get("x-e2e-control") == "arrives"
        for name in ("x-first-nominated", "x-second-nominated"):
            assert name not in echoed, f"{name} was forwarded upstream"


@pytest.mark.scenario("positive-7.2-hop-hop-headers-stripped", part="B")
@pytest.mark.asyncio
async def test_client_forwarding_headers_not_forwarded(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """#4982: client forwarding headers (`Forwarded`, `X-Forwarded-*`,
    `X-Real-IP`, `X-Client-IP`, `True-Client-IP`) do not reach the upstream
    under `passthrough: all`; an allowlist that names one forwards it.
    """
    _ = mock_upstream
    sent = {
        "x-forwarded-for": "203.0.113.7",
        "x-forwarded-host": "evil.example",
        "x-forwarded-proto": "http",
        "x-real-ip": "203.0.113.8",
        "forwarded": "for=203.0.113.9;host=internal",
        "x-client-ip": "203.0.113.10",
        "true-client-ip": "203.0.113.11",
        "x-e2e-control": "arrives",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        all_alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "fwd-all", "POST", "/echo", upstream_headers=PASSTHROUGH_ALL,
        )
        allow_alias = await _upstream_with_route(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "fwd-allow", "POST", "/echo",
            upstream_headers={"request": {
                "passthrough": "allowlist",
                "passthrough_allowlist": ["x-forwarded-for", "x-e2e-control"],
            }},
        )

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{all_alias}/echo",
            headers={**oagw_headers, **sent}, json={},
        )
        assert resp.status_code == 200, resp.text[:300]
        echoed = resp.json()["headers"]
        assert echoed.get("x-e2e-control") == "arrives"
        for name in ("x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "x-real-ip",
                     "forwarded", "x-client-ip", "true-client-ip"):
            assert name not in echoed, f"{name} was forwarded upstream"

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{allow_alias}/echo",
            headers={**oagw_headers, **sent}, json={},
        )
        assert resp.status_code == 200, resp.text[:300]
        echoed = resp.json()["headers"]
        assert echoed.get("x-forwarded-for") == "203.0.113.7"
        assert "x-forwarded-host" not in echoed
