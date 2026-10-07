"""Deletion request identity: replay, conflicts and mode."""

import uuid

import pytest

from .helpers import (
    assert_exact,
    assert_exact_entity,
    assert_json,
    assert_problem,
    await_deletion,
    blocked,
    completed,
    assert_operation,
    delete_and_assert,
    delete_one,
    get_entity,
    gts_uuid,
    key_conflict,
    managed,
    mandatory,
    removal,
    post_batch_delete,
    schema_entity,
    schema_with_id,
    target,
)


async def _assert_replay(client, response, operation, location):
    """A terminal replay: `200`, the original operation and `Location`, no `Retry-After`."""
    assert response.status_code == 200, response.text
    assert response.headers["idempotency-replayed"] == "true"
    assert "retry-after" not in response.headers, response.headers
    assert response.headers["location"] == location
    assert_json(
        response.json(),
        {"operation_id": operation["operation_id"], "status": "completed", "replayed": True},
    )
    polled = await client.get(response.url.join(location))
    assert_json(polled.json(), operation)


async def _single(client, api_path, key, version, idempotency_key):
    return await delete_one(client, api_path, key, version, idempotency_key=idempotency_key)


async def _batch(client, api_path, key, version, idempotency_key):
    return await post_batch_delete(
        client,
        api_path,
        [{"entity_key": key, "expected_resource_version": version}],
        idempotency_key=idempotency_key,
    )


@pytest.mark.scenario("TR-DEL-301")
@pytest.mark.scenario("TR-DEL-305")
async def test_a_deletion_replays_across_routes_and_key_spellings(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Route and key spelling name one request; replay moves nothing."""
    first = deletion_fixture("person_schema")
    second = schema_with_id(first, first["gts_id"].replace(".person.v1~", ".second_person.v1~"))
    await given_registered(first, second)

    workflows = (
        # The identical request, then every other route and spelling.
        (first, [(_single, "gts_id"), (_single, "gts_id"), (_single, "uuid"),
                 (_batch, "gts_id"), (_batch, "uuid")]),
        (second, [(_batch, "uuid"), (_single, "gts_id")]),
    )
    for document, (start, *replays) in workflows:
        keys = {"gts_id": document["gts_id"], "uuid": gts_uuid(document["gts_id"])}
        idempotency_key = str(uuid.uuid4())
        route, spelling = start
        response = await route(
            registry_http, registry_api_path, keys[spelling], 1, idempotency_key
        )
        location = response.headers["location"]
        operation = await await_deletion(registry_http, response)
        assert_operation(
            operation, completed("deletion", removal(keys[spelling], "succeeded", 2)),
            ordered=True
        )
        tombstone = schema_entity(document, 2, lifecycle_status="deleted")
        before = await assert_exact_entity(registry_http, registry_api_path, document, tombstone)

        for route, spelling in replays:
            await _assert_replay(
                registry_http,
                await route(registry_http, registry_api_path, keys[spelling], 1, idempotency_key),
                operation,
                location,
            )
        after = await assert_exact_entity(
            registry_http, registry_api_path, document, tombstone, etag=before.headers["etag"]
        )
        assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-DEL-302")
@pytest.mark.scenario("TR-DEL-209")
async def test_a_refusal_replays_after_its_blocker_disappears(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Replay does not re-evaluate; a new key succeeds at the still-current version."""
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    await given_registered(person, referrer)
    idempotency_key = str(uuid.uuid4())
    response = await _batch(registry_http, registry_api_path, person["gts_id"], 1, idempotency_key)
    location = response.headers["location"]
    refused = await await_deletion(registry_http, response)
    assert_operation(
        refused, completed("deletion", blocked(person, 1)), ordered=True, exact_messages=True
    )
    await delete_and_assert(
        registry_http, registry_api_path, [target(referrer, 1)], removal(referrer, "succeeded", 2)
    )

    await _assert_replay(
        registry_http,
        await _batch(registry_http, registry_api_path, person["gts_id"], 1, idempotency_key),
        refused,
        location,
    )
    # Neither the refusal nor the blocker's deletion moved the target's version.
    await assert_exact_entity(registry_http, registry_api_path, person, schema_entity(person, 1))
    await delete_and_assert(
        registry_http, registry_api_path, [target(person, 1)], removal(person, "succeeded", 2)
    )
    await assert_exact_entity(
        registry_http,
        registry_api_path,
        person,
        schema_entity(person, 2, lifecycle_status="deleted"),
    )
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, referrer["gts_id"], select="content,origin"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(referrer, "deleted"),
                "origin": managed(2),
                "content": referrer["content"],
            },
        },
    )


@pytest.mark.scenario("TR-DEL-303")
async def test_changed_targets_or_preconditions_conflict_with_a_used_key(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    await given_registered(person, other)
    idempotency_key = str(uuid.uuid4())
    response = await _batch(registry_http, registry_api_path, person["gts_id"], 1, idempotency_key)
    location = response.headers["location"]
    operation = await await_deletion(registry_http, response)
    other_before = await assert_exact_entity(
        registry_http, registry_api_path, other, schema_entity(other, 1)
    )

    for key, version in ((other["gts_id"], 1), (person["gts_id"], 2)):
        assert_problem(
            await _batch(registry_http, registry_api_path, key, version, idempotency_key),
            409,
            key_conflict(operation["operation_id"]),
        )
    polled = await registry_http.get(response.url.join(location))
    assert_json(polled.json(), operation)
    await assert_exact_entity(
        registry_http,
        registry_api_path,
        person,
        schema_entity(person, 2, lifecycle_status="deleted"),
    )
    other_after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        other,
        schema_entity(other, 1),
        etag=other_before.headers["etag"],
    )
    assert_json(other_after.json(), other_before.json())


@pytest.mark.scenario("TR-DEL-304")
async def test_dry_run_and_real_deletion_are_different_requests(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    schema = deletion_fixture("person_schema")
    await given_registered(schema)
    idempotency_key = str(uuid.uuid4())
    prediction = await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(schema, 1)],
        removal(schema, "succeeded", None),
        dry_run=True,
        idempotency_key=idempotency_key,
    )

    assert_problem(
        await _batch(registry_http, registry_api_path, schema["gts_id"], 1, idempotency_key),
        409,
        key_conflict(prediction["operation_id"]),
    )
    await assert_exact_entity(registry_http, registry_api_path, schema, schema_entity(schema, 1))
    await delete_and_assert(
        registry_http, registry_api_path, [target(schema, 1)], removal(schema, "succeeded", 2)
    )
