"""Deletion scenarios: tombstones, dependant-first ordering and refusals."""

import pytest

from .helpers import (
    assert_operation,
    delete_batch_and_poll,
    delete_one_and_poll,
    read_entity,
    read_tombstone,
    submit_and_poll,
)


RECEIPT = {
    "operation_id": "<operation_id>",
    "status": "<status>",
    "replayed": False,
}


def completed(items):
    """A terminal deletion operation carrying exactly these item outcomes."""
    return {
        "operation_id": "<operation_id>",
        "kind": "deletion",
        "dry_run": False,
        "status": "completed",
        "created_at": "<created_at>",
        "started_at": "<started_at>",
        "completed_at": "<completed_at>",
        "items": items,
    }


def deleted(gts_id, resource_version):
    """A successful deletion reports the version its tombstone now carries."""
    return {
        "gts_id": gts_id,
        "status": "succeeded",
        "resource_version": resource_version,
        "error": None,
    }


def refused(gts_id, reason):
    """A refusal allocates no version; the message wording is not a contract."""
    return {
        "gts_id": gts_id,
        "status": "failed",
        "resource_version": None,
        "error": {"reason": reason, "message": "<message>"},
    }


@pytest.fixture
def given_registered(registry_http, registry_api_path):
    """Register prerequisites, failing loudly if the setup itself did not commit."""

    async def register(*items):
        operation = await submit_and_poll(
            registry_http, registry_api_path, list(items), RECEIPT
        )
        assert all(item["status"] == "succeeded" for item in operation["items"]), (
            f"the scenario's precondition did not register: {operation}"
        )
        return operation

    return register


@pytest.mark.smoke
@pytest.mark.scenario("TR-DEL-001")
async def test_delete_one_entity_leaves_a_readable_tombstone(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A deleted entity keeps its whole body and reads back as a tombstone."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    before = await read_entity(registry_http, registry_api_path, schema["gts_id"])
    assert before["lifecycle_status"] == "active", before
    assert before["resource_version"] == 1, before

    operation = await delete_one_and_poll(
        registry_http, registry_api_path, schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        operation, completed([deleted(schema["gts_id"], 2)]), ordered=True
    )
    await read_tombstone(registry_http, registry_api_path, before, operation)


@pytest.mark.scenario("TR-DEL-002")
async def test_batch_deletion_orders_dependants_before_their_target(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A batch naming a schema before its own Instance still deletes both."""
    schema = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    await given_registered(schema, instance)

    # The target is submitted *before* its dependant deliberately: executing the
    # batch in submission order would refuse the schema with
    # has_registered_dependents. Deletion reorders dependant-first instead.
    operation = await delete_batch_and_poll(
        registry_http,
        registry_api_path,
        [
            {"key": schema["gts_id"], "expected_resource_version": 1},
            {"key": instance["gts_id"], "expected_resource_version": 1},
        ],
        RECEIPT,
    )
    assert_operation(
        operation,
        completed([deleted(schema["gts_id"], 2), deleted(instance["gts_id"], 2)]),
        ordered=True,
    )
    for candidate in (schema, instance):
        body = await read_entity(registry_http, registry_api_path, candidate["gts_id"])
        assert body["lifecycle_status"] == "deleted", body


@pytest.mark.scenario("TR-DEL-003")
async def test_batch_deletion_reports_outcomes_in_request_order(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Outcomes arrive in request order, which is what UUID callers match on."""
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    await given_registered(person, other)
    # Two independent schemas, so nothing reorders them for dependency reasons.
    # `other.v1~` sorts before `person.v1~`, so submitting person first makes
    # request order distinguishable from identifier order.
    assert other["gts_id"] < person["gts_id"]

    operation = await delete_batch_and_poll(
        registry_http,
        registry_api_path,
        [
            {"key": person["gts_id"], "expected_resource_version": 1},
            {"key": other["gts_id"], "expected_resource_version": 1},
        ],
        RECEIPT,
    )
    assert_operation(
        operation,
        completed([deleted(person["gts_id"], 2), deleted(other["gts_id"], 2)]),
        ordered=True,
    )


@pytest.mark.scenario("TR-DEL-004")
async def test_a_stale_expected_version_is_a_terminal_item_not_a_412(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A failed precondition is reported on the operation, not as HTTP 412."""
    schema = deletion_fixture("person_schema")
    await given_registered(schema)

    # The submission is still accepted with 202 — asserted inside the helper —
    # because the precondition is checked asynchronously at admission.
    operation = await delete_one_and_poll(
        registry_http, registry_api_path, schema["gts_id"], 7, RECEIPT
    )
    assert_operation(
        operation,
        completed([refused(schema["gts_id"], "precondition_failed")]),
        ordered=True,
    )

    after = await read_entity(registry_http, registry_api_path, schema["gts_id"])
    assert after["lifecycle_status"] == "active", after
    assert after["resource_version"] == 1, after


@pytest.mark.scenario("TR-DEL-005")
async def test_a_live_dependant_outside_the_batch_blocks_the_deletion(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Ordering resolves in-batch dependants only; external ones refuse."""
    schema = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    await given_registered(schema, instance)

    # Only the schema is named, so its Instance stays live and the commit-time
    # check refuses. The refusal reports how many dependants, never which ones.
    operation = await delete_one_and_poll(
        registry_http, registry_api_path, schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        operation,
        completed([refused(schema["gts_id"], "has_registered_dependents")]),
        ordered=True,
    )

    after = await read_entity(registry_http, registry_api_path, schema["gts_id"])
    assert after["lifecycle_status"] == "active", after
    assert after["resource_version"] == 1, after
