"""Coupled producer + consumer journey (flows/1.01).

Composes all integration steps into one transcript:
1. Producer publishes 3 events.
2. Consumer creates a group, JOINs, SEEKs to earliest.
3. Consumer opens an SSE stream and receives all 3 events in order.

This test exercises the full publish→subscribe→consume pipeline and confirms
that events published before the subscription is opened are replayed correctly
(the broker replays from the cursor set by SEEK, not from the subscription's
join time).
"""

from __future__ import annotations

import asyncio
import json
import re
from datetime import datetime, timezone

import pytest

from .conftest import (
    API_BASE,
    EVENT_TYPE_4P,
    SUBJECT_TYPE,
    TOPIC_4P,
    TRACE_ID,
    SseFrameReader,
)
from .test_consumer_stream import (
    _LIVE_CHILD_TENANT,
    _check_owner_request,
    _live_event_fields,
    _live_group,
    _live_heartbeat,
    _live_join,
    _live_manifest_bounds,
    _live_seek_earliest,
    _live_seek_result,
    _live_sse_frame,
    _live_stream,
    _live_topology,
)
from .test_publish_consume import _stream_instant


async def _record_live_checkpoint(client, sub_id, topic, partition, offset, deadline):
    expected_held = {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 409,
        "detail": "Operation precondition not met",
        "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "violations": [{
                "type": "streaming_in_progress",
                "subject": sub_id,
                "description": "one stream per subscription; a second stream or a SEEK "
                               "is refused while it is open",
            }],
        },
    }
    async with asyncio.timeout_at(deadline):
        while True:
            response = await client.post(f"/subscriptions/{sub_id}:seek", json={
                "topology_version": 1,
                "positions": {topic: [{"partition": partition, "value": offset}]},
            })
            if response.status_code != 409:
                _live_seek_result(response, topic, {partition: offset})
                return
            _check_owner_request(response)
            assert response.headers["content-type"] == "application/problem+json"
            assert response.json() == expected_held
            assert type(response.json()["status"]) is int
            await asyncio.sleep(0.1)


async def _resumed_multipart(response, deadline):
    content_type = response.headers["content-type"]
    declared = re.fullmatch(r"multipart/mixed; boundary=(eb-frame-[0-9a-f]{32})", content_type)
    assert declared is not None
    prefix = f"--{declared[1]}\r\nContent-Type: application/json\r\n\r\n".encode()
    chunks = response.aiter_bytes()
    pending = b""
    while True:
        async with asyncio.timeout_at(deadline):
            while len(pending) < len(prefix):
                pending += await anext(chunks)
            assert pending.startswith(prefix), pending
            pending = pending[len(prefix):]
            while b"\r\n" not in pending:
                pending += await anext(chunks)
        raw, pending = pending.split(b"\r\n", 1)
        frame = json.loads(raw)
        assert isinstance(frame, dict)
        yield frame


@pytest.mark.backend_sensitive
@pytest.mark.timeout(30, func_only=True)
async def test_publish_subscribe_consume_full_flow(api, test_env):
    """scenario: flows/1.01-flow-publish-subscribe-consume.md

    The replacement member resumes the group's checkpoint without another SEEK.
    """
    clock = asyncio.get_running_loop().time
    deadline = clock() + 30
    topic, event_type = TOPIC_4P, EVENT_TYPE_4P
    partitions = range(4)
    interest = {
        "topic": topic,
        "subject_tenant": _LIVE_CHILD_TENANT,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    events = [
        {
            "id": f"29e20000-0000-4000-8001-{n:012x}",
            "type": event_type,
            "subject_tenant": _LIVE_CHILD_TENANT,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
            "data": {"n": n},
        }
        for n in range(1, 4)
    ]
    async with api() as client:
        bounds = await _live_manifest_bounds(client, topic, partitions)
        offsets = {p: bounds[p][0] for p in partitions}
        heads = {p: bounds[p][1] for p in partitions}
        began_at = datetime.now(timezone.utc).replace(microsecond=0)
        for event in events:
            published = await client.post("/events", json=event)
            _check_owner_request(published)
            assert (published.status_code, published.content) == (202, b"")

        group_id = await _live_group(client)
        sub_id = await _live_join(client, group_id, interest, partitions, agent="e2e-flow-consumer")
        await _live_seek_earliest(client, sub_id, topic, offsets)

        case_ids = {event["id"] for event in events}
        delivered = []
        last_seen = offsets.copy()
        async with _live_stream(api, sub_id) as response:
            assert response.headers["content-type"] == "text/event-stream"
            reader = SseFrameReader(response)
            opening = await _live_sse_frame(reader, min(deadline, clock() + 5))
            _live_topology(opening, topic, offsets)
            while len(delivered) < len(events):
                frame = await _live_sse_frame(reader, deadline)
                if frame["kind"] == "heartbeat":
                    _live_heartbeat(frame)
                    continue
                assert set(frame) == {"kind", "payload"}
                assert frame["kind"] == "event"
                payload = frame["payload"]
                assert isinstance(payload, dict)
                partition, sequence = payload["partition"], payload["sequence"]
                assert type(partition) is int
                assert type(sequence) is int
                assert partition in partitions
                assert sequence > last_seen[partition]
                last_seen[partition] = sequence
                if sequence <= heads[partition]:
                    assert payload["id"] not in case_ids
                    assert payload["type"] == event_type
                    assert payload["subject_tenant"] == _LIVE_CHILD_TENANT
                    assert payload["subject_type"] == SUBJECT_TYPE
                    assert set(payload) == {
                        "id", "type", "subject_tenant", "source", "subject", "subject_type",
                        "occurred_at", "trace_parent", "data", "partition", "sequence",
                        "sequence_time",
                    }
                    _stream_instant(payload["occurred_at"])
                    _stream_instant(payload["sequence_time"])
                    continue
                event = events[len(delivered)]
                _live_event_fields(frame, began_at)
                occurred_at = _stream_instant(payload["occurred_at"])
                assert occurred_at == _stream_instant(event["occurred_at"])
                assert frame == {
                    "kind": "event",
                    "payload": {
                        **event,
                        "occurred_at": payload["occurred_at"],
                        "trace_parent": None,
                        "partition": partition,
                        "sequence": sequence,
                        "sequence_time": payload["sequence_time"],
                    },
                }
                delivered.append((partition, sequence))

        (routed, first), (second_partition, second), (third_partition, checkpoint) = delivered
        assert (second_partition, third_partition) == (routed, routed)
        assert heads[routed] < first < second < checkpoint
        # A successful SEEK can only acquire the lease after the closed stream releases it.
        await _record_live_checkpoint(
            client, sub_id, topic, routed, checkpoint, min(deadline, clock() + 7),
        )
        left = await client.delete(f"/subscriptions/{sub_id}")
        _check_owner_request(left)
        assert (left.status_code, left.content) == (204, b"")
        replacement = await _live_join(
            client, group_id, interest, partitions, agent="e2e-flow-consumer",
        )
        assert replacement != sub_id

        resumed_offsets = {**offsets, routed: checkpoint}
        async with _live_stream(api, replacement, "stream") as response:
            frames = _resumed_multipart(response, min(deadline, clock() + 12))
            _live_topology(await anext(frames), topic, resumed_offsets)
            resumed_at = clock()
            _live_heartbeat(await anext(frames))
            idle_for = clock() - resumed_at
            await frames.aclose()
        assert idle_for > 0
        print(f"resumed multipart heartbeat after {idle_for:.6f}s")
