"""Streaming transport scenarios (consumer/stream/1.01-1.20).

Skips:
- 1.07 and 1.08 are already covered by ``test_stream_guardrails.py``
- 1.09 (SSE happy path) is already covered by ``test_publish_consume.py``
- 1.06 (terminated subscription via shard shutdown) is xfail: triggering an
  internal shard teardown is not possible from outside in E2E.
- 1.14 (control-progress-frame) runs on TOPIC_PROGRESS: a producer fills it with
  events another tenant's subscription rejects, past the drift threshold the
  generated config sets.
- 1.17 (only-mine-delivery) runs on TOPIC_OWNERSHIP and 1.19 (subtree-delivery)
  on TOPIC_SUBTREE: one partition each, written by that test alone. 1.19 is
  xfail: the suite runs with auth disabled and has no tenant tree.
- 1.18 (subject-types-selector) runs on TOPIC_SUBJECT_SELECTOR: one partition,
  and an event type admitting two subject types.

Patterns:
- Multipart (``/events:stream``) tests use ``MultipartReader`` from conftest.
- SSE tests use ``SseFrameReader`` from conftest.
- Every streaming test opens a fresh subscription on its own group/tenant_id.
"""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    EVENT_TYPE_OWNERSHIP,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_SUBTREE,
    group_seek_positions,
    SUBJECT_TYPE,
    TOPIC_STREAM,
    TOPIC_4P,
    EVENT_TYPE_4P,
    EVENT_TYPE_PROGRESS,
    EVENT_TYPE_SUBJECT_SELECTOR,
    PROGRESS_DRIFT_THRESHOLD,
    SUBJECT_TYPE_OTHER,
    TOPIC_PROGRESS,
    TOPIC_OWNERSHIP,
    TOPIC_SUBJECT_SELECTOR,
    TOPIC_SUBTREE,
    MultipartReader,
    SseFrameReader,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TRACE_ID,
    XFAIL_AUTH_DISABLED,
    XFAIL_UNLESS_STATIC_TOKENS,
)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


async def _setup_seeded_sub(client, topic: str = TOPIC_STREAM, event_type: str = EVENT_TYPE_STREAM) -> tuple[str, str]:
    """Create group → JOIN → SEEK earliest.  Returns (tenant_id, sub_id)."""
    group_resp = await client.post("/consumer-groups")
    assert group_resp.status_code == 201
    group_id = group_resp.json()["id"]

    tenant_id = str(uuid.uuid4())
    sub_resp = await client.post(
        "/subscriptions",
        json={
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [{"topic": topic, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [event_type]}],
        },
    )
    assert sub_resp.status_code == 201
    sub_id = str(sub_resp.json()["id"])
    assigned = sub_resp.json()["assigned"]

    positions = [
        {"topic": a["topic"], "partition": a["partition"], "value": "earliest"}
        for a in assigned
    ]
    seek_resp = await client.post(
        f"/subscriptions/{sub_id}:seek",
        json={"topology_version": sub_resp.json()["topology_version"], "positions": group_seek_positions(positions)},
    )
    assert seek_resp.status_code == 200
    return tenant_id, sub_id


@pytest.mark.backend_sensitive
@pytest.mark.timeout(45, func_only=True)
async def test_multipart_stream_delivers_event_frame(api, test_env):
    """scenario: consumer/stream/1.01-positive-stream-multipart-frames.md

    GET /events:stream with ``Accept: multipart/mixed`` returns a multipart
    response.  The first frame is a ``topology`` frame, then events arrive as
    ``event`` frames.
    """
    async with api() as client:
        tenant_id, sub_id = await _setup_seeded_sub(client)

    event_id = str(uuid.uuid4())
    occurred_at = _now()
    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET",
            f"/events:stream?subscription={sub_id}",
            headers={"Accept": "multipart/mixed"},
        ) as stream_resp:
            assert stream_resp.status_code == 200
            ct = stream_resp.headers.get("content-type", "")
            assert "multipart/mixed" in ct

            reader = MultipartReader(stream_resp)
            topology = await reader.await_kind("topology")
            assert "topology_version" in topology
            assert "assigned" in topology

            # Publish an event and verify it arrives as a multipart frame.
            async with api() as pub_client:
                pub_resp = await pub_client.post(
                    "/events",
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
            assert pub_resp.status_code == 202

            event_frame = await reader.await_kind("event")

    payload = event_frame["payload"]
    assert event_frame == {
        "kind": "event",
        "payload": {
            "id": event_id,
            "type": EVENT_TYPE_STREAM,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": payload["sequence"],
            "sequence_time": payload["sequence_time"],
        },
    }


@pytest.mark.backend_sensitive
@pytest.mark.timeout(20, func_only=True)
async def test_heartbeat_frame_arrives_on_idle_stream(api, test_env):
    """scenario: consumer/stream/1.02-positive-stream-heartbeat-cadence.md

    An idle SSE stream emits a ``heartbeat`` frame approximately every 5 s.
    The test waits up to 12 s for one heartbeat frame after the topology frame.
    """
    async with api() as client:
        tenant_id, sub_id = await _setup_seeded_sub(client)

    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            kind, _ = await reader.next_frame(timeout=5)
            assert kind == "topology"

            # The next frame should be a heartbeat within ~10 s.
            kind, data = await reader.next_frame(timeout=12)
            assert kind == "heartbeat"
            assert "at" in data


@pytest.mark.timeout(20, func_only=True)
async def test_topology_frame_on_rebalance(api, test_env):
    """scenario: consumer/stream/1.03-positive-stream-topology-frame-on-rebalance.md

    A mid-stream JOIN by a second member triggers a topology frame on the first
    member's open stream.  Uses TOPIC_4P so there are enough partitions to
    split across two members.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub1_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-1",
                "interests": [{"topic": TOPIC_4P, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_4P]}],
            },
        )
        assert sub1_resp.status_code == 201
        sub1_id = str(sub1_resp.json()["id"])
        assigned = sub1_resp.json()["assigned"]

        seek_resp = await client.post(
            f"/subscriptions/{sub1_id}:seek",
            json={
                "topology_version": 1, "positions": group_seek_positions({"topic": a["topic"], "partition": a["partition"], "value": "earliest"} for a in assigned)
            },
        )
        assert seek_resp.status_code == 200

    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub1_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)

            # Initial topology: sub1 is the sole member and owns all 4
            # partitions. No events have been published to TOPIC_4P in this
            # test, so offset and last_examined are both 0.
            kind, init_data = await reader.next_frame(timeout=5)
            assert kind == "topology"
            assert init_data["topology_version"] == 1
            assert sorted(init_data["assigned"], key=lambda a: a["partition"]) == [
                {"topic": TOPIC_4P, "partition": p, "offset": 0, "last_examined": 0}
                for p in range(4)
            ]

            # Second member JOINs → triggers rebalance → topology frame.
            async with api() as client2:
                sub2_resp = await client2.post(
                    "/subscriptions",
                    json={
                        "consumer_group": group_id,
                        "client_agent": "e2e-test-2",
                        "interests": [{"topic": TOPIC_4P, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_4P]}],
                    },
                )
            assert sub2_resp.status_code == 201

            # Sub1's stream receives a topology frame reflecting the 2/2
            # partition split. topology_version advances to 2 and exactly 2
            # of the original 4 partitions remain assigned to sub1. Cursors
            # for the retained partitions are unchanged (offset = 0, since no
            # events were delivered before the rebalance).
            kind, rebalance_data = await reader.next_frame(timeout=10)

    assert kind == "topology"
    assert rebalance_data["topology_version"] == 2
    rebalanced = sorted(rebalance_data["assigned"], key=lambda a: a["partition"])
    assert len(rebalanced) == 2
    assert {a["partition"] for a in rebalanced} <= {0, 1, 2, 3}
    assert all(
        a == {"topic": TOPIC_4P, "partition": a["partition"], "offset": 0, "last_examined": 0}
        for a in rebalanced
    )


async def test_positions_not_set_returns_409(api):
    """scenario: consumer/stream/1.04-negative-stream-positions-not-set.md

    Opening a stream without prior SEEK returns 409 PositionsNotSet.
    The error.rs override sets HTTP 409 for this condition.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [{"topic": TOPIC_STREAM, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_STREAM]}],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        # No SEEK performed - stream must reject.
        stream_resp = await client.get(
            f"/events:stream?subscription={sub_id}",
            headers={"Accept": "multipart/mixed"},
        )
    assert stream_resp.status_code == 409
    body = stream_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 409,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "violations": [
                {
                    "type": "positions_not_set",
                    "subject": f"{TOPIC_STREAM}:0",
                    "description": body["context"]["violations"][0]["description"],
                }
            ]
        },
    }


async def test_unknown_subscription_returns_404(api):
    """scenario: consumer/stream/1.05-negative-stream-unknown-subscription.md"""
    fake_sub_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.get(
            f"/events:stream?subscription={fake_sub_id}",
            headers={"Accept": "multipart/mixed"},
        )
    assert resp.status_code == 404
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "resource_name": fake_sub_id,
        },
    }


@pytest.mark.xfail(
    reason="triggering a terminated subscription requires internal shard shutdown not available in E2E",
    strict=False,
)
async def test_terminated_subscription_returns_410(api):
    """scenario: consumer/stream/1.06-negative-stream-terminated-subscription.md

    When the delivery shard shuts down it closes the stream with 410.  Not
    triggerable from the outside in E2E.
    """
    async with api() as client:
        _, sub_id = await _setup_seeded_sub(client)
    async with httpx.AsyncClient(timeout=None) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 410


# 1.07 and 1.08 are covered by test_stream_guardrails.py - not duplicated here.
# 1.09 (SSE happy path) is covered by test_publish_consume.py.


async def test_stream_rejects_unknown_query_params(api):
    """scenario: consumer/stream/1.10-negative-stream-rejects-timeout-collect-params.md

    Legacy ``timeout`` and ``collect`` query parameters are not accepted;
    the endpoint only recognises ``subscription``.
    """
    fake_sub_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.get(
            f"/events:stream?subscription={fake_sub_id}&timeout=20&collect=50",
            headers={"Accept": "multipart/mixed"},
        )
    # The query is checked before the subscription lookup, so a random id
    # still answers 400, naming the first unknown parameter.
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidQuery: unknown field `timeout`, expected `subscription`",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidQuery: unknown field `timeout`, expected `subscription`",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


@pytest.mark.timeout(15, func_only=True)
async def test_second_stream_on_same_subscription_returns_409(api, test_env):
    """scenario: consumer/stream/1.11-negative-streaming-in-progress.md

    A second concurrent stream on the same ``subscription_id`` is rejected 409.
    """
    async with api() as client:
        _, sub_id = await _setup_seeded_sub(client)

    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200

            async with api() as second_client:
                second_resp = await second_client.get(
                    f"/events:sse?subscription={sub_id}",
                )

    assert second_resp.status_code == 409
    body = second_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 409,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "violations": [
                {
                    "type": "streaming_in_progress",
                    "subject": sub_id,
                    "description": body["context"]["violations"][0]["description"],
                }
            ]
        },
    }


@pytest.mark.timeout(15, func_only=True)
async def test_delete_while_streaming_closes_connection(api, test_env):
    """scenario: consumer/stream/1.13-positive-delete-while-streaming.md

    DELETE /subscriptions/{id} while a stream is open terminates the stream
    and returns 204.  No control frame precedes the close.
    """
    async with api() as client:
        _, sub_id = await _setup_seeded_sub(client)

    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200

            # Consume the topology frame.
            reader = SseFrameReader(stream_resp)
            kind, _ = await reader.next_frame(timeout=5)
            assert kind == "topology"

            # DELETE while the stream is open.
            async with api() as del_client:
                del_resp = await del_client.delete(f"/subscriptions/{sub_id}")
            assert del_resp.status_code == 204

    # After DELETE, the subscription is gone.
    async with api() as check_client:
        get_resp = await check_client.get(f"/subscriptions/{sub_id}")
    assert get_resp.status_code == 404


@pytest.mark.timeout(30, func_only=True)
async def test_stream_terminates_with_control_on_rebalance_gain(api, test_env):
    """scenario: consumer/stream/1.12-positive-stream-terminates-on-rebalance-gain.md

    When a group member leaves and the surviving member *gains* partitions, the
    broker terminates the surviving member's open subscription with a
    ``control`` frame with ``code: "terminal"``.  Partition loss emits a
    ``topology`` frame; partition gain emits a terminal ``control`` frame.
    """
    tenant_id = str(uuid.uuid4())
    base_url = f"{test_env.base_url}/event-broker/v1"

    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        # Member A: joins first, gets all 4 partitions.
        sub_a_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [{"topic": TOPIC_4P, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_4P]}],
            },
        )
        assert sub_a_resp.status_code == 201
        sub_a_id = str(sub_a_resp.json()["id"])
        assigned_a = sub_a_resp.json()["assigned"]

        seek_a = await client.post(
            f"/subscriptions/{sub_a_id}:seek",
            json={
                "topology_version": sub_a_resp.json()["topology_version"],
                "positions": group_seek_positions({"topic": p["topic"], "partition": p["partition"], "value": "earliest"} for p in assigned_a),
            },
        )
        assert seek_a.status_code == 200

        # Member B: joins second, triggers rebalance (2+2 split).
        sub_b_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-b",
                "interests": [{"topic": TOPIC_4P, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_4P]}],
            },
        )
        assert sub_b_resp.status_code == 201
        sub_b_id = str(sub_b_resp.json()["id"])
        assigned_b = sub_b_resp.json()["assigned"]

        # B's JOIN rebalanced the group, so A's join-time assignment and the
        # topology_version it was issued at are both stale.  Re-read the
        # subscription for the current pair before re-seeking - a SEEK carrying
        # the pre-rebalance version is fenced off with 412.
        a_current = await client.get(f"/subscriptions/{sub_a_id}")
        assert a_current.status_code == 200
        re_seek_a = await client.post(
            f"/subscriptions/{sub_a_id}:seek",
            json={
                "topology_version": a_current.json()["topology_version"],
                "positions": group_seek_positions({"topic": p["topic"], "partition": p["partition"], "value": "earliest"} for p in a_current.json()["assigned"]),
            },
        )
        assert re_seek_a.status_code == 200, re_seek_a.text

        seek_b = await client.post(
            f"/subscriptions/{sub_b_id}:seek",
            json={
                "topology_version": sub_b_resp.json()["topology_version"],
                "positions": group_seek_positions({"topic": p["topic"], "partition": p["partition"], "value": "earliest"} for p in assigned_b),
            },
        )
        assert seek_b.status_code == 200, seek_b.text

    # Open A's stream.
    async with httpx.AsyncClient(base_url=base_url, timeout=None) as stream_a:
        async with stream_a.stream("GET", f"/events:sse?subscription={sub_a_id}") as resp_a:
            assert resp_a.status_code == 200
            reader_a = SseFrameReader(resp_a)

            # Consume the initial topology.
            kind, _ = await reader_a.next_frame(timeout=5)
            assert kind == "topology"

            # B leaves → A gains partitions → broker terminates A's subscription.
            async with api() as del_client:
                del_resp = await del_client.delete(f"/subscriptions/{sub_b_id}")
            assert del_resp.status_code == 204

            # Gain emits a control/terminal frame (not a topology frame).
            while True:
                kind, ctrl = await reader_a.next_frame(timeout=10)
                if kind != "heartbeat":
                    break
            assert kind == "control"
            assert ctrl["code"] == "terminal"


@pytest.mark.backend_sensitive
@pytest.mark.timeout(45, func_only=True)
async def test_control_progress_frame_emitted_on_filtered_stream(api, test_env):
    """scenario: consumer/stream/1.14-positive-control-progress-frame.md

    A producer appends more than PROGRESS_DRIFT_THRESHOLD events for one tenant;
    a consumer filtering on another tenant scans them all and is delivered
    none. The stream reports where its scan has got to with a ``control``
    frame, ``code: "progress"``: the partition's delivered offset is still the
    seeked 0 and ``last_examined`` reaches the last event's sequence.
    """
    producer_tenant = str(uuid.uuid4())
    count = PROGRESS_DRIFT_THRESHOLD + 100
    async with api() as client:
        # A reader on the producer's tenant, to learn the last event's sequence.
        reader_group = (await client.post("/consumer-groups")).json()["id"]
        reader = await client.post(
            "/subscriptions",
            json={
                "consumer_group": reader_group,
                "client_agent": "e2e-test",
                "interests": [{"topic": TOPIC_PROGRESS, "subject_tenant": producer_tenant, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_PROGRESS]}],
            },
        )
        assert reader.status_code == 201
        reader_sub = reader.json()["id"]
        seek = await client.post(
            f"/subscriptions/{reader_sub}:seek",
            json={"topology_version": 1, "positions": {TOPIC_PROGRESS: [{"partition": 0, "value": "earliest"}]}},
        )
        assert seek.status_code == 200
        for start in range(0, count, 100):
            batch = [
                {
                    "id": str(uuid.uuid4()),
                    "type": EVENT_TYPE_PROGRESS,
                    "subject_tenant": producer_tenant,
                    "source": "e2e-test",
                    "subject": f"s{n}",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": _now(),
                }
                for n in range(start, min(start + 100, count))
            ]
            resp = await client.post("/events:batch", json={"events": batch})
            assert resp.status_code == 202, resp.text
        _, filtered_sub = await _setup_seeded_sub(client, TOPIC_PROGRESS, EVENT_TYPE_PROGRESS)

    base_url = f"{test_env.base_url}/event-broker/v1"
    async with httpx.AsyncClient(base_url=base_url, timeout=None) as stream_client:
        # Every event stored, and the last one's sequence: read them back.
        async with stream_client.stream("GET", f"/events:sse?subscription={reader_sub}") as resp:
            assert resp.status_code == 200
            frames = SseFrameReader(resp)
            await frames.await_kind("topology", timeout=5)
            for _ in range(count):
                head = (await frames.await_kind("event", timeout=15))["payload"]["sequence"]

        async with stream_client.stream("GET", f"/events:sse?subscription={filtered_sub}") as resp:
            assert resp.status_code == 200
            frames = SseFrameReader(resp)
            await frames.await_kind("topology", timeout=5)
            # A report may go out before the scan reaches the head; the next one
            # follows within the minimum interval.
            while True:
                control = await frames.await_kind("control", timeout=15)
                if control["positions"][0]["last_examined"] == head:
                    break
                assert control["positions"][0]["last_examined"] < head, control

    assert control == {
        "kind": "control",
        "code": "progress",
        "positions": [{"topic": TOPIC_PROGRESS, "partition": 0, "offset": 0, "last_examined": head}],
    }


@pytest.mark.timeout(30, func_only=True)
async def test_subject_types_selector(api, test_env):
    """scenario: consumer/stream/1.18-positive-subject-types-selector.md

    An interest whose ``subject_types`` names only SUBJECT_TYPE_OTHER is not
    delivered an event about SUBJECT_TYPE. Event A (SUBJECT_TYPE) is published
    before event B (SUBJECT_TYPE_OTHER) on a one-partition topic, so a stream
    that ignored the selector would deliver A first.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_SUBJECT_SELECTOR,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_SUBJECT_SELECTOR],
                        "subject_types": [SUBJECT_TYPE_OTHER],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        joined = sub_resp.json()
        assert joined["interests"] == [
            {
                "topic": TOPIC_SUBJECT_SELECTOR,
                "subject_tenant": tenant_id,
                "ownership_filter": "entire_tenant",
                "types": [EVENT_TYPE_SUBJECT_SELECTOR],
                "subject_types": [SUBJECT_TYPE_OTHER],
                "barrier_mode": "respect",
                "filter": None,
                "unmatched_types": [],
            }
        ]
        sub_id = joined["id"]
        seek_resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": joined["topology_version"],
                "positions": group_seek_positions(
                    [
                        {"topic": a["topic"], "partition": a["partition"], "value": "earliest"}
                        for a in joined["assigned"]
                    ]
                ),
            },
        )
        assert seek_resp.status_code == 200, seek_resp.text

    event_a = str(uuid.uuid4())
    event_b = str(uuid.uuid4())
    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET",
            f"/events:stream?subscription={sub_id}",
            headers={"Accept": "multipart/mixed"},
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = MultipartReader(stream_resp)
            await reader.await_kind("topology")

            async with api() as pub_client:
                for event_id, subject_type in [
                    (event_a, SUBJECT_TYPE),
                    (event_b, SUBJECT_TYPE_OTHER),
                ]:
                    pub_resp = await pub_client.post(
                        "/events",
                        json={
                            "id": event_id,
                            "type": EVENT_TYPE_SUBJECT_SELECTOR,
                            "subject_tenant": tenant_id,
                            "source": "e2e-test",
                            "subject": "s1",
                            "subject_type": subject_type,
                            "occurred_at": _now(),
                        },
                    )
                    assert pub_resp.status_code == 202, pub_resp.text

            event_frame = await reader.await_kind("event", timeout=15)

    payload = event_frame["payload"]
    assert event_frame == {
        "kind": "event",
        "payload": {
            "id": event_b,
            "type": EVENT_TYPE_SUBJECT_SELECTOR,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE_OTHER,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": payload["sequence"],
            "sequence_time": payload["sequence_time"],
        },
    }

async def _join_seeded(client, interest: dict) -> str:
    """JOINs a fresh group with one interest and SEEKs it to "earliest"; returns
    the subscription id."""
    group_resp = await client.post("/consumer-groups")
    assert group_resp.status_code == 201
    sub_resp = await client.post(
        "/subscriptions",
        json={
            "consumer_group": group_resp.json()["id"],
            "client_agent": "e2e-test",
            "interests": [interest],
        },
    )
    assert sub_resp.status_code == 201, sub_resp.text
    joined = sub_resp.json()
    seek_resp = await client.post(
        f"/subscriptions/{joined['id']}:seek",
        json={
            "topology_version": joined["topology_version"],
            "positions": {interest["topic"]: [{"partition": 0, "value": "earliest"}]},
        },
    )
    assert seek_resp.status_code == 200, seek_resp.text
    return str(joined["id"])


async def _read_events(base_url: str, sub_id: str, count: int) -> list[dict]:
    """Opens the subscription's multipart stream and returns its first ``count``
    event frames."""
    async with httpx.AsyncClient(base_url=f"{base_url}{API_BASE}", timeout=None) as client:
        async with client.stream(
            "GET",
            f"/events:stream?subscription={sub_id}",
            headers={"Accept": "multipart/mixed"},
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = MultipartReader(stream_resp)
            await reader.await_kind("topology")
            return [await reader.await_kind("event", timeout=10) for _ in range(count)]


def _delivered(event: dict, frame: dict) -> dict:
    """The event frame that delivers the published ``event`` on partition 0: the
    published members plus the broker's, with the times the broker set taken
    from ``frame``."""
    payload = frame["payload"]
    return {
        "kind": "event",
        "payload": {
            **event,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": payload["sequence"],
            "sequence_time": payload["sequence_time"],
        },
    }


@pytest.mark.timeout(45, func_only=True)
async def test_only_mine_delivery(api, test_env):
    """scenario: consumer/stream/1.17-positive-only-mine-delivery.md

    Three events of the caller's tenant on a one-partition topic: one with no
    owner, one owned by another subject, then one owned by the caller. A JOIN
    without ``ownership_filter`` is delivered only the caller's event, so the
    two before it were skipped; an ``entire_tenant`` JOIN is delivered all
    three, in order.
    """
    unowned = {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE_OWNERSHIP,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "unowned",
        "subject_type": SUBJECT_TYPE,
    }
    foreign = {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE_OWNERSHIP,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "foreign",
        "subject_type": SUBJECT_TYPE,
        "subject_owner": str(uuid.uuid4()),
    }
    mine = {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE_OWNERSHIP,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "mine",
        "subject_type": SUBJECT_TYPE,
        "subject_owner": DEFAULT_SUBJECT_ID,
    }
    async with api() as client:
        only_mine_sub = await _join_seeded(
            client, {"topic": TOPIC_OWNERSHIP, "types": [EVENT_TYPE_OWNERSHIP]}
        )
        entire_tenant_sub = await _join_seeded(
            client,
            {
                "topic": TOPIC_OWNERSHIP,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "entire_tenant",
                "types": [EVENT_TYPE_OWNERSHIP],
            },
        )
        for event in (unowned, foreign, mine):
            pub_resp = await client.post("/events", json={**event, "occurred_at": _now()})
            assert pub_resp.status_code == 202, pub_resp.text

    only_mine_frames = await _read_events(test_env.base_url, only_mine_sub, 1)
    assert only_mine_frames == [_delivered(mine, only_mine_frames[0])]

    entire_tenant_frames = await _read_events(test_env.base_url, entire_tenant_sub, 3)
    assert entire_tenant_frames == [
        _delivered(event, frame)
        for event, frame in zip((unowned, foreign, mine), entire_tenant_frames)
    ]


@pytest.mark.xfail(reason="no tenant tree: static PDP scope is the caller's tenant only", strict=True)
@pytest.mark.timeout(45, func_only=True)
async def test_subtree_delivery(api, test_env):
    """scenario: consumer/stream/1.19-positive-subtree-delivery.md

    A ``tenant_subtree_included`` interest rooted at the caller's tenant is
    delivered an event of a child tenant and not one of a tenant outside the
    subtree, published first on the same partition. The suite runs with
    ``auth_disabled`` and has no tenant tree, so the broker cannot place either
    tenant under the caller's and the stream does not deliver the child's event
    alone.
    """
    outsider = {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE_SUBTREE,
        "subject_tenant": str(uuid.uuid4()),
        "source": "e2e-test",
        "subject": "outsider",
        "subject_type": SUBJECT_TYPE,
    }
    child = {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE_SUBTREE,
        "subject_tenant": str(uuid.uuid4()),
        "source": "e2e-test",
        "subject": "child",
        "subject_type": SUBJECT_TYPE,
    }
    async with api() as client:
        sub_id = await _join_seeded(
            client,
            {
                "topic": TOPIC_SUBTREE,
                "ownership_filter": "tenant_subtree_included",
                "types": [EVENT_TYPE_SUBTREE],
            },
        )
        for event in (outsider, child):
            pub_resp = await client.post("/events", json={**event, "occurred_at": _now()})
            assert pub_resp.status_code == 202, pub_resp.text

    frames = await _read_events(test_env.base_url, sub_id, 1)
    assert frames == [_delivered(child, frames[0])]


async def _open_status(client, sub_id: str, headers: dict) -> tuple[int, dict | None]:
    """Opens ``:stream`` for ``sub_id`` and closes it at once: the status, and
    the problem body when the open is refused (an open stream never ends)."""
    async with client.stream(
        "GET",
        f"/events:stream?subscription={sub_id}",
        headers={**headers, "Accept": "multipart/mixed"},
    ) as resp:
        if resp.status_code == 200:
            return 200, None
        await resp.aread()
        return resp.status_code, resp.json()


@XFAIL_UNLESS_STATIC_TOKENS
@pytest.mark.timeout(20, func_only=True)
async def test_stream_subscription_not_owned(api):
    """scenario: consumer/stream/1.20-negative-stream-subscription-not-owned.md

    Principal A JOINs a fresh group with one interest on its own tenant (the
    tenant its group records) and SEEKs it; principal B, a subject of the same
    tenant, opens its stream and is refused ``SubscriptionNotOwned``; A's own
    open then answers 200. Under ``auth_disabled`` the gateway ignores both
    tokens and injects one default identity, so B's open answers 200.
    """
    principal_a = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_A}"}
    principal_b = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_B}"}
    async with api() as client:
        group_resp = await client.post("/consumer-groups", headers=principal_a)
        assert group_resp.status_code == 201, group_resp.text
        group = group_resp.json()
        sub_resp = await client.post(
            "/subscriptions",
            headers=principal_a,
            json={
                "consumer_group": group["id"],
                "client_agent": "e2e-test",
                "interests": [
                    {"topic": TOPIC_STREAM, "subject_tenant": group["tenant"], "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_STREAM]}
                ],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        sub_id = str(sub_resp.json()["id"])
        seek_resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            headers=principal_a,
            json={"topology_version": 1, "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]}},
        )
        assert seek_resp.status_code == 200, seek_resp.text

        refused_status, refused_body = await _open_status(client, sub_id, principal_b)
        assert refused_status == 403
        assert refused_body == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/events:stream",
            "trace_id": TRACE_ID,
            "context": {
                "reason": f"subscription '{sub_id}' was joined by another principal",
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        }
        own_status, own_body = await _open_status(client, sub_id, principal_a)
    assert (own_status, own_body) == (200, None)
