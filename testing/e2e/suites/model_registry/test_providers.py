"""Model Registry E2E: provider management (MR-P*)."""
import uuid

import pytest

from .conftest import HIERARCHY_ROOT, OPEN_TYPE, ROOT_SUBJECT
from .helpers import PROVIDER_RESOURCE, UUID_V7_RE, assert_problem, collect, etag_of

ANY = {"If-Match": "*"}


class TestProviderCrud:
    """Create, read, update and delete of own providers."""

    @pytest.mark.smoke
    async def test_create_applies_defaults(self, client, providers_url, root_headers, unique_slug):
        """MR-P01: create returns 201, Location, ETag and the documented defaults."""
        slug = unique_slug()
        resp = await client.post(
            providers_url,
            headers=root_headers,
            json={"slug": slug, "name": "OpenAI", "gts_type": OPEN_TYPE},
        )
        assert resp.status_code == 201, resp.text
        body = resp.json()
        try:
            assert UUID_V7_RE.match(body["id"]), body["id"]
            assert resp.headers["location"].endswith(f"/model-registry/v1/providers/{body['id']}")
            assert etag_of(resp) == '"1"'
            assert body["slug"] == slug
            assert body["name"] == "OpenAI"
            assert body["gts_type"] == OPEN_TYPE
            assert body["tenant_id"] == HIERARCHY_ROOT
            assert body["enabled"] is True
            assert body["sharing_mode"] == "tenant"
            assert body["managed"] is False
            assert body.get("metadata") is None
            assert body["version"] == 1
            assert body["created_by"] == ROOT_SUBJECT
            assert body["updated_by"] == ROOT_SUBJECT
            assert body["created_at"] and body["updated_at"]
        finally:
            await client.delete(f"{providers_url}/{body['id']}", headers={**root_headers, **ANY})

    async def test_get_and_list_own_provider(self, client, providers_url, root_headers, create_provider):
        """MR-P02: an own enabled provider reads back with its entry flags."""
        created = await create_provider(root_headers, metadata={"tier": "gold"})

        resp = await client.get(f"{providers_url}/{created['id']}", headers=root_headers)
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"1"'
        entry = resp.json()
        assert entry["metadata"] == {"tier": "gold"}
        assert entry["inherited"] is False
        assert entry["shadowed"] is False
        assert entry["available_for_eval"] is True

        listed = await collect(
            client, providers_url, root_headers, {"$filter": f"slug eq '{created['slug']}'"}
        )
        assert [p["id"] for p in listed] == [created["id"]]
        assert listed[0]["inherited"] is False
        assert listed[0]["available_for_eval"] is True

    async def test_update_replaces_present_fields_only(
        self, client, providers_url, root_headers, create_provider
    ):
        """MR-P03: present fields replace, absent fields stay, null clears metadata."""
        created = await create_provider(root_headers, metadata={"a": 1}, managed=True)
        url = f"{providers_url}/{created['id']}"

        resp = await client.patch(
            url, headers={**root_headers, "If-Match": '"1"'}, json={"name": "Renamed", "enabled": False}
        )
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"2"'
        body = resp.json()
        assert body["name"] == "Renamed"
        assert body["enabled"] is False
        assert body["available_for_eval"] is False
        assert body["version"] == 2
        # Untouched fields keep their values.
        assert body["metadata"] == {"a": 1}
        assert body["managed"] is True
        assert body["slug"] == created["slug"]
        assert body["sharing_mode"] == "tenant"

        resp = await client.patch(url, headers={**root_headers, "If-Match": '"2"'}, json={"metadata": None})
        assert resp.status_code == 200, resp.text
        assert resp.json().get("metadata") is None

        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"3"'
        assert resp.json()["name"] == "Renamed"

    async def test_delete_is_hard_and_not_idempotent(
        self, client, providers_url, root_headers, create_provider
    ):
        """MR-P04: delete returns 204; later reads and a second delete are 404."""
        created = await create_provider(root_headers)
        url = f"{providers_url}/{created['id']}"

        resp = await client.delete(url, headers=root_headers)
        assert resp.status_code == 204, resp.text

        resp = await client.get(url, headers=root_headers)
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)
        resp = await client.delete(url, headers=root_headers)
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)

    async def test_get_unknown_provider_is_404(self, client, providers_url, root_headers):
        """MR-P04: an unknown id is a provider not-found problem."""
        resp = await client.get(f"{providers_url}/{uuid.uuid4()}", headers=root_headers)
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)

    async def test_delete_removes_models(
        self, client, providers_url, models_url, root_headers, create_provider, create_model
    ):
        """MR-P09: deleting a provider deletes its models."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])

        resp = await client.delete(f"{providers_url}/{provider['id']}", headers=root_headers)
        assert resp.status_code == 204, resp.text

        resp = await client.get(f"{models_url}/{model['id']}", headers=root_headers)
        assert resp.status_code == 404, resp.text


class TestProviderUniqueness:
    async def test_slug_is_unique_per_tenant(
        self, client, providers_url, root_headers, l1b_headers, create_provider
    ):
        """MR-P05: a second provider with the slug is 409 in the tenant only."""
        first = await create_provider(root_headers)
        body = {"slug": first["slug"], "name": "Again", "gts_type": OPEN_TYPE}

        resp = await client.post(providers_url, headers=root_headers, json=body)
        assert_problem(resp, 409, "already_exists", resource_type=PROVIDER_RESOURCE)

        # Another tenant may use the same slug.
        other = await create_provider(l1b_headers, slug=first["slug"])
        assert other["id"] != first["id"]


class TestProviderValidation:
    @pytest.mark.parametrize("slug", ["Upper", "under_score", "with space", ""])
    async def test_bad_slug_is_invalid_format(self, client, providers_url, root_headers, slug):
        """MR-P06: a slug outside [a-z0-9-]{1,64} is rejected."""
        resp = await client.post(
            providers_url, headers=root_headers, json={"slug": slug, "name": "X", "gts_type": OPEN_TYPE}
        )
        assert_problem(resp, 400, "invalid_argument", field="slug", field_reason="INVALID_FORMAT")

    async def test_long_name_is_too_large(self, client, providers_url, root_headers, unique_slug):
        """MR-P06: a name over 255 characters is TOO_LARGE."""
        resp = await client.post(
            providers_url,
            headers=root_headers,
            json={"slug": unique_slug(), "name": "n" * 256, "gts_type": OPEN_TYPE},
        )
        assert_problem(resp, 400, "invalid_argument", field="name", field_reason="TOO_LARGE")

    @pytest.mark.parametrize(
        "gts_type,reason",
        [
            ("not-a-gts-type", "INVALID_GTS_TYPE"),
            (OPEN_TYPE.rstrip("~"), "INVALID_GTS_TYPE"),
            ("gts.cf.genai.model_registry.provider.v1~", "NOT_A_PROVIDER_TYPE"),
            ("gts.cf.genai.model.provider.v1~", "NOT_A_PROVIDER_TYPE"),
            ("gts.cf.genai.model.provider.v1~cf.e2e._.absent.v1~", "GTS_TYPE_NOT_REGISTERED"),
        ],
    )
    async def test_gts_type_checks(self, client, providers_url, root_headers, unique_slug, gts_type, reason):
        """MR-P07: each GTS check reports its own reason on gts_type."""
        slug = unique_slug()
        resp = await client.post(
            providers_url, headers=root_headers, json={"slug": slug, "name": "X", "gts_type": gts_type}
        )
        assert_problem(resp, 400, "invalid_argument", field="gts_type", field_reason=reason)

        listed = await collect(client, providers_url, root_headers, {"$filter": f"slug eq '{slug}'"})
        assert listed == []

    @pytest.mark.parametrize(
        "patch",
        [
            {"slug": "other-slug"},
            {"tenant_id": "00000000-0000-0000-0000-000000000005"},
            {"version": 7},
            {"created_at": "2020-01-01T00:00:00Z"},
        ],
    )
    async def test_immutable_fields_are_rejected(
        self, client, providers_url, root_headers, create_provider, patch
    ):
        """MR-P08: an update naming an immutable field is IMMUTABLE_FIELD."""
        created = await create_provider(root_headers)
        url = f"{providers_url}/{created['id']}"
        [field] = patch

        resp = await client.patch(url, headers={**root_headers, **ANY}, json=patch)
        assert_problem(resp, 400, "invalid_argument", field=field, field_reason="IMMUTABLE_FIELD")

        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"1"'
        assert resp.json()["slug"] == created["slug"]

    async def test_unknown_update_field_is_rejected(
        self, client, providers_url, root_headers, create_provider
    ):
        """MR-P08: an unknown top-level update field is INVALID_FORMAT on body."""
        created = await create_provider(root_headers)
        resp = await client.patch(
            f"{providers_url}/{created['id']}", headers={**root_headers, **ANY}, json={"nmae": "typo"}
        )
        assert_problem(resp, 400, "invalid_argument", field="body", field_reason="INVALID_FORMAT")
