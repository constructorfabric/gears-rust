"""Model Registry E2E: OData listing and pagination (MR-L*)."""
import uuid

import pytest

from .helpers import assert_problem, collect


@pytest.fixture
async def five_models(root_headers, create_provider, create_model):
    """A provider with five models; returns (provider, models by name)."""
    provider = await create_provider(root_headers)
    models = {}
    for name, fields in {
        "delta": {"family": "x", "sort_order": 1},
        "alpha": {"family": "x", "sort_order": 5, "approval_state": "pending"},
        "echo": {"family": "y", "sort_order": 3, "sharing_mode": "shared"},
        "charlie": {"sort_order": 2},
        "bravo": {"family": "y", "sort_order": 4, "approval_state": "pending"},
    }.items():
        models[name] = await create_model(root_headers, provider["id"], name, **fields)
    return provider, models


def by_provider(provider: dict, extra: str = "") -> str:
    return f"provider_id eq {provider['id']}{extra}"


class TestDefaults:
    async def test_models_default_order_and_page_size(self, client, models_url, root_headers, five_models):
        """MR-L01: models list by canonical_id ascending with a default page of 20."""
        provider, _ = five_models
        resp = await client.get(models_url, headers=root_headers, params={"$filter": by_provider(provider)})
        assert resp.status_code == 200, resp.text
        page = resp.json()
        assert [m["name"] for m in page["items"]] == ["alpha", "bravo", "charlie", "delta", "echo"]
        assert page["page_info"]["limit"] == 20
        assert not page["page_info"].get("next_cursor")

    async def test_providers_default_order(self, client, providers_url, root_headers, create_provider):
        """MR-L01: providers list by slug ascending."""
        tag = uuid.uuid4().hex[:12]
        name = f"order-{tag}"
        for suffix in ("c", "a", "b"):
            await create_provider(root_headers, slug=f"e2e-{tag}-{suffix}", name=name)

        resp = await client.get(providers_url, headers=root_headers, params={"$filter": f"name eq '{name}'"})
        assert resp.status_code == 200, resp.text
        page = resp.json()
        assert [p["slug"][-1] for p in page["items"]] == ["a", "b", "c"]
        assert page["page_info"]["limit"] == 20


class TestPagination:
    @pytest.mark.smoke
    @pytest.mark.parametrize("endpoint", ["models", "available"])
    async def test_cursor_walk_has_no_gaps_or_duplicates(
        self, client, models_url, available_url, root_headers, create_provider, create_model, endpoint
    ):
        """MR-L02: keyset pages cover every row once, in order."""
        provider = await create_provider(root_headers)
        names = [f"m{i}" for i in range(5)]
        for name in reversed(names):
            await create_model(root_headers, provider["id"], name)
        url = models_url if endpoint == "models" else available_url

        seen: list = []
        params = {"$filter": by_provider(provider), "limit": 2}
        for _ in range(10):
            resp = await client.get(url, headers=root_headers, params=params)
            assert resp.status_code == 200, resp.text
            page = resp.json()
            assert len(page["items"]) <= 2
            assert page["page_info"]["limit"] == 2
            seen.extend(m["name"] for m in page["items"])
            cursor = page["page_info"].get("next_cursor")
            if not cursor:
                break
            params = {"$filter": by_provider(provider), "limit": 2, "cursor": cursor}
        assert seen == names

    async def test_top_and_skiptoken_aliases(self, client, models_url, root_headers, five_models):
        """MR-L02: `$top` and `$skiptoken` are aliases of `limit` and `cursor`."""
        provider, _ = five_models
        resp = await client.get(
            models_url, headers=root_headers, params={"$filter": by_provider(provider), "$top": 3}
        )
        assert resp.status_code == 200, resp.text
        page = resp.json()
        assert [m["name"] for m in page["items"]] == ["alpha", "bravo", "charlie"]

        resp = await client.get(
            models_url,
            headers=root_headers,
            params={
                "$filter": by_provider(provider),
                "$top": 3,
                "$skiptoken": page["page_info"]["next_cursor"],
            },
        )
        assert resp.status_code == 200, resp.text
        assert [m["name"] for m in resp.json()["items"]] == ["delta", "echo"]


class TestFilterAndOrder:
    @pytest.mark.parametrize(
        "clause,expected",
        [
            (" and approval_state eq 'pending'", ["alpha", "bravo"]),
            (" and family eq 'y'", ["bravo", "echo"]),
            (" and sharing_mode eq 'shared'", ["echo"]),
            (" and name eq 'charlie'", ["charlie"]),
            (" and sort_order gt 3", ["alpha", "bravo"]),
            (" and api_completion eq true", ["alpha", "bravo", "charlie", "delta", "echo"]),
            (" and api_embedding eq true", []),
        ],
    )
    async def test_model_filters(self, client, models_url, root_headers, five_models, clause, expected):
        """MR-L03: documented model fields filter the list."""
        provider, _ = five_models
        listed = await collect(client, models_url, root_headers, {"$filter": by_provider(provider, clause)})
        assert [m["name"] for m in listed] == expected

    async def test_provider_filters(self, client, providers_url, root_headers, create_provider):
        """MR-L03: documented provider fields filter the list."""
        name = f"filter-{uuid.uuid4().hex[:12]}"
        on = await create_provider(root_headers, name=name)
        off = await create_provider(root_headers, name=name, enabled=False, sharing_mode="shared")

        async def ids(clause: str) -> set:
            listed = await collect(
                client, providers_url, root_headers, {"$filter": f"name eq '{name}'{clause}"}
            )
            return {p["id"] for p in listed}

        assert await ids("") == {on["id"], off["id"]}
        assert await ids(" and enabled eq false") == {off["id"]}
        assert await ids(" and sharing_mode eq 'tenant'") == {on["id"]}
        assert await ids(f" and id eq {on['id']}") == {on["id"]}

    @pytest.mark.parametrize(
        "orderby,expected",
        [
            ("name desc", ["echo", "delta", "charlie", "bravo", "alpha"]),
            ("sort_order asc", ["delta", "charlie", "echo", "bravo", "alpha"]),
            ("approval_state desc,name desc", ["bravo", "alpha", "echo", "delta", "charlie"]),
        ],
    )
    async def test_model_orderby(self, client, models_url, root_headers, five_models, orderby, expected):
        """MR-L04: non-nullable fields order the list."""
        provider, _ = five_models
        listed = await collect(
            client, models_url, root_headers, {"$filter": by_provider(provider), "$orderby": orderby}
        )
        assert [m["name"] for m in listed] == expected

    @pytest.mark.parametrize("field", ["family", "vendor", "region", "last_release_at", "max_output_tokens"])
    async def test_orderby_on_nullable_field_is_rejected(self, client, models_url, root_headers, field):
        """MR-L04: a filter-only field cannot order the list."""
        resp = await client.get(models_url, headers=root_headers, params={"$orderby": field})
        assert_problem(resp, 400, "invalid_argument")
        assert resp.json()["context"]["field_violations"], resp.text

    @pytest.mark.parametrize(
        "expr", ["inherited eq true", "available_for_eval eq true", "cost eq 1", "provider_enabled eq true"]
    )
    async def test_filter_on_unlisted_field_is_rejected(self, client, models_url, root_headers, expr):
        """MR-L04: cost and the entry flags are not filterable."""
        resp = await client.get(models_url, headers=root_headers, params={"$filter": expr})
        assert_problem(resp, 400, "invalid_argument")

    async def test_select_projects_the_response(self, client, models_url, root_headers, five_models):
        """MR-L05: `$select` narrows the returned fields."""
        provider, _ = five_models
        resp = await client.get(
            models_url,
            headers=root_headers,
            params={"$filter": by_provider(provider), "$select": "id,canonical_id"},
        )
        assert resp.status_code == 200, resp.text
        items = resp.json()["items"]
        assert len(items) == 5
        for item in items:
            assert set(item) == {"id", "canonical_id"}, item


class TestInvalidQueries:
    @pytest.mark.parametrize(
        "params",
        [
            {"$top": 0},
            {"$top": 5, "limit": 5},
            {"$skip": 1},
            {"$count": "true"},
            {"cursor": "not-a-cursor"},
            {"$filter": "name eq"},
        ],
    )
    @pytest.mark.parametrize("endpoint", ["providers", "models", "available-models"])
    async def test_bad_query_is_400(self, client, base_url, root_headers, endpoint, params):
        """MR-L06: invalid OData options are invalid_argument problems."""
        resp = await client.get(
            f"{base_url}/model-registry/v1/{endpoint}", headers=root_headers, params=params
        )
        assert_problem(resp, 400, "invalid_argument")

    async def test_orderby_with_cursor_is_400(self, client, models_url, root_headers, five_models):
        """MR-L06: `$orderby` cannot accompany a cursor."""
        provider, _ = five_models
        resp = await client.get(
            models_url, headers=root_headers, params={"$filter": by_provider(provider), "limit": 2}
        )
        cursor = resp.json()["page_info"]["next_cursor"]
        assert cursor

        resp = await client.get(
            models_url,
            headers=root_headers,
            params={"$filter": by_provider(provider), "cursor": cursor, "$orderby": "name desc"},
        )
        assert_problem(resp, 400, "invalid_argument")
