"""Model Registry E2E: provider type migration (MR-G*)."""
from .conftest import OPEN_TYPE, STRICT_TYPE
from .helpers import assert_problem, canonical_segment, collect, etag_of

ANY = {"If-Match": "*"}


async def models_of(client, models_url, headers, provider) -> dict:
    listed = await collect(client, models_url, headers, {"$filter": f"provider_id eq {provider['id']}"})
    return {m["name"]: m for m in listed}


class TestTypeMigration:
    async def test_migration_moves_provider_and_models_together(
        self, client, providers_url, models_url, available_url, root_headers, create_provider, create_model
    ):
        """MR-G01: a valid migration switches every row's type and keeps identities."""
        provider = await create_provider(root_headers)
        one = await create_model(root_headers, provider["id"], "one", provider_settings={"deployment": "a"})
        two = await create_model(root_headers, provider["id"], "two", provider_settings={"deployment": "b"})

        resp = await client.patch(
            f"{providers_url}/{provider['id']}",
            headers={**root_headers, "If-Match": '"1"'},
            json={"gts_type": STRICT_TYPE},
        )
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"2"'
        body = resp.json()
        assert body["gts_type"] == STRICT_TYPE
        assert body["id"] == provider["id"]
        assert body["slug"] == provider["slug"]

        after = await models_of(client, models_url, root_headers, provider)
        for before in (one, two):
            model = after[before["name"]]
            assert model["id"] == before["id"]
            assert model["canonical_id"] == before["canonical_id"]
            assert model["info"]["gts_type"] == STRICT_TYPE
            assert model["info"]["provider_settings"] == before["info"]["provider_settings"]
            assert model["version"] == 2

        resp = await client.get(
            f"{available_url}/{canonical_segment(one['canonical_id'])}", headers=root_headers
        )
        assert resp.status_code == 200, resp.text
        assert resp.json()["info"]["gts_type"] == STRICT_TYPE

    async def test_one_invalid_model_fails_the_migration(
        self, client, providers_url, models_url, root_headers, create_provider, create_model
    ):
        """MR-G02: one model that fails the new schema leaves everything unchanged."""
        provider = await create_provider(root_headers)
        await create_model(root_headers, provider["id"], "fits", provider_settings={"deployment": "a"})
        misfit = await create_model(root_headers, provider["id"], "misfit", provider_settings={"other": 1})
        url = f"{providers_url}/{provider['id']}"

        resp = await client.patch(url, headers={**root_headers, **ANY}, json={"gts_type": STRICT_TYPE})
        body = assert_problem(
            resp, 400, "invalid_argument", field="gts_type", field_reason="SCHEMA_MISMATCH"
        )
        violations = body["context"]["field_violations"]
        assert len(violations) == 1, violations
        assert misfit["canonical_id"] in violations[0]["description"]

        resp = await client.get(url, headers=root_headers)
        assert etag_of(resp) == '"1"'
        assert resp.json()["gts_type"] == OPEN_TYPE
        for model in (await models_of(client, models_url, root_headers, provider)).values():
            assert model["info"]["gts_type"] == OPEN_TYPE
            assert model["version"] == 1

    async def test_migration_runs_the_provider_type_checks(
        self, client, providers_url, root_headers, create_provider
    ):
        """MR-G02: the new type passes the same GTS checks as on create."""
        provider = await create_provider(root_headers)
        url = f"{providers_url}/{provider['id']}"

        resp = await client.patch(
            url,
            headers={**root_headers, **ANY},
            json={"gts_type": "gts.cf.genai.model.provider.v1~cf.e2e._.absent.v1~"},
        )
        assert_problem(
            resp, 400, "invalid_argument", field="gts_type", field_reason="GTS_TYPE_NOT_REGISTERED"
        )
        resp = await client.get(url, headers=root_headers)
        assert resp.json()["gts_type"] == OPEN_TYPE

    async def test_equal_type_is_a_plain_update(
        self, client, providers_url, models_url, root_headers, create_provider, create_model
    ):
        """MR-G01: resending the stored type updates the provider only."""
        provider = await create_provider(root_headers)
        await create_model(root_headers, provider["id"], "one")

        resp = await client.patch(
            f"{providers_url}/{provider['id']}",
            headers={**root_headers, **ANY},
            json={"gts_type": OPEN_TYPE, "name": "Renamed"},
        )
        assert resp.status_code == 200, resp.text
        assert etag_of(resp) == '"2"'

        models = await models_of(client, models_url, root_headers, provider)
        assert models["one"]["version"] == 1
