"""E2E tests for SSRF protection of upstream endpoints.

The suite runs with the SSRF policy on and `allow_cidrs` exempting loopback
for its mock upstream. Connect-time refusal (a hostname resolving into a
denied range) has no E2E test: no hostname here resolves into one.
"""
import httpx
import pytest

from .helpers import (
    PROXY_SCHEMA,
    assert_problem_body,
    create_upstream_raw,
    err_type,
    list_all,
    traced,
    unique_alias,
)

_NON_PUBLIC_IPV6 = "negative-26.4-non-public-ipv6-address-refused"


def _blocked(host: str, reason: str) -> dict:
    detail = f"endpoint[0] IP address '{host}' is blocked by SSRF protection: {reason}"
    return {
        "type": err_type("invalid_argument"),
        "title": "Invalid Argument",
        "status": 400,
        "detail": detail,
        "instance": "/oagw/v1/upstreams",
        "context": {"format": detail, "resource_type": PROXY_SCHEMA},
    }


@pytest.mark.scenario(_NON_PUBLIC_IPV6, part="A")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("host", "reason"),
    [
        pytest.param("[fec0::1]", "site-local address (fec0::/10)", id="site-local"),
        pytest.param("[5f00::1]", "SRv6 segment identifier (5f00::/16)", id="srv6-sid"),
        pytest.param(
            "[64:ff9b:1::a00:1]", "local-use NAT64 address (64:ff9b:1::/48)",
            id="local-use-nat64",
        ),
    ],
)
async def test_non_public_ipv6_endpoint_rejected(
    host, reason, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
):
    """Scenario 26.4-A: an IP endpoint in a non-public IPv6 range is a 400 naming it, and is not stored."""
    alias = unique_alias("ssrf-v6")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=alias,
            server={"endpoints": [{"scheme": "https", "host": host, "port": 443}]},
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem_body(resp, _blocked(host, reason))
        upstreams = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert not [u for u in upstreams if u["alias"] == alias]
