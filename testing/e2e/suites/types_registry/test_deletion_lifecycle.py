"""What a tombstone still answers, and what it no longer admits."""

import pytest

from .helpers import (
    assert_absent,
    assert_batch,
    assert_exact,
    assert_exact_entity,
    assert_pages,
    batch_get,
    delete_and_assert,
    get_entity,
    gts_uuid,
    managed,
    mandatory,
    namespace_pattern,
    outcome,
    removal,
    register_and_assert,
    schema_entity,
    target,
    walk,
)

TRAIT_KEYWORDS = {"$id", "$schema", "x-gts-traits-schema", "x-gts-traits"}
EFFECTIVE_SELECT = "content,origin,resolved_schema,effective_traits,effective_traits_schema"


def _trait_derived_entity(derived, base, version, lifecycle_status):
    """The derived schema's documents, computed from the two authored fixtures:
    the base is inlined without identity or trait keywords, and supplies both
    traits documents."""
    resolved = {
        **derived["content"],
        "allOf": [
            {key: value for key, value in base["content"].items() if key not in TRAIT_KEYWORDS}
        ],
    }
    return {
        **mandatory(derived, lifecycle_status),
        "origin": managed(version),
        "content": derived["content"],
        "resolved_schema": resolved,
        "effective_traits": base["content"]["x-gts-traits"],
        "effective_traits_schema": {
            "$schema": "http://json-schema.org/draft-07/schema#",
            **base["content"]["x-gts-traits-schema"],
        },
    }


@pytest.mark.scenario("TR-DEL-501")
async def test_a_tombstone_keeps_its_effective_documents_under_both_keys(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Each key's old validator reads the tombstone; its new one is current."""
    base = deletion_fixture("trait_base_schema")
    derived = deletion_fixture("trait_derived_schema")
    await given_registered(base, derived)
    keys = (derived["gts_id"], gts_uuid(derived["gts_id"]))
    live = _trait_derived_entity(derived, base, 1, "active")
    old = {}
    for key in keys:
        old[key] = assert_exact(
            await get_entity(registry_http, registry_api_path, key, select=EFFECTIVE_SELECT),
            {"status": 200, "etag": "<etag>", "body": live},
        )

    await delete_and_assert(
        registry_http, registry_api_path, [target(derived, 1)], removal(derived, "succeeded", 2)
    )
    tombstone = _trait_derived_entity(derived, base, 2, "deleted")
    for key in keys:
        current = assert_exact(
            await get_entity(
                registry_http,
                registry_api_path,
                key,
                select=EFFECTIVE_SELECT,
                if_none_match=old[key],
            ),
            {"status": 200, "etag": "<etag>", "body": tombstone},
            known_etags=tuple(old.values()),
        )
        assert_exact(
            await get_entity(
                registry_http,
                registry_api_path,
                key,
                select=EFFECTIVE_SELECT,
                if_none_match=current,
            ),
            {"status": 304, "etag": current},
        )
    assert_exact(
        await get_entity(registry_http, registry_api_path, base["gts_id"], select="content,origin"),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(base), "origin": managed(1), "content": base["content"]},
        },
    )


@pytest.mark.scenario("TR-DEL-502")
async def test_deletion_moves_an_entity_between_discovery_filters(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Discovery hides a tombstone by default; its issued keys still read it."""
    person = deletion_fixture("person_schema")
    other = deletion_fixture("other_schema")
    await given_registered(person, other)
    await delete_and_assert(
        registry_http, registry_api_path, [target(person, 1)], removal(person, "succeeded", 2)
    )
    live = {**mandatory(other), "origin": managed(1)}
    tombstone = {**mandatory(person, "deleted"), "origin": managed(2)}
    pattern = namespace_pattern(person)

    for lifecycle_status, items in (
        (None, [live]),
        ("active", [live]),
        ("deleted", [tombstone]),
        ("all", sorted([live, tombstone], key=lambda item: item["gts_id"])),
    ):
        query = {"pattern": pattern, "limit": 10}
        if lifecycle_status is not None:
            query["lifecycle_status"] = lifecycle_status
        assert_pages(
            await walk(registry_http, registry_api_path, query),
            [{"items": items, "page_info": {"limit": 10}}],
        )

    keys = (person["gts_id"], gts_uuid(person["gts_id"]))
    body = {**mandatory(person, "deleted"), "content": person["content"]}
    for key in keys:
        assert_exact(
            await get_entity(registry_http, registry_api_path, key, select="content"),
            {"status": 200, "etag": "<etag>", "body": body},
        )
    assert_batch(
        await batch_get(
            registry_http, registry_api_path, [{"entity_key": key} for key in keys], select="content"
        ),
        {
            "items": [
                {"entity_key": key, "status": "found", "etag": "<etag>", "entity": body} for key in keys
            ]
        },
    )


@pytest.mark.scenario("TR-DEL-503")
async def test_a_readable_tombstone_cannot_become_a_new_dependency(
    registry_http, registry_api_path, deletion_fixture, given_registered
):
    """Every edge kind to the tombstone is refused and names it; an unrelated
    candidate in the same batch commits."""
    person = deletion_fixture("person_schema")
    instance = deletion_fixture("person_instance")
    derived = deletion_fixture("derived_employee_schema")
    referrer = deletion_fixture("person_referrer_schema")
    other = deletion_fixture("other_schema")
    await given_registered(person)
    await delete_and_assert(
        registry_http, registry_api_path, [target(person, 1)], removal(person, "succeeded", 2)
    )
    tombstone = schema_entity(person, 2, lifecycle_status="deleted")
    before = await assert_exact_entity(registry_http, registry_api_path, person, tombstone)

    def deleted_dependency(document, kind):
        return outcome(
            document,
            "failed",
            None,
            "dependency_deleted",
            dependency_id=person["gts_id"],
            dependency_kind=kind,
        )

    await register_and_assert(
        registry_http,
        registry_api_path,
        [instance, derived, referrer, other],
        deleted_dependency(instance, "conforming_type"),
        # The derivation role wins over the `allOf` `$ref` naming the same base.
        deleted_dependency(derived, "base"),
        deleted_dependency(referrer, "ref"),
        outcome(other, "succeeded", 1),
    )
    for document in (instance, derived, referrer):
        await assert_absent(registry_http, registry_api_path, document)
    await assert_exact_entity(
        registry_http, registry_api_path, person, tombstone, etag=before.headers["etag"]
    )
