"""HTTP workflow and explicit normalization for complete admission responses."""
import asyncio
from copy import deepcopy
from datetime import datetime
import json
import re
import uuid
from urllib.parse import urljoin

import httpx


GTS_NAMESPACE = uuid.uuid5(uuid.NAMESPACE_URL, "gts")
# Every request carries this valid W3C `traceparent`, so the wire `trace_id` the
# canonical error layer echoes is deterministic. `extract_trace_id` prefers the
# live OTel span, but the request span continues this inbound `traceparent`, so
# its trace-id equals the header's; with OTel off the header is used directly.
# Either way the value is TRACE_ID, the header's 32-hex trace-id segment. Uses
# the W3C spec's example ids.
TRACE_ID = "0af7651916cd43dd8448eb211c80319c"
TRACEPARENT = f"00-{TRACE_ID}-b7ad6b7169203331-01"
# Everything a registration writes except `provenance`, whose implementation
# versions are not a scenario claim. The default projection is document-free.
ENTITY_SELECT = "origin,content,resolved_schema,effective_traits,effective_traits_schema"
RFC3339 = re.compile(
    r"\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2})"
)
# An opaque strong entity-tag; the exact read's `ETag` and a batch `etag` agree.
ENTITY_TAG = re.compile(r'"[^"\s]+"')
# A receipt for a fresh submission; `status` depends on how fast the worker was.
RECEIPT = {
    "operation_id": "<operation_id>",
    "status": "<status>",
    "replayed": False,
}


def assert_json(actual, expected):
    """Compare every field; JSON serialization also distinguishes true from 1."""
    assert json.dumps(actual, indent=2, sort_keys=True) == json.dumps(
        expected, indent=2, sort_keys=True
    )


def completed(kind, *items, dry_run=False):
    """A complete terminal operation with caller-specified item outcomes."""
    return {
        "operation_id": "<operation_id>",
        "kind": kind,
        "dry_run": dry_run,
        "status": "completed",
        "created_at": "<created_at>",
        "started_at": "<started_at>",
        "completed_at": "<completed_at>",
        "items": list(items),
    }


def outcome(document, status, resource_version, reason=None, *, message="<message>", **context):
    """A complete operation item; error messages alone are non-contractual."""
    return {
        "gts_id": document["gts_id"],
        "status": status,
        "resource_version": resource_version,
        "error": None
        if reason is None
        else {"reason": reason, "message": message, "context": context},
    }


def not_found(key):
    """The whole RFC-9457 response for an absent registry key."""
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": "<detail>",
        "instance": "<request_path>",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.types_registry.entity.v1~",
            "resource_name": key,
        },
    }


def key_conflict(operation_id):
    """The whole `409` for an `Idempotency-Key` bound to a different request."""
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~",
        "title": "Already Exists",
        "status": 409,
        "detail": (
            "this Idempotency-Key is already bound to operation "
            f"{operation_id} with a different request"
        ),
        "instance": "<request_path>",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.types_registry.operation.v1~",
            "resource_name": operation_id,
        },
    }


def timestamp(value):
    assert isinstance(value, str) and RFC3339.fullmatch(value), value
    return datetime.fromisoformat(value.replace("Z", "+00:00").replace("z", "+00:00"))


def assert_uuid(value):
    assert isinstance(value, str), value
    parsed = uuid.UUID(value)
    assert str(parsed) == value and parsed.int != 0, value


def replace_text(document, field):
    """Only explicitly named, non-contractual text may be normalized."""
    assert isinstance(document[field], str) and document[field].strip(), document
    document[field] = f"<{field}>"


def assert_operation(operation, expected, *, ordered=False, exact_messages=False):
    """Compare a terminal operation in full.

    `ordered=True` compares `items` as they arrived. Deletion outcomes are
    reported in request order so that a caller who deleted by Registry
    Reference can match identifier-keyed outcomes positionally (DESIGN §3.3);
    for registration, response order carries no contract and is sorted away.
    `exact_messages=True` compares error messages verbatim, for a scenario
    whose claim is what a message discloses.
    """
    actual = deepcopy(operation)
    assert_uuid(actual["operation_id"])
    assert timestamp(actual["created_at"]) <= timestamp(actual["started_at"]) <= timestamp(
        actual["completed_at"]
    ), actual
    for field in ("operation_id", "created_at", "started_at", "completed_at"):
        actual[field] = f"<{field}>"
    for item in actual["items"]:
        if item["error"] is not None and not exact_messages:
            replace_text(item["error"], "message")
    expected = deepcopy(expected)
    if not ordered:
        actual["items"].sort(key=lambda item: item["gts_id"])
        expected["items"].sort(key=lambda item: item["gts_id"])
    assert_json(actual, expected)


async def read_created(client, api_path, expected, operation):
    """Read an entity this operation created and compare the whole exact read.

    Status, `ETag` and body are compared as `assert_exact` does. Beyond that,
    `created_at` must fall inside the operation's run and equal `updated_at`,
    because nothing has revised the entity since.
    """
    response = await get_entity(client, api_path, expected["gts_id"], select=ENTITY_SELECT)
    assert_exact(response, {"status": 200, "etag": "<etag>", "body": expected})
    entity = response.json()
    created = timestamp(entity["origin"]["created_at"])
    assert created == timestamp(entity["origin"]["updated_at"]), entity
    assert timestamp(operation["started_at"]) <= created <= timestamp(
        operation["completed_at"]
    ), entity
    return entity


def _problem(response, status):
    assert response.status_code == status, response.text
    assert response.headers["content-type"].startswith("application/problem+json")
    assert "location" not in response.headers, response.headers
    actual = response.json()
    assert actual["instance"] == response.request.url.path, actual
    actual["instance"] = "<request_path>"
    return actual


def assert_problem(response, status, expected):
    """Compare a whole RFC-9457 response; only the path is masked.

    A refusal never carries an operation `Location`: nothing was admitted.
    """
    assert_json(_problem(response, status), expected)


def assert_not_found(response, expected):
    """A 404 names what is missing; its `detail` wording is not a contract."""
    actual = _problem(response, 404)
    replace_text(actual, "detail")
    assert_json(actual, expected)


async def assert_absent(client, api_path, document):
    """Read an absent entity and compare its complete Problem response."""
    key = document["gts_id"]
    response = await client.get(f"{api_path}/entities/{key}")
    assert_not_found(response, not_found(key))


def assert_bad_request(response, expected):
    """A refusal names its field; its wording and trace are compared."""
    assert_problem(response, 400, expected)


def invalid_argument(field, reason, description, *, resource_name=None):
    """The whole `400 invalid_argument` response with one field violation.

    `resource_name` is present when the refusal is about one named entity.
    """
    context = {"resource_type": "gts.cf.core.types_registry.entity.v1~"}
    if resource_name is not None:
        context["resource_name"] = resource_name
    context["field_violations"] = [
        {"field": field, "reason": reason, "description": description},
    ]
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": "<request_path>",
        "trace_id": TRACE_ID,
        "context": context,
    }


def gts_uuid(gts_id):
    """The Registry Reference is derived from the identifier, so it is stated exactly."""
    return str(uuid.uuid5(GTS_NAMESPACE, gts_id))


def mandatory(document, lifecycle_status="active"):
    """The four fields every projection returns, whatever `$select` names.

    A GTS Type Schema identifier ends in `~`; an Instance identifier does not.
    """
    gts_id = document["gts_id"]
    return {
        "gts_id": gts_id,
        "gts_uuid": gts_uuid(gts_id),
        "kind": "type_schema" if gts_id.endswith("~") else "instance",
        "lifecycle_status": lifecycle_status,
    }


def managed(resource_version):
    """A managed `origin`; its timestamps are validated and masked on read."""
    return {
        "type": "managed",
        "resource_version": resource_version,
        "created_at": "<created_at>",
        "updated_at": "<updated_at>",
    }


def schema_entity(document, resource_version, lifecycle_status="active", resolved_schema=None):
    """Full selected read of a registration schema without authored traits."""
    return {
        **mandatory(document, lifecycle_status),
        "origin": managed(resource_version),
        "content": document["content"],
        "resolved_schema": document["content"] if resolved_schema is None else resolved_schema,
        "effective_traits": {},
        "effective_traits_schema": {"$schema": "http://json-schema.org/draft-07/schema#"},
    }


def schema_with_id(template, gts_id):
    """Copy an authored schema under a new GTS ID with its matching `$id`."""
    document = deepcopy(template)
    document["gts_id"] = gts_id
    document["content"]["$id"] = f"gts://{gts_id}"
    return document


def instance_entity(document, resource_version, lifecycle_status="active"):
    """Full selected Instance read, with schema-only fields absent."""
    return {
        **mandatory(document, lifecycle_status),
        "origin": managed(resource_version),
        "content": document["content"],
    }


def namespace_pattern(document):
    """`gts.cf.e2e.r<uuid>.*`: every identifier of the document's test namespace."""
    vendor, package, namespace = document["gts_id"].split(".")[1:4]
    return f"gts.{vendor}.{package}.{namespace}.*"


def provenance(compat_forced):
    """A `provenance` group; implementation versions are not a scenario claim."""
    return {
        "gts_spec_version": "<gts_spec_version>",
        "gts_impl_version": "<gts_impl_version>",
        "compat_forced": compat_forced,
    }


def _entity(body):
    """Mask what a test cannot predict: timestamps and implementation versions."""
    body = deepcopy(body)
    origin = body.get("origin")
    if origin is not None:
        assert timestamp(origin["created_at"]) <= timestamp(origin["updated_at"]), body
        origin["created_at"] = "<created_at>"
        origin["updated_at"] = "<updated_at>"
    if "provenance" in body:
        for field in ("gts_spec_version", "gts_impl_version"):
            replace_text(body["provenance"], field)
    return body


def _etag(value, known_etags):
    """A known validator is compared verbatim; any other reads `<etag>`."""
    assert isinstance(value, str) and ENTITY_TAG.fullmatch(value), value
    return value if value in known_etags else "<etag>"


async def get_entity(client, api_path, key, *, select=None, if_none_match=None):
    """`GET {api}/entities/{key}`, optionally projected and conditional."""
    params = {} if select is None else {"$select": select}
    headers = {} if if_none_match is None else {"If-None-Match": if_none_match}
    return await client.get(f"{api_path}/entities/{key}", params=params, headers=headers)


async def assert_exact_entity(client, api_path, document, expected, *, etag="<etag>"):
    """Read every registration document field and compare the whole HTTP result."""
    response = await get_entity(client, api_path, document["gts_id"], select=ENTITY_SELECT)
    assert_exact(response, {"status": 200, "etag": etag, "body": expected})
    return response


def assert_exact(response, expected, *, known_etags=()):
    """Compare an exact read as one JSON value: `status`, `etag` and `body`.

    An expected raw ETag is compared verbatim. `"etag": "<etag>"` is a
    well-formed validator that is none of `known_etags`, so a changed ETag is
    part of the expectation. A `304` has no `body` key at all. Returns the raw
    ETag for later conditional requests.
    """
    actual = {"status": response.status_code}
    etag = response.headers.get("etag")
    if etag is not None:
        actual["etag"] = _etag(etag, {*known_etags, expected.get("etag")})
    if response.content:
        assert response.headers["content-type"].startswith("application/json")
        actual["body"] = _entity(response.json())
    assert_json(actual, expected)
    return etag


async def batch_get(client, api_path, items, *, select=None, headers=None):
    """`POST {api}/entities:batchGet` with wire-shaped `items`."""
    body = {"items": items}
    if select is not None:
        body["$select"] = select
    return await client.post(
        f"{api_path}/entities:batchGet", json=body, headers=headers or {}
    )


def assert_batch(response, expected, *, known_etags=()):
    """Compare a batch read body in full, results sorted by echoed key.

    Result order is not contractual; sorting both sides still catches a
    missing, extra or duplicated key. `etag` follows `assert_exact`'s rule.
    Returns each key's raw `etag`.
    """
    assert response.status_code == 200, response.text
    assert response.headers["content-type"].startswith("application/json")
    body = response.json()
    actual = deepcopy(body)
    known_etags = {*known_etags, *(item.get("etag") for item in expected["items"])}
    for item in actual["items"]:
        if "etag" in item:
            item["etag"] = _etag(item["etag"], known_etags)
        if "entity" in item:
            item["entity"] = _entity(item["entity"])
    expected = deepcopy(expected)
    for side in (actual, expected):
        side["items"].sort(key=lambda item: item["entity_key"])
    assert_json(actual, expected)
    return {item["entity_key"]: item.get("etag") for item in body["items"]}


async def discover(client, api_path, params):
    """Return one discovery page's raw response."""
    return await client.get(f"{api_path}/entities", params=params)


def _discovery_page(response):
    """A page is JSON and never carries a validator, unlike an exact read."""
    assert response.status_code == 200, response.text
    assert response.headers["content-type"].startswith("application/json")
    assert "etag" not in response.headers, response.headers
    return response.json()


async def walk(client, api_path, params, *, cursor=None, max_pages=20):
    """Follow `next_cursor` to the last page and return every page.

    Starts at the beginning, or at `cursor` when resuming an earlier walk.
    """
    pages = []
    while True:
        query = params if cursor is None else {**params, "cursor": cursor}
        pages.append(_discovery_page(await discover(client, api_path, query)))
        cursor = pages[-1]["page_info"].get("next_cursor")
        if cursor is None:
            return pages
        assert len(pages) < max_pages, f"the walk did not terminate: {pages}"


def paged(items, limit):
    """The pages a walk at `limit` must return: every page but the last is full
    and carries a cursor, so an empty result is one page without one."""
    chunks = [items[start : start + limit] for start in range(0, len(items), limit)]
    pages = [
        {"items": chunk, "page_info": {"next_cursor": "<next_cursor>", "limit": limit}}
        for chunk in chunks or [[]]
    ]
    del pages[-1]["page_info"]["next_cursor"]
    return pages


def _page(page):
    """Cursors are opaque, so only their presence counts."""
    page = deepcopy(page)
    page["items"] = [_entity(item) for item in page["items"]]
    if "next_cursor" in page["page_info"]:
        replace_text(page["page_info"], "next_cursor")
    return page


def assert_pages(pages, expected):
    """Compare a whole walk, page by page."""
    assert_json([_page(page) for page in pages], expected)


def assert_page(response, expected):
    """Compare one discovery page in full and return its raw body."""
    page = _discovery_page(response)
    assert_json(_page(page), expected)
    return page


def _accept(response, expected_receipt):
    """Validate a 202 receipt and return it with the operation's absolute URL."""
    assert response.status_code == 202, response.text
    assert response.headers["content-type"].startswith("application/json")
    receipt = response.json()
    assert_uuid(receipt["operation_id"])
    assert receipt["status"] in {"pending", "running", "completed"}, receipt
    normalized = {**receipt, "operation_id": "<operation_id>", "status": "<status>"}
    assert_json(normalized, expected_receipt)
    assert "location" in response.headers, response.headers
    return receipt, urljoin(str(response.url), response.headers["location"])


async def poll_operation(client, receipt, location, kind, *, dry_run=False):
    """Return terminal outcomes; completed never implies all items succeeded."""
    last_operation = None
    try:
        async with asyncio.timeout(4):
            while True:
                polled = await client.get(location)
                assert polled.status_code == 200, polled.text
                assert polled.headers["content-type"].startswith("application/json")
                last_operation = polled.json()
                assert last_operation["operation_id"] == receipt["operation_id"]
                assert last_operation["kind"] == kind, last_operation
                assert last_operation["dry_run"] is dry_run, last_operation
                status = last_operation["status"]
                assert status in {"pending", "running", "completed"}, last_operation
                if status == "completed":
                    return last_operation
                await asyncio.sleep(0.05)
    except (TimeoutError, httpx.TimeoutException):
        raise AssertionError(
            f"Operation {receipt['operation_id']} did not complete at {location}; "
            f"last operation (including item errors): {last_operation}"
        ) from None


def _idempotency_key():
    return {"Idempotency-Key": str(uuid.uuid4())}


async def post_registration(client, api_path, candidates, *, idempotency_key=None, dry_run=False):
    """Submit registration and return the raw HTTP response for receipt/refusal tests."""
    body = {"items": candidates}
    if dry_run:
        body["dry_run"] = True
    return await client.post(
        f"{api_path}/entities",
        headers={
            "Idempotency-Key": str(uuid.uuid4()) if idempotency_key is None else idempotency_key
        },
        json=body,
    )


async def submit_and_poll(
    client, api_path, candidates, expected_receipt, *, dry_run=False, idempotency_key=None
):
    """Register a batch through `POST {api}/entities`."""
    response = await post_registration(
        client, api_path, candidates, dry_run=dry_run, idempotency_key=idempotency_key
    )
    receipt, location = _accept(response, expected_receipt)
    return await poll_operation(client, receipt, location, "registration", dry_run=dry_run)


async def register_and_assert(
    client, api_path, candidates, *items, dry_run=False, idempotency_key=None
):
    """Submit, poll and compare the whole registration operation."""
    operation = await submit_and_poll(
        client,
        api_path,
        candidates,
        RECEIPT,
        dry_run=dry_run,
        idempotency_key=idempotency_key,
    )
    assert_operation(operation, completed("registration", *items, dry_run=dry_run))
    return operation


def target(document, expected_resource_version, *, key=None):
    """A `:batchDelete` item naming `document` by GTS ID, or by `key` when given."""
    return {
        "entity_key": document["gts_id"] if key is None else key,
        "expected_resource_version": expected_resource_version,
    }


async def post_batch_delete(
    client, api_path, targets, *, idempotency_key=None, dry_run=False, headers=None
):
    """Submit `POST {api}/entities:batchDelete` and return the raw response."""
    body = {"items": targets}
    if dry_run:
        body["dry_run"] = True
    return await client.post(
        f"{api_path}/entities:batchDelete",
        headers={**_key_header(idempotency_key), **(headers or {})},
        json=body,
    )


async def delete_one(
    client,
    api_path,
    key,
    expected_resource_version,
    *,
    idempotency_key=None,
    dry_run=False,
    headers=None,
):
    """Submit `DELETE {api}/entities/{key}` and return the raw response."""
    params = {}
    if expected_resource_version is not None:
        params["expected_resource_version"] = expected_resource_version
    if dry_run:
        params["dry_run"] = "true"
    return await client.delete(
        f"{api_path}/entities/{key}",
        headers={**_key_header(idempotency_key), **(headers or {})},
        params=params,
    )


def _key_header(idempotency_key):
    return {"Idempotency-Key": str(uuid.uuid4()) if idempotency_key is None else idempotency_key}


async def await_deletion(client, response, expected_receipt=RECEIPT, *, dry_run=False):
    """Accept a deletion receipt and poll its operation to completion."""
    receipt, location = _accept(response, expected_receipt)
    return await poll_operation(client, receipt, location, "deletion", dry_run=dry_run)


async def delete_batch_and_poll(client, api_path, targets, expected_receipt):
    """Delete a batch through `POST {api}/entities:batchDelete`.

    Each target is `{"entity_key": ..., "expected_resource_version": ...}`; the key is
    a canonical GTS identifier or a Registry Reference UUID.
    """
    return await await_deletion(
        client, await post_batch_delete(client, api_path, targets), expected_receipt
    )


async def delete_one_and_poll(client, api_path, key, expected_resource_version, expected_receipt):
    """Delete one entity through `DELETE {api}/entities/{key}`."""
    return await await_deletion(
        client,
        await delete_one(client, api_path, key, expected_resource_version),
        expected_receipt,
    )


async def delete_and_assert(
    client, api_path, targets, *items, dry_run=False, idempotency_key=None, exact_messages=False
):
    """Batch-delete, poll and compare the whole operation in request order."""
    operation = await await_deletion(
        client,
        await post_batch_delete(
            client, api_path, targets, idempotency_key=idempotency_key, dry_run=dry_run
        ),
        dry_run=dry_run,
    )
    assert_operation(
        operation,
        completed("deletion", *items, dry_run=dry_run),
        ordered=True,
        exact_messages=exact_messages,
    )
    return operation


def removal(subject, status, resource_version, reason=None, *, message="<message>", **context):
    """A complete deletion item; `subject` is a document, named by its GTS ID,
    or the key the item echoes: a GTS ID as sent, a UUID in canonical lowercase
    hyphenated form."""
    key = subject["gts_id"] if isinstance(subject, dict) else subject
    return {
        "entity_key": key,
        "status": status,
        "resource_version": resource_version,
        "error": None
        if reason is None
        else {"reason": reason, "message": message, "context": context},
    }


def blocked(document, count):
    """A deletion refused for live direct dependants; the diagnostic states how
    many and never which, so it is compared verbatim (`exact_messages=True`)."""
    return removal(
        document,
        "failed",
        None,
        "has_registered_dependents",
        message=f"'{document['gts_id']}' has {count} live direct registered "
        "dependants; delete or revise them first",
    )


async def read_entity(client, api_path, key):
    """Read one entity, tombstone or not, with its origin and documents."""
    response = await client.get(
        f"{api_path}/entities/{key}", params={"$select": ENTITY_SELECT}
    )
    assert response.status_code == 200, response.text
    assert response.headers["content-type"].startswith("application/json")
    return response.json()


async def read_tombstone(client, api_path, before, operation):
    """A tombstone stays exact-readable until purge (ADR-0013).

    Compared against the body observed before the deletion rather than against
    a hand-written dictionary: the claim is that *nothing* changed except the
    lifecycle, the version and the update timestamp.
    """
    actual = await read_entity(client, api_path, before["gts_id"])
    updated = timestamp(actual["origin"]["updated_at"])
    assert timestamp(operation["started_at"]) <= updated <= timestamp(
        operation["completed_at"]
    ), actual
    assert_json(
        actual,
        {
            **before,
            "lifecycle_status": "deleted",
            "origin": {
                **before["origin"],
                "resource_version": before["origin"]["resource_version"] + 1,
                "updated_at": actual["origin"]["updated_at"],
            },
        },
    )
    return actual
