"""Successful deletion and the tombstones it leaves."""

from copy import deepcopy

import pytest

from .helpers import (
    ENTITY_SELECT,
    RECEIPT,
    assert_exact,
    assert_exact_entity,
    assert_json,
    assert_operation,
    completed,
    delete_and_assert,
    delete_one_and_poll,
    get_entity,
    gts_uuid,
    instance_entity,
    outcome,
    removal,
    register_and_assert,
    schema_entity,
    schema_with_id,
    target,
    timestamp,
)


async def _tombstone(client, api_path, document, expected, operation):
    """Deletion moves only lifecycle, version and `updated_at`, within the operation."""
    response = await assert_exact_entity(client, api_path, document, expected)
    updated = timestamp(response.json()["origin"]["updated_at"])
    assert timestamp(operation["started_at"]) <= updated <= timestamp(
        operation["completed_at"]
    ), response.json()
    return response


async def _unchanged(client, api_path, document, expected, before):
    after = await assert_exact_entity(
        client, api_path, document, expected, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())


@pytest.mark.smoke
@pytest.mark.scenario("TR-DEL-001")
async def test_delete_one_entity_leaves_a_readable_tombstone(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A deleted entity keeps its whole body and reads back as a tombstone."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    await assert_exact_entity(registry_http, registry_api_path, schema, schema_entity(schema, 1))

    operation = await delete_one_and_poll(
        registry_http, registry_api_path, schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        operation, completed("deletion", removal(schema, "succeeded", 2)), ordered=True
    )
    await _tombstone(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 2, lifecycle_status="deleted"),
        operation,
    )


@pytest.mark.scenario("TR-DEL-002")
async def test_batch_deletion_reports_mixed_key_outcomes_in_request_order(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Outcomes arrive in request order, which is what UUID callers match on."""
    person = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    other = deletion_fixture("other_schema")
    spare = schema_with_id(other, other["gts_id"].replace(".other.v1~", ".spare.v1~"))
    await given_registered(person, instance, other, spare)
    assert other["gts_id"] < spare["gts_id"]
    person_before = await assert_exact_entity(
        registry_http, registry_api_path, person, schema_entity(person, 1)
    )

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [
            target(spare, 1, key=gts_uuid(spare["gts_id"])),
            target(instance, 1),
            target(other, 1, key=gts_uuid(other["gts_id"])),
        ],
        removal(gts_uuid(spare["gts_id"]), "succeeded", 2),
        removal(instance, "succeeded", 2),
        removal(gts_uuid(other["gts_id"]), "succeeded", 2),
    )
    await _unchanged(
        registry_http, registry_api_path, person, schema_entity(person, 1), person_before
    )


@pytest.mark.scenario("TR-DEL-003")
async def test_deleting_an_instance_keeps_its_schema_and_sibling(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Deletion is per entity: nothing cascades to the schema or a sibling."""
    schema = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    sibling = deepcopy(instance)
    sibling["gts_id"] = sibling["gts_id"].replace(".alice.v1", ".bob.v1")
    sibling["content"]["name"] = "Bob"
    await given_registered(schema, instance, sibling)
    schema_before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    sibling_before = await assert_exact_entity(
        registry_http, registry_api_path, sibling, instance_entity(sibling, 1)
    )

    operation = await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(instance, 1)],
        removal(instance, "succeeded", 2),
    )
    await _tombstone(
        registry_http,
        registry_api_path,
        instance,
        instance_entity(instance, 2, lifecycle_status="deleted"),
        operation,
    )
    await _unchanged(
        registry_http, registry_api_path, schema, schema_entity(schema, 1), schema_before
    )
    await _unchanged(
        registry_http, registry_api_path, sibling, instance_entity(sibling, 1), sibling_before
    )


@pytest.mark.scenario("TR-DEL-004")
async def test_deletion_through_a_registry_reference(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The single route takes the UUID a read issued; the outcome echoes it."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    read = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    reference = read.json()["gts_uuid"]

    operation = await delete_one_and_poll(registry_http, registry_api_path, reference, 1, RECEIPT)
    assert_operation(
        operation, completed("deletion", removal(reference, "succeeded", 2)), ordered=True
    )
    tombstone = await _tombstone(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 2, lifecycle_status="deleted"),
        operation,
    )
    by_reference = await get_entity(
        registry_http, registry_api_path, reference, select=ENTITY_SELECT
    )
    assert_exact(
        by_reference,
        {
            "status": 200,
            "etag": tombstone.headers["etag"],
            "body": schema_entity(schema, 2, lifecycle_status="deleted"),
        },
    )
    assert_json(by_reference.json(), tombstone.json())


@pytest.mark.scenario("TR-DEL-005")
async def test_deletion_keeps_the_current_content_after_several_revisions(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The tombstone is the latest revision, not an earlier definition."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    current = schema
    for version, title in ((2, "Person revision 2"), (3, "Person revision 3")):
        current = deepcopy(current)
        current["content"]["title"] = title
        current["expected_resource_version"] = version - 1
        await register_and_assert(
            registry_http, registry_api_path, [current], outcome(schema, "succeeded", version)
        )
    await assert_exact_entity(registry_http, registry_api_path, schema, schema_entity(current, 3))

    operation = await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(schema, 3)],
        removal(schema, "succeeded", 4),
    )
    await _tombstone(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(current, 4, lifecycle_status="deleted"),
        operation,
    )
