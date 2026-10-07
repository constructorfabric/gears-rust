"""Dry-run registration as an HTTP prediction of a later commit."""

from copy import deepcopy
import uuid

import pytest

from .helpers import (
    assert_absent,
    assert_exact_entity,
    assert_json,
    assert_problem,
    instance_entity,
    key_conflict,
    outcome,
    post_registration,
    read_created,
    register_and_assert,
    schema_entity,
    schema_with_id,
)


async def _unchanged(client, api_path, document, expected, before):
    """The whole read and its validator are exactly what they were."""
    after = await assert_exact_entity(
        client, api_path, document, expected, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-701")
@pytest.mark.scenario("TR-REG-207")
async def test_dry_run_predicts_creation_then_new_key_commits_it(
    registry_http, registry_api_path, registration_fixture
):
    """Dry-run predicts both creations without reserving either identifier."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    dry_key = str(uuid.uuid4())
    commit_key = str(uuid.uuid4())

    predicted = await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, schema],
        outcome(instance, "succeeded", None),
        outcome(schema, "succeeded", None),
        dry_run=True,
        idempotency_key=dry_key,
    )
    await assert_absent(registry_http, registry_api_path, schema)
    await assert_absent(registry_http, registry_api_path, instance)

    # The mode is part of the request, so the prediction's key cannot commit it.
    assert_problem(
        await post_registration(
            registry_http, registry_api_path, [instance, schema], idempotency_key=dry_key
        ),
        409,
        key_conflict(predicted["operation_id"]),
    )
    await assert_absent(registry_http, registry_api_path, schema)
    await assert_absent(registry_http, registry_api_path, instance)
    assert_json(
        (
            await registry_http.get(
                f"{registry_api_path}/operations/{predicted['operation_id']}"
            )
        ).json(),
        predicted,
    )

    committed = await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, schema],
        outcome(instance, "succeeded", 1),
        outcome(schema, "succeeded", 1),
        idempotency_key=commit_key,
    )
    assert committed["operation_id"] != predicted["operation_id"]
    await read_created(registry_http, registry_api_path, schema_entity(schema, 1), committed)
    await read_created(
        registry_http, registry_api_path, instance_entity(instance, 1), committed
    )


@pytest.mark.scenario("TR-REG-702")
async def test_mixed_dry_run_predicts_committed_item_verdicts(
    registry_http, registry_api_path, registration_fixture
):
    """Dry-run and commit agree on each item verdict in a partial batch."""
    independent = registration_fixture("person_schema")
    broken = registration_fixture("missing_ref_schema")
    blocked = registration_fixture("blocked_instance")
    candidates = [blocked, broken, independent]
    missing_id = broken["content"]["allOf"][0]["$ref"].removeprefix("gts://")
    dry_key = str(uuid.uuid4())
    commit_key = str(uuid.uuid4())

    predicted = await register_and_assert(
        registry_http,
        registry_api_path,
        candidates,
        outcome(blocked, "failed", None, "blocked_by_dependency"),
        outcome(
            broken,
            "failed",
            None,
            "dependency_not_found",
            dependency_id=missing_id,
            dependency_kind="ref",
        ),
        outcome(independent, "succeeded", None),
        dry_run=True,
        idempotency_key=dry_key,
    )
    for document in candidates:
        await assert_absent(registry_http, registry_api_path, document)

    committed = await register_and_assert(
        registry_http,
        registry_api_path,
        candidates,
        outcome(blocked, "failed", None, "blocked_by_dependency"),
        outcome(
            broken,
            "failed",
            None,
            "dependency_not_found",
            dependency_id=missing_id,
            dependency_kind="ref",
        ),
        outcome(independent, "succeeded", 1),
        idempotency_key=commit_key,
    )
    assert committed["operation_id"] != predicted["operation_id"]

    def verdicts(operation):
        return {
            item["gts_id"]: {
                "status": item["status"],
                "reason": None if item["error"] is None else item["error"]["reason"],
            }
            for item in operation["items"]
        }

    assert_json(verdicts(predicted), verdicts(committed))
    for document in (broken, blocked):
        await assert_absent(registry_http, registry_api_path, document)
    await read_created(
        registry_http, registry_api_path, schema_entity(independent, 1), committed
    )


@pytest.mark.scenario("TR-REG-703")
async def test_a_dry_run_predicts_a_revision_beside_an_unchanged_schema(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """Only the committed revision moves a version; `unchanged` never does."""
    person = registration_fixture("person_schema")
    other = schema_with_id(person, person["gts_id"].replace(".person.v1~", ".other_person.v1~"))
    await given_registered(person, other)
    person_before = await assert_exact_entity(
        registry_http, registry_api_path, person, schema_entity(person, 1)
    )
    other_before = await assert_exact_entity(
        registry_http, registry_api_path, other, schema_entity(other, 1)
    )
    revision = deepcopy(person)
    revision["content"]["properties"]["nickname"] = {"type": "string"}
    revision["expected_resource_version"] = 1
    same = {**other, "expected_resource_version": 1}

    await register_and_assert(
        registry_http,
        registry_api_path,
        [revision, same],
        outcome(person, "succeeded", None),
        outcome(other, "unchanged", 1),
        dry_run=True,
    )
    await _unchanged(
        registry_http, registry_api_path, person, schema_entity(person, 1), person_before
    )
    await _unchanged(
        registry_http, registry_api_path, other, schema_entity(other, 1), other_before
    )

    await register_and_assert(
        registry_http,
        registry_api_path,
        [revision, same],
        outcome(person, "succeeded", 2),
        outcome(other, "unchanged", 1),
    )
    await assert_exact_entity(
        registry_http, registry_api_path, person, schema_entity(revision, 2)
    )
    await _unchanged(
        registry_http, registry_api_path, other, schema_entity(other, 1), other_before
    )


@pytest.mark.scenario("TR-REG-704")
async def test_an_instance_is_predicted_against_its_schemas_virtual_revision(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """The stored schema would refuse `nickname`; the batch's revision admits it."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    await given_registered(schema)
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )
    revision = deepcopy(schema)
    revision["content"]["properties"]["nickname"] = {"type": "string"}
    revision["expected_resource_version"] = 1
    instance["content"]["nickname"] = "Al"

    await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, revision],
        outcome(instance, "succeeded", None),
        outcome(schema, "succeeded", None),
        dry_run=True,
    )
    await _unchanged(
        registry_http, registry_api_path, schema, schema_entity(schema, 1), before
    )
    await assert_absent(registry_http, registry_api_path, instance)

    committed = await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, revision],
        outcome(instance, "succeeded", 1),
        outcome(schema, "succeeded", 2),
    )
    await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(revision, 2)
    )
    await read_created(
        registry_http, registry_api_path, instance_entity(instance, 1), committed
    )


@pytest.mark.scenario("TR-REG-705")
async def test_a_passing_dry_run_does_not_reserve_the_observed_version(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A later writer wins; the predicted revision then fails its precondition."""
    schema = registration_fixture("person_schema")
    await given_registered(schema)
    proposed = deepcopy(schema)
    proposed["content"]["description"] = "Proposed description"
    proposed["expected_resource_version"] = 1
    prediction = await register_and_assert(
        registry_http,
        registry_api_path,
        [proposed],
        outcome(schema, "succeeded", None),
        dry_run=True,
    )
    concurrent = deepcopy(schema)
    concurrent["content"]["title"] = "Concurrent title"
    concurrent["expected_resource_version"] = 1
    await register_and_assert(
        registry_http, registry_api_path, [concurrent], outcome(schema, "succeeded", 2)
    )
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(concurrent, 2)
    )

    await register_and_assert(
        registry_http,
        registry_api_path,
        [proposed],
        outcome(schema, "failed", None, "precondition_failed"),
    )
    await _unchanged(
        registry_http, registry_api_path, schema, schema_entity(concurrent, 2), before
    )
    polled = await registry_http.get(
        f"{registry_api_path}/operations/{prediction['operation_id']}"
    )
    assert_json(polled.json(), prediction)
