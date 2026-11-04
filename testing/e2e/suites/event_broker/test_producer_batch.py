"""Batch publish scenarios (producer/batch/1.01-1.09).

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
"""

from __future__ import annotations

import uuid
from datetime import datetime, timezone

from .conftest import (
    API_BASE,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_LONGPOLL,
    EVENT_TYPE_STRICT,
    SUBJECT_TYPE,
    TOPIC_STREAM,
    TOPIC_LONGPOLL,
    TOPIC_STRICT,
    TRACE_ID,
)


def _event(event_type: str, tenant_id: str, subject: str = "s1") -> dict:
    return {
        "id": str(uuid.uuid4()),
        "type": event_type,
        "tenant_id": tenant_id,
        "source": "e2e-test",
        "subject": subject,
        "subject_type": SUBJECT_TYPE,
        "occurred_at": datetime.now(timezone.utc).isoformat(),
    }


async def test_batch_publish_returns_202(api):
    """scenario: producer/batch/1.01-positive-publish-batch.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    _event(EVENT_TYPE_STREAM, tenant_id, "s1"),
                    _event(EVENT_TYPE_STREAM, tenant_id, "s2"),
                ]
            },
        )
    assert resp.status_code == 202
    assert resp.text == "", "202 Accepted must carry no body"


async def test_batch_mixing_topics_rejected_400(api):
    """scenario: producer/batch/1.02-negative-mixed-topic-batch.md

    TOPIC_STREAM and TOPIC_LONGPOLL are different topics; mixing event types
    from different topics in one batch is rejected 400.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    _event(EVENT_TYPE_STREAM, tenant_id),
                    _event(EVENT_TYPE_LONGPOLL, tenant_id),
                ]
            },
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
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


async def test_batch_too_large_rejected_413(api):
    """scenario: producer/batch/1.03-negative-batch-too-large.md

    101 events in one batch exceeds the 100-event limit; the broker returns 413.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            json={"events": [_event(EVENT_TYPE_STREAM, tenant_id) for _ in range(101)]},
        )
    assert resp.status_code == 413
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 413,
        "detail": "batch too large: 101 events (max 100)",
        "instance": f"{API_BASE}/events:batch",
        "trace_id": TRACE_ID,
        "context": {
            "format": "batch too large: 101 events (max 100)",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_batch_late_validation_failure_rejected_422(api):
    """scenario: producer/batch/1.04-negative-batch-late-validation-failure.md

    A batch is all-or-nothing: the first event is valid on its own, and the
    second one's payload fails its type's schema, so neither is admitted.

    The failure has to be a *payload* violation for this to test what the
    scenario describes.  EVENT_TYPE_STRICT requires ``data.strict_field``, so
    omitting it is exactly that, and it resolves to ``422`` - envelope and
    read-only violations are the ``400`` family and are covered separately by
    the single-publish read-only test.
    """
    tenant_id = str(uuid.uuid4())
    valid = {**_event(EVENT_TYPE_STRICT, tenant_id), "data": {"strict_field": "ok"}}
    invalid = {**_event(EVENT_TYPE_STRICT, tenant_id), "data": {}}
    async with api() as client:
        resp = await client.post(
            "/events:batch",
            json={"events": [valid, invalid]},
        )
    assert resp.status_code == 422
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

    import asyncio

    # A slow drain can only hide an admitted event, never invent one, so this fails only on a leak.
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
        resp = await client.post("/events:batch", json={"events": []})
    assert resp.status_code == 400
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
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_batch_repeating_an_event_id_is_refused_naming_the_repeat_400(api):
    """scenario: producer/batch/1.06-negative-batch-duplicate-event-id.md"""
    tenant_id = str(uuid.uuid4())
    repeated_id = str(uuid.uuid4())
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
                        **_event(EVENT_TYPE_STREAM, tenant_id),
                        "id": repeated_id,
                        "meta": {"version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1},
                    },
                    {
                        **_event(EVENT_TYPE_STREAM, tenant_id),
                        "id": repeated_id,
                        "meta": {"version": 1, "producer_id": producer_id, "previous": 1, "sequence": 2},
                    },
                ]
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400
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
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "producer_id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_batch_with_a_chained_run_that_does_not_link_is_refused_as_malformed_run_400(api):
    """scenario: producer/batch/1.07-negative-batch-malformed-run.md"""
    tenant_id = str(uuid.uuid4())
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
                        **_event(EVENT_TYPE_STREAM, tenant_id),
                        "meta": {"version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1},
                    },
                    {
                        **_event(EVENT_TYPE_STREAM, tenant_id),
                        "meta": {"version": 1, "producer_id": producer_id, "previous": 2, "sequence": 3},
                    },
                ]
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 400
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
                    "description": "does not follow the same producer's event before it in the batch",
                    "reason": "malformed_run",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "producer_id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [],
    }


async def test_batch_with_a_run_behind_the_committed_head_is_refused_naming_the_head_412(api):
    """scenario: producer/batch/1.08-negative-batch-sequence-violation.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        seed = await client.post(
            "/events",
            json={
                **_event(EVENT_TYPE_STREAM, tenant_id),
                "meta": {"version": 1, "producer_id": producer_id, "previous": 6, "sequence": 7},
            },
        )
        assert seed.status_code == 202

        resp = await client.post(
            "/events:batch",
            json={
                "events": [
                    {
                        **_event(EVENT_TYPE_STREAM, tenant_id),
                        "meta": {"version": 1, "producer_id": producer_id, "previous": 3, "sequence": 4},
                    },
                    {
                        **_event(EVENT_TYPE_STREAM, tenant_id),
                        "meta": {"version": 1, "producer_id": producer_id, "previous": 4, "sequence": 5},
                    },
                ]
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 412
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
                        f"index=0 producer_id={producer_id} topic={TOPIC_STREAM} last_sequence=7"
                    ),
                }
            ],
        },
    }
    assert cursors.status_code == 200
    assert cursors.json() == {
        "producer_id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [{"topic": TOPIC_STREAM, "last_sequence": 7}],
    }


async def test_batch_resent_after_a_lost_ack_answers_200_and_keeps_the_cursor(api):
    """scenario: producer/batch/1.09-positive-batch-all-duplicates.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        reg = await client.post(
            "/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"}
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]

        seed = await client.post(
            "/events",
            json={
                **_event(EVENT_TYPE_STREAM, tenant_id),
                "meta": {"version": 1, "producer_id": producer_id, "previous": 8, "sequence": 9},
            },
        )
        assert seed.status_code == 202

        batch = {
            "events": [
                {
                    **_event(EVENT_TYPE_STREAM, tenant_id),
                    "meta": {"version": 1, "producer_id": producer_id, "previous": 9, "sequence": 10},
                },
                {
                    **_event(EVENT_TYPE_STREAM, tenant_id),
                    "meta": {"version": 1, "producer_id": producer_id, "previous": 10, "sequence": 11},
                },
                {
                    **_event(EVENT_TYPE_STREAM, tenant_id),
                    "meta": {"version": 1, "producer_id": producer_id, "previous": 11, "sequence": 12},
                },
            ]
        }
        first = await client.post("/events:batch", json=batch)
        assert first.status_code == 202

        resp = await client.post("/events:batch", json=batch)
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 200
    assert resp.text == "", "200 OK must carry no body"
    assert cursors.status_code == 200
    assert cursors.json() == {
        "producer_id": producer_id,
        "client_agent": "e2e-test/1.0",
        "topics": [{"topic": TOPIC_STREAM, "last_sequence": 12}],
    }
