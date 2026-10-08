"""Discovery scenarios (`GET /entities`), each walk compared page by page in full."""

import pytest

from .helpers import (
    RECEIPT,
    assert_bad_request,
    assert_batch,
    assert_exact,
    assert_operation,
    assert_page,
    assert_pages,
    batch_get,
    delete_one_and_poll,
    discover,
    get_entity,
    gts_uuid,
    invalid_argument,
    managed,
    mandatory,
    namespace_pattern,
    paged,
    walk,
)


SCHEMAS = (
    "person_schema",
    "employee_schema",
    "manager_schema",
    "director_schema",
    "contractor_schema",
    "device_schema",
)
INSTANCES = (
    "person_instance",
    "employee_instance",
    "manager_instance",
    "director_instance",
    "contractor_instance",
    "device_instance",
)
# Identifier byte order: a type precedes its Instances and derived types, and
# siblings sort by name (`alice` < `contractor` < `employee`, `carol` < `director`).
CANONICAL = (
    "device_schema",
    "device_instance",
    "person_schema",
    "person_instance",
    "contractor_schema",
    "contractor_instance",
    "employee_schema",
    "employee_instance",
    "manager_schema",
    "manager_instance",
    "director_schema",
    "director_instance",
)
# Each derived schema's `allOf` `$ref` names its parent.
PARENTS = {
    "employee_schema": "person_schema",
    "manager_schema": "employee_schema",
    "director_schema": "manager_schema",
    "contractor_schema": "person_schema",
}


def metadata(document, lifecycle_status="active", resource_version=1):
    """The default, document-free projection."""
    return {
        **mandatory(document, lifecycle_status),
        "origin": managed(resource_version),
    }


def with_content(document):
    return {**mandatory(document), "content": document["content"]}


def resolved_schema(tree, name):
    """A root resolves to itself; a derived schema inlines its parent's resolved
    document, without `$id` and `$schema`, in place of the `$ref`."""
    parent = PARENTS.get(name)
    if parent is None:
        return tree[name]["content"]
    inherited = {
        keyword: value
        for keyword, value in resolved_schema(tree, parent).items()
        if keyword not in ("$id", "$schema")
    }
    return {**tree[name]["content"], "allOf": [inherited]}


@pytest.fixture
async def tree(discovery_fixture, given_registered):
    """Register the six schemas and six Instances, in dependency order."""
    documents = {name: discovery_fixture(name) for name in (*SCHEMAS, *INSTANCES)}
    await given_registered(*documents.values())
    return documents


@pytest.fixture
async def neighbour_pairs(neighbour_discovery_fixture, given_registered):
    """Control pairs a target namespace pattern must never return."""
    documents = [
        neighbour_discovery_fixture(name)
        for name in ("person_schema", "person_instance", "device_schema", "device_instance")
    ]
    await given_registered(*documents)
    return documents


@pytest.mark.smoke
@pytest.mark.scenario("TR-DISC-001")
async def test_a_limit_one_cursor_visits_every_row_once(
    registry_http, registry_api_path, discovery_fixture, given_registered, neighbour_pairs
):
    """A full page carries a cursor; the last page carries none."""
    person_schema = discovery_fixture("person_schema")
    device_schema = discovery_fixture("device_schema")
    await given_registered(person_schema, device_schema)

    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {"pattern": namespace_pattern(person_schema), "limit": 1},
        ),
        [
            {
                "items": [
                    {
                        "gts_id": device_schema["gts_id"],
                        "gts_uuid": gts_uuid(device_schema["gts_id"]),
                        "kind": "type_schema",
                        "lifecycle_status": "active",
                        "origin": {
                            "type": "managed",
                            "resource_version": 1,
                            "created_at": "<created_at>",
                            "updated_at": "<updated_at>",
                        },
                    },
                ],
                "page_info": {"next_cursor": "<next_cursor>", "limit": 1},
            },
            {
                "items": [
                    {
                        "gts_id": person_schema["gts_id"],
                        "gts_uuid": gts_uuid(person_schema["gts_id"]),
                        "kind": "type_schema",
                        "lifecycle_status": "active",
                        "origin": {
                            "type": "managed",
                            "resource_version": 1,
                            "created_at": "<created_at>",
                            "updated_at": "<updated_at>",
                        },
                    },
                ],
                "page_info": {"limit": 1},
            },
        ],
    )


@pytest.mark.scenario("TR-DISC-002")
async def test_a_pattern_selects_a_namespace_or_a_branch(
    registry_http, registry_api_path, tree, neighbour_discovery_fixture, given_registered
):
    """A pattern bounds a walk to one namespace, or to one type and its descendants."""
    await given_registered(neighbour_discovery_fixture("person_schema"))
    employee_id = tree["employee_schema"]["gts_id"]

    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {"pattern": namespace_pattern(tree["person_schema"]), "limit": 20},
        ),
        paged([metadata(tree[name]) for name in CANONICAL], 20),
    )
    # The contractor branch, the direct person Instance and the device tree stay outside.
    assert_pages(
        await walk(registry_http, registry_api_path, {"pattern": f"{employee_id}*", "limit": 20}),
        paged(
            [
                metadata(tree[name])
                for name in (
                    "employee_schema",
                    "employee_instance",
                    "manager_schema",
                    "manager_instance",
                    "director_schema",
                    "director_instance",
                )
            ],
            20,
        ),
    )
    # A valid pattern that matches nothing is an empty page, not a 404.
    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {"pattern": employee_id.replace("employee.v1~", "nobody.v1~*"), "limit": 20},
        ),
        [{"items": [], "page_info": {"limit": 20}}],
    )


@pytest.mark.scenario("TR-DISC-003")
async def test_a_content_projection_survives_page_continuation(
    registry_http, registry_api_path, tree
):
    """Every page of a walk keeps the selection the walk started with."""
    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {
                "pattern": namespace_pattern(tree["person_schema"]),
                "$select": "content",
                "limit": 1,
            },
        ),
        paged([with_content(tree[name]) for name in CANONICAL], 1),
    )


@pytest.mark.scenario("TR-DISC-004")
async def test_schema_only_documents_stay_absent_on_discovered_instances(
    registry_http, registry_api_path, tree
):
    """`$select=resolved_schema` fills Type Schemas and leaves Instances without it."""
    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {
                "pattern": namespace_pattern(tree["person_schema"]),
                "$select": "resolved_schema",
                "limit": 20,
            },
        ),
        paged(
            [
                {**mandatory(tree[name]), "resolved_schema": resolved_schema(tree, name)}
                if name in SCHEMAS
                else mandatory(tree[name])
                for name in CANONICAL
            ],
            20,
        ),
    )


@pytest.mark.scenario("TR-DISC-101")
async def test_kind_selects_type_schemas_or_instances(registry_http, registry_api_path, tree):
    """`kind` narrows before paging, so every page holds only that kind."""
    pattern = namespace_pattern(tree["person_schema"])
    for kind, names in (("type_schema", SCHEMAS), ("instance", INSTANCES)):
        assert_pages(
            await walk(
                registry_http,
                registry_api_path,
                {"pattern": pattern, "kind": kind, "limit": 2},
            ),
            paged([metadata(tree[name]) for name in CANONICAL if name in names], 2),
        )


@pytest.mark.scenario("TR-DISC-102")
async def test_pattern_depth_kind_and_selection_compose(
    registry_http, registry_api_path, tree, neighbour_discovery_fixture, given_registered
):
    """Each filter excludes its own entities; together they leave one."""
    await given_registered(
        *(
            neighbour_discovery_fixture(name)
            for name in ("person_schema", "employee_schema", "employee_instance")
        )
    )
    query = {
        "pattern": f"{tree['employee_schema']['gts_id']}*",
        "kind": "instance",
        "$select": "content",
        "limit": 10,
    }

    # Outside: the neighbour's employee Instance and the contractor Instance
    # (pattern), the employee and manager schemas (kind), and the manager
    # Instance, four segments deep (depth).
    assert_pages(
        await walk(registry_http, registry_api_path, {**query, "depth": 3}),
        [{"items": [with_content(tree["employee_instance"])], "page_info": {"limit": 10}}],
    )
    assert_pages(
        await walk(registry_http, registry_api_path, {**query, "depth": 2}),
        [{"items": [], "page_info": {"limit": 10}}],
    )


@pytest.mark.scenario("TR-DISC-103")
async def test_the_lifecycle_filter_separates_live_rows_and_tombstones(
    registry_http, registry_api_path, discovery_fixture, given_registered, neighbour_pairs
):
    """Discovery lists live entities by default; tombstones only on request."""
    person_schema = discovery_fixture("person_schema")
    device_schema = discovery_fixture("device_schema")
    await given_registered(person_schema, device_schema)
    operation = await delete_one_and_poll(
        registry_http, registry_api_path, person_schema["gts_id"], 1, RECEIPT
    )
    assert_operation(
        operation,
        {
            "operation_id": "<operation_id>",
            "kind": "deletion",
            "dry_run": False,
            "status": "completed",
            "created_at": "<created_at>",
            "started_at": "<started_at>",
            "completed_at": "<completed_at>",
            "items": [
                {
                    "entity_key": person_schema["gts_id"],
                    "status": "succeeded",
                    "resource_version": 2,
                    "error": None,
                },
            ],
        },
        ordered=True,
    )
    live = metadata(device_schema)
    tombstone = metadata(person_schema, lifecycle_status="deleted", resource_version=2)
    pattern = namespace_pattern(person_schema)

    for lifecycle_status, items in (
        (None, [live]),
        ("active", [live]),
        ("deleted", [tombstone]),
        ("all", [live, tombstone]),
    ):
        query = {"pattern": pattern, "limit": 10}
        if lifecycle_status is not None:
            query["lifecycle_status"] = lifecycle_status
        assert_pages(
            await walk(registry_http, registry_api_path, query),
            [{"items": items, "page_info": {"limit": 10}}],
        )
    assert_exact(
        await get_entity(registry_http, registry_api_path, person_schema["gts_id"]),
        {"status": 200, "etag": "<etag>", "body": tombstone},
    )


@pytest.mark.scenario("TR-DISC-201")
async def test_a_changed_pattern_cannot_reuse_a_cursor(
    registry_http, registry_api_path, discovery_fixture, given_registered, neighbour_pairs
):
    """A cursor is bound to its traversal; resuming another one is a 400."""
    person_schema = discovery_fixture("person_schema")
    device_schema = discovery_fixture("device_schema")
    await given_registered(person_schema, device_schema)

    first = assert_page(
        await discover(
            registry_http,
            registry_api_path,
            {"pattern": namespace_pattern(person_schema), "limit": 1},
        ),
        {
            "items": [metadata(device_schema)],
            "page_info": {"next_cursor": "<next_cursor>", "limit": 1},
        },
    )
    assert_bad_request(
        await discover(
            registry_http,
            registry_api_path,
            {
                "pattern": namespace_pattern(neighbour_pairs[0]),
                "limit": 1,
                "cursor": first["page_info"]["next_cursor"],
            },
        ),
        invalid_argument(
            "cursor",
            "VALIDATION_FAILED",
            "the cursor cannot be used for this request: FILTER_MISMATCH",
        ),
    )


@pytest.mark.scenario("TR-DISC-202")
async def test_the_page_size_may_change_mid_walk(
    registry_http, registry_api_path, discovery_fixture, given_registered, neighbour_pairs
):
    """A cursor fixes the position and filters, not the page size."""

    def root(name):
        """An independent closed-envelope root, rewritten from `person_schema`."""
        person = discovery_fixture("person_schema")
        gts_id = person["gts_id"].replace("person.v1~", f"{name}.v1~")
        return {"gts_id": gts_id, "content": {**person["content"], "$id": f"gts://{gts_id}"}}

    roots = [root(f"item{number:03}") for number in range(4)]
    await given_registered(*roots)
    query = {"pattern": namespace_pattern(roots[0]), "$select": "content"}

    first = assert_page(
        await discover(registry_http, registry_api_path, {**query, "limit": 1}),
        {
            "items": [with_content(roots[0])],
            "page_info": {"next_cursor": "<next_cursor>", "limit": 1},
        },
    )
    assert_pages(
        await walk(
            registry_http,
            registry_api_path,
            {**query, "limit": 2},
            cursor=first["page_info"]["next_cursor"],
        ),
        paged([with_content(document) for document in roots[1:]], 2),
    )


@pytest.mark.scenario("TR-DISC-301")
async def test_discovered_references_hydrate_through_batch_get(
    registry_http, registry_api_path, tree
):
    """Discovery lists references cheaply; one batchGet reads their documents."""
    pages = await walk(
        registry_http,
        registry_api_path,
        {"pattern": namespace_pattern(tree["person_schema"]), "limit": 1},
    )
    assert_pages(pages, paged([metadata(tree[name]) for name in CANONICAL], 1))
    references = [item["gts_uuid"] for page in pages for item in page["items"]]

    assert_batch(
        await batch_get(
            registry_http,
            registry_api_path,
            [{"entity_key": reference} for reference in references],
            select="content",
        ),
        {
            "items": [
                {
                    "entity_key": gts_uuid(tree[name]["gts_id"]),
                    "status": "found",
                    "etag": "<etag>",
                    "entity": with_content(tree[name]),
                }
                for name in CANONICAL
            ],
        },
    )
