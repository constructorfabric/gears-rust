"""E2E tests for Management API permission checks (#4972).

Every upstream and route CRUD call asks the policy engine for `create`,
`override`, `read` or `delete` on the upstream or route type. With
static-authz the deny comes from its nil-tenant special case, so these tests
cover OAGW's enforcement and error mapping, not a specific permission set.
"""
import os

import httpx
import pytest

from .helpers import (
    ROUTE_SCHEMA,
    UPSTREAM_SCHEMA,
    assert_problem,
    create_route,
    create_upstream,
    unique_alias,
)


def _denied_headers() -> dict:
    token = os.getenv("E2E_AUTH_TOKEN_NIL_TENANT", "e2e-token-nil-tenant")
    return {"Authorization": f"Bearer {token}", "content-type": "application/json"}


@pytest.mark.scenario("negative-1.2-permission-gates-upstream-route-plugin-crud", part="C")
@pytest.mark.asyncio
async def test_management_calls_without_permission_get_403(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Each upstream and route operation returns 403 for a denied caller.

    Before #4972 the list calls returned 200 and an empty list, and create
    succeeded in the caller's tenant.
    """
    _ = mock_upstream
    denied = _denied_headers()
    base = f"{oagw_base_url}/oagw/v1"
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("mgmt-authz"),
        ))
        route = await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1/models",
        )
        # Valid bodies, so a 403 is not a body error.
        upstream_body = {
            "server": upstream["server"],
            "protocol": upstream["protocol"],
            "alias": unique_alias("mgmt-authz-new"),
            "tags": [],
            "enabled": True,
        }
        route_body = {
            "upstream_id": upstream["id"],
            "match": {"http": {
                "methods": ["GET"],
                "path": "/v2",
                "query_allowlist": [],
                "path_suffix_mode": "append",
            }},
            "tags": [],
            "priority": 0,
            "enabled": True,
        }

        upstream_calls = [
            ("GET", f"{base}/upstreams", None),
            ("POST", f"{base}/upstreams", upstream_body),
            ("GET", f"{base}/upstreams/{upstream['id']}", None),
            ("PUT", f"{base}/upstreams/{upstream['id']}", upstream_body),
            ("DELETE", f"{base}/upstreams/{upstream['id']}", None),
        ]
        route_calls = [
            ("GET", f"{base}/routes", None),
            ("POST", f"{base}/routes", route_body),
            ("GET", f"{base}/routes/{route['id']}", None),
            ("PUT", f"{base}/routes/{route['id']}", route_body),
            ("DELETE", f"{base}/routes/{route['id']}", None),
        ]
        for calls, resource_type in ((upstream_calls, UPSTREAM_SCHEMA), (route_calls, ROUTE_SCHEMA)):
            for method, url, body in calls:
                resp = await client.request(method, url, headers=denied, json=body)
                assert_problem(
                    resp, 403,
                    esrc=None,
                    category="permission_denied",
                    resource_type=resource_type,
                )

        # Nothing changed: the allowed caller still sees both resources.
        resp = await client.get(f"{base}/upstreams/{upstream['id']}", headers=oagw_headers)
        assert resp.status_code == 200, resp.text[:300]
        resp = await client.get(f"{base}/routes/{route['id']}", headers=oagw_headers)
        assert resp.status_code == 200, resp.text[:300]
