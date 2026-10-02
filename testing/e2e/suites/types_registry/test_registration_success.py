"""Successful Type Schema and Instance registration workflows."""

import pytest

from .helpers import (
    assert_json,
    instance_entity,
    outcome,
    read_created,
    read_entity,
    register_and_assert,
    schema_entity,
)


@pytest.mark.smoke
@pytest.mark.scenario("TR-REG-001")
async def test_register_schema(registry_http, registry_api_path, registration_fixture):
    """A submitted schema completes, then reads back by GTS ID and by UUID."""
    schema = registration_fixture("person_schema")
    operation = await register_and_assert(
        registry_http, registry_api_path, [schema], outcome(schema, "succeeded", 1)
    )
    # A dependency-free root resolves to itself and inherits no traits.
    entity = await read_created(
        registry_http, registry_api_path, schema_entity(schema, 1), operation
    )
    by_uuid = await read_entity(registry_http, registry_api_path, entity["gts_uuid"])
    assert_json(by_uuid, entity)


@pytest.mark.scenario("TR-REG-002")
async def test_register_instance(registry_http, registry_api_path, registration_fixture):
    """An Instance registers against a schema created by an earlier operation."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    await register_and_assert(
        registry_http, registry_api_path, [schema], outcome(schema, "succeeded", 1)
    )
    operation = await register_and_assert(
        registry_http, registry_api_path, [instance], outcome(instance, "succeeded", 1)
    )
    # Schema-only documents are absent from an Instance, not null.
    await read_created(
        registry_http, registry_api_path, instance_entity(instance, 1), operation
    )


@pytest.mark.scenario("TR-REG-003")
async def test_register_batch_with_instance_first(
    registry_http, registry_api_path, registration_fixture
):
    """A batch that lists an Instance before its schema still registers both."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    operation = await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, schema],
        outcome(instance, "succeeded", 1),
        outcome(schema, "succeeded", 1),
    )
    await read_created(registry_http, registry_api_path, schema_entity(schema, 1), operation)
    await read_created(
        registry_http, registry_api_path, instance_entity(instance, 1), operation
    )


@pytest.mark.scenario("TR-REG-004")
async def test_register_scalar_schema_and_string_instance(
    registry_http, registry_api_path, registration_fixture
):
    """A non-object root schema admits an Instance whose content is a bare string."""
    schema = registration_fixture("label_schema")
    instance = registration_fixture("label_instance")
    operation = await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, schema],
        outcome(instance, "succeeded", 1),
        outcome(schema, "succeeded", 1),
    )
    await read_created(registry_http, registry_api_path, schema_entity(schema, 1), operation)
    # The whole-body comparison fails if the string comes back wrapped or re-encoded.
    await read_created(
        registry_http, registry_api_path, instance_entity(instance, 1), operation
    )
