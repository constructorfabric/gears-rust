"""Registration request identity and idempotency as observed over HTTP."""

from copy import deepcopy
import uuid
from urllib.parse import urljoin

import pytest

from .helpers import (
    RECEIPT,
    TRACE_ID,
    assert_exact_entity,
    assert_json,
    assert_not_found,
    assert_operation,
    assert_absent,
    assert_problem,
    assert_uuid,
    completed,
    key_conflict,
    outcome,
    poll_operation,
    post_registration,
    read_created,
    schema_entity,
)


def _assert_receipt(response, *, replayed_operation_id=None):
    """Compare the complete receipt and the headers that tell a replay apart.

    A fresh submission is `202` with an advisory `Retry-After`; a replay of a
    terminal operation is `200` with `Idempotency-Replayed` and no `Retry-After`.
    """
    replayed = replayed_operation_id is not None
    assert response.status_code == (200 if replayed else 202), response.text
    assert response.headers["content-type"].startswith("application/json")
    assert "location" in response.headers, response.headers
    receipt = response.json()
    if replayed:
        assert response.headers["idempotency-replayed"] == "true"
        assert "retry-after" not in response.headers, response.headers
        assert_json(
            receipt,
            {"operation_id": replayed_operation_id, "status": "completed", "replayed": True},
        )
    else:
        assert "retry-after" in response.headers, response.headers
        assert "idempotency-replayed" not in response.headers, response.headers
        assert_uuid(receipt["operation_id"])
        assert receipt["status"] in {"pending", "running", "completed"}, receipt
        assert_json(
            {**receipt, "operation_id": "<operation_id>", "status": "<status>"}, RECEIPT
        )
    return receipt, urljoin(str(response.url), response.headers["location"])


async def _submit_and_await(client, api_path, schema, key, *items):
    """Submit under `key`, check the fresh receipt and the whole terminal operation."""
    receipt, location = _assert_receipt(
        await post_registration(client, api_path, [schema], idempotency_key=key)
    )
    operation = await poll_operation(client, receipt, location, "registration")
    assert_operation(operation, completed("registration", *items))
    return operation, location


@pytest.mark.scenario("TR-REG-201")
async def test_replay_retains_the_creation_outcome_after_a_revision(
    registry_http, registry_api_path, registration_fixture
):
    """A replay returns the original operation after a compatible revision."""
    schema = registration_fixture("person_schema")
    k1 = str(uuid.uuid4())
    original, original_location = await _submit_and_await(
        registry_http, registry_api_path, schema, k1, outcome(schema, "succeeded", 1)
    )

    revised = deepcopy(schema)
    revised["content"]["title"] = "Person with a revised title"
    revised["expected_resource_version"] = 1
    await _submit_and_await(
        registry_http,
        registry_api_path,
        revised,
        str(uuid.uuid4()),
        outcome(schema, "succeeded", 2),
    )
    current = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(revised, 2)
    )

    replay_receipt, replay_location = _assert_receipt(
        await post_registration(registry_http, registry_api_path, [schema], idempotency_key=k1),
        replayed_operation_id=original["operation_id"],
    )
    assert replay_location == original_location
    # The stored version-1 creation, although the entity is now at version 2.
    assert_json(
        await poll_operation(registry_http, replay_receipt, replay_location, "registration"),
        original,
    )
    after_replay = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(revised, 2),
        etag=current.headers["etag"],
    )
    assert_json(after_replay.json(), current.json())


@pytest.mark.scenario("TR-REG-202")
async def test_reusing_a_key_for_different_content_is_a_conflict(
    registry_http, registry_api_path, registration_fixture
):
    """A changed request under K1 conflicts with its original operation."""
    schema = registration_fixture("person_schema")
    k1 = str(uuid.uuid4())
    original, original_location = await _submit_and_await(
        registry_http, registry_api_path, schema, k1, outcome(schema, "succeeded", 1)
    )
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )

    changed = deepcopy(schema)
    changed["content"]["title"] = "A different request under the same key"
    assert_problem(
        await post_registration(registry_http, registry_api_path, [changed], idempotency_key=k1),
        409,
        {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~",
            "title": "Already Exists",
            "status": 409,
            "detail": (
                "this Idempotency-Key is already bound to operation "
                f"{original['operation_id']} with a different request"
            ),
            "instance": "<request_path>",
            "trace_id": TRACE_ID,
            # The conflicting resource is the operation the key is bound to.
            "context": {
                "resource_type": "gts.cf.core.types_registry.operation.v1~",
                "resource_name": original["operation_id"],
            },
        },
    )
    assert_json(
        await poll_operation(registry_http, original, original_location, "registration"),
        original,
    )
    after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 1),
        etag=before.headers["etag"],
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-203")
async def test_a_new_key_does_not_turn_duplicate_creation_into_replay(
    registry_http, registry_api_path, registration_fixture
):
    """An equal creation under a new key gets a failed item and leaves v1 intact."""
    schema = registration_fixture("person_schema")
    original, _ = await _submit_and_await(
        registry_http,
        registry_api_path,
        schema,
        str(uuid.uuid4()),
        outcome(schema, "succeeded", 1),
    )
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(schema, 1)
    )

    duplicate, _ = await _submit_and_await(
        registry_http,
        registry_api_path,
        schema,
        str(uuid.uuid4()),
        outcome(schema, "failed", None, "already_exists"),
    )
    assert duplicate["operation_id"] != original["operation_id"]
    after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(schema, 1),
        etag=before.headers["etag"],
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-204")
async def test_an_unknown_operation_is_a_not_found_naming_the_operation(
    registry_http, registry_api_path
):
    """Polling an operation that was never created names an operation, not an entity."""
    operation_id = str(uuid.uuid4())
    assert_not_found(
        await registry_http.get(f"{registry_api_path}/operations/{operation_id}"),
        {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "<detail>",
            "instance": "<request_path>",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.types_registry.operation.v1~",
                "resource_name": operation_id,
            },
        },
    )


@pytest.mark.scenario("TR-REG-205")
async def test_replay_keeps_a_refusal_after_its_dependency_arrives(
    registry_http, registry_api_path, registration_fixture
):
    """Replay does not re-evaluate; a new key does."""
    target = registration_fixture("person_schema")
    referrer = registration_fixture("person_referrer_schema")
    k1 = str(uuid.uuid4())
    refused, refused_location = await _submit_and_await(
        registry_http,
        registry_api_path,
        referrer,
        k1,
        outcome(
            referrer,
            "failed",
            None,
            "dependency_not_found",
            dependency_id=target["gts_id"],
            dependency_kind="ref",
        ),
    )
    await _submit_and_await(
        registry_http,
        registry_api_path,
        target,
        str(uuid.uuid4()),
        outcome(target, "succeeded", 1),
    )
    target_read = await assert_exact_entity(
        registry_http, registry_api_path, target, schema_entity(target, 1)
    )

    replay_receipt, replay_location = _assert_receipt(
        await post_registration(registry_http, registry_api_path, [referrer], idempotency_key=k1),
        replayed_operation_id=refused["operation_id"],
    )
    assert replay_location == refused_location
    assert_json(
        await poll_operation(registry_http, replay_receipt, replay_location, "registration"),
        refused,
    )
    await assert_absent(registry_http, registry_api_path, referrer)

    admitted, _ = await _submit_and_await(
        registry_http,
        registry_api_path,
        referrer,
        str(uuid.uuid4()),
        outcome(referrer, "succeeded", 1),
    )
    resolved = deepcopy(referrer["content"])
    resolved["properties"]["payload"]["properties"]["person"] = {
        key: value for key, value in target["content"].items() if key not in {"$id", "$schema"}
    }
    await read_created(
        registry_http,
        registry_api_path,
        schema_entity(referrer, 1, resolved_schema=resolved),
        admitted,
    )
    await assert_exact_entity(
        registry_http,
        registry_api_path,
        target,
        schema_entity(target, 1),
        etag=target_read.headers["etag"],
    )


@pytest.mark.scenario("TR-REG-206")
async def test_changing_only_the_precondition_conflicts_with_a_used_key(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """The precondition is part of the request a key is bound to."""
    schema = registration_fixture("person_schema")
    await given_registered(schema)
    current = deepcopy(schema)
    current["content"]["title"] = "Current Person"
    current["expected_resource_version"] = 1
    await _submit_and_await(
        registry_http,
        registry_api_path,
        current,
        str(uuid.uuid4()),
        outcome(schema, "succeeded", 2),
    )
    candidate = deepcopy(current)
    candidate["content"]["description"] = "Updated description"
    k1 = str(uuid.uuid4())
    stale, stale_location = await _submit_and_await(
        registry_http,
        registry_api_path,
        candidate,
        k1,
        outcome(schema, "failed", None, "precondition_failed"),
    )
    before = await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(current, 2)
    )

    corrected = {**candidate, "expected_resource_version": 2}
    assert_problem(
        await post_registration(
            registry_http, registry_api_path, [corrected], idempotency_key=k1
        ),
        409,
        key_conflict(stale["operation_id"]),
    )
    assert_json(
        await poll_operation(registry_http, stale, stale_location, "registration"),
        stale,
    )
    await assert_exact_entity(
        registry_http,
        registry_api_path,
        schema,
        schema_entity(current, 2),
        etag=before.headers["etag"],
    )

    await _submit_and_await(
        registry_http,
        registry_api_path,
        corrected,
        str(uuid.uuid4()),
        outcome(schema, "succeeded", 3),
    )
    await assert_exact_entity(
        registry_http, registry_api_path, schema, schema_entity(corrected, 3)
    )
