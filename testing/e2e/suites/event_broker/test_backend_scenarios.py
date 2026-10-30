"""Storage backend scenarios (feature 0007 §11), on whichever backend the
suite runs on (`EB_E2E_BACKEND`).

S1, S5 and retention (by age, by bytes, and down to no event at all) are
here. S3 (tail wake) is
`test_publish_consume.py::test_publish_then_consume_happy_path`, which opens
the stream at the tail and then publishes; S4 (crash after an answered
publish) is `test_restart_durability.py`, on a durable backend. S6 (a crash
between the backend commit and the outbox ack) has no e2e hook to stop the
process at that point; the gear's `redelivery_after_append_is_a_duplicate` and
the conformance suite's dedup scenarios cover it. S2 (`Prefer: wait`) answers
501 and has no test.
"""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    API_BASE,
    EVENT_TYPE_FRESH,
    EVENT_TYPE_ORDER,
    EVENT_TYPE_BYTES,
    EVENT_TYPE_DRAIN,
    EVENT_TYPE_RETENTION,
    RETENTION_DURATION_S,
    RETENTION_SEGMENT_EVENTS,
    SUBJECT_TYPE,
    TOPIC_FRESH,
    TOPIC_ORDER,
    TOPIC_BYTES,
    TOPIC_DRAIN,
    TOPIC_RETENTION,
    TRACE_ID,
    SseFrameReader,
)


async def _subscribe(client, topic: str, interests: list[dict]) -> str:
    group = await client.post("/consumer-groups")
    assert group.status_code == 201
    sub = await client.post(
        "/subscriptions",
        json={"consumer_group": group.json()["id"], "client_agent": "e2e-test", "interests": interests},
    )
    assert sub.status_code == 201
    return sub.json()["id"]


def _event(event_type: str, tenant_id: str, n: int) -> dict:
    return {
        "id": str(uuid.uuid4()),
        "type": event_type,
        "tenant_id": tenant_id,
        "source": "e2e-test",
        "subject": f"s{n}",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": datetime.now(timezone.utc).isoformat(),
        "data": {"n": n},
    }


@pytest.mark.backend_sensitive
async def test_events_of_one_key_arrive_in_publish_order_from_one_partition(api, test_env):
    """feature 0007 §11 S1 - key order.

    Events of two keys (the type partitions by tenant) are published
    interleaved on a four-partition topic; each key's events are delivered in
    publish order, all from one partition.
    """
    tenants = [str(uuid.uuid4()), str(uuid.uuid4())]
    async with api() as client:
        sub_id = await _subscribe(
            client,
            TOPIC_ORDER,
            [{"topic": TOPIC_ORDER, "tenant_id": t, "types": [EVENT_TYPE_ORDER]} for t in tenants],
        )
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1,
                "positions": {TOPIC_ORDER: [{"partition": p, "value": "latest"} for p in range(4)]},
            },
        )
        assert seek.status_code == 200

    async with httpx.AsyncClient(base_url=f"{test_env.base_url}/event-broker/v1", timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            await reader.await_kind("topology", timeout=5)

            async with api() as publish_client:
                for n in range(1, 4):
                    for tenant in tenants:
                        resp = await publish_client.post("/events", json=_event(EVENT_TYPE_ORDER, tenant, n))
                        assert resp.status_code == 202

            delivered = []
            for _ in range(6):
                body = await reader.await_kind("event", timeout=10)
                payload = body["payload"]
                delivered.append((payload["tenant_id"], payload["partition"], payload["data"]["n"]))

    for tenant in tenants:
        mine = [(partition, n) for t, partition, n in delivered if t == tenant]
        assert [n for _, n in mine] == [1, 2, 3], f"{tenant}: {mine}"
        assert len({partition for partition, _ in mine}) == 1, f"{tenant} split over partitions: {mine}"


@pytest.mark.backend_sensitive
async def test_a_consumer_joined_before_any_publish_receives_the_first_event(api, test_env):
    """feature 0007 §11 S5 - consumer before producer.

    A consumer joins a topic nothing was ever published to and seeks it
    earliest; the seek resolves to 0 and the first event published afterwards
    is delivered as sequence 1.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        sub_id = await _subscribe(
            client,
            TOPIC_FRESH,
            [{"topic": TOPIC_FRESH, "tenant_id": tenant_id, "types": [EVENT_TYPE_FRESH]}],
        )
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {TOPIC_FRESH: [{"partition": 0, "value": "earliest"}]}},
        )
        assert seek.status_code == 200
        assert seek.json() == {"positions": {TOPIC_FRESH: [{"partition": 0, "value": 0}]}}

    async with httpx.AsyncClient(base_url=f"{test_env.base_url}/event-broker/v1", timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            topology = await reader.await_kind("topology", timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [{"topic": TOPIC_FRESH, "partition": 0, "offset": 0, "last_examined": 0}],
            }

            event = _event(EVENT_TYPE_FRESH, tenant_id, 1)
            async with api() as publish_client:
                resp = await publish_client.post("/events", json=event)
                assert resp.status_code == 202

            payload = (await reader.await_kind("event", timeout=10))["payload"]
            assert payload == {
                "id": event["id"],
                "type": EVENT_TYPE_FRESH,
                "topic": TOPIC_FRESH,
                "tenant_id": tenant_id,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": {"n": 1},
                "partition": 0,
                "sequence": 1,
                "sequence_time": payload["sequence_time"],
            }


async def _read(test_env, sub_id: str, count: int) -> list[dict]:
    """The next `count` event payloads on `sub_id`'s stream, then closes it."""
    async with httpx.AsyncClient(base_url=f"{test_env.base_url}/event-broker/v1", timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            await reader.await_kind("topology", timeout=5)
            return [(await reader.await_kind("event", timeout=15))["payload"] for _ in range(count)]


async def _seek(client, sub_id: str, value, topic: str = TOPIC_RETENTION) -> object:
    resp = await client.post(
        f"/subscriptions/{sub_id}:seek",
        json={"topology_version": 1, "positions": {topic: [{"partition": 0, "value": value}]}},
    )
    return resp


async def _resolved(client, sub_id: str, value, topic: str) -> int:
    """What a SEEK of `value` on partition 0 of `topic` resolves to."""
    resp = await _seek(client, sub_id, value, topic)
    assert resp.status_code == 200, resp.text
    return resp.json()["positions"][topic][0]["value"]


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_retention_removes_the_events_past_their_age_and_keeps_the_rest(api, test_env):
    """Retention by age (feature 0007 §2.6), seen through SEEK and delivery.

    TOPIC_RETENTION keeps events RETENTION_DURATION_S. One storage segment's
    worth of events is published and left to age past that; one more is then
    published. The backend removes exactly the aged ones - by event or by whole
    segment, the old batch is one segment - and keeps the new one: "earliest"
    resolves at or past the last aged event's sequence and below the new one's
    (a backend with gaps may count a position between them as removed), a seek
    below it is refused, and a read from the start delivers the new event first.
    """
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_RETENTION, "tenant_id": tenant_id, "types": [EVENT_TYPE_RETENTION]}]
    async with api() as client:
        reader_sub = await _subscribe(client, TOPIC_RETENTION, interest)
        assert (await _seek(client, reader_sub, "earliest")).status_code == 200
        for start in range(0, RETENTION_SEGMENT_EVENTS, 100):
            batch = [_event(EVENT_TYPE_RETENTION, tenant_id, n) for n in range(start, start + 100)]
            resp = await client.post("/events:batch", json={"events": batch})
            assert resp.status_code == 202

    # Reading the batch back waits for it to be stored and gives the last
    # sequence and storage time, which is what its age runs from.
    old = await _read(test_env, reader_sub, RETENTION_SEGMENT_EVENTS)
    assert [e["data"]["n"] for e in old] == list(range(RETENTION_SEGMENT_EVENTS))
    last_old = old[-1]
    stored_at = datetime.fromisoformat(last_old["sequence_time"].replace("Z", "+00:00"))
    aged_for = (datetime.now(timezone.utc) - stored_at).total_seconds()
    await asyncio.sleep(max(0.0, RETENTION_DURATION_S - aged_for) + 1)

    fresh = _event(EVENT_TYPE_RETENTION, tenant_id, RETENTION_SEGMENT_EVENTS)
    async with api() as client:
        published_at = asyncio.get_running_loop().time()
        assert (await client.post("/events", json=fresh)).status_code == 202

        sub_id = await _subscribe(client, TOPIC_RETENTION, interest)
        # The fresh event ages out RETENTION_DURATION_S after it was stored, so
        # the floor must reach the old batch's end well before then.
        deadline = published_at + RETENTION_DURATION_S - 2
        while True:
            seek = await _seek(client, sub_id, "earliest")
            assert seek.status_code == 200
            floor = seek.json()["positions"][TOPIC_RETENTION][0]["value"]
            if floor >= last_old["sequence"]:
                break
            assert asyncio.get_running_loop().time() < deadline, (
                f"the floor is {floor}, not {last_old['sequence']}, "
                f"{RETENTION_DURATION_S}s after the old events aged out"
            )
            await asyncio.sleep(0.25)

    [delivered] = await _read(test_env, sub_id, 1)
    assert delivered == {
        "id": fresh["id"],
        "type": EVENT_TYPE_RETENTION,
        "topic": TOPIC_RETENTION,
        "tenant_id": tenant_id,
        "source": "e2e-test",
        "subject": fresh["subject"],
        "subject_type": SUBJECT_TYPE,
        "occurred_at": delivered["occurred_at"],
        "trace_parent": None,
        "data": {"n": RETENTION_SEGMENT_EVENTS},
        "partition": 0,
        "sequence": delivered["sequence"],
        "sequence_time": delivered["sequence_time"],
    }
    assert last_old["sequence"] <= floor < delivered["sequence"]

    async with api() as client:
        below_sub = await _subscribe(client, TOPIC_RETENTION, interest)
        below = await _seek(client, below_sub, 0)
    assert below.status_code == 400
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
                        f"topic {TOPIC_RETENTION} partition 0: the seek position is below the valid range "
                        f"[{floor}, {delivered['sequence']}]"
                    ),
                    "reason": "below_retention_floor",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def _publish(client, event_type: str, tenant_id: str, count: int, data=lambda n: {"n": n}) -> list[dict]:
    """Publish `count` events in batches of 100 (the batch limit); returns them."""
    events = []
    for start in range(0, count, 100):
        batch = []
        for n in range(start, min(start + 100, count)):
            event = _event(event_type, tenant_id, n)
            event["data"] = data(n)
            batch.append(event)
        resp = await client.post("/events:batch", json={"events": batch})
        assert resp.status_code == 202, resp.text
        events.extend(batch)
    return events


# Events the byte-bound test publishes, each carrying about a kilobyte: enough
# to fill two whole segments where a backend expires by segment (1000 events
# on PostgreSQL and MySQL, a megabyte on Kafka) and leave an open one behind.
BYTES_EVENTS = 2500
BYTES_PAD = "x" * 1000


@pytest.mark.backend_sensitive
@pytest.mark.timeout(90, func_only=True)
async def test_retention_by_bytes_keeps_a_suffix_within_the_bound(api, test_env):
    """Retention by bytes (feature 0007 §2.6), seen through SEEK and delivery.

    TOPIC_BYTES keeps at most RETENTION_BYTES per partition and has the default
    age. Many times that is published, crossing whole segments on the backends
    that remove by segment. The oldest events go - by event or by segment, as
    the backend's granularity allows - and what stays is an unbroken run of the
    newest ones: "earliest" moves above 0, and a read from it delivers the
    newest events in publish order through the last one.
    """
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_BYTES, "tenant_id": tenant_id, "types": [EVENT_TYPE_BYTES]}]
    async with api() as client:
        published = await _publish(
            client, EVENT_TYPE_BYTES, tenant_id, BYTES_EVENTS, data=lambda n: {"n": n, "pad": BYTES_PAD}
        )
        # Every publish has been accepted; wait for the bound to take effect.
        sub_id = await _subscribe(client, TOPIC_BYTES, interest)
        deadline = asyncio.get_running_loop().time() + 60
        while await _resolved(client, sub_id, "earliest", TOPIC_BYTES) == 0:
            assert asyncio.get_running_loop().time() < deadline, "nothing was removed past the byte bound"
            await asyncio.sleep(0.5)

    # The remaining run: read from the floor until the last published event.
    delivered = []
    async with httpx.AsyncClient(base_url=f"{test_env.base_url}/event-broker/v1", timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            await reader.await_kind("topology", timeout=5)
            while not delivered or delivered[-1]["id"] != published[-1]["id"]:
                delivered.append((await reader.await_kind("event", timeout=15))["payload"])

    # Removal may go on while the run is read (a backend removing one segment
    # per pass), so the reader can skip what goes under it. What must hold:
    # everything delivered is in publish order, and above the floor once the
    # read is done the run is unbroken through the last event.
    ids = [e["id"] for e in published]
    at = [ids.index(e["id"]) for e in delivered]
    assert at == sorted(at), "delivered out of publish order"
    async with api() as client:
        floor_sub = await _subscribe(client, TOPIC_BYTES, interest)
        final_floor = await _resolved(client, floor_sub, "earliest", TOPIC_BYTES)
    kept = [e["id"] for e in delivered if e["sequence"] > final_floor]
    assert 0 < len(kept) < BYTES_EVENTS
    assert kept == ids[-len(kept):]


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_latest_still_names_the_last_event_once_retention_removed_every_one(api, test_env):
    """Retention down to an empty partition (feature 0007 §2.4).

    TOPIC_DRAIN keeps events RETENTION_DURATION_S. One storage segment's worth
    is published - a whole segment, so the backends that expire by segment can
    remove all of it - and left to age out. Once nothing is stored, "earliest"
    and "latest" both resolve to the last event's sequence, never back to 0,
    and the next event published is delivered above it.
    """
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_DRAIN, "tenant_id": tenant_id, "types": [EVENT_TYPE_DRAIN]}]
    async with api() as client:
        reader_sub = await _subscribe(client, TOPIC_DRAIN, interest)
        assert (await _seek(client, reader_sub, "earliest", TOPIC_DRAIN)).status_code == 200
        await _publish(client, EVENT_TYPE_DRAIN, tenant_id, RETENTION_SEGMENT_EVENTS)
    stored = await _read(test_env, reader_sub, RETENTION_SEGMENT_EVENTS)
    head = stored[-1]["sequence"]

    async with api() as client:
        sub_id = await _subscribe(client, TOPIC_DRAIN, interest)
        deadline = asyncio.get_running_loop().time() + RETENTION_DURATION_S * 3
        while (floor := await _resolved(client, sub_id, "earliest", TOPIC_DRAIN)) != head:
            assert floor < head, f"the floor {floor} passed the head {head}"
            assert asyncio.get_running_loop().time() < deadline, (
                f"the floor is {floor}, not {head}, {RETENTION_DURATION_S * 3}s after publishing"
            )
            await asyncio.sleep(0.5)
        assert await _resolved(client, sub_id, "latest", TOPIC_DRAIN) == head

        fresh = _event(EVENT_TYPE_DRAIN, tenant_id, RETENTION_SEGMENT_EVENTS)
        assert (await client.post("/events", json=fresh)).status_code == 202

    [delivered] = await _read(test_env, sub_id, 1)
    assert delivered == {
        "id": fresh["id"],
        "type": EVENT_TYPE_DRAIN,
        "topic": TOPIC_DRAIN,
        "tenant_id": tenant_id,
        "source": "e2e-test",
        "subject": fresh["subject"],
        "subject_type": SUBJECT_TYPE,
        "occurred_at": delivered["occurred_at"],
        "trace_parent": None,
        "data": {"n": RETENTION_SEGMENT_EVENTS},
        "partition": 0,
        "sequence": delivered["sequence"],
        "sequence_time": delivered["sequence_time"],
    }
    assert delivered["sequence"] > head
