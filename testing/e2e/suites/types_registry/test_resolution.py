"""Batch resolution scenarios: `:batchResolve` on both planes, compared whole."""

import uuid

import pytest

from .helpers import (
    PLATFORM_API,
    RECEIPT,
    TENANT_API,
    TRACE_ID,
    assert_bad_request,
    assert_json,
    assert_operation,
    assert_resolutions,
    batch_resolve,
    completed,
    delete_one_and_poll,
    gts_uuid,
    invalid_argument,
    removal,
    resolved,
)


@pytest.fixture
def registry_api_path():
    """Register fixtures through the platform API."""
    return PLATFORM_API


@pytest.mark.scenario("TR-READ-401")
async def test_both_planes_resolve_every_distinct_key(
    registry_http, registry_tenant_http, reading_fixture, given_registered
):
    """Both planes return the stored pair for each distinct registered key."""
    person_schema, person_instance, other_schema, other_instance = map(
        reading_fixture,
        ("person_schema", "person_instance", "other_schema", "other_instance"),
    )
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    schema_id = person_schema["gts_id"]
    reference = gts_uuid(schema_id)
    absent_id = schema_id.replace("person.v1~", "absent.v1~")
    absent_uuid = str(uuid.uuid4())
    keys = [
        schema_id,
        # The first mention of the reference; the lower-case one collapses onto it.
        reference.upper(),
        person_instance["gts_id"],
        schema_id,
        reference,
        absent_id,
        absent_uuid,
    ]
    expected = (
        resolved(schema_id, person_schema),
        # Echoed canonically, as a batch read echoes it.
        resolved(reference, person_schema),
        resolved(person_instance["gts_id"], person_instance),
        resolved(absent_id),
        resolved(absent_uuid),
    )

    assert_resolutions(await batch_resolve(registry_http, PLATFORM_API, keys), *expected)
    assert_resolutions(await batch_resolve(registry_tenant_http, TENANT_API, keys), *expected)


@pytest.mark.scenario("TR-READ-402")
async def test_a_deleted_entity_keeps_its_identity(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Identity is permanent: a tombstone resolves by either key, unchanged."""
    person_schema = reading_fixture("person_schema")
    await given_registered(person_schema)
    schema_id = person_schema["gts_id"]
    absent_id = schema_id.replace("person.v1~", "absent.v1~")
    keys = [schema_id, gts_uuid(schema_id), absent_id]
    expected = (
        resolved(schema_id, person_schema),
        resolved(gts_uuid(schema_id), person_schema),
        resolved(absent_id),
    )
    assert_resolutions(await batch_resolve(registry_http, registry_api_path, keys), *expected)

    operation = await delete_one_and_poll(
        registry_http, registry_api_path, schema_id, 1, RECEIPT
    )
    assert_operation(
        operation, completed("deletion", removal(person_schema, "succeeded", 2)), ordered=True
    )

    assert_resolutions(await batch_resolve(registry_http, registry_api_path, keys), *expected)


@pytest.mark.scenario("TR-READ-404")
async def test_a_resolution_takes_no_selection_or_condition(
    registry_http, registry_api_path, reading_fixture
):
    """Reject `$select` and `if_none_match` even for a valid, unregistered key."""
    schema_id = reading_fixture("person_schema")["gts_id"]

    assert_bad_request(
        await batch_resolve(
            registry_http, registry_api_path, [schema_id], params={"$select": "gts_id"}
        ),
        invalid_argument(
            "$select",
            "UNSUPPORTED_QUERY_PARAM",
            "unsupported query parameter `$select`; this route accepts no query parameters",
        ),
    )

    conditional = await registry_http.post(
        f"{registry_api_path}/entities:batchResolve",
        json={"items": [{"entity_key": schema_id, "if_none_match": '"x"'}]},
    )
    assert conditional.status_code == 422, conditional.text
    assert conditional.headers["content-type"].startswith("application/problem+json")
    problem = conditional.json()
    # Check the field name and ignore the deserializer's wording and column.
    [violation] = problem["context"]["field_violations"]
    assert "if_none_match" in violation["description"], violation
    violation["description"] = "<description>"
    assert_json(
        problem,
        {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 422,
            "detail": "Request validation failed",
            "instance": conditional.request.url.path,
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.http.request.v1~",
                "field_violations": [
                    {
                        "field": "body",
                        "reason": "invalid_json_body",
                        "description": "<description>",
                    }
                ],
            },
        },
    )
