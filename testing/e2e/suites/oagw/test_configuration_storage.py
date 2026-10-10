"""E2E tests for configuration storage (ADR-0018): OAGW on a database, with
upstreams and routes provisioned from the types registry.

They need this suite's ``sqlite`` profile (``make e2e-oagw-sqlite``). It binds
OAGW to SQLite and adds the upstream and routes below to the types-registry
``entities`` (see ``e2e.yaml``). The launcher cannot restart the server, so
restart survival is tested in the gear's ``gear_tests.rs`` and the boot
reconcile in ``domain/services/registry_reconcile_tests.rs``.

The tests run when ``OAGW_E2E_PROFILE=sqlite`` is set or the server has the
registry upstream, and are skipped only when neither holds. 19.1 reads the
stored rows from the SQLite file at ``OAGW_E2E_DB``.
"""
import os
import sqlite3
import uuid

import httpx
import pytest

from .helpers import (
    HTTP_PROTOCOL_ID,
    ROUTE_SCHEMA,
    UPSTREAM_SCHEMA,
    assert_problem,
    create_route,
    list_all,
)

# Mirrors the types-registry entities of the sqlite profile in e2e.yaml.
REGISTRY_UPSTREAM_ID = f"{UPSTREAM_SCHEMA}5e0a1900-0000-4000-8000-000000000001"
REGISTRY_ROUTE_ID = f"{ROUTE_SCHEMA}5e0a1900-0000-4000-8000-000000000002"
BARE_UUID_ROUTE_ID = f"{ROUTE_SCHEMA}5e0a1900-0000-4000-8000-000000000003"
REGISTRY_ALIAS = "e2e-registry-upstream"
REGISTRY_TAGS = ["e2e-registry"]
# The tenant of `oagw_headers`, the root tenant a registry instance without
# `tenant_id` is provisioned in.
ROOT_TENANT_ID = "00000000-df51-5b42-9538-d2b56b7ee953"


@pytest.fixture(autouse=True)
async def sqlite_profile(oagw_base_url, oagw_headers):
    """Run on the sqlite profile; skip only when the profile is not set and
    the server lacks the registry upstream, so a lost profile ``env:`` cannot
    turn these tests into silent skips."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/upstreams/{REGISTRY_UPSTREAM_ID}", headers=oagw_headers,
        )
    if os.getenv("OAGW_E2E_PROFILE") == "sqlite":
        assert resp.status_code == 200, (
            f"OAGW_E2E_PROFILE=sqlite but the registry upstream is missing: {resp.text[:300]}"
        )
    elif resp.status_code == 404:
        pytest.skip("needs the sqlite profile (registry entities, OAGW on SQLite): make e2e-oagw-sqlite")
    else:
        assert resp.status_code == 200, resp.text[:300]


def _stored_owner(db: sqlite3.Connection, table: str, gts_id: str) -> str:
    """``managed_by`` of a stored row; SQLite stores UUIDs as 16-byte blobs."""
    row_id = uuid.UUID(gts_id.rsplit("~", 1)[-1]).bytes
    row = db.execute(f"SELECT managed_by FROM {table} WHERE id = ?", (row_id,)).fetchone()
    assert row is not None, f"{gts_id} is not in {table}"
    return row[0]


async def _get(client, base, headers, collection, rid):
    resp = await client.get(f"{base}/oagw/v1/{collection}/{rid}", headers=headers)
    assert resp.status_code == 200, resp.text[:500]
    return resp.json()


def _assert_registry_managed(resp: httpx.Response, resource_type: str) -> None:
    body = assert_problem(
        resp, 400, esrc=None, category="failed_precondition", resource_type=resource_type,
    )
    violations = body["context"].get("violations") or []
    assert [(v.get("type"), v.get("subject")) for v in violations] == [
        ("REGISTRY_MANAGED", "managed_by"),
    ], body


@pytest.mark.scenario("positive-19.1-registry-instances-provisioned-at-startup")
@pytest.mark.asyncio
async def test_registry_instances_provisioned_at_startup(
    oagw_base_url, oagw_headers, mock_upstream,
):
    """Scenario 19.1: the registry upstream and routes exist under their instance
    IDs in the root tenant, are listed, proxy to the mock upstream, and are
    stored in OAGW's SQLite file as registry-managed rows."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await _get(client, oagw_base_url, oagw_headers, "upstreams", REGISTRY_UPSTREAM_ID)
        assert upstream["tenant_id"] == ROOT_TENANT_ID
        assert upstream["alias"] == REGISTRY_ALIAS
        assert upstream["server"]["endpoints"] == [
            {"scheme": "http", "host": "127.0.0.1", "port": 19876},
        ]
        assert upstream["protocol"] == HTTP_PROTOCOL_ID
        assert upstream["enabled"] is True
        assert upstream["tags"] == REGISTRY_TAGS

        route = await _get(client, oagw_base_url, oagw_headers, "routes", REGISTRY_ROUTE_ID)
        assert route["tenant_id"] == ROOT_TENANT_ID
        assert route["upstream_id"] == REGISTRY_UPSTREAM_ID
        assert route["match"]["http"]["methods"] == ["GET"]
        assert route["match"]["http"]["path"] == "/v1/models"
        assert route["tags"] == REGISTRY_TAGS
        # Left out of the instance, so the defaults apply.
        assert route["enabled"] is True
        assert route["priority"] == 0

        # This instance names its upstream by bare UUID.
        bare = await _get(client, oagw_base_url, oagw_headers, "routes", BARE_UUID_ROUTE_ID)
        assert bare["upstream_id"] == REGISTRY_UPSTREAM_ID
        assert bare["match"]["http"]["path"] == "/v1/bare-uuid"

        upstreams = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert [u["id"] for u in upstreams if u["alias"] == REGISTRY_ALIAS] == [REGISTRY_UPSTREAM_ID]
        routes = await list_all(
            client, oagw_base_url, oagw_headers, "routes", upstream_id=REGISTRY_UPSTREAM_ID,
        )
        assert {REGISTRY_ROUTE_ID, BARE_UUID_ROUTE_ID} <= {r["id"] for r in routes}

        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{REGISTRY_ALIAS}/v1/models", headers=oagw_headers,
        )
        assert resp.status_code == 200, resp.text[:300]
        assert resp.headers.get("x-oagw-error-source") == "upstream"
        assert resp.json()["object"] == "list"
        # The mock has no /v1/bare-uuid: its own 404 shows the route matched.
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{REGISTRY_ALIAS}/v1/bare-uuid", headers=oagw_headers,
        )
        assert resp.status_code == 404, resp.text[:300]
        assert resp.headers.get("x-oagw-error-source") == "upstream"

    # The rows are in OAGW's SQLite file, so OAGW runs on the database and
    # not in memory.
    db_path = os.path.expanduser(os.getenv("OAGW_E2E_DB", "~/.cf-gears/oagw/oagw.db"))
    assert os.path.isfile(db_path), (
        f"no OAGW database at {db_path}; set OAGW_E2E_DB to <home_dir>/oagw/oagw.db"
    )
    with sqlite3.connect(f"file:{db_path}?mode=ro", uri=True) as db:
        assert _stored_owner(db, "oagw_upstream", REGISTRY_UPSTREAM_ID) == "registry"
        for route_id in (REGISTRY_ROUTE_ID, BARE_UUID_ROUTE_ID):
            assert _stored_owner(db, "oagw_route", route_id) == "registry"


@pytest.mark.scenario("negative-19.2-registry-managed-resources-read-only")
@pytest.mark.asyncio
async def test_registry_managed_resources_read_only(
    oagw_base_url, oagw_headers, mock_upstream, cleanup,
):
    """Scenario 19.2: PUT and DELETE on a registry upstream or route are a 400
    REGISTRY_MANAGED and change nothing; a route created through the API on
    the registry upstream is the caller's, and can be deleted."""
    _ = mock_upstream
    json_headers = {**oagw_headers, "content-type": "application/json"}
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await _get(client, oagw_base_url, oagw_headers, "upstreams", REGISTRY_UPSTREAM_ID)
        route = await _get(client, oagw_base_url, oagw_headers, "routes", REGISTRY_ROUTE_ID)

        # Valid replacements, so only the ownership check can reject them.
        resp = await client.put(
            f"{oagw_base_url}/oagw/v1/upstreams/{REGISTRY_UPSTREAM_ID}",
            headers=json_headers,
            json={
                "server": upstream["server"],
                "protocol": upstream["protocol"],
                "alias": REGISTRY_ALIAS,
                "enabled": False,
                "tags": ["changed"],
            },
        )
        _assert_registry_managed(resp, UPSTREAM_SCHEMA)
        resp = await client.put(
            f"{oagw_base_url}/oagw/v1/routes/{REGISTRY_ROUTE_ID}",
            headers=json_headers,
            json={
                "match": {"http": {"methods": ["GET"], "path": "/v1/changed"}},
                "enabled": False,
                "tags": ["changed"],
                "priority": 0,
            },
        )
        _assert_registry_managed(resp, ROUTE_SCHEMA)
        resp = await client.delete(
            f"{oagw_base_url}/oagw/v1/routes/{REGISTRY_ROUTE_ID}", headers=oagw_headers,
        )
        _assert_registry_managed(resp, ROUTE_SCHEMA)
        resp = await client.delete(
            f"{oagw_base_url}/oagw/v1/upstreams/{REGISTRY_UPSTREAM_ID}", headers=oagw_headers,
        )
        _assert_registry_managed(resp, UPSTREAM_SCHEMA)

        assert await _get(client, oagw_base_url, oagw_headers, "upstreams", REGISTRY_UPSTREAM_ID) == upstream
        assert await _get(client, oagw_base_url, oagw_headers, "routes", REGISTRY_ROUTE_ID) == route
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{REGISTRY_ALIAS}/v1/models", headers=oagw_headers,
        )
        assert resp.status_code == 200, resp.text[:300]

        # A route created through the API on the registry upstream is API-managed.
        # The mock answers an unknown path with its own 404, which proves the
        # request was routed; the path is unique, so a row left by a killed
        # run of this test can't overlap.
        path = f"/api-route-{uuid.uuid4().hex[:8]}"
        api_route = cleanup.route(oagw_headers, await create_route(
            client, oagw_base_url, oagw_headers, REGISTRY_UPSTREAM_ID, ["GET"], path,
        ))
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/proxy/{REGISTRY_ALIAS}{path}", headers=oagw_headers,
        )
        assert resp.status_code == 404, resp.text[:300]
        assert resp.headers.get("x-oagw-error-source") == "upstream"

        resp = await client.delete(
            f"{oagw_base_url}/oagw/v1/routes/{api_route['id']}", headers=oagw_headers,
        )
        assert resp.status_code == 204, resp.text[:300]
        resp = await client.get(
            f"{oagw_base_url}/oagw/v1/routes/{api_route['id']}", headers=oagw_headers,
        )
        assert_problem(resp, 404, esrc=None, category="not_found")
        routes = await list_all(
            client, oagw_base_url, oagw_headers, "routes", upstream_id=REGISTRY_UPSTREAM_ID,
        )
        ids = [r["id"] for r in routes]
        assert REGISTRY_ROUTE_ID in ids and api_route["id"] not in ids
