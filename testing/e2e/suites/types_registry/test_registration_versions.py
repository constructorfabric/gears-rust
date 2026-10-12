"""Minor-version ordering, family shape and immutable minor definitions."""

from copy import deepcopy

import pytest

from .helpers import (
    assert_absent,
    assert_bad_request,
    assert_exact,
    assert_exact_entity,
    assert_json,
    get_entity,
    invalid_argument,
    mandatory,
    outcome,
    provenance,
    read_created,
    post_registration,
    register_and_assert,
    schema_entity,
    schema_with_id,
)


async def _read(client, api_path, document, version, etag="<etag>"):
    return await assert_exact_entity(
        client, api_path, document, schema_entity(document, version), etag=etag
    )


@pytest.mark.scenario("TR-REG-601")
async def test_contiguous_minors_are_ordered_within_a_batch(
    registry_http, registry_api_path, registration_fixture
):
    """The worker registers contiguous minors even when the request reverses them."""
    first = registration_fixture("person_minor_0_schema")
    second = registration_fixture("person_minor_1_schema")
    await register_and_assert(
        registry_http,
        registry_api_path,
        [second, first],
        outcome(second, "succeeded", 1),
        outcome(first, "succeeded", 1),
    )
    first_read = await _read(registry_http, registry_api_path, first, 1)
    second_read = await _read(registry_http, registry_api_path, second, 1)
    assert first_read.json()["gts_uuid"] != second_read.json()["gts_uuid"]


@pytest.mark.scenario("TR-REG-602")
async def test_missing_predecessor_refuses_later_minor(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A later minor cannot skip its immediately preceding minor."""
    first = registration_fixture("person_minor_0_schema")
    skipped = registration_fixture("person_minor_2_schema")
    await given_registered(first)
    before = await _read(registry_http, registry_api_path, first, 1)
    await register_and_assert(
        registry_http,
        registry_api_path,
        [skipped],
        outcome(skipped, "failed", None, "missing_predecessor"),
    )
    await assert_absent(registry_http, registry_api_path, skipped)
    after = await _read(
        registry_http, registry_api_path, first, 1, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-603")
async def test_major_only_and_minor_shapes_conflict_in_both_directions(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A major cannot acquire the opposite identifier shape after admission."""
    template = registration_fixture("person_schema")
    prefix = template["gts_id"].removesuffix("person.v1~")
    a_major = schema_with_id(template, f"{prefix}shape_a.v1~")
    a_minor = schema_with_id(template, f"{prefix}shape_a.v1.0~")
    b_minor = schema_with_id(template, f"{prefix}shape_b.v1.0~")
    b_major = schema_with_id(template, f"{prefix}shape_b.v1~")
    await given_registered(a_major, b_minor)
    a_before = await _read(registry_http, registry_api_path, a_major, 1)
    b_before = await _read(registry_http, registry_api_path, b_minor, 1)

    await register_and_assert(
        registry_http,
        registry_api_path,
        [a_minor, b_major],
        outcome(a_minor, "failed", None, "family_shape_conflict"),
        outcome(b_major, "failed", None, "family_shape_conflict"),
    )
    await assert_absent(registry_http, registry_api_path, a_minor)
    await assert_absent(registry_http, registry_api_path, b_major)
    a_after = await _read(
        registry_http, registry_api_path, a_major, 1, etag=a_before.headers["etag"]
    )
    b_after = await _read(
        registry_http, registry_api_path, b_minor, 1, etag=b_before.headers["etag"]
    )
    assert_json(a_after.json(), a_before.json())
    assert_json(b_after.json(), b_before.json())


@pytest.mark.scenario("TR-REG-604")
async def test_minor_revision_is_refused_before_existence_lookup(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """Existing and absent minor revisions receive the same synchronous refusal."""
    first = registration_fixture("person_minor_0_schema")
    absent = registration_fixture("person_minor_1_schema")
    await given_registered(first)
    before = await _read(registry_http, registry_api_path, first, 1)
    changed = deepcopy(first)
    changed["content"]["title"] = "Changed minor content"
    changed["expected_resource_version"] = 1
    absent["expected_resource_version"] = 1

    for candidate in (changed, absent):
        assert_bad_request(
            await post_registration(registry_http, registry_api_path, [candidate]),
            invalid_argument(
                "expected_resource_version",
                "VALIDATION_FAILED",
                f"minor-bearing Type Schema '{candidate['gts_id']}' is immutable; "
                "register a new minor instead",
                resource_name=candidate["gts_id"],
            ),
        )
    after = await _read(
        registry_http, registry_api_path, first, 1, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())
    await assert_absent(registry_http, registry_api_path, absent)


@pytest.mark.scenario("TR-REG-605")
async def test_failed_first_minor_blocks_the_next_minor(
    registry_http, registry_api_path, registration_fixture
):
    """An in-batch failed first minor blocks its successor as a predecessor."""
    first = registration_fixture("person_minor_0_schema")
    second = registration_fixture("person_minor_1_schema")
    missing_id = first["gts_id"].removesuffix("person.v1.0~") + "absent.v1~"
    first["content"]["properties"]["payload"]["properties"] = {
        "missing": {"$ref": f"gts://{missing_id}"}
    }
    await register_and_assert(
        registry_http,
        registry_api_path,
        [second, first],
        outcome(second, "failed", None, "blocked_by_predecessor"),
        outcome(
            first,
            "failed",
            None,
            "dependency_not_found",
            dependency_id=missing_id,
            dependency_kind="ref",
        ),
    )
    await assert_absent(registry_http, registry_api_path, first)
    await assert_absent(registry_http, registry_api_path, second)


@pytest.mark.scenario("TR-REG-606")
async def test_a_compatible_minor_with_changed_content_needs_no_force(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """An ordinary cross-minor check admits a widening; nothing is waived."""
    first = registration_fixture("person_minor_0_schema")
    second = registration_fixture("person_minor_1_schema")
    await given_registered(first)
    before = await _read(registry_http, registry_api_path, first, 1)
    second["content"]["properties"]["nickname"] = {"type": "string"}

    operation = await register_and_assert(
        registry_http, registry_api_path, [second], outcome(second, "succeeded", 1)
    )
    await read_created(registry_http, registry_api_path, schema_entity(second, 1), operation)
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, second["gts_id"], select="provenance"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(second), "provenance": provenance(False)},
        },
    )
    after = await _read(
        registry_http, registry_api_path, first, 1, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-607")
async def test_an_incompatible_minor_is_refused_without_force(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A deployment that permits `force` still applies it only when asked."""
    first = registration_fixture("person_minor_0_schema")
    second = registration_fixture("person_minor_1_schema")
    await given_registered(first)
    before = await _read(registry_http, registry_api_path, first, 1)
    second["content"]["properties"]["name"]["minLength"] = 2

    await register_and_assert(
        registry_http,
        registry_api_path,
        [second],
        outcome(second, "failed", None, "incompatible_with_baseline"),
    )
    await assert_absent(registry_http, registry_api_path, second)
    after = await _read(
        registry_http, registry_api_path, first, 1, etag=before.headers["etag"]
    )
    assert_json(after.json(), before.json())
