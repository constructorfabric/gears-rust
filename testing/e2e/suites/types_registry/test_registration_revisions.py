"""Instance and schema revisions, preconditions, and tombstones."""

from copy import deepcopy

import pytest

from .helpers import (
    RECEIPT,
    assert_exact_entity,
    assert_json,
    assert_operation,
    completed,
    delete_one_and_poll,
    instance_entity,
    outcome,
    removal,
    register_and_assert,
    schema_entity,
    timestamp,
)


@pytest.mark.scenario("TR-REG-301")
async def test_optional_property_at_a_closed_root_is_compatible(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """An optional root property succeeds without changing the schema identity."""
    schema = registration_fixture("person_schema")
    await given_registered(schema)
    original = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )

    revised = deepcopy(schema)
    revised["content"]["properties"]["nickname"] = {"type": "string"}
    revised["expected_resource_version"] = 1
    await register_and_assert(
        registry_http, registry_api_path, [revised], outcome(schema, "succeeded", 2)
    )
    after = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(revised, 2)
    )
    assert after.json()["gts_uuid"] == original.json()["gts_uuid"]
    assert after.json()["origin"]["created_at"] == original.json()["origin"]["created_at"]
    assert timestamp(after.json()["origin"]["updated_at"]) >= timestamp(
        original.json()["origin"]["updated_at"]
    )


@pytest.mark.scenario("TR-REG-302")
async def test_instance_revision_does_not_revise_its_schema(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """An Instance value revision leaves its Type Schema at version one."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    await given_registered(schema, instance)
    schema_before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    instance_before = await assert_exact_entity(
        registry_http, registry_api_path, instance, instance_entity(instance, 1)
    )

    revised = deepcopy(instance)
    revised["content"]["name"] = "Alicia"
    revised["expected_resource_version"] = 1
    await register_and_assert(
        registry_http, registry_api_path, [revised], outcome(instance, "succeeded", 2)
    )
    instance_after = await assert_exact_entity(
        registry_http, registry_api_path, instance, instance_entity(revised, 2)
    )
    assert instance_after.json()["gts_uuid"] == instance_before.json()["gts_uuid"]
    assert (
        instance_after.json()["origin"]["created_at"]
        == instance_before.json()["origin"]["created_at"]
    )
    schema_after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 1),
        etag=schema_before.headers["etag"],
    )
    assert_json(schema_after.json(), schema_before.json())


@pytest.mark.scenario("TR-REG-303")
async def test_equal_instance_content_is_unchanged(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """Equal authored Instance content reports unchanged without moving timestamps."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    await given_registered(schema, instance)
    before = await assert_exact_entity(
        registry_http, registry_api_path, instance, instance_entity(instance, 1)
    )

    same = {**instance, "expected_resource_version": 1}
    await register_and_assert(
        registry_http, registry_api_path, [same], outcome(instance, "unchanged", 1)
    )
    after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        instance,
        instance_entity(instance, 1),
        etag=before.headers["etag"],
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-304")
async def test_stale_writer_reads_and_rebases_its_description(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A stale writer preserves the current title when it retries its description."""
    schema = registration_fixture("person_schema")
    await given_registered(schema)
    await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    writer_b = deepcopy(schema)
    writer_b["content"]["title"] = "B's title"
    writer_b["expected_resource_version"] = 1
    writer_a = deepcopy(schema)
    writer_a["content"]["description"] = "A's description"
    writer_a["expected_resource_version"] = 1

    await register_and_assert(
        registry_http, registry_api_path, [writer_b], outcome(schema, "succeeded", 2)
    )
    b_current = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(writer_b, 2)
    )
    await register_and_assert(
        registry_http,
        registry_api_path,
        [writer_a],
        outcome(schema, "failed", None, "precondition_failed"),
    )
    after_stale = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(writer_b, 2),
        etag=b_current.headers["etag"],
    )
    assert_json(after_stale.json(), b_current.json())

    reconciled = deepcopy(schema)
    reconciled["content"] = deepcopy(after_stale.json()["content"])
    reconciled["content"]["description"] = writer_a["content"]["description"]
    reconciled["expected_resource_version"] = 2
    await register_and_assert(
        registry_http, registry_api_path, [reconciled], outcome(schema, "succeeded", 3)
    )
    await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(reconciled, 3)
    )


@pytest.mark.scenario("TR-REG-305")
async def test_tombstone_refuses_creation_and_revision(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A tombstoned ID refuses both creation and version-matched revision."""
    schema = registration_fixture("person_schema")
    await given_registered(schema)
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    deletion = await delete_one_and_poll(
        registry_http, registry_api_path, schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        deletion, completed("deletion", removal(schema, "succeeded", 2)), ordered=True
    )
    tombstone = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 2, "deleted")
    )
    assert tombstone.json()["gts_uuid"] == before.json()["gts_uuid"]
    assert tombstone.json()["origin"]["created_at"] == before.json()["origin"]["created_at"]

    await register_and_assert(
        registry_http,
        registry_api_path,
        [schema],
        outcome(schema, "failed", None, "already_exists"),
    )
    revision = {**schema, "expected_resource_version": 2}
    await register_and_assert(
        registry_http,
        registry_api_path,
        [revision],
        outcome(schema, "failed", None, "entity_deleted"),
    )
    unchanged = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 2, "deleted"),
        etag=tombstone.headers["etag"],
    )
    assert_json(unchanged.json(), tombstone.json())
