"""Managed schema dialect and unstable major-zero admission rules."""

from copy import deepcopy

import pytest

from .helpers import (
    assert_absent,
    assert_bad_request,
    assert_exact_entity,
    invalid_argument,
    outcome,
    post_registration,
    register_and_assert,
    schema_entity,
    schema_with_id,
)


@pytest.mark.scenario("TR-REG-801")
async def test_missing_schema_dialect_refuses_the_whole_batch(
    registry_http, registry_api_path, registration_fixture
):
    """Missing top-level dialect rejects the batch before an operation exists."""
    schema = registration_fixture("person_schema")
    instance = registration_fixture("person_instance")
    del schema["content"]["$schema"]
    assert_bad_request(
        await post_registration(registry_http, registry_api_path, [instance, schema]),
        invalid_argument(
            "entity",
            "VALIDATION_FAILED",
            f"'{schema['gts_id']}' declares no top-level $schema",
            resource_name=schema["gts_id"],
        ),
    )
    await assert_absent(registry_http, registry_api_path, schema)
    await assert_absent(registry_http, registry_api_path, instance)


@pytest.mark.scenario("TR-REG-802")
async def test_major_zero_allows_breaking_revision_but_quarantines_instances(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A v0 schema can break compatibility while its Instance is quarantined."""
    schema = registration_fixture("person_major_zero_schema")
    await given_registered(schema)
    revised = deepcopy(schema)
    revised["content"]["properties"]["name"]["type"] = "integer"
    revised["expected_resource_version"] = 1
    await register_and_assert(
        registry_http, registry_api_path, [revised], outcome(schema, "succeeded", 2)
    )
    await assert_exact_entity(
        registry_http, registry_api_path, revised, schema_entity(revised, 2)
    )

    instance = registration_fixture("person_instance")
    instance["gts_id"] = instance["gts_id"].replace(".person.v1~", ".person.v0~")
    # Conforms to the revised v0 schema, so only the quarantine can refuse it.
    instance["content"]["name"] = 42
    await register_and_assert(
        registry_http,
        registry_api_path,
        [instance],
        outcome(instance, "failed", None, "instance_of_major_zero"),
    )
    await assert_absent(registry_http, registry_api_path, instance)


@pytest.mark.scenario("TR-REG-803")
async def test_stable_schemas_cannot_derive_from_or_reference_major_zero(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """Stable schemas fail per item on derivation or reference to major zero."""
    base = registration_fixture("person_major_zero_schema")
    await given_registered(base)
    # `gts.cf.e2e.r<uuid>.person.v0~` + `cf.e2e.r<uuid>.derived.v1~`: the ID
    # derives from the v0 base, while the content is a root with no `$ref` to it.
    namespace = base["gts_id"].removeprefix("gts.").removesuffix("person.v0~")
    derived = schema_with_id(
        registration_fixture("person_schema"),
        f"{base['gts_id']}{namespace}derived.v1~",
    )
    referrer = registration_fixture("person_referrer_schema")
    referrer["content"]["properties"]["payload"]["properties"]["person"]["$ref"] = (
        f"gts://{base['gts_id']}"
    )
    await register_and_assert(
        registry_http,
        registry_api_path,
        [derived, referrer],
        outcome(derived, "failed", None, "stable_derives_from_major_zero"),
        outcome(referrer, "failed", None, "stable_refs_major_zero"),
    )
    await assert_absent(registry_http, registry_api_path, derived)
    await assert_absent(registry_http, registry_api_path, referrer)
