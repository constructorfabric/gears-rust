"""E2E seam tests for ``GET /tenants/{id}/children?recursive=true``.

Behaviour (visible set, barrier carve-out, ancestor chains, cursor walk)
is pinned by the Rust suites ``list_descendants_integration.rs`` (SQLite),
``list_descendants_integration_pg.rs`` (Postgres) and ``api_children_test.rs``
(in-process router). This file pins only what those cannot see, per
``docs/toolkit_unified_system/13_e2e_testing.md``:

* R1 -- recursive_listing_wire_shape: ``ancestors`` crosses the JSON
        boundary with the documented keys and order, and is ABSENT (not
        ``null``) without the flag -- the serde attributes on the real
        server binary, through the gateway.
* R2 -- recursive_name_filter_through_url: ``$filter=contains(name,...)``
        survives URL encoding into the recursive SQL path.
* R3 -- cursor_bound_to_mode_over_http: a ``next_cursor`` minted in one
        mode, sent back through a real URL, is rejected in the other mode
        with a ``problem+json`` 400 and accepted in its own.
* R4 -- invalid_recursive_flag_is_problem_json: the flag parser's 400
        leaves the gateway as a canonical Problem envelope.
* R5 -- self_managed_descendant_listed_through_real_scope: the real
        static-authz policy bundle's barrier-respecting scope drives the
        recursive branch, and a self-managed tenant two levels down still
        surfaces through the direct-child carve-out. (Rows BELOW such a
        tenant cannot be seeded through the API as this caller, so their
        exclusion is pinned by the Rust suites, not here.)
"""

import httpx

from .conftest import (
    DEFAULT_TENANT_TYPE,
    REQUEST_TIMEOUT,
    _children,
)

ANCESTOR_KEYS = {"id", "name", "tenant_type"}


async def _seed_three_levels(create_tenant) -> tuple[dict, dict, dict]:
    """``parent -> child -> grandchild`` under the caller's root tenant."""
    parent = await create_tenant("rch-parent")
    child = await create_tenant("rch-child", parent_id=parent["id"])
    grandchild = await create_tenant("rch-grandchild", parent_id=child["id"])
    return parent, child, grandchild


def _by_id(body: dict) -> dict[str, dict]:
    return {item["id"]: item for item in body["items"]}


async def test_recursive_listing_wire_shape(am_base_url, am_headers, create_tenant):
    parent, child, grandchild = await _seed_three_levels(create_tenant)

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(
            _children(am_base_url, parent["id"]),
            headers=am_headers,
            params={"recursive": "true"},
        )
        assert r.status_code == 200, f"recursive listing: {r.status_code} {r.text}"
        items = _by_id(r.json())
        assert set(items) == {child["id"], grandchild["id"]}, items

        assert items[child["id"]]["ancestors"] == [], (
            "a direct child carries an empty chain in recursive mode"
        )
        chain = items[grandchild["id"]]["ancestors"]
        assert [a["id"] for a in chain] == [child["id"]], chain
        assert set(chain[0]) <= ANCESTOR_KEYS and {"id", "name"} <= set(chain[0]), chain[0]
        assert chain[0]["name"] == child["name"]
        assert chain[0]["tenant_type"] == DEFAULT_TENANT_TYPE

        r = await c.get(_children(am_base_url, parent["id"]), headers=am_headers)
        assert r.status_code == 200, f"direct listing: {r.status_code} {r.text}"
        items = r.json()["items"]
        assert [i["id"] for i in items] == [child["id"]]
        assert "ancestors" not in items[0], (
            f"the key must be absent without the flag, not null: {items[0]}"
        )


async def test_recursive_name_filter_through_url(am_base_url, am_headers, create_tenant):
    parent, child, grandchild = await _seed_three_levels(create_tenant)
    # `unique_name` suffixes a run-unique counter; the grandchild's full
    # name matches exactly one tenant anywhere in the store.
    needle = grandchild["name"]

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(
            _children(am_base_url, parent["id"]),
            headers=am_headers,
            params={"recursive": "true", "$filter": f"contains(name,'{needle}')"},
        )
        assert r.status_code == 200, f"filtered listing: {r.status_code} {r.text}"
        items = r.json()["items"]
        assert [i["id"] for i in items] == [grandchild["id"]], items
        assert [a["id"] for a in items[0]["ancestors"]] == [child["id"]]


async def test_cursor_bound_to_mode_over_http(am_base_url, am_headers, create_tenant):
    parent, _child, _grandchild = await _seed_three_levels(create_tenant)
    await create_tenant("rch-sibling", parent_id=parent["id"])
    url = _children(am_base_url, parent["id"])

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(url, headers=am_headers, params={"recursive": "true", "limit": 1})
        assert r.status_code == 200, r.text
        recursive_cursor = r.json()["page_info"]["next_cursor"]
        assert recursive_cursor, "three descendants at limit=1 leave more pages"

        r = await c.get(url, headers=am_headers, params={"limit": 1})
        assert r.status_code == 200, r.text
        direct_cursor = r.json()["page_info"]["next_cursor"]
        assert direct_cursor, "two direct children at limit=1 leave more pages"

        same = await c.get(
            url,
            headers=am_headers,
            params={"recursive": "true", "limit": 1, "cursor": recursive_cursor},
        )
        assert same.status_code == 200, f"same-mode cursor: {same.status_code} {same.text}"

        for params in (
            {"limit": 1, "cursor": recursive_cursor},
            {"recursive": "true", "limit": 1, "cursor": direct_cursor},
        ):
            cross = await c.get(url, headers=am_headers, params=params)
            assert cross.status_code == 400, (
                f"cross-mode cursor {params}: {cross.status_code} {cross.text}"
            )
            assert cross.headers["content-type"].startswith("application/problem+json")


async def test_invalid_recursive_flag_is_problem_json(am_base_url, am_headers, create_tenant):
    parent = await create_tenant("rch-flag")

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(
            _children(am_base_url, parent["id"]),
            headers=am_headers,
            params={"recursive": "True"},
        )
        assert r.status_code == 400, f"recursive=True: {r.status_code} {r.text}"
        assert r.headers["content-type"].startswith("application/problem+json")
        assert "recursive" in r.text


async def test_self_managed_descendant_listed_through_real_scope(am_base_url, am_headers, create_tenant):
    parent = await create_tenant("rch-sm-parent")
    child = await create_tenant("rch-sm-child", parent_id=parent["id"])
    self_managed = await create_tenant(
        "rch-sm-barrier", parent_id=child["id"], self_managed=True
    )

    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        r = await c.get(
            _children(am_base_url, parent["id"]),
            headers=am_headers,
            params={"recursive": "true"},
        )
        assert r.status_code == 200, r.text
        items = _by_id(r.json())
        assert set(items) == {child["id"], self_managed["id"]}, items
        barrier = items[self_managed["id"]]
        assert barrier["self_managed"] is True
        assert [a["id"] for a in barrier["ancestors"]] == [child["id"]]
