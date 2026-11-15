"""Single-event publish scenarios (producer/single/1.01-1.09, 1.13).

Each test exercises one publish path:
- 1.01: publish → 202, no body (async by default)
- 1.02: sync publish (Prefer: wait=N): 201 once the backend stores the event,
  202 when the wait elapses first, 200 when the event is a duplicate; a 201's
  event is readable through the consumer path
- 1.03: schema validation failure → 400 (requires EVENT_TYPE_STRICT)
- 1.04: rate limiting → 429 (xfail: rate limiting not configured in standalone)
- 1.05: read-only ``partition`` field rejected → 400
- 1.06: malformed ``Prefer: wait`` value → 400, nothing enqueued
- 1.07: malformed ``trace_parent`` → 400
- 1.08: event over the 60 KiB per-event limit → 413 ``event_too_large``
- 1.09: partition key over 1024 bytes → 400 ``partition_key_too_long``
- 1.13: event tenant is the nil UUID → 400 ``InvalidTenant``
"""

from __future__ import annotations

import json
import uuid
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    API_BASE,
    EVENT_TYPE_KEYED,
    SKIP_LIMITS,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_STRICT,
    SUBJECT_TYPE,
    TOPIC_STREAM,
    TOPIC_STRICT,
    TRACE_ID,
    SseFrameReader,
)


def _base_event(tenant_id: str, event_type: str = EVENT_TYPE_STREAM) -> dict:
    return {
        "id": str(uuid.uuid4()),
        "type": event_type,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": datetime.now(timezone.utc).isoformat(),
    }


async def test_publish_single_async_returns_202(api):
    """scenario: producer/single/1.01-positive-publish-single-async.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post("/events", json=_base_event(tenant_id))
    assert resp.status_code == 202
    assert resp.text == "", "202 Accepted must carry no body"


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_returns_201_once_stored(api):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    Publish is asynchronous by default (202). A producer that wants to block
    until the backend confirms persistence requests it with the standard
    ``Prefer: wait=N`` header (RFC 7240, N capped by configuration). The broker
    holds the response until the backend stores the event and answers
    ``201 Created`` - the backend persists the event well within the wait.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json=_base_event(tenant_id),
        )
    assert resp.status_code == 201
    assert resp.text == "", "201 Created must carry no body"
    # RFC 7240 §3: the broker reports the wait it applied (10s, within the cap),
    # so a client can tell a synchronous answer from an ignored preference.
    assert resp.headers.get("preference-applied") == "wait=10"


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_201_event_is_readable(api, test_env):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    Feature 0007 S2: once a synchronous publish is answered ``201`` the event is
    stored and MUST be readable. Publish with ``Prefer: wait``, assert ``201``,
    then read it back through the consumer path - proof the ingest outbox's
    ``Flush::Sync`` append made the event's position visible, not merely durable.
    """
    tenant_id = str(uuid.uuid4())
    event_id = str(uuid.uuid4())
    occurred_at = datetime.now(timezone.utc).isoformat()
    async with api() as client:
        published = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json={
                "id": event_id,
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": tenant_id,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": occurred_at,
            },
        )
        assert published.status_code == 201, "the event is stored within the wait"

        group_id = (await client.post("/consumer-groups")).json()["id"]
        sub = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {"topic": TOPIC_STREAM, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_STREAM]}
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
        assert seek.status_code == 200

    # Read from earliest: the event answered 201 before the consumer joined, so
    # it must already be on the backend and delivered here.
    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            topology_kind, _ = await reader.next_frame(timeout=5)
            assert topology_kind == "topology"
            event_kind, event_data = await reader.next_frame(timeout=15)
    assert event_kind == "event"
    assert event_data["payload"]["id"] == event_id


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_zero_is_201_or_202(api):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    ``Prefer: wait=0`` asks the broker to hold for zero seconds. On a live
    backend this races the drain: the event is durably enqueued and usually not
    stored within a zero wait (``202 Accepted``, stored out of band), but a fast
    drain can land first (``201 Created``). Either is correct here; both carry no
    body. This is a fast live-backend smoke check - the *deterministic* timed-out
    case (backend unreachable) is
    ``test_backend_outage.py::test_sync_publish_times_out_to_202_when_the_backend_is_down``.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            headers={"Prefer": "wait=0"},
            json=_base_event(tenant_id),
        )
    assert resp.status_code in (201, 202)
    assert resp.text == "", "a 201/202 for a sync publish carries no body"


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_duplicate_returns_200(api):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    A synchronous publish of an idempotent duplicate (a lost-ack resend at the
    producer chain's head) is answered ``200 OK`` at once: nothing is written,
    so there is nothing to wait on.
    """
    tenant_id = str(uuid.uuid4())
    event_id = str(uuid.uuid4())
    meta = {"version": 1, "producer": None, "previous": 0, "sequence": 1}
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        meta["producer"] = reg.json()["id"]

        first = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json={**_base_event(tenant_id), "id": event_id, "meta": meta},
        )
        assert first.status_code == 201, "the first publish is stored within the wait"

        # Lost-ack resend of the chain head: a duplicate, answered 200 without waiting.
        resend = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json={**_base_event(tenant_id), "id": event_id, "meta": meta},
        )
    assert resend.status_code == 200
    assert resend.text == "", "200 OK must carry no body"


async def test_schema_validation_failure_returns_422(api):
    """scenario: producer/single/1.03-negative-schema-validation-failure.md

    EVENT_TYPE_STRICT is registered with a data schema that requires
    ``strict_field`` (string).  Publishing without it fails validation: the
    body parsed, its ``data`` does not satisfy the type's schema, so the
    answer is 422 ``SchemaViolation`` (``docs/openapi.yaml``).
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                **_base_event(tenant_id, event_type=EVENT_TYPE_STRICT),
                "data": {"missing_the_required_field": True},
            },
        )
    assert resp.status_code == 422
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 422,
        "detail": "Request validation failed",
        "instance": "/event-broker/v1/events",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "description": "event data does not satisfy the event type's data schema",
                    "field": "(payload)",
                    "reason": "schema_validation",
                },
            ],
            "resource_name": TOPIC_STRICT,
            "resource_type": "gts.cf.core.events.topic.v1~",
        },
    }


@SKIP_LIMITS
async def test_rate_limited_returns_429(api):
    """scenario: producer/single/1.04-negative-rate-limited.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = None
        for _ in range(500):
            resp = await client.post("/events", json=_base_event(tenant_id))
            if resp.status_code == 429:
                break
    assert resp is not None and resp.status_code == 429
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
        "title": "Resource Exhausted",
        "status": 429,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }
    assert "retry-after" in {k.lower() for k in resp.headers}


async def test_readonly_partition_rejected_returns_400(api):
    """scenario: producer/single/1.05-negative-readonly-partition-rejected.md

    ``partition`` is a consumer-facing read-side field; a producer supplying it
    on publish gets a 400 Bad Request.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                **_base_event(tenant_id),
                "partition": 0,  # read-only field; must be rejected
            },
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


async def test_prefer_wait_malformed_returns_400(api):
    """scenario: producer/single/1.06-negative-prefer-wait-malformed.md

    A ``Prefer: wait`` value that is not a non-negative whole number of seconds
    is a malformed preference: the broker answers ``400``, it does not fall back
    to async or to the cap. (A numeric value above the cap is not malformed - it
    clamps - so only a non-numeric token is rejected here.)
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            headers={"Prefer": "wait=soon"},
            json=_base_event(tenant_id),
        )
    assert resp.status_code == 400
    assert resp.json()["type"] == (
        "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~"
    )


async def test_malformed_trace_parent_returns_400(api):
    """scenario: producer/single/1.07-negative-invalid-trace-parent.md

    A ``trace_parent`` that is not a W3C traceparent (here, uppercase hex) is
    rejected with 400. The violation names the field and the rule, never the
    value.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                **_base_event(tenant_id),
                "trace_parent": "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
            },
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "trace_parent",
                    "description": (
                        "must be a W3C traceparent: four lowercase hex groups"
                        " of 2, 32, 16 and 2 digits joined by '-'"
                    ),
                    "reason": "invalid_event_field",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


# The per-event limit and the partition-key cap, as `EventV1` states them.
MAX_ENCODED_BYTES = 61_440
MAX_PARTITION_KEY_BYTES = 1024


def _sized_event(tenant_id: str, pad: int) -> dict:
    """A stream event whose ``data.pad`` is ``pad`` bytes, at a fixed time, so its
    encoding is what the broker measures."""
    return {
        **_base_event(tenant_id),
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


async def test_an_event_over_the_size_limit_returns_413(api):
    """scenario: producer/single/1.08-negative-event-too-large.md

    An event whose encoding is over 60 KiB is refused 413, naming its size and
    the limit, never its content.
    """
    event = _sized_event(str(uuid.uuid4()), MAX_ENCODED_BYTES)
    size = _encoded_len(event)
    async with api() as client:
        resp = await client.post("/events", json=event)
    assert resp.status_code == 413
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 413,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "events[0]",
                    "description": f"{size} bytes encoded, over the limit of {MAX_ENCODED_BYTES}",
                    "reason": "event_too_large",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_a_partition_key_over_its_cap_returns_400(api):
    """scenario: producer/single/1.09-negative-partition-key-too-long.md

    An event whose partition key (``data.k`` on the keyed type) is over 1024
    bytes is refused 400, naming the key's size and the cap, never the key.
    """
    event = {
        **_base_event(str(uuid.uuid4()), EVENT_TYPE_KEYED),
        "data": {"k": "k" * (MAX_PARTITION_KEY_BYTES + 1)},
    }
    async with api() as client:
        resp = await client.post("/events", json=event)
    assert resp.status_code == 400
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "events[0].partition_key",
                    "description": "1025 bytes, over the limit of 1024",
                    "reason": "partition_key_too_long",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_publish_with_a_nil_tenant_returns_400(api):
    """scenario: producer/single/1.13-negative-publish-nil-tenant.md

    An event whose ``subject_tenant`` is the nil UUID is refused 400 before any
    authorization call; the event is not admitted.
    """
    event = _base_event("00000000-0000-0000-0000-000000000000")
    async with api() as client:
        resp = await client.post("/events", json=event)
    assert resp.status_code == 400
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidTenant: event 0: the event's tenant must not be the nil UUID",
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidTenant: event 0: the event's tenant must not be the nil UUID",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
