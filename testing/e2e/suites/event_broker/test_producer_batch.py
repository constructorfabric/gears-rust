"""Batch publish scenarios (producer/batch/1.01-1.14).

- 1.01: batch of two events, same topic → 202
- 1.02: batch mixing topics → 400 (the two pre-provisioned topics serve as
        the two different topics needed to trigger the rejection)
- 1.03: batch too large (> 100 events) → 413
- 1.04: late validation failure (invalid event at position N) → 422
- 1.05: empty batch → 400
- 1.06: event id repeated within the batch → 400
- 1.07: chained run that does not link → 400
- 1.08: run behind the committed chain head → 412
- 1.09: resent batch of only duplicates → 200
- 1.10: sync batch (Prefer: wait=N) → 201 once every admitted event is stored
- 1.11: batch with an event over the 60 KiB per-event limit → 413 naming it
- 1.12: batch with a partition key over 1024 bytes → 400 naming its event
- 1.13: batch with one event in an unreachable tenant → 403, nothing enqueued
- 1.14: batch with a chained event missing ``meta.previous`` → 400, nothing enqueued
"""

from __future__ import annotations

import asyncio
import json
import uuid
from datetime import datetime, timezone

import pytest

from .conftest import (
    API_BASE,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_KEYED,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_LONGPOLL,
    EVENT_TYPE_STRICT,
    SUBJECT_TYPE,
    TOPIC_KEYED,
    TOPIC_STREAM,
    TOPIC_LONGPOLL,
    TOPIC_STRICT,
    TRACE_ID,
)


def _event(
    event_type: str,
    tenant_id: str,
    subject: str = "s1",
    *,
    event_id: str | None = None,
    occurred_at: str | None = None,
) -> dict:
    return {
        "id": event_id or str(uuid.uuid4()),
        "type": event_type,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": subject,
        "subject_type": SUBJECT_TYPE,
        "occurred_at": occurred_at or datetime.now(timezone.utc).isoformat(),
    }


def _owner_event(event_type: str, event_id: str, subject: str = "s1") -> dict:
    return _event(
        event_type,
        DEFAULT_TENANT_ID,
        subject,
        event_id=event_id,
        occurred_at="2026-05-29T10:00:00Z",
    )


async def test_batch_publish_returns_202(api):
    """scenario: producer/batch/1.01-positive-publish-batch.md"""
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    _owner_event(EVENT_TYPE_STREAM, "044b2e1d-a5c0-50e0-8a4e-481a8a49dae6", "s1"),
                    _owner_event(EVENT_TYPE_STREAM, "262393da-37e6-5ae1-88bc-64a1d41f20f8", "s2"),
                ]
            },
        )
    assert resp.status_code == 202
    assert resp.content == b""


@pytest.mark.backend_sensitive
async def test_batch_prefer_wait_returns_201_once_stored(api):
    """scenario: producer/batch/1.10-positive-batch-sync-wait.md

    ``Prefer: wait=N`` applies to the batch as a whole: once every admitted
    event is stored the broker answers ``201``, and ``Preference-Applied``
    reports the applied wait (RFC 7240 §3).
    """
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            headers={"Prefer": "wait=10"},
            json={
                "events": [
                    _owner_event(EVENT_TYPE_STREAM, "19cfbbca-1f1d-5f49-a149-5586a45165fd", "s1"),
                    _owner_event(EVENT_TYPE_STREAM, "a47f0466-815e-508e-8340-28a334df14ec", "s2"),
                ]
            },
        )
    assert resp.status_code == 201
    assert resp.content == b""
    assert resp.headers.get("preference-applied") == "wait=10"


async def test_batch_mixing_topics_rejected_400(api):
    """scenario: producer/batch/1.02-negative-mixed-topic-batch.md"""

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
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }

        meta = {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1}
        events = [
            {
                **_owner_event(EVENT_TYPE_STREAM, "15a00119-813c-4b62-952f-000000000001"),
                "meta": meta,
            },
            {
                **_owner_event(EVENT_TYPE_LONGPOLL, "15a00119-813c-4b62-952f-000000000002"),
                "meta": meta,
            },
        ]
        resp = await client.post("/events:batch", json={"events": events})
        assert resp.status_code == 400
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "MixedTopics: a batch must target a single topic",
            "instance": f"{API_BASE}/events:batch",
            "trace_id": TRACE_ID,
            "context": {
                "format": "MixedTopics: a batch must target a single topic",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }


async def test_batch_too_large_rejected_413(api):
    """scenario: producer/batch/1.03-negative-batch-too-large.md"""

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
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }

        events = [
            {
                **_owner_event(EVENT_TYPE_STREAM, f"15a00120-813c-4b62-952f-{sequence:012d}"),
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": sequence - 1,
                    "sequence": sequence,
                },
            }
            for sequence in range(1, 102)
        ]
        resp = await client.post("/events:batch", json={"events": events})
        assert resp.status_code == 413
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": f"{API_BASE}/events:batch",
            "trace_id": TRACE_ID,
            "context": {
                "field_violations": [
                    {
                        "field": "batch.count",
                        "description": "batch too large: 101 events (max 100)",
                        "reason": "batch_too_large",
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }


async def test_batch_late_validation_failure_rejected_422(api):
    """scenario: producer/batch/1.04-negative-batch-late-validation-failure.md

    The first event satisfies the schema. The second omits ``data.strict_field``
    to reach payload validation (422). Batch admission is atomic.
    """
    valid = {
        **_owner_event(EVENT_TYPE_STRICT, "16f41047-7333-5a72-9093-be34a02762bf"),
        "data": {"strict_field": "ok"},
    }
    invalid = {
        **_owner_event(EVENT_TYPE_STRICT, "39851c0d-9c51-521b-8901-0890e1ee9b6e"),
        "data": {},
    }
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            json={"events": [valid, invalid]},
        )
    assert resp.status_code == 422
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 422,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/events:batch",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "(payload)",
                    "description": "event data does not satisfy the event type's data schema",
                    "reason": "schema_validation",
                }
            ],
            "resource_type": "gts.cf.core.events.topic.v1~",
            "resource_name": TOPIC_STRICT,
        },
    }

    # Give the drain time to expose any partial admission in the manifest.
    await asyncio.sleep(1.0)
    async with api() as client:
        resp = await client.get(f"/topics/segments?topic={TOPIC_STRICT}&partition=0")
    assert resp.status_code == 200
    assert resp.json() == {
        "topic": TOPIC_STRICT,
        "partition": 0,
        "start_sequence": 0,
        "end_sequence": 0,
        "start_time": None,
        "end_time": None,
        "segments": [],
    }


async def test_batch_without_events_is_refused_as_empty_batch_400(api):
    """scenario: producer/batch/1.05-negative-batch-empty.md"""

    async with api() as client:
        first_marker = await client.post(
            "/events", headers={"Prefer": "wait=10"},
            json=_owner_event(EVENT_TYPE_STREAM, "15a00122-813c-4b62-952f-000000000001"),
        )
        assert first_marker.status_code == 201
        assert first_marker.content == b""
        assert first_marker.headers["preference-applied"] == "wait=10"
        before_resp = await client.get(
            "/topics/segments", params={"topic": TOPIC_STREAM, "partition": 0}
        )
        assert before_resp.status_code == 200
        assert before_resp.headers["content-type"] == "application/json"
        before = before_resp.json()
        assert set(before) == {
            "topic",
            "partition",
            "start_sequence",
            "end_sequence",
            "start_time",
            "end_time",
            "segments",
        }
        assert before == {
            "topic": TOPIC_STREAM,
            "partition": 0,
            "start_sequence": before["start_sequence"],
            "end_sequence": before["end_sequence"],
            "start_time": before["start_time"],
            "end_time": before["end_time"],
            "segments": before["segments"],
        }
        assert 0 < before["start_sequence"] <= before["end_sequence"]
        assert isinstance(before["start_time"], str)
        assert isinstance(before["end_time"], str)
        assert isinstance(before["segments"], list)

        resp = await client.post("/events:batch", json={"events": []})
        assert resp.status_code == 400
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": f"{API_BASE}/events:batch",
            "trace_id": TRACE_ID,
            "context": {
                "field_violations": [
                    {
                        "field": "events",
                        "description": "a batch carries at least one event",
                        "reason": "empty_batch",
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        after = await client.get("/topics/segments", params={"topic": TOPIC_STREAM, "partition": 0})
        assert after.status_code == 200
        assert after.headers["content-type"] == "application/json"
        assert after.json() == before


async def test_batch_repeating_an_event_id_is_refused_naming_the_repeat_400(api):
    """scenario: producer/batch/1.06-negative-batch-duplicate-event-id.md"""

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
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }

        event = _owner_event(EVENT_TYPE_STREAM, "15a00123-813c-4b62-952f-000000000001")
        events = [
            {
                **event,
                "meta": {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1},
            },
            {
                **event,
                "meta": {"version": 1, "producer": producer_id, "previous": 1, "sequence": 2},
            },
        ]
        resp = await client.post("/events:batch", json={"events": events})
        assert resp.status_code == 400
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": f"{API_BASE}/events:batch",
            "trace_id": TRACE_ID,
            "context": {
                "field_violations": [
                    {
                        "field": "events[1].id",
                        "description": "repeats the id of an earlier event of the batch",
                        "reason": "duplicate_event_id",
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }


async def test_batch_with_a_chained_run_that_does_not_link_is_refused_as_malformed_run_400(api):
    """scenario: producer/batch/1.07-negative-batch-malformed-run.md"""
    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "620ede6b-0ca8-558d-a3f8-d7c0a9dca9b7"),
                        "meta": {
                            "version": 1,
                            "producer": producer_id,
                            "previous": 0,
                            "sequence": 1,
                        },
                    },
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "48f157b3-d44c-5671-9f29-49d91d06504a"),
                        "meta": {
                            "version": 1,
                            "producer": producer_id,
                            "previous": 2,
                            "sequence": 3,
                        },
                    },
                ]
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/events:batch",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "events[1].meta",
                    "description": (
                        "does not follow the same producer's event before it in the batch"
                    ),
                    "reason": "malformed_run",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_batch_with_a_run_behind_the_committed_head_is_refused_naming_the_head_412(api):
    """scenario: producer/batch/1.08-negative-batch-sequence-violation.md"""
    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        seed = await client.post(
            "/events",
            json={
                **_owner_event(EVENT_TYPE_STREAM, "95411c22-7497-57ec-877e-2586bd2566fb"),
                "meta": {"version": 1, "producer": producer_id, "previous": 6, "sequence": 7},
            },
        )
        assert seed.status_code == 202
        assert seed.content == b""

        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "7e23e1f1-962d-5597-9bdc-8eaa119ecc02"),
                        "meta": {
                            "version": 1,
                            "producer": producer_id,
                            "previous": 3,
                            "sequence": 4,
                        },
                    },
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "9f10c7b4-1564-5f3c-bdc4-eeda0a677734"),
                        "meta": {
                            "version": 1,
                            "producer": producer_id,
                            "previous": 4,
                            "sequence": 5,
                        },
                    },
                ]
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 412
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 412,
        "detail": "Operation precondition not met",
        "instance": f"{API_BASE}/events:batch",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.topic.v1~",
            "resource_name": TOPIC_STREAM,
            "violations": [
                {
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": (
                        f"index=0 producer={producer_id} topic={TOPIC_STREAM} last_sequence=7"
                    ),
                }
            ],
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [{"topic": TOPIC_STREAM, "last_sequence": 7}],
    }


async def test_batch_resent_after_a_lost_ack_answers_200_and_keeps_the_cursor(api):
    """scenario: producer/batch/1.09-positive-batch-all-duplicates.md"""
    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        seed = await client.post(
            "/events",
            json={
                **_owner_event(EVENT_TYPE_STREAM, "e56943f3-3300-5d07-9f07-2841b4308c36"),
                "meta": {"version": 1, "producer": producer_id, "previous": 8, "sequence": 9},
            },
        )
        assert seed.status_code == 202
        assert seed.content == b""

        batch = {
            "events": [
                {
                    **_owner_event(EVENT_TYPE_STREAM, "21f531bc-1e78-5431-9140-f064e8f4c848"),
                    "meta": {"version": 1, "producer": producer_id, "previous": 9, "sequence": 10},
                },
                {
                    **_owner_event(EVENT_TYPE_STREAM, "ad84fe68-655e-5183-9818-bf6569253e52"),
                    "meta": {"version": 1, "producer": producer_id, "previous": 10, "sequence": 11},
                },
                {
                    **_owner_event(EVENT_TYPE_STREAM, "ae8cae77-3502-5631-b1b2-70939be4de2d"),
                    "meta": {"version": 1, "producer": producer_id, "previous": 11, "sequence": 12},
                },
            ]
        }
        first = await client.post("/events:batch", json=batch)
        assert first.status_code == 202
        assert first.content == b""

        resp = await client.post("/events:batch", json=batch)
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 200
    assert resp.content == b""
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [{"topic": TOPIC_STREAM, "last_sequence": 12}],
    }


def _sized_event(tenant_id: str, pad: int) -> dict:
    """A stream event whose ``data.pad`` is ``pad`` bytes, at a fixed time, so its
    encoding is what the broker measures."""
    return {
        **_event(EVENT_TYPE_STREAM, tenant_id),
        "occurred_at": "2026-11-09T10:00:00Z",
        "data": {"pad": "x" * pad},
    }


def _encoded_len(event: dict) -> int:
    """The event's size as the broker counts it: its compact JSON in the SDK's
    ``EventV1`` form, an absent ``subject_owner``, ``trace_parent`` or ``meta``
    written as ``null``."""
    counted = {
        **event,
        "subject_owner": event.get("subject_owner"),
        "trace_parent": event.get("trace_parent"),
        "meta": event.get("meta"),
    }
    return len(json.dumps(counted, separators=(",", ":"), ensure_ascii=False).encode())


async def test_batch_with_an_event_over_the_size_limit_rejected_413(api):
    """scenario: producer/batch/1.11-negative-batch-event-too-large.md"""

    small = {
        **_owner_event(EVENT_TYPE_STREAM, "15a00127-813c-4b62-952f-000000000002"),
        "data": {"pad": "x" * 10},
    }
    over = {
        **_owner_event(EVENT_TYPE_STREAM, "15a00127-813c-4b62-952f-000000000003"),
        "data": {"pad": "x" * 61_440},
    }
    size = _encoded_len(over)
    assert _encoded_len(small) <= 61_440 < size

    async with api() as client:
        first_marker = await client.post(
            "/events", headers={"Prefer": "wait=10"},
            json=_owner_event(EVENT_TYPE_STREAM, "15a00127-813c-4b62-952f-000000000001"),
        )
        assert first_marker.status_code == 201
        assert first_marker.content == b""
        assert first_marker.headers["preference-applied"] == "wait=10"
        before_resp = await client.get(
            "/topics/segments", params={"topic": TOPIC_STREAM, "partition": 0}
        )
        assert before_resp.status_code == 200
        assert before_resp.headers["content-type"] == "application/json"
        before = before_resp.json()
        assert set(before) == {
            "topic",
            "partition",
            "start_sequence",
            "end_sequence",
            "start_time",
            "end_time",
            "segments",
        }
        assert before == {
            "topic": TOPIC_STREAM,
            "partition": 0,
            "start_sequence": before["start_sequence"],
            "end_sequence": before["end_sequence"],
            "start_time": before["start_time"],
            "end_time": before["end_time"],
            "segments": before["segments"],
        }
        assert 0 < before["start_sequence"] <= before["end_sequence"]
        assert isinstance(before["start_time"], str)
        assert isinstance(before["end_time"], str)
        assert isinstance(before["segments"], list)

        resp = await client.post("/events:batch", json={"events": [small, over]})
        assert resp.status_code == 413
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": f"{API_BASE}/events:batch",
            "trace_id": TRACE_ID,
            "context": {
                "field_violations": [
                    {
                        "field": "events[1]",
                        "description": f"{size} bytes encoded, over the limit of 61440",
                        "reason": "event_too_large",
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        last_marker = await client.post(
            "/events", headers={"Prefer": "wait=10"},
            json=_owner_event(EVENT_TYPE_STREAM, "15a00127-813c-4b62-952f-000000000004"),
        )
        assert last_marker.status_code == 201
        assert last_marker.content == b""
        assert last_marker.headers["preference-applied"] == "wait=10"
        after_resp = await client.get(
            "/topics/segments", params={"topic": TOPIC_STREAM, "partition": 0}
        )
        assert after_resp.status_code == 200
        assert after_resp.headers["content-type"] == "application/json"
        after = after_resp.json()
        assert after.keys() == before.keys()
        assert after == {
            "topic": TOPIC_STREAM,
            "partition": 0,
            "start_sequence": before["start_sequence"],
            "end_sequence": before["end_sequence"] + 1,
            "start_time": before["start_time"],
            "end_time": after["end_time"],
            "segments": after["segments"],
        }
        assert isinstance(after["end_time"], str)
        assert isinstance(after["segments"], list)


async def test_batch_with_a_partition_key_over_its_cap_rejected_400(api):
    """scenario: producer/batch/1.12-negative-batch-partition-key-too-long.md

    The batch's second event resolves a partition key over 1024 bytes: the
    whole batch is refused 400, naming that event.
    """
    events = [
        {
            **_owner_event(EVENT_TYPE_KEYED, "615595f5-bf07-569f-8e06-b28cd09437f7"),
            "data": {"k": "order-1"},
        },
        {
            **_owner_event(EVENT_TYPE_KEYED, "4780065a-ea54-5369-a1bb-98ecee056f4e"),
            "data": {"k": "k" * 1025},
        },
    ]
    async with api() as client:
        resp = await client.post("/events:batch", json={"events": events})
    assert resp.status_code == 400
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/events:batch",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "events[1].partition_key",
                    "description": "1025 bytes, over the limit of 1024",
                    "reason": "partition_key_too_long",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_batch_with_one_event_in_an_unreachable_tenant_returns_403(api):
    """scenario: producer/batch/1.13-negative-batch-tenant-not-authorized.md

    The reachable child events surround a sibling-tenant event on one outbox route.
    """

    reachable = "00000000-0000-0000-0000-000000000004"
    unreachable = "00000000-0000-0000-0000-000000000003"
    occurred_at = "2026-05-29T10:00:00Z"
    data = {"k": "batch-tenant"}

    first_event = {
        **_event(
            EVENT_TYPE_KEYED, reachable,
            event_id="15a00129-813c-4b62-952f-000000000001",
            occurred_at=occurred_at,
        ),
        "data": data,
    }
    last_event = {
        **_event(
            EVENT_TYPE_KEYED, reachable,
            event_id="15a00129-813c-4b62-952f-000000000005",
            occurred_at=occurred_at,
        ),
        "data": data,
    }

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
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }

        first_marker = await client.post("/events", headers={"Prefer": "wait=10"}, json=first_event)
        assert first_marker.status_code == 201
        assert first_marker.content == b""
        assert first_marker.headers["preference-applied"] == "wait=10"
        before_resp = await client.get(
            "/topics/segments", params={"topic": TOPIC_KEYED, "partition": 0}
        )
        assert before_resp.status_code == 200
        assert before_resp.headers["content-type"] == "application/json"
        before = before_resp.json()
        assert set(before) == {
            "topic",
            "partition",
            "start_sequence",
            "end_sequence",
            "start_time",
            "end_time",
            "segments",
        }
        assert before == {
            "topic": TOPIC_KEYED,
            "partition": 0,
            "start_sequence": before["start_sequence"],
            "end_sequence": before["end_sequence"],
            "start_time": before["start_time"],
            "end_time": before["end_time"],
            "segments": before["segments"],
        }
        assert 0 < before["start_sequence"] <= before["end_sequence"]
        assert isinstance(before["start_time"], str)
        assert isinstance(before["end_time"], str)
        assert isinstance(before["segments"], list)

        events = [
            {
                **_event(EVENT_TYPE_KEYED, tenant, event_id=event_id, occurred_at=occurred_at),
                "data": data,
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": sequence - 1,
                    "sequence": sequence,
                },
            }
            for sequence, (tenant, event_id) in enumerate(
                [
                    (reachable, "15a00129-813c-4b62-952f-000000000002"),
                    (unreachable, "15a00129-813c-4b62-952f-000000000003"),
                    (reachable, "15a00129-813c-4b62-952f-000000000004"),
                ],
                start=1,
            )
        ]
        resp = await client.post("/events:batch", json={"events": events})
        assert resp.status_code == 403
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/events:batch",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        last_marker = await client.post("/events", headers={"Prefer": "wait=10"}, json=last_event)
        assert last_marker.status_code == 201
        assert last_marker.content == b""
        assert last_marker.headers["preference-applied"] == "wait=10"
        after_resp = await client.get(
            "/topics/segments", params={"topic": TOPIC_KEYED, "partition": 0}
        )
        assert after_resp.status_code == 200
        assert after_resp.headers["content-type"] == "application/json"
        after = after_resp.json()
        assert after.keys() == before.keys()
        assert after == {
            "topic": TOPIC_KEYED,
            "partition": 0,
            "start_sequence": before["start_sequence"],
            "end_sequence": before["end_sequence"] + 1,
            "start_time": before["start_time"],
            "end_time": after["end_time"],
            "segments": after["segments"],
        }
        assert isinstance(after["end_time"], str)
        assert isinstance(after["segments"], list)

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }

        retried = await client.post(
            "/events",
            json={
                **first_event,
                "id": "15a00129-813c-4b62-952f-000000000006",
                "meta": {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1},
            },
        )
        assert retried.status_code == 202
        assert retried.content == b""
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_KEYED, "last_sequence": 1}],
        }


async def test_batch_with_a_chained_event_missing_previous_is_refused_400(api):
    """scenario: producer/batch/1.14-negative-batch-mode-shape-violation.md"""
    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "9e855196-ce0e-53de-8fad-94b1ee286453"),
                        "meta": {
                            "version": 1,
                            "producer": producer_id,
                            "previous": 0,
                            "sequence": 1,
                        },
                    },
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "c4760209-cc91-5fba-98a6-5e9d5bdc1ee9"),
                        "meta": {"version": 1, "producer": producer_id, "sequence": 2},
                    },
                    {
                        **_owner_event(EVENT_TYPE_STREAM, "e8667b98-4142-518a-81a6-5148319fac18"),
                        "meta": {
                            "version": 1,
                            "producer": producer_id,
                            "previous": 2,
                            "sequence": 3,
                        },
                    },
                ]
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400, resp.text
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": (
            "ChainModeFieldsMissing: a chained-mode publish needs "
            "meta.previous and meta.sequence"
        ),
        "instance": f"{API_BASE}/events:batch",
        "trace_id": TRACE_ID,
        "context": {
            "format": (
                "ChainModeFieldsMissing: a chained-mode publish needs "
                "meta.previous and meta.sequence"
            ),
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }
