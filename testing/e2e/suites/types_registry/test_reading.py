"""Exact and batch read scenarios, each compared as whole JSON responses."""

import uuid

import pytest

from .helpers import (
    RECEIPT,
    assert_bad_request,
    assert_batch,
    assert_exact,
    assert_json,
    assert_not_found,
    assert_operation,
    assert_pages,
    batch_get,
    completed,
    delete_one_and_poll,
    get_entity,
    invalid_argument,
    gts_uuid,
    managed,
    mandatory,
    namespace_pattern,
    not_found,
    outcome,
    removal,
    provenance,
    submit_and_poll,
    timestamp,
    walk,
)


# The two independent schema/Instance pairs most scenarios register.
PAIRS = ("person_schema", "person_instance", "other_schema", "other_instance")


def resolved_employee_schema(derived, **base_keywords):
    """The derived schema with the base inlined in place of its `$ref`.

    The base arrives without its identity and trait keywords: its closed
    envelope requires `name`, while `payload` stays open for derived fields.
    """
    return {
        "$id": f"gts://{derived['gts_id']}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            {
                **base_keywords,
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "payload": {"type": "object", "additionalProperties": True},
                },
                "required": ["name", "payload"],
                "additionalProperties": False,
            },
        ],
        "type": "object",
        "properties": {
            "payload": {
                "type": "object",
                "properties": {"employee_id": {"type": "string"}},
                "required": ["employee_id"],
                "additionalProperties": True,
            },
        },
        "required": ["payload"],
    }


@pytest.mark.smoke
@pytest.mark.scenario("TR-READ-001")
async def test_default_read_is_document_free_metadata(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Without `$select` an entity is its identity and managed origin only."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)

    for schema in (person_schema, other_schema):
        assert_exact(
            await get_entity(registry_http, registry_api_path, schema["gts_id"]),
            {
                "status": 200,
                "etag": "<etag>",
                "body": {
                    "gts_id": schema["gts_id"],
                    "gts_uuid": gts_uuid(schema["gts_id"]),
                    "kind": "type_schema",
                    "lifecycle_status": "active",
                    "origin": {
                        "type": "managed",
                        "resource_version": 1,
                        "created_at": "<created_at>",
                        "updated_at": "<updated_at>",
                    },
                },
            },
        )


@pytest.mark.scenario("TR-READ-002")
async def test_gts_id_and_registry_reference_read_the_same_entity(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Both key spellings of one entity return one body, timestamps included."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)

    by_id = await get_entity(registry_http, registry_api_path, person_schema["gts_id"])
    by_uuid = await get_entity(registry_http, registry_api_path, by_id.json()["gts_uuid"])
    other = await get_entity(registry_http, registry_api_path, other_schema["gts_id"])

    for response, schema in (
        (by_id, person_schema),
        (by_uuid, person_schema),
        (other, other_schema),
    ):
        assert_exact(
            response,
            {
                "status": 200,
                "etag": "<etag>",
                "body": {**mandatory(schema), "origin": managed(1)},
            },
        )
    assert_json(by_uuid.json(), by_id.json())


@pytest.mark.scenario("TR-READ-003")
async def test_content_is_selected_independently(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """`$select=content` adds the authored document to the mandatory fields only."""
    documents = list(map(reading_fixture, PAIRS))
    await given_registered(*documents)

    for document in documents:
        assert_exact(
            await get_entity(
                registry_http, registry_api_path, document["gts_id"], select="content"
            ),
            {
                "status": 200,
                "etag": "<etag>",
                "body": {**mandatory(document), "content": document["content"]},
            },
        )


@pytest.mark.scenario("TR-READ-004")
async def test_effective_documents_are_selected_independently(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Each effective document of a derived schema is its own selection."""
    base = reading_fixture("trait_base_schema")
    derived = reading_fixture("trait_derived_schema")
    employee = reading_fixture("trait_employee_instance")
    other_schema = reading_fixture("other_schema")
    other_instance = reading_fixture("other_instance")
    await given_registered(base, derived, other_schema, other_instance)
    operation = await submit_and_poll(registry_http, registry_api_path, [employee], RECEIPT)
    assert_operation(operation, completed("registration", outcome(employee, "succeeded", 1)))

    effective_documents = {
        "resolved_schema": resolved_employee_schema(derived),
        # Inherited from the base, which declares both.
        "effective_traits": {"category": "internal"},
        "effective_traits_schema": {
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "category": {"type": "string", "enum": ["internal", "public"]},
            },
            "required": ["category"],
            "additionalProperties": False,
        },
    }
    for field, document in effective_documents.items():
        assert_exact(
            await get_entity(registry_http, registry_api_path, derived["gts_id"], select=field),
            {
                "status": 200,
                "etag": "<etag>",
                "body": {**mandatory(derived), field: document},
            },
        )
    for instance in (employee, other_instance):
        assert_exact(
            await get_entity(
                registry_http, registry_api_path, instance["gts_id"], select="resolved_schema"
            ),
            {"status": 200, "etag": "<etag>", "body": mandatory(instance)},
        )

    # The inherited envelope is closed: an undeclared top-level field is refused.
    intruder = reading_fixture("trait_derived_extra_field_instance")
    operation = await submit_and_poll(registry_http, registry_api_path, [intruder], RECEIPT)
    assert_operation(
        operation, completed("registration", outcome(intruder, "failed", None, "invalid_value"))
    )
    assert_not_found(
        await get_entity(registry_http, registry_api_path, intruder["gts_id"]),
        not_found(intruder["gts_id"]),
    )


@pytest.mark.scenario("TR-READ-005")
async def test_provenance_is_one_selected_group(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Provenance arrives as one nested group, never as top-level fields."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)

    # `compat_forced` is a schema admission fact; an Instance has an explicit null.
    for document, compat_forced in (
        (person_schema, False),
        (person_instance, None),
        (other_schema, False),
        (other_instance, None),
    ):
        assert_exact(
            await get_entity(
                registry_http, registry_api_path, document["gts_id"], select="provenance"
            ),
            {
                "status": 200,
                "etag": "<etag>",
                "body": {**mandatory(document), "provenance": provenance(compat_forced)},
            },
        )


@pytest.mark.scenario("TR-READ-006")
async def test_schema_only_documents_are_absent_on_an_instance(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """An Instance omits Type Schema documents rather than returning null."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)

    assert_exact(
        await get_entity(
            registry_http,
            registry_api_path,
            person_instance["gts_id"],
            select="resolved_schema,effective_traits,effective_traits_schema",
        ),
        {"status": 200, "etag": "<etag>", "body": mandatory(person_instance)},
    )
    # A root schema resolves to its own authored document.
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, other_schema["gts_id"], select="resolved_schema"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(other_schema), "resolved_schema": other_schema["content"]},
        },
    )


@pytest.mark.scenario("TR-READ-007")
async def test_a_projected_tombstone_is_not_an_absence(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Exact read has no lifecycle filter: a tombstone answers by key."""
    person_schema = reading_fixture("person_schema")
    other_schema = reading_fixture("other_schema")
    other_instance = reading_fixture("other_instance")
    device_schema = reading_fixture("device_schema")
    device_instance = reading_fixture("device_instance")
    await given_registered(
        person_schema, other_schema, other_instance, device_schema, device_instance
    )
    live_etag = assert_exact(
        await get_entity(
            registry_http, registry_api_path, person_schema["gts_id"], select="content"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(person_schema), "content": person_schema["content"]},
        },
    )
    operation = await delete_one_and_poll(
        registry_http, registry_api_path, person_schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        operation, completed("deletion", removal(person_schema, "succeeded", 2)), ordered=True
    )

    # `kind` and `lifecycle_status` are mandatory, so the projection shows the deletion.
    tombstone_etag = assert_exact(
        await get_entity(
            registry_http,
            registry_api_path,
            person_schema["gts_id"],
            select="content",
            if_none_match=live_etag,
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(person_schema, lifecycle_status="deleted"),
                "content": person_schema["content"],
            },
        },
        known_etags=[live_etag],
    )
    assert_exact(
        await get_entity(
            registry_http,
            registry_api_path,
            person_schema["gts_id"],
            select="content",
            if_none_match=tombstone_etag,
        ),
        {"status": 304, "etag": tombstone_etag},
    )
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, other_schema["gts_id"], select="content"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(other_schema), "content": other_schema["content"]},
        },
    )
    # Default discovery, by contrast, lists only the live entities, in canonical order.
    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {"pattern": namespace_pattern(person_schema), "limit": 10},
        ),
        [
            {
                "items": [
                    {**mandatory(document), "origin": managed(1)}
                    for document in (device_schema, device_instance, other_schema, other_instance)
                ],
                "page_info": {"limit": 10},
            },
        ],
    )


@pytest.mark.scenario("TR-READ-008")
async def test_absent_and_impossible_keys_are_one_not_found(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """An absent identifier, an absent reference and a non-key answer alike."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)

    for key in (
        person_schema["gts_id"].replace("person.v1~", "absent.v1~"),
        str(uuid.uuid4()),
        "not-a-gts-id",
    ):
        assert_not_found(
            await get_entity(registry_http, registry_api_path, key), not_found(key)
        )


@pytest.mark.scenario("TR-READ-101")
async def test_a_mixed_batch_answers_every_key(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Absent keys are results, not errors, and do not hide the found ones."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    absent_id = person_schema["gts_id"].replace("person.v1~", "absent.v1~")
    absent_uuid = str(uuid.uuid4())

    assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [
                {"entity_key": absent_id},
                {"entity_key": person_schema["gts_id"]},
                {"entity_key": absent_uuid},
                {"entity_key": person_instance["gts_id"]},
            ],
        ),
        {
            "items": [
                {"entity_key": absent_id, "status": "not_found"},
                {
                    "entity_key": person_schema["gts_id"],
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {**mandatory(person_schema), "origin": managed(1)},
                },
                {"entity_key": absent_uuid, "status": "not_found"},
                {
                    "entity_key": person_instance["gts_id"],
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {**mandatory(person_instance), "origin": managed(1)},
                },
            ],
        },
    )


@pytest.mark.scenario("TR-READ-102")
async def test_a_batch_projection_equals_the_exact_read(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """A batch result is the exact read of its key: same body, same validator."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    select = "content,provenance"
    expected = {
        person_schema["gts_id"]: {
            **mandatory(person_schema),
            "content": person_schema["content"],
            "provenance": provenance(False),
        },
        person_instance["gts_id"]: {
            **mandatory(person_instance),
            "content": person_instance["content"],
            "provenance": provenance(None),
        },
    }

    exact_reads = {}
    exact_etags = {}
    for key, entity in expected.items():
        exact_reads[key] = await get_entity(registry_http, registry_api_path, key, select=select)
        exact_etags[key] = assert_exact(
            exact_reads[key], {"status": 200, "etag": "<etag>", "body": entity}
        )
    batch = await batch_get(
        registry_http, registry_api_path, [{"entity_key": key} for key in expected], select=select
    )

    assert_batch(
        batch,
        {
            "items": [
                {"entity_key": key, "status": "found", "etag": exact_etags[key], "entity": entity}
                for key, entity in expected.items()
            ],
        },
    )
    for item in batch.json()["items"]:
        assert_json(item["entity"], exact_reads[item["entity_key"]].json())


@pytest.mark.scenario("TR-READ-103")
async def test_a_batch_reads_a_tombstone_beside_a_live_entity(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """`:batchGet` has no lifecycle filter either: a tombstone is `found`."""
    person_schema = reading_fixture("person_schema")
    other_schema = reading_fixture("other_schema")
    await given_registered(
        person_schema,
        other_schema,
        reading_fixture("other_instance"),
        reading_fixture("device_schema"),
        reading_fixture("device_instance"),
    )
    operation = await delete_one_and_poll(
        registry_http, registry_api_path, person_schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        operation, completed("deletion", removal(person_schema, "succeeded", 2)), ordered=True
    )

    assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": person_schema["gts_id"]}, {"entity_key": other_schema["gts_id"]}],
            select="content",
        ),
        {
            "items": [
                {
                    "entity_key": person_schema["gts_id"],
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {
                        **mandatory(person_schema, lifecycle_status="deleted"),
                        "content": person_schema["content"],
                    },
                },
                {
                    "entity_key": other_schema["gts_id"],
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {**mandatory(other_schema), "content": other_schema["content"]},
                },
            ],
        },
    )


@pytest.mark.scenario("TR-READ-104")
async def test_an_unknown_selection_is_refused_on_both_transports(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """A misspelled field is a 400, never a silent fall back to the default."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    refusal = invalid_argument(
        "$select",
        "INVALID_SELECT",
        "'contents' is not a selectable field; select from content, effective_traits, "
        "effective_traits_schema, gts_id, gts_uuid, kind, lifecycle_status, origin, "
        "provenance, resolved_schema",
    )

    assert_bad_request(
        await get_entity(
            registry_http, registry_api_path, person_schema["gts_id"], select="contents"
        ),
        refusal,
    )
    assert_bad_request(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": person_schema["gts_id"]}],
            select="contents",
        ),
        refusal,
    )


@pytest.mark.scenario("TR-READ-105")
async def test_a_batch_wide_if_none_match_is_refused(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """One header cannot validate many keys; conditions belong to the items."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    etag = assert_exact(
        await get_entity(registry_http, registry_api_path, person_schema["gts_id"]),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(person_schema), "origin": managed(1)},
        },
    )

    assert_bad_request(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": person_schema["gts_id"]}],
            headers={"If-None-Match": etag},
        ),
        invalid_argument(
            "If-None-Match",
            "VALIDATION_FAILED",
            "If-None-Match is not supported on a batch read; carry each key's validator "
            "in that item's if_none_match, because one header cannot represent a batch",
        ),
    )


@pytest.mark.scenario("TR-READ-201")
async def test_every_read_route_observes_a_completed_revision(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Exact read, batchGet and discovery agree as soon as the operation completes."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    select = "content,origin"

    def selected(document, content, resource_version):
        return {**mandatory(document), "content": content, "origin": managed(resource_version)}

    async def read_person_on_every_route(exact_key, person):
        """Compare each route in full and return the three raw person bodies."""
        exact = await get_entity(registry_http, registry_api_path, exact_key, select=select)
        assert_exact(exact, {"status": 200, "etag": "<etag>", "body": person})
        batch = await batch_get(
            registry_http, registry_api_path, [{"entity_key": person_schema["gts_id"]}], select=select
        )
        assert_batch(
            batch,
            {
                "items": [
                    {
                        "entity_key": person_schema["gts_id"],
                        "status": "found",
                        "etag": "<etag>",
                        "entity": person,
                    },
                ],
            },
        )
        pages = await walk(
            registry_http,
            registry_api_path,
            {"pattern": namespace_pattern(person_schema), "$select": select, "limit": 10},
        )
        # Canonical order; only person ever changes.
        assert_pages(
            pages,
            [
                {
                    "items": [
                        selected(other_schema, other_schema["content"], 1),
                        selected(other_instance, other_instance["content"], 1),
                        person,
                        selected(person_instance, person_instance["content"], 1),
                    ],
                    "page_info": {"limit": 10},
                },
            ],
        )
        discovered = next(
            item for item in pages[0]["items"] if item["gts_id"] == person_schema["gts_id"]
        )
        return exact.json(), batch.json()["items"][0]["entity"], discovered

    before = await read_person_on_every_route(
        person_schema["gts_id"], selected(person_schema, person_schema["content"], 1)
    )
    revised = reading_fixture("person_schema_revised")
    operation = await submit_and_poll(registry_http, registry_api_path, [revised], RECEIPT)
    assert_operation(operation, completed("registration", outcome(revised, "succeeded", 2)))

    # The revision changes only the title; exact read now goes by the retained UUID.
    after = await read_person_on_every_route(
        before[0]["gts_uuid"],
        selected(person_schema, {**person_schema["content"], "title": "Revised Person"}, 2),
    )
    for body in after:
        assert_json(body, after[0])
    assert after[0]["origin"]["created_at"] == before[0]["origin"]["created_at"], after
    assert timestamp(after[0]["origin"]["updated_at"]) >= timestamp(
        before[0]["origin"]["updated_at"]
    ), (before, after)


@pytest.mark.scenario("TR-READ-202")
async def test_selected_null_content_is_present(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """A `null` document is a value: selected it is present, unselected absent."""
    null_schema = reading_fixture("null_schema")
    null_instance = reading_fixture("null_instance")
    other_schema = reading_fixture("other_schema")
    other_instance = reading_fixture("other_instance")
    await given_registered(null_schema, null_instance, other_schema, other_instance)

    # Whole-JSON comparison tells a present `null` from a missing field.
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, null_instance["gts_id"], select="content"
        ),
        {"status": 200, "etag": "<etag>", "body": {**mandatory(null_instance), "content": None}},
    )
    assert_exact(
        await get_entity(registry_http, registry_api_path, null_instance["gts_id"]),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(null_instance), "origin": managed(1)},
        },
    )
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, other_instance["gts_id"], select="content"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(other_instance), "content": other_instance["content"]},
        },
    )


@pytest.mark.scenario("TR-READ-203")
async def test_an_exact_key_never_falls_back_to_a_minor_version(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """`person.v1~` is its own key, not a pattern matching `person.v1.0~`."""
    minor = reading_fixture("person_minor_schema")
    # Loaded for its identifier only: the major-only schema is never registered.
    major_only_id = reading_fixture("person_schema")["gts_id"]
    await given_registered(
        minor,
        reading_fixture("other_schema"),
        reading_fixture("other_instance"),
        reading_fixture("device_schema"),
        reading_fixture("device_instance"),
    )
    minor_entity = {**mandatory(minor), "content": minor["content"]}

    assert_not_found(
        await get_entity(registry_http, registry_api_path, major_only_id, select="content"),
        not_found(major_only_id),
    )
    assert_exact(
        await get_entity(registry_http, registry_api_path, minor["gts_id"], select="content"),
        {"status": 200, "etag": "<etag>", "body": minor_entity},
    )
    assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": major_only_id}, {"entity_key": minor["gts_id"]}],
            select="content",
        ),
        {
            "items": [
                {"entity_key": major_only_id, "status": "not_found"},
                {
                    "entity_key": minor["gts_id"],
                    "status": "found",
                    "etag": "<etag>",
                    "entity": minor_entity,
                },
            ],
        },
    )


@pytest.mark.scenario("TR-READ-301")
async def test_an_etag_survives_a_no_op_and_changes_with_a_revision(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """A validator follows the entity's version, not admission activity."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    select = "content,origin"

    async def read(document, if_none_match=None):
        return await get_entity(
            registry_http,
            registry_api_path,
            document["gts_id"],
            select=select,
            if_none_match=if_none_match,
        )

    schema_etag = assert_exact(
        await read(person_schema),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(person_schema),
                "content": person_schema["content"],
                "origin": managed(1),
            },
        },
    )
    instance_etag = assert_exact(
        await read(person_instance),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(person_instance),
                "content": person_instance["content"],
                "origin": managed(1),
            },
        },
    )
    assert_exact(await read(person_schema, schema_etag), {"status": 304, "etag": schema_etag})

    # Identical content is a no-op: no new version, so the validator still holds.
    operation = await submit_and_poll(
        registry_http,
        registry_api_path,
        [{**person_schema, "expected_resource_version": 1}],
        RECEIPT,
    )
    assert_operation(operation, completed("registration", outcome(person_schema, "unchanged", 1)))
    assert_exact(await read(person_schema, schema_etag), {"status": 304, "etag": schema_etag})

    operation = await submit_and_poll(
        registry_http, registry_api_path, [reading_fixture("person_schema_revised")], RECEIPT
    )
    assert_operation(operation, completed("registration", outcome(person_schema, "succeeded", 2)))
    revised_etag = assert_exact(
        await read(person_schema, schema_etag),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(person_schema),
                "content": {**person_schema["content"], "title": "Revised Person"},
                "origin": managed(2),
            },
        },
        known_etags=[schema_etag],
    )
    assert_exact(await read(person_schema, revised_etag), {"status": 304, "etag": revised_etag})
    # The Instance's own version did not move.
    assert_exact(
        await read(person_instance, instance_etag), {"status": 304, "etag": instance_etag}
    )


@pytest.mark.scenario("TR-READ-302")
async def test_batch_revalidation_answers_each_key(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """Per-item validators: one stale key does not refresh the others."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)
    select = "content,origin"
    person_id, other_id = person_schema["gts_id"], other_schema["gts_id"]
    absent_id = person_id.replace("person.v1~", "absent.v1~")

    held = assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": person_id}, {"entity_key": other_id}],
            select=select,
        ),
        {
            "items": [
                {
                    "entity_key": person_id,
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {
                        **mandatory(person_schema),
                        "content": person_schema["content"],
                        "origin": managed(1),
                    },
                },
                {
                    "entity_key": other_id,
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {
                        **mandatory(other_schema),
                        "content": other_schema["content"],
                        "origin": managed(1),
                    },
                },
            ],
        },
    )
    operation = await submit_and_poll(
        registry_http, registry_api_path, [reading_fixture("person_schema_revised")], RECEIPT
    )
    assert_operation(operation, completed("registration", outcome(person_schema, "succeeded", 2)))

    returned = assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [
                {"entity_key": other_id, "if_none_match": held[other_id]},
                {"entity_key": person_id, "if_none_match": held[person_id]},
                {"entity_key": other_instance["gts_id"]},
                {"entity_key": absent_id, "if_none_match": held[person_id]},
            ],
            select=select,
        ),
        {
            "items": [
                {"entity_key": other_id, "status": "unchanged", "etag": held[other_id]},
                {
                    "entity_key": person_id,
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {
                        **mandatory(person_schema),
                        "content": {**person_schema["content"], "title": "Revised Person"},
                        "origin": managed(2),
                    },
                },
                {
                    "entity_key": other_instance["gts_id"],
                    "status": "found",
                    "etag": "<etag>",
                    "entity": {
                        **mandatory(other_instance),
                        "content": other_instance["content"],
                        "origin": managed(1),
                    },
                },
                {"entity_key": absent_id, "status": "not_found"},
            ],
        },
        known_etags=held.values(),
    )

    # Copying every returned `etag` forward revalidates the whole set at once.
    current = {key: etag for key, etag in returned.items() if etag is not None}
    assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": key, "if_none_match": etag} for key, etag in current.items()],
            select=select,
        ),
        {
            "items": [
                {"entity_key": key, "status": "unchanged", "etag": etag}
                for key, etag in current.items()
            ],
        },
    )


@pytest.mark.scenario("TR-READ-303")
async def test_a_validator_belongs_to_one_projection(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """A metadata ETag never declares a wider representation unchanged."""
    person_schema, person_instance, other_schema, other_instance = map(reading_fixture, PAIRS)
    await given_registered(person_schema, person_instance, other_schema, other_instance)

    for document in (person_schema, person_instance):
        metadata_etag = assert_exact(
            await get_entity(registry_http, registry_api_path, document["gts_id"]),
            {
                "status": 200,
                "etag": "<etag>",
                "body": {**mandatory(document), "origin": managed(1)},
            },
        )
        content_etag = assert_exact(
            await get_entity(
                registry_http,
                registry_api_path,
                document["gts_id"],
                select="content",
                if_none_match=metadata_etag,
            ),
            {
                "status": 200,
                "etag": "<etag>",
                "body": {**mandatory(document), "content": document["content"]},
            },
            known_etags=[metadata_etag],
        )
        assert_exact(
            await get_entity(
                registry_http,
                registry_api_path,
                document["gts_id"],
                select="content",
                if_none_match=content_etag,
            ),
            {"status": 304, "etag": content_etag},
        )
    assert_exact(
        await get_entity(
            registry_http, registry_api_path, other_schema["gts_id"], select="content"
        ),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {**mandatory(other_schema), "content": other_schema["content"]},
        },
    )


@pytest.mark.scenario("TR-READ-304")
async def test_a_base_refresh_changes_the_derived_etag(
    registry_http, registry_api_path, reading_fixture, given_registered
):
    """The derived validator covers its resolved document, not only its version."""
    base = reading_fixture("trait_base_schema")
    derived = reading_fixture("trait_derived_schema")
    other_schema = reading_fixture("other_schema")
    other_instance = reading_fixture("other_instance")
    await given_registered(
        base, derived, reading_fixture("trait_employee_instance"), other_schema, other_instance
    )
    select = "resolved_schema,origin"

    async def read(document, if_none_match=None):
        return await get_entity(
            registry_http,
            registry_api_path,
            document["gts_id"],
            select=select,
            if_none_match=if_none_match,
        )

    derived_etag = assert_exact(
        await read(derived),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(derived),
                "resolved_schema": resolved_employee_schema(derived),
                "origin": managed(1),
            },
        },
    )
    control_etags = [
        (
            other_schema,
            assert_exact(
                await read(other_schema),
                {
                    "status": 200,
                    "etag": "<etag>",
                    "body": {
                        **mandatory(other_schema),
                        "resolved_schema": other_schema["content"],
                        "origin": managed(1),
                    },
                },
            ),
        ),
        (
            other_instance,
            assert_exact(
                await read(other_instance),
                {
                    "status": 200,
                    "etag": "<etag>",
                    "body": {**mandatory(other_instance), "origin": managed(1)},
                },
            ),
        ),
    ]

    operation = await submit_and_poll(
        registry_http, registry_api_path, [reading_fixture("trait_base_schema_revised")], RECEIPT
    )
    assert_operation(operation, completed("registration", outcome(base, "succeeded", 2)))

    # The derived schema keeps version 1, yet its resolved document now carries
    # the revised base title, so its validator changes.
    refreshed_etag = assert_exact(
        await read(derived, derived_etag),
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(derived),
                "resolved_schema": resolved_employee_schema(derived, title="Revised Record"),
                "origin": managed(1),
            },
        },
        known_etags=[derived_etag],
    )
    for control, etag in control_etags:
        assert_exact(await read(control, etag), {"status": 304, "etag": etag})
    assert_exact(await read(derived, refreshed_etag), {"status": 304, "etag": refreshed_etag})
