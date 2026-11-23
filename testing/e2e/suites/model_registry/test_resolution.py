"""Model Registry E2E: resolve_model and the available list (MR-R*)."""
import uuid

import httpx
import pytest

from .conftest import HIERARCHY_L1A, HIERARCHY_L2B, HIERARCHY_ROOT, ROOT_SUBJECT
from .helpers import MODEL_RESOURCE, assert_problem, canonical_segment, collect

ANY = {"If-Match": "*"}
ENTRY_FLAGS = ("inherited", "shadowed", "provider_enabled", "available_for_eval")


@pytest.fixture
def resolve(client, available_url):
    async def _resolve(headers: dict, canonical_id: str):
        return await client.get(
            f"{available_url}/{canonical_segment(canonical_id)}", headers=headers
        )

    return _resolve


def family() -> str:
    """A unique `family` label that scopes list assertions to one test."""
    return f"fam-{uuid.uuid4().hex[:12]}"


class TestResolve:
    @pytest.mark.smoke
    async def test_resolve_returns_the_full_model(
        self, client, models_url, root_headers, create_provider, create_model, resolve
    ):
        """MR-R01: a percent-encoded canonical id resolves to the full ModelV1."""
        provider = await create_provider(root_headers)
        model = await create_model(
            root_headers,
            provider["id"],
            "gpt-4.1_mini",
            provider_settings={"deployment": "prod"},
            default_parameters={"temperature": 0.2},
        )

        resp = await resolve(root_headers, f"{provider['slug']}::gpt-4.1_mini")
        assert resp.status_code == 200, resp.text
        assert "%3A%3A" in str(resp.request.url)
        body = resp.json()
        for flag in ENTRY_FLAGS:
            assert flag not in body, f"resolve returned entry flag {flag}"
        assert body["info"]["provider_settings"] == {"deployment": "prod"}
        assert body["info"]["default_parameters"] == {"temperature": 0.2}
        assert body["info"].get("base_url") is None

        # The same record the management read returns, minus the entry flags.
        resp = await client.get(f"{models_url}/{model['id']}", headers=root_headers)
        entry = {k: v for k, v in resp.json().items() if k not in ENTRY_FLAGS}
        assert body == entry

    @pytest.mark.parametrize("suffix", ["::missing", "", "::", "-other::m1"])
    async def test_unknown_or_malformed_id_is_not_found(
        self, root_headers, create_provider, create_model, resolve, suffix
    ):
        """MR-R02: an unknown model, slug or malformed canonical id is model not found."""
        provider = await create_provider(root_headers)
        await create_model(root_headers, provider["id"])

        resp = await resolve(root_headers, f"{provider['slug']}{suffix}")
        assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)

    async def test_disabled_provider_stops_resolution(
        self, client, providers_url, models_url, available_url, root_headers, create_provider, create_model, resolve
    ):
        """MR-R03: disabling a provider is reversible and reported as PROVIDER_DISABLED."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])
        provider_url = f"{providers_url}/{provider['id']}"
        by_provider = {"$filter": f"provider_id eq {provider['id']}"}

        resp = await client.patch(provider_url, headers={**root_headers, **ANY}, json={"enabled": False})
        assert resp.status_code == 200, resp.text

        resp = await resolve(root_headers, model["canonical_id"])
        assert_problem(resp, 403, "permission_denied", reason="PROVIDER_DISABLED")
        assert await collect(client, available_url, root_headers, by_provider) == []

        # The management list still shows the model.
        [entry] = await collect(client, models_url, root_headers, by_provider)
        assert entry["provider_enabled"] is False
        assert entry["available_for_eval"] is False

        resp = await client.patch(provider_url, headers={**root_headers, **ANY}, json={"enabled": True})
        assert resp.status_code == 200, resp.text
        resp = await resolve(root_headers, model["canonical_id"])
        assert resp.status_code == 200, resp.text
        available = await collect(client, available_url, root_headers, by_provider)
        assert [m["id"] for m in available] == [model["id"]]

    async def test_disabled_provider_is_reported_before_approval(
        self, root_headers, create_provider, create_model, resolve
    ):
        """MR-R03: a pending model of a disabled provider reports PROVIDER_DISABLED."""
        provider = await create_provider(root_headers, enabled=False)
        model = await create_model(root_headers, provider["id"], approval_state="pending")

        resp = await resolve(root_headers, model["canonical_id"])
        assert_problem(resp, 403, "permission_denied", reason="PROVIDER_DISABLED")


class TestApproval:
    @pytest.mark.parametrize("state", ["pending", "rejected", "revoked"])
    async def test_only_approved_models_resolve(
        self, client, models_url, available_url, root_headers, create_provider, create_model, resolve, state
    ):
        """MR-R04: a model that is not approved is MODEL_NOT_APPROVED and not listed."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"], approval_state=state)
        assert model["approval"]["state"] == state
        assert model["available_for_eval"] is False

        resp = await resolve(root_headers, model["canonical_id"])
        assert_problem(resp, 403, "permission_denied", reason="MODEL_NOT_APPROVED")
        available = await collect(
            client, available_url, root_headers, {"$filter": f"provider_id eq {provider['id']}"}
        )
        assert available == []

    async def test_approve_then_revoke(
        self, client, models_url, root_headers, create_provider, create_model, resolve
    ):
        """MR-R04: approval takes effect at once, records the decision, and revoke stops it."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"], approval_state="pending")
        url = f"{models_url}/{model['id']}"

        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"approval_state": "approved"})
        assert resp.status_code == 200, resp.text
        approved = resp.json()["approval"]
        assert approved["state"] == "approved"
        assert approved["decided_by"] == ROOT_SUBJECT
        assert approved["decided_at"] > model["approval"]["decided_at"]
        resp = await resolve(root_headers, model["canonical_id"])
        assert resp.status_code == 200, resp.text
        assert resp.json()["approval"] == approved

        # Resending the same state records a new decision.
        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"approval_state": "approved"})
        assert resp.status_code == 200, resp.text
        again = resp.json()["approval"]
        assert again["state"] == "approved"
        assert again["decided_at"] > approved["decided_at"]

        # An update without approval_state keeps the decision.
        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"display_name": "Renamed"})
        assert resp.json()["approval"] == again

        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"approval_state": "revoked"})
        assert resp.status_code == 200, resp.text
        resp = await resolve(root_headers, model["canonical_id"])
        assert_problem(resp, 403, "permission_denied", reason="MODEL_NOT_APPROVED")


class TestAvailableModels:
    async def test_list_is_exactly_what_resolves(
        self, client, available_url, root_headers, l1a_headers, create_provider, create_model, resolve
    ):
        """MR-R05: the available list holds exactly the models that resolve."""
        fam = family()
        inherited = await create_provider(root_headers, sharing_mode="shared")
        own = await create_provider(l1a_headers)
        disabled = await create_provider(l1a_headers, enabled=False)
        shadowed = await create_provider(root_headers, sharing_mode="shared")
        shadow = await create_provider(l1a_headers, slug=shadowed["slug"])

        async def add(headers, provider, name, **fields):
            model = await create_model(headers, provider["id"], name, family=fam, **fields)
            return model["canonical_id"]

        expected = {
            await add(root_headers, inherited, "shared-approved", sharing_mode="shared"),
            await add(l1a_headers, own, "own-approved"),
            await add(l1a_headers, shadow, "winner"),
        }
        others = {
            await add(root_headers, inherited, "shared-pending", sharing_mode="shared", approval_state="pending"),
            await add(root_headers, inherited, "not-shared"),
            await add(l1a_headers, own, "own-rejected", approval_state="rejected"),
            await add(l1a_headers, disabled, "of-disabled"),
            await add(root_headers, shadowed, "loser", sharing_mode="shared"),
        }

        listed = await collect(client, available_url, l1a_headers, {"$filter": f"family eq '{fam}'"})
        assert {m["canonical_id"] for m in listed} == expected
        for model in listed:
            for flag in ENTRY_FLAGS:
                assert flag not in model

        for canonical_id in expected | others:
            resp = await resolve(l1a_headers, canonical_id)
            assert (resp.status_code == 200) == (canonical_id in expected), (
                f"{canonical_id}: {resp.status_code} {resp.text}"
            )

    async def test_list_combines_inherited_and_own_models(
        self,
        client,
        available_url,
        root_headers,
        l1a_headers,
        l2b_headers,
        create_provider,
        create_model,
    ):
        """MR-R09: one list holds own models and those inherited from every ancestor."""
        fam = family()
        of_root = await create_provider(root_headers, sharing_mode="shared")
        of_l1a = await create_provider(l1a_headers, sharing_mode="shared")
        of_l2b = await create_provider(l2b_headers)

        from_root = await create_model(root_headers, of_root["id"], "from-root", family=fam, sharing_mode="shared")
        from_l1a = await create_model(l1a_headers, of_l1a["id"], "from-l1a", family=fam, sharing_mode="shared")
        l1a_private = await create_model(l1a_headers, of_l1a["id"], "l1a-private", family=fam)
        own = await create_model(l2b_headers, of_l2b["id"], "own", family=fam)

        async def owners(headers) -> dict:
            listed = await collect(client, available_url, headers, {"$filter": f"family eq '{fam}'"})
            return {m["canonical_id"]: m["tenant_id"] for m in listed}

        assert await owners(l2b_headers) == {
            from_root["canonical_id"]: HIERARCHY_ROOT,
            from_l1a["canonical_id"]: HIERARCHY_L1A,
            own["canonical_id"]: HIERARCHY_L2B,
        }
        assert await owners(l1a_headers) == {
            from_root["canonical_id"]: HIERARCHY_ROOT,
            from_l1a["canonical_id"]: HIERARCHY_L1A,
            l1a_private["canonical_id"]: HIERARCHY_L1A,
        }
        assert await owners(root_headers) == {from_root["canonical_id"]: HIERARCHY_ROOT}

    async def test_capability_filter_uses_effective_values(
        self, client, available_url, models_url, root_headers, create_provider, create_model
    ):
        """MR-R06: `vision eq true` matches declared-and-not-disabled vision only."""
        fam = family()
        provider = await create_provider(root_headers)
        vision = {"vision": {"enabled": True, "supported_mime_types": ["image/png"]}}
        seeing = await create_model(root_headers, provider["id"], "seeing", family=fam, capabilities=vision)
        await create_model(root_headers, provider["id"], "blind", family=fam)
        await create_model(
            root_headers,
            provider["id"],
            "switched-off",
            family=fam,
            capabilities=vision,
            disabled_capabilities={"vision": {"disabled": True}},
        )
        params = {"$filter": f"family eq '{fam}' and vision eq true"}

        for url in (available_url, models_url):
            listed = await collect(client, url, root_headers, params)
            assert [m["id"] for m in listed] == [seeing["id"]]

        listed = await collect(
            client, available_url, root_headers, {"$filter": f"family eq '{fam}' and vision eq false"}
        )
        assert sorted(m["name"] for m in listed) == ["blind", "switched-off"]

    async def test_empty_result_is_an_empty_page(self, client, available_url, root_headers):
        """MR-R07: no matching model is 200 with an empty page."""
        resp = await client.get(
            available_url, headers=root_headers, params={"$filter": f"family eq '{family()}'"}
        )
        assert resp.status_code == 200, resp.text
        assert resp.json()["items"] == []


class TestAuthentication:
    @pytest.mark.parametrize("path", ["providers", "models", "available-models", "available-models/a%3A%3Ab"])
    async def test_no_token_is_401(self, base_url, path):
        """MR-R08: every route requires authentication."""
        async with httpx.AsyncClient(timeout=5.0) as anonymous:
            resp = await anonymous.get(f"{base_url}/model-registry/v1/{path}")
        assert resp.status_code == 401, resp.text

    async def test_denied_subject_is_403(self, client, available_url, providers_url):
        """MR-R08: a subject the PDP denies gets NOT_PERMITTED."""
        denied = {"Authorization": "Bearer e2e-token-nil-tenant"}
        for url in (available_url, providers_url):
            resp = await client.get(url, headers=denied)
            assert_problem(resp, 403, "permission_denied", reason="NOT_PERMITTED")
