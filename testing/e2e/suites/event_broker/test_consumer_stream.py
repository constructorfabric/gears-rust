"""Streaming transport scenarios (consumer/stream/1.01-1.20).

Skips:
- 1.07 and 1.08 are already covered by ``test_stream_guardrails.py``
- 1.09 (SSE happy path) is already covered by ``test_publish_consume.py``
- 1.06 (terminated subscription via shard shutdown) is xfail: triggering an
  internal shard teardown is not possible from outside in E2E.
- 1.14 (control-progress-frame) runs on TOPIC_PROGRESS: a producer fills it with
  events another tenant's subscription rejects, past the drift threshold the
  generated config sets.
- 1.17 (only-mine-delivery) runs on TOPIC_OWNERSHIP and 1.19 (subtree-delivery)
  on TOPIC_SUBTREE: one partition each, written by that test alone.
- 1.18 (subject-types-selector) runs on TOPIC_SUBJECT_SELECTOR: one partition,
  and an event type admitting two subject types.

Patterns:
- Multipart (``/events:stream``) tests use ``MultipartReader`` from conftest.
- SSE tests use ``SseFrameReader`` from conftest.
- Every streaming test opens a fresh subscription on its own group/tenant_id.
"""

from __future__ import annotations

import asyncio
import re
import uuid
from contextlib import AsyncExitStack, aclosing, asynccontextmanager
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    ALL_EVENT_TYPES_PATTERN,
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_LONGPOLL,
    EVENT_TYPE_OWNERSHIP,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_SUBTREE,
    group_seek_positions,
    SUBJECT_TYPE,
    TOPIC_LONGPOLL,
    TOPIC_STREAM,
    TOPIC_4P,
    EVENT_TYPE_4P,
    EVENT_TYPE_PROGRESS,
    EVENT_TYPE_SUBJECT_SELECTOR,
    PROGRESS_DRIFT_THRESHOLD,
    SUBJECT_TYPE_OTHER,
    TOPIC_PROGRESS,
    TOPIC_OWNERSHIP,
    TOPIC_SUBJECT_SELECTOR,
    TOPIC_SUBTREE,
    MultipartReader,
    SseFrameReader,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TRACE_ID,
    XFAIL_AUTH_DISABLED,
)
from .test_consumer_subscriptions import (
    _created_subscription_id,
    _member_stream,
    _subscription_created_at,
)


_LIVE_CHILD_TENANT = "00000000-0000-0000-0000-000000000004"


def _check_owner_request(response: httpx.Response) -> None:
    headers = response.request.headers
    assert headers.get("authorization") == f"Bearer {TOKEN_PRINCIPAL_A}", "expected A@TA"
    assert "x-toolkit-internal-token" not in headers


async def _live_manifest_bounds(client, topic, partitions):
    bounds = {}
    for partition in partitions:
        response = await client.get("/topics/segments", params={
            "topic": topic,
            "partition": partition,
        })
        _check_owner_request(response)
        assert (response.status_code, response.headers["content-type"]) == (200, "application/json")
        manifest = response.json()
        assert set(manifest) == {
            "topic", "partition", "start_sequence", "end_sequence", "start_time",
            "end_time", "segments",
        }
        assert type(manifest["partition"]) is int
        assert (manifest["topic"], manifest["partition"]) == (topic, partition)
        start, head = manifest["start_sequence"], manifest["end_sequence"]
        assert type(start) is int
        assert type(head) is int
        assert 0 <= start <= head
        for field in ("start_time", "end_time"):
            if manifest[field] is not None:
                _stream_instant(manifest[field])
        assert isinstance(manifest["segments"], list)
        bounds[partition] = (max(start - 1, 0), head)
    return bounds


async def _live_group(client):
    created = await client.post("/consumer-groups")
    _check_owner_request(created)
    assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
    location = created.headers["location"]
    group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
    prefix = "gts.cf.core.events.consumer_group.v1~"
    assert group_id.startswith(prefix)
    assert group_id == f"{prefix}{uuid.UUID(group_id.removeprefix(prefix))}"
    assert location == f"{API_BASE}/consumer-groups/{group_id}"
    read = await client.get(f"/consumer-groups/{group_id}")
    _check_owner_request(read)
    assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
    expected = {
        "id": group_id,
        "kind": "anonymous",
        "tenant": DEFAULT_TENANT_ID,
        "owner_principal": DEFAULT_SUBJECT_ID,
        "description": None,
        "created_at": _subscription_created_at(read.json()),
    }
    assert created.json() == expected
    assert read.json() == expected
    return group_id


async def _live_join(client, group_id, interest, partitions, *, agent="e2e-test"):
    joined = await client.post("/subscriptions", json={
        "consumer_group": group_id,
        "client_agent": agent,
        "interests": [interest],
    })
    _check_owner_request(joined)
    assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
    sub_id = _created_subscription_id(joined)
    read = await client.get(f"/subscriptions/{sub_id}")
    _check_owner_request(read)
    assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": agent,
        "assigned": [{"topic": interest["topic"], "partition": p} for p in partitions],
        "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
        "topology_version": 1,
        "created_at": _subscription_created_at(read.json()),
    }
    assert read.json() == expected
    assert joined.json() == {
        **expected,
        "interests": [{**expected["interests"][0], "unmatched_types": []}],
    }
    for body in (read.json(), joined.json()):
        assert type(body["topology_version"]) is int
        assert all(type(row["partition"]) is int for row in body["assigned"])
    return sub_id


def _live_seek_result(response, topic, offsets):
    _check_owner_request(response)
    assert (response.status_code, response.headers["content-type"]) == (200, "application/json")
    body = response.json()
    assert body == {
        "positions": {topic: [{"partition": p, "value": offset} for p, offset in offsets.items()]},
    }
    for row in body["positions"][topic]:
        assert type(row["partition"]) is int
        assert type(row["value"]) is int


async def _live_seek_earliest(client, sub_id, topic, offsets):
    response = await client.post(f"/subscriptions/{sub_id}:seek", json={
        "topology_version": 1,
        "positions": {topic: [{"partition": p, "value": "earliest"} for p in offsets]},
    })
    _live_seek_result(response, topic, offsets)


def _live_topology(frame, topic, offsets):
    assert frame == {
        "kind": "topology",
        "topology_version": 1,
        "assigned": [
            {"topic": topic, "partition": p, "offset": offset, "last_examined": offset}
            for p, offset in offsets.items()
        ],
    }
    assert type(frame["topology_version"]) is int
    for row in frame["assigned"]:
        for key in ("partition", "offset", "last_examined"):
            assert type(row[key]) is int


@asynccontextmanager
async def _live_stream(api, sub_id, transport="sse"):
    accept = "text/event-stream" if transport == "sse" else "multipart/mixed"
    async with api() as client, AsyncExitStack() as stack:
        stream = client.stream(
            "GET", f"/events:{transport}", params={"subscription": sub_id},
            headers={"Accept": accept}, timeout=None,
        )
        async with asyncio.timeout(5):
            response = await stack.enter_async_context(stream)
        _check_owner_request(response)
        assert response.request.headers.get("accept") == accept
        assert response.status_code == 200
        yield response


async def _live_sse_frame(reader, deadline):
    clock = asyncio.get_running_loop().time
    remaining = deadline - clock()
    assert remaining > 0, "stream frame deadline elapsed"
    name, frame = await reader.next_frame(timeout=remaining)
    assert isinstance(frame, dict)
    assert name == frame.get("kind"), (name, frame)
    return frame


def _live_heartbeat(frame):
    assert set(frame) == {"kind", "at"}
    assert frame["kind"] == "heartbeat"
    return _stream_instant(frame["at"])


def _live_event_fields(frame, began_at):
    assert set(frame) == {"kind", "payload"}
    assert frame["kind"] == "event"
    payload = frame["payload"]
    assert isinstance(payload, dict)
    assert type(payload["partition"]) is int
    assert type(payload["sequence"]) is int
    assert payload["sequence"] > 0
    sequenced, _ = _stream_instant(payload["sequence_time"])
    assert began_at <= sequenced <= datetime.now(timezone.utc).replace(microsecond=0)
    return payload


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


async def _setup_seeded_sub(client, topic: str = TOPIC_STREAM, event_type: str = EVENT_TYPE_STREAM) -> tuple[str, str]:
    """Create group → JOIN → SEEK earliest.  Returns (tenant_id, sub_id)."""
    group_resp = await client.post("/consumer-groups")
    assert group_resp.status_code == 201
    group_id = group_resp.json()["id"]

    tenant_id = str(uuid.uuid4())
    sub_resp = await client.post(
        "/subscriptions",
        json={
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [{"topic": topic, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [event_type]}],
        },
    )
    assert sub_resp.status_code == 201
    sub_id = str(sub_resp.json()["id"])
    assigned = sub_resp.json()["assigned"]

    positions = [
        {"topic": a["topic"], "partition": a["partition"], "value": "earliest"}
        for a in assigned
    ]
    seek_resp = await client.post(
        f"/subscriptions/{sub_id}:seek",
        json={"topology_version": sub_resp.json()["topology_version"], "positions": group_seek_positions(positions)},
    )
    assert seek_resp.status_code == 200
    return tenant_id, sub_id


@pytest.mark.backend_sensitive
@pytest.mark.timeout(45, func_only=True)
async def test_multipart_stream_delivers_event_frame(api, test_env):
    """scenario: consumer/stream/1.01-positive-stream-multipart-frames.md"""
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
            "id": "17e20000-0000-4000-8000-000000006901",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
        }
        marker = {**event, "id": "17e20000-0000-4000-8000-000000006900"}
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
        for event_id in (
            "17e20000-0000-4000-8000-000000006901",
            "17e20000-0000-4000-8000-000000006902",
            "17e20000-0000-4000-8000-000000006903",
        ):
            event = {**event, "id": event_id}
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
        heartbeat = await reader.next_frame(timeout=10)
        assert heartbeat == {
            "kind": "heartbeat",
            "at": _subscription_created_at({"created_at": heartbeat["at"]}),
        }


@pytest.mark.backend_sensitive
@pytest.mark.timeout(20, func_only=True)
async def test_heartbeat_frame_arrives_on_idle_stream(api, test_env):
    """scenario: consumer/stream/1.02-positive-stream-heartbeat-cadence.md

    An idle SSE stream emits three heartbeats on the configured five-second cadence.
    """
    clock = asyncio.get_running_loop().time
    deadline = clock() + 20
    interest = {
        "topic": TOPIC_LONGPOLL,
        "subject_tenant": _LIVE_CHILD_TENANT,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_LONGPOLL],
    }
    async with api() as client:
        bounds = await _live_manifest_bounds(client, TOPIC_LONGPOLL, [0])
        offsets = {0: bounds[0][0]}
        group_id = await _live_group(client)
        sub_id = await _live_join(client, group_id, interest, [0])
        await _live_seek_earliest(client, sub_id, TOPIC_LONGPOLL, offsets)

    async with _live_stream(api, sub_id) as response:
        assert response.headers["content-type"] == "text/event-stream"
        reader = SseFrameReader(response)
        frame = await _live_sse_frame(reader, min(deadline, clock() + 5))
        _live_topology(frame, TOPIC_LONGPOLL, offsets)
        topology_received = clock()
        first_deadline = min(deadline, topology_received + 12)
        instants = []
        receipts = []
        for heartbeat_deadline in (first_deadline, deadline, deadline):
            frame = await _live_sse_frame(reader, heartbeat_deadline)
            receipts.append(clock())
            instants.append(_live_heartbeat(frame))

    assert topology_received < receipts[0] < receipts[1] < receipts[2] <= deadline
    assert receipts[0] <= first_deadline
    assert instants[0] < instants[1] < instants[2]
    intervals = [b - a for a, b in zip([topology_received, *receipts[:2]], receipts, strict=True)]
    print(f"idle heartbeat intervals (s): {intervals}")


@pytest.mark.timeout(20, func_only=True)
async def test_topology_frame_on_rebalance(api, test_env):
    """scenario: consumer/stream/1.03-positive-stream-topology-frame-on-rebalance.md"""
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


async def test_positions_not_set_returns_409(api):
    """scenario: consumer/stream/1.04-negative-stream-positions-not-set.md"""
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


async def test_unknown_subscription_returns_404(api):
    """scenario: consumer/stream/1.05-negative-stream-unknown-subscription.md"""
    sub_id = "99999999-8888-7777-6666-555555555555"
    async with api() as client:
        resp = await client.get(
            "/events:stream",
            params={"subscription": sub_id},
            headers={"Accept": "multipart/mixed"},
        )
    assert resp.status_code == 404
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
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


@pytest.mark.xfail(
    reason="triggering a terminated subscription requires internal shard shutdown not available in E2E",
    strict=False,
)
async def test_terminated_subscription_returns_410(api):
    """scenario: consumer/stream/1.06-negative-stream-terminated-subscription.md"""
    from .test_flows import _resumed_multipart

    interest = {
        "topic": TOPIC_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_STREAM],
    }
    accept = {"Accept": "multipart/mixed"}
    clock = asyncio.get_running_loop().time
    deadline = clock() + 8

    async def first_topology(owner, subscription):
        async with owner.stream(
            "GET", "/events:stream", params={"subscription": subscription}, headers=accept,
        ) as response:
            _check_owner_request(response)
            assert response.status_code == 200
            async with aclosing(_resumed_multipart(response, min(deadline, clock() + 5))) as frames:
                return await anext(frames)

    async with asyncio.timeout_at(deadline), api(TOKEN_PRINCIPAL_A) as owner:
        group_id = await _live_group(owner)
        sub_id = await _live_join(owner, group_id, interest, [0])
        bounds = await _live_manifest_bounds(owner, TOPIC_STREAM, [0])
        floor, head = bounds[0]
        checkpoint = floor + 1
        # The earlier multipart case stores events in this ordinary partition.
        assert 0 < checkpoint and floor < checkpoint < head
        seeded = await owner.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1,
                "positions": {TOPIC_STREAM: [{"partition": 0, "value": checkpoint}]},
            },
        )
        assert (seeded.status_code, seeded.headers["content-type"]) == (200, "application/json")
        positions = seeded.json()
        assert positions == {"positions": {TOPIC_STREAM: [{"partition": 0, "value": checkpoint}]}}
        position = positions["positions"][TOPIC_STREAM][0]
        assert type(position["partition"]) is int
        assert type(position["value"]) is int

        expected_topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [{
                "topic": TOPIC_STREAM,
                "partition": 0,
                "offset": checkpoint,
                "last_examined": checkpoint,
            }],
        }
        first = await first_topology(owner, sub_id)
        assert first == expected_topology

        left = await owner.delete(f"/subscriptions/{sub_id}")
        assert (left.status_code, left.content) == (204, b"")

        async with owner.stream(
            "GET", "/events:stream", params={"subscription": sub_id}, headers=accept,
        ) as stream_resp:
            _check_owner_request(stream_resp)
            assert stream_resp.status_code in (404, 410)
            assert stream_resp.headers["content-type"] == "application/problem+json"
            await stream_resp.aread()
            problem = stream_resp.json()
            if stream_resp.status_code == 404:
                detail = f"subscription '{sub_id}' does not exist"
            else:
                detail = (
                    f"subscription {sub_id} was terminated (reaped or left); re-JOIN to continue"
                )
            assert problem == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                "title": "Not Found",
                "status": stream_resp.status_code,
                "detail": detail,
                "instance": f"{API_BASE}/events:stream",
                "trace_id": TRACE_ID,
                "context": {
                    "resource_type": "gts.cf.core.events.subscription.v1~",
                    "resource_name": sub_id,
                },
            }

        replacement = await _live_join(owner, group_id, interest, [0])
        assert replacement != sub_id
        resumed = await first_topology(owner, replacement)
        assert resumed == expected_topology
        for topology in (first, resumed):
            assert type(topology["topology_version"]) is int
            assignment = topology["assigned"][0]
            for field in ("partition", "offset", "last_examined"):
                assert type(assignment[field]) is int

    assert stream_resp.status_code == 410


# 1.07 and 1.08 are covered by test_stream_guardrails.py - not duplicated here.
# 1.09 (SSE happy path) is covered by test_publish_consume.py.


async def test_stream_rejects_unknown_query_params(api):
    """scenario: consumer/stream/1.10-negative-stream-rejects-timeout-collect-params.md

    Legacy ``timeout`` and ``collect`` query parameters are not accepted;
    the endpoint only recognises ``subscription``.
    """
    fake_sub_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.get(
            f"/events:stream?subscription={fake_sub_id}&timeout=20&collect=50",
            headers={"Accept": "multipart/mixed"},
        )
    # The query is checked before the subscription lookup, so a random id
    # still answers 400, naming the first unknown parameter.
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidQuery: unknown field `timeout`, expected `subscription`",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidQuery: unknown field `timeout`, expected `subscription`",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


@pytest.mark.timeout(15, func_only=True)
async def test_second_stream_on_same_subscription_returns_409(api, test_env):
    """scenario: consumer/stream/1.11-negative-streaming-in-progress.md"""
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
                        "type": "streaming_in_progress",
                        "subject": sub_id,
                        "description": (
                            "one stream per subscription; a second stream or a SEEK "
                            "is refused while it is open"
                        ),
                    }
                ],
            },
        }
        kind, heartbeat = await reader.next_frame(timeout=10)
        assert kind == "heartbeat"
        assert heartbeat == {
            "kind": "heartbeat",
            "at": _subscription_created_at({"created_at": heartbeat["at"]}),
        }


@pytest.mark.timeout(15, func_only=True)
async def test_delete_while_streaming_closes_connection(api, test_env):
    """scenario: consumer/stream/1.13-positive-delete-while-streaming.md"""
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
        left = await client.delete(f"/subscriptions/{sub_id}")
        assert (left.status_code, left.content) == (204, b"")
        terminal = await reader.await_kind("control", timeout=10)
        assert terminal == {
            "kind": "control",
            "code": "terminal",
            "reason": "lose_all",
            "positions": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in partitions
            ],
        }
        with pytest.raises(StopAsyncIteration):
            await reader.next_frame(timeout=10)
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
        read = await client.get(f"/consumer-groups/{group_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        assert read.json() == group
        listing = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
        assert (listing.status_code, listing.headers["content-type"]) == (
            200,
            "application/json",
        )
        assert listing.json() == {
            "items": [],
            "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
        }


@pytest.mark.timeout(30, func_only=True)
async def test_stream_terminates_with_control_on_rebalance_gain(api, test_env):
    """scenario: consumer/stream/1.12-positive-stream-terminates-on-rebalance-gain.md"""

    topic = TOPIC_4P
    partitions = [0, 1, 2, 3]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_4P],
    }
    read_interest = {**interest, "barrier_mode": "respect", "filter": None}
    async with api() as client:
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{group_prefix}{uuid.UUID(group_id.removeprefix(group_prefix))}"
        created_body = created.json()
        assert isinstance(created_body, dict)
        assert "created_at" in created_body
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(created_body),
        }
        assert created_body == group
        group_read = await client.get(f"/consumer-groups/{group_id}")
        assert (group_read.status_code, group_read.headers["content-type"]) == (
            200, "application/json",
        )
        assert group_read.json() == group

        heads = {}
        for partition in partitions:
            manifest = await client.get(
                "/topics/segments", params={"topic": topic, "partition": partition},
            )
            assert (manifest.status_code, manifest.headers["content-type"]) == (
                200, "application/json",
            )
            snapshot = manifest.json()
            assert isinstance(snapshot, dict)
            assert set(snapshot) == {
                "topic", "partition", "start_sequence", "end_sequence",
                "start_time", "end_time", "segments",
            }
            assert (snapshot["topic"], snapshot["partition"]) == (topic, partition)
            assert type(snapshot["partition"]) is int
            head = snapshot["end_sequence"]
            assert type(head) is int
            assert head >= 0
            heads[partition] = head

        members = {}
        assignments = {}
        for version, agent in enumerate(("consumer-a", "consumer-b"), start=1):
            joined = await client.post(
                "/subscriptions",
                json={"consumer_group": group_id, "client_agent": agent, "interests": [interest]},
            )
            assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
            member_id = _created_subscription_id(joined)
            assert member_id not in members
            ranks = sorted([*members, member_id], key=uuid.UUID)
            ranges = (partitions,) if version == 1 else (partitions[:2], partitions[2:])
            assignments = dict(zip(ranks, ranges, strict=True))
            joined_body = joined.json()
            assert isinstance(joined_body, dict)
            assert "created_at" in joined_body
            member = {
                "id": member_id,
                "consumer_group": group_id,
                "client_agent": agent,
                "interests": [read_interest],
                "assigned": [{"topic": topic, "partition": p} for p in assignments[member_id]],
                "topology_version": version,
                "created_at": _subscription_created_at(joined_body),
            }
            assert joined_body == {
                **member,
                "interests": [{**read_interest, "unmatched_types": []}],
            }
            assert type(joined_body["topology_version"]) is int
            assert all(type(row["partition"]) is int for row in joined_body["assigned"])
            members[member_id] = member
            for current_id, current_parts in assignments.items():
                read = await client.get(f"/subscriptions/{current_id}")
                assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
                current = read.json()
                assert current == {
                    **members[current_id],
                    "assigned": [{"topic": topic, "partition": p} for p in current_parts],
                    "topology_version": version,
                }
                assert type(current["topology_version"]) is int
                assert all(type(row["partition"]) is int for row in current["assigned"])

                seek = await client.post(
                    f"/subscriptions/{current_id}:seek",
                    json={
                        "topology_version": version,
                        "positions": {
                            topic: [{"partition": p, "value": "latest"} for p in current_parts],
                        },
                    },
                )
                assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
                resolved = seek.json()
                assert resolved == {
                    "positions": {
                        topic: [{"partition": p, "value": heads[p]} for p in current_parts],
                    },
                }
                for position in resolved["positions"][topic]:
                    assert type(position["partition"]) is int
                    assert type(position["value"]) is int

        survivor_id, peer_id = members
        final_positions = [
            {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
            for p in assignments[survivor_id]
        ]
        # The budget spans the open, peer LEAVE, terminal frame and EOF.
        async with asyncio.timeout(15) as budget:
            async with _member_stream(api, survivor_id, "sse") as reader:
                deadline = budget.when()
                loop = asyncio.get_running_loop()
                kind, opening = await reader.next_frame(timeout=deadline - loop.time())
                assert (kind, opening) == ("topology", {
                    "kind": "topology",
                    "topology_version": 2,
                    "assigned": final_positions,
                })
                assert type(opening["topology_version"]) is int
                for position in opening["assigned"]:
                    for field in ("partition", "offset", "last_examined"):
                        assert type(position[field]) is int

                left = await client.delete(f"/subscriptions/{peer_id}")
                assert (left.status_code, left.content) == (204, b"")
                while True:
                    kind, frame = await reader.next_frame(timeout=deadline - loop.time())
                    if kind != "heartbeat":
                        break
                    assert isinstance(frame, dict)
                    assert set(frame) == {"kind", "at"}
                    assert frame == {
                        "kind": "heartbeat",
                        "at": _subscription_created_at({"created_at": frame["at"]}),
                    }
                assert (kind, frame) == ("control", {
                    "kind": "control",
                    "code": "terminal",
                    "reason": "rebalanced",
                    "positions": final_positions,
                })
                for position in frame["positions"]:
                    for field in ("partition", "offset", "last_examined"):
                        assert type(position[field]) is int
                with pytest.raises(StopAsyncIteration):
                    await reader.next_frame(timeout=deadline - loop.time())

        read = await client.get(f"/subscriptions/{survivor_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        survivor = read.json()
        assert survivor == {
            **members[survivor_id],
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "topology_version": 3,
        }
        assert type(survivor["topology_version"]) is int
        assert all(type(row["partition"]) is int for row in survivor["assigned"])
        group_read = await client.get(f"/consumer-groups/{group_id}")
        assert (group_read.status_code, group_read.headers["content-type"]) == (
            200, "application/json",
        )
        assert group_read.json() == group


@pytest.mark.backend_sensitive
@pytest.mark.timeout(45, func_only=True)
async def test_control_progress_frame_emitted_on_filtered_stream(api, test_env):
    """scenario: consumer/stream/1.14-positive-control-progress-frame.md

    The TA cursor stays at its floor while its scan reaches the TC reader's head.
    """
    clock = asyncio.get_running_loop().time
    deadline = clock() + 45
    topic = TOPIC_PROGRESS
    event_type = EVENT_TYPE_PROGRESS
    interest = {
        "topic": topic,
        "subject_tenant": _LIVE_CHILD_TENANT,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    events = [
        {
            "id": f"29e20000-0000-4000-8000-{n:012x}",
            "type": event_type,
            "subject_tenant": _LIVE_CHILD_TENANT,
            "source": "e2e-test",
            "subject": f"s{n}",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
        }
        for n in range(PROGRESS_DRIFT_THRESHOLD + 100)
    ]
    assert len(events) == 600
    async with api() as client:
        bounds = await _live_manifest_bounds(client, topic, [0])
        assert bounds == {0: (0, 0)}
        offsets = {0: bounds[0][0]}
        broad_group = await _live_group(client)
        broad_sub = await _live_join(client, broad_group, interest, [0])
        await _live_seek_earliest(client, broad_sub, topic, offsets)
        began_at = datetime.now(timezone.utc).replace(microsecond=0)
        for start in range(0, len(events), 100):
            response = await client.post(
                "/events:batch", json={"events": events[start:start + 100]},
            )
            _check_owner_request(response)
            assert (response.status_code, response.content) == (202, b"")

        async with _live_stream(api, broad_sub) as response:
            assert response.headers["content-type"] == "text/event-stream"
            reader = SseFrameReader(response)
            opening = await _live_sse_frame(reader, min(deadline, clock() + 5))
            _live_topology(opening, topic, offsets)
            head = offsets[0]
            for event in events:
                frame = await _live_sse_frame(reader, deadline)
                while frame["kind"] == "heartbeat":
                    _live_heartbeat(frame)
                    frame = await _live_sse_frame(reader, deadline)
                payload = _live_event_fields(frame, began_at)
                occurred_at = _stream_instant(payload["occurred_at"])
                assert occurred_at == _stream_instant(event["occurred_at"])
                assert payload["sequence"] > head
                assert frame == {
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
                head = payload["sequence"]
            _live_heartbeat(await _live_sse_frame(reader, deadline))

        filtered_group = await _live_group(client)
        filtered_sub = await _live_join(client, filtered_group, {
            **interest, "subject_tenant": DEFAULT_TENANT_ID,
        }, [0])
        assert filtered_group != broad_group
        assert filtered_sub != broad_sub
        await _live_seek_earliest(client, filtered_sub, topic, offsets)

    async with _live_stream(api, filtered_sub) as response:
        assert response.headers["content-type"] == "text/event-stream"
        reader = SseFrameReader(response)
        _live_topology(await _live_sse_frame(reader, min(deadline, clock() + 5)), topic, offsets)
        floor = offsets[0]
        frontier = floor
        while frontier < head:
            frame = await _live_sse_frame(reader, deadline)
            if frame["kind"] == "heartbeat":
                _live_heartbeat(frame)
                continue
            assert frame["kind"] == "control", frame
            assert frame.get("code") == "progress", frame
            position, = frame["positions"]
            for field in ("partition", "offset", "last_examined"):
                assert type(position[field]) is int
            examined = position["last_examined"]
            assert frontier < examined <= head
            assert frame == {
                "kind": "control",
                "code": "progress",
                "positions": [
                    {"topic": topic, "partition": 0, "offset": floor, "last_examined": examined},
                ],
            }
            frontier = examined
        assert frame == {
            "kind": "control",
            "code": "progress",
            "positions": [{"topic": topic, "partition": 0, "offset": floor, "last_examined": head}],
        }
        # Leave a five-second heartbeat interval plus slack for the last read.
        if deadline - clock() > 6:
            _live_heartbeat(await _live_sse_frame(reader, deadline))


def _stream_instant(value: str) -> tuple[datetime, int]:
    assert isinstance(value, str), value
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?Z",
        value,
    ), value
    clock = datetime.fromisoformat(value.replace("Z", "+00:00")).replace(microsecond=0)
    return clock, int(value[:-1].partition(".")[2].ljust(9, "0"))


async def _read_case_window(
    reader: MultipartReader, high_water: int, sentinel: str, deadline: float,
) -> tuple[list[dict], list[dict]]:
    history: list[dict] = []
    delivered: list[dict] = []
    clock = asyncio.get_running_loop().time
    while True:
        remaining = deadline - clock()
        assert remaining > 0, f"no sentinel {sentinel}; got {history + delivered}"
        frame = await reader.await_kind("event", timeout=remaining)
        assert set(frame) == {"kind", "payload"}, frame
        match frame:
            case {"kind": "event", "payload": {"sequence": int(sequence), "id": str(event_id)}}:
                assert type(sequence) is int and sequence > 0
            case unexpected:
                pytest.fail(f"unexpected event frame: {unexpected}")
        if sequence <= high_water:
            assert not delivered, frame
            history.append(frame)
            continue
        delivered.append(frame)
        if event_id == sentinel:
            return history, delivered


@pytest.mark.timeout(30, func_only=True)
async def test_subject_types_selector(api, test_env):
    """scenario: consumer/stream/1.18-positive-subject-types-selector.md

    An interest whose ``subject_types`` names only SUBJECT_TYPE_OTHER is not
    delivered an event about SUBJECT_TYPE. Event A (SUBJECT_TYPE) is published
    before event B (SUBJECT_TYPE_OTHER) on a one-partition topic, so a stream
    that ignored the selector would deliver A first.
    """
    topic = TOPIC_SUBJECT_SELECTOR
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SUBJECT_SELECTOR],
            "subject_types": [SUBJECT_TYPE_OTHER],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000080{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SUBJECT_SELECTOR, SUBJECT_TYPE),
                (EVENT_TYPE_SUBJECT_SELECTOR, SUBJECT_TYPE_OTHER),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + 15
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[1]]


@pytest.mark.timeout(45, func_only=True)
async def test_only_mine_delivery(api, test_env):
    """scenario: consumer/stream/1.17-positive-only-mine-delivery.md

    Three events of the caller's tenant on a one-partition topic: one with no
    owner, one owned by another subject, then one owned by the caller. A JOIN
    without ``ownership_filter`` is delivered only the caller's event, so the
    two before it were skipped; an ``entire_tenant`` JOIN is delivered all
    three, in order.
    """
    topic = TOPIC_OWNERSHIP
    target_interests = [
        {
            "topic": topic,
            "types": [EVENT_TYPE_OWNERSHIP],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": "16e20000-0000-4000-8000-000000810001",
            "type": EVENT_TYPE_OWNERSHIP,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "unowned",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
        },
        {
            "id": "16e20000-0000-4000-8000-000000810002",
            "type": EVENT_TYPE_OWNERSHIP,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "foreign",
            "subject_type": SUBJECT_TYPE,
            "subject_owner": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "occurred_at": "2026-01-19T10:20:31Z",
        },
        {
            "id": "16e20000-0000-4000-8000-000000810003",
            "type": EVENT_TYPE_OWNERSHIP,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "mine",
            "subject_type": SUBJECT_TYPE,
            "subject_owner": DEFAULT_SUBJECT_ID,
            "occurred_at": "2026-01-19T10:20:32Z",
        },
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            if position == "earliest":
                expected_interests[0].update(
                    subject_tenant=DEFAULT_TENANT_ID, ownership_filter="only_mine",
                )
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + 10
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[2]]


@pytest.mark.timeout(45, func_only=True)
async def test_subtree_delivery(api, test_env):
    """scenario: consumer/stream/1.19-positive-subtree-delivery.md

    A@TA relies on the default subtree root. A@TB reads the sibling event
    before the child publish so both events compete on the same partition.
    """
    from .test_auth import TOKEN_PRINCIPAL_A_IN_TB, _OTHER_TENANT
    from .test_flows import _resumed_multipart

    topic = TOPIC_SUBTREE
    event_type = EVENT_TYPE_SUBTREE
    clock = asyncio.get_running_loop().time
    async with api(TOKEN_PRINCIPAL_A) as parent, api(TOKEN_PRINCIPAL_A_IN_TB) as sibling:
        response = await parent.get("/topics/segments", params={"topic": topic, "partition": 0})
        _check_owner_request(response)
        assert (response.status_code, response.headers.get("content-type")) == (
            200, "application/json",
        )
        manifest = response.json()
        assert set(manifest) == {
            "topic", "partition", "start_sequence", "end_sequence", "start_time",
            "end_time", "segments",
        }
        assert type(manifest["partition"]) is int
        assert (manifest["topic"], manifest["partition"]) == (topic, 0)
        start, head = manifest["start_sequence"], manifest["end_sequence"]
        assert type(start) is int
        assert type(head) is int
        assert 0 <= start <= head
        assert isinstance(manifest["segments"], list)
        if head == 0:
            assert (manifest["start_time"], manifest["end_time"], manifest["segments"]) == (
                None, None, [],
            )
        else:
            assert _stream_instant(manifest["start_time"]) <= _stream_instant(manifest["end_time"])
        floor = max(start - 1, 0)

        groups = []
        members = []
        for client, tenant, ownership in (
            (parent, DEFAULT_TENANT_ID, "tenant_subtree_included"),
            (sibling, _OTHER_TENANT, "entire_tenant"),
        ):
            created = await client.post("/consumer-groups")
            assert (created.status_code, created.headers.get("content-type")) == (
                201, "application/json",
            )
            group_path = f"{API_BASE}/consumer-groups/"
            group_id = created.headers["location"].removeprefix(group_path)
            prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(prefix)
            group_uuid = uuid.UUID(group_id.removeprefix(prefix))
            assert group_id == f"{prefix}{group_uuid}"
            assert created.headers["location"] == f"{group_path}{group_id}"

            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": tenant,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": _subscription_created_at(group_body),
            }
            assert created.json() == group_body == expected_group

            interest = {"topic": topic, "ownership_filter": ownership, "types": [event_type]}
            if client is sibling:
                interest["subject_tenant"] = _OTHER_TENANT
            joined = await client.post("/subscriptions", json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            })
            assert (joined.status_code, joined.headers.get("content-type")) == (
                201, "application/json",
            )
            member_id = _created_subscription_id(joined)
            read = await client.get(f"/subscriptions/{member_id}")
            assert (read.status_code, read.headers.get("content-type")) == (200, "application/json")
            normalized = {
                "topic": topic,
                "subject_tenant": tenant,
                "ownership_filter": ownership,
                "types": [event_type],
                "barrier_mode": "respect",
                "filter": None,
            }
            member_body = read.json()
            expected_member = {
                "id": member_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "assigned": [{"topic": topic, "partition": 0}],
                "interests": [normalized],
                "topology_version": 1,
                "created_at": _subscription_created_at(member_body),
            }
            assert member_body == expected_member
            join_body = joined.json()
            assert join_body == {
                **expected_member,
                "interests": [{**normalized, "unmatched_types": []}],
            }
            for body in (join_body, member_body):
                assert type(body["topology_version"]) is int
                assert type(body["assigned"][0]["partition"]) is int

            seek = await client.post(f"/subscriptions/{member_id}:seek", json={
                "topology_version": 1,
                "positions": {topic: [{"partition": 0, "value": "earliest"}]},
            })
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            cursor = seek.json()
            assert cursor == {"positions": {topic: [{"partition": 0, "value": floor}]}}
            position = cursor["positions"][topic][0]
            assert type(position["partition"]) is int
            assert type(position["value"]) is int
            groups.append(group_uuid)
            members.append(member_id)

        assert groups[0] != groups[1]
        assert members[0] != members[1]
        outside_id = str(uuid.uuid5(groups[0], "subtree-sibling"))
        child_id = str(uuid.uuid5(groups[0], "subtree-child"))
        assert outside_id != child_id

        previous_write = head
        for client, token, member, event_id, tenant, subject, occurred_at, visible_tenants in (
            (
                sibling, TOKEN_PRINCIPAL_A_IN_TB, members[1], outside_id, _OTHER_TENANT,
                "subtree-sibling", "2026-01-19T10:20:30Z", (_OTHER_TENANT,),
            ),
            (
                parent, TOKEN_PRINCIPAL_A, members[0], child_id, _LIVE_CHILD_TENANT,
                "subtree-child", "2026-01-19T10:20:31Z", (DEFAULT_TENANT_ID, _LIVE_CHILD_TENANT),
            ),
        ):
            event = {
                "id": event_id,
                "type": event_type,
                "subject_tenant": tenant,
                "source": "e2e-test",
                "subject": subject,
                "subject_type": SUBJECT_TYPE,
                "occurred_at": occurred_at,
                "data": {"strict_field": subject, "k": subject},
            }
            published = await client.post("/events", json=event)
            right_publisher = published.request.headers.get("authorization") == f"Bearer {token}"
            assert right_publisher, f"wrong authenticated publisher for {subject}"
            assert "x-toolkit-internal-token" not in published.request.headers
            assert (published.status_code, published.content) == (202, b"")

            accept = "multipart/mixed"
            deadline = clock() + 10
            async with asyncio.timeout_at(deadline):
                async with client.stream(
                    "GET", "/events:stream", params={"subscription": member},
                    headers={"Accept": accept}, timeout=None,
                ) as opened:
                    right_reader = opened.request.headers.get("authorization") == f"Bearer {token}"
                    assert right_reader, f"wrong authenticated reader for {subject}"
                    assert "x-toolkit-internal-token" not in opened.request.headers
                    assert opened.request.headers.get("accept") == accept
                    assert opened.status_code == 200
                    async with aclosing(_resumed_multipart(opened, deadline)) as frames:
                        topology = await anext(frames)
                        assert topology == {
                            "kind": "topology",
                            "topology_version": 1,
                            "assigned": [{
                                "topic": topic, "partition": 0, "offset": floor,
                                "last_examined": floor,
                            }],
                        }
                        assert type(topology["topology_version"]) is int
                        assigned = topology["assigned"][0]
                        assert all(type(assigned[key]) is int for key in (
                            "partition", "offset", "last_examined",
                        ))

                        previous_read = floor
                        async for frame in frames:
                            if frame.get("kind") == "heartbeat":
                                _live_heartbeat(frame)
                                continue
                            assert set(frame) == {"kind", "payload"}
                            assert frame["kind"] == "event"
                            payload = frame["payload"]
                            assert isinstance(payload, dict)
                            assert set(payload) == {
                                *event, "trace_parent", "partition", "sequence", "sequence_time",
                            }
                            assert payload["type"] == event_type
                            assert payload["subject_tenant"] in visible_tenants
                            assert type(payload["partition"]) is int
                            assert payload["partition"] == 0
                            sequence = payload["sequence"]
                            assert type(sequence) is int
                            assert sequence > previous_read
                            observed_occurrence = _stream_instant(payload["occurred_at"])
                            _stream_instant(payload["sequence_time"])
                            if sequence <= head:
                                old_id = payload["id"]
                                assert str(uuid.UUID(old_id)) == old_id
                                assert old_id not in (outside_id, child_id)
                                previous_read = sequence
                                continue
                            assert sequence > previous_write
                            assert {**payload, "occurred_at": observed_occurrence} == {
                                **event,
                                "occurred_at": _stream_instant(occurred_at),
                                "trace_parent": None,
                                "partition": 0,
                                "sequence": sequence,
                                "sequence_time": payload["sequence_time"],
                            }
                            previous_write = sequence
                            break


@pytest.mark.timeout(20, func_only=True)
async def test_stream_subscription_not_owned(api):
    """scenario: consumer/stream/1.20-negative-stream-subscription-not-owned.md"""

    from .test_flows import _resumed_multipart

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

        segments = await owner.get(
            "/topics/segments",
            params={"topic": TOPIC_STREAM, "partition": 0},
        )
        assert segments.status_code == 200
        assert segments.headers["content-type"] == "application/json"
        manifest = segments.json()
        assert isinstance(manifest, dict)
        assert set(manifest) == {
            "topic", "partition", "start_sequence", "end_sequence",
            "start_time", "end_time", "segments",
        }
        assert manifest["topic"] == TOPIC_STREAM
        assert type(manifest["partition"]) is int
        assert manifest["partition"] == 0
        start, end = manifest["start_sequence"], manifest["end_sequence"]
        assert type(start) is int
        assert type(end) is int
        assert 0 <= start <= end
        assert isinstance(manifest["segments"], list)
        for field in ("start_time", "end_time"):
            if manifest[field] is not None:
                _subscription_created_at({"created_at": manifest[field]})
        earliest = max(start - 1, 0)

        seek_path = f"{sub_path}:seek"
        seek_body = {
            "topology_version": 1,
            "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]},
        }

        seeded = await owner.post(seek_path, json=seek_body)
        assert seeded.status_code == 200
        assert seeded.headers["content-type"] == "application/json"
        seeded_body = seeded.json()
        assert seeded_body == {"positions": {TOPIC_STREAM: [{"partition": 0, "value": earliest}]}}
        cursor = seeded_body["positions"][TOPIC_STREAM][0]
        assert type(cursor["partition"]) is int
        assert type(cursor["value"]) is int

        peer_member = await peer.get(sub_path)
        assert peer_member.status_code == 200
        assert peer_member.headers["content-type"] == "application/json"
        peer_member_body = peer_member.json()
        assert peer_member_body == expected_member
        assert type(peer_member_body["topology_version"]) is int
        assert type(peer_member_body["assigned"][0]["partition"]) is int

        stream_params = {"subscription": sub_id}
        multipart = {"Accept": "multipart/mixed"}
        async with peer.stream(
            "GET", "/events:stream", params=stream_params, headers=multipart,
        ) as refused:
            assert refused.status_code == 403
            assert refused.headers["content-type"] == "application/problem+json"
            await refused.aread()
            assert refused.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
                "title": "Permission Denied",
                "status": 403,
                "detail": "You do not have permission to perform this operation",
                "instance": f"{API_BASE}/events:stream",
                "trace_id": TRACE_ID,
                "context": {
                    "reason": f"subscription '{sub_id}' was joined by another principal",
                    "resource_type": "gts.cf.core.events.subscription.v1~",
                },
            }

        async with owner.stream(
            "GET", "/events:stream", params=stream_params, headers=multipart,
        ) as opened:
            assert opened.status_code == 200
            deadline = asyncio.get_running_loop().time() + 5
            async with aclosing(_resumed_multipart(opened, deadline)) as frames:
                topology = await anext(frames)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [{
                    "topic": TOPIC_STREAM,
                    "partition": 0,
                    "offset": earliest,
                    "last_examined": earliest,
                }],
            }
            assert type(topology["topology_version"]) is int
            assignment = topology["assigned"][0]
            assert type(assignment["partition"]) is int
            assert type(assignment["offset"]) is int
            assert type(assignment["last_examined"]) is int
