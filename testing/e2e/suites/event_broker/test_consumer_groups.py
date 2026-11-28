"""Consumer group CRUD scenarios (consumer/groups/1.01-1.10, consumer/subscriptions/1.16-1.17).

Each test creates its own group to avoid cross-test interference.
``groups/1.08`` (named group JOIN) is xfail: the gear does not implement
named consumer groups yet, and a JOIN naming one answers 501.
The authenticated ``groups/1.09`` cases use one subject in sibling tenants.
The authenticated ``groups/1.10`` case uses different subjects in one tenant.
Only the group creator may DELETE the group or JOIN it
(``groups/1.10`` and ``subscriptions/1.16``).
``subscriptions/1.17`` (JOIN without ``consume`` on the group) needs an
action-aware policy, which no config of this suite runs, so it is strict xfail.
``groups/1.06`` validation tests (client_agent length and grammar, description
encoding) run live; the server enforces each rule.
"""

from __future__ import annotations

import re
import uuid
from datetime import datetime

import pytest

from .conftest import (
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_4P,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TOPIC_4P,
    TOPIC_STREAM,
    TRACE_ID,
)
from .test_consumer_subscriptions import (
    TOKEN_PRINCIPAL_C,
    _created_subscription_id,
    _subscription_created_at,
)

TOKEN_PRINCIPAL_A_IN_TB = "eb-e2e-principal-a-in-tb"


async def test_create_anonymous_group_returns_201(api):
    """scenario: consumer/groups/1.01-positive-create-anonymous-group.md"""
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "order-fulfilment workers"},
        )
        assert resp.status_code == 201
        assert resp.headers["content-type"] == "application/json"
        location = resp.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        assert location == f"{API_BASE}/consumer-groups/{group_id}"
        read_resp = await client.get(f"/consumer-groups/{group_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    created_at = read_body["created_at"]
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
        created_at,
    )
    datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    expected = {
        "id": group_id,
        "kind": "anonymous",
        "tenant": DEFAULT_TENANT_ID,
        "owner_principal": DEFAULT_SUBJECT_ID,
        "description": "order-fulfilment workers",
        "created_at": created_at,
    }
    assert resp.json() == expected
    assert read_body == expected


async def test_create_anonymous_group_no_body_returns_201(api):
    """scenario: consumer/groups/1.01 - the request body is optional."""
    async with api() as client:
        request = client.build_request("POST", "/consumer-groups")
        assert request.content == b""
        resp = await client.send(request)
        assert resp.status_code == 201
        assert resp.headers["content-type"] == "application/json"
        location = resp.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        assert location == f"{API_BASE}/consumer-groups/{group_id}"
        read_resp = await client.get(f"/consumer-groups/{group_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    created_at = read_body["created_at"]
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
        created_at,
    )
    datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    expected = {
        "id": group_id,
        "kind": "anonymous",
        "tenant": DEFAULT_TENANT_ID,
        "owner_principal": DEFAULT_SUBJECT_ID,
        "description": None,
        "created_at": created_at,
    }
    assert resp.json() == expected
    assert read_body == expected


async def test_get_group_by_id_returns_200(api):
    """scenario: consumer/groups/1.02-positive-get-group-by-id.md"""
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "test group"},
        )
        assert resp.status_code == 201
        assert resp.headers["content-type"] == "application/json"
        location = resp.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        assert location == f"{API_BASE}/consumer-groups/{group_id}"
        read_resp = await client.get(f"/consumer-groups/{group_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    created_at = read_body["created_at"]
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
        created_at,
    )
    datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    expected = {
        "id": group_id,
        "kind": "anonymous",
        "tenant": DEFAULT_TENANT_ID,
        "owner_principal": DEFAULT_SUBJECT_ID,
        "description": "test group",
        "created_at": created_at,
    }
    assert resp.json() == expected
    assert read_body == expected


async def test_list_groups_returns_paged_list(api):
    """scenario: consumer/groups/1.03-positive-list-groups.md"""
    async with api() as client:
        create_resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "order-fulfilment workers"},
        )
        assert create_resp.status_code == 201
        assert create_resp.headers["content-type"] == "application/json"
        location = create_resp.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        read_resp = await client.get(group_path)
        assert read_resp.status_code == 200
        assert read_resp.headers["content-type"] == "application/json"
        group_body = read_resp.json()
        assert isinstance(group_body, dict)
        assert isinstance(group_body.get("created_at"), str)
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": "order-fulfilment workers",
            "created_at": _subscription_created_at(group_body),
        }
        assert create_resp.json() == expected_group
        assert group_body == expected_group

        list_resp = await client.get("/consumer-groups")
        seen_cursors = set()
        while True:
            assert list_resp.status_code == 200
            assert list_resp.headers["content-type"] == "application/json"
            page = list_resp.json()
            assert isinstance(page, dict)
            assert set(page) == {"items", "page_info"}
            assert isinstance(page["items"], list)
            assert isinstance(page["page_info"], dict)
            assert set(page["page_info"]) == {"next_cursor", "prev_cursor", "limit"}
            assert page["page_info"]["limit"] == 25
            found = [item for item in page["items"] if item["id"] == group_id]
            if found:
                assert found == [expected_group]
                break
            cursor = page["page_info"]["next_cursor"]
            assert isinstance(cursor, str), f"group {group_id} not found in the unfiltered list"
            assert cursor
            assert cursor not in seen_cursors
            seen_cursors.add(cursor)
            list_resp = await client.get("/consumer-groups", params={"cursor": cursor})

        group_filter = {"$filter": f"id eq '{group_id}'"}
        page_info = {"next_cursor": None, "prev_cursor": None, "limit": 25}
        filtered = await client.get("/consumer-groups", params=group_filter)
        assert filtered.status_code == 200
        assert filtered.headers["content-type"] == "application/json"
        assert filtered.json() == {"items": [expected_group], "page_info": page_info}

        deleted = await client.delete(group_path)
        assert deleted.status_code == 204
        assert deleted.content == b""
        empty = await client.get("/consumer-groups", params=group_filter)
        assert empty.status_code == 200
        assert empty.headers["content-type"] == "application/json"
        assert empty.json() == {"items": [], "page_info": page_info}


async def test_delete_empty_group_returns_204(api):
    """scenario: consumer/groups/1.04-positive-delete-empty-group.md"""
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

        del_resp = await client.delete(f"/consumer-groups/{group_id}")
    assert del_resp.status_code == 204
    assert del_resp.text == ""


async def test_delete_group_with_active_members_returns_409(api):
    """scenario: consumer/groups/1.05-negative-delete-group-with-active-members.md"""
    async with api() as client:
        created = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "contract-active"},
        )
        assert created.status_code == 201
        assert created.headers["content-type"] == "application/json"
        location = created.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        group_before = await client.get(group_path)
        assert group_before.status_code == 200
        assert group_before.headers["content-type"] == "application/json"
        group_body = group_before.json()
        assert isinstance(group_body, dict)
        assert isinstance(group_body.get("created_at"), str)
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": "contract-active",
            "created_at": _subscription_created_at(group_body),
        }
        assert created.json() == expected_group
        assert group_body == expected_group

        interest = {
            "topic": TOPIC_4P,
            "subject_tenant": group_body["tenant"],
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_4P],
        }
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            },
        )
        assert joined.status_code == 201
        assert joined.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(joined)
        member_path = f"/subscriptions/{sub_id}"

        member_before = await client.get(member_path)
        assert member_before.status_code == 200
        assert member_before.headers["content-type"] == "application/json"
        member_body = member_before.json()
        assert isinstance(member_body, dict)
        assert isinstance(member_body.get("created_at"), str)
        member_interest = {**interest, "barrier_mode": "respect", "filter": None}
        expected_member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [member_interest],
            "assigned": [{"topic": TOPIC_4P, "partition": partition} for partition in range(4)],
            "topology_version": 1,
            "created_at": _subscription_created_at(member_body),
        }
        assert joined.json() == {
            **expected_member,
            "interests": [{**member_interest, "unmatched_types": []}],
        }
        assert member_body == expected_member

        denied = await client.delete(group_path)
        assert denied.status_code == 409
        assert denied.headers["content-type"] == "application/problem+json"
        assert denied.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 409,
            "detail": "Operation precondition not met",
            "instance": f"{API_BASE}{group_path}",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
                "violations": [
                    {
                        "type": "consumer_group_has_active_members",
                        "subject": group_id,
                        "description": f"consumer group '{group_id}' still has active members",
                    },
                ],
            },
        }

        group_after = await client.get(group_path)
        assert group_after.status_code == 200
        assert group_after.headers["content-type"] == "application/json"
        assert group_after.json() == expected_group
        member_after = await client.get(member_path)
        assert member_after.status_code == 200
        assert member_after.headers["content-type"] == "application/json"
        assert member_after.json() == expected_member

        left = await client.delete(member_path)
        assert left.status_code == 204
        assert left.content == b""
        deleted = await client.delete(group_path)
        assert deleted.status_code == 204
        assert deleted.content == b""


async def test_client_agent_too_long_returns_400(api):
    """scenario: consumer/groups/1.06-negative-invalid-client-agent.md

    A ``client_agent`` of 257 bytes (one past the 256-byte cap) is rejected
    with 400.  The error names the field, states the bound and the measured
    length, and carries no part of the submitted value.
    """
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "x" * 257},
        )
    assert resp.status_code == 400
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/consumer-groups",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "client_agent",
                    "description": "must be 1-256 bytes, got 257",
                    "reason": "field_too_long",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_client_agent_breaking_the_user_agent_grammar_returns_400(api):
    """scenario: consumer/groups/1.06-negative-invalid-client-agent.md

    A ``client_agent`` that is printable ASCII within 256 bytes but ends in
    ``/`` with no version breaks the RFC 9110 User-Agent grammar and is
    rejected with 400.  The error names the field and the grammar, and
    carries no part of the submitted value.
    """
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "agent/"},
        )
    assert resp.status_code == 400
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/consumer-groups",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "client_agent",
                    "description": (
                        "must follow the RFC 9110 User-Agent grammar: "
                        "product *( SP ( product / comment ) )"
                    ),
                    "reason": "user_agent_grammar",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_description_non_ascii_returns_400(api):
    """scenario: consumer/groups/1.06-negative-invalid-client-agent.md

    A ``description`` carrying bytes outside the printable-ASCII range
    (0x20-0x7E) is rejected with 400.  The error names the field and the
    ascii_only rule; it does not reproduce the submitted value.
    """
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "é\x01"},
        )
    assert resp.status_code == 400
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/consumer-groups",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "description",
                    "description": "must contain only printable ASCII (0x20-0x7E)",
                    "reason": "ascii_only",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_get_unknown_group_returns_404(api):
    """scenario: consumer/groups/1.07-negative-get-unknown-group.md"""
    group_id = "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000"
    async with api() as client:
        resp = await client.get(f"/consumer-groups/{group_id}")
    assert resp.status_code == 404
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": f"consumer group '{group_id}' is not registered",
        "instance": f"{API_BASE}/consumer-groups/{group_id}",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
            "resource_name": group_id,
        },
    }


@pytest.mark.xfail(
    reason=(
        "named consumer groups are not implemented: a JOIN naming one answers "
        "501 (domain/local_broker.rs), so seeding one would not make it testable"
    ),
    strict=False,
)
async def test_named_group_join_without_create_step(api):
    """scenario: consumer/groups/1.08-positive-named-group-join.md

    A named consumer group has a well-known GTS identifier registered via
    types_registry at startup.  The consumer JOINs directly without a prior
    POST /consumer-groups.  This test is xfail until the gear implements named
    groups; JOIN currently answers 501 Not Implemented.
    """
    named_group_id = "gts.cf.core.events.consumer_group.v1~cf.e2e.event_broker.named_group.v1"
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": named_group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )
    assert sub_resp.status_code == 201


async def test_cross_tenant_get_anonymous_group_returns_403(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    The same subject has consume in its own tenant, making this a reach check.
    """
    async with (
        api(TOKEN_PRINCIPAL_A) as owner,
        api(TOKEN_PRINCIPAL_A_IN_TB) as caller,
    ):
        create = await owner.post("/consumer-groups")
        assert create.status_code == 201
        assert create.headers["content-type"] == "application/json"
        location = create.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        before_group = await owner.get(group_path)
        assert before_group.status_code == 200
        assert before_group.headers["content-type"] == "application/json"
        before_body = before_group.json()
        assert isinstance(before_body, dict)
        created_at = before_body.get("created_at")
        assert isinstance(created_at, str)
        assert re.fullmatch(
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
            created_at,
        )
        datetime.fromisoformat(created_at.replace("Z", "+00:00"))
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": created_at,
        }
        assert create.json() == expected_group
        assert before_body == expected_group

        group_filter = {"$filter": f"consumer_group eq '{group_id}'"}
        empty_page = {
            "items": [],
            "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
        }
        before_members = await owner.get("/subscriptions", params=group_filter)
        assert before_members.status_code == 200
        assert before_members.headers["content-type"] == "application/json"
        assert before_members.json() == empty_page

        refused = await caller.get(group_path)
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}{group_path}",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        after_group = await owner.get(group_path)
        assert after_group.status_code == 200
        assert after_group.headers["content-type"] == "application/json"
        assert after_group.json() == expected_group
        after_members = await owner.get("/subscriptions", params=group_filter)
        assert after_members.status_code == 200
        assert after_members.headers["content-type"] == "application/json"
        assert after_members.json() == empty_page


async def test_cross_tenant_delete_anonymous_group_returns_403(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    The same subject has manage in its own tenant, making this a reach check.
    """
    async with (
        api(TOKEN_PRINCIPAL_A) as owner,
        api(TOKEN_PRINCIPAL_A_IN_TB) as caller,
    ):
        create = await owner.post("/consumer-groups")
        assert create.status_code == 201
        assert create.headers["content-type"] == "application/json"
        location = create.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        before_group = await owner.get(group_path)
        assert before_group.status_code == 200
        assert before_group.headers["content-type"] == "application/json"
        before_body = before_group.json()
        assert isinstance(before_body, dict)
        created_at = before_body.get("created_at")
        assert isinstance(created_at, str)
        assert re.fullmatch(
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
            created_at,
        )
        datetime.fromisoformat(created_at.replace("Z", "+00:00"))
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": created_at,
        }
        assert create.json() == expected_group
        assert before_body == expected_group

        group_filter = {"$filter": f"consumer_group eq '{group_id}'"}
        empty_page = {
            "items": [],
            "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
        }
        before_members = await owner.get("/subscriptions", params=group_filter)
        assert before_members.status_code == 200
        assert before_members.headers["content-type"] == "application/json"
        assert before_members.json() == empty_page

        refused = await caller.delete(group_path)
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}{group_path}",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        after_group = await owner.get(group_path)
        assert after_group.status_code == 200
        assert after_group.headers["content-type"] == "application/json"
        assert after_group.json() == expected_group
        after_members = await owner.get("/subscriptions", params=group_filter)
        assert after_members.status_code == 200
        assert after_members.headers["content-type"] == "application/json"
        assert after_members.json() == empty_page

        deleted = await owner.delete(group_path)
        assert deleted.status_code == 204
        assert deleted.content == b""


async def test_list_excludes_other_tenant_anonymous_groups(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    The same subject in two sibling tenants isolates the tenant reach check.
    """
    sibling_tenant = "00000000-0000-0000-0000-000000000003"
    group_collection = "/consumer-groups"
    id_prefix = "gts.cf.core.events.consumer_group.v1~"
    groups = {}
    baseline = {}
    async with (
        api(TOKEN_PRINCIPAL_A) as tenant_a,
        api(TOKEN_PRINCIPAL_A_IN_TB) as tenant_b,
    ):
        actors = ((tenant_a, DEFAULT_TENANT_ID), (tenant_b, sibling_tenant))
        for after_creation in (False, True):
            if after_creation:
                for client, tenant in actors:
                    request = client.build_request("POST", group_collection)
                    assert request.content == b""
                    created = await client.send(request)
                    assert created.status_code == 201
                    assert created.headers["content-type"] == "application/json"
                    location = created.headers["location"]
                    group_id = location.removeprefix(f"{API_BASE}{group_collection}/")
                    assert group_id.startswith(id_prefix)
                    issued_uuid = group_id.removeprefix(id_prefix)
                    assert str(uuid.UUID(issued_uuid)) == issued_uuid
                    group_path = f"{group_collection}/{group_id}"
                    assert location == f"{API_BASE}{group_path}"
                    group = created.json()
                    assert isinstance(group, dict)
                    assert isinstance(group.get("created_at"), str)
                    expected = {
                        "id": group_id,
                        "kind": "anonymous",
                        "tenant": tenant,
                        "owner_principal": DEFAULT_SUBJECT_ID,
                        "description": None,
                        "created_at": _subscription_created_at(group),
                    }
                    assert group == expected
                    read = await client.get(group_path)
                    assert read.status_code == 200
                    assert read.headers["content-type"] == "application/json"
                    assert read.json() == expected
                    groups[tenant] = expected
                assert groups[DEFAULT_TENANT_ID]["id"] != groups[sibling_tenant]["id"]

            for client, tenant in actors:
                if after_creation:
                    other_tenant = (
                        sibling_tenant if tenant == DEFAULT_TENANT_ID else DEFAULT_TENANT_ID
                    )
                    foreign_id = groups[other_tenant]["id"]
                cursor = None
                seen_cursors = set()
                visible = {}
                prior_id = ""
                # A malformed pager with endlessly new cursors cannot scan past 2,500 rows.
                for _ in range(100):
                    params = {} if cursor is None else {"cursor": cursor}
                    listed = await client.get(group_collection, params=params)
                    assert listed.status_code == 200
                    assert listed.headers["content-type"] == "application/json"
                    page = listed.json()
                    assert isinstance(page, dict)
                    assert set(page) == {"items", "page_info"}
                    items, page_info = page["items"], page["page_info"]
                    assert isinstance(items, list)
                    assert len(items) <= 25
                    assert isinstance(page_info, dict)
                    assert set(page_info) == {"next_cursor", "prev_cursor", "limit"}
                    assert type(page_info["limit"]) is int
                    assert page_info["limit"] == 25
                    previous = page_info["prev_cursor"]
                    if cursor is None:
                        assert previous is None
                    else:
                        assert isinstance(previous, str) and previous
                        assert items
                    for item in items:
                        assert isinstance(item, dict)
                        assert set(item) == {
                            "id", "kind", "tenant", "owner_principal", "description", "created_at",
                        }
                        group_id = item["id"]
                        assert isinstance(group_id, str) and group_id
                        if after_creation:
                            assert group_id != foreign_id
                        assert group_id not in visible
                        assert prior_id < group_id
                        visible[group_id] = item
                        prior_id = group_id
                    next_cursor = page_info["next_cursor"]
                    if next_cursor is None:
                        break
                    assert isinstance(next_cursor, str) and next_cursor
                    assert next_cursor not in seen_cursors
                    assert len(items) == 25
                    seen_cursors.add(next_cursor)
                    cursor = next_cursor
                else:
                    pytest.fail("unfiltered group list exceeds 100 pages")

                expected_inventory = visible
                if after_creation:
                    own_group = groups[tenant]
                    assert own_group["id"] not in baseline[tenant]
                    expected_inventory = {**baseline[tenant], own_group["id"]: own_group}
                    assert visible == expected_inventory
                else:
                    baseline[tenant] = visible
                if cursor is None:
                    assert page == {
                        "items": [expected_inventory[key] for key in sorted(expected_inventory)],
                        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
                    }


async def test_join_anonymous_group_not_owned(api):
    """scenario: consumer/subscriptions/1.16-negative-join-anonymous-group-not-owned.md

    A peer in the owner's tenant fails ownership before the group consume check.
    """
    async with (
        api(TOKEN_PRINCIPAL_A) as owner,
        api(TOKEN_PRINCIPAL_B) as caller,
    ):
        create = await owner.post("/consumer-groups")
        assert create.status_code == 201
        assert create.headers["content-type"] == "application/json"
        location = create.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        id_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id.startswith(id_prefix)
        issued_uuid = group_id.removeprefix(id_prefix)
        assert str(uuid.UUID(issued_uuid)) == issued_uuid
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        before_group = await owner.get(group_path)
        assert before_group.status_code == 200
        assert before_group.headers["content-type"] == "application/json"
        before_body = before_group.json()
        assert isinstance(before_body, dict)
        created_at = before_body.get("created_at")
        assert isinstance(created_at, str)
        assert re.fullmatch(
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
            created_at,
        )
        datetime.fromisoformat(created_at.replace("Z", "+00:00"))
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": created_at,
        }
        assert create.json() == expected_group
        assert before_body == expected_group

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_STREAM],
        }
        joined = await owner.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            },
        )
        assert joined.status_code == 201
        assert joined.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(joined)
        member_path = f"/subscriptions/{sub_id}"
        member = await owner.get(member_path)
        assert member.status_code == 200
        assert member.headers["content-type"] == "application/json"
        member_body = member.json()
        assert isinstance(member_body, dict)
        assert isinstance(member_body.get("created_at"), str)
        member_interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
            "types": [EVENT_TYPE_STREAM],
            "filter": None,
        }
        expected_member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [member_interest],
            "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(member_body),
        }
        assert joined.json() == {
            **expected_member,
            "interests": [{**member_interest, "unmatched_types": []}],
        }
        assert member_body == expected_member

        group_filter = {"$filter": f"consumer_group eq '{group_id}'"}

        expected_page = {
            "items": [expected_member],
            "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
        }
        members = await owner.get("/subscriptions", params=group_filter)
        assert members.status_code == 200
        assert members.headers["content-type"] == "application/json"
        assert members.json() == expected_page

        refused = await caller.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            },
        )
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/subscriptions",
            "trace_id": TRACE_ID,
            "context": {
                "reason": f"consumer group '{group_id}' was created by another principal",
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
            },
        }

        after_member = await owner.get(member_path)
        assert after_member.status_code == 200
        assert after_member.headers["content-type"] == "application/json"
        assert after_member.json() == expected_member
        after_members = await owner.get("/subscriptions", params=group_filter)
        assert after_members.status_code == 200
        assert after_members.headers["content-type"] == "application/json"
        assert after_members.json() == expected_page


async def test_join_consumer_group_not_authorized(_action_grants_api):
    """scenario: consumer/subscriptions/1.17-negative-join-consumer-group-not-authorized.md

    The post-JOIN revocation cases for stream and SEEK run in domain/group_check_tests.rs.
    """
    tenant_id = "00000000-0000-0000-0000-000000000003"
    caller_id = "cccccccc-cccc-cccc-cccc-cccccccccccc"
    interest = {
        "topic": TOPIC_STREAM,
        "subject_tenant": tenant_id,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_STREAM],
    }
    async with (
        _action_grants_api(TOKEN_PRINCIPAL_C) as caller,
        _action_grants_api(TOKEN_PRINCIPAL_A_IN_TB) as observer,
    ):
        groups = []
        for creator, owner_id in ((caller, caller_id), (observer, DEFAULT_SUBJECT_ID)):
            created = await creator.post("/consumer-groups")
            assert (created.status_code, created.headers["content-type"]) == (
                201, "application/json",
            )
            location = created.headers["location"]
            group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
            group_family = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(group_family)
            issued_uuid = group_id.removeprefix(group_family)
            assert str(uuid.UUID(issued_uuid)) == issued_uuid
            assert location == f"{API_BASE}/consumer-groups/{group_id}"
            body = created.json()
            assert isinstance(body, dict)
            created_at = body.get("created_at")
            assert isinstance(created_at, str)
            assert re.fullmatch(
                r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
                created_at,
            )
            datetime.fromisoformat(created_at.replace("Z", "+00:00"))
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": tenant_id,
                "owner_principal": owner_id,
                "description": None,
                "created_at": created_at,
            }
            assert body == expected_group
            groups.append(expected_group)
        target_group, control_group = groups
        group_id = target_group["id"]
        control_group_id = control_group["id"]
        assert group_id != control_group_id
        group_path = f"/consumer-groups/{group_id}"
        group_filter = {"$filter": f"consumer_group eq '{group_id}'"}

        control = await observer.post(
            "/subscriptions",
            json={
                "consumer_group": control_group_id,
                "client_agent": "worker/1.0.0",
                "interests": [interest],
            },
        )
        assert (control.status_code, control.headers["content-type"]) == (201, "application/json")
        control_id = _created_subscription_id(control)
        control_body = control.json()
        assert isinstance(control_body, dict)
        assert isinstance(control_body.get("created_at"), str)
        control_interest = {**interest, "barrier_mode": "respect", "filter": None}
        expected_control = {
            "id": control_id,
            "consumer_group": control_group_id,
            "client_agent": "worker/1.0.0",
            "interests": [control_interest],
            "assigned": [{"topic": interest["topic"], "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(control_body),
        }
        assert control_body == {
            **expected_control,
            "interests": [{**control_interest, "unmatched_types": []}],
        }
        assert type(control_body["topology_version"]) is int
        assert type(control_body["assigned"][0]["partition"]) is int

        page_info = {"next_cursor": None, "prev_cursor": None, "limit": 25}

        visible = await observer.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{control_group_id}'"},
        )
        assert (visible.status_code, visible.headers["content-type"]) == (200, "application/json")
        visible_page = visible.json()
        assert visible_page == {"items": [expected_control], "page_info": page_info}
        assert type(visible_page["page_info"]["limit"]) is int
        assert type(visible_page["items"][0]["topology_version"]) is int
        assert type(visible_page["items"][0]["assigned"][0]["partition"]) is int

        before_group = await observer.get(group_path)
        assert (before_group.status_code, before_group.headers["content-type"]) == (
            200, "application/json",
        )
        assert before_group.json() == target_group

        before_members = await observer.get("/subscriptions", params=group_filter)
        assert (before_members.status_code, before_members.headers["content-type"]) == (
            200, "application/json",
        )
        before_page = before_members.json()
        assert before_page == {"items": [], "page_info": page_info}
        assert type(before_page["page_info"]["limit"]) is int

        refused = await caller.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "worker/1.0.0",
                "interests": [interest],
            },
        )
        assert (refused.request.method, refused.request.url.path) == (
            "POST", f"{API_BASE}/subscriptions",
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            403, "application/problem+json",
        )
        problem = refused.json()
        assert problem == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/subscriptions",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
            },
        }
        assert type(problem["status"]) is int

        caller_members = await caller.get("/subscriptions", params=group_filter)
        assert (caller_members.status_code, caller_members.headers["content-type"]) == (
            200, "application/json",
        )
        caller_page = caller_members.json()
        assert caller_page == {"items": [], "page_info": page_info}
        assert type(caller_page["page_info"]["limit"]) is int

        after_members = await observer.get("/subscriptions", params=group_filter)
        assert (after_members.status_code, after_members.headers["content-type"]) == (
            200, "application/json",
        )
        after_page = after_members.json()
        assert after_page == {"items": [], "page_info": page_info}
        assert type(after_page["page_info"]["limit"]) is int

        after_group = await observer.get(group_path)
        assert (after_group.status_code, after_group.headers["content-type"]) == (
            200, "application/json",
        )
        assert after_group.json() == target_group


async def test_delete_anonymous_group_not_owned(api):
    """scenario: consumer/groups/1.10-negative-delete-anonymous-group-not-owned.md"""
    async with api(TOKEN_PRINCIPAL_A) as owner, api(TOKEN_PRINCIPAL_B) as peer:
        created = await owner.post("/consumer-groups")
        assert created.status_code == 201
        assert created.headers["content-type"] == "application/json"
        location = created.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{prefix}{uuid.UUID(group_id.removeprefix(prefix))}"
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        group_body = created.json()
        assert isinstance(group_body, dict)
        assert isinstance(group_body.get("created_at"), str)
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(group_body),
        }
        assert group_body == expected_group
        owner_group = await owner.get(group_path)
        assert owner_group.status_code == 200
        assert owner_group.headers["content-type"] == "application/json"
        assert owner_group.json() == expected_group

        peer_group = await peer.get(group_path)
        assert peer_group.status_code == 200
        assert peer_group.headers["content-type"] == "application/json"
        assert peer_group.json() == expected_group

        refused = await peer.delete(group_path)
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}{group_path}",
            "trace_id": TRACE_ID,
            "context": {
                "reason": f"consumer group '{group_id}' was created by another principal",
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
            },
        }

        remaining = await owner.get(group_path)
        assert remaining.status_code == 200
        assert remaining.headers["content-type"] == "application/json"
        assert remaining.json() == expected_group

        deleted = await owner.delete(group_path)
        assert deleted.status_code == 204
        assert deleted.headers.get("content-type") is None
        assert deleted.content == b""
