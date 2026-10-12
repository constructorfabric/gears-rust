"""Dry-run deletion as a prediction of the committed outcome."""

from copy import deepcopy

import pytest

from .helpers import (
    assert_exact,
    assert_exact_entity,
    assert_json,
    assert_operation,
    await_deletion,
    blocked,
    completed,
    delete_and_assert,
    delete_one,
    get_entity,
    managed,
    mandatory,
    outcome,
    removal,
    register_and_assert,
    schema_entity,
    schema_with_id,
    target,
)


async def _state(client, api_path, document, version, lifecycle="active", etag="<etag>"):
    """Authored content and origin; a kept ETag proves the entity did not move."""
    return assert_exact(
        await get_entity(client, api_path, document["gts_id"], select="content,origin"),
        {
            "status": 200,
            "etag": etag,
            "body": {
                **mandatory(document, lifecycle),
                "origin": managed(version),
                "content": document["content"],
            },
        },
    )


@pytest.mark.scenario("TR-DEL-401")
async def test_a_single_dry_run_deletion_changes_nothing(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )

    prediction = await await_deletion(
        registry_http,
        await delete_one(registry_http, registry_api_path, schema["gts_id"], 1, dry_run=True),
        dry_run=True,
    )
    assert_operation(
        prediction,
        completed("deletion", removal(schema, "succeeded", None), dry_run=True),
        ordered=True,
    )
    after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 1),
        etag=before.headers["etag"],
    )
    assert_json(after.json(), before.json())
    await delete_and_assert(
        registry_http, registry_api_path, [target(schema, 1)], removal(schema, "succeeded", 2)
    )


@pytest.mark.scenario("TR-DEL-402")
async def test_a_dry_run_predicts_a_whole_graph_through_virtual_deletions(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Virtually deleted dependants let the prediction reach their base."""
    person = deletion_fixture("person_schema")
    derived = deletion_fixture("derived_employee_schema")
    employee = deletion_fixture("employee_instance")
    chain = (person, derived, employee)
    await given_registered(*chain)
    etags = [await _state(registry_http, registry_api_path, document, 1) for document in chain]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(document, 1) for document in chain],
        *(removal(document, "succeeded", None) for document in chain),
        dry_run=True,
    )
    for document, etag in zip(chain, etags, strict=True):
        await _state(registry_http, registry_api_path, document, 1, etag=etag)
    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(document, 1) for document in chain],
        *(removal(document, "succeeded", 2) for document in chain),
    )


@pytest.mark.scenario("TR-DEL-403")
@pytest.mark.scenario("TR-DEL-208")
async def test_a_failed_dependant_keeps_its_target_blocked_in_both_modes(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A stale dependant stays live, so its target is refused for a live
    dependant rather than blocked by an in-batch failure."""
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    other = deletion_fixture("other_schema")
    second = schema_with_id(
        referrer, referrer["gts_id"].replace(".referrer.v1~", ".second_referrer.v1~")
    )
    await given_registered(person, referrer, second, other)
    revised = deepcopy(second)
    revised["content"]["title"] = "Revised Referrer"
    revised["expected_resource_version"] = 1
    await register_and_assert(
        registry_http, registry_api_path, [revised], outcome(second, "succeeded", 2)
    )
    batch = [target(person, 1), target(referrer, 1), target(second, 1), target(other, 1)]
    stale = removal(
        second,
        "failed",
        None,
        "precondition_failed",
        message=f"'{second['gts_id']}' is at resource_version 2, and this deletion expected 1",
    )
    states = [(person, 1), (referrer, 1), (revised, 2), (other, 1)]
    etags = [
        await _state(registry_http, registry_api_path, document, version)
        for document, version in states
    ]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        batch,
        blocked(person, 1),
        removal(referrer, "succeeded", None),
        stale,
        removal(other, "succeeded", None),
        dry_run=True,
        exact_messages=True,
    )
    for (document, version), etag in zip(states, etags, strict=True):
        await _state(registry_http, registry_api_path, document, version, etag=etag)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        batch,
        blocked(person, 1),
        removal(referrer, "succeeded", 2),
        stale,
        removal(other, "succeeded", 2),
        exact_messages=True,
    )
    for document in (referrer, other):
        await _state(registry_http, registry_api_path, document, 2, "deleted")
    await _state(registry_http, registry_api_path, person, 1, etag=etags[0])
    await _state(registry_http, registry_api_path, revised, 2, etag=etags[2])


@pytest.mark.scenario("TR-DEL-405")
async def test_a_passing_dry_run_does_not_reserve_deletion_eligibility(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A new dependant moves no target version, yet the prediction goes stale."""
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    await given_registered(person)
    prediction = await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1)],
        removal(person, "succeeded", None),
        dry_run=True,
    )
    await register_and_assert(
        registry_http, registry_api_path, [referrer], outcome(referrer, "succeeded", 1)
    )

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1)],
        blocked(person, 1),
        exact_messages=True,
    )
    for document in (person, referrer):
        await _state(registry_http, registry_api_path, document, 1)
    polled = await registry_http.get(
        f"{registry_api_path}/operations/{prediction['operation_id']}"
    )
    assert_json(polled.json(), prediction)
