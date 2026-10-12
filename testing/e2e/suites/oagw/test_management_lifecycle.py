"""E2E tests for OAGW Management API lifecycle (upstream + route CRUD).

The in-memory repositories echo their input on create, so every create is
checked again through a read (GET or list) to prove it was stored.
"""
import uuid

import httpx
import pytest

from .helpers import (
    HTTP_PROTOCOL_ID,
    ROUTE_SCHEMA,
    UPSTREAM_SCHEMA,
    assert_problem,
    create_route,
    create_upstream,
    create_upstream_raw,
    delete_upstream,
    list_all,
    unique_alias,
    update_upstream_raw,
)


def _unique_host() -> str:
    return f"mgmt-{uuid.uuid4().hex[:8]}.example.com"


async def _get(client, base, headers, collection, rid):
    return await client.get(f"{base}/oagw/v1/{collection}/{rid}", headers=headers)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("port", "alias_suffix"),
    [
        # The standard port is left out of the alias.
        pytest.param(443, "", id="standard-port", marks=pytest.mark.scenario(
            "positive-2.1-create-minimal-http-upstream")),
        # A non-standard port is kept.
        pytest.param(8443, ":8443", id="non-standard-port", marks=pytest.mark.scenario(
            "positive-2.2-alias-auto-generation-non-standard-port")),
    ],
)
async def test_create_minimal_upstream_returns_201(
    port, alias_suffix, oagw_base_url, oagw_headers, mock_upstream, cleanup,
):
    """A minimal create (server + protocol only) defaults `enabled` and derives the alias."""
    _ = mock_upstream
    host = _unique_host()
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/upstreams",
            headers=oagw_headers,
            json={
                "server": {"endpoints": [{"scheme": "https", "host": host, "port": port}]},
                "protocol": HTTP_PROTOCOL_ID,
            },
        )
        assert resp.status_code == 201, resp.text[:500]
        created = cleanup.upstream(oagw_headers, resp.json())
        assert created["id"].startswith(UPSTREAM_SCHEMA)
        assert created["enabled"] is True
        assert created["alias"] == f"{host}{alias_suffix}"

        stored = (await _get(client, oagw_base_url, oagw_headers, "upstreams", created["id"])).json()
        assert stored["alias"] == created["alias"]
        assert stored["enabled"] is True


@pytest.mark.asyncio
async def test_get_upstream_by_id(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """GET /oagw/v1/upstreams/{id} returns the stored upstream."""
    _ = mock_upstream
    alias = unique_alias("mgmt-get")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))

        resp = await _get(client, oagw_base_url, oagw_headers, "upstreams", upstream["id"])
        assert resp.status_code == 200
        data = resp.json()
        assert data["id"] == upstream["id"]
        assert data["alias"] == alias
        assert data["server"]["endpoints"] == upstream["server"]["endpoints"]
        assert data["enabled"] is True


@pytest.mark.scenario("positive-2.12-list-upstreams-includes-disabled-resources")
@pytest.mark.asyncio
async def test_list_upstreams_includes_created(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenarios 2.12: the list includes created upstreams, disabled ones too."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        enabled = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=unique_alias("mgmt-list"),
        ))
        disabled = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("mgmt-list-off"), enabled=False,
        ))

        items = {u["id"]: u for u in await list_all(client, oagw_base_url, oagw_headers, "upstreams")}
        assert items[enabled["id"]]["enabled"] is True
        assert items[disabled["id"]]["enabled"] is False


@pytest.mark.scenario("positive-2.5-update-upstream")
@pytest.mark.asyncio
async def test_update_upstream_persists_changes(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.5: a PUT that changes mutable fields is reflected by GET."""
    _ = mock_upstream
    alias = unique_alias("mgmt-upd-ok")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias, tags=["v1"],
        ))
        resp = await update_upstream_raw(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, tags=["v2"], enabled=False,
        )
        assert resp.status_code == 200, resp.text[:500]

        stored = (await _get(client, oagw_base_url, oagw_headers, "upstreams", upstream["id"])).json()
        assert stored["tags"] == ["v2"]
        assert stored["enabled"] is False


@pytest.mark.asyncio
async def test_update_upstream_alias_immutable(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """ADR-0010: the alias is immutable once set; a PUT changing it is a 400."""
    _ = mock_upstream
    alias = unique_alias("mgmt-upd")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        uid = upstream["id"]

        # Control: a PUT that keeps the alias is accepted.
        ok = await update_upstream_raw(
            client, oagw_base_url, oagw_headers, uid, mock_upstream_url, alias=alias, tags=["v2"],
        )
        assert ok.status_code == 200, ok.text[:500]

        bad = await update_upstream_raw(
            client, oagw_base_url, oagw_headers, uid, mock_upstream_url,
            alias=unique_alias("mgmt-upd-v2"),
        )
        assert_problem(bad, 400, esrc=None, detail_contains="alias cannot be changed")

        # The rejected PUT did not partially apply.
        stored = (await _get(client, oagw_base_url, oagw_headers, "upstreams", uid)).json()
        assert stored["alias"] == alias
        assert stored["tags"] == ["v2"]


@pytest.mark.asyncio
async def test_update_hostname_upstream_to_ip_rejected(
    oagw_base_url, oagw_headers, mock_upstream, cleanup,
):
    """ADR-0010: moving a hostname upstream to an IP endpoint would orphan its derived alias."""
    _ = mock_upstream
    host = _unique_host()
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/upstreams",
            headers=oagw_headers,
            json={
                "server": {"endpoints": [{"scheme": "https", "host": host, "port": 443}]},
                "protocol": HTTP_PROTOCOL_ID,
            },
        )
        assert resp.status_code == 201, resp.text[:500]
        upstream = cleanup.upstream(oagw_headers, resp.json())

        bad = await client.put(
            f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}",
            headers=oagw_headers,
            json={
                "server": {"endpoints": [{"scheme": "https", "host": "10.0.0.1", "port": 443}]},
                "protocol": HTTP_PROTOCOL_ID,
                # A PUT must carry `enabled` and `tags` or it is a 422 (M-03).
                "enabled": True,
                "tags": [],
            },
        )
        assert_problem(bad, 400, esrc=None, detail_contains="cannot change hostname-based endpoints to IP-based")

        stored = (await _get(client, oagw_base_url, oagw_headers, "upstreams", upstream["id"])).json()
        assert stored["alias"] == host
        assert stored["server"]["endpoints"][0]["host"] == host


@pytest.mark.scenario("negative-2.8-alias-uniqueness-enforced-per-tenant")
@pytest.mark.asyncio
async def test_duplicate_alias_rejected(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.8: a second upstream with the same alias in a tenant is rejected."""
    _ = mock_upstream
    alias = unique_alias("mgmt-dup")
    async with httpx.AsyncClient(timeout=10.0) as client:
        cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem(resp, 409, esrc=None, category="already_exists")

        matching = [
            u for u in await list_all(client, oagw_base_url, oagw_headers, "upstreams")
            if u["alias"] == alias
        ]
        assert len(matching) == 1


@pytest.mark.asyncio
async def test_delete_upstream_returns_204(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """DELETE /oagw/v1/upstreams/{id} returns 204 and the resource is gone."""
    _ = mock_upstream
    alias = unique_alias("mgmt-del")
    async with httpx.AsyncClient(timeout=10.0) as client:
        # Registered so a failure before the DELETE still cleans up.
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        resp = await delete_upstream(client, oagw_base_url, oagw_headers, upstream["id"])
        assert resp.status_code == 204

        resp = await _get(client, oagw_base_url, oagw_headers, "upstreams", upstream["id"])
        assert_problem(resp, 404, esrc=None, category="not_found")


@pytest.mark.scenario("positive-3.1-create-http-route-method-path")
# ---------------------------------------------------------------------------
# Route lifecycle
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_create_route_returns_201(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 3.1: a created route is stored and routes proxy traffic."""
    _ = mock_upstream
    alias = unique_alias("mgmt-rte")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        route = await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1/models",
        )
        assert route["id"].startswith(ROUTE_SCHEMA)
        assert route["upstream_id"] == upstream["id"]
        assert route["match"]["http"]["methods"] == ["GET"]
        assert route["match"]["http"]["path"] == "/v1/models"

        stored = await _get(client, oagw_base_url, oagw_headers, "routes", route["id"])
        assert stored.status_code == 200
        assert stored.json()["match"] == route["match"]

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models", headers=oagw_headers,
        )
        assert resp.status_code == 200
        assert resp.headers.get("x-oagw-error-source") == "upstream"


@pytest.mark.scenario("positive-2.7-delete-upstream-cascades-routes")
@pytest.mark.asyncio
async def test_delete_upstream_cascades_routes(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.7: deleting an upstream deletes its routes."""
    _ = mock_upstream
    alias = unique_alias("mgmt-cascade")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        uid = upstream["id"]
        route = await create_route(client, oagw_base_url, oagw_headers, uid, ["GET"], "/test")
        by_upstream = {"upstream_id": uid}
        before = await list_all(client, oagw_base_url, oagw_headers, "routes", **by_upstream)
        assert [r["id"] for r in before] == [route["id"]]

        resp = await delete_upstream(client, oagw_base_url, oagw_headers, uid)
        assert resp.status_code == 204

        resp = await _get(client, oagw_base_url, oagw_headers, "routes", route["id"])
        assert_problem(resp, 404, esrc=None, category="not_found")
        assert await list_all(client, oagw_base_url, oagw_headers, "routes", **by_upstream) == []


@pytest.mark.scenario("positive-2.9-tags-support-discovery-filtering")
# ---------------------------------------------------------------------------
# Tags
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_create_upstream_with_tags(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.9 (discovery half): tags are stored and returned by GET and list.

    The filtering half is not implemented (no tag filter exists).
    """
    _ = mock_upstream
    alias = unique_alias("mgmt-tags")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, tags=["openai", "llm"],
        ))

        stored = (await _get(client, oagw_base_url, oagw_headers, "upstreams", upstream["id"])).json()
        assert sorted(stored["tags"]) == ["llm", "openai"]

        listed = [
            u for u in await list_all(client, oagw_base_url, oagw_headers, "upstreams")
            if u["id"] == upstream["id"]
        ]
        assert len(listed) == 1 and sorted(listed[0]["tags"]) == ["llm", "openai"]


@pytest.mark.scenario("positive-2.9-tags-support-discovery-filtering")
@pytest.mark.asyncio
@pytest.mark.parametrize("param", ["$filter", "$orderby", "tags"])
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="M-07: list endpoints ignore $filter, $orderby and unknown parameters "
           "and return the unfiltered list with 200",
)
async def test_list_rejects_unsupported_query_parameters(param, oagw_base_url, oagw_headers):
    """Scenario 2.9 (filter half): an unsupported list parameter is a 400, not a silent no-op."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/upstreams", headers=oagw_headers, params={param: "llm"},
        )
        assert_problem(resp, 400, esrc=None)
