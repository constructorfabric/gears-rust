"""Deletion preconditions, synchronous refusals and per-item failures."""

from copy import deepcopy
import pytest

from .helpers import (
    ENTITY_SELECT,
    assert_absent,
    assert_bad_request,
    assert_exact,
    assert_exact_entity,
    assert_json,
    assert_not_found,
    await_deletion,
    completed,
    assert_operation,
    delete_and_assert,
    delete_one,
    get_entity,
    gts_uuid,
    invalid_argument,
    not_found,
    outcome,
    removal,
    post_batch_delete,
    register_and_assert,
    schema_entity,
    schema_with_id,
    target,
)


async def _revise(client, api_path, document, title, version):
    revised = deepcopy(document)
    revised["content"]["title"] = title
    revised["expected_resource_version"] = version - 1
    await register_and_assert(
        client, api_path, [revised], outcome(document, "succeeded", version)
    )
    return revised


async def _unchanged(client, api_path, document, expected, before):
    after = await assert_exact_entity(
        client, api_path, document, expected, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())


def _requires_version(document):
    gts_id = document["gts_id"]
    return invalid_argument(
        "expected_resource_version",
        "VALIDATION_FAILED",
        f"deleting '{gts_id}' requires a positive expected_resource_version; "
        "an absent one is not a request to delete whatever is there",
        resource_name=gts_id,
    )


@pytest.mark.scenario("TR-DEL-101")
async def test_a_stale_precondition_fails_the_item_and_a_fresh_read_retries(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A stale version is a terminal item, not a 412; rereading recovers."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    revised = await _revise(registry_http, registry_api_path, schema, "Revised Person", 2)
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(revised, 2)
    )

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(schema, 1)],
        removal(schema, "failed", None, "precondition_failed"),
    )
    await _unchanged(
        registry_http, registry_api_path, schema, schema_entity(revised, 2), before
    )
    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(schema, 2)],
        removal(schema, "succeeded", 3),
    )
    await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(revised, 3, lifecycle_status="deleted"),
    )


@pytest.mark.scenario("TR-DEL-102")
async def test_an_absent_identifier_fails_without_reserving_it(
    registry_http, registry_api_path, deletion_fixture
):
    """A refused deletion leaves nothing behind that would block a creation."""
    schema = deletion_fixture("person_schema")
    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(schema, 1)],
        removal(schema, "failed", None, "precondition_failed"),
    )
    await assert_absent(registry_http, registry_api_path, schema)
    await register_and_assert(
        registry_http, registry_api_path, [schema], outcome(schema, "succeeded", 1)
    )


@pytest.mark.scenario("TR-DEL-103")
async def test_a_new_deletion_of_a_tombstone_is_not_active_whatever_its_version(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Lifecycle is checked before version, so no answer invites a retry."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    await delete_and_assert(
        registry_http, registry_api_path, [target(schema, 1)], removal(schema, "succeeded", 2)
    )
    tombstone = schema_entity(schema, 2, lifecycle_status="deleted")
    before = await assert_exact_entity(registry_http, registry_api_path, schema, tombstone)

    for version in (1, 2):
        await delete_and_assert(
            registry_http,
            registry_api_path,
            [target(schema, version)],
            removal(schema, "failed", None, "not_active"),
        )
    await _unchanged(registry_http, registry_api_path, schema, tombstone, before)


@pytest.mark.scenario("TR-DEL-104")
async def test_independent_successes_survive_item_refusals(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A stale and an absent target fail alone; the rest of the batch commits."""
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    absent = schema_with_id(person, person["gts_id"].replace(".person.v1~", ".absent.v1~"))
    await given_registered(person, other)
    revised = await _revise(registry_http, registry_api_path, other, "Revised Other", 2)
    other_before = await assert_exact_entity(
        registry_http, registry_api_path, other, schema_entity(revised, 2)
    )

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1), target(other, 1), target(absent, 1)],
        removal(person, "succeeded", 2),
        removal(other, "failed", None, "precondition_failed"),
        removal(absent, "failed", None, "precondition_failed"),
    )
    await _unchanged(
        registry_http, registry_api_path, other, schema_entity(revised, 2), other_before
    )
    await assert_absent(registry_http, registry_api_path, absent)


@pytest.mark.scenario("TR-DEL-105")
async def test_missing_preconditions_and_if_match_are_synchronous_refusals(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The version is a required field on both routes and If-Match never replaces it."""
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    await given_registered(person, other)
    person_before = await assert_exact_entity(
        registry_http, registry_api_path, person, schema_entity(person, 1)
    )
    other_before = await assert_exact_entity(
        registry_http, registry_api_path, other, schema_entity(other, 1)
    )
    if_match = {"If-Match": person_before.headers["etag"]}
    refused_if_match = invalid_argument(
        "If-Match",
        "VALIDATION_FAILED",
        "If-Match is not supported on this route; name the precondition in "
        "expected_resource_version, whose failure is reported on the operation item",
    )

    for response, expected in (
        (
            await delete_one(registry_http, registry_api_path, person["gts_id"], None),
            _requires_version(person),
        ),
        (
            await post_batch_delete(
                registry_http,
                registry_api_path,
                [target(other, 1), {"entity_key": person["gts_id"]}],
            ),
            _requires_version(person),
        ),
        (
            await delete_one(
                registry_http, registry_api_path, person["gts_id"], 1, headers=if_match
            ),
            refused_if_match,
        ),
        (
            await post_batch_delete(
                registry_http, registry_api_path, [target(person, 1)], headers=if_match
            ),
            refused_if_match,
        ),
    ):
        assert "location" not in response.headers, response.headers
        assert_bad_request(response, expected)
    await _unchanged(
        registry_http, registry_api_path, person, schema_entity(person, 1), person_before
    )
    await _unchanged(
        registry_http, registry_api_path, other, schema_entity(other, 1), other_before
    )


@pytest.mark.scenario("TR-DEL-106")
async def test_both_deletion_routes_require_an_idempotency_key(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Without a key a retry could not be told from a new deletion."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    missing_key = invalid_argument(
        "Idempotency-Key", "VALIDATION_FAILED", "an Idempotency-Key header is required"
    )
    for response in (
        await registry_http.delete(
            f"{registry_api_path}/entities/{schema['gts_id']}",
            params={"expected_resource_version": 1},
        ),
        await registry_http.post(
            f"{registry_api_path}/entities:batchDelete",
            json={"items": [target(schema, 1)]},
        ),
    ):
        assert "location" not in response.headers, response.headers
        assert_bad_request(response, missing_key)
    await _unchanged(
        registry_http, registry_api_path, schema, schema_entity(schema, 1), before
    )


@pytest.mark.scenario("TR-DEL-107")
async def test_one_entity_under_two_key_spellings_is_a_duplicate(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Duplicates are detected on the Registry Reference, not on key text."""
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    await given_registered(person, other)
    person_before = await assert_exact_entity(
        registry_http, registry_api_path, person, schema_entity(person, 1)
    )
    other_before = await assert_exact_entity(
        registry_http, registry_api_path, other, schema_entity(other, 1)
    )
    reference = gts_uuid(person["gts_id"])

    response = await post_batch_delete(
        registry_http,
        registry_api_path,
        [target(person, 1), target(other, 1), target(person, 1, key=reference)],
    )
    assert "location" not in response.headers, response.headers
    # Positions, not a key: the two keys share no text, so neither repeats.
    assert_bad_request(
        response,
        invalid_argument(
            "entity_key",
            "VALIDATION_FAILED",
            "items[0] and items[2] name the same entity",
        ),
    )
    await _unchanged(
        registry_http, registry_api_path, person, schema_entity(person, 1), person_before
    )
    await _unchanged(
        registry_http, registry_api_path, other, schema_entity(other, 1), other_before
    )


@pytest.mark.scenario("TR-DEL-108")
async def test_an_unknown_registry_reference_fails_its_item_only(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """An unknown UUID is accepted like an unknown GTS ID, fails alone, and
    reserves nothing."""
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    candidate = schema_with_id(other, other["gts_id"].replace(".other.v1~", ".unregistered.v1~"))
    await given_registered(person)
    unknown = gts_uuid(candidate["gts_id"])
    await assert_absent(registry_http, registry_api_path, other)
    assert_not_found(
        await registry_http.get(f"{registry_api_path}/entities/{unknown}"), not_found(unknown)
    )

    single = await await_deletion(
        registry_http, await delete_one(registry_http, registry_api_path, unknown, 1)
    )
    assert_operation(single, completed("deletion", removal(unknown, "failed", None, "precondition_failed")), ordered=True)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(candidate, 1, key=unknown), target(person, 1), target(other, 1)],
        removal(unknown, "failed", None, "precondition_failed"),
        removal(person, "succeeded", 2),
        removal(other, "failed", None, "precondition_failed"),
    )
    await assert_exact_entity(
        registry_http,
        registry_api_path,
        person,
        schema_entity(person, 2, lifecycle_status="deleted"),
    )
    await assert_absent(registry_http, registry_api_path, other)
    assert_not_found(
        await registry_http.get(f"{registry_api_path}/entities/{unknown}"), not_found(unknown)
    )

    await register_and_assert(
        registry_http, registry_api_path, [candidate], outcome(candidate, "succeeded", 1)
    )
    assert_exact(
        await get_entity(registry_http, registry_api_path, unknown, select=ENTITY_SELECT),
        {"status": 200, "etag": "<etag>", "body": schema_entity(candidate, 1)},
    )
