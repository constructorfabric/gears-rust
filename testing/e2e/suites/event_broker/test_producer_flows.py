"""Producer flow scenarios (producer/flows/1.01-1.18).

Register-and-publish patterns that exercise the exactly-once ingest-side
deduplication protocol: registration, chained sequence, idempotency key dedup,
sequence-violation rejection, cursor recovery, chain reset, and unknown-producer
rejection, plus the monotonic-mode publish rules and the `meta` mode-shape
checks.

Each test registers its own producer so tests are independent.
"""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime, timezone

from .conftest import API_BASE, EVENT_TYPE_STREAM, SUBJECT_TYPE, TOPIC_STREAM, TRACE_ID, SseFrameReader


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _event(tenant_id: str, event_id: str | None = None) -> dict:
    return {
        "id": event_id or str(uuid.uuid4()),
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": "gts.cf.e2e.event_broker.subject.v1~",
        "occurred_at": _now(),
    }


async def test_register_chained_producer_returns_201(api):
    """scenario: producer/flows/1.01-positive-register-chained-producer.md"""
    async with api() as client:
        resp = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
    assert resp.status_code == 201
    body = resp.json()
    assert body == {
        "id": body["id"],
        "mode": "chained",
        "client_agent": "e2e-test/1.0",
    }
    assert body["id"] != ""


async def test_register_monotonic_producer_returns_201(api):
    """scenario: producer/flows/1.02-positive-register-monotonic-producer.md"""
    async with api() as client:
        resp = await client.post(
            "/producers",
            json={"mode": "monotonic", "client_agent": "e2e-test/1.0"},
        )
    assert resp.status_code == 201
    body = resp.json()
    assert body == {
        "id": body["id"],
        "mode": "monotonic",
        "client_agent": "e2e-test/1.0",
    }


async def test_chained_mode_sequence_advances(api):
    """scenario: producer/flows/1.03-positive-chained-mode-sequence.md

    A chained producer publishes an event whose meta.previous is the chain head;
    the broker validates the chain, admits the event, and advances last_sequence
    to its sequence.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        # First event: previous=0, sequence=1
        resp1 = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert resp1.status_code == 202

        # Second event: previous=1, sequence=2
        resp2 = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 1,
                    "sequence": 2,
                },
            },
        )
    assert resp2.status_code == 202


async def test_a_resend_at_the_chain_head_is_a_duplicate_whatever_its_id(api):
    """scenario: producer/flows/1.04-positive-chain-head-retry-dedup.md

    Dedup keys on ``meta.sequence`` against the chain head, so a resend at the
    head under a new event ``id`` is still a duplicate: 200, nothing stored, the
    chain goes on.
    """
    tenant_id = str(uuid.uuid4())
    first_id = "e2e10400-0000-0000-0000-000000000001"
    resend_id = "e2e10400-0000-0000-0000-000000000002"
    next_id = "e2e10400-0000-0000-0000-000000000003"
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        head = {
            **_event(tenant_id, event_id=first_id),
            "meta": {
                "version": 1,
                "producer": producer_id,
                "previous": 0,
                "sequence": 1,
            },
        }
        first = await client.post("/events", json=head)
        assert first.status_code == 202, first.text
        assert first.text == ""

        resend = await client.post("/events", json={**head, "id": resend_id})
        assert resend.status_code == 200, resend.text
        assert resend.text == "", "200 dedup response must carry no body"

        following = await client.post(
            "/events",
            json={
                **_event(tenant_id, event_id=next_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 1,
                    "sequence": 2,
                },
            },
        )
        assert following.status_code == 202, following.text
        assert following.text == ""

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 2}],
        }

        # All three events share one tenant, so one outbox partition drains
        # them in publish order: seeing the last one rules out a stored resend
        # behind it.
        delivered = await _deliveries_through(client, tenant_id, next_id)

    assert [e["id"] for e in delivered] == [first_id, next_id]
    [a, c] = delivered
    assert c["sequence"] > a["sequence"]
    assert delivered == [
        {
            "id": first_id,
            "type": EVENT_TYPE_STREAM,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": a["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": a["sequence"],
            "sequence_time": a["sequence_time"],
        },
        {
            "id": next_id,
            "type": EVENT_TYPE_STREAM,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": c["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": c["sequence"],
            "sequence_time": c["sequence_time"],
        },
    ]


async def test_a_resend_at_the_chain_head_is_a_duplicate_whatever_its_previous(api):
    """scenario: producer/flows/1.04-positive-chain-head-retry-dedup.md

    A resend is a duplicate when its ``sequence`` is the head, whatever its
    ``previous``: 200, nothing stored, the chain goes on.
    """
    tenant_id = str(uuid.uuid4())
    first_id = "e2e10401-0000-0000-0000-000000000001"
    second_id = "e2e10401-0000-0000-0000-000000000002"
    resend_id = "e2e10401-0000-0000-0000-000000000003"
    next_id = "e2e10401-0000-0000-0000-000000000004"
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        first = await client.post(
            "/events",
            json={
                **_event(tenant_id, event_id=first_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert first.status_code == 202, first.text
        assert first.text == ""

        second = {
            **_event(tenant_id, event_id=second_id),
            "meta": {
                "version": 1,
                "producer": producer_id,
                "previous": 1,
                "sequence": 2,
            },
        }
        head = await client.post("/events", json=second)
        assert head.status_code == 202, head.text
        assert head.text == ""

        resend = await client.post(
            "/events",
            json={
                **second,
                "id": resend_id,
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 2,
                },
            },
        )
        assert resend.status_code == 200, resend.text
        assert resend.text == "", "200 dedup response must carry no body"

        following = await client.post(
            "/events",
            json={
                **_event(tenant_id, event_id=next_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 2,
                    "sequence": 3,
                },
            },
        )
        assert following.status_code == 202, following.text
        assert following.text == ""

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 3}],
        }

        # All four events share one tenant, so one outbox partition drains
        # them in publish order: seeing the last one rules out a stored resend
        # behind it.
        delivered = await _deliveries_through(client, tenant_id, next_id)

    assert [e["id"] for e in delivered] == [first_id, second_id, next_id]
    [a, b, c] = delivered
    assert a["sequence"] < b["sequence"] < c["sequence"]
    assert delivered == [
        {
            "id": first_id,
            "type": EVENT_TYPE_STREAM,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": a["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": a["sequence"],
            "sequence_time": a["sequence_time"],
        },
        {
            "id": second_id,
            "type": EVENT_TYPE_STREAM,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": b["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": b["sequence"],
            "sequence_time": b["sequence_time"],
        },
        {
            "id": next_id,
            "type": EVENT_TYPE_STREAM,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": c["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": c["sequence"],
            "sequence_time": c["sequence_time"],
        },
    ]


async def test_chained_sequence_violation_returns_412(api):
    """scenario: producer/flows/1.05-negative-chained-sequence-violation.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        # First event: establishes last_sequence=1 for this producer.
        first = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert first.status_code == 202

        # Stale previous: broker has last_sequence=1 but we claim previous=99.
        resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 99,
                    "sequence": 100,
                },
            },
        )
    assert resp.status_code == 412
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 412,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "violations": [
                {
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": (
                        f"index=0 producer={producer_id} topic={TOPIC_STREAM} last_sequence=1"
                    ),
                }
            ],
            "resource_type": "gts.cf.core.events.topic.v1~",
            "resource_name": TOPIC_STREAM,
        },
    }


async def test_cursor_recovery_returns_last_sequence(api):
    """scenario: producer/flows/1.06-positive-cursor-recovery.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        # Publish two events so the cursor has something to return.
        await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 1,
                    "sequence": 2,
                },
            },
        )

        cursor_resp = await client.get(f"/producers/{producer_id}/cursors")
    assert cursor_resp.status_code == 200
    body = cursor_resp.json()
    assert body == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [
            {
                "topic": TOPIC_STREAM,
                "last_sequence": 2,
            }
        ],
    }


async def test_chain_reset_returns_200(api):
    """scenario: producer/flows/1.07-positive-chain-reset.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        # Publish one event to create state.
        await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )

        # Reset clears the chain state.
        reset_resp = await client.post(f"/producers/{producer_id}:reset")
    assert reset_resp.status_code == 200
    assert reset_resp.text == ""


async def test_unknown_producer_returns_404(api):
    """scenario: producer/flows/1.08-negative-unknown-producer.md

    Publishing with an unregistered (or reaped) producer id is a missing
    resource: the producer's own registration is gone, so the broker rejects
    with ``404 Not Found`` naming the producer resource. The id is a validated
    UUID by the time existence is checked, so it rides in ``resource_name``;
    the recovery is to re-register via ``POST /v1/producers``.
    """
    fake_producer_id = str(uuid.uuid4())
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": fake_producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
    assert resp.status_code == 404, resp.text
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.producer.v1~",
            "resource_name": fake_producer_id,
        },
    }


async def test_chained_producer_desync_recovery(api):
    """scenario: producer/flows/1.09-flow-chained-producer-desync-recovery.md

    A chained producer's local sequence counter diverges from the broker's
    (simulated by using an incorrect ``meta.previous``).  The first publish
    fails with 412 SequenceViolation.  The producer reads the authoritative
    cursor via ``GET /producers/{id}/cursors`` and republishes with the correct
    sequence.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        # Register producer.
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        # Publish two events to advance the broker's sequence to 2.
        for seq in range(1, 3):
            r = await client.post(
                "/events",
                json={
                    **_event(tenant_id),
                    "meta": {
                        "version": 1,
                        "producer": producer_id,
                        "previous": seq - 1,
                        "sequence": seq,
                    },
                },
            )
            assert r.status_code == 202

        # Step 1 - Publish with a stale sequence (simulating a crash/restore).
        # Broker last accepted sequence=2; producer thinks it's 0.
        stale_resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert stale_resp.status_code == 412
        stale_body = stale_resp.json()
        assert stale_body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": stale_body["detail"],
            "instance": stale_body["instance"],
            "trace_id": TRACE_ID,
            "context": {
                "violations": [
                    {
                        "type": "sequence_mismatch",
                        "subject": "(producer)",
                        "description": stale_body["context"]["violations"][0]["description"],
                    }
                ],
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": TOPIC_STREAM,
            },
        }

        # Step 2 - Read the broker's cursor to learn the authoritative sequence.
        cursors_resp = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors_resp.status_code == 200
        cursor_body = cursors_resp.json()
        assert cursor_body["id"] == producer_id
        assert cursor_body["topics"] == [{"topic": TOPIC_STREAM, "last_sequence": 2}]
        last_seq = 2

        # Step 3 - Republish with the correct sequence (previous=2, sequence=3).
        recovered_resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": last_seq,
                    "sequence": last_seq + 1,
                },
            },
        )
    assert recovered_resp.status_code == 202


async def _deliveries_through(client, tenant_id: str, last_id: str) -> list[dict]:
    """Subscribes a new group to the stream topic for ``tenant_id``, seeks it to the
    earliest offset and returns the payloads delivered up to and including
    ``last_id``, within 7s.
    """
    group = await client.post("/consumer-groups")
    assert group.status_code == 201
    sub = await client.post(
        "/subscriptions",
        json={
            "consumer_group": group.json()["id"],
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
    assert sub.status_code == 201
    sub_id = sub.json()["id"]
    seek = await client.post(
        f"/subscriptions/{sub_id}:seek",
        json={
            "topology_version": 1,
            "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]},
        },
    )
    assert seek.status_code == 200, seek.text
    assert seek.json() == {
        "positions": {TOPIC_STREAM: [{"partition": 0, "value": 0}]}
    }

    delivered: list[dict] = []
    async with client.stream("GET", f"/events:sse?subscription={sub_id}") as stream:
        assert stream.status_code == 200
        reader = SseFrameReader(stream)
        loop = asyncio.get_running_loop()
        # 7s bounds the topology and delivery waits together, under the 10s test timeout.
        deadline = loop.time() + 7
        await reader.await_kind("topology", timeout=deadline - loop.time())
        while not delivered or delivered[-1]["id"] != last_id:
            remaining = deadline - loop.time()
            assert remaining > 0, f"{last_id} not delivered within 7s: {delivered}"
            delivered.append((await reader.await_kind("event", timeout=remaining))["payload"])
    return delivered


async def _register_monotonic(client) -> str:
    reg = await client.post(
        "/producers", json={"mode": "monotonic", "client_agent": "e2e-test/1.0"}
    )
    assert reg.status_code == 201
    return reg.json()["id"]


async def _register_chained(client) -> str:
    reg = await client.post(
        "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
    )
    assert reg.status_code == 201
    return reg.json()["id"]


# The `detail` (and `context.format`) of each mode-shape refusal.
META_WITHOUT_PRODUCER_ID = (
    "MetaWithoutProducerId: meta carries previous or sequence but no producer"
)
CHAIN_MODE_FIELDS_MISSING = (
    "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence"
)
MONOTONIC_MODE_FIELDS_VIOLATION = (
    "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence "
    "and must omit meta.previous"
)
UNKNOWN_META_VERSION = (
    "UnknownMetaVersion: meta.version is newer than the broker supports (1)"
)


def _monotonic_meta(producer_id: str, sequence: int) -> dict:
    """Monotonic publish meta: `previous` is absent, by contract."""
    return {"version": 1, "producer": producer_id, "sequence": sequence}


async def _last_sequence(client, producer_id: str) -> int:
    cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert cursors.status_code == 200
    topics = cursors.json()["topics"]
    assert len(topics) == 1, f"expected one topic in the cursor, got {topics}"
    return topics[0]["last_sequence"]


async def test_monotonic_publish_with_gap_accepted(api):
    """scenario: producer/flows/1.10-positive-monotonic-mode-sequence.md

    Monotonic mode admits any `meta.sequence` strictly greater than the stored
    `last_sequence`; the step may be larger than one.  `last_sequence` is then
    set to the submitted value rather than incremented, so the skipped values
    are consumed for good.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_monotonic(client)

        first = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": _monotonic_meta(producer_id, 7)},
        )
        assert first.status_code == 202, first.text
        assert await _last_sequence(client, producer_id) == 7

        # 12 > 7: the gap over 8-11 is not checked.
        gapped = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": _monotonic_meta(producer_id, 12)},
        )
        assert gapped.status_code == 202, gapped.text
        assert gapped.text == "", "202 Accepted must carry no body"

        assert await _last_sequence(client, producer_id) == 12


async def test_monotonic_sequence_not_advanced_is_duplicate(api):
    """scenario: producer/flows/1.11-negative-monotonic-sequence-not-advanced.md

    A `meta.sequence` at or below `last_sequence` is a re-send, not a chain
    break: it is deduplicated, `last_sequence` does not move, and no 412 is
    raised - that outcome belongs to chained mode.

    The exact success status is unsettled (the scenario's chained equivalent
    says 202, ``DESIGN.md:2248`` says 200 with the original event), so this
    accepts either and asserts the invariant that is settled: nothing is
    admitted twice.  Tighten to one value once that is resolved.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_monotonic(client)

        first = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": _monotonic_meta(producer_id, 12)},
        )
        assert first.status_code == 202, first.text
        assert await _last_sequence(client, producer_id) == 12

        replay = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": _monotonic_meta(producer_id, 12)},
        )
        assert replay.status_code != 412, (
            "a monotonic re-send is a duplicate, not a sequence violation: "
            f"{replay.text}"
        )
        assert replay.status_code in (200, 202), replay.text

        # A value below the head - one skipped by the earlier gap - behaves
        # the same way.
        behind = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": _monotonic_meta(producer_id, 9)},
        )
        assert behind.status_code != 412, behind.text
        assert behind.status_code in (200, 202), behind.text

        assert await _last_sequence(client, producer_id) == 12


async def test_monotonic_publish_with_previous_rejected_400(api):
    """scenario: producer/flows/1.12-negative-monotonic-publish-carries-previous.md

    ``meta.previous`` is a chained-mode field.  A monotonic producer sending it
    is a mode-shape violation, rejected before the sequence comparison, so the
    otherwise-acceptable sequence is not applied either.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_monotonic(client)

        seeded = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": _monotonic_meta(producer_id, 7)},
        )
        assert seeded.status_code == 202, seeded.text

        resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 7,
                    "sequence": 8,
                },
            },
        )
        assert resp.status_code == 400, resp.text
        body = resp.json()
        assert body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": MONOTONIC_MODE_FIELDS_VIOLATION,
            "instance": f"{API_BASE}/events",
            "trace_id": TRACE_ID,
            "context": {
                "format": MONOTONIC_MODE_FIELDS_VIOLATION,
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        # The shape check precedes the sequence comparison, so sequence 8 was
        # never applied.
        assert await _last_sequence(client, producer_id) == 7


async def test_chained_publish_without_previous_rejected_400(api):
    """scenario: producer/flows/1.13-negative-chained-publish-missing-previous.md

    A chained producer's ``meta`` without ``meta.previous`` is refused before
    any sequence comparison; the chain is not started.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_chained(client)

        resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {"version": 1, "producer": producer_id, "sequence": 1},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": CHAIN_MODE_FIELDS_MISSING,
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": CHAIN_MODE_FIELDS_MISSING,
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_chained_publish_without_sequence_rejected_400(api):
    """scenario: producer/flows/1.14-negative-chained-publish-missing-sequence.md

    A chained producer's ``meta`` without ``meta.sequence`` is refused before
    any sequence comparison; the chain is not started.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_chained(client)

        resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {"version": 1, "producer": producer_id, "previous": 0},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": CHAIN_MODE_FIELDS_MISSING,
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": CHAIN_MODE_FIELDS_MISSING,
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_monotonic_publish_without_sequence_rejected_400(api):
    """scenario: producer/flows/1.15-negative-monotonic-publish-missing-sequence.md

    A monotonic producer's ``meta`` without ``meta.sequence`` is refused before
    any sequence comparison; no ``last_sequence`` is recorded.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_monotonic(client)

        resp = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": {"version": 1, "producer": producer_id}},
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": MONOTONIC_MODE_FIELDS_VIOLATION,
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": MONOTONIC_MODE_FIELDS_VIOLATION,
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_meta_without_producer_rejected_400(api):
    """scenario: producer/flows/1.16-negative-meta-without-producer.md

    A ``meta`` carrying ``meta.sequence`` but no ``meta.producer`` is refused;
    a stream read through a later sentinel delivers only the sentinel.
    """
    tenant_id = str(uuid.uuid4())
    sentinel_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={**_event(tenant_id), "meta": {"version": 1, "sequence": 5}},
        )
        sentinel = await client.post("/events", json=_event(tenant_id, sentinel_id))
        assert sentinel.status_code == 202, sentinel.text
        delivered = await _deliveries_through(client, tenant_id, sentinel_id)
    assert resp.status_code == 400, resp.text
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": META_WITHOUT_PRODUCER_ID,
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": META_WITHOUT_PRODUCER_ID,
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert [event["id"] for event in delivered] == [sentinel_id]


async def test_unknown_meta_version_rejected_400(api):
    """scenario: producer/flows/1.17-negative-unknown-meta-version.md

    A chained-shape ``meta`` at ``version`` 2 is refused; the chain is not
    started.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        producer_id = await _register_chained(client)

        resp = await client.post(
            "/events",
            json={
                **_event(tenant_id),
                "meta": {"version": 2, "producer": producer_id, "previous": 0, "sequence": 1},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": UNKNOWN_META_VERSION,
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": UNKNOWN_META_VERSION,
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_meta_with_only_a_newer_version_rejected_400(api):
    """scenario: producer/flows/1.17-negative-unknown-meta-version.md

    A ``meta`` carrying only ``version`` 2, with no producer, is refused rather
    than read as stateless; a stream read through a later sentinel delivers
    only the sentinel.
    """
    tenant_id = str(uuid.uuid4())
    sentinel_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events", json={**_event(tenant_id), "meta": {"version": 2}}
        )
        sentinel = await client.post("/events", json=_event(tenant_id, sentinel_id))
        assert sentinel.status_code == 202, sentinel.text
        delivered = await _deliveries_through(client, tenant_id, sentinel_id)
    assert resp.status_code == 400, resp.text
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": UNKNOWN_META_VERSION,
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": UNKNOWN_META_VERSION,
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert [event["id"] for event in delivered] == [sentinel_id]

async def test_meta_with_only_version_is_stateless(api):
    """scenario: producer/flows/1.18-positive-meta-with-only-version-is-stateless.md

    Two publishes whose ``meta`` carries only a version are each stateless:
    both are accepted and both are delivered, in publish order.
    """
    tenant_id = str(uuid.uuid4())
    first_id = str(uuid.uuid4())
    second_id = str(uuid.uuid4())
    async with api() as client:
        first = await client.post(
            "/events", json={**_event(tenant_id, first_id), "meta": {"version": 1}}
        )
        second = await client.post(
            "/events", json={**_event(tenant_id, second_id), "meta": {"version": 1}}
        )
        assert first.status_code == 202, first.text
        assert second.status_code == 202, second.text
        delivered = await _deliveries_through(client, tenant_id, second_id)
    assert first.text == "", "202 Accepted must carry no body"
    assert second.text == "", "202 Accepted must carry no body"
    assert [event["id"] for event in delivered] == [first_id, second_id]
