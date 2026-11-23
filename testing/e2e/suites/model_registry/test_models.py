"""Model Registry E2E: model management (MR-M*)."""
import uuid

import pytest

from .conftest import HIERARCHY_ROOT, OPEN_TYPE, ROOT_SUBJECT, STRICT_TYPE, model_body
from .helpers import (
    MODEL_RESOURCE,
    PROVIDER_RESOURCE,
    UUID_V7_RE,
    assert_problem,
    collect,
    etag_of,
)

ANY = {"If-Match": "*"}


class TestModelCrud:
    @pytest.mark.smoke
    async def test_create_builds_identity_and_defaults(
        self, client, models_url, root_headers, create_provider
    ):
        """MR-M01: create derives canonical id, tenant, type and approval."""
        provider = await create_provider(root_headers)
        resp = await client.post(
            models_url, headers=root_headers, json=model_body(provider["id"], "gpt-4o_mini.v1")
        )
        assert resp.status_code == 201, resp.text
        body = resp.json()
        assert UUID_V7_RE.match(body["id"]), body["id"]
        assert resp.headers["location"].endswith(f"/model-registry/v1/models/{body['id']}")
        assert etag_of(resp) == '"1"'
        assert body["canonical_id"] == f"{provider['slug']}::gpt-4o_mini.v1"
        assert body["tenant_id"] == HIERARCHY_ROOT
        assert body["provider_id"] == provider["id"]
        assert body["sharing_mode"] == "tenant"
        assert body["version"] == 1
        assert body["created_by"] == ROOT_SUBJECT
        assert body["approval"]["state"] == "approved"
        assert body["approval"]["decided_by"] == ROOT_SUBJECT
        assert body["approval"]["decided_at"]
        info = body["info"]
        assert info["gts_type"] == OPEN_TYPE
        assert info["managed"] is False
        assert info["sort_order"] == 0
        assert info["allow_parameter_override"] is False
        assert info["allow_extra_params"] == []
        assert info["additional_info"] == {}
        # The registry stores no default endpoint.
        assert info.get("base_url") is None
        assert info.get("secret_ref") is None

    async def test_get_and_list_own_model(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M02: an own approved model of an enabled provider reads back with flags."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])

        resp = await client.get(f"{models_url}/{model['id']}", headers=root_headers)
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"1"'
        entry = resp.json()
        assert entry["inherited"] is False
        assert entry["shadowed"] is False
        assert entry["provider_enabled"] is True
        assert entry["available_for_eval"] is True

        listed = await collect(
            client, models_url, root_headers, {"$filter": f"provider_id eq {provider['id']}"}
        )
        assert [m["id"] for m in listed] == [model["id"]]
        assert listed[0]["available_for_eval"] is True

    async def test_cost_round_trips(self, client, models_url, root_headers, create_provider, create_model):
        """MR-M03: the five rates are stored as sent; absent rates stay absent."""
        provider = await create_provider(root_headers)
        cost = {
            "input_per_1m_micro": 2_500_000,
            "output_per_1m_micro": 10_000_000,
            "cache_read_per_1m_micro": 1_250_000,
            "cache_write_per_1m_micro": 3_000_000,
            "web_search_per_1k_calls_micro": 30_000_000,
        }
        priced = await create_model(root_headers, provider["id"], "priced", cost=cost)
        partial = await create_model(
            root_headers, provider["id"], "partial", cost={"input_per_1m_micro": 1}
        )
        free = await create_model(root_headers, provider["id"], "free")

        assert priced["info"]["cost"] == cost
        assert partial["info"]["cost"] == {"input_per_1m_micro": 1}
        assert free["info"]["cost"] == {}

        resp = await client.get(f"{models_url}/{priced['id']}", headers=root_headers)
        assert resp.json()["info"]["cost"] == cost

    async def test_delete_is_hard(self, client, models_url, root_headers, create_provider, create_model):
        """MR-M11: delete returns 204; later reads and a second delete are 404."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])
        url = f"{models_url}/{model['id']}"

        resp = await client.delete(url, headers=root_headers)
        assert resp.status_code == 204, resp.text
        resp = await client.get(url, headers=root_headers)
        assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)
        resp = await client.delete(url, headers=root_headers)
        assert_problem(resp, 404, "not_found", resource_type=MODEL_RESOURCE)

        # The name is free again.
        await create_model(root_headers, provider["id"])


class TestModelUpdate:
    async def test_present_fields_replace_whole_values(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M04: nested objects are replaced, not merged; absent fields stay."""
        provider = await create_provider(root_headers)
        model = await create_model(
            root_headers,
            provider["id"],
            cost={"input_per_1m_micro": 1, "output_per_1m_micro": 2},
            capabilities={
                "vision": {"enabled": True, "supported_mime_types": ["image/png"]},
                "streaming": True,
            },
            family="gpt-4",
        )
        url = f"{models_url}/{model['id']}"

        resp = await client.patch(
            url,
            headers={**root_headers, "If-Match": '"1"'},
            json={"cost": {"input_per_1m_micro": 9}, "capabilities": {"streaming": True}},
        )
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"2"'
        info = resp.json()["info"]
        assert info["cost"] == {"input_per_1m_micro": 9}
        assert info["capabilities"]["streaming"] is True
        assert info["capabilities"]["vision"]["enabled"] is False
        assert info["capabilities"]["vision"]["supported_mime_types"] == []
        # Untouched fields keep their values.
        assert info["family"] == "gpt-4"
        assert info["display_name"] == model["info"]["display_name"]
        assert resp.json()["canonical_id"] == model["canonical_id"]

    async def test_null_clears_optional_fields(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M04: null clears an optional field and is rejected on a required one."""
        provider = await create_provider(root_headers)
        model = await create_model(
            root_headers,
            provider["id"],
            base_url="https://llm.example.com/v1",
            description="A model",
        )
        assert model["info"]["base_url"] == "https://llm.example.com/v1"
        url = f"{models_url}/{model['id']}"

        resp = await client.patch(
            url, headers={**root_headers, **ANY}, json={"base_url": None, "description": None}
        )
        assert resp.status_code == 200, resp.text
        info = resp.json()["info"]
        assert info.get("base_url") is None
        assert info.get("description") is None

        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"display_name": None})
        assert_problem(resp, 400, "invalid_argument")
        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"2"'

    @pytest.mark.parametrize(
        "patch",
        [
            {"name": "renamed"},
            {"provider_id": "01900000-0000-7000-8000-000000000000"},
            {"canonical_id": "x::y"},
            {"gts_type": STRICT_TYPE},
            {"tenant_id": "00000000-0000-0000-0000-000000000005"},
            {"version": 7},
        ],
    )
    async def test_immutable_fields_are_rejected(
        self, client, models_url, root_headers, create_provider, create_model, patch
    ):
        """MR-M05: an update naming an immutable field is IMMUTABLE_FIELD."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])
        [field] = patch

        resp = await client.patch(
            f"{models_url}/{model['id']}", headers={**root_headers, **ANY}, json=patch
        )
        assert_problem(resp, 400, "invalid_argument", field=field, field_reason="IMMUTABLE_FIELD")

        resp = await client.get(f"{models_url}/{model['id']}", headers=root_headers)
        assert etag_of(resp) == '"1"'
        assert resp.json()["canonical_id"] == model["canonical_id"]

    async def test_provider_model_id_is_mutable_and_not_unique(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M05: provider_model_id changes freely and may repeat; canonical id stays."""
        provider = await create_provider(root_headers)
        east = await create_model(root_headers, provider["id"], "east", provider_model_id="gpt-4o")
        west = await create_model(root_headers, provider["id"], "west", provider_model_id="gpt-4o")
        assert east["info"]["provider_model_id"] == west["info"]["provider_model_id"] == "gpt-4o"

        resp = await client.patch(
            f"{models_url}/{east['id']}",
            headers={**root_headers, **ANY},
            json={"provider_model_id": "gpt-4o-2024-08-06"},
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["info"]["provider_model_id"] == "gpt-4o-2024-08-06"
        assert body["canonical_id"] == east["canonical_id"]
        assert body["name"] == "east"

    async def test_unknown_update_field_is_rejected(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M05: an unknown top-level update field is INVALID_FORMAT on body."""
        provider = await create_provider(root_headers)
        model = await create_model(root_headers, provider["id"])
        resp = await client.patch(
            f"{models_url}/{model['id']}", headers={**root_headers, **ANY}, json={"displayname": "x"}
        )
        assert_problem(resp, 400, "invalid_argument", field="body", field_reason="INVALID_FORMAT")


class TestModelCreateRefusals:
    async def test_duplicate_name_under_provider(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M06: a second model with the name under the provider is 409."""
        provider = await create_provider(root_headers)
        await create_model(root_headers, provider["id"], "dup")

        resp = await client.post(
            models_url, headers=root_headers, json=model_body(provider["id"], "dup")
        )
        assert_problem(resp, 409, "already_exists", resource_type=MODEL_RESOURCE)

        # The same name under another provider is a different canonical id.
        other = await create_provider(root_headers)
        await create_model(root_headers, other["id"], "dup")

    async def test_unknown_provider_is_404(self, client, models_url, root_headers):
        """MR-M07: create under an unknown provider is provider not found."""
        resp = await client.post(
            models_url, headers=root_headers, json=model_body(str(uuid.uuid4()), "m1")
        )
        assert_problem(resp, 404, "not_found", resource_type=PROVIDER_RESOURCE)

    @pytest.mark.parametrize(
        "fields,field,reason",
        [
            ({"name": "Has Space"}, "name", "INVALID_FORMAT"),
            ({"name": "UPPER"}, "name", "INVALID_FORMAT"),
            ({"base_url": "https://user:pw@llm.example.com"}, "base_url", "INVALID_FORMAT"),
            ({"base_url": "ftp://llm.example.com"}, "base_url", "INVALID_FORMAT"),
            ({"base_url": "/relative/path"}, "base_url", "INVALID_FORMAT"),
            ({"icon": "data:image/png;base64,AAAA"}, "icon", "INVALID_FORMAT"),
            ({"provider_model_id": "has space"}, "provider_model_id", "INVALID_FORMAT"),
            ({"description": "d" * 4097}, "description", "TOO_LARGE"),
            ({"display_name": "d" * 256}, "display_name", "TOO_LARGE"),
            ({"cost": {"input_per_1M_micro": 1}}, "cost/input_per_1M_micro", "INVALID_FORMAT"),
            (
                {"context_window": {"max_input_tokens": 1, "max_tokens": 2}},
                "context_window/max_tokens",
                "INVALID_FORMAT",
            ),
        ],
    )
    async def test_invalid_fields(
        self, client, models_url, root_headers, create_provider, fields, field, reason
    ):
        """MR-M08: each invalid field is one violation on that field."""
        provider = await create_provider(root_headers)
        body = model_body(provider["id"], **{"name": "m1", **fields})

        resp = await client.post(models_url, headers=root_headers, json=body)
        assert_problem(resp, 400, "invalid_argument", field=field, field_reason=reason)

        listed = await collect(
            client, models_url, root_headers, {"$filter": f"provider_id eq {provider['id']}"}
        )
        assert listed == []


class TestProviderSettings:
    async def test_settings_follow_the_provider_type(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M09: settings are validated against the provider type's schema."""
        provider = await create_provider(root_headers, gts_type=STRICT_TYPE)

        model = await create_model(
            root_headers, provider["id"], "ok", provider_settings={"deployment": "prod-eu"}
        )
        assert model["info"]["gts_type"] == STRICT_TYPE
        assert model["info"]["provider_settings"] == {"deployment": "prod-eu"}

        for settings in ({}, {"deployment": 5}, {"deployment": "d", "extra": True}):
            resp = await client.post(
                models_url,
                headers=root_headers,
                json=model_body(provider["id"], "bad", provider_settings=settings),
            )
            assert_problem(
                resp, 400, "invalid_argument", field="provider_settings", field_reason="SCHEMA_MISMATCH"
            )

        # An update is validated the same way and leaves the row alone.
        url = f"{models_url}/{model['id']}"
        resp = await client.patch(
            url, headers={**root_headers, **ANY}, json={"provider_settings": {"wrong": 1}}
        )
        assert_problem(
            resp, 400, "invalid_argument", field="provider_settings", field_reason="SCHEMA_MISMATCH"
        )
        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"1"'

    async def test_open_type_accepts_any_object(self, root_headers, create_provider, create_model):
        """MR-M09: a type that does not constrain the settings accepts any object."""
        provider = await create_provider(root_headers)
        settings = {"anything": {"nested": [1, 2, 3]}}
        model = await create_model(root_headers, provider["id"], provider_settings=settings)
        assert model["info"]["provider_settings"] == settings

    async def test_settings_must_be_an_object(self, client, models_url, root_headers, create_provider):
        """MR-M09: provider_settings that is not a JSON object is rejected."""
        provider = await create_provider(root_headers)
        resp = await client.post(
            models_url,
            headers=root_headers,
            json=model_body(provider["id"], "m1", provider_settings=["not", "an", "object"]),
        )
        assert_problem(resp, 400, "invalid_argument", field="provider_settings")


class TestSecretRef:
    async def test_binding_a_secret_fails_closed(
        self, client, models_url, root_headers, create_provider, create_model
    ):
        """MR-M10: without CredStore v2 a new secret_ref is 503 and writes nothing."""
        provider = await create_provider(root_headers)

        resp = await client.post(
            models_url,
            headers=root_headers,
            json=model_body(provider["id"], "with-secret", secret_ref="openai-key"),
        )
        assert_problem(resp, 503, "service_unavailable")

        model = await create_model(root_headers, provider["id"], "no-secret")
        url = f"{models_url}/{model['id']}"
        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"secret_ref": "openai-key"})
        assert_problem(resp, 503, "service_unavailable")

        listed = await collect(
            client, models_url, root_headers, {"$filter": f"provider_id eq {provider['id']}"}
        )
        assert [m["name"] for m in listed] == ["no-secret"]
        assert listed[0]["version"] == 1

        # Clearing makes no CredStore call.
        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"secret_ref": None})
        assert resp.status_code == 200, resp.text

    async def test_bad_secret_ref_format(self, client, models_url, root_headers, create_provider):
        """MR-M10: a secret_ref outside [a-zA-Z0-9_-]{1,255} is INVALID_FORMAT."""
        provider = await create_provider(root_headers)
        resp = await client.post(
            models_url,
            headers=root_headers,
            json=model_body(provider["id"], "m1", secret_ref="has space/slash"),
        )
        assert_problem(resp, 400, "invalid_argument", field="secret_ref", field_reason="INVALID_FORMAT")
