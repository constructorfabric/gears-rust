"""Storage backend scenarios (feature 0007 §11), on whichever backend the
suite runs on (`EB_E2E_BACKEND`).

S1, S5 and retention (by age, by bytes, and down to no event at all) are
here. S3 (tail wake) is
`test_publish_consume.py::test_publish_then_consume_happy_path`, which opens
the stream at the tail and then publishes; S4 (crash after an answered
publish) is `test_restart_durability.py`, on a durable backend. S6 (a crash
between the backend commit and the outbox ack) has no e2e hook to stop the
process at that point; the gear's `redelivery_after_append_is_a_duplicate` and
the conformance suite's dedup scenarios cover it. S2 (`Prefer: wait`) is
synchronous publish, covered by `test_producer_single.py` (`201` stored,
`202` on timeout, `200` duplicate).
"""

from __future__ import annotations

import asyncio
import contextlib
import re
import uuid
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    API_BASE,
    BACKEND,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_FRESH,
    EVENT_TYPE_ORDER,
    EVENT_TYPE_BYTES,
    EVENT_TYPE_DRAIN,
    EVENT_TYPE_RETENTION,
    EVENT_TYPE_SLOW_PUBLISH,
    REQUEST_TIMEOUT,
    RETENTION_DURATION_S,
    RETENTION_SEGMENT_EVENTS,
    SUBJECT_TYPE,
    TOPIC_FRESH,
    TOPIC_ORDER,
    TOPIC_BYTES,
    TOPIC_DRAIN,
    TOPIC_RETENTION,
    TOPIC_RETENTION_AGE_S,
    TOPIC_SLOW_PUBLISH,
    TRACE_ID,
    SseFrameReader,
)
from .test_consumer_subscriptions import _created_subscription_id


async def _subscribe(client, topic: str, interests: list[dict], partitions=range(1)) -> str:
    group = await client.post("/consumer-groups")
    assert (group.status_code, group.headers.get("content-type")) == (201, "application/json")
    location = group.headers["location"]
    group_prefix = f"{API_BASE}/consumer-groups/"
    group_id = location.removeprefix(group_prefix)
    gts_prefix = "gts.cf.core.events.consumer_group.v1~"
    assert group_id.startswith(gts_prefix)
    group_uuid = group_id.removeprefix(gts_prefix)
    assert str(uuid.UUID(group_uuid)) == group_uuid
    assert location == f"{group_prefix}{group_id}"
    group_body = group.json()
    assert isinstance(group_body, dict)
    _stream_instant(group_body["created_at"])
    assert group_body == {
        "id": group_id,
        "kind": "anonymous",
        "tenant": DEFAULT_TENANT_ID,
        "owner_principal": DEFAULT_SUBJECT_ID,
        "description": None,
        "created_at": group_body["created_at"],
    }

    sub = await client.post(
        "/subscriptions",
        json={"consumer_group": group_id, "client_agent": "e2e-test", "interests": interests},
    )
    assert (sub.status_code, sub.headers.get("content-type")) == (201, "application/json")
    sub_id = _created_subscription_id(sub)
    body = sub.json()
    assert isinstance(body, dict)
    _stream_instant(body["created_at"])
    assert body == {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {**interest, "barrier_mode": "respect", "filter": None, "unmatched_types": []}
            for interest in interests
        ],
        "assigned": [{"topic": topic, "partition": p} for p in partitions],
        "topology_version": 1,
        "created_at": body["created_at"],
    }
    assert type(body["topology_version"]) is int
    assert all(type(entry["partition"]) is int for entry in body["assigned"])
    return sub_id


def _interest(topic: str, event_type: str, tenant_id: str = DEFAULT_TENANT_ID) -> dict:
    return {
        "topic": topic,
        "subject_tenant": tenant_id,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }


def _event(case: int, event_type: str, n: int, tenant_id: str = DEFAULT_TENANT_ID) -> dict:
    return {
        "id": f"16e20000-0000-4000-8000-{case:02d}{tenant_id[-4:]}{n:06d}",
        "type": event_type,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": f"s{n}",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-19T10:20:30.123456789Z",
        "data": {"n": n},
    }


@pytest.mark.backend_sensitive
async def test_events_of_one_key_arrive_in_publish_order_from_one_partition(api, test_env):
    """feature 0007 §11 S1 - key order.

    The type routes by subject_tenant, so the two interests select distinct
    keys even if the backend maps them to the same partition.
    """
    tenants = [DEFAULT_TENANT_ID, "00000000-0000-0000-0000-000000000004"]
    partitions = range(4)
    interests = [_interest(TOPIC_ORDER, EVENT_TYPE_ORDER, tenant) for tenant in tenants]
    published = [
        _event(23, EVENT_TYPE_ORDER, n, tenant)
        for n in range(1, 4)
        for tenant in tenants
    ]
    async with api() as client:
        sub_id = await _subscribe(client, TOPIC_ORDER, interests, partitions)
        seek = await _seek(client, sub_id, "latest", TOPIC_ORDER, partitions)
        starts = _positions(seek, TOPIC_ORDER, partitions)
        assert starts == {0: 0, 1: 0, 2: 0, 3: 0}

        async with _stream(api, sub_id, TOPIC_ORDER, starts) as stream:
            for event in published:
                resp = await client.post("/events", json=event)
                assert (resp.status_code, resp.headers.get("content-type"), resp.content) == (
                    202, None, b"",
                )
            delivered = await stream.take(len(published), wait_s=10)

    by_id = {event["id"]: event for event in published}
    for payload in delivered:
        assert payload["id"] in by_id
        event = by_id[payload["id"]]
        partition = payload["partition"]
        assert partition in partitions
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        assert payload == {
            **event,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "partition": partition,
            "sequence": payload["sequence"],
            "sequence_time": payload["sequence_time"],
        }

    for tenant in tenants:
        mine = [payload for payload in delivered if payload["subject_tenant"] == tenant]
        assert [payload["data"]["n"] for payload in mine] == [1, 2, 3], mine
        assert len({payload["partition"] for payload in mine}) == 1, mine
    last = starts.copy()
    for payload in delivered:
        partition = payload["partition"]
        assert payload["sequence"] > last[partition]
        last[partition] = payload["sequence"]


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


@pytest.mark.backend_sensitive
async def test_a_consumer_joined_before_any_publish_receives_the_first_event(api, test_env):
    """feature 0007 §11 S5 - consumer before producer.

    A consumer joins a topic nothing was ever published to and seeks it
    earliest; the seek resolves to 0 and the first event published afterwards
    is delivered as sequence 1.
    """
    topic = TOPIC_FRESH
    position = "earliest"
    interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_FRESH],
        },
    ]
    unmatched_types = [[]]
    event = {
        "id": "16e20000-0000-4000-8000-000000240001",
        "type": EVENT_TYPE_FRESH,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-19T10:20:30Z",
        "data": {"n": 1},
    }

    async with api() as client, api() as stream_client:
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
        start = 0
        assert seek.json() == {"positions": {topic: [{"partition": 0, "value": start}]}}
        before = await client.get("/topics/segments", params={"topic": topic, "partition": 0})
        assert (before.status_code, before.headers.get("content-type")) == (200, "application/json")
        assert before.json() == {
            "topic": topic,
            "partition": 0,
            "start_sequence": 0,
            "end_sequence": 0,
            "start_time": None,
            "end_time": None,
            "segments": [],
        }
        async with stream_client.stream(
            "GET", "/events:sse", params={"subscription": sub_id},
            headers={"Accept": "text/event-stream"},
        ) as response:
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

            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")
            deadline = asyncio.get_running_loop().time() + 10
            frame = await _read_sse_event(reader, deadline)
            assert set(frame) == {"kind", "payload"}, frame

        after = await client.get("/topics/segments", params={"topic": topic, "partition": 0})
    assert (after.status_code, after.headers.get("content-type")) == (200, "application/json")
    manifest = after.json()
    segments = manifest.pop("segments")
    assert isinstance(segments, list)
    assert manifest == {
        "topic": topic,
        "partition": 0,
        "start_sequence": 1,
        "end_sequence": 1,
        "start_time": frame["payload"]["sequence_time"],
        "end_time": frame["payload"]["sequence_time"],
    }
    if BACKEND == "memory":
        assert segments == [{"start_sequence": 1, "end_sequence": 1, "event_count": 1}]
        assert all(type(value) is int for value in segments[0].values())
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
            "sequence": 1,
            "sequence_time": payload["sequence_time"],
        },
    }


EVENT_WAIT_S = 15


class _Stream:
    def __init__(self, reader: SseFrameReader):
        self._reader = reader
        self.started_at = None
        self.first_event_at = None

    async def next_event(self, deadline: float) -> dict:
        frame = await _read_sse_event(self._reader, deadline)
        assert frame.keys() == {"kind", "payload"}, frame
        payload = frame["payload"]
        assert isinstance(payload, dict), payload
        assert {"id", "occurred_at", "sequence_time", "sequence", "partition", "data"} <= (
            payload.keys()
        ), payload
        assert type(payload["sequence"]) is int, payload
        assert payload["sequence"] > 0, payload
        assert type(payload["partition"]) is int, payload
        assert isinstance(payload["data"], dict), payload
        assert type(payload["data"].get("n")) is int, payload
        _stream_instant(payload["sequence_time"])
        return payload

    async def take(self, count: int, wait_s: float = EVENT_WAIT_S) -> list[dict]:
        clock = asyncio.get_running_loop().time
        self.started_at = clock()
        events: list[dict] = []
        while len(events) < count:
            try:
                events.append(await self.next_event(clock() + wait_s))
            except (TimeoutError, httpx.ReadTimeout, StopAsyncIteration) as stalled:
                raise AssertionError(
                    f"{_short_read(events, count, clock() - self.started_at)} ({stalled})"
                ) from stalled
            if len(events) == 1:
                self.first_event_at = clock()
        return events

    async def take_during(self, count: int, publishing) -> list[dict]:
        # A reader stalled behind the publisher can miss events once they expire.
        reading = asyncio.create_task(self.take(count))
        writing = asyncio.create_task(publishing)
        try:
            events, _ = await asyncio.gather(reading, writing)
            return events
        finally:
            reading.cancel()
            writing.cancel()
            await asyncio.gather(reading, writing, return_exceptions=True)


@contextlib.asynccontextmanager
async def _stream(api, sub_id: str, topic: str, starts: dict[int, int]):
    timeout = httpx.Timeout(REQUEST_TIMEOUT, read=EVENT_WAIT_S)
    async with api() as client:
        async with client.stream(
            "GET", "/events:sse", params={"subscription": sub_id},
            headers={"Accept": "text/event-stream"}, timeout=timeout,
        ) as response:
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
                    {"topic": topic, "partition": p, "offset": offset, "last_examined": offset}
                    for p, offset in starts.items()
                ],
            }
            assert type(topology["topology_version"]) is int
            for entry in topology["assigned"]:
                assert type(entry["partition"]) is int
                assert type(entry["offset"]) is int
                assert type(entry["last_examined"]) is int
            yield _Stream(reader)


def _short_read(events: list[dict], count: int, elapsed: float) -> str:
    if not events:
        return f"0 of {count} events delivered in {elapsed:.1f}s"
    first, last = events[0], events[-1]
    return (
        f"{len(events)} of {count} events delivered in {elapsed:.1f}s: "
        f"sequences {first['sequence']}..{last['sequence']}, "
        f"data.n {first['data']['n']}..{last['data']['n']}, "
        f"first stored at {first['sequence_time']}, last at {last['sequence_time']}"
    )


async def _seek(client, sub_id: str, value, topic: str, partitions=range(1)) -> httpx.Response:
    return await client.post(
        f"/subscriptions/{sub_id}:seek",
        json={
            "topology_version": 1,
            "positions": {topic: [{"partition": p, "value": value} for p in partitions]},
        },
    )


def _positions(response: httpx.Response, topic: str, partitions=range(1)) -> dict[int, int]:
    assert (response.status_code, response.headers.get("content-type")) == (200, "application/json")
    body = response.json()
    assert isinstance(body, dict), body
    assert body.keys() == {"positions"}, body
    assert isinstance(body["positions"], dict), body
    assert body["positions"].keys() == {topic}, body
    entries = body["positions"][topic]
    assert isinstance(entries, list), body
    assert len(entries) == len(partitions), body
    offsets = {}
    for partition, entry in zip(partitions, entries, strict=True):
        assert isinstance(entry, dict), body
        assert entry.keys() == {"partition", "value"}, entry
        assert type(entry["partition"]) is int, entry
        assert entry["partition"] == partition, entry
        assert type(entry["value"]) is int, entry
        assert entry["value"] >= 0, entry
        offsets[partition] = entry["value"]
    return offsets


async def _resolved(client, sub_id: str, value, topic: str) -> int:
    return _positions(await _seek(client, sub_id, value, topic), topic)[0]


# The floor poll leaves at least 12 - 6 = 6 s of the fresh event's age
# budget for opening its stream and reading it.
FLOOR_POLL_S = 6


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_retention_removes_the_events_past_their_age_and_keeps_the_rest(api, test_env):
    """Retention by storage age (feature 0007 §2.6) and the exclusive seek floor.

    The old batch fills one segment on backends that expire whole segments.
    Reading during publication keeps the read delay within the age budget.
    """
    interest = [_interest(TOPIC_RETENTION, EVENT_TYPE_RETENTION)]
    published = [_event(25, EVENT_TYPE_RETENTION, n) for n in range(RETENTION_SEGMENT_EVENTS)]
    async with api() as client:
        reader_sub = await _subscribe(client, TOPIC_RETENTION, interest)
        start = await _resolved(client, reader_sub, "earliest", TOPIC_RETENTION)
        assert start == 0
        async with _stream(api, reader_sub, TOPIC_RETENTION, {0: start}) as stream:
            old = await stream.take_during(len(published), _publish(client, published))

        assert [payload["data"]["n"] for payload in old] == list(range(RETENTION_SEGMENT_EVENTS))
        last = start
        for payload, event in zip(old, published, strict=True):
            assert payload["sequence"] > last
            last = payload["sequence"]
            assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
            assert payload == {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "partition": 0,
                "sequence": last,
                "sequence_time": payload["sequence_time"],
            }
        last_old = old[-1]
        stored_at, nanos = _stream_instant(last_old["sequence_time"])
        aged_for = (datetime.now(timezone.utc) - stored_at).total_seconds() - nanos / 1e9
        await asyncio.sleep(max(0.0, TOPIC_RETENTION_AGE_S - aged_for) + 1)

        fresh = _event(25, EVENT_TYPE_RETENTION, RETENTION_SEGMENT_EVENTS)
        deadline = asyncio.get_running_loop().time() + FLOOR_POLL_S
        async with asyncio.timeout_at(deadline):
            response = await client.post("/events", json=fresh)
            assert (
                response.status_code, response.headers.get("content-type"), response.content,
            ) == (202, None, b"")
            sub_id = await _subscribe(client, TOPIC_RETENTION, interest)
            while (floor := await _resolved(client, sub_id, "earliest", TOPIC_RETENTION)) < last:
                await asyncio.sleep(0.25)

        async with _stream(api, sub_id, TOPIC_RETENTION, {0: floor}) as stream:
            [delivered] = await stream.take(1)
        assert last_old["sequence"] <= floor
        assert floor < delivered["sequence"]
        assert _stream_instant(delivered["occurred_at"]) == _stream_instant(fresh["occurred_at"])
        assert delivered == {
            **fresh,
            "occurred_at": delivered["occurred_at"],
            "trace_parent": None,
            "partition": 0,
            "sequence": delivered["sequence"],
            "sequence_time": delivered["sequence_time"],
        }

        below_sub = await _subscribe(client, TOPIC_RETENTION, interest)
        below = await _seek(client, below_sub, 0, TOPIC_RETENTION)
    assert (below.status_code, below.headers.get("content-type")) == (
        400, "application/problem+json",
    )
    assert below.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/subscriptions/{below_sub}:seek",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "positions",
                    "description": (
                        f"topic {TOPIC_RETENTION} partition 0: the seek position is below the "
                        f"valid range [{floor}, {delivered['sequence']}]"
                    ),
                    "reason": "below_retention_floor",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def _publish(
    client,
    events: list[dict],
    pause_s: float = 0,
    timeline: list[tuple[float, datetime]] | None = None,
) -> None:
    for start in range(0, len(events), 100):
        if timeline is not None:
            timeline.append((asyncio.get_running_loop().time(), datetime.now(timezone.utc)))
        resp = await client.post("/events:batch", json={"events": events[start:start + 100]})
        assert (resp.status_code, resp.headers.get("content-type"), resp.content) == (
            202, None, b"",
        )
        if pause_s:
            await asyncio.sleep(pause_s)


# The pad crosses whole segments for backends that expire by segment.
BYTES_EVENTS = 2500
BYTES_PAD = "x" * 1000


BYTES_TIMEOUT_S = 90
# Leave room for the independent final seek after the publish, floor poll and read.
BYTES_FINAL_S = 5


@pytest.mark.backend_sensitive
@pytest.mark.timeout(BYTES_TIMEOUT_S, func_only=True)
async def test_retention_by_bytes_keeps_a_suffix_within_the_bound(api, test_env):
    """Retention by bytes (feature 0007 §2.6) keeps a suffix through the head."""
    clock = asyncio.get_running_loop().time
    deadline = clock() + BYTES_TIMEOUT_S - BYTES_FINAL_S
    interest = [_interest(TOPIC_BYTES, EVENT_TYPE_BYTES)]
    published = [
        {**_event(26, EVENT_TYPE_BYTES, n), "data": {"n": n, "pad": BYTES_PAD}}
        for n in range(BYTES_EVENTS)
    ]
    index_by_id = {event["id"]: n for n, event in enumerate(published)}
    delivered = []
    async with api() as client:
        async with asyncio.timeout_at(deadline):
            await _publish(client, published)
            sub_id = await _subscribe(client, TOPIC_BYTES, interest)
            while (floor := await _resolved(client, sub_id, "earliest", TOPIC_BYTES)) == 0:
                await asyncio.sleep(0.5)

            read_started = clock()
            async with _stream(api, sub_id, TOPIC_BYTES, {0: floor}) as stream:
                last_index = -1
                sequence = floor
                while True:
                    try:
                        payload = await stream.next_event(min(deadline, clock() + EVENT_WAIT_S))
                    except (TimeoutError, httpx.ReadTimeout, StopAsyncIteration) as stalled:
                        raise AssertionError(
                            f"the read stopped at n={last_index} before {BYTES_EVENTS - 1}: "
                            f"{len(delivered)} events in {clock() - read_started:.1f}s"
                        ) from stalled
                    assert payload["id"] in index_by_id, payload
                    index = index_by_id[payload["id"]]
                    assert index > last_index, (last_index, index)
                    assert payload["sequence"] > sequence, payload
                    event = published[index]
                    assert _stream_instant(payload["occurred_at"]) == _stream_instant(
                        event["occurred_at"]
                    )
                    assert payload == {
                        **event,
                        "occurred_at": payload["occurred_at"],
                        "trace_parent": None,
                        "partition": 0,
                        "sequence": payload["sequence"],
                        "sequence_time": payload["sequence_time"],
                    }
                    delivered.append(payload)
                    last_index = index
                    sequence = payload["sequence"]
                    if index == BYTES_EVENTS - 1:
                        break

        floor_sub = await _subscribe(client, TOPIC_BYTES, interest)
        final_floor = await _resolved(client, floor_sub, "earliest", TOPIC_BYTES)
    # A segment can expire during the read. Only events above the final floor
    # must still form an unbroken suffix.
    assert final_floor >= floor
    kept = [
        index_by_id[payload["id"]] for payload in delivered if payload["sequence"] > final_floor
    ]
    assert kept
    assert len(kept) < BYTES_EVENTS
    assert kept == list(range(BYTES_EVENTS - len(kept), BYTES_EVENTS))


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_latest_still_names_the_last_event_once_retention_removed_every_one(api, test_env):
    """Retention down to an empty partition (feature 0007 §2.4).

    The batch fills one storage segment so whole-segment retention can
    remove it. Once empty, the partition retains the position of its head.
    """
    clock = asyncio.get_running_loop().time
    interest = [_interest(TOPIC_DRAIN, EVENT_TYPE_DRAIN)]
    published = [_event(27, EVENT_TYPE_DRAIN, n) for n in range(RETENTION_SEGMENT_EVENTS)]
    async with api() as client:
        reader_sub = await _subscribe(client, TOPIC_DRAIN, interest)
        start = await _resolved(client, reader_sub, "earliest", TOPIC_DRAIN)
        assert start == 0
        async with _stream(api, reader_sub, TOPIC_DRAIN, {0: start}) as stream:
            stored = await stream.take_during(len(published), _publish(client, published))
            deadline = clock() + RETENTION_DURATION_S * 3

        assert [payload["data"]["n"] for payload in stored] == list(range(RETENTION_SEGMENT_EVENTS))
        head = start
        for payload, event in zip(stored, published, strict=True):
            assert payload["sequence"] > head
            head = payload["sequence"]
            assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
            assert payload == {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "partition": 0,
                "sequence": head,
                "sequence_time": payload["sequence_time"],
            }

        async with asyncio.timeout_at(deadline):
            sub_id = await _subscribe(client, TOPIC_DRAIN, interest)
            while (floor := await _resolved(client, sub_id, "earliest", TOPIC_DRAIN)) != head:
                assert floor < head, (floor, head)
                await asyncio.sleep(0.5)
        assert await _resolved(client, sub_id, "latest", TOPIC_DRAIN) == head

        fresh = _event(27, EVENT_TYPE_DRAIN, RETENTION_SEGMENT_EVENTS)
        response = await client.post("/events", json=fresh)
        assert (response.status_code, response.headers.get("content-type"), response.content) == (
            202, None, b"",
        )
        async with _stream(api, sub_id, TOPIC_DRAIN, {0: head}) as stream:
            [delivered] = await stream.take(1)
    assert delivered["sequence"] > head
    assert _stream_instant(delivered["occurred_at"]) == _stream_instant(fresh["occurred_at"])
    assert delivered == {
        **fresh,
        "occurred_at": delivered["occurred_at"],
        "trace_parent": None,
        "partition": 0,
        "sequence": delivered["sequence"],
        "sequence_time": delivered["sequence_time"],
    }


SLOW_PUBLISH_PAUSE_S = 1


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_a_batch_published_for_longer_than_its_age_bound_is_read_back_whole(api, test_env):
    """Read live through a publish longer than the storage age bound (feature 0007 §2.6)."""
    interest = [_interest(TOPIC_SLOW_PUBLISH, EVENT_TYPE_SLOW_PUBLISH)]
    published = [_event(28, EVENT_TYPE_SLOW_PUBLISH, n) for n in range(RETENTION_SEGMENT_EVENTS)]
    timeline: list[tuple[float, datetime]] = []
    async with api() as client:
        sub_id = await _subscribe(client, TOPIC_SLOW_PUBLISH, interest)
        start = await _resolved(client, sub_id, "earliest", TOPIC_SLOW_PUBLISH)
        assert start == 0
        async with _stream(api, sub_id, TOPIC_SLOW_PUBLISH, {0: start}) as stream:
            delivered = await stream.take_during(
                len(published),
                _publish(client, published, pause_s=SLOW_PUBLISH_PAUSE_S, timeline=timeline),
            )
    assert [payload["data"]["n"] for payload in delivered] == list(range(RETENTION_SEGMENT_EVENTS))
    last = start
    for payload, event in zip(delivered, published, strict=True):
        assert payload["sequence"] > last
        last = payload["sequence"]
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        assert payload == {
            **event,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "partition": 0,
            "sequence": last,
            "sequence_time": payload["sequence_time"],
        }

    assert len(timeline) == 10
    first_batch, _ = timeline[0]
    last_batch, last_batch_wall = timeline[-1]
    assert stream.started_at <= first_batch
    assert stream.first_event_at < last_batch
    assert last_batch - first_batch > RETENTION_DURATION_S
    stored_at, nanos = _stream_instant(delivered[0]["sequence_time"])
    first_age = (last_batch_wall - stored_at).total_seconds() - nanos / 1e9
    assert first_age > RETENTION_DURATION_S
