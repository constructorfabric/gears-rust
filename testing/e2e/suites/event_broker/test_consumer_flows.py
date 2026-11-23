"""Consumer-only end-to-end flow scenarios (consumer/flows/1.01-1.04).

These tests exercise multi-step journeys that don't fit neatly in a single
scenario domain:

- 1.01: Two-consumer rebalance - both consumers stream concurrently and each
  receives events from its assigned partitions.
- 1.02: PositionsNotSet recovery - SDK-style re-SEEK loop after a 409.
- 1.03: Path-A consumer with persistent offset store - reconnect from exact
  last-processed offset.
- 1.04: Leave triggers gain+terminate - leaving member's partition is gained
  by surviving member; the leaving member's stream terminates.
"""

from __future__ import annotations

import asyncio
import uuid
from contextlib import AsyncExitStack
from datetime import datetime, timezone

import pytest

from .conftest import (
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_4P,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    TOPIC_4P,
    TOPIC_STREAM,
    TRACE_ID,
)
from .test_publish_consume import _stream_instant
from .test_consumer_subscriptions import (
    _created_subscription_id,
    _member_stream,
    _subscription_created_at,
)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


@pytest.mark.timeout(5, func_only=True)
async def test_two_consumer_rebalance(api, test_env):
    """scenario: consumer/flows/1.01-flow-two-consumer-rebalance.md"""
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
        requested = [{"partition": p, "value": "latest"} for p in gained]
        seek = await client.post(
            f"/subscriptions/{peer_id}:seek",
            json={"topology_version": 2, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": heads[p]} for p in gained]},
        }
        peer_reader = await stack.enter_async_context(_member_stream(api, peer_id, "sse"))
        baseline = await peer_reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 2,
            "assigned": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in gained
            ],
        }
        assert baseline == ("topology", topology)


@pytest.mark.timeout(5, func_only=True)
async def test_positions_not_set_recovery(api):
    """scenario: consumer/flows/1.02-flow-positions-not-set-recovery.md"""
    topic = TOPIC_STREAM
    event_type = EVENT_TYPE_STREAM
    partitions = [0]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        event = {
            "id": "17e20000-0000-4000-8000-000000003001",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
        }
        marker = {**event, "id": "17e20000-0000-4000-8000-000000003000"}
        fence = await client.post("/events", json=marker, headers={"Prefer": "wait=10"}, timeout=12)
        assert (fence.status_code, fence.content, fence.headers.get("preference-applied")) == (
            201,
            b"",
            "wait=10",
        )
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
        for _ in range(2):
            async with client.stream(
                "GET",
                "/events:stream",
                params={"subscription": sub_id},
                headers={"Accept": "multipart/mixed"},
            ) as refused:
                assert refused.status_code == 409
                await refused.aread()
            assert (refused.status_code, refused.headers["content-type"]) == (
                409,
                "application/problem+json",
            )
            assert refused.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
                "title": "Failed Precondition",
                "status": 409,
                "detail": "Operation precondition not met",
                "instance": f"{API_BASE}/events:stream",
                "trace_id": TRACE_ID,
                "context": {
                    "resource_type": "gts.cf.core.events.subscription.v1~",
                    "violations": [
                        {
                            "type": "positions_not_set",
                            "subject": f"{topic}:{p}",
                            "description": (
                                f"call POST /v1/subscriptions/{sub_id}:seek "
                                "before re-opening the stream"
                            ),
                        }
                        for p in partitions
                    ],
                },
            }
        requested = [{"partition": p, "value": "earliest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in partitions]},
        }
        reader = await stack.enter_async_context(_member_stream(api, sub_id, "stream"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": 0, "last_examined": 0}
                for p in partitions
            ],
        }
        assert baseline == topology
        original_group = group_id
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
        assert group_id != original_group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "history-reader",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        witness_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{witness_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        witness = {
            "id": witness_id,
            "consumer_group": group_id,
            "client_agent": "history-reader",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == witness
        assert joined.json() == {
            **witness,
            "interests": [{**witness["interests"][0], "unmatched_types": []}],
        }
        requested = [{"partition": p, "value": "earliest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{witness_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in partitions]},
        }
        witness_reader = await stack.enter_async_context(_member_stream(api, witness_id, "sse"))
        baseline = await witness_reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": 0, "last_examined": 0}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        group_id = original_group
        deadline = asyncio.get_running_loop().time() + 5
        sequence = 0
        while sequence < heads[0]:
            remaining = deadline - asyncio.get_running_loop().time()
            assert remaining > 0
            stored = await witness_reader.await_kind("event", timeout=remaining)
            remaining = deadline - asyncio.get_running_loop().time()
            assert remaining > 0
            observed = await reader.await_kind("event", timeout=remaining)
            assert observed == stored
            assert set(stored) == {"kind", "payload"} and stored["kind"] == "event"
            payload = stored["payload"]
            assert payload["partition"] == 0 and type(payload["sequence"]) is int
            assert sequence < payload["sequence"] <= heads[0]
            sequence = payload["sequence"]
        assert sequence == heads[0] and sequence > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(marker["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert stored == {
            "kind": "event",
            "payload": {
                **marker,
                "occurred_at": payload["occurred_at"],
                "partition": 0,
                "sequence": sequence,
                "sequence_time": payload["sequence_time"],
                "trace_parent": None,
                "data": None,
            },
        }
        event["id"] = "17e20000-0000-4000-8000-000000003001"
        publish = await client.post("/events", json=event)
        assert (publish.status_code, publish.content) == (202, b"")
        deadline = asyncio.get_running_loop().time() + 5
        stored = await witness_reader.await_kind("event", timeout=5)
        remaining = deadline - asyncio.get_running_loop().time()
        assert remaining > 0
        delivered = await reader.await_kind("event", timeout=remaining)
        payload = stored["payload"]
        assert type(payload["sequence"]) is int and payload["sequence"] > sequence
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert (
            delivered
            == stored
            == {
                "kind": "event",
                "payload": {
                    **event,
                    "occurred_at": payload["occurred_at"],
                    "trace_parent": None,
                    "data": None,
                    "partition": 0,
                    "sequence": payload["sequence"],
                    "sequence_time": payload["sequence_time"],
                },
            }
        )
        sequence = delivered["payload"]["sequence"]


@pytest.mark.backend_sensitive
@pytest.mark.timeout(5, func_only=True)
async def test_path_a_consumer_reconnects_from_exact_offset(api, test_env):
    """scenario: consumer/flows/1.03-flow-path-a-consumer-with-db.md"""
    topic = TOPIC_STREAM
    event_type = EVENT_TYPE_STREAM
    partitions = [0]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        event = {
            "id": "17e20000-0000-4000-8000-000000003101",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
        }
        marker = {**event, "id": "17e20000-0000-4000-8000-000000003100"}
        fence = await client.post("/events", json=marker, headers={"Prefer": "wait=10"}, timeout=12)
        assert (fence.status_code, fence.content, fence.headers.get("preference-applied")) == (
            201,
            b"",
            "wait=10",
        )
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
        requested = [{"partition": p, "value": "earliest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in partitions]},
        }
        original_stream = await stack.enter_async_context(AsyncExitStack())
        reader = await original_stream.enter_async_context(_member_stream(api, sub_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": 0, "last_examined": 0}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        original_group = group_id
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
        assert group_id != original_group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "history-reader",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        witness_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{witness_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        witness = {
            "id": witness_id,
            "consumer_group": group_id,
            "client_agent": "history-reader",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == witness
        assert joined.json() == {
            **witness,
            "interests": [{**witness["interests"][0], "unmatched_types": []}],
        }
        requested = [{"partition": p, "value": "earliest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{witness_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in partitions]},
        }
        witness_reader = await stack.enter_async_context(_member_stream(api, witness_id, "sse"))
        baseline = await witness_reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": 0, "last_examined": 0}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        group_id = original_group
        deadline = asyncio.get_running_loop().time() + 5
        sequence = 0
        while sequence < heads[0]:
            remaining = deadline - asyncio.get_running_loop().time()
            assert remaining > 0
            stored = await witness_reader.await_kind("event", timeout=remaining)
            remaining = deadline - asyncio.get_running_loop().time()
            assert remaining > 0
            observed = await reader.await_kind("event", timeout=remaining)
            assert observed == stored
            assert set(stored) == {"kind", "payload"} and stored["kind"] == "event"
            payload = stored["payload"]
            assert payload["partition"] == 0 and type(payload["sequence"]) is int
            assert sequence < payload["sequence"] <= heads[0]
            sequence = payload["sequence"]
        assert sequence == heads[0] and sequence > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(marker["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert stored == {
            "kind": "event",
            "payload": {
                **marker,
                "occurred_at": payload["occurred_at"],
                "partition": 0,
                "sequence": sequence,
                "sequence_time": payload["sequence_time"],
                "trace_parent": None,
                "data": None,
            },
        }
        event["id"] = "17e20000-0000-4000-8000-000000003101"
        publish = await client.post("/events", json=event)
        assert (publish.status_code, publish.content) == (202, b"")
        deadline = asyncio.get_running_loop().time() + 5
        stored = await witness_reader.await_kind("event", timeout=5)
        remaining = deadline - asyncio.get_running_loop().time()
        assert remaining > 0
        delivered = await reader.await_kind("event", timeout=remaining)
        payload = stored["payload"]
        assert type(payload["sequence"]) is int and payload["sequence"] > sequence
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert (
            delivered
            == stored
            == {
                "kind": "event",
                "payload": {
                    **event,
                    "occurred_at": payload["occurred_at"],
                    "trace_parent": None,
                    "data": None,
                    "partition": 0,
                    "sequence": payload["sequence"],
                    "sequence_time": payload["sequence_time"],
                },
            }
        )
        sequence = delivered["payload"]["sequence"]
        persisted = sequence
        await original_stream.aclose()
        # A stale SEEK passes the lease check only after release, then fails before any write.
        loop = asyncio.get_running_loop()
        deadline = loop.time() + 1
        async with asyncio.timeout_at(deadline):
            while True:
                probe = await client.post(
                    f"/subscriptions/{sub_id}:seek",
                    json={
                        "topology_version": member["topology_version"] + 1,
                        "positions": {topic: [{"partition": 0, "value": "earliest"}]},
                    },
                    timeout=deadline - loop.time(),
                )
                assert probe.headers["content-type"] == "application/problem+json"
                assert probe.status_code in (409, 412)
                held = probe.status_code == 409
                reason, description = (
                    (
                        "streaming_in_progress",
                        "one stream per subscription; a second stream or a SEEK "
                        "is refused while it is open",
                    )
                    if held
                    else (
                        "topology_version_mismatch",
                        "subscription topology changed; re-read the subscription and re-seek",
                    )
                )
                assert probe.json() == {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
                    "title": "Failed Precondition",
                    "status": 409 if held else 412,
                    "detail": "Operation precondition not met",
                    "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
                    "trace_id": TRACE_ID,
                    "context": {
                        "resource_type": "gts.cf.core.events.subscription.v1~",
                        "violations": [
                            {"type": reason, "subject": sub_id, "description": description}
                        ],
                    },
                }
                if not held:
                    break
                await asyncio.sleep(0.05)
        event["id"] = "17e20000-0000-4000-8000-000000003102"
        publish = await client.post("/events", json=event)
        assert (publish.status_code, publish.content) == (202, b"")
        kind, stored = await witness_reader.next_frame(timeout=5)
        assert kind == "event"
        payload = stored["payload"]
        assert type(payload["sequence"]) is int and payload["sequence"] > sequence
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert stored == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": None,
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        replacement_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{replacement_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        replacement = {
            "id": replacement_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 2,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == replacement
        assert joined.json() == {
            **replacement,
            "interests": [{**replacement["interests"][0], "unmatched_types": []}],
        }
        assert replacement_id != sub_id
        absent = await client.get(f"/subscriptions/{sub_id}")
        assert (absent.status_code, absent.headers["content-type"]) == (
            404,
            "application/problem+json",
        )
        assert absent.json() == {
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
        requested = [{"partition": p, "value": persisted} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{replacement_id}:seek",
            json={
                "topology_version": replacement["topology_version"],
                "positions": {topic: requested},
            },
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": persisted} for p in partitions]},
        }
        reader = await stack.enter_async_context(_member_stream(api, replacement_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 2,
            "assigned": [
                {"topic": topic, "partition": p, "offset": persisted, "last_examined": persisted}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        assert await reader.next_frame(timeout=5) == ("event", stored)


@pytest.mark.parametrize("disconnect", [False, True], ids=["delete", "disconnect"])
@pytest.mark.timeout(20, func_only=True)
async def test_gain_and_terminate(api, test_env, disconnect):
    """scenario: consumer/flows/1.04-flow-leave-triggers-gain-terminate.md"""
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
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-b",
                "interests": [interest],
                **({"session_timeout": "PT2S"} if disconnect else {}),
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
        requested = [{"partition": p, "value": "latest"} for p in kept]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 2, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": heads[p]} for p in kept]},
        }
        requested = [{"partition": p, "value": "latest"} for p in gained]
        seek = await client.post(
            f"/subscriptions/{peer_id}:seek",
            json={"topology_version": 2, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": heads[p]} for p in gained]},
        }
        reader = await stack.enter_async_context(_member_stream(api, sub_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 2,
            "assigned": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in kept
            ],
        }
        assert baseline == ("topology", topology)
        if disconnect:
            async with AsyncExitStack() as connection:
                peer_reader = await connection.enter_async_context(
                    _member_stream(api, peer_id, "sse")
                )
                baseline = await peer_reader.next_frame(timeout=5)
                topology = {
                    "kind": "topology",
                    "topology_version": 2,
                    "assigned": [
                        {
                            "topic": topic,
                            "partition": p,
                            "offset": heads[p],
                            "last_examined": heads[p],
                        }
                        for p in gained
                    ],
                }
                assert baseline == ("topology", topology)
        else:
            left = await client.delete(f"/subscriptions/{peer_id}")
            assert (left.status_code, left.content) == (204, b"")
        terminal = await reader.await_kind("control", timeout=10)
        assert terminal == {
            "kind": "control",
            "code": "terminal",
            "reason": "rebalanced",
            "positions": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in kept
            ],
        }
        with pytest.raises(StopAsyncIteration):
            await reader.next_frame(timeout=10)
        recorded = {p: heads[p] for p in gained}
        recorded.update(
            {item["partition"]: item["last_examined"] for item in terminal["positions"]}
        )
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        replacement_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{replacement_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        replacement = {
            "id": replacement_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 4,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == replacement
        assert joined.json() == {
            **replacement,
            "interests": [{**replacement["interests"][0], "unmatched_types": []}],
        }
        assert replacement_id not in (sub_id, peer_id)
        requested = [{"partition": p, "value": recorded[p]} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{replacement_id}:seek",
            json={
                "topology_version": replacement["topology_version"],
                "positions": {topic: requested},
            },
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": recorded[p]} for p in partitions]},
        }
        replacement_reader = await stack.enter_async_context(
            _member_stream(api, replacement_id, "sse")
        )
        baseline = await replacement_reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 4,
            "assigned": [
                {
                    "topic": topic,
                    "partition": p,
                    "offset": recorded[p],
                    "last_examined": recorded[p],
                }
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        async with client.stream(
            "GET",
            "/events:stream",
            params={"subscription": sub_id},
            headers={"Accept": "multipart/mixed"},
        ) as absent:
            assert absent.status_code == 404
            await absent.aread()
        assert (absent.status_code, absent.headers["content-type"]) == (
            404,
            "application/problem+json",
        )
        assert absent.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": f"subscription '{sub_id}' does not exist",
            "instance": f"{API_BASE}/events:stream",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "resource_name": sub_id,
            },
        }
