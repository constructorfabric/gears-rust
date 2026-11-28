"""Single-event publish scenarios (producer/single/1.01-1.09, 1.13).

Each test exercises one publish path:
- 1.01: publish → 202, no body (async by default)
- 1.02: sync publish (Prefer: wait=N): 201 once the backend stores the event,
  202 when the wait elapses first, 200 when the event is a duplicate; a 201's
  event is readable through the consumer path
- 1.03: schema validation failure returns 422 (requires EVENT_TYPE_STRICT)
- 1.04: rate limiting → 429 (xfail: rate limiting not configured in standalone)
- 1.05: read-only ``partition`` field rejected → 400
- 1.06: malformed ``Prefer: wait`` value → 400, nothing enqueued
- 1.07: malformed ``trace_parent`` → 400
- 1.08: event over the 60 KiB per-event limit → 413 ``event_too_large``
- 1.09: partition key over 1024 bytes → 400 ``partition_key_too_long``
- 1.13: event tenant is the nil UUID → 400 ``InvalidTenant``
"""

from __future__ import annotations

import asyncio
import json
import uuid
from datetime import datetime, timezone

import pytest

from .conftest import (
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
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
from .test_consumer_subscriptions import _created_subscription_id
from .test_publish_consume import _read_case_window, _stream_instant


def _base_event(
    tenant_id: str,
    event_type: str = EVENT_TYPE_STREAM,
    *,
    event_id: str | None = None,
    occurred_at: str | None = None,
) -> dict:
    return {
        "id": event_id or str(uuid.uuid4()),
        "type": event_type,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": occurred_at or datetime.now(timezone.utc).isoformat(),
    }


def _owner_event(event_id: str, event_type: str = EVENT_TYPE_STREAM) -> dict:
    return _base_event(
        DEFAULT_TENANT_ID, event_type, event_id=event_id, occurred_at="2026-05-29T10:00:00Z",
    )


async def test_publish_single_async_returns_202(api):
    """scenario: producer/single/1.01-positive-publish-single-async.md"""
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                "id": "a1b2c3d4-0000-0000-0000-000000000001",
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-05-29T10:00:00Z",
            },
        )
    assert resp.status_code == 202
    assert resp.content == b"", "202 Accepted must carry no body"


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_returns_201_once_stored(api):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    Publish is asynchronous by default (202). A producer that wants to block
    until the backend confirms persistence requests it with the standard
    ``Prefer: wait=N`` header (RFC 7240, N capped by configuration). The broker
    holds the response until the backend stores the event and answers
    ``201 Created`` - the backend persists the event well within the wait.
    """
    async with api() as client:
        resp = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json=_owner_event("17269569-424b-5e40-a0da-b0206fc39f5a"),
        )
    assert resp.status_code == 201
    assert resp.content == b""
    # RFC 7240 §3: the broker reports the wait it applied (10s, within the cap),
    # so a client can tell a synchronous answer from an ignored preference.
    assert resp.headers.get("preference-applied") == "wait=10"


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_201_event_is_readable(api, test_env):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    The first manifest read after a stored publish exposes its sequence.
    A consumer joining afterwards can replay the event at that sequence.
    """
    event = {
        "id": "a1b2c3d4-0000-0000-0000-0000000000a2",
        "type": EVENT_TYPE_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "order-service",
        "subject": "order-900",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-05-29T10:05:00Z",
        "data": {"order_id": "order-900", "total_cents": 200},
    }
    marker = {**event, "id": "e4bdf020-852d-580b-9ed8-9f5711279235"}
    params = {"topic": TOPIC_STREAM, "partition": 0}
    async with api() as client:
        fence = await client.post("/events", json=marker, headers={"Prefer": "wait=10"})
        assert (fence.status_code, fence.content, fence.headers.get("preference-applied")) == (
            201, b"", "wait=10",
        )
        before = await client.get("/topics/segments", params=params)
        assert (before.status_code, before.headers.get("content-type")) == (200, "application/json")
        before_body = before.json()
        assert set(before_body) == {
            "topic", "partition", "start_sequence", "end_sequence",
            "start_time", "end_time", "segments",
        }
        assert (before_body["topic"], before_body["partition"]) == (TOPIC_STREAM, 0)
        high_water = before_body["end_sequence"]
        assert type(high_water) is int, before_body
        assert high_water > 0, before_body

        published = await client.post("/events", json=event, headers={"Prefer": "wait=10"})
        assert (
            published.status_code, published.content, published.headers.get("preference-applied")
        ) == (201, b"", "wait=10")
        after = await client.get("/topics/segments", params=params)
        assert (after.status_code, after.headers.get("content-type")) == (200, "application/json")
        after_body = after.json()
        assert set(after_body) == set(before_body)
        assert (after_body["topic"], after_body["partition"]) == (TOPIC_STREAM, 0)
        stored_end = after_body["end_sequence"]
        assert type(stored_end) is int, after_body
        assert stored_end > high_water, (before_body, after_body)

        group = await client.post("/consumer-groups")
        assert (group.status_code, group.headers.get("content-type")) == (201, "application/json")
        location = group.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{prefix}{uuid.UUID(group_id.removeprefix(prefix))}"
        assert location == f"{API_BASE}/consumer-groups/{group_id}"
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
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1,
                "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]},
            },
        )
        assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
        seek_body = seek.json()
        match seek_body:
            case {"positions": dict(resolved)}:
                match resolved.get(TOPIC_STREAM):
                    case [{"value": earliest}]:
                        assert type(earliest) is int, seek_body
                    case _:
                        raise AssertionError(seek_body)
            case _:
                raise AssertionError(seek_body)
        assert earliest >= 0, seek_body
        assert earliest < high_water, (seek_body, before_body)
        assert seek_body == {"positions": {TOPIC_STREAM: [{"partition": 0, "value": earliest}]}}

        deadline = asyncio.get_running_loop().time() + 7
        async with asyncio.timeout_at(deadline), client.stream(
            "GET", "/events:sse", params={"subscription": sub_id},
            headers={"Accept": "text/event-stream"},
        ) as stream:
            assert (stream.status_code, stream.headers.get("content-type")) == (
                200, "text/event-stream",
            )
            reader = SseFrameReader(stream)
            topology = await reader.next_frame(timeout=deadline - asyncio.get_running_loop().time())
            match topology:
                case ("topology", {"assigned": [{
                    "offset": offset, "last_examined": last_examined,
                }]}):
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
                        "offset": earliest, "last_examined": earliest,
                    }],
                },
            )
            history, delivered = await _read_case_window(reader, high_water, event["id"], deadline)

    previous = earliest
    for frame in history:
        payload = frame["payload"]
        assert payload["id"] != event["id"], frame
        assert payload["sequence"] > previous, (frame, previous)
        previous = payload["sequence"]
    assert history, "earliest replay must include the durable marker"
    last = history[-1]["payload"]
    assert (last["id"], last["sequence"]) == (marker["id"], high_water)

    assert len(delivered) == 1, delivered
    payload = delivered[0]["payload"]
    assert _stream_instant(payload.get("occurred_at")) == _stream_instant(event["occurred_at"])
    _stream_instant(payload.get("sequence_time"))
    assert delivered == [{
        "kind": "event",
        "payload": {
            **event, "occurred_at": payload["occurred_at"], "trace_parent": None,
            "partition": 0, "sequence": stored_end, "sequence_time": payload["sequence_time"],
        },
    }]


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
    async with api() as client:
        resp = await client.post(
            "/events",
            headers={"Prefer": "wait=0"},
            json=_owner_event("9e1a73ef-11ac-524d-b0cc-2cb928a91050"),
        )
    assert resp.status_code in (201, 202)
    assert resp.content == b""
    assert resp.headers.get("preference-applied") == "wait=0"


@pytest.mark.backend_sensitive
async def test_publish_prefer_wait_duplicate_returns_200(api):
    """scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md

    A synchronous publish of an idempotent duplicate (a lost-ack resend at the
    producer chain's head) is answered ``200 OK`` at once: nothing is written,
    so there is nothing to wait on.
    """
    async with api() as client:
        reg = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test/1.0"},
        )
        assert reg.status_code == 201
        producer_id = reg.json()["id"]
        event = {
            **_owner_event("df757b70-168b-5d68-8973-83ee48a9e777"),
            "meta": {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1},
        }

        first = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json=event,
        )
        assert first.status_code == 201, "the first publish is stored within the wait"
        assert first.content == b""
        assert first.headers.get("preference-applied") == "wait=10"

        resend = await client.post(
            "/events",
            headers={"Prefer": "wait=10"},
            json=event,
        )
    assert resend.status_code == 200
    assert resend.content == b""
    assert resend.headers.get("preference-applied") == "wait=10"


async def test_schema_validation_failure_returns_422(api):
    """scenario: producer/single/1.03-negative-schema-validation-failure.md

    EVENT_TYPE_STRICT is registered with a data schema that requires
    ``strict_field`` (string).  Publishing without it fails validation: the
    body parsed, its ``data`` does not satisfy the type's schema, so the
    answer is 422 ``SchemaViolation`` (``docs/openapi.yaml``).
    """
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                **_owner_event("6ab57cbf-2034-5fae-8967-271de490d04d", EVENT_TYPE_STRICT),
                "data": {"missing_the_required_field": True},
            },
        )
    assert resp.status_code == 422
    assert resp.headers["content-type"] == "application/problem+json"
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
    """scenario: producer/single/1.06-negative-prefer-wait-malformed.md"""

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

        event = {
            **_owner_event("15a00159-813c-4b62-952f-000000000001"),
            "meta": {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1},
        }
        resp = await client.post("/events", headers={"Prefer": "wait=soon"}, json=event)
        detail = "InvalidHeader: Prefer: wait expects a non-negative whole number of seconds"
        assert resp.status_code == 400
        assert resp.headers["content-type"] == "application/problem+json"
        assert resp.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": detail,
            "instance": f"{API_BASE}/events",
            "trace_id": TRACE_ID,
            "context": {
                "format": detail,
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
        retried = await client.post("/events", json=event)
        assert retried.status_code == 202
        assert retried.content == b""
        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [{"topic": TOPIC_STREAM, "last_sequence": 1}],
        }


async def test_malformed_trace_parent_returns_400(api):
    """scenario: producer/single/1.07-negative-invalid-trace-parent.md"""

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

        event = {
            **_owner_event("15a00160-813c-4b62-952f-000000000001"),
            "trace_parent": "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
            "meta": {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1},
        }
        resp = await client.post("/events", json=event)
        assert resp.status_code == 400
        assert resp.headers["content-type"] == "application/problem+json"
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
                        "field": "trace_parent",
                        "description": (
                        "must be a W3C traceparent: four lowercase hex groups"
                        " of 2, 32, 16 and 2 digits joined by '-'"
                    ),
                        "reason": "invalid_event_field",
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
    """scenario: producer/single/1.08-negative-event-too-large.md"""

    event = {
        **_owner_event("15a00161-813c-4b62-952f-000000000002"),
        "data": {"pad": "x" * MAX_ENCODED_BYTES},
    }
    size = _encoded_len(event)
    assert size > MAX_ENCODED_BYTES

    async with api() as client:
        first_marker = await client.post(
            "/events", headers={"Prefer": "wait=10"},
            json=_owner_event("15a00161-813c-4b62-952f-000000000001"),
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

        resp = await client.post("/events", json=event)
        assert resp.status_code == 413
        assert resp.headers["content-type"] == "application/problem+json"
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
                        "description": f"{size} bytes encoded, over the limit of 61440",
                        "reason": "event_too_large",
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        }

        last_marker = await client.post(
            "/events", headers={"Prefer": "wait=10"},
            json=_owner_event("15a00161-813c-4b62-952f-000000000003"),
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


async def test_a_partition_key_over_its_cap_returns_400(api):
    """scenario: producer/single/1.09-negative-partition-key-too-long.md

    An event whose partition key (``data.k`` on the keyed type) is over 1024
    bytes is refused 400, naming the key's size and the cap, never the key.
    """
    event = {
        **_owner_event("f5e7b372-73c6-5a8b-b031-3e2e0f1ca785", EVENT_TYPE_KEYED),
        "data": {"k": "k" * (MAX_PARTITION_KEY_BYTES + 1)},
    }
    async with api() as client:
        resp = await client.post("/events", json=event)
    assert resp.status_code == 400
    assert resp.headers["content-type"] == "application/problem+json"
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
    """scenario: producer/single/1.13-negative-publish-nil-tenant.md"""

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

        event = {
            **_owner_event("15a00163-813c-4b62-952f-000000000001"),
            "subject_tenant": "00000000-0000-0000-0000-000000000000",
            "meta": {"version": 1, "producer": producer_id, "previous": 0, "sequence": 1},
        }
        resp = await client.post("/events", json=event)
        assert resp.status_code == 400
        assert resp.headers["content-type"] == "application/problem+json"
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

        cursors = await client.get(f"/producers/{producer_id}/cursors")
        assert cursors.status_code == 200
        assert cursors.headers["content-type"] == "application/json"
        assert cursors.json() == {
            "id": producer_id,
            "client_agent": "e2e-test/1.0",
            "topics": [],
        }
