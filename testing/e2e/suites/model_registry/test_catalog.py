"""Model Registry E2E: sharing, shadowing and tenant isolation (MR-T*).

Roles from the PRD share-and-shadow use case: hierarchy-root is Root, l1a the
Partner or Customer, l1b the other Partner, l2b the Team below l1a.
"""
import pytest

from .conftest import HIERARCHY_L1A, HIERARCHY_ROOT, model_body
from .helpers import (
    MODEL_RESOURCE,
    PROVIDER_RESOURCE,
    assert_problem,
    canonical_segment,
    collect,
    etag_of,
)

ANY = {"If-Match": "*"}


@pytest.fixture
def resolve(client, available_url):
    async def _resolve(headers: dict, canonical_id: str):
        return await client.get(
            f"{available_url}/{canonical_segment(canonical_id)}", headers=headers
        )

    return _resolve


@pytest.fixture
def root_catalog(root_headers, create_provider, create_model):
    """Root's shared provider with two shared models and one tenant-mode model."""

    async def _build() -> dict:
        provider = await create_provider(root_headers, sharing_mode="shared")
        pid = provider["id"]
        return {
            "provider": provider,
            "slug": provider["slug"],
            "gpt-4o": await create_model(root_headers, pid, "gpt-4o", sharing_mode="shared"),
            "gpt-4o-mini": await create_model(root_headers, pid, "gpt-4o-mini", sharing_mode="shared"),
            "o1-preview": await create_model(root_headers, pid, "o1-preview"),
        }

    return _build


async def assert_resolves(resolve, headers, canonical_id, model):
    resp = await resolve(headers, canonical_id)
    assert resp.status_code == 200, resp.text
    assert resp.json()["id"] == model["id"], f"{canonical_id} resolved to another tenant's model"


async def assert_model_not_found(resolve, headers, canonical_id):
    resp = await resolve(headers, canonical_id)
    assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)


class TestSharing:
    async def test_tenant_mode_stays_with_the_owner(
        self,
        client,
        providers_url,
        models_url,
        root_headers,
        l1a_headers,
        l1b_headers,
        create_provider,
        create_model,
        resolve,
    ):
        """MR-T01: tenant-mode entries are invisible to children, siblings and parents."""
        provider = await create_provider(l1a_headers)
        model = await create_model(l1a_headers, provider["id"], sharing_mode="shared")
        slug_filter = {"$filter": f"slug eq '{provider['slug']}'"}

        for headers in (root_headers, l1b_headers):
            resp = await client.get(f"{providers_url}/{provider['id']}", headers=headers)
            assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
            resp = await client.get(f"{models_url}/{model['id']}", headers=headers)
            assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)
            assert await collect(client, providers_url, headers, slug_filter) == []
            await assert_model_not_found(resolve, headers, model["canonical_id"])

        await assert_resolves(resolve, l1a_headers, model["canonical_id"], model)

    async def test_parent_tenant_mode_is_invisible_to_child(
        self, client, providers_url, root_headers, l1a_headers, create_provider, create_model, resolve
    ):
        """MR-T01: a parent's tenant-mode provider hides even its shared models."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"], sharing_mode="shared")

        resp = await client.get(f"{providers_url}/{provider['id']}", headers=l1a_headers)
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
        await assert_model_not_found(resolve, l1a_headers, model["canonical_id"])

    @pytest.mark.smoke
    async def test_shared_entries_are_inherited_in_full(
        self,
        client,
        providers_url,
        models_url,
        root_headers,
        l1a_headers,
        l2b_headers,
        create_provider,
        create_model,
        resolve,
    ):
        """MR-T02: descendants see a shared provider and model as full inherited records."""
        provider = await create_provider(root_headers, sharing_mode="shared")
        model = await create_model(
            root_headers,
            provider["id"],
            sharing_mode="shared",
            cost={"input_per_1m_micro": 7},
            provider_settings={"deployment": "prod"},
            base_url="https://llm.example.com/v1",
        )

        for headers in (l1a_headers, l2b_headers):
            resp = await client.get(f"{providers_url}/{provider['id']}", headers=headers)
            assert resp.status_code == 200, resp.text
            entry = resp.json()
            assert entry["tenant_id"] == HIERARCHY_ROOT
            assert entry["inherited"] is True
            assert entry["shadowed"] is False
            assert entry["available_for_eval"] is True

            resp = await client.get(f"{models_url}/{model['id']}", headers=headers)
            assert resp.status_code == 200, resp.text
            entry = resp.json()
            assert entry["inherited"] is True
            assert entry["shadowed"] is False
            assert entry["available_for_eval"] is True
            assert entry["info"]["cost"] == {"input_per_1m_micro": 7}
            assert entry["info"]["provider_settings"] == {"deployment": "prod"}
            assert entry["info"]["base_url"] == "https://llm.example.com/v1"

            listed = await collect(
                client, models_url, headers, {"$filter": f"provider_id eq {provider['id']}"}
            )
            assert [m["id"] for m in listed] == [model["id"]]

            await assert_resolves(resolve, headers, model["canonical_id"], model)

        # The owner sees its own row, not an inherited one.
        resp = await client.get(f"{models_url}/{model['id']}", headers=root_headers)
        assert resp.json()["inherited"] is False

    async def test_model_needs_both_levels_shared(
        self, client, providers_url, models_url, root_headers, l1a_headers, root_catalog, resolve
    ):
        """MR-T03: a tenant-mode model of a shared provider is not inherited."""
        cat = await root_catalog()

        resp = await client.get(f"{providers_url}/{cat['provider']['id']}", headers=l1a_headers)
        assert resp.status_code == 200, resp.text
        resp = await client.get(f"{models_url}/{cat['o1-preview']['id']}", headers=l1a_headers)
        assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)

        listed = await collect(
            client, models_url, l1a_headers, {"$filter": f"provider_id eq {cat['provider']['id']}"}
        )
        assert sorted(m["name"] for m in listed) == ["gpt-4o", "gpt-4o-mini"]

        await assert_model_not_found(resolve, l1a_headers, f"{cat['slug']}::o1-preview")
        await assert_resolves(resolve, root_headers, f"{cat['slug']}::o1-preview", cat["o1-preview"])

    async def test_sharing_changes_apply_to_the_next_request(
        self, client, providers_url, models_url, root_headers, l1a_headers, root_catalog, resolve
    ):
        """MR-T10: un-sharing a model or provider removes it from descendants at once."""
        cat = await root_catalog()
        slug = cat["slug"]
        await assert_resolves(resolve, l1a_headers, f"{slug}::gpt-4o", cat["gpt-4o"])

        resp = await client.patch(
            f"{models_url}/{cat['gpt-4o']['id']}",
            headers={**root_headers, **ANY},
            json={"sharing_mode": "tenant"},
        )
        assert resp.status_code == 200, resp.text
        await assert_model_not_found(resolve, l1a_headers, f"{slug}::gpt-4o")
        await assert_resolves(resolve, l1a_headers, f"{slug}::gpt-4o-mini", cat["gpt-4o-mini"])

        resp = await client.patch(
            f"{providers_url}/{cat['provider']['id']}",
            headers={**root_headers, **ANY},
            json={"sharing_mode": "tenant"},
        )
        assert resp.status_code == 200, resp.text
        await assert_model_not_found(resolve, l1a_headers, f"{slug}::gpt-4o-mini")
        resp = await client.get(f"{providers_url}/{cat['provider']['id']}", headers=l1a_headers)
        assert resp.status_code == 404, resp.text

        # The owner is unaffected.
        await assert_resolves(resolve, root_headers, f"{slug}::gpt-4o", cat["gpt-4o"])


class TestInheritedEntriesAreReadOnly:
    async def test_writes_on_inherited_entries_are_403(
        self, client, providers_url, models_url, root_headers, l1a_headers, root_catalog
    ):
        """MR-T04: update, delete and create-under on inherited entries are INHERITED_READ_ONLY."""
        cat = await root_catalog()
        provider_url = f"{providers_url}/{cat['provider']['id']}"
        model_url = f"{models_url}/{cat['gpt-4o']['id']}"
        child = {**l1a_headers, **ANY}

        resp = await client.patch(provider_url, headers=child, json={"enabled": False})
        assert_problem(resp, 403, "permission_denied", reason="INHERITED_READ_ONLY")
        resp = await client.delete(provider_url, headers=child)
        assert_problem(resp, 403, "permission_denied", reason="INHERITED_READ_ONLY")
        resp = await client.patch(model_url, headers=child, json={"display_name": "Mine now"})
        assert_problem(resp, 403, "permission_denied", reason="INHERITED_READ_ONLY")
        resp = await client.delete(model_url, headers=child)
        assert_problem(resp, 403, "permission_denied", reason="INHERITED_READ_ONLY")
        resp = await client.post(
            models_url, headers=l1a_headers, json=model_body(cat["provider"]["id"], "child-model")
        )
        assert_problem(resp, 403, "permission_denied", reason="INHERITED_READ_ONLY")

        resp = await client.get(provider_url, headers=root_headers)
        assert etag_of(resp) == '"1"'
        assert resp.json()["enabled"] is True
        resp = await client.get(model_url, headers=root_headers)
        assert etag_of(resp) == '"1"'
        listed = await collect(
            client, models_url, root_headers, {"$filter": f"provider_id eq {cat['provider']['id']}"}
        )
        assert len(listed) == 3

    async def test_descendant_cannot_change_approval(
        self, client, models_url, l1a_headers, root_catalog, resolve
    ):
        """MR-T12: a descendant cannot revoke an inherited model."""
        cat = await root_catalog()
        resp = await client.patch(
            f"{models_url}/{cat['gpt-4o']['id']}",
            headers={**l1a_headers, **ANY},
            json={"approval_state": "revoked"},
        )
        assert_problem(resp, 403, "permission_denied", reason="INHERITED_READ_ONLY")
        await assert_resolves(resolve, l1a_headers, f"{cat['slug']}::gpt-4o", cat["gpt-4o"])

    async def test_writes_on_invisible_entries_are_404(
        self, client, providers_url, models_url, root_headers, l1b_headers, create_provider, create_model
    ):
        """MR-T05: a write on another tenant's invisible entry is not found."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])
        other = {**l1b_headers, **ANY}

        resp = await client.patch(f"{providers_url}/{provider['id']}", headers=other, json={"name": "x"})
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
        resp = await client.delete(f"{providers_url}/{provider['id']}", headers=other)
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
        resp = await client.patch(f"{models_url}/{model['id']}", headers=other, json={"display_name": "x"})
        assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)
        resp = await client.delete(f"{models_url}/{model['id']}", headers=other)
        assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)
        resp = await client.post(
            models_url, headers=l1b_headers, json=model_body(provider["id"], "intruder")
        )
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)

        resp = await client.get(f"{providers_url}/{provider['id']}", headers=root_headers)
        assert etag_of(resp) == '"1"'


class TestShadowing:
    async def test_tenant_mode_shadow_affects_the_owner_only(
        self,
        client,
        providers_url,
        models_url,
        root_headers,
        l1a_headers,
        l1b_headers,
        l2b_headers,
        create_provider,
        create_model,
        root_catalog,
        resolve,
    ):
        """MR-T06: a tenant-mode provider with an inherited slug shadows it for its owner."""
        cat = await root_catalog()
        slug = cat["slug"]
        own = await create_provider(l1a_headers, slug=slug)
        own_model = await create_model(l1a_headers, own["id"], "gpt-4o")

        # Partner: own provider wins; Root's models under the slug are gone.
        await assert_resolves(resolve, l1a_headers, f"{slug}::gpt-4o", own_model)
        await assert_model_not_found(resolve, l1a_headers, f"{slug}::gpt-4o-mini")

        entries = {
            p["id"]: p
            for p in await collect(client, providers_url, l1a_headers, {"$filter": f"slug eq '{slug}'"})
        }
        assert set(entries) == {own["id"], cat["provider"]["id"]}
        assert entries[own["id"]]["inherited"] is False
        assert entries[own["id"]]["shadowed"] is False
        assert entries[own["id"]]["available_for_eval"] is True
        shadowed = entries[cat["provider"]["id"]]
        assert shadowed["inherited"] is True
        assert shadowed["shadowed"] is True
        assert shadowed["available_for_eval"] is False

        resp = await client.get(f"{models_url}/{cat['gpt-4o-mini']['id']}", headers=l1a_headers)
        assert resp.status_code == 200, resp.text
        entry = resp.json()
        assert entry["inherited"] is True
        assert entry["shadowed"] is True
        assert entry["available_for_eval"] is False

        # Team below the Partner and the other Partner keep Root's catalog.
        for headers in (l2b_headers, l1b_headers):
            await assert_resolves(resolve, headers, f"{slug}::gpt-4o", cat["gpt-4o"])
            await assert_resolves(resolve, headers, f"{slug}::gpt-4o-mini", cat["gpt-4o-mini"])
            await assert_model_not_found(resolve, headers, f"{slug}::o1-preview")

        # Root is unaffected.
        await assert_resolves(resolve, root_headers, f"{slug}::gpt-4o", cat["gpt-4o"])
        await assert_resolves(resolve, root_headers, f"{slug}::o1-preview", cat["o1-preview"])

    async def test_shared_shadow_covers_the_subtree(
        self,
        l1a_headers,
        l1b_headers,
        l2b_headers,
        create_provider,
        create_model,
        root_catalog,
        resolve,
    ):
        """MR-T07: a shared shadowing provider replaces the slug for its subtree."""
        cat = await root_catalog()
        slug = cat["slug"]
        own = await create_provider(l1a_headers, slug=slug, sharing_mode="shared")
        own_model = await create_model(l1a_headers, own["id"], "gpt-4o", sharing_mode="shared")

        for headers in (l1a_headers, l2b_headers):
            await assert_resolves(resolve, headers, f"{slug}::gpt-4o", own_model)
            await assert_model_not_found(resolve, headers, f"{slug}::gpt-4o-mini")

        await assert_resolves(resolve, l1b_headers, f"{slug}::gpt-4o", cat["gpt-4o"])
        await assert_resolves(resolve, l1b_headers, f"{slug}::gpt-4o-mini", cat["gpt-4o-mini"])

    async def test_disabled_shared_shadow_blocks_the_slug(
        self,
        l1a_headers,
        l1b_headers,
        l2b_headers,
        create_provider,
        create_model,
        root_catalog,
        resolve,
    ):
        """MR-T08: a shared, disabled shadowing provider blocks the slug in its subtree."""
        cat = await root_catalog()
        slug = cat["slug"]
        own = await create_provider(l1a_headers, slug=slug, sharing_mode="shared", enabled=False)
        await create_model(l1a_headers, own["id"], "gpt-4o", sharing_mode="shared")

        for headers in (l1a_headers, l2b_headers):
            resp = await resolve(headers, f"{slug}::gpt-4o")
            assert_problem(resp, 403, "permission_denied", reason="PROVIDER_DISABLED")
            await assert_model_not_found(resolve, headers, f"{slug}::gpt-4o-mini")

        await assert_resolves(resolve, l1b_headers, f"{slug}::gpt-4o", cat["gpt-4o"])

    async def test_deleting_the_shadow_restores_inheritance(
        self,
        client,
        providers_url,
        l1a_headers,
        l2b_headers,
        create_provider,
        create_model,
        root_catalog,
        resolve,
    ):
        """MR-T09: deleting a shadowing provider restores the inherited provider and models."""
        cat = await root_catalog()
        slug = cat["slug"]
        own = await create_provider(l1a_headers, slug=slug, sharing_mode="shared")
        own_model = await create_model(l1a_headers, own["id"], "gpt-4o", sharing_mode="shared")
        await assert_resolves(resolve, l2b_headers, f"{slug}::gpt-4o", own_model)

        resp = await client.delete(f"{providers_url}/{own['id']}", headers=l1a_headers)
        assert resp.status_code == 204, resp.text

        for headers in (l1a_headers, l2b_headers):
            await assert_resolves(resolve, headers, f"{slug}::gpt-4o", cat["gpt-4o"])
            await assert_resolves(resolve, headers, f"{slug}::gpt-4o-mini", cat["gpt-4o-mini"])

        resp = await client.get(f"{providers_url}/{cat['provider']['id']}", headers=l1a_headers)
        assert resp.json()["shadowed"] is False
        assert resp.json()["available_for_eval"] is True


class TestInheritedModels:
    async def test_unapproved_parent_model_leaves_the_children(
        self,
        client,
        models_url,
        available_url,
        root_headers,
        l1a_headers,
        l2b_headers,
        root_catalog,
        resolve,
    ):
        """MR-T13: a model the parent revokes stops being available to descendants."""
        cat = await root_catalog()
        canonical_id = f"{cat['slug']}::gpt-4o"
        model_url = f"{models_url}/{cat['gpt-4o']['id']}"
        of_provider = {"$filter": f"provider_id eq {cat['provider']['id']}"}

        for headers in (l1a_headers, l2b_headers):
            await assert_resolves(resolve, headers, canonical_id, cat["gpt-4o"])
            listed = await collect(client, available_url, headers, of_provider)
            assert sorted(m["name"] for m in listed) == ["gpt-4o", "gpt-4o-mini"]

        resp = await client.patch(
            model_url, headers={**root_headers, **ANY}, json={"approval_state": "revoked"}
        )
        assert resp.status_code == 200, resp.text

        for headers in (l1a_headers, l2b_headers):
            resp = await resolve(headers, canonical_id)
            assert_problem(resp, 403, "permission_denied", reason="MODEL_NOT_APPROVED")
            listed = await collect(client, available_url, headers, of_provider)
            assert [m["name"] for m in listed] == ["gpt-4o-mini"]

            # The management read still shows the inherited model and why it is out.
            resp = await client.get(model_url, headers=headers)
            assert resp.status_code == 200, resp.text
            entry = resp.json()
            assert entry["inherited"] is True
            assert entry["approval"]["state"] == "revoked"
            assert entry["available_for_eval"] is False

        resp = await client.patch(
            model_url, headers={**root_headers, **ANY}, json={"approval_state": "approved"}
        )
        assert resp.status_code == 200, resp.text
        for headers in (l1a_headers, l2b_headers):
            await assert_resolves(resolve, headers, canonical_id, cat["gpt-4o"])

    async def test_parent_model_created_unapproved_never_reaches_children(
        self, client, available_url, root_headers, l1a_headers, create_provider, create_model, resolve
    ):
        """MR-T13: a shared model created as pending is not available to descendants."""
        provider = await create_provider(root_headers, sharing_mode="shared")
        model = await create_model(
            root_headers, provider["id"], sharing_mode="shared", approval_state="pending"
        )

        resp = await resolve(l1a_headers, model["canonical_id"])
        assert_problem(resp, 403, "permission_denied", reason="MODEL_NOT_APPROVED")
        listed = await collect(
            client, available_url, l1a_headers, {"$filter": f"provider_id eq {provider['id']}"}
        )
        assert listed == []

    async def test_models_of_a_shadowing_provider_replace_the_inherited_ones(
        self,
        client,
        available_url,
        l1a_headers,
        l1b_headers,
        create_provider,
        create_model,
        root_catalog,
        resolve,
    ):
        """MR-T14: the shadowing provider's models are the slug's whole catalog for its owner."""
        cat = await root_catalog()
        slug = cat["slug"]
        inherited = {"$filter": f"provider_id eq {cat['provider']['id']}"}

        # An empty shadowing provider already hides every inherited model of the slug.
        own = await create_provider(l1a_headers, slug=slug)
        owned = {"$filter": f"provider_id eq {own['id']}"}
        await assert_model_not_found(resolve, l1a_headers, f"{slug}::gpt-4o")
        await assert_model_not_found(resolve, l1a_headers, f"{slug}::gpt-4o-mini")
        assert await collect(client, available_url, l1a_headers, inherited) == []
        assert await collect(client, available_url, l1a_headers, owned) == []

        # A model with an inherited name resolves to the override, with its own content.
        override = await create_model(
            l1a_headers,
            own["id"],
            "gpt-4o",
            provider_model_id="partner-gpt-4o",
            cost={"input_per_1m_micro": 42},
            provider_settings={"deployment": "partner"},
        )
        resp = await resolve(l1a_headers, f"{slug}::gpt-4o")
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["id"] == override["id"]
        assert body["tenant_id"] == HIERARCHY_L1A
        assert body["provider_id"] == own["id"]
        assert body["info"]["provider_model_id"] == "partner-gpt-4o"
        assert body["info"]["cost"] == {"input_per_1m_micro": 42}
        assert body["info"]["provider_settings"] == {"deployment": "partner"}

        # A model with a new name is added; inherited names without an override stay hidden.
        extra = await create_model(l1a_headers, own["id"], "partner-only")
        await assert_resolves(resolve, l1a_headers, f"{slug}::partner-only", extra)
        await assert_model_not_found(resolve, l1a_headers, f"{slug}::gpt-4o-mini")

        listed = await collect(client, available_url, l1a_headers, owned)
        assert {m["id"] for m in listed} == {override["id"], extra["id"]}
        assert await collect(client, available_url, l1a_headers, inherited) == []

        # The other partner still resolves Root's models.
        resp = await resolve(l1b_headers, f"{slug}::gpt-4o")
        assert resp.status_code == 200, resp.text
        assert resp.json()["id"] == cat["gpt-4o"]["id"]
        assert resp.json()["tenant_id"] == HIERARCHY_ROOT
        await assert_model_not_found(resolve, l1b_headers, f"{slug}::partner-only")


class TestSelfManagedBarrier:
    async def test_inheritance_stops_at_a_self_managed_tenant(
        self,
        client,
        providers_url,
        models_url,
        root_headers,
        sm_headers,
        sm_child_headers,
        create_provider,
        create_model,
        root_catalog,
        resolve,
    ):
        """MR-T11: a self-managed tenant inherits nothing; its subtree inherits from it only."""
        cat = await root_catalog()
        slug = cat["slug"]

        for headers in (sm_headers, sm_child_headers):
            await assert_model_not_found(resolve, headers, f"{slug}::gpt-4o")
            resp = await client.get(f"{providers_url}/{cat['provider']['id']}", headers=headers)
            assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
            listed = await collect(client, providers_url, headers, {"$filter": f"slug eq '{slug}'"})
            assert listed == []

        own = await create_provider(sm_headers, sharing_mode="shared")
        own_model = await create_model(sm_headers, own["id"], "local", sharing_mode="shared")

        await assert_resolves(resolve, sm_child_headers, own_model["canonical_id"], own_model)
        resp = await client.get(f"{models_url}/{own_model['id']}", headers=sm_child_headers)
        assert resp.status_code == 200, resp.text
        assert resp.json()["inherited"] is True

        # The ancestor does not see into the self-managed subtree.
        resp = await client.get(f"{providers_url}/{own['id']}", headers=root_headers)
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
        await assert_model_not_found(resolve, root_headers, own_model["canonical_id"])


class TestTenantIsolation:
    async def test_filter_cannot_widen_visibility(
        self, client, providers_url, models_url, l1a_headers, l1b_headers, create_provider, create_model
    ):
        """MR-L07: a filter naming another tenant returns nothing of that tenant."""
        provider = await create_provider(l1a_headers, sharing_mode="shared")
        await create_model(l1a_headers, provider["id"], sharing_mode="shared")

        providers = await collect(
            client, providers_url, l1b_headers, {"$filter": f"tenant_id eq {HIERARCHY_L1A}"}
        )
        assert providers == []
        models = await collect(
            client, models_url, l1b_headers, {"$filter": f"tenant_id eq {HIERARCHY_L1A}"}
        )
        assert models == []

        every = await collect(client, providers_url, l1b_headers)
        assert provider["id"] not in {p["id"] for p in every}
