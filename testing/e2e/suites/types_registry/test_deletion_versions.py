"""Deletion within version families: majors, minors and major zero."""

import pytest

from .helpers import (
    assert_absent,
    assert_exact,
    blocked,
    delete_and_assert,
    get_entity,
    managed,
    mandatory,
    outcome,
    removal,
    register_and_assert,
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


@pytest.mark.parametrize("deleted", ["older", "newer"])
@pytest.mark.scenario("TR-DEL-601")
async def test_either_major_is_deleted_without_a_successor(
    registry_http, registry_api_path, deletion_fixture, given_registered, deleted
):
    """Majors have no succession, so deleting one never touches the other."""
    older = deletion_fixture("person_schema")
    newer = schema_with_id(older, older["gts_id"].replace(".person.v1~", ".person.v2~"))
    await given_registered(older, newer)
    removed, kept = (older, newer) if deleted == "older" else (newer, older)
    kept_etag = await _state(registry_http, registry_api_path, kept, 1)

    await delete_and_assert(
        registry_http, registry_api_path, [target(removed, 1)], removal(removed, "succeeded", 2)
    )
    await _state(registry_http, registry_api_path, removed, 2, "deleted")
    await _state(registry_http, registry_api_path, kept, 1, etag=kept_etag)


@pytest.mark.scenario("TR-DEL-602")
async def test_a_middle_minor_is_deleted_while_a_higher_one_stays(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Predecessor ordering at registration is no deletion blocker."""
    minors = [deletion_fixture(f"person_minor_{n}_schema") for n in range(3)]
    await given_registered(*minors)
    etags = [await _state(registry_http, registry_api_path, minor, 1) for minor in minors]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(minors[1], 1)],
        removal(minors[1], "succeeded", 2),
    )
    await _state(registry_http, registry_api_path, minors[1], 2, "deleted")
    for index in (0, 2):
        await _state(registry_http, registry_api_path, minors[index], 1, etag=etags[index])


@pytest.mark.scenario("TR-DEL-603")
async def test_the_next_minor_is_admitted_after_its_predecessor_is_deleted(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A tombstoned predecessor still closes the contiguous sequence."""
    first = deletion_fixture("person_minor_0_schema")
    second = deletion_fixture("person_minor_1_schema")
    third = deletion_fixture("person_minor_2_schema")
    await given_registered(first, second)
    await delete_and_assert(
        registry_http, registry_api_path, [target(second, 1)], removal(second, "succeeded", 2)
    )
    second_etag = await _state(registry_http, registry_api_path, second, 2, "deleted")

    await register_and_assert(
        registry_http, registry_api_path, [third], outcome(third, "succeeded", 1)
    )
    await _state(registry_http, registry_api_path, third, 1)
    await _state(registry_http, registry_api_path, second, 2, "deleted", etag=second_etag)


@pytest.mark.scenario("TR-DEL-604")
async def test_a_new_minor_is_compared_against_its_deleted_predecessor(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """v1.2 matches v1.0 but narrows v1.1, so skipping the tombstone would admit it."""
    first = deletion_fixture("person_minor_0_schema")
    second = deletion_fixture("person_minor_1_schema")
    narrowing = deletion_fixture("person_minor_2_incompatible_schema")
    await given_registered(first, second)
    await delete_and_assert(
        registry_http, registry_api_path, [target(second, 1)], removal(second, "succeeded", 2)
    )
    second_etag = await _state(registry_http, registry_api_path, second, 2, "deleted")

    await register_and_assert(
        registry_http,
        registry_api_path,
        [narrowing],
        outcome(narrowing, "failed", None, "incompatible_with_baseline"),
    )
    await assert_absent(registry_http, registry_api_path, narrowing)
    await _state(registry_http, registry_api_path, second, 2, "deleted", etag=second_etag)


@pytest.mark.scenario("TR-DEL-605")
async def test_a_tombstone_still_fixes_its_majors_shape(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """A family whose only member is deleted is not an empty family."""
    major_only = deletion_fixture("person_schema")
    first_minor = deletion_fixture("person_minor_0_schema")
    await given_registered(major_only)
    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(major_only, 1)],
        removal(major_only, "succeeded", 2),
    )

    await register_and_assert(
        registry_http,
        registry_api_path,
        [first_minor],
        outcome(first_minor, "failed", None, "family_shape_conflict"),
    )
    await assert_absent(registry_http, registry_api_path, first_minor)


@pytest.mark.scenario("TR-DEL-606")
async def test_major_zero_keeps_dependency_safety(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """The unstable compatibility profile does not waive deletion blockers."""
    person = deletion_fixture("person_schema")
    derived = deletion_fixture("derived_employee_schema")
    base = schema_with_id(person, person["gts_id"].replace(".person.v1~", ".person.v0~"))
    dependant = schema_with_id(
        derived,
        derived["gts_id"]
        .replace(".person.v1~", ".person.v0~")
        .replace(".employee.v1~", ".employee.v0~"),
    )
    dependant["content"]["allOf"][0]["$ref"] = f"gts://{base['gts_id']}"
    await given_registered(base, dependant)
    etags = [await _state(registry_http, registry_api_path, doc, 1) for doc in (base, dependant)]

    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(base, 1)],
        blocked(base, 1),
        exact_messages=True,
    )
    for document, etag in zip((base, dependant), etags, strict=True):
        await _state(registry_http, registry_api_path, document, 1, etag=etag)
    await delete_and_assert(
        registry_http,
        registry_api_path,
        [target(base, 1), target(dependant, 1)],
        removal(base, "succeeded", 2),
        removal(dependant, "succeeded", 2),
    )
    for document in (base, dependant):
        await _state(registry_http, registry_api_path, document, 2, "deleted")
