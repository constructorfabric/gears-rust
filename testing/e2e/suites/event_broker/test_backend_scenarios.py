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
import contextlib
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
    EVENT_TYPE_SLOW_PUBLISH,
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


# How long a read waits for each next event. `POST /events` acks with 202
# before the outbox has stored the event, so a read-back waits on the drain:
# its rate is ~80 events/s on a debug build (1000 events stored over 12-15 s
# locally, idle or at load average 100+), so consecutive events arrive tens of
# milliseconds apart and 15 s only trips on a stream that has stopped.
EVENT_WAIT_S = 15


class _Stream:
    """An open SSE stream on one subscription, read `count` events at a time."""

    def __init__(self, reader: SseFrameReader):
        self._reader = reader

    async def take(self, count: int) -> list[dict]:
        """The next `count` event payloads.

        A stream that stops short fails with what it did deliver - how many
        events, the first and last sequence and `data.n`, and how long the read
        ran - so a short read because events were removed before they were
        read (`n` starts late or skips) is told apart from a stall.
        """
        loop = asyncio.get_running_loop()
        started = loop.time()
        events: list[dict] = []
        while len(events) < count:
            try:
                events.append((await self._reader.await_kind("event", timeout=EVENT_WAIT_S))["payload"])
            except (AssertionError, TimeoutError) as stalled:
                raise AssertionError(
                    f"{_short_read(events, count, loop.time() - started)} ({type(stalled).__name__}: {stalled})"
                ) from stalled
        return events

    async def take_during(self, count: int, publishing) -> list[dict]:
        """The next `count` event payloads, read while `publishing` runs.

        Read concurrently, not once `publishing` is done: frames left unread
        fill the socket buffers, the server then stops reading the backend, and
        what it has not read yet keeps ageing as if no one were reading.

        A plain task rather than a `TaskGroup`, so a failure surfaces as itself
        and not as an `ExceptionGroup` that hides its message in the summary.
        """
        reading = asyncio.ensure_future(self.take(count))
        try:
            await publishing
        except BaseException:
            reading.cancel()
            with contextlib.suppress(BaseException):
                await reading
            raise
        return await reading


@contextlib.asynccontextmanager
async def _stream(test_env, sub_id: str):
    """`sub_id`'s SSE stream, open once its topology frame has arrived.

    Open it before publishing what it is to read back: it then delivers each
    event as soon as the outbox stores it, so an event's age when read is the
    stream's lag behind storage, not that plus however long the publish took.
    """
    async with httpx.AsyncClient(base_url=f"{test_env.base_url}/event-broker/v1", timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            await reader.await_kind("topology", timeout=5)
            yield _Stream(reader)


async def _read(test_env, sub_id: str, count: int) -> list[dict]:
    """The next `count` event payloads on `sub_id`'s stream, then closes it."""
    async with _stream(test_env, sub_id) as stream:
        return await stream.take(count)


def _short_read(events: list[dict], count: int, elapsed: float) -> str:
    """What a read that stopped short delivered, for its failure message."""
    if not events:
        return f"0 of {count} events delivered in {elapsed:.1f}s"
    first, last = events[0], events[-1]
    return (
        f"{len(events)} of {count} events delivered in {elapsed:.1f}s: "
        f"sequences {first['sequence']}..{last['sequence']}, "
        f"data.n {(first.get('data') or {}).get('n')}..{(last.get('data') or {}).get('n')}, "
        f"first stored at {first['sequence_time']}, last at {last['sequence_time']}"
    )


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


# How long the age test polls for the floor to pass the old batch, from the
# fresh event's publish. Not derived from the age bound, so the read after it
# keeps TOPIC_RETENTION_AGE_S - FLOOR_POLL_S before the fresh event ages out.
FLOOR_POLL_S = 6


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_retention_removes_the_events_past_their_age_and_keeps_the_rest(api, test_env):
    """Retention by age (feature 0007 §2.6), seen through SEEK and delivery.

    TOPIC_RETENTION keeps events TOPIC_RETENTION_AGE_S. One storage segment's
    worth of events is published and left to age past that; one more is then
    published. The backend removes exactly the aged ones - by event or by whole
    segment, the old batch is one segment - and keeps the new one: "earliest"
    resolves at or past the last aged event's sequence and below the new one's
    (a backend with gaps may count a position between them as removed), a seek
    below it is refused, and a read from the start delivers the new event first.

    Margins (memory backend, debug build; sweeps run every second, so an event
    goes at an age between TOPIC_RETENTION_AGE_S and one second more):

    - Read-back of the old batch: the stream is open and read while the batch
      is published, so an event's age when read is the stream's lag behind
      the outbox - at most 0.06 s measured, against an age bound of 8 s, also
      with the publish slowed to 11 s. Read only after the publish, as it
      once was, the front of the batch aged out before the stream opened
      whenever the publish went on longer than the age bound after the first
      event was stored, and the read then waited for events that were gone;
      test_a_batch_published_for_longer_than_its_age_bound_is_read_back_whole
      fails every time on that shape on the memory backend.
    - The batch is stored at the outbox's rate, ~80 events/s, so the read-back
      takes 12-15 s; consecutive events arrived at most 0.5 s apart, against
      EVENT_WAIT_S of 15 s.
    - The floor deadline, `published_at + FLOOR_POLL_S`: the fresh event is
      stored after its POST, so it ages out no earlier than
      `published_at + TOPIC_RETENTION_AGE_S`, which leaves the read after the
      poll at least TOPIC_RETENTION_AGE_S - FLOOR_POLL_S = 12 - 6 = 6 s for a
      stream open and one stored event. The deadline does not follow the age:
      `published_at + age - 2` leaves 2 s at any age.
    - The 60 s bound: the read-back (12-15 s), the age wait (at most
      TOPIC_RETENTION_AGE_S + 1 = 13 s), the floor poll (at most FLOOR_POLL_S
      = 6 s) and one read, about 35 s; in CI the whole test took 22-26 s in
      the 15 green runs of 2026-11-04, with an age bound 4 s shorter.
    """
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_RETENTION, "tenant_id": tenant_id, "types": [EVENT_TYPE_RETENTION]}]
    async with api() as client:
        reader_sub = await _subscribe(client, TOPIC_RETENTION, interest)
        assert (await _seek(client, reader_sub, "earliest")).status_code == 200
    # Reading the batch back waits for it to be stored and gives the last
    # sequence and storage time, which is what its age runs from. The stream
    # opens first: the outbox stores the batch while it is still being
    # published, and a read opened only afterwards could find its front
    # already removed.
    async with _stream(test_env, reader_sub) as stream, api() as client:
        old = await stream.take_during(
            RETENTION_SEGMENT_EVENTS,
            _publish(client, EVENT_TYPE_RETENTION, tenant_id, RETENTION_SEGMENT_EVENTS),
        )
    assert [e["data"]["n"] for e in old] == list(range(RETENTION_SEGMENT_EVENTS))
    last_old = old[-1]
    stored_at = datetime.fromisoformat(last_old["sequence_time"].replace("Z", "+00:00"))
    aged_for = (datetime.now(timezone.utc) - stored_at).total_seconds()
    await asyncio.sleep(max(0.0, TOPIC_RETENTION_AGE_S - aged_for) + 1)

    fresh = _event(EVENT_TYPE_RETENTION, tenant_id, RETENTION_SEGMENT_EVENTS)
    async with api() as client:
        published_at = asyncio.get_running_loop().time()
        assert (await client.post("/events", json=fresh)).status_code == 202

        sub_id = await _subscribe(client, TOPIC_RETENTION, interest)
        # The fresh event ages out TOPIC_RETENTION_AGE_S after it was stored,
        # so the floor must reach the old batch's end well before then.
        deadline = published_at + FLOOR_POLL_S
        while True:
            seek = await _seek(client, sub_id, "earliest")
            assert seek.status_code == 200
            floor = seek.json()["positions"][TOPIC_RETENTION][0]["value"]
            if floor >= last_old["sequence"]:
                break
            assert asyncio.get_running_loop().time() < deadline, (
                f"the floor is {floor}, not {last_old['sequence']}, "
                f"{FLOOR_POLL_S}s after the fresh event was published"
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


async def _publish(
    client, event_type: str, tenant_id: str, count: int, data=lambda n: {"n": n}, pause_s: float = 0
) -> list[dict]:
    """Publish `count` events in batches of 100 (the batch limit), pausing
    `pause_s` after each; returns them."""
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
        if pause_s:
            await asyncio.sleep(pause_s)
    return events


# Events the byte-bound test publishes, each carrying about a kilobyte: enough
# to fill two whole segments where a backend expires by segment (1000 events
# on PostgreSQL and MySQL, a megabyte on Kafka) and leave an open one behind.
BYTES_EVENTS = 2500
BYTES_PAD = "x" * 1000


BYTES_TIMEOUT_S = 90
# What the byte-bound test keeps of BYTES_TIMEOUT_S past its own deadline (the
# final seek and the pytest timeout's slack), so that deadline fires first and
# says which phase ran out.
BYTES_FINAL_S = 5


@pytest.mark.backend_sensitive
@pytest.mark.timeout(BYTES_TIMEOUT_S, func_only=True)
async def test_retention_by_bytes_keeps_a_suffix_within_the_bound(api, test_env):
    """Retention by bytes (feature 0007 §2.6), seen through SEEK and delivery.

    TOPIC_BYTES keeps at most RETENTION_BYTES per partition and has the default
    age. Many times that is published, crossing whole segments on the backends
    that remove by segment. The oldest events go - by event or by segment, as
    the backend's granularity allows - and what stays is an unbroken run of the
    newest ones: "earliest" moves above 0, and a read from it delivers the
    newest events in publish order through the last one.

    Budget: one deadline, BYTES_FINAL_S short of BYTES_TIMEOUT_S from the
    start, bounds the publish, the floor poll and the read together, so the
    run fails on its own message rather than on the pytest timeout. The read
    ends with the last of the 2500 events, which is stored at the outbox's
    rate (~80 events/s): in CI the whole test took 30-40 s in the 19 runs of
    2026-11-04, under half of the 85 s the deadline gives.
    """
    loop = asyncio.get_running_loop()
    started = loop.time()
    deadline = started + BYTES_TIMEOUT_S - BYTES_FINAL_S
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_BYTES, "tenant_id": tenant_id, "types": [EVENT_TYPE_BYTES]}]
    async with api() as client:
        published = await _publish(
            client, EVENT_TYPE_BYTES, tenant_id, BYTES_EVENTS, data=lambda n: {"n": n, "pad": BYTES_PAD}
        )
        # Every publish has been accepted; wait for the bound to take effect.
        sub_id = await _subscribe(client, TOPIC_BYTES, interest)
        while await _resolved(client, sub_id, "earliest", TOPIC_BYTES) == 0:
            assert loop.time() < deadline, "nothing was removed past the byte bound"
            await asyncio.sleep(0.5)

    # The remaining run: read from the floor until the last published event.
    delivered = []
    read_started = loop.time()
    async with httpx.AsyncClient(base_url=f"{test_env.base_url}/event-broker/v1", timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            await reader.await_kind("topology", timeout=5)
            while not delivered or delivered[-1]["id"] != published[-1]["id"]:
                remaining = min(EVENT_WAIT_S, deadline - loop.time())
                try:
                    delivered.append((await reader.await_kind("event", timeout=remaining))["payload"])
                except (AssertionError, TimeoutError) as stalled:
                    last_n = delivered[-1]["data"]["n"] if delivered else None
                    raise AssertionError(
                        f"the read from the floor stopped at data.n {last_n}, before the last published "
                        f"event ({BYTES_EVENTS - 1}): {len(delivered)} events in "
                        f"{loop.time() - read_started:.1f}s ({type(stalled).__name__}: {stalled})"
                    ) from stalled

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

    Margins (see the age test for the read-back's): the floor deadline, three
    times RETENTION_DURATION_S from the end of the read-back, is against the
    last event's removal at most RETENTION_DURATION_S + 1 s after it was
    stored, and the last event is stored about when the read-back ends, so
    its removal is due 8-9 s into the 24 s deadline. The 60 s bound covers the
    read-back (12-15 s), that deadline (24 s) and one read; in CI the whole
    test took 21-26 s in the 15 green runs of 2026-11-04.
    """
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_DRAIN, "tenant_id": tenant_id, "types": [EVENT_TYPE_DRAIN]}]
    async with api() as client:
        reader_sub = await _subscribe(client, TOPIC_DRAIN, interest)
        assert (await _seek(client, reader_sub, "earliest", TOPIC_DRAIN)).status_code == 200
    # Open before publishing, as in the age test: the batch starts ageing
    # while it is still being published.
    async with _stream(test_env, reader_sub) as stream, api() as client:
        stored = await stream.take_during(
            RETENTION_SEGMENT_EVENTS,
            _publish(client, EVENT_TYPE_DRAIN, tenant_id, RETENTION_SEGMENT_EVENTS),
        )
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


# The slow-publish test's pause after each batch of 100: ten batches, so the
# publish goes on ~10 s, past RETENTION_DURATION_S and the second a sweep may
# take on top of it.
SLOW_PUBLISH_PAUSE_S = 1


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_a_batch_published_for_longer_than_its_age_bound_is_read_back_whole(api, test_env):
    """The retention tests' read-back, against a publish slower than the age bound.

    TOPIC_SLOW_PUBLISH keeps events RETENTION_DURATION_S. RETENTION_SEGMENT_EVENTS
    events are published in batches of 100, SLOW_PUBLISH_PAUSE_S after each,
    so the publish goes on ~10 s. The outbox stores each batch as it comes, so
    the first events reach their age bound before the last ones are
    published. A stream opened before the publish and read while it runs
    delivers every event, in order.

    This is a slow runner's publish made deterministic. Read only after the
    publish, as the retention tests once did, the stream on the memory
    backend opens after the front of the batch was swept and the read fails
    every time, the first events missing:
    `950 of 1000 events delivered ... data.n 50..999`. The backends that
    remove whole segments keep the batch's segment until well after the
    publish, so there the test passes either way.
    """
    tenant_id = str(uuid.uuid4())
    interest = [{"topic": TOPIC_SLOW_PUBLISH, "tenant_id": tenant_id, "types": [EVENT_TYPE_SLOW_PUBLISH]}]
    async with api() as client:
        sub_id = await _subscribe(client, TOPIC_SLOW_PUBLISH, interest)
        assert (await _seek(client, sub_id, "earliest", TOPIC_SLOW_PUBLISH)).status_code == 200
    async with _stream(test_env, sub_id) as stream, api() as client:
        delivered = await stream.take_during(
            RETENTION_SEGMENT_EVENTS,
            _publish(
                client,
                EVENT_TYPE_SLOW_PUBLISH,
                tenant_id,
                RETENTION_SEGMENT_EVENTS,
                pause_s=SLOW_PUBLISH_PAUSE_S,
            ),
        )
    assert [e["data"]["n"] for e in delivered] == list(range(RETENTION_SEGMENT_EVENTS))
