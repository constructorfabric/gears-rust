"""Publish -> consume e2e happy path (design.md D8, task 11.2).

Every request body sent and response body asserted is inlined per test -
no shared helper hides a request shape (unlike `gears/oagw/helpers.py`'s
`create_upstream`/`create_route`, which build the JSON body internally).
"""

from __future__ import annotations

import asyncio
import re
import uuid
from contextlib import AsyncExitStack
from datetime import datetime

import pytest

from .conftest import (
    ALL_EVENT_TYPES_PATTERN,
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    TOPIC_STREAM,
    SseFrameReader,
)
from .test_consumer_subscriptions import _created_subscription_id


def _stream_instant(value: str) -> tuple[datetime, int]:
    assert isinstance(value, str), value
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?Z",
        value,
    ), value
    clock = datetime.fromisoformat(value.replace("Z", "+00:00")).replace(microsecond=0)
    return clock, int(value[:-1].partition(".")[2].ljust(9, "0"))


async def _read_sse_event(reader: SseFrameReader, deadline: float) -> dict:
    clock = asyncio.get_running_loop().time
    while True:
        remaining = deadline - clock()
        assert remaining > 0, "no SSE event before the deadline"
        name, frame = await reader.next_frame(timeout=remaining)
        assert name == frame["kind"], (name, frame)
        if name == "event":
            return frame
        assert name == "heartbeat", frame


async def _read_case_window(
    reader: SseFrameReader, high_water: int, sentinel: str, deadline: float,
) -> tuple[list[dict], list[dict]]:
    history: list[dict] = []
    delivered: list[dict] = []
    while True:
        frame = await _read_sse_event(reader, deadline)
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


@pytest.mark.backend_sensitive
async def test_publish_then_consume_happy_path(api, test_env):
    """scenario: consumer/stream/1.09-positive-sse-event-stream.md"""
    topic = TOPIC_STREAM
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_STREAM],
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
    event = {
        "id": "16e20000-0000-4000-8000-000001640001",
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-19T10:20:30Z",
    }
    events = [event]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        # Earlier STREAM cases can leave async writes pending in the full suite.
        # The durable marker uses the same tenant key as the stream events.
        fence = await client.post(
            "/events",
            json={
                **event,
                "id": "16e20000-0000-4000-8000-000001640000",
                "subject": "stream-head-fence",
            },
            headers={"Prefer": "wait=10"},
            timeout=12,
        )
        assert (fence.status_code, fence.content, fence.headers.get("preference-applied")) == (
            201, b"", "wait=10",
        )
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
                    "GET", "/events:sse", params={"subscription": sub_id},
                    headers={"Accept": "text/event-stream"},
                )
            )
            assert (response.status_code, response.headers.get("content-type")) == (
                200, "text/event-stream",
            )
            reader = SseFrameReader(response)
            name, topology = await reader.next_frame(timeout=5)
            assert name == "topology"
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

        deadline = asyncio.get_running_loop().time() + 5
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
    assert delivered == [stored[0]]


@pytest.mark.backend_sensitive
async def test_wildcard_interest_delivers_every_event_type(api, test_env):
    """scenario: consumer/stream/1.15-positive-wildcard-event-type-stream.md"""
    topic = TOPIC_STREAM
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
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
    event = {
        "id": "16e20000-0000-4000-8000-000001650001",
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-19T10:20:30Z",
    }
    events = [event]

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
                    "GET", "/events:sse", params={"subscription": sub_id},
                    headers={"Accept": "text/event-stream"},
                )
            )
            assert (response.status_code, response.headers.get("content-type")) == (
                200, "text/event-stream",
            )
            reader = SseFrameReader(response)
            name, topology = await reader.next_frame(timeout=5)
            assert name == "topology"
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

        deadline = asyncio.get_running_loop().time() + 5
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
    assert delivered == [stored[0]]
