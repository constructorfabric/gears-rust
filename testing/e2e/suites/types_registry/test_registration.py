"""Registration scenarios with input JSON fixtures and complete expected responses."""

import uuid

import pytest

from .helpers import (
    assert_json,
    assert_not_found,
    assert_operation,
    read_created,
    read_entity,
    replace_text,
    submit_and_poll,
)


# Marks a `$id` the scenario removes rather than replaces.
ABSENT = object()


@pytest.fixture
def register_entities(registry_http, registry_api_path):
    """Submit, await completion, and compare both complete response bodies."""

    async def register(candidates, expected_operation):
        operation = await submit_and_poll(
            registry_http,
            registry_api_path,
            candidates,
            {
                "operation_id": "<operation_id>",
                "status": "<status>",
                "replayed": False,
            },
        )
        assert_operation(operation, expected_operation)
        return operation

    return register


@pytest.mark.smoke
@pytest.mark.scenario("TR-REG-001")
async def test_register_schema(
    registry_http, registry_api_path, registration_fixture, register_entities
):
    """A submitted schema completes, then reads back by GTS ID and by UUID."""
    schema = registration_fixture("person_schema")
    operation = await register_entities(
        [schema],
        {
            "operation_id": "<operation_id>",
            "kind": "registration",
            "dry_run": False,
            "status": "completed",
            "created_at": "<created_at>",
            "started_at": "<started_at>",
            "completed_at": "<completed_at>",
            "items": [
                {
                    "gts_id": schema["gts_id"],
                    "status": "succeeded",
                    "resource_version": 1,
                    "error": None,
                },
            ],
        },
    )
    entity = await read_created(
        registry_http,
        registry_api_path,
        {
            "gts_id": schema["gts_id"],
            "gts_uuid": "<gts_uuid>",
            "kind": "type_schema",
            "lifecycle_status": "active",
            "origin": {
                "type": "managed",
                "resource_version": 1,
                "created_at": "<created_at>",
                "updated_at": "<updated_at>",
            },
            "content": schema["content"],
            "resolved_schema": schema["content"],
            "effective_traits": {},
            "effective_traits_schema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
            },
        },
        operation,
    )
    by_uuid = await read_entity(registry_http, registry_api_path, entity["gts_uuid"])
    assert_json(by_uuid, entity)


@pytest.mark.scenario("TR-REG-002")
async def test_register_instance(
    registry_http, registry_api_path, registration_fixture, register_entities
):
    """An Instance registers against a schema created by an earlier operation."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    await register_entities(
        [schema],
        {
            "operation_id": "<operation_id>",
            "kind": "registration",
            "dry_run": False,
            "status": "completed",
            "created_at": "<created_at>",
            "started_at": "<started_at>",
            "completed_at": "<completed_at>",
            "items": [
                {
                    "gts_id": schema["gts_id"],
                    "status": "succeeded",
                    "resource_version": 1,
                    "error": None,
                },
            ],
        },
    )
    operation = await register_entities(
        [instance],
        {
            "operation_id": "<operation_id>",
            "kind": "registration",
            "dry_run": False,
            "status": "completed",
            "created_at": "<created_at>",
            "started_at": "<started_at>",
            "completed_at": "<completed_at>",
            "items": [
                {
                    "gts_id": instance["gts_id"],
                    "status": "succeeded",
                    "resource_version": 1,
                    "error": None,
                },
            ],
        },
    )
    await read_created(
        registry_http,
        registry_api_path,
        {
            "gts_id": instance["gts_id"],
            "gts_uuid": "<gts_uuid>",
            "kind": "instance",
            "lifecycle_status": "active",
            "origin": {
                "type": "managed",
                "resource_version": 1,
                "created_at": "<created_at>",
                "updated_at": "<updated_at>",
            },
            "content": instance["content"],
        },
        operation,
    )


@pytest.mark.scenario("TR-REG-003")
async def test_register_batch_with_instance_first(
    registry_http, registry_api_path, registration_fixture, register_entities
):
    """A batch that lists an Instance before its schema still registers both."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    operation = await register_entities(
        [instance, schema],
        {
            "operation_id": "<operation_id>",
            "kind": "registration",
            "dry_run": False,
            "status": "completed",
            "created_at": "<created_at>",
            "started_at": "<started_at>",
            "completed_at": "<completed_at>",
            "items": [
                {
                    "gts_id": instance["gts_id"],
                    "status": "succeeded",
                    "resource_version": 1,
                    "error": None,
                },
                {
                    "gts_id": schema["gts_id"],
                    "status": "succeeded",
                    "resource_version": 1,
                    "error": None,
                },
            ],
        },
    )
    for expected_entity in (
        {
            "gts_id": schema["gts_id"],
            "gts_uuid": "<gts_uuid>",
            "kind": "type_schema",
            "lifecycle_status": "active",
            "origin": {
                "type": "managed",
                "resource_version": 1,
                "created_at": "<created_at>",
                "updated_at": "<updated_at>",
            },
            "content": schema["content"],
            "resolved_schema": schema["content"],
            "effective_traits": {},
            "effective_traits_schema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
            },
        },
        {
            "gts_id": instance["gts_id"],
            "gts_uuid": "<gts_uuid>",
            "kind": "instance",
            "lifecycle_status": "active",
            "origin": {
                "type": "managed",
                "resource_version": 1,
                "created_at": "<created_at>",
                "updated_at": "<updated_at>",
            },
            "content": instance["content"],
        },
    ):
        await read_created(registry_http, registry_api_path, expected_entity, operation)


@pytest.mark.scenario("TR-REG-004")
async def test_register_batch_with_partial_failure(
    registry_http, registry_api_path, registration_fixture, register_entities
):
    """A batch keeps its successful item while reporting structured failures."""
    independent = registration_fixture("person_schema")
    broken = registration_fixture("missing_ref_schema")
    blocked = registration_fixture("blocked_instance")
    operation = await register_entities(
        [blocked, broken, independent],
        {
            "operation_id": "<operation_id>",
            "kind": "registration",
            "dry_run": False,
            "status": "completed",
            "created_at": "<created_at>",
            "started_at": "<started_at>",
            "completed_at": "<completed_at>",
            "items": [
                {
                    "gts_id": blocked["gts_id"],
                    "status": "failed",
                    "resource_version": None,
                    "error": {
                        "reason": "blocked_by_dependency",
                        "message": "<message>",
                    },
                },
                {
                    "gts_id": broken["gts_id"],
                    "status": "failed",
                    "resource_version": None,
                    "error": {
                        "reason": "dependency_not_found",
                        "message": "<message>",
                        "dependency_id": broken["content"]["allOf"][0]["$ref"].removeprefix(
                            "gts://"
                        ),
                        "dependency_kind": "ref",
                    },
                },
                {
                    "gts_id": independent["gts_id"],
                    "status": "succeeded",
                    "resource_version": 1,
                    "error": None,
                },
            ],
        },
    )
    for candidate in (broken, blocked):
        response = await registry_http.get(
            f"{registry_api_path}/entities/{candidate['gts_id']}"
        )
        assert_not_found(
            response,
            {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                "title": "Not Found",
                "status": 404,
                "detail": "<detail>",
                "instance": "<request_path>",
                "trace_id": "<trace_id>",
                "context": {
                    "resource_type": "gts.cf.types_registry.registry.type.v1~",
                    "resource_name": candidate["gts_id"],
                },
            },
        )
    await read_created(
        registry_http,
        registry_api_path,
        {
            "gts_id": independent["gts_id"],
            "gts_uuid": "<gts_uuid>",
            "kind": "type_schema",
            "lifecycle_status": "active",
            "origin": {
                "type": "managed",
                "resource_version": 1,
                "created_at": "<created_at>",
                "updated_at": "<updated_at>",
            },
            "content": independent["content"],
            "resolved_schema": independent["content"],
            "effective_traits": {},
            "effective_traits_schema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
            },
        },
        operation,
    )


def _expected_id_refusal(gts_id, description):
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": "<request_path>",
        "trace_id": "<trace_id>",
        "context": {
            "resource_type": "gts.cf.types_registry.registry.type.v1~",
            "resource_name": gts_id,
            "field_violations": [
                {"field": "entity", "reason": "VALIDATION_FAILED", "description": description},
            ],
        },
    }


@pytest.mark.scenario("TR-REG-005")
@pytest.mark.parametrize(
    "declared_id",
    [
        pytest.param(ABSENT, id="absent"),
        pytest.param(7, id="non-string"),
        pytest.param("gts://not a gts id", id="malformed"),
        pytest.param("gts://gts.cf.e2e.registration.other.v1~", id="other-type"),
    ],
)
async def test_register_batch_refuses_mismatched_schema_id(
    registry_http, registry_api_path, registration_fixture, declared_id
):
    """A Type Schema `$id` other than `gts://<gts_id>` refuses the whole batch before 202."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    namespace = schema["gts_id"].removesuffix("person.v1~")
    expected_uri = f"gts://{schema['gts_id']}"
    if declared_id is ABSENT:
        del schema["content"]["$id"]
    elif isinstance(declared_id, str):
        # Keep the other Type Schema inside this test's namespace.
        schema["content"]["$id"] = declared_id.replace("gts.cf.e2e.registration.", namespace)
    else:
        schema["content"]["$id"] = declared_id
    if isinstance(declared_id, str):
        # The declared value is never echoed back.
        description = (
            f"Type Schema '{schema['gts_id']}' declares a top-level $id other than "
            f"'{expected_uri}'"
        )
    else:
        description = (
            f"Type Schema '{schema['gts_id']}' declares no string top-level $id; "
            f"it must be '{expected_uri}'"
        )

    response = await registry_http.post(
        f"{registry_api_path}/entities",
        headers={"Idempotency-Key": str(uuid.uuid4())},
        json={"items": [instance, schema]},
    )
    assert response.status_code == 400, response.text
    assert response.headers["content-type"].startswith("application/problem+json")
    assert "location" not in response.headers, response.headers
    actual = response.json()
    assert actual["instance"] == response.request.url.path, actual
    actual["instance"] = "<request_path>"
    replace_text(actual, "trace_id")
    assert_json(actual, _expected_id_refusal(schema["gts_id"], description))

    for candidate in (instance, schema):
        response = await registry_http.get(
            f"{registry_api_path}/entities/{candidate['gts_id']}"
        )
        assert_not_found(
            response,
            {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                "title": "Not Found",
                "status": 404,
                "detail": "<detail>",
                "instance": "<request_path>",
                "trace_id": "<trace_id>",
                "context": {
                    "resource_type": "gts.cf.types_registry.registry.type.v1~",
                    "resource_name": candidate["gts_id"],
                },
            },
        )
