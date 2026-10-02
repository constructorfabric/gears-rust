"""Partial batch outcomes and synchronous authored-identity refusals."""

from copy import deepcopy

import pytest

from .helpers import (
    assert_absent,
    assert_bad_request,
    instance_entity,
    invalid_argument,
    outcome,
    post_registration,
    read_created,
    register_and_assert,
    schema_entity,
)


# Marks a `$id` the scenario removes rather than replaces.
ABSENT = object()


@pytest.mark.scenario("TR-REG-101")
async def test_register_batch_with_partial_failure(
    registry_http, registry_api_path, registration_fixture
):
    """A batch keeps its successful item while reporting structured failures."""
    independent = registration_fixture("person_schema")
    broken = registration_fixture("missing_ref_schema")
    blocked = registration_fixture("blocked_instance")
    operation = await register_and_assert(
        registry_http,
        registry_api_path,
        [blocked, broken, independent],
        outcome(blocked, "failed", None, "blocked_by_dependency"),
        outcome(
            broken,
            "failed",
            None,
            "dependency_not_found",
            dependency_id=broken["content"]["allOf"][0]["$ref"].removeprefix("gts://"),
            dependency_kind="ref",
        ),
        outcome(independent, "succeeded", 1),
    )
    await assert_absent(registry_http, registry_api_path, broken)
    await assert_absent(registry_http, registry_api_path, blocked)
    await read_created(
        registry_http, registry_api_path, schema_entity(independent, 1), operation
    )


@pytest.mark.scenario("TR-REG-102")
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

    assert_bad_request(
        await post_registration(registry_http, registry_api_path, [instance, schema]),
        invalid_argument(
            "entity", "VALIDATION_FAILED", description, resource_name=schema["gts_id"]
        ),
    )
    # Nothing was admitted, including the valid Instance beside the schema.
    await assert_absent(registry_http, registry_api_path, instance)
    await assert_absent(registry_http, registry_api_path, schema)


@pytest.mark.scenario("TR-REG-103")
async def test_register_batch_rejects_invalid_content_for_scalar_schema(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """Content the string schema rejects fails as an item, beside a valid string Instance."""
    schema = registration_fixture("label_schema")
    template = registration_fixture("label_instance")
    await given_registered(schema)

    def label(entity_name, content):
        document = deepcopy(template)
        document["gts_id"] = document["gts_id"].replace(".primary.v1", f".{entity_name}.v1")
        document["content"] = content
        return document

    valid = label("secondary", "secondary")
    invalid = [
        label("object", {}),
        label("integer", 42),
        label("boolean", True),
        label("array", ["primary"]),
        # Present as JSON null, not missing.
        label("null", None),
        # Right JSON type, but below `minLength`.
        label("empty", ""),
    ]

    operation = await register_and_assert(
        registry_http,
        registry_api_path,
        [valid, *invalid],
        outcome(valid, "succeeded", 1),
        *(outcome(document, "failed", None, "invalid_value") for document in invalid),
    )
    await read_created(
        registry_http, registry_api_path, instance_entity(valid, 1), operation
    )
    for document in invalid:
        await assert_absent(registry_http, registry_api_path, document)
