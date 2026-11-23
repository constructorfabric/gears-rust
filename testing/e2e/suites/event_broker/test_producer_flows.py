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
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from datetime import datetime, timezone

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
    SseFrameReader,
)
from .test_consumer_subscriptions import _created_subscription_id
from .test_publish_consume import _read_case_window, _stream_instant


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _event(
    tenant_id: str, event_id: str | None = None, *, occurred_at: str | None = None
) -> dict:
    return {
        "id": event_id or str(uuid.uuid4()),
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": "gts.cf.e2e.event_broker.subject.v1~",
        "occurred_at": occurred_at or _now(),
    }


def _owner_event(event_id: str) -> dict:
    return _event(DEFAULT_TENANT_ID, event_id, occurred_at="2026-05-29T10:00:00Z")


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
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        resp1 = await client.post(
            "/events",
            json={
                **_owner_event("da2c36ca-313e-5b3b-a90e-43878eb5bab0"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert resp1.status_code == 202
        assert resp1.content == b""

        resp2 = await client.post(
            "/events",
            json={
                **_owner_event("ec76793d-d2c8-5d16-a95f-c42f2f2af945"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 1,
                    "sequence": 2,
                },
            },
        )
    assert resp2.status_code == 202
    assert resp2.content == b""


async def test_a_resend_at_the_chain_head_is_a_duplicate_whatever_its_id(api):
    """scenario: producer/flows/1.04-positive-chain-head-retry-dedup.md

    A retry with a new event ID at the current chain head leaves the cursor
    and storage unchanged. The next link still appends.
    """
    first = _owner_event("e2e10400-0000-0000-0000-000000000001")
    retry = {**first, "id": "e2e10400-0000-0000-0000-000000000002"}
    following = _owner_event("e2e10400-0000-0000-0000-000000000003")
    marker = _owner_event("1fdde9a1-6b77-57fe-ad17-1555d19c838c")
    deadline = asyncio.get_running_loop().time() + 9
    async with api() as client, _open_metadata_window(client, marker, deadline) as (head, reader):
        registration = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert (registration.status_code, registration.headers.get("content-type")) == (
            201, "application/json",
        )
        location = registration.headers["location"]
        producer_id = location.removeprefix(f"{API_BASE}/producers/")
        assert str(uuid.UUID(producer_id)) == producer_id
        assert location == f"{API_BASE}/producers/{producer_id}"
        assert registration.json() == {
            "id": producer_id, "mode": "chained", "client_agent": "e2e-test/1.0",
        }

        for event, previous, sequence, status in (
            (first, 0, 1, 202), (retry, 0, 1, 200), (following, 1, 2, 202),
        ):
            published = await client.post("/events", json={
                **event,
                "meta": {
                    "version": 1, "producer": producer_id, "previous": previous,
                    "sequence": sequence,
                },
            })
            assert (published.status_code, published.content) == (status, b"")
            cursors = await client.get(f"/producers/{producer_id}/cursors")
            assert (cursors.status_code, cursors.headers.get("content-type")) == (
                200, "application/json",
            )
            body = cursors.json()
            match body:
                case {"topics": [{"last_sequence": last_sequence}]}:
                    assert type(last_sequence) is int, body
                case _:
                    raise AssertionError(body)
            assert body == {
                "id": producer_id,
                "client_agent": "e2e-test/1.0",
                "topics": [{"topic": TOPIC_STREAM, "last_sequence": sequence}],
            }
        history, delivered = await _read_case_window(reader, head, following["id"], deadline)

    assert history == []
    assert len(delivered) == 2, delivered
    previous = head
    expected = []
    for frame, event in zip(delivered, (first, following), strict=True):
        payload = frame["payload"]
        sequence = payload["sequence"]
        assert sequence > previous, (sequence, previous)
        previous = sequence
        assert _stream_instant(payload.get("occurred_at")) == _stream_instant(event["occurred_at"])
        _stream_instant(payload.get("sequence_time"))
        expected.append({
            "kind": "event",
            "payload": {
                **event, "occurred_at": payload["occurred_at"], "trace_parent": None,
                "data": None, "partition": 0, "sequence": sequence,
                "sequence_time": payload["sequence_time"],
            },
        })
    assert delivered == expected


async def test_a_resend_at_the_chain_head_is_a_duplicate_whatever_its_previous(api):
    """scenario: producer/flows/1.04-positive-chain-head-retry-dedup.md

    A retry at sequence 2 with previous 0 leaves the cursor and storage
    unchanged. A continuation from sequence 2 still appends.
    """
    first = _owner_event("e2e10401-0000-0000-0000-000000000001")
    second = _owner_event("e2e10401-0000-0000-0000-000000000002")
    retry = {**second, "id": "e2e10401-0000-0000-0000-000000000003"}
    following = _owner_event("e2e10401-0000-0000-0000-000000000004")
    marker = _owner_event("2cb044e5-e8c9-5f6d-9b4a-576fedd447c1")
    deadline = asyncio.get_running_loop().time() + 9
    async with api() as client, _open_metadata_window(client, marker, deadline) as (head, reader):
        registration = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert (registration.status_code, registration.headers.get("content-type")) == (
            201, "application/json",
        )
        location = registration.headers["location"]
        producer_id = location.removeprefix(f"{API_BASE}/producers/")
        assert str(uuid.UUID(producer_id)) == producer_id
        assert location == f"{API_BASE}/producers/{producer_id}"
        assert registration.json() == {
            "id": producer_id, "mode": "chained", "client_agent": "e2e-test/1.0",
        }

        for event, previous, sequence, status in (
            (first, 0, 1, 202), (second, 1, 2, 202), (retry, 0, 2, 200), (following, 2, 3, 202),
        ):
            published = await client.post("/events", json={
                **event,
                "meta": {
                    "version": 1, "producer": producer_id, "previous": previous,
                    "sequence": sequence,
                },
            })
            assert (published.status_code, published.content) == (status, b"")
            cursors = await client.get(f"/producers/{producer_id}/cursors")
            assert (cursors.status_code, cursors.headers.get("content-type")) == (
                200, "application/json",
            )
            body = cursors.json()
            match body:
                case {"topics": [{"last_sequence": last_sequence}]}:
                    assert type(last_sequence) is int, body
                case _:
                    raise AssertionError(body)
            assert body == {
                "id": producer_id,
                "client_agent": "e2e-test/1.0",
                "topics": [{"topic": TOPIC_STREAM, "last_sequence": sequence}],
            }
        history, delivered = await _read_case_window(reader, head, following["id"], deadline)

    assert history == []
    assert len(delivered) == 3, delivered
    previous = head
    expected = []
    for frame, event in zip(delivered, (first, second, following), strict=True):
        payload = frame["payload"]
        sequence = payload["sequence"]
        assert sequence > previous, (sequence, previous)
        previous = sequence
        assert _stream_instant(payload.get("occurred_at")) == _stream_instant(event["occurred_at"])
        _stream_instant(payload.get("sequence_time"))
        expected.append({
            "kind": "event",
            "payload": {
                **event, "occurred_at": payload["occurred_at"], "trace_parent": None,
                "data": None, "partition": 0, "sequence": sequence,
                "sequence_time": payload["sequence_time"],
            },
        })
    assert delivered == expected


async def test_chained_sequence_violation_returns_412(api):
    """scenario: producer/flows/1.05-negative-chained-sequence-violation.md"""
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
                **_owner_event("b14ce3f3-0ece-541c-be67-3c04c6a39b4c"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert first.status_code == 202
        assert first.content == b""

        resp = await client.post(
            "/events",
            json={
                **_owner_event("3dc474e1-a487-5d5d-82ac-de4541ede317"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 99,
                    "sequence": 100,
                },
            },
        )
    assert resp.status_code == 412
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 412,
        "detail": "Operation precondition not met",
        "instance": "/event-broker/v1/events",
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
                **_owner_event("887cc2c9-bd57-5726-937c-68d90cd62475"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert first.status_code == 202
        assert first.content == b""

        second = await client.post(
            "/events",
            json={
                **_owner_event("5f7f585a-df14-5cbf-a5bd-397d9c67c784"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 1,
                    "sequence": 2,
                },
            },
        )

        assert second.status_code == 202
        assert second.content == b""

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

    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        assert reg.headers["content-type"] == "application/json"
        registration = reg.json()
        assert set(registration) == {"id", "mode", "client_agent"}
        producer_id = registration["id"]
        assert str(uuid.UUID(producer_id)) == producer_id
        assert reg.headers["location"] == f"{API_BASE}/producers/{producer_id}"
        assert registration == {
            "id": producer_id, "mode": "chained",
            "client_agent": "e2e-test/1.0",
        }

        meta = {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1}
        for event_id, event_type in (
            ("15a00138-813c-4b62-952f-000000000001", EVENT_TYPE_STREAM),
            ("15a00138-813c-4b62-952f-000000000002", EVENT_TYPE_4P),
        ):
            seeded = await client.post(
                "/events", json={**_owner_event(event_id), "type": event_type, "meta": meta}
            )
            assert seeded.status_code == 202
            assert seeded.content == b""

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        before = cursors.json()
        assert set(before) == {"id", "client_agent", "topics"}
        assert isinstance(before["topics"], list)
        assert all(set(row) == {"topic", "last_sequence"} for row in before["topics"])
        assert {**before, "topics": sorted(before["topics"], key=lambda row: row["topic"])} == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [
                {"topic": TOPIC_4P, "last_sequence": 1},
                {"topic": TOPIC_STREAM, "last_sequence": 1},
            ],
        }

        reset_resp = await client.post(f"/producers/{producer_id}:reset")
        assert reset_resp.status_code == 200
        assert reset_resp.content == b""
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }

        fresh = await client.post(
            "/events",
            json={**_owner_event("15a00138-813c-4b62-952f-000000000003"), "meta": meta},
        )
        assert fresh.status_code == 202
        assert fresh.content == b""
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 1}],
        }


async def test_unknown_producer_returns_404(api):
    """scenario: producer/flows/1.08-negative-unknown-producer.md

    Publishing with an unregistered (or reaped) producer id is a missing
    resource: the producer's own registration is gone, so the broker rejects
    with ``404 Not Found`` naming the producer resource. The id is a validated
    UUID by the time existence is checked, so it rides in ``resource_name``;
    the recovery is to re-register via ``POST /event-broker/v1/producers``.
    """
    unknown_producer_id = "31f4665b-b614-598d-9a9e-c8b0c9bb141c"
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                **_owner_event("802fc299-8eb0-5847-8775-c50833731913"),
                "meta": {
                    "version": 1,
                    "producer": unknown_producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
    assert resp.status_code == 404, resp.text
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": "the producer is not registered or its registration has expired",
        "instance": "/event-broker/v1/events",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.producer.v1~",
            "resource_name": unknown_producer_id,
        },
    }


async def test_chained_producer_desync_recovery(api):
    """scenario: producer/flows/1.09-flow-chained-producer-desync-recovery.md

    A stale publish fails with 412 SequenceViolation. The producer reads the
    authoritative cursor before resuming the chain from that sequence.
    """
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        seed_ids = (
            "62b64594-71e7-540d-821e-1d99674127f4",
            "21637af9-641c-5b1f-9b19-24705edbb2bb",
        )
        for seq, event_id in enumerate(seed_ids, start=1):
            seed = await client.post(
                "/events",
                json={
                    **_owner_event(event_id),
                    "meta": {
                        "version": 1,
                        "producer": producer_id,
                        "previous": seq - 1,
                        "sequence": seq,
                    },
                },
            )
            assert seed.status_code == 202
            assert seed.content == b""

        # Simulate a producer that restores an older local sequence.
        stale_resp = await client.post(
            "/events",
            json={
                **_owner_event("6441dde6-c7ff-5ca2-9c12-d81d0d928fc8"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert stale_resp.status_code == 412
        assert stale_resp.headers["content-type"] == "application/problem+json"
        stale_body = stale_resp.json()
        assert stale_body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": "Operation precondition not met",
            "instance": "/event-broker/v1/events",
            "trace_id": TRACE_ID,
            "context": {
                "violations": [
                    {
                        "type": "sequence_mismatch",
                        "subject": "(producer)",
                        "description": (
                            f"index=0 producer={producer_id} topic={TOPIC_STREAM} "
                            "last_sequence=2"
                        ),
                    }
                ],
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": TOPIC_STREAM,
            },
        }

        cursors_resp = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors_resp.status_code == 200
        assert cursors_resp.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 2}],
        }
        last_seq = 2

        recovered_resp = await client.post(
            "/events",
            json={
                **_owner_event("3b56a003-48c0-5895-b2e9-cf2336f10e09"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": last_seq,
                    "sequence": last_seq + 1,
                },
            },
        )
    assert recovered_resp.status_code == 202
    assert recovered_resp.content == b""


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


async def test_monotonic_publish_with_gap_accepted(api):
    """scenario: producer/flows/1.10-positive-monotonic-mode-sequence.md

    Monotonic mode admits any `meta.sequence` strictly greater than the stored
    `last_sequence`; the step may be larger than one.  `last_sequence` is then
    set to the submitted value rather than incremented, so the skipped values
    are consumed for good.
    """
    async with api() as client:
        producer_id = await _register_monotonic(client)

        first = await client.post(
            "/events",
            json={
                **_owner_event("ea7d64c0-66df-5aa9-99b0-5ecb497bbd55"),
                "meta": _monotonic_meta(producer_id, 7),
            },
        )
        assert first.status_code == 202, first.text
        assert first.content == b""
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 7}],
        }

        gapped = await client.post(
            "/events",
            json={
                **_owner_event("6919bf99-bdf0-517a-a87b-5bc66806835b"),
                "meta": _monotonic_meta(producer_id, 12),
            },
        )
        assert gapped.status_code == 202, gapped.text
        assert gapped.content == b""

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 12}],
        }


async def test_monotonic_sequence_not_advanced_is_duplicate(api):
    """scenario: producer/flows/1.11-negative-monotonic-sequence-not-advanced.md

    A `meta.sequence` at or below `last_sequence` is a duplicate. The broker
    answers 200 with an empty body and keeps the current head.
    """
    async with api() as client:
        producer_id = await _register_monotonic(client)

        first = await client.post(
            "/events",
            json={
                **_owner_event("a17aa935-b76b-585e-bbbe-3c5918399db9"),
                "meta": _monotonic_meta(producer_id, 12),
            },
        )
        assert first.status_code == 202, first.text
        assert first.content == b""
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 12}],
        }

        replay = await client.post(
            "/events",
            json={
                **_owner_event("b4443814-4ac3-599b-bf09-b9b70200ce28"),
                "meta": _monotonic_meta(producer_id, 12),
            },
        )
        assert replay.status_code == 200, replay.text
        assert replay.content == b""

        # Sequence 9 is a duplicate even though the seed skipped it.
        behind = await client.post(
            "/events",
            json={
                **_owner_event("af6dce1c-a789-5215-bddd-946c19b8bbde"),
                "meta": _monotonic_meta(producer_id, 9),
            },
        )
        assert behind.status_code == 200, behind.text
        assert behind.content == b""

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 12}],
        }


async def test_monotonic_publish_with_previous_rejected_400(api):
    """scenario: producer/flows/1.12-negative-monotonic-publish-carries-previous.md

    ``meta.previous`` is a chained-mode field.  A monotonic producer sending it
    is a mode-shape violation, rejected before the sequence comparison, so the
    otherwise-acceptable sequence is not applied either.
    """
    async with api() as client:
        producer_id = await _register_monotonic(client)

        seeded = await client.post(
            "/events",
            json={
                **_owner_event("c26207a1-96cd-5615-b6d0-b0d3e889af23"),
                "meta": _monotonic_meta(producer_id, 7),
            },
        )
        assert seeded.status_code == 202, seeded.text
        assert seeded.content == b""

        resp = await client.post(
            "/events",
            json={
                **_owner_event("1e0b90e0-0684-53ae-ad80-922f3e390ea6"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 7,
                    "sequence": 8,
                },
            },
        )
        assert resp.status_code == 400, resp.text
        assert resp.headers["content-type"] == "application/problem+json"
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
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 7}],
        }


async def test_chained_publish_without_previous_rejected_400(api):
    """scenario: producer/flows/1.13-negative-chained-publish-missing-previous.md

    A chained producer's ``meta`` without ``meta.previous`` is refused before
    any sequence comparison; the chain is not started.
    """
    async with api() as client:
        producer_id = await _register_chained(client)

        resp = await client.post(
            "/events",
            json={
                **_owner_event("856da652-b08f-54bf-9bab-14a446b56103"),
                "meta": {"version": 1, "producer": producer_id, "sequence": 1},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.headers["content-type"] == "application/problem+json"
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
    async with api() as client:
        producer_id = await _register_chained(client)

        resp = await client.post(
            "/events",
            json={
                **_owner_event("44540df9-7ea4-5b51-b65d-f9d5b0290cf3"),
                "meta": {"version": 1, "producer": producer_id, "previous": 0},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.headers["content-type"] == "application/problem+json"
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
    async with api() as client:
        producer_id = await _register_monotonic(client)

        resp = await client.post(
            "/events",
            json={
                **_owner_event("b8dbf96f-3182-52b8-856d-7546f1dccbd8"),
                "meta": {"version": 1, "producer": producer_id},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.headers["content-type"] == "application/problem+json"
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


@asynccontextmanager
async def _open_metadata_window(
    client, marker: dict, deadline: float,
) -> AsyncIterator[tuple[int, SseFrameReader]]:
    # A durable marker on the same key fences earlier async publishes before sampling the head.
    fence = await client.post("/events", json=marker, headers={"Prefer": "wait=10"}, timeout=12)
    assert (fence.status_code, fence.content, fence.headers.get("preference-applied")) == (
        201, b"", "wait=10",
    )
    manifest = await client.get("/topics/segments", params={"topic": TOPIC_STREAM, "partition": 0})
    assert (manifest.status_code, manifest.headers.get("content-type")) == (200, "application/json")
    snapshot = manifest.json()
    assert set(snapshot) == {
        "topic", "partition", "start_sequence", "end_sequence",
        "start_time", "end_time", "segments",
    }
    assert (snapshot["topic"], snapshot["partition"]) == (TOPIC_STREAM, 0)
    high_water = snapshot["end_sequence"]
    assert type(high_water) is int and high_water > 0, snapshot

    group = await client.post("/consumer-groups")
    assert (group.status_code, group.headers.get("content-type")) == (201, "application/json")
    group_location = group.headers["location"]
    group_id = group_location.removeprefix(f"{API_BASE}/consumer-groups/")
    group_prefix = "gts.cf.core.events.consumer_group.v1~"
    assert group_id == f"{group_prefix}{uuid.UUID(group_id.removeprefix(group_prefix))}"
    assert group_location == f"{API_BASE}/consumer-groups/{group_id}"
    group_body = group.json()
    _stream_instant(group_body.get("created_at"))
    assert group_body == {
        "id": group_id,
        "kind": "anonymous",
        "tenant": DEFAULT_TENANT_ID,
        "owner_principal": DEFAULT_SUBJECT_ID,
        "description": None,
        "created_at": group_body["created_at"],
    }

    interest = {
        "topic": TOPIC_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_STREAM],
    }
    joined = await client.post(
        "/subscriptions",
        json={"consumer_group": group_id, "client_agent": "e2e-test", "interests": [interest]},
    )
    assert (joined.status_code, joined.headers.get("content-type")) == (201, "application/json")
    sub_id = _created_subscription_id(joined)
    member = joined.json()
    _stream_instant(member.get("created_at"))
    assert member == {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [{
            **interest, "barrier_mode": "respect", "filter": None, "unmatched_types": [],
        }],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": member["created_at"],
    }
    positions = {TOPIC_STREAM: [{"partition": 0, "value": high_water}]}
    seek = await client.post(
        f"/subscriptions/{sub_id}:seek",
        json={"topology_version": 1, "positions": positions},
    )
    assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
    seek_body = seek.json()
    match seek_body:
        case {"positions": dict(resolved)}:
            match resolved.get(TOPIC_STREAM):
                case [{"value": value}]:
                    assert type(value) is int, seek_body
                case _:
                    raise AssertionError(seek_body)
        case _:
            raise AssertionError(seek_body)
    assert seek_body == {"positions": positions}

    async with client.stream(
        "GET", "/events:sse", params={"subscription": sub_id},
        headers={"Accept": "text/event-stream"},
    ) as response:
        assert (response.status_code, response.headers.get("content-type")) == (
            200, "text/event-stream",
        )
        remaining = deadline - asyncio.get_running_loop().time()
        assert remaining > 0, "no SSE topology before the deadline"
        reader = SseFrameReader(response)
        topology = await reader.next_frame(timeout=remaining)
        match topology:
            case ("topology", {"assigned": [{"offset": offset, "last_examined": last_examined}]}):
                assert type(offset) is int, topology
                assert type(last_examined) is int, topology
            case _:
                raise AssertionError(topology)
        assert topology == (
            "topology",
            {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [{
                    "topic": TOPIC_STREAM, "partition": 0,
                    "offset": high_water, "last_examined": high_water,
                }],
            },
        )
        yield high_water, reader


async def test_meta_without_producer_rejected_400(api):
    """scenario: producer/flows/1.16-negative-meta-without-producer.md

    A ``meta`` carrying ``meta.sequence`` but no ``meta.producer`` is refused;
    a stream read through a later sentinel delivers only the sentinel.
    """
    event = {
        "id": "d0000000-0000-0000-0000-000000000016",
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "order-service",
        "subject": "order-anon",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-05-29T10:07:00Z",
        "data": {"order_id": "order-anon", "total_cents": 700},
    }
    marker = {**event, "id": "3e246f70-89a4-571a-95a7-a6a833d3f063"}
    sentinel = {**event, "id": "1b5b2ebc-d4eb-5f4a-a7f6-eba202ae415e"}
    deadline = asyncio.get_running_loop().time() + 9
    async with api() as client, _open_metadata_window(client, marker, deadline) as (head, reader):
        resp = await client.post(
            "/events", json={**event, "meta": {"version": 1, "sequence": 5}},
        )
        assert (resp.status_code, resp.headers.get("content-type")) == (
            400, "application/problem+json",
        )
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
        accepted = await client.post("/events", json=sentinel)
        assert (accepted.status_code, accepted.content) == (202, b"")
        history, delivered = await _read_case_window(reader, head, sentinel["id"], deadline)

    assert history == []
    assert len(delivered) == 1, delivered
    payload = delivered[0]["payload"]
    assert _stream_instant(payload.get("occurred_at")) == _stream_instant(sentinel["occurred_at"])
    _stream_instant(payload.get("sequence_time"))
    assert delivered == [{
        "kind": "event",
        "payload": {
            **sentinel,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "partition": 0,
            "sequence": payload["sequence"],
            "sequence_time": payload["sequence_time"],
        },
    }]


async def test_unknown_meta_version_rejected_400(api):
    """scenario: producer/flows/1.17-negative-unknown-meta-version.md

    A chained-shape ``meta`` at ``version`` 2 is refused; the chain is not
    started.
    """
    async with api() as client:
        producer_id = await _register_chained(client)

        resp = await client.post(
            "/events",
            json={
                **_owner_event("48710387-58b3-5cc7-bb8f-fec7292948a2"),
                "meta": {"version": 2, "producer": producer_id, "previous": 0, "sequence": 1},
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.headers["content-type"] == "application/problem+json"
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
    event = {
        "id": "d0000000-0000-0000-0000-000000000117",
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "order-service",
        "subject": "order-anon",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-05-29T10:08:00Z",
        "data": {"order_id": "order-anon", "total_cents": 700},
    }
    marker = {**event, "id": "880d6c72-31fd-54d1-b636-b4548c60525e"}
    sentinel = {**event, "id": "d4bcec7d-5aa1-57b8-abb3-a98256173e17"}
    deadline = asyncio.get_running_loop().time() + 9
    async with api() as client, _open_metadata_window(client, marker, deadline) as (head, reader):
        resp = await client.post("/events", json={**event, "meta": {"version": 2}})
        assert (resp.status_code, resp.headers.get("content-type")) == (
            400, "application/problem+json",
        )
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
        accepted = await client.post("/events", json=sentinel)
        assert (accepted.status_code, accepted.content) == (202, b"")
        history, delivered = await _read_case_window(reader, head, sentinel["id"], deadline)

    assert history == []
    assert len(delivered) == 1, delivered
    payload = delivered[0]["payload"]
    assert _stream_instant(payload.get("occurred_at")) == _stream_instant(sentinel["occurred_at"])
    _stream_instant(payload.get("sequence_time"))
    assert delivered == [{
        "kind": "event",
        "payload": {
            **sentinel,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "partition": 0,
            "sequence": payload["sequence"],
            "sequence_time": payload["sequence_time"],
        },
    }]


async def test_meta_with_only_version_is_stateless(api):
    """scenario: producer/flows/1.18-positive-meta-with-only-version-is-stateless.md

    Two publishes whose ``meta`` carries only a version are each stateless:
    both are accepted and both are delivered, in publish order.
    """
    first = {
        "id": "d0000000-0000-0000-0000-000000000018",
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "order-service",
        "subject": "order-1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-05-29T10:07:00Z",
        "data": {"order_id": "order-1", "total_cents": 700},
    }
    second = {
        **first,
        "id": "d0000000-0000-0000-0000-000000000019",
        "source": "billing-service",
        "subject": "invoice-1",
        "occurred_at": "2026-05-29T10:07:01Z",
    }
    marker = {**first, "id": "a504fe50-9438-54cf-badc-2f30eb8c8b8c"}
    deadline = asyncio.get_running_loop().time() + 9
    async with api() as client, _open_metadata_window(client, marker, deadline) as (head, reader):
        for event in (first, second):
            accepted = await client.post("/events", json={**event, "meta": {"version": 1}})
            assert (accepted.status_code, accepted.content) == (202, b"")
        history, delivered = await _read_case_window(reader, head, second["id"], deadline)

    assert history == []
    assert len(delivered) == 2, delivered
    previous = head
    expected = []
    for frame, event in zip(delivered, (first, second), strict=True):
        payload = frame["payload"]
        sequence = payload["sequence"]
        assert sequence > previous, (sequence, previous)
        previous = sequence
        assert _stream_instant(payload.get("occurred_at")) == _stream_instant(event["occurred_at"])
        _stream_instant(payload.get("sequence_time"))
        expected.append({
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "partition": 0,
                "sequence": sequence,
                "sequence_time": payload["sequence_time"],
            },
        })
    assert delivered == expected
