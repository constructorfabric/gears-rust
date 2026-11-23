"""Subscription (JOIN/LEAVE) scenarios (consumer/subscriptions/1.01-1.15, 1.18-1.24).

Each test manages its own consumer group.  Tests that need multiple members
(rebalance tests 1.11, 1.12) use TOPIC_4P (4 partitions) so there are enough
partitions to distribute.  The at-capacity rejection (1.13) uses TOPIC_STREAM
(1 partition): with one member already holding the sole partition, a second
JOIN has nowhere to assign and is refused 429.
"""

from __future__ import annotations

import asyncio
import re
import uuid
from contextlib import AsyncExitStack
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from operator import itemgetter

import httpx
import pytest

from .conftest import (
    MultipartReader,
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    SKIP_LIMITS,
    EVENT_TYPE_4P,
    EVENT_TYPE_SEL_PREFIX_PATTERN,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_SUBJECT_SELECTOR,
    EVENT_TYPE_LONGPOLL,
    SUBJECT_TYPE,
    SUBJECT_TYPE_OTHER,
    TOPIC_4P,
    TOPIC_SEL_PREFIX,
    TOPIC_STREAM,
    TOPIC_SUBJECT_SELECTOR,
    TOPIC_LONGPOLL,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    SseFrameReader,
    TRACE_ID,
)

TOKEN_PRINCIPAL_C = "eb-e2e-principal-c"
_OTHER_TENANT = "00000000-0000-0000-0000-000000000003"


def _interest(topic: str, event_type: str, tenant_id: str) -> dict:
    return {"topic": topic, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [event_type]}


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _created_subscription_id(response: httpx.Response) -> str:
    location = response.headers["location"]
    subscription_id = location.removeprefix(f"{API_BASE}/subscriptions/")
    assert str(uuid.UUID(subscription_id)) == subscription_id
    assert location == f"{API_BASE}/subscriptions/{subscription_id}"
    return subscription_id


def _subscription_created_at(body: dict) -> str:
    created_at = body["created_at"]
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
        created_at,
    )
    datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    return created_at


async def test_cold_join_fresh_group_returns_201(api):
    """scenario: consumer/subscriptions/1.01-positive-cold-join-fresh-group.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID)],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
            },
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assert read_body == expected
    assert sub_resp.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": []}],
    }


async def test_join_unregistered_type_reported(api):
    """scenario: consumer/subscriptions/1.18-positive-join-unregistered-type-reported.md"""
    unregistered = "gts.cf.core.events.event.v1~cf.e2e.event_broker.never_registered.v1~"
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, unregistered, DEFAULT_TENANT_ID)],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [unregistered],
                "filter": None,
            },
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assert read_body == expected
    assert sub_resp.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": [unregistered]}],
    }


async def test_join_multi_topic_interests_returns_201(api):
    """scenario: consumer/subscriptions/1.02-positive-join-multi-topic-interests.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID),
                    _interest(TOPIC_LONGPOLL, EVENT_TYPE_LONGPOLL, DEFAULT_TENANT_ID),
                ],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
            },
            {
                "topic": TOPIC_LONGPOLL,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_LONGPOLL],
                "filter": None,
            },
        ],
        "assigned": [
            {"topic": TOPIC_LONGPOLL, "partition": 0},
            {"topic": TOPIC_STREAM, "partition": 0},
        ],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assignment_key = itemgetter("topic", "partition")
    join_body = sub_resp.json()
    assert {**read_body, "assigned": sorted(read_body["assigned"], key=assignment_key)} == expected
    assert {**join_body, "assigned": sorted(join_body["assigned"], key=assignment_key)} == {
        **expected,
        "interests": [
            {**interest, "unmatched_types": []} for interest in expected["interests"]
        ],
    }


async def test_join_with_typed_filter_returns_201(api):
    """scenario: consumer/subscriptions/1.03-positive-join-with-typed-filter.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": DEFAULT_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                        "filter": {
                            "engine": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
                            "expression": "event.data.total_cents > 100000",
                        },
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": {
                    "engine": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
                    "expression": "event.data.total_cents > 100000",
                },
            },
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assert read_body == expected
    assert sub_resp.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": []}],
    }


async def test_parallelism_multiple_subscriptions(api, test_env):
    """scenario: consumer/subscriptions/1.04-positive-parallelism-multiple-subscriptions.md"""
    topic = TOPIC_4P
    event_type = EVENT_TYPE_4P
    partitions = list(range(4))
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        heads = {}
        for partition in partitions:
            manifest = await client.get(
                "/topics/segments", params={"topic": topic, "partition": partition}
            )
            assert (manifest.status_code, manifest.headers["content-type"]) == (
                200,
                "application/json",
            )
            snapshot = manifest.json()
            assert snapshot["topic"] == topic and snapshot["partition"] == partition
            assert set(snapshot) == {
                "topic",
                "partition",
                "start_sequence",
                "end_sequence",
                "start_time",
                "end_time",
                "segments",
            }
            assert isinstance(snapshot["segments"], list)
            head = snapshot["end_sequence"]
            assert type(head) is int and head >= 0
            heads[partition] = head
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_uuid = group_id.removeprefix("gts.cf.core.events.consumer_group.v1~")
        assert group_id == f"gts.cf.core.events.consumer_group.v1~{uuid.UUID(group_uuid)}"
        read = await client.get(f"/consumer-groups/{group_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == created.json() == group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        sub_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{sub_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == member
        assert joined.json() == {
            **member,
            "interests": [{**member["interests"][0], "unmatched_types": []}],
        }
        requested = [{"partition": p, "value": "latest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": heads[p]} for p in partitions]},
        }
        reader = await stack.enter_async_context(_member_stream(api, sub_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-b",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        peer_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{peer_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        assert peer_id != sub_id
        kept, gained = partitions[:2], partitions[2:]
        if uuid.UUID(sub_id) > uuid.UUID(peer_id):
            kept, gained = gained, kept
        peer = {
            "id": peer_id,
            "consumer_group": group_id,
            "client_agent": "consumer-b",
            "assigned": [{"topic": topic, "partition": p} for p in gained],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 2,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == peer
        assert joined.json() == {
            **peer,
            "interests": [{**peer["interests"][0], "unmatched_types": []}],
        }
        current = await client.get(f"/subscriptions/{sub_id}")
        assert (current.status_code, current.headers["content-type"]) == (200, "application/json")
        assert current.json() == {
            **member,
            "topology_version": 2,
            "assigned": [{"topic": topic, "partition": p} for p in kept],
        }
        first_parts = {a["partition"] for a in current.json()["assigned"]}
        second_parts = {a["partition"] for a in peer["assigned"]}
        assert first_parts.isdisjoint(second_parts)
        assert first_parts | second_parts == set(partitions)
        pushed = await reader.await_kind("topology", timeout=10)
        assert pushed == {
            "kind": "topology",
            "topology_version": 2,
            "assigned": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in kept
            ],
        }


async def test_leave_subscription_returns_204(api):
    """scenario: consumer/subscriptions/1.05-positive-leave-subscription.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID)],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
        del_resp = await client.delete(f"/subscriptions/{sub_id}")
        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
            },
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assert read_body == expected
    assert sub_resp.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": []}],
    }
    assert del_resp.status_code == 204
    assert del_resp.content == b""
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_unauthorized_topic_returns_403(_action_grants_api):
    """scenario: consumer/subscriptions/1.06-negative-join-unauthorized-topic.md"""
    tenant_id = DEFAULT_TENANT_ID
    caller_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
    interest = {
        "topic": TOPIC_SEL_PREFIX,
        "subject_tenant": tenant_id,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SEL_PREFIX_PATTERN],
    }
    async with (
        _action_grants_api(TOKEN_PRINCIPAL_B) as caller,
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

        before_group = await caller.get(group_path)
        assert (before_group.status_code, before_group.headers["content-type"]) == (
            200, "application/json",
        )
        assert before_group.json() == target_group

        joined = await caller.post(
            "/subscriptions",
            json={"consumer_group": group_id, "client_agent": "worker/1.0.0", "interests": []},
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        member_path = f"/subscriptions/{_created_subscription_id(joined)}"
        joined_body = joined.json()
        assert isinstance(joined_body, dict)
        assert isinstance(joined_body.get("created_at"), str)
        expected_member = {
            "id": member_path.removeprefix("/subscriptions/"),
            "consumer_group": group_id,
            "client_agent": "worker/1.0.0",
            "interests": [],
            "assigned": [],
            "topology_version": 1,
            "created_at": _subscription_created_at(joined_body),
        }
        assert joined_body == expected_member
        assert type(joined_body["topology_version"]) is int

        before_member = await caller.get(member_path)
        assert (before_member.status_code, before_member.headers["content-type"]) == (
            200, "application/json",
        )
        before_body = before_member.json()
        assert before_body == expected_member
        assert type(before_body["topology_version"]) is int

        before_members = await caller.get("/subscriptions", params=group_filter)
        assert (before_members.status_code, before_members.headers["content-type"]) == (
            200, "application/json",
        )
        before_page = before_members.json()
        assert before_page == {"items": [expected_member], "page_info": page_info}
        assert type(before_page["page_info"]["limit"]) is int
        assert type(before_page["items"][0]["topology_version"]) is int

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
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        }
        assert type(problem["status"]) is int

        after_member = await caller.get(member_path)
        assert (after_member.status_code, after_member.headers["content-type"]) == (
            200, "application/json",
        )
        after_body = after_member.json()
        assert after_body == expected_member
        assert type(after_body["topology_version"]) is int

        after_members = await caller.get("/subscriptions", params=group_filter)
        assert (after_members.status_code, after_members.headers["content-type"]) == (
            200, "application/json",
        )
        after_page = after_members.json()
        assert after_page == {"items": [expected_member], "page_info": page_info}
        assert type(after_page["page_info"]["limit"]) is int
        assert type(after_page["items"][0]["topology_version"]) is int

        after_group = await caller.get(group_path)
        assert (after_group.status_code, after_group.headers["content-type"]) == (
            200, "application/json",
        )
        assert after_group.json() == target_group


async def test_too_many_interests_returns_400(api):
    """scenario: consumer/subscriptions/1.07-negative-join-too-many-interests.md

    More than 64 interests in one JOIN is rejected 400.
    """
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID)
                    for _ in range(65)
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.headers["content-type"] == "application/problem+json"
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "TooManyInterests: interests count 65 exceeds the maximum of 64",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "TooManyInterests: interests count 65 exceeds the maximum of 64",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_with_a_nil_interest_tenant_returns_400(api):
    """scenario: consumer/subscriptions/1.20-negative-join-nil-interest-tenant.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(
                        TOPIC_STREAM,
                        EVENT_TYPE_STREAM,
                        "00000000-0000-0000-0000-000000000000",
                    )
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.headers["content-type"] == "application/problem+json"
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidTenant: interest 0: the interest's tenant must not be the nil UUID",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidTenant: interest 0: the interest's tenant must not be the nil UUID",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_with_too_many_types_returns_400(api):
    """scenario: consumer/subscriptions/1.21-negative-join-too-many-types.md"""
    too_many_types = [
        f"gts.cf.core.events.event.v1~x.e2e.toomany.t{i}.v1~" for i in range(33)
    ]
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID),
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": DEFAULT_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": too_many_types,
                    },
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.headers["content-type"] == "application/problem+json"
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "TooManyTypes: interest 1: types count 33 exceeds the maximum of 32",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "TooManyTypes: interest 1: types count 33 exceeds the maximum of 32",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_with_too_many_subject_types_returns_400(api):
    """scenario: consumer/subscriptions/1.22-negative-join-too-many-subject-types.md"""
    too_many_subject_types = [
        f"gts.cf.e2e.event_broker.toomany_s{i}.v1~" for i in range(33)
    ]
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID),
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": DEFAULT_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                        "subject_types": too_many_subject_types,
                    },
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.headers["content-type"] == "application/problem+json"
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "TooManySubjectTypes: interest 1: subject types count 33 exceeds the maximum of 32",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "TooManySubjectTypes: interest 1: subject types count 33 exceeds the maximum of 32",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_only_mine_default(api):
    """scenario: consumer/subscriptions/1.14-positive-join-only-mine.md"""
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

        interest = {
            "topic": TOPIC_STREAM,
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
            "ownership_filter": "only_mine",
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


async def test_join_only_mine_foreign_tenant(api):
    """scenario: consumer/subscriptions/1.19-negative-join-only-mine-foreign-tenant.md

    The default ownership filter rejects a tenant other than the caller's own.
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
        assert refused.status_code == 400
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidTenant: interest 0: only_mine reads the caller's own tenant",
            "instance": f"{API_BASE}/subscriptions",
            "trace_id": TRACE_ID,
            "context": {
                "format": "InvalidTenant: interest 0: only_mine reads the caller's own tenant",
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
            "ownership_filter": "only_mine",
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


async def test_join_max_depth_rejected(api):
    """scenario: consumer/subscriptions/1.15-negative-join-max-depth-rejected.md

    ``max_depth`` is not an interest field: a JOIN that sends it is refused
    400, naming the field and its replacement, and no subscription is
    created.
    """
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": DEFAULT_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "max_depth": 0,
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.headers["content-type"] == "application/problem+json"
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidBody: 'max_depth' is not supported; use 'ownership_filter'",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidBody: 'max_depth' is not supported; use 'ownership_filter'",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_subject_type_not_authorized(_scoped_api):
    """scenario: consumer/subscriptions/1.24-negative-join-subject-type-not-authorized.md"""
    tenant = "00000000-0000-0000-0000-000000000004"
    principal = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
    interest = {
        "topic": TOPIC_SUBJECT_SELECTOR,
        "subject_tenant": tenant,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SUBJECT_SELECTOR],
        "subject_types": [SUBJECT_TYPE],
    }
    allowed_interest = {**interest, "subject_types": [SUBJECT_TYPE_OTHER]}
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
                "reason": f"calling principal lacks consume on subject type '{SUBJECT_TYPE}'",
                "resource_type": "gts.cf.core.events.request.v1~",
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
            "assigned": [{"topic": TOPIC_SUBJECT_SELECTOR, "partition": 0}],
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


async def test_leave_unknown_subscription_returns_404(api):
    """scenario: consumer/subscriptions/1.08-negative-leave-unknown-subscription.md"""
    sub_id = "99999999-8888-7777-6666-555555555555"
    async with api() as client:
        resp = await client.delete(f"/subscriptions/{sub_id}")
    assert resp.status_code == 404
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": f"subscription '{sub_id}' does not exist",
        "instance": f"{API_BASE}/subscriptions/{sub_id}",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "resource_name": sub_id,
        },
    }


async def test_list_subscriptions_returns_paged_list(api):
    """scenario: consumer/subscriptions/1.09-positive-list-subscriptions.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID)],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
            },
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assert read_body == expected
    assert sub_resp.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": []}],
    }
    assert list_resp.status_code == 200
    assert list_resp.headers["content-type"] == "application/json"
    assert list_resp.json() == {
        "items": [expected],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_read_subscription_returns_200(api):
    """scenario: consumer/subscriptions/1.10-positive-read-subscription.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, DEFAULT_TENANT_ID)],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        assert sub_resp.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(sub_resp)
        read_resp = await client.get(f"/subscriptions/{sub_id}")
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
            },
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read_body),
    }
    assert read_body == expected
    assert sub_resp.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": []}],
    }


async def test_second_join_triggers_rebalance(api):
    """scenario: consumer/subscriptions/1.11-positive-second-join-triggers-rebalance.md

    With TOPIC_4P (4 partitions) and one existing member holding all 4, a second
    JOIN causes the broker to rebalance: each member should receive 2 partitions.
    """
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub1_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-1",
                "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, DEFAULT_TENANT_ID)],
            },
        )
        assert sub1_resp.status_code == 201
        assert sub1_resp.headers["content-type"] == "application/json"
        first_id = _created_subscription_id(sub1_resp)

        sub2_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-2",
                "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, DEFAULT_TENANT_ID)],
            },
        )
        assert sub2_resp.status_code == 201
        assert sub2_resp.headers["content-type"] == "application/json"
        second_id = _created_subscription_id(sub2_resp)

        sub1_after = await client.get(f"/subscriptions/{first_id}")
        sub2_after = await client.get(f"/subscriptions/{second_id}")
    assert sub1_after.status_code == 200
    assert sub2_after.status_code == 200
    assert sub1_after.headers["content-type"] == "application/json"
    assert sub2_after.headers["content-type"] == "application/json"
    first_read = sub1_after.json()
    second_read = sub2_after.json()

    interest = {
        "topic": TOPIC_4P,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "barrier_mode": "respect",
        "types": [EVENT_TYPE_4P],
        "filter": None,
    }
    join_interest = {**interest, "unmatched_types": []}
    partitions = [{"topic": TOPIC_4P, "partition": partition} for partition in range(4)]
    assert first_id != second_id
    first_partitions, second_partitions = partitions[:2], partitions[2:]
    if uuid.UUID(first_id) > uuid.UUID(second_id):
        first_partitions, second_partitions = second_partitions, first_partitions
    first_expected = {
        "id": first_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test-1",
        "interests": [interest],
        "assigned": first_partitions,
        "topology_version": 2,
        "created_at": _subscription_created_at(first_read),
    }
    second_expected = {
        "id": second_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test-2",
        "interests": [interest],
        "assigned": second_partitions,
        "topology_version": 2,
        "created_at": _subscription_created_at(second_read),
    }
    assert sub1_resp.json() == {
        **first_expected,
        "interests": [join_interest],
        "assigned": partitions,
        "topology_version": 1,
    }
    assert first_read == first_expected
    assert sub2_resp.json() == {**second_expected, "interests": [join_interest]}
    assert second_read == second_expected


async def test_third_join_triggers_rebalance(api):
    """scenario: consumer/subscriptions/1.12-positive-third-join-triggers-rebalance.md"""
    topic = TOPIC_4P
    partitions = [0, 1, 2, 3]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_4P],
    }
    json_ok = (200, "application/json")
    loop = asyncio.get_running_loop()
    deadline = loop.time() + 9

    async def next_frame(reader):
        remaining = deadline - loop.time()
        assert remaining > 0, "third-JOIN stream deadline expired"
        frame = await reader.next_frame(timeout=remaining)
        if frame[0] == "heartbeat":
            body = frame[1]
            assert isinstance(body, dict) and set(body) == {"kind", "at"}, frame
            assert isinstance(body["at"], str), frame
            assert body == {
                "kind": "heartbeat",
                "at": _subscription_created_at({"created_at": body["at"]}),
            }
        return frame

    async def next_change(reader):
        while True:
            frame = await next_frame(reader)
            if frame[0] != "heartbeat":
                return frame

    async with api() as client, AsyncExitStack() as streams:
        heads = {}
        for partition in partitions:
            manifest = await client.get(
                "/topics/segments", params={"topic": topic, "partition": partition}
            )
            assert (manifest.status_code, manifest.headers.get("content-type")) == json_ok
            snapshot = manifest.json()
            assert isinstance(snapshot, dict)
            assert set(snapshot) == {
                "topic", "partition", "start_sequence", "end_sequence",
                "start_time", "end_time", "segments",
            }
            assert (snapshot["topic"], snapshot["partition"]) == (topic, partition)
            assert isinstance(snapshot["segments"], list)
            head = snapshot["end_sequence"]
            assert type(head) is int and head >= 0
            heads[partition] = head

        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers.get("content-type")) == (
            201, "application/json",
        )
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{group_prefix}{uuid.UUID(group_id.removeprefix(group_prefix))}"
        group_read = await client.get(f"/consumer-groups/{group_id}")
        assert (group_read.status_code, group_read.headers.get("content-type")) == json_ok
        assert created.json() == group_read.json() == {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(group_read.json()),
        }

        ids = {}
        readers = {}
        members = {}
        assigned = {}
        for version, label, ranges in (
            (1, "A", (partitions,)),
            (2, "B", ([0, 1], [2, 3])),
            (3, "C", ([0], [1], [2, 3])),
        ):
            joined = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": f"three-reader-{label}",
                    "session_timeout": "PT30S",
                    "interests": [interest],
                },
            )
            assert (joined.status_code, joined.headers.get("content-type")) == (
                201, "application/json",
            ), label
            member_id = _created_subscription_id(joined)
            assert member_id not in ids.values(), label
            ids[label] = member_id
            ranks = sorted(ids, key=lambda name: uuid.UUID(ids[name]))
            previous, assigned = assigned, dict(zip(ranks, ranges, strict=True))
            current = assigned[label]
            subscription = joined.json()
            assert isinstance(subscription, dict) and "created_at" in subscription, label
            members[label] = {
                "id": member_id,
                "consumer_group": group_id,
                "client_agent": f"three-reader-{label}",
                "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
                "assigned": [{"topic": topic, "partition": p} for p in current],
                "topology_version": version,
                "created_at": _subscription_created_at(subscription),
            }
            assert subscription == {
                **members[label],
                "interests": [{**members[label]["interests"][0], "unmatched_types": []}],
            }, label
            seek = await client.post(
                f"/subscriptions/{member_id}:seek",
                json={
                    "topology_version": version,
                    "positions": {topic: [{"partition": p, "value": "latest"} for p in current]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == json_ok, label
            assert seek.json() == {
                "positions": {topic: [{"partition": p, "value": heads[p]} for p in current]},
            }, label
            reader = await streams.enter_async_context(_member_stream(api, member_id, "sse"))
            readers[label] = reader
            assert await next_frame(reader) == ("topology", {
                "kind": "topology",
                "topology_version": version,
                "assigned": [
                    {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                    for p in current
                ],
            }), label
            if label == "B":
                assert await next_change(readers["A"]) == ("topology", {
                    "kind": "topology",
                    "topology_version": 2,
                    "assigned": [
                        {"topic": topic, "partition": p,
                         "offset": heads[p], "last_examined": heads[p]}
                        for p in assigned["A"]
                    ],
                }), "A after B JOIN"

        continuing = {}
        for label in ("A", "B"):
            reader = readers[label]
            frame = await next_change(reader)
            if set(assigned[label]) - set(previous[label]):
                assert frame == ("control", {
                    "kind": "control",
                    "code": "terminal",
                    "reason": "rebalanced",
                    "positions": [
                        {"topic": topic, "partition": p,
                         "offset": heads[p], "last_examined": heads[p]}
                        for p in previous[label]
                    ],
                }), label
                with pytest.raises(StopAsyncIteration):
                    await next_frame(reader)
            else:
                assert frame == ("topology", {
                    "kind": "topology",
                    "topology_version": 3,
                    "assigned": [
                        {"topic": topic, "partition": p,
                         "offset": heads[p], "last_examined": heads[p]}
                        for p in assigned[label]
                    ],
                }), label
                continuing[label] = reader

        for label, member_id in ids.items():
            own = await client.get(f"/subscriptions/{member_id}")
            assert (own.status_code, own.headers.get("content-type")) == json_ok, label
            assert own.json() == {
                **members[label],
                "topology_version": 3,
                "assigned": [{"topic": topic, "partition": p} for p in assigned[label]],
            }, label

        async with asyncio.TaskGroup() as tasks:
            liveness = {
                label: tasks.create_task(next_frame(reader))
                for label, reader in continuing.items()
            }
        for label, observation in liveness.items():
            assert observation.result()[0] == "heartbeat", label


@SKIP_LIMITS
async def test_join_group_at_capacity_returns_429(api):
    """scenario: consumer/subscriptions/1.13-negative-join-group-at-capacity.md

    TOPIC_STREAM has 1 partition.  One member joins first and holds the only
    partition.  A second JOIN has no partition to assign and is refused 429
    GroupAtCapacity.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub1_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-1",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
        assert sub1_resp.status_code == 201
        assert len(sub1_resp.json()["assigned"]) == 1

        sub2_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-2",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
    assert sub2_resp.status_code == 429
    body = sub2_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
        "title": "Resource Exhausted",
        "status": 429,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }
    assert "retry-after" in {k.lower() for k in sub2_resp.headers}


async def test_cross_tenant_join_anonymous_group_returns_403(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    A principal in another tenant fails ownership while targeting its own tenant.
    """
    async with (
        api(TOKEN_PRINCIPAL_A) as owner,
        api(TOKEN_PRINCIPAL_C) as caller,
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


async def test_leave_subscription_not_owned(api):
    """scenario: consumer/subscriptions/1.23-negative-leave-subscription-not-owned.md"""
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
        sub_path = f"/subscriptions/{sub_id}"
        joined_body = joined.json()
        assert isinstance(joined_body, dict)
        assert isinstance(joined_body.get("created_at"), str)
        member_interest = {**interest, "barrier_mode": "respect", "filter": None}
        expected_member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [member_interest],
            "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(joined_body),
        }
        assert joined_body == {
            **expected_member,
            "interests": [{**member_interest, "unmatched_types": []}],
        }
        assert type(joined_body["topology_version"]) is int
        assert type(joined_body["assigned"][0]["partition"]) is int

        owner_member = await owner.get(sub_path)
        assert owner_member.status_code == 200
        assert owner_member.headers["content-type"] == "application/json"
        owner_member_body = owner_member.json()
        assert owner_member_body == expected_member
        assert type(owner_member_body["topology_version"]) is int
        assert type(owner_member_body["assigned"][0]["partition"]) is int

        refused = await peer.delete(sub_path)
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}{sub_path}",
            "trace_id": TRACE_ID,
            "context": {
                "reason": f"subscription '{sub_id}' was joined by another principal",
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        }

        remaining = await owner.get(sub_path)
        assert remaining.status_code == 200
        assert remaining.headers["content-type"] == "application/json"
        remaining_body = remaining.json()
        assert remaining_body == expected_member
        assert type(remaining_body["topology_version"]) is int
        assert type(remaining_body["assigned"][0]["partition"]) is int

        group_filter = {"$filter": f"consumer_group eq '{group_id}'"}
        page_info = {"next_cursor": None, "prev_cursor": None, "limit": 25}
        members = await owner.get("/subscriptions", params=group_filter)
        assert members.status_code == 200
        assert members.headers["content-type"] == "application/json"
        members_body = members.json()
        assert members_body == {"items": [expected_member], "page_info": page_info}
        listed_member = members_body["items"][0]
        assert type(listed_member["topology_version"]) is int
        assert type(listed_member["assigned"][0]["partition"]) is int

        left = await owner.delete(sub_path)
        assert left.status_code == 204
        assert left.headers.get("content-type") is None
        assert left.content == b""

        members_after = await owner.get("/subscriptions", params=group_filter)
        assert members_after.status_code == 200
        assert members_after.headers["content-type"] == "application/json"
        assert members_after.json() == {"items": [], "page_info": page_info}


@asynccontextmanager
async def _member_stream(api, sub_id, transport):
    media = "text/event-stream" if transport == "sse" else "multipart/mixed"
    async with (
        api() as stream_client,
        stream_client.stream(
            "GET",
            f"/events:{transport}",
            params={"subscription": sub_id},
            headers={"Accept": media},
            timeout=None,
        ) as response,
    ):
        assert response.status_code == 200
        content_type = response.headers["content-type"]
        if transport == "sse":
            assert content_type == media
            yield SseFrameReader(response)
        else:
            assert re.fullmatch(r"multipart/mixed; boundary=eb-frame-[0-9a-f]{32}", content_type)
            yield MultipartReader(response)
