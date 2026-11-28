"""Auth & permissions scenarios (auth/1.01-1.13).

Individual xfail markers identify scenarios pending conversion to the
authenticated fixture.
"""

from __future__ import annotations

import asyncio
import re
import uuid
from contextlib import aclosing
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    API_BASE,
    BACKEND,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_DEMO_GREETING,
    EVENT_TYPE_KEYED,
    EVENT_TYPE_SEL_EXACT,
    EVENT_TYPE_SEL_EXACT_SIBLING,
    EVENT_TYPE_SEL_PREFIX_PATTERN,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    TOKEN_BARRIER,
    TOKEN_NIL_SUBJECT,
    TOKEN_NIL_TENANT,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TOPIC_KEYED,
    TOPIC_SEL_EXACT,
    TOPIC_SEL_PREFIX,
    TOPIC_STREAM,
    TRACE_ID,
)
from .test_consumer_stream import _live_heartbeat, _stream_instant
from .test_consumer_subscriptions import _created_subscription_id, _subscription_created_at
from .test_flows import _resumed_multipart

TOKEN_PRINCIPAL_A_IN_TB = "eb-e2e-principal-a-in-tb"
_OTHER_TENANT = "00000000-0000-0000-0000-000000000003"
_BARRIER_TENANT = "00000000-0000-0000-0000-000000000005"

async def test_missing_bearer_token_returns_401(api):
    """scenario: auth/1.01-negative-missing-bearer-token.md"""
    async with api(None) as client:
        request = client.build_request(
            "POST",
            "/events",
            json={
                "id": "ea719f67-c0cd-4f22-82e8-52a3b8964001",
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": "00000000-0000-0000-0000-000000000002",
                "source": "x",
                "subject": "y",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-06-15T10:00:00Z",
                "data": {},
            },
        )
        assert request.url.path == "/event-broker/v1/events"
        assert request.headers["content-type"] == "application/json"
        assert request.headers["traceparent"] == (
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        )
        assert "authorization" not in request.headers
        assert "x-toolkit-internal-token" not in request.headers
        resp = await client.send(request)
    assert resp.status_code == 401
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
        "title": "Unauthenticated",
        "status": 401,
        "detail": "Authentication required",
        "instance": "/event-broker/v1/events",
        "trace_id": TRACE_ID,
        "context": {"reason": "MISSING_BEARER"},
    }


async def test_invalid_bearer_token_returns_401(api):
    """scenario: auth/1.02-negative-invalid-bearer-token.md"""
    async with api("this-is-not-a-valid-jwt") as client:
        resp = await client.get(
            "/topics",
            headers={"traceparent": "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"},
        )
    assert resp.status_code == 401
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
        "title": "Unauthenticated",
        "status": 401,
        "detail": "Authentication required",
        "instance": "/event-broker/v1/topics",
        "trace_id": "0123456789abcdef0123456789abcdef",
        "context": {"reason": "AUTHN_FAILED"},
    }


async def _join_keyed_window(client, tenant):
    created = await client.post("/consumer-groups")
    assert (created.status_code, created.headers.get("content-type")) == (201, "application/json")
    group_path = f"{API_BASE}/consumer-groups/"
    group_id = created.headers["location"].removeprefix(group_path)
    prefix = "gts.cf.core.events.consumer_group.v1~"
    group_uuid = uuid.UUID(group_id.removeprefix(prefix))
    assert created.headers["location"] == f"{group_path}{prefix}{group_uuid}"

    joined = await client.post("/subscriptions", json={
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [{
            "topic": TOPIC_KEYED,
            "types": [EVENT_TYPE_KEYED],
            "subject_tenant": tenant,
            "ownership_filter": "entire_tenant",
        }],
    })
    assert (joined.status_code, joined.headers.get("content-type")) == (201, "application/json")
    return group_uuid, _created_subscription_id(joined)


async def _read_keyed_window(frames, key, closing_id):
    window = []
    async for frame in frames:
        match frame:
            case {"kind": "heartbeat"}:
                _live_heartbeat(frame)
            case {"kind": "event", "payload": {"id": str(event_id), "data": dict(data)}}:
                if data.get("k") == key:
                    window.append(frame)
                    if event_id == closing_id:
                        return window
            case unexpected:
                pytest.fail(f"unexpected frame while reading publish window: {unexpected}")
    pytest.fail(f"stream ended before closing event {closing_id}: {window}")


@pytest.mark.timeout(45, func_only=True)
async def test_no_produce_permission_returns_403(_action_grants_api):
    """scenario: auth/1.03-negative-insufficient-permission-produce.md

    The action grants let A mark and read the window in the tenant A shares with B.
    """

    async with _action_grants_api(TOKEN_PRINCIPAL_A) as client:
        group_uuid, sub_id = await _join_keyed_window(client, DEFAULT_TENANT_ID)
        seek = await client.post(f"/subscriptions/{sub_id}:seek", json={
            "topology_version": 1,
            "positions": {TOPIC_KEYED: [{"partition": 0, "value": "latest"}]},
        })
        assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
        resolved = seek.json()
        match resolved:
            case {"positions": dict(positions)}:
                pass
            case unexpected:
                pytest.fail(f"unexpected SEEK response: {unexpected}")
        match positions.get(TOPIC_KEYED):
            case [{"partition": partition, "value": cursor}]:
                assert type(partition) is int
                assert type(cursor) is int
                assert cursor >= 0
            case unexpected:
                pytest.fail(f"unexpected partition cursor: {unexpected}")
        assert resolved == {"positions": {TOPIC_KEYED: [{"partition": 0, "value": cursor}]}}

        opening_id = str(uuid.uuid5(group_uuid, "opening"))
        refused_id = str(uuid.uuid5(group_uuid, "refused"))
        closing_id = str(uuid.uuid5(group_uuid, "closing"))
        key = str(group_uuid)
        event = {
            "type": EVENT_TYPE_KEYED,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-05-29T10:00:00Z",
            "data": {"k": key},
        }
        # Both synchronous publishes can wait up to ten seconds for storage.
        deadline = asyncio.get_running_loop().time() + 30
        async with (
            asyncio.timeout_at(deadline),
            client.stream(
                "GET", "/events:stream", params={"subscription": sub_id},
                headers={"Accept": "multipart/mixed"}, timeout=None,
            ) as opened,
            aclosing(_resumed_multipart(opened, deadline)) as frames,
        ):
            assert opened.status_code == 200
            topology = await anext(frames)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [{
                    "topic": TOPIC_KEYED, "partition": 0,
                    "offset": cursor, "last_examined": cursor,
                }],
            }
            assert type(topology["topology_version"]) is int
            assert all(type(topology["assigned"][0][field]) is int for field in (
                "partition", "offset", "last_examined",
            ))

            first_marker = await client.post(
                "/events", headers={"Prefer": "wait=10"}, json={**event, "id": opening_id},
            )
            assert (first_marker.status_code, first_marker.content) == (201, b"")
            assert first_marker.headers["preference-applied"] == "wait=10"

            async with _action_grants_api(TOKEN_PRINCIPAL_B) as denied_actor:
                resp = await denied_actor.post("/events", json={**event, "id": refused_id})
            assert resp.status_code == 403
            assert resp.headers["content-type"] == "application/problem+json"
            assert resp.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
                "title": "Permission Denied",
                "status": 403,
                "detail": "You do not have permission to perform this operation",
                "instance": f"{API_BASE}/events",
                "trace_id": TRACE_ID,
                "context": {
                    "reason": "access denied by policy",
                    "resource_type": "gts.cf.core.events.request.v1~",
                },
            }

            last_marker = await client.post(
                "/events", headers={"Prefer": "wait=10"}, json={**event, "id": closing_id},
            )
            assert (last_marker.status_code, last_marker.content) == (201, b"")
            assert last_marker.headers["preference-applied"] == "wait=10"
            window = await _read_keyed_window(frames, key, closing_id)

    previous = cursor
    for frame in window:
        payload = frame["payload"]
        assert type(payload.get("partition")) is int
        sequence = payload.get("sequence")
        assert type(sequence) is int
        assert sequence > previous
        _stream_instant(payload.get("sequence_time"))
        previous = sequence
    assert window == [
        {
            "kind": "event",
            "payload": {
                **event,
                "id": event_id,
                "trace_parent": None,
                "partition": 0,
                "sequence": marker["payload"]["sequence"],
                "sequence_time": marker["payload"]["sequence_time"],
            },
        }
        for event_id, marker in ((opening_id, window[0]), (closing_id, window[-1]))
    ]


async def test_no_consume_permission_returns_403(_action_grants_api):
    """scenario: auth/1.04-negative-insufficient-permission-consume.md"""
    tenant_id = DEFAULT_TENANT_ID
    caller_id = "dddddddd-dddd-dddd-dddd-dddddddddddd"
    interest = {
        "topic": TOPIC_SEL_PREFIX,
        "subject_tenant": tenant_id,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SEL_PREFIX_PATTERN],
    }
    async with (
        _action_grants_api("eb-e2e-barrier") as caller,
        _action_grants_api(TOKEN_PRINCIPAL_A) as observer,
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
                "client_agent": "report-runner/1.0.0",
                "session_timeout": "PT30S",
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


async def test_cross_tenant_anonymous_group_returns_403(api):
    """scenario: auth/1.05-negative-cross-tenant-anonymous-group.md

    The same subject in another tenant fails the anonymous group owner check.
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

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": _OTHER_TENANT,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_STREAM],
        }
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

        after_group = await owner.get(group_path)
        assert after_group.status_code == 200
        assert after_group.headers["content-type"] == "application/json"
        assert after_group.json() == expected_group
        after_members = await owner.get("/subscriptions", params=group_filter)
        assert after_members.status_code == 200
        assert after_members.headers["content-type"] == "application/json"
        assert after_members.json() == empty_page

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


@pytest.mark.timeout(45, func_only=True)
async def test_publish_to_a_tenant_outside_the_subtree_returns_403(api):
    """scenario: auth/1.06-negative-tenant-outside-subtree.md

    A's sibling identity marks and reads the window in TB. A@TA cannot publish there.
    """

    async with api(TOKEN_PRINCIPAL_A_IN_TB) as client:
        group_uuid, sub_id = await _join_keyed_window(client, _OTHER_TENANT)
        seek = await client.post(f"/subscriptions/{sub_id}:seek", json={
            "topology_version": 1,
            "positions": {TOPIC_KEYED: [{"partition": 0, "value": "latest"}]},
        })
        assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
        resolved = seek.json()
        match resolved:
            case {"positions": dict(positions)}:
                pass
            case unexpected:
                pytest.fail(f"unexpected SEEK response: {unexpected}")
        match positions.get(TOPIC_KEYED):
            case [{"partition": partition, "value": cursor}]:
                assert type(partition) is int
                assert type(cursor) is int
                assert cursor >= 0
            case unexpected:
                pytest.fail(f"unexpected partition cursor: {unexpected}")
        assert resolved == {"positions": {TOPIC_KEYED: [{"partition": 0, "value": cursor}]}}

        opening_id = str(uuid.uuid5(group_uuid, "opening"))
        refused_id = str(uuid.uuid5(group_uuid, "refused"))
        closing_id = str(uuid.uuid5(group_uuid, "closing"))
        key = str(group_uuid)
        event = {
            "type": EVENT_TYPE_KEYED,
            "subject_tenant": _OTHER_TENANT,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-05-29T10:00:00Z",
            "data": {"k": key},
        }
        # Both synchronous publishes can wait up to ten seconds for storage.
        deadline = asyncio.get_running_loop().time() + 30
        async with (
            asyncio.timeout_at(deadline),
            client.stream(
                "GET", "/events:stream", params={"subscription": sub_id},
                headers={"Accept": "multipart/mixed"}, timeout=None,
            ) as opened,
            aclosing(_resumed_multipart(opened, deadline)) as frames,
        ):
            assert opened.status_code == 200
            topology = await anext(frames)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [{
                    "topic": TOPIC_KEYED, "partition": 0,
                    "offset": cursor, "last_examined": cursor,
                }],
            }
            assert type(topology["topology_version"]) is int
            assert all(type(topology["assigned"][0][field]) is int for field in (
                "partition", "offset", "last_examined",
            ))

            first_marker = await client.post(
                "/events", headers={"Prefer": "wait=10"}, json={**event, "id": opening_id},
            )
            assert (first_marker.status_code, first_marker.content) == (201, b"")
            assert first_marker.headers["preference-applied"] == "wait=10"

            async with api(TOKEN_PRINCIPAL_A) as denied_actor:
                resp = await denied_actor.post("/events", json={**event, "id": refused_id})
            assert resp.status_code == 403
            assert resp.headers["content-type"] == "application/problem+json"
            assert resp.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
                "title": "Permission Denied",
                "status": 403,
                "detail": "You do not have permission to perform this operation",
                "instance": f"{API_BASE}/events",
                "trace_id": TRACE_ID,
                "context": {
                    "reason": "access denied by policy",
                    "resource_type": "gts.cf.core.events.request.v1~",
                },
            }

            last_marker = await client.post(
                "/events", headers={"Prefer": "wait=10"}, json={**event, "id": closing_id},
            )
            assert (last_marker.status_code, last_marker.content) == (201, b"")
            assert last_marker.headers["preference-applied"] == "wait=10"
            window = await _read_keyed_window(frames, key, closing_id)

    previous = cursor
    for frame in window:
        payload = frame["payload"]
        assert type(payload.get("partition")) is int
        sequence = payload.get("sequence")
        assert type(sequence) is int
        assert sequence > previous
        _stream_instant(payload.get("sequence_time"))
        previous = sequence
    assert window == [
        {
            "kind": "event",
            "payload": {
                **event,
                "id": event_id,
                "trace_parent": None,
                "partition": 0,
                "sequence": marker["payload"]["sequence"],
                "sequence_time": marker["payload"]["sequence_time"],
            },
        }
        for event_id, marker in ((opening_id, window[0]), (closing_id, window[-1]))
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_publish_across_a_barrier_with_the_grant_returns_202(_barrier_publish_api):
    """scenario: auth/1.07-positive-publish-across-barrier-with-grant.md"""
    topic = "gts.cf.core.events.topic.v1~cf.core.demo.topic.v1"
    child_tenant = "00000000-0000-0000-0000-000000000004"
    # The fixture inherits eight partitions, and the topic listing omits the count.
    partitions = range(8)
    utc_timestamp = r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z"
    event = {
        "type": EVENT_TYPE_DEMO_GREETING,
        "subject_tenant": _BARRIER_TENANT,
        "subject_type": "gts.cf.core.demo.subject.v1~",
        "source": "order-service",
        "subject": "order-1",
        "occurred_at": "2026-06-15T10:00:00Z",
        "data": {"order_id": "order-1"},
    }

    async def read_manifests(client):
        manifests = []
        for partition in partitions:
            response = await client.get(
                "/topics/segments", params={"topic": topic, "partition": partition},
            )
            assert (response.status_code, response.headers["content-type"]) == (
                200, "application/json",
            )
            manifest = response.json()
            assert isinstance(manifest, dict)
            assert set(manifest) == {
                "topic", "partition", "start_sequence", "end_sequence", "start_time",
                "end_time", "segments",
            }
            for field in ("partition", "start_sequence", "end_sequence"):
                assert type(manifest[field]) is int
            assert isinstance(manifest["segments"], list)
            if BACKEND in ("memory", "sqlite"):
                for segment in manifest["segments"]:
                    assert isinstance(segment, dict)
                    assert all(type(value) is int for value in segment.values())
            manifests.append(manifest)
        return manifests

    def advanced_partitions(previous, current):
        return [
            partition
            for partition in partitions
            if current[partition]["end_sequence"] > previous[partition]["end_sequence"]
        ]

    async def wait_for_advance(client, previous):
        while True:
            current = await read_manifests(client)
            advanced = advanced_partitions(previous, current)
            if advanced:
                return current, advanced, datetime.now(timezone.utc)
            # The manifest API has no notification for queued appends.
            await asyncio.sleep(0.05)

    async with (
        asyncio.timeout(22),
        _barrier_publish_api(TOKEN_PRINCIPAL_A) as owner,
        _barrier_publish_api(TOKEN_BARRIER) as publisher,
    ):
        before = await read_manifests(owner)
        assert before == [
            {
                "topic": topic,
                "partition": partition,
                "start_sequence": 0,
                "end_sequence": 0,
                "start_time": None,
                "end_time": None,
                "segments": [],
            }
            for partition in partitions
        ]

        child_started = datetime.now(timezone.utc)
        control = await publisher.post(
            "/events",
            json={
                **event,
                "id": "11111111-0000-0000-0000-000000000171",
                "subject_tenant": child_tenant,
            },
        )
        assert (control.status_code, control.content) == (202, b"")
        after_child, child_advanced, child_seen = await wait_for_advance(owner, before)
        assert len(child_advanced) == 1, child_advanced
        [child_partition] = child_advanced

        refused = await owner.post(
            "/events", json={**event, "id": "11111111-0000-0000-0000-000000000172"},
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            403, "application/problem+json",
        )
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/events",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }
        after_refused = await read_manifests(owner)
        assert after_refused == after_child

        # The stored response lets the next read cover earlier appends on the TX key.
        fence_started = datetime.now(timezone.utc)
        fence = await publisher.post(
            "/events",
            json={**event, "id": "11111111-0000-0000-0000-000000000173"},
            headers={"Prefer": "wait=10"},
            timeout=12,
        )
        assert (fence.status_code, fence.content, fence.headers.get("preference-applied")) == (
            201, b"", "wait=10",
        )
        fenced = await read_manifests(owner)
        fence_seen = datetime.now(timezone.utc)
        fence_advanced = advanced_partitions(after_child, fenced)
        assert len(fence_advanced) == 1, fence_advanced
        [fence_partition] = fence_advanced

        barrier_started = datetime.now(timezone.utc)
        allowed = await publisher.post(
            "/events", json={**event, "id": "11111111-0000-0000-0000-000000000107"},
        )
        assert (allowed.status_code, allowed.content) == (202, b"")
        after, barrier_advanced, barrier_seen = await wait_for_advance(owner, fenced)
        assert barrier_advanced == [fence_partition]

        event_counts = {}
        for previous, current, partition, started, seen in (
            (before, after_child, child_partition, child_started, child_seen),
            (after_child, fenced, fence_partition, fence_started, fence_seen),
            (fenced, after, fence_partition, barrier_started, barrier_seen),
        ):
            prior = previous[partition]
            manifest = current[partition]
            end_sequence = manifest["end_sequence"]
            assert end_sequence > prior["end_sequence"]
            end_time = manifest["end_time"]
            assert isinstance(end_time, str)
            assert re.fullmatch(utc_timestamp, end_time)
            assert started <= datetime.fromisoformat(end_time) <= seen
            if prior["end_sequence"] == 0:
                start_sequence, start_time = end_sequence, end_time
            else:
                start_sequence, start_time = prior["start_sequence"], prior["start_time"]

            event_counts[partition] = event_counts.get(partition, 0) + 1
            segments = manifest["segments"]
            assert segments
            if BACKEND in ("memory", "sqlite"):
                assert len(segments) == 1
            if BACKEND == "memory":
                expected_segments = [{
                    "start_sequence": start_sequence,
                    "end_sequence": end_sequence,
                    "event_count": event_counts[partition],
                }]
            elif BACKEND == "sqlite":
                expected_segments = [{"from_sequence": start_sequence, "to_sequence": end_sequence}]
            else:
                expected_segments = segments

            expected = previous.copy()
            expected[partition] = {
                "topic": topic,
                "partition": partition,
                "start_sequence": start_sequence,
                "end_sequence": end_sequence,
                "start_time": start_time,
                "end_time": end_time,
                "segments": expected_segments,
            }
            assert current == expected


async def _join_refused(api, interest: dict) -> tuple[httpx.Response, httpx.Response]:
    """JOINs a fresh group with one interest; returns the JOIN response and the
    group's subscription listing read after it."""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            },
        )
        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    return sub_resp, list_resp


async def test_join_tenant_outside_subtree(api):
    """scenario: auth/1.08-negative-join-tenant-outside-subtree.md

    The caller has topic consume, but the sibling tenant is outside its reach.
    """
    async with api(TOKEN_PRINCIPAL_A) as owner:
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

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": _OTHER_TENANT,
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
            "types": [EVENT_TYPE_STREAM],
        }
        refused = await owner.post(
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

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
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


async def test_join_barrier_ignore_without_grant(_action_grants_api):
    """scenario: auth/1.09-negative-join-barrier-ignore-without-grant.md

    The caller has topic consume, but lacks the right to ignore a barrier.
    """
    async with _action_grants_api(TOKEN_PRINCIPAL_A) as owner:
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

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": _BARRIER_TENANT,
            "ownership_filter": "entire_tenant",
            "barrier_mode": "ignore",
            "types": [EVENT_TYPE_STREAM],
        }
        refused = await owner.post(
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

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
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


async def test_join_event_type_excluded_by_scope(_scoped_api):
    """scenario: auth/1.10-negative-join-event-type-excluded-by-scope.md"""
    tenant = "00000000-0000-0000-0000-000000000004"
    principal = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
    interest = {
        "topic": TOPIC_SEL_EXACT,
        "subject_tenant": tenant,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SEL_EXACT],
    }
    allowed_interest = {**interest, "types": [EVENT_TYPE_SEL_EXACT_SIBLING]}
    page_info = {"next_cursor": None, "prev_cursor": None, "limit": 25}

    async with _scoped_api(TOKEN_PRINCIPAL_B) as caller, _scoped_api() as observer:
        groups = []
        for _ in range(2):
            created = await caller.post("/consumer-groups")
            assert (created.status_code, created.headers["content-type"]) == (
                201, "application/json",
            )
            location = created.headers["location"]
            group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
            prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(prefix)
            issued = group_id.removeprefix(prefix)
            assert str(uuid.UUID(issued)) == issued
            assert location == f"{API_BASE}/consumer-groups/{group_id}"
            body = created.json()
            assert isinstance(body, dict)
            group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": principal,
                "description": None,
                "created_at": _subscription_created_at(body),
            }
            assert body == group
            groups.append(group)

        target, control = groups
        assert target["id"] != control["id"]
        group_path = f"/consumer-groups/{target['id']}"
        group_filter = {"$filter": f"consumer_group eq '{target['id']}'"}

        before = await caller.get(group_path)
        assert (before.status_code, before.headers["content-type"]) == (200, "application/json")
        assert before.json() == target
        for reader in (caller, observer):
            listed = await reader.get("/subscriptions", params=group_filter)
            assert (listed.status_code, listed.headers["content-type"]) == (200, "application/json")
            page = listed.json()
            assert page == {"items": [], "page_info": page_info}
            assert type(page["page_info"]["limit"]) is int

        denied = await caller.post(
            "/subscriptions",
            json={
                "consumer_group": target["id"],
                "client_agent": "worker/1.0.0",
                "interests": [interest],
            },
        )
        assert (denied.request.method, denied.request.url.path) == (
            "POST", f"{API_BASE}/subscriptions",
        )
        assert (denied.status_code, denied.headers["content-type"]) == (
            403, "application/problem+json",
        )
        problem = denied.json()
        assert problem == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/subscriptions",
            "trace_id": TRACE_ID,
            "context": {
                "reason": f"calling principal lacks consume on event type '{EVENT_TYPE_SEL_EXACT}'",
                "resource_type": "gts.cf.core.events.event_type.v1~",
            },
        }
        assert type(problem["status"]) is int

        after = await caller.get(group_path)
        assert (after.status_code, after.headers["content-type"]) == (200, "application/json")
        assert after.json() == target
        for reader in (caller, observer):
            listed = await reader.get("/subscriptions", params=group_filter)
            assert (listed.status_code, listed.headers["content-type"]) == (200, "application/json")
            page = listed.json()
            assert page == {"items": [], "page_info": page_info}
            assert type(page["page_info"]["limit"]) is int

        admitted = await caller.post(
            "/subscriptions",
            json={
                "consumer_group": control["id"],
                "client_agent": "worker/1.0.0",
                "interests": [allowed_interest],
            },
        )
        assert (admitted.status_code, admitted.headers["content-type"]) == (201, "application/json")
        subscription_id = _created_subscription_id(admitted)
        body = admitted.json()
        assert isinstance(body, dict)
        returned_interest = {**allowed_interest, "barrier_mode": "respect", "filter": None}
        member = {
            "id": subscription_id,
            "consumer_group": control["id"],
            "client_agent": "worker/1.0.0",
            "interests": [returned_interest],
            "assigned": [{"topic": TOPIC_SEL_EXACT, "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(body),
        }
        assert body == {
            **member,
            "interests": [{**returned_interest, "unmatched_types": []}],
        }
        assert type(body["topology_version"]) is int
        assert type(body["assigned"][0]["partition"]) is int

        read_control = await caller.get(f"/consumer-groups/{control['id']}")
        assert (read_control.status_code, read_control.headers["content-type"]) == (
            200, "application/json",
        )
        assert read_control.json() == control
        listed = await caller.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{control['id']}'"},
        )
        assert (listed.status_code, listed.headers["content-type"]) == (200, "application/json")
        page = listed.json()
        assert page == {"items": [member], "page_info": page_info}
        assert type(page["page_info"]["limit"]) is int
        assert type(page["items"][0]["topology_version"]) is int
        assert type(page["items"][0]["assigned"][0]["partition"]) is int


async def test_management_by_a_nil_caller_is_refused(api):
    """scenario: auth/1.11-negative-nil-caller-management.md"""
    group_type = "gts.cf.core.events.consumer_group.v1~"
    subscription_type = "gts.cf.core.events.subscription.v1~"
    missing_subscription = "99999999-8888-7777-6666-555555555555"
    missing_group = f"{group_type}{missing_subscription}"
    interest = {
        "topic": TOPIC_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_STREAM],
    }
    group_snapshots = []
    async with api(TOKEN_PRINCIPAL_A) as owner, api(TOKEN_NIL_SUBJECT) as caller:
        groups = []
        group_location_prefix = f"{API_BASE}/consumer-groups/"
        for description in ("Group with a member", "Empty group"):
            created = await owner.post(
                "/consumer-groups", json={"client_agent": "e2e-test", "description": description},
            )
            assert (created.status_code, created.headers["content-type"]) == (
                201, "application/json",
            )
            location = created.headers["location"]
            group_uuid = uuid.UUID(location.removeprefix(f"{group_location_prefix}{group_type}"))
            group_id = f"{group_type}{group_uuid}"
            assert location == f"{group_location_prefix}{group_id}"
            body = created.json()
            assert isinstance(body, dict)
            group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": description,
                "created_at": _subscription_created_at(body),
            }
            assert body == group
            groups.append(group)
        occupied, empty = groups
        group_id, empty_id = occupied["id"], empty["id"]
        assert group_id != empty_id
        denied_create = {
            "client_agent": "e2e-test",
            "description": f"Refused create {group_id.removeprefix(group_type)}",
        }
        join = {"consumer_group": group_id, "client_agent": "e2e-test", "interests": [interest]}
        joined = await owner.post("/subscriptions", json=join)
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        subscription_id = _created_subscription_id(joined)
        body = joined.json()
        assert isinstance(body, dict)
        stored_interest = {**interest, "barrier_mode": "respect", "filter": None}
        member = {
            "id": subscription_id,
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [stored_interest],
            "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(body),
        }
        assert body == {**member, "interests": [{**stored_interest, "unmatched_types": []}]}
        assert type(body["topology_version"]) is int
        assert type(body["assigned"][0]["partition"]) is int

        seek = {
            "topology_version": 1,
            "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]},
        }
        subscriptions = (subscription_id, missing_subscription)
        multipart = "multipart/mixed"
        subscription_path = f"/subscriptions/{subscription_id}"
        for phase in ("before", "after"):
            for group in groups:
                read = await owner.get(f"/consumer-groups/{group['id']}")
                assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
                assert read.json() == group
            read = await owner.get(subscription_path)
            assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
            body = read.json()
            assert body == member
            assert type(body["topology_version"]) is int
            assert type(body["assigned"][0]["partition"]) is int

            for path, resource_type, resource_name, detail in (
                (
                    f"/consumer-groups/{missing_group}", group_type, missing_group,
                    f"consumer group '{missing_group}' is not registered",
                ),
                (
                    f"/subscriptions/{missing_subscription}", subscription_type,
                    missing_subscription, f"subscription '{missing_subscription}' does not exist",
                ),
            ):
                read = await owner.get(path)
                assert (read.status_code, read.headers["content-type"]) == (
                    404, "application/problem+json",
                )
                assert read.json() == {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                    "title": "Not Found",
                    "status": 404,
                    "detail": detail,
                    "instance": f"{API_BASE}{path}",
                    "trace_id": TRACE_ID,
                    "context": {"resource_type": resource_type, "resource_name": resource_name},
                }

            # A create can mint a fresh ID anywhere in the owner's paged group list.
            pages = []
            visible_ids = []
            cursor = None
            seen_cursors = set()
            while True:
                params = {} if cursor is None else {"cursor": cursor}
                listed = await owner.get("/consumer-groups", params=params)
                assert (listed.status_code, listed.headers["content-type"]) == (
                    200, "application/json",
                )
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
                    assert isinstance(previous, str)
                    assert previous
                    assert items
                for item in items:
                    assert isinstance(item, dict)
                    assert set(item) == set(occupied)
                    assert isinstance(item["id"], str)
                    visible_ids.append(item["id"])
                pages.append(page)
                cursor = page_info["next_cursor"]
                if cursor is None:
                    break
                assert isinstance(cursor, str)
                assert cursor
                assert len(items) == 25
                assert cursor not in seen_cursors
                seen_cursors.add(cursor)
            assert visible_ids == sorted(set(visible_ids))
            visible = {item["id"]: item for page in pages for item in page["items"]}
            assert [visible.get(g["id"]) for g in groups] == groups
            group_snapshots.append(pages)

            for target_group, expected_items in ((group_id, [member]), (empty_id, [])):
                listed = await owner.get(
                    "/subscriptions", params={"$filter": f"consumer_group eq '{target_group}'"},
                )
                assert (listed.status_code, listed.headers["content-type"]) == (
                    200, "application/json",
                )
                page = listed.json()
                assert page == {
                    "items": expected_items,
                    "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
                }
                assert type(page["page_info"]["limit"]) is int
                for item in page["items"]:
                    assert type(item["topology_version"]) is int
                    assert type(item["assigned"][0]["partition"]) is int

            # Subscription metadata omits the positions; opening the stream checks them.
            async with owner.stream(
                "GET",
                "/events:stream",
                params={"subscription": subscription_id},
                headers={"Accept": multipart},
            ) as unseeked:
                assert (unseeked.status_code, unseeked.headers["content-type"]) == (
                    409, "application/problem+json",
                )
                await unseeked.aread()
                assert unseeked.json() == {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
                    "title": "Failed Precondition",
                    "status": 409,
                    "detail": "Operation precondition not met",
                    "instance": f"{API_BASE}/events:stream",
                    "trace_id": TRACE_ID,
                    "context": {
                        "resource_type": subscription_type,
                        "violations": [{
                            "type": "positions_not_set",
                            "subject": f"{TOPIC_STREAM}:0",
                            "description": (
                                f"call POST /v1{subscription_path}:seek "
                                "before re-opening the stream"
                            ),
                        }],
                    },
                }

            if phase == "after":
                break

            denials = []
            for method, route, ids, payload, accept in (
                ("GET", "/subscriptions", (None,), None, None),
                ("GET", "/consumer-groups", (None,), None, None),
                ("POST", "/consumer-groups", (None,), denied_create, None),
                ("GET", "/consumer-groups/{id}", (group_id, missing_group), None, None),
                ("GET", "/subscriptions/{id}", subscriptions, None, None),
                ("DELETE", "/subscriptions/{id}", subscriptions, None, None),
                ("POST", "/subscriptions/{id}:seek", subscriptions, seek, None),
                ("GET", "/events:stream", subscriptions, None, multipart),
                ("GET", "/events:sse", subscriptions, None, "text/event-stream"),
                ("DELETE", "/consumer-groups/{id}", (empty_id, missing_group), None, None),
                ("POST", "/subscriptions", (None,), {**join, "consumer_group": empty_id}, None),
            ):
                for resource_id in ids:
                    path = route.format(id=resource_id)
                    instance = f"{API_BASE}{path}"
                    params = {"subscription": resource_id} if accept else None
                    headers = {"Accept": accept} if accept else None
                    request = caller.build_request(
                        method, path, params=params, headers=headers, json=payload,
                    )
                    row = (method, instance, request.url.query)
                    assert request.method == method
                    assert request.url.path == instance
                    assert request.url.query == (
                        f"subscription={resource_id}".encode("ascii") if accept else b""
                    )
                    assert request.headers["accept"] == (accept or "*/*")
                    assert request.headers["traceparent"] == (
                        f"00-{TRACE_ID}-b7ad6b7169203331-01"
                    )
                    if payload is None:
                        assert request.content == b""
                        assert "content-type" not in request.headers
                    else:
                        assert request.headers["content-type"] == "application/json"
                    expected = {
                        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
                        "title": "Permission Denied",
                        "status": 403,
                        "detail": "You do not have permission to perform this operation",
                        "instance": instance,
                        "trace_id": TRACE_ID,
                        "context": {
                            "reason": (
                                "the caller has no identity: its subject or tenant is the nil UUID"
                            ),
                            "resource_type": "gts.cf.core.events.request.v1~",
                        },
                    }
                    async with aclosing(await caller.send(request, stream=True)) as refused:
                        assert (refused.status_code, refused.headers["content-type"]) == (
                            403, "application/problem+json",
                        ), row
                        await refused.aread()
                        denials.append((row, refused.json(), expected))
            for row, problem, expected in denials:
                assert problem == expected, row
                assert type(problem["status"]) is int, row

    before_groups, after_groups = group_snapshots
    assert before_groups == after_groups
    assert all(
        item["description"] != denied_create["description"]
        for page in after_groups
        for item in page["items"]
    )


def _refused_by_policy(path: str, resource_type: str) -> dict:
    """The broker's 403 for a request the policy refuses, at ``path``."""
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}{path}",
        "trace_id": TRACE_ID,
        "context": {
            "reason": "access denied by policy",
            "resource_type": resource_type,
        },
    }


async def test_segments_for_a_topic_the_caller_may_not_read_return_403_before_404(api):
    """scenario: auth/1.12-negative-segments-topic-not-authorized.md"""
    path = "/topics/segments"
    registered = {"topic": TOPIC_STREAM, "partition": 0}
    unregistered = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.unregistered.v1"
    async with api(TOKEN_PRINCIPAL_A) as owner, api(TOKEN_NIL_TENANT) as caller:
        before = await owner.get(path, params=registered)
        for topic in (TOPIC_STREAM, unregistered):
            resp = await caller.get(path, params={"topic": topic, "partition": 0})
            assert resp.status_code == 403
            assert resp.headers["content-type"] == "application/problem+json"
            assert resp.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
                "title": "Permission Denied",
                "status": 403,
                "detail": "You do not have permission to perform this operation",
                "instance": "/event-broker/v1/topics/segments",
                "trace_id": TRACE_ID,
                "context": {
                    "reason": "access denied by policy",
                    "resource_type": "gts.cf.core.events.topic.v1~",
                },
            }
        after = await owner.get(path, params=registered)

    manifests = []
    for resp in (before, after):
        assert resp.status_code == 200
        assert resp.headers["content-type"] == "application/json"
        manifest = resp.json()
        assert isinstance(manifest, dict)
        assert set(manifest) == {
            "topic", "partition", "start_sequence", "end_sequence", "start_time",
            "end_time", "segments",
        }
        for field in ("partition", "start_sequence", "end_sequence"):
            assert type(manifest[field]) is int
        first = manifest["start_sequence"]
        last = manifest["end_sequence"]
        if (first, last) == (0, 0):
            assert manifest == {
                "topic": TOPIC_STREAM,
                "partition": 0,
                "start_sequence": 0,
                "end_sequence": 0,
                "start_time": None,
                "end_time": None,
                "segments": [],
            }
        else:
            start_time = manifest["start_time"]
            end_time = manifest["end_time"]
            for timestamp in (start_time, end_time):
                if timestamp is None:
                    assert BACKEND not in ("memory", "sqlite")
                else:
                    _stream_instant(timestamp)

            segments = manifest["segments"]
            assert isinstance(segments, list)
            assert all(isinstance(segment, dict) for segment in segments)
            expected_segments = segments
            if BACKEND in ("memory", "sqlite"):
                assert 0 < first <= last
                assert len(segments) == 1
                segment = segments[0]
                assert all(type(value) is int for value in segment.values()), segment
                if BACKEND == "memory":
                    count = segment.get("event_count")
                    assert type(count) is int
                    assert count > 0
                    expected_segments = [{
                        "start_sequence": first,
                        "end_sequence": last,
                        "event_count": count,
                    }]
                else:
                    expected_segments = [{"from_sequence": first, "to_sequence": last}]
            assert manifest == {
                "topic": TOPIC_STREAM,
                "partition": 0,
                "start_sequence": first,
                "end_sequence": last,
                "start_time": start_time,
                "end_time": end_time,
                "segments": expected_segments,
            }
        manifests.append(manifest)

    assert manifests[1] == manifests[0]


async def test_producer_registration_the_tenant_check_refuses_returns_403(api):
    """scenario: auth/1.13-negative-producer-registration-tenant-not-authorized.md

    Client-agent validation precedes the nil tenant's policy check.
    """
    valid = {"mode": "chained", "client_agent": "order-service/1.0"}
    async with api(TOKEN_NIL_TENANT) as caller:
        refused = await caller.post("/producers", json=valid)
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/producers",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        malformed = await caller.post(
            "/producers",
            json={"mode": "chained", "client_agent": "agent/"},
        )
        assert malformed.status_code == 400
        assert malformed.headers["content-type"] == "application/problem+json"
        assert malformed.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/producers",
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
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

    async with api(TOKEN_PRINCIPAL_A) as owner:
        created = await owner.post("/producers", json=valid)
    assert created.status_code == 201
    assert created.headers["content-type"] == "application/json"
    registration = created.json()
    assert isinstance(registration, dict)
    assert set(registration) == {"id", "mode", "client_agent"}
    producer_id = registration["id"]
    assert str(uuid.UUID(producer_id)) == producer_id
    assert registration == {
        "id": producer_id,
        "mode": "chained",
        "client_agent": "order-service/1.0",
    }
    assert created.headers["location"] == f"/event-broker/v1/producers/{producer_id}"
