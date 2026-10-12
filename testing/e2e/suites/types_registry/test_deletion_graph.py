"""Dependency ordering, live-dependant refusals and partial deletion."""

from copy import deepcopy

import pytest

from .helpers import (
    assert_exact,
    assert_exact_entity,
    blocked,
    delete_and_assert,
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


def _referrer_copy(referrer, name, target_document):
    copy = schema_with_id(referrer, referrer["gts_id"].replace(".referrer.v1~", f".{name}.v1~"))
    copy["content"]["properties"]["payload"]["properties"]["person"] = {
        "$ref": f"gts://{target_document['gts_id']}"
    }
    return copy


@pytest.mark.scenario("TR-DEL-201")
@pytest.mark.scenario("TR-DEL-404")
async def test_a_live_instance_blocks_deletion_of_its_schema(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The count names no dependant; a dry run assumes no unrequested removal."""
    schema = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    await given_registered(schema, instance)
    etags = [
        await _state(registry_http, registry_api_path, document, 1)
        for document in (schema, instance)
    ]

    for dry_run in (True, False):
        await delete_and_assert(
            registry_http,
            registry_api_path,
            [target(schema, 1)],
            blocked(schema, 1),
            dry_run=dry_run,
            exact_messages=True,
        )
        for document, etag in zip((schema, instance), etags, strict=True):
            await _state(registry_http, registry_api_path, document, 1, etag=etag)


@pytest.mark.scenario("TR-DEL-202")
async def test_an_instance_is_deleted_before_its_schema_despite_request_order(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A batch naming a schema before its own Instance still deletes both."""
    schema = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    await given_registered(schema, instance)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(schema, 1), target(instance, 1)],
        removal(schema, "succeeded", 2),
        removal(instance, "succeeded", 2),
    )
    for document in (schema, instance):
        await _state(registry_http, registry_api_path, document, 2, "deleted")


def _derived_by_identifier(derived, base):
    """The derived schema with the base's constraints restated instead of
    `$ref`d, so derivation is its only edge; dropping them would widen the
    closed base, which GTS refuses."""
    variant = deepcopy(derived)
    content = variant["content"]
    del content["allOf"]
    content["properties"] = {
        **base["content"]["properties"],
        "payload": content["properties"]["payload"],
    }
    content["required"] = base["content"]["required"]
    content["additionalProperties"] = base["content"]["additionalProperties"]
    return variant


@pytest.mark.parametrize(
    "by_identifier",
    [
        pytest.param(True, marks=pytest.mark.scenario("TR-DEL-203"), id="derivation-only"),
        pytest.param(False, marks=pytest.mark.scenario("TR-DEL-213"), id="derivation-and-ref"),
    ],
)
async def test_a_derived_schema_is_one_dependant_of_its_base(
    registry_http, registry_api_path, deletion_fixture, given_registered, by_identifier
):
    """Derivation alone protects the base, and a second edge kind to the same
    base does not count the derived schema twice."""
    base = deletion_fixture("person_schema")
    derived = deletion_fixture("derived_employee_schema")
    if by_identifier:
        derived = _derived_by_identifier(derived, base)
    await given_registered(base, derived)
    etags = [
        await _state(registry_http, registry_api_path, document, 1)
        for document in (base, derived)
    ]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(base, 1)],
        blocked(base, 1),
        exact_messages=True,
    )
    for document, etag in zip((base, derived), etags, strict=True):
        await _state(registry_http, registry_api_path, document, 1, etag=etag)
    if not by_identifier:
        await delete_and_assert(
            registry_http,
            registry_api_path,
            [target(base, 1), target(derived, 1)],
            removal(base, "succeeded", 2),
            removal(derived, "succeeded", 2),
        )


@pytest.mark.scenario("TR-DEL-204")
async def test_a_schema_reference_blocks_deletion_of_its_target(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    await given_registered(person, referrer)
    etags = [
        await _state(registry_http, registry_api_path, document, 1)
        for document in (person, referrer)
    ]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1)],
        blocked(person, 1),
        exact_messages=True,
    )
    for document, etag in zip((person, referrer), etags, strict=True):
        await _state(registry_http, registry_api_path, document, 1, etag=etag)


@pytest.mark.scenario("TR-DEL-205")
async def test_a_complete_derivation_and_conformance_chain_is_deleted(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The worker deletes from the far end, whatever the request order."""
    person = deletion_fixture("person_schema")
    derived = deletion_fixture("derived_employee_schema")
    employee = deletion_fixture("employee_instance")
    await given_registered(person, derived, employee)
    chain = (person, derived, employee)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(document, 1) for document in chain],
        *(removal(document, "succeeded", 2) for document in chain),
    )
    for document in chain:
        await _state(registry_http, registry_api_path, document, 2, "deleted")


@pytest.mark.scenario("TR-DEL-206")
async def test_every_branch_is_deleted_before_their_shared_target(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    second = _referrer_copy(referrer, "second_referrer", person)
    await given_registered(person, referrer, second)
    batch = (person, referrer, second)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(document, 1) for document in batch],
        *(removal(document, "succeeded", 2) for document in batch),
    )
    for document in batch:
        await _state(registry_http, registry_api_path, document, 2, "deleted")


@pytest.mark.scenario("TR-DEL-207")
async def test_partial_success_stands_when_a_dependant_is_outside_the_batch(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The in-batch referrer is deleted and stays deleted; its target is refused."""
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    second = _referrer_copy(referrer, "second_referrer", person)
    await given_registered(person, referrer, second)
    person_etag = await _state(registry_http, registry_api_path, person, 1)
    second_etag = await _state(registry_http, registry_api_path, second, 1)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1), target(referrer, 1)],
        blocked(person, 1),
        removal(referrer, "succeeded", 2),
        exact_messages=True,
    )
    await _state(registry_http, registry_api_path, referrer, 2, "deleted")
    await _state(registry_http, registry_api_path, person, 1, etag=person_etag)
    await _state(registry_http, registry_api_path, second, 1, etag=second_etag)


@pytest.mark.scenario("TR-DEL-210")
async def test_only_the_current_revisions_references_protect_a_target(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A reference the referrer's history retains protects nothing."""
    person = deletion_fixture("person_schema")
    replacement = schema_with_id(
        person, person["gts_id"].replace(".person.v1~", ".replacement_person.v1~")
    )
    referrer = deletion_fixture("person_referrer_schema")
    await given_registered(person, replacement, referrer)
    revised = deepcopy(referrer)
    revised["content"]["properties"]["payload"]["properties"]["person"]["$ref"] = (
        f"gts://{replacement['gts_id']}"
    )
    revised["expected_resource_version"] = 1
    await register_and_assert(
        registry_http, registry_api_path, [revised], outcome(referrer, "succeeded", 2)
    )
    revised_etag = await _state(registry_http, registry_api_path, revised, 2)

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1), target(replacement, 1)],
        removal(person, "succeeded", 2),
        blocked(replacement, 1),
        exact_messages=True,
    )
    await _state(registry_http, registry_api_path, revised, 2, etag=revised_etag)


@pytest.mark.scenario("TR-DEL-211")
async def test_an_x_gts_ref_constraint_does_not_protect_its_named_entity(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """`x-gts-ref` validates a value's spelling; it is no edge and causes no refresh."""
    person = deletion_fixture("person_schema")
    holder = deletion_fixture("person_x_gts_ref_schema")
    await given_registered(person, holder)
    before = await assert_exact_entity(
        registry_http, registry_api_path, holder, schema_entity(holder, 1)
    )

    await delete_and_assert(
        registry_http, registry_api_path, [target(person, 1)], removal(person, "succeeded", 2)
    )
    after = await assert_exact_entity(
        registry_http,
        registry_api_path,
        holder,
        schema_entity(holder, 1),
        etag=before.headers["etag"],
    )
    assert after.json() == before.json()


@pytest.mark.scenario("TR-DEL-212")
async def test_only_direct_dependants_are_counted(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """An indirect referrer reaches the target only through a direct one."""
    person = deletion_fixture("person_schema")
    referrer = deletion_fixture("person_referrer_schema")
    indirect = _referrer_copy(referrer, "indirect_referrer", referrer)
    await given_registered(person, referrer, indirect)
    chain = (person, referrer, indirect)
    etags = [await _state(registry_http, registry_api_path, document, 1) for document in chain]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(person, 1)],
        blocked(person, 1),
        exact_messages=True,
    )
    for document, etag in zip(chain, etags, strict=True):
        await _state(registry_http, registry_api_path, document, 1, etag=etag)
