"""SEEK / cursor scenarios (consumer/positions/1.01-1.18).

All SEEK requests use the topic-keyed wire format:
``{"topology_version": 1, "positions": {"<topic>": [{"partition": int, "value": ...}]}}``.

The SEEK response mirrors that shape:
``{"positions": {"<topic>": [{"partition": int, "value": int}]}}``
where ``value`` is the resolved cursor (RF-1 for "earliest", the head - the
last stored event's sequence - for "latest").

Every test that asserts a concrete cursor value or a quoted valid range runs
against TOPIC_PREFILLED, which is seeded with PREFILLED_COUNT events into a
fresh medium at session bootstrap. That fixes RF-1 at 0, and the
`prefilled_topic` fixture records the sequence every seeded event got, so the
head is known too rather than whatever a shared topic happens to hold.
Sequences rise but may have gaps, so no position is computed from the count.
"""

from __future__ import annotations

import uuid
from contextlib import AsyncExitStack
import asyncio

import httpx
import pytest

from .conftest import (
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
    API_BASE,
    group_seek_positions,
    EVENT_TYPE_4P,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    EVENT_TYPE_PREFILLED,
    TOPIC_4P,
    PREFILLED_TENANT_ID,
    TOPIC_STREAM,
    TOPIC_PREFILLED,
    SseFrameReader,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TRACE_ID,
)
from .test_consumer_subscriptions import (
    _created_subscription_id,
    _member_stream,
    _subscription_created_at,
)


@pytest.mark.backend_sensitive
async def test_seek_earliest_returns_resolved_cursor(api, prefilled_topic):
    """scenario: consumer/positions/1.01-positive-seek-earliest.md

    "earliest" resolves to RF-1.  RF=1 throughout this test session (no
    retention eviction on TOPIC_PREFILLED), so the cursor is always 0 even
    though the topic carries PREFILLED_COUNT events.  Using a pre-seeded
    topic proves that "earliest" is 0 while "latest" (tested separately) is
    non-zero - the two sentinels resolve to distinct values.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "earliest"}]}
            },
        )
    assert resp.status_code == 200
    body = resp.json()
    assert isinstance(body, dict)
    assert list(body["positions"]) == [TOPIC_PREFILLED]
    assert body["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": 0}]


@pytest.mark.backend_sensitive
async def test_seek_latest_returns_hwm_cursor(api, prefilled_topic):
    """scenario: consumer/positions/1.02-positive-seek-latest.md

    "latest" resolves to the head, the last stored event's sequence, which
    the `prefilled_topic` fixture recorded when it read the seeded events back.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "latest"}]}
            },
        )
    assert resp.status_code == 200
    body = resp.json()
    assert isinstance(body, dict)
    assert list(body["positions"]) == [TOPIC_PREFILLED]
    assert body["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": prefilled_topic.head}]


@pytest.mark.backend_sensitive
async def test_seek_exact_offset_accepted(api, prefilled_topic):
    """scenario: consumer/positions/1.03-positive-seek-exact-offset.md

    First seek to "earliest" to obtain a valid cursor integer, then re-seek
    to that same integer explicitly.  TOPIC_PREFILLED has RF=1 so "earliest"
    resolves to 0; the explicit re-seek also returns 0.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        earliest = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "earliest"}]}
            },
        )
        assert earliest.status_code == 200
        assert earliest.json()["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": 0}]

        # Re-seek to the same integer explicitly.
        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": 0}]}
            },
        )
    assert resp.status_code == 200
    assert resp.json()["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": 0}]


async def test_mixed_sentinels_and_integers_accepted(api, prefilled_topic):
    """scenario: consumer/positions/1.04-positive-mixed-sentinels.md

    A SEEK may send an integer alongside a sentinel in the same request.  With
    TOPIC_PREFILLED (1 partition) a true mix of two partitions is not possible,
    so the test demonstrates that an integer equivalent to a sentinel resolves
    to the same concrete value: seeking 0 (= "earliest") and seeking the head
    (= "latest") both return the expected integers.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        # Integer equivalent of "earliest".
        resp_low = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": 0}]}
            },
        )
        assert resp_low.status_code == 200
        assert resp_low.json()["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": 0}]

        # Integer equivalent of "latest".
        resp_high = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": prefilled_topic.head}]}
            },
        )
    assert resp_high.status_code == 200
    assert resp_high.json()["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": prefilled_topic.head}]


@pytest.mark.backend_sensitive
async def test_out_of_range_below_rf_rejected_400(api, prefilled_topic):
    """scenario: consumer/positions/1.05-negative-out-of-range-offset.md

    An integer SEEK value below RF-1 is rejected.  TOPIC_PREFILLED fixes the
    valid range - RF-1 = 0 and the head the fixture recorded - so the range the
    error quotes is known and can be asserted literally.  Seeking to -1
    is below the floor.  The error names the offending entry by its index in
    ``partition_positions`` and carries no part of the submitted value.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": -1}]}
            },
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "positions",
                    "description": f"topic {TOPIC_PREFILLED} partition 0: the seek position is below the valid range [0, {prefilled_topic.head}]",
                    "reason": "below_retention_floor",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


@pytest.mark.backend_sensitive
async def test_offset_above_hwm_rejected_400(api, prefilled_topic):
    """scenario: consumer/positions/1.06-negative-offset-above-hwm.md

    A SEEK value above the head is rejected - it would claim to have processed
    events that do not exist yet.  The fixture recorded TOPIC_PREFILLED's head,
    so seeking one past it is the tightest possible breach and the quoted range
    is known.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": prefilled_topic.head + 1}]}
            },
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "positions",
                    "description": f"topic {TOPIC_PREFILLED} partition 0: the seek position is above the valid range [0, {prefilled_topic.head}]",
                    "reason": "above_high_water_mark",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


@pytest.mark.timeout(15, func_only=True)
async def test_seek_while_streaming_returns_409(api, test_env):
    """scenario: consumer/positions/1.07-negative-seek-while-streaming.md"""
    topic = TOPIC_STREAM
    event_type = EVENT_TYPE_STREAM
    partitions = [0]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        marker = {
            "id": "17e20000-0000-4000-8000-000000005700",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "seek-while-streaming",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:30Z",
        }
        stored = await client.post(
            "/events", json=marker, headers={"Prefer": "wait=10"}, timeout=12
        )
        assert (stored.status_code, stored.content, stored.headers.get("preference-applied")) == (
            201,
            b"",
            "wait=10",
        )
        heads = {}
        for partition in partitions:
            manifest = await client.get(
                "/topics/segments", params={"topic": topic, "partition": partition}
            )
            assert (manifest.status_code, manifest.headers["content-type"]) == (
                200,
                "application/json",
            )
            snapshot = manifest.json()
            assert snapshot["topic"] == topic and snapshot["partition"] == partition
            assert set(snapshot) == {
                "topic",
                "partition",
                "start_sequence",
                "end_sequence",
                "start_time",
                "end_time",
                "segments",
            }
            assert isinstance(snapshot["segments"], list)
            head = snapshot["end_sequence"]
            assert type(head) is int and head >= 0
            heads[partition] = head
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_uuid = group_id.removeprefix("gts.cf.core.events.consumer_group.v1~")
        assert group_id == f"gts.cf.core.events.consumer_group.v1~{uuid.UUID(group_uuid)}"
        read = await client.get(f"/consumer-groups/{group_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == created.json() == group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        sub_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{sub_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == member
        assert joined.json() == {
            **member,
            "interests": [{**member["interests"][0], "unmatched_types": []}],
        }
        earliest = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1,
                "positions": {topic: [{"partition": 0, "value": "earliest"}]},
            },
        )
        assert (earliest.status_code, earliest.headers["content-type"]) == (200, "application/json")
        resolved = earliest.json()
        assert set(resolved) == {"positions"} and set(resolved["positions"]) == {topic}
        floor, = resolved["positions"][topic]
        assert set(floor) == {"partition", "value"}
        assert resolved == {"positions": {topic: [{"partition": 0, "value": floor["value"]}]}}
        assert type(floor["value"]) is int and 0 <= floor["value"] < heads[0]
        requested = [{"partition": p, "value": "latest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": heads[p]} for p in partitions]},
        }
        original_stream = await stack.enter_async_context(AsyncExitStack())
        reader = await original_stream.enter_async_context(_member_stream(api, sub_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": heads[p], "last_examined": heads[p]}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        refused = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1,
                "positions": {
                    topic: [
                        {"partition": 0, "value": "earliest"},
                    ]
                },
            },
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            409,
            "application/problem+json",
        )
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 409,
            "detail": "Operation precondition not met",
            "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "violations": [
                    {
                        "type": "streaming_in_progress",
                        "subject": sub_id,
                        "description": (
                            "one stream per subscription; a second stream or a SEEK "
                            "is refused while it is open"
                        ),
                    }
                ],
            },
        }
        kind, heartbeat = await reader.next_frame(timeout=10)
        assert kind == "heartbeat"
        assert heartbeat == {
            "kind": "heartbeat",
            "at": _subscription_created_at({"created_at": heartbeat["at"]}),
        }
        await original_stream.aclose()
        # A stale SEEK passes the lease check only after release, then fails before any write.
        loop = asyncio.get_running_loop()
        deadline = loop.time() + 1
        async with asyncio.timeout_at(deadline):
            while True:
                probe = await client.post(
                    f"/subscriptions/{sub_id}:seek",
                    json={
                        "topology_version": member["topology_version"] + 1,
                        "positions": {topic: [{"partition": 0, "value": "earliest"}]},
                    },
                    timeout=deadline - loop.time(),
                )
                assert probe.headers["content-type"] == "application/problem+json"
                assert probe.status_code in (409, 412)
                held = probe.status_code == 409
                reason, description = (
                    (
                        "streaming_in_progress",
                        "one stream per subscription; a second stream or a SEEK "
                        "is refused while it is open",
                    )
                    if held
                    else (
                        "topology_version_mismatch",
                        "subscription topology changed; re-read the subscription and re-seek",
                    )
                )
                assert probe.json() == {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
                    "title": "Failed Precondition",
                    "status": 409 if held else 412,
                    "detail": "Operation precondition not met",
                    "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
                    "trace_id": TRACE_ID,
                    "context": {
                        "resource_type": "gts.cf.core.events.subscription.v1~",
                        "violations": [
                            {"type": reason, "subject": sub_id, "description": description}
                        ],
                    },
                }
                if not held:
                    break
                await asyncio.sleep(0.05)
        reopened = await stack.enter_async_context(_member_stream(api, sub_id, "sse"))
        assert await reopened.next_frame(timeout=5) == ("topology", topology)


async def test_seek_unknown_subscription_returns_404(api):
    """scenario: consumer/positions/1.08-negative-seek-unknown-subscription.md

    SEEK on a subscription that does not exist returns 404 Not Found.
    """
    fake_sub_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            f"/subscriptions/{fake_sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]}
            },
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


async def test_seek_unassigned_partition_rejected_409(api):
    """scenario: consumer/positions/1.09-negative-seek-unassigned-partition.md"""

    topic = TOPIC_4P
    partitions = [0, 1, 2, 3]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_4P],
    }
    read_interest = {**interest, "barrier_mode": "respect", "filter": None}
    async with api() as client:
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{group_prefix}{uuid.UUID(group_id.removeprefix(group_prefix))}"
        created_body = created.json()
        assert isinstance(created_body, dict)
        assert "created_at" in created_body
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(created_body),
        }
        assert created_body == group
        group_read = await client.get(f"/consumer-groups/{group_id}")
        assert (group_read.status_code, group_read.headers["content-type"]) == (
            200, "application/json",
        )
        assert group_read.json() == group

        members = {}
        assignments = {}
        for version, agent in enumerate(("consumer-a", "consumer-b"), start=1):
            joined = await client.post(
                "/subscriptions",
                json={"consumer_group": group_id, "client_agent": agent, "interests": [interest]},
            )
            assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
            member_id = _created_subscription_id(joined)
            assert member_id not in members
            ranks = sorted([*members, member_id], key=uuid.UUID)
            ranges = (partitions,) if version == 1 else (partitions[:2], partitions[2:])
            assignments = dict(zip(ranks, ranges, strict=True))
            joined_body = joined.json()
            assert isinstance(joined_body, dict)
            assert "created_at" in joined_body
            member = {
                "id": member_id,
                "consumer_group": group_id,
                "client_agent": agent,
                "interests": [read_interest],
                "assigned": [{"topic": topic, "partition": p} for p in assignments[member_id]],
                "topology_version": version,
                "created_at": _subscription_created_at(joined_body),
            }
            assert joined_body == {
                **member,
                "interests": [{**read_interest, "unmatched_types": []}],
            }
            assert type(joined_body["topology_version"]) is int
            assert all(type(row["partition"]) is int for row in joined_body["assigned"])
            members[member_id] = member
            for current_id, current_parts in assignments.items():
                read = await client.get(f"/subscriptions/{current_id}")
                assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
                current = read.json()
                assert current == {
                    **members[current_id],
                    "assigned": [{"topic": topic, "partition": p} for p in current_parts],
                    "topology_version": version,
                }
                assert type(current["topology_version"]) is int
                assert all(type(row["partition"]) is int for row in current["assigned"])

        failed_precondition = {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "detail": "Operation precondition not met",
            "trace_id": TRACE_ID,
        }
        resource_type = "gts.cf.core.events.subscription.v1~"

        target_id, peer_id = assignments
        target_path = f"/subscriptions/{target_id}:seek"
        refused = await client.post(
            target_path,
            json={
                "topology_version": 2,
                "positions": {topic: [
                    {"partition": 0, "value": "earliest"},
                    {"partition": 2, "value": "earliest"},
                ]},
            },
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            409, "application/problem+json",
        )
        assert refused.json() == {
            **failed_precondition,
            "status": 409,
            "instance": f"{API_BASE}{target_path}",
            "context": {
                "resource_type": resource_type,
                "violations": [{
                    "type": "partition_not_assigned",
                    "subject": f"{topic}:2",
                    "description": (
                        f"partition is not assigned to subscription {target_id} "
                        "(topology_version=2)"
                    ),
                }],
            },
        }

        for member_id, missing in ((target_id, [0, 1]), (peer_id, [2, 3])):
            async with client.stream(
                "GET", "/events:stream", params={"subscription": member_id},
                headers={"Accept": "multipart/mixed"},
            ) as stream:
                assert (stream.status_code, stream.headers["content-type"]) == (
                    409, "application/problem+json",
                )
                await stream.aread()
            assert stream.json() == {
                **failed_precondition,
                "status": 409,
                "instance": f"{API_BASE}/events:stream",
                "context": {
                    "resource_type": resource_type,
                    "violations": [
                        {
                            "type": "positions_not_set",
                            "subject": f"{topic}:{partition}",
                            "description": (
                                f"call POST /v1/subscriptions/{member_id}:seek "
                                "before re-opening the stream"
                            ),
                        }
                        for partition in missing
                    ],
                },
            }

        seek = await client.post(
            target_path,
            json={
                "topology_version": 2,
                "positions": {topic: [{"partition": 0, "value": "earliest"}]},
            },
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        resolved = seek.json()
        assert resolved == {"positions": {topic: [{"partition": 0, "value": 0}]}}
        position, = resolved["positions"][topic]
        assert type(position["partition"]) is int
        assert type(position["value"]) is int


async def test_seek_stale_topology_version_rejected_412(api):
    """scenario: consumer/positions/1.15-negative-seek-stale-topology-version.md"""
    topic = TOPIC_4P
    event_type = EVENT_TYPE_4P
    partitions = list(range(4))
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_uuid = group_id.removeprefix("gts.cf.core.events.consumer_group.v1~")
        assert group_id == f"gts.cf.core.events.consumer_group.v1~{uuid.UUID(group_uuid)}"
        read = await client.get(f"/consumer-groups/{group_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == created.json() == group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        sub_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{sub_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == member
        assert joined.json() == {
            **member,
            "interests": [{**member["interests"][0], "unmatched_types": []}],
        }
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-b",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        peer_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{peer_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        assert peer_id != sub_id
        kept, gained = partitions[:2], partitions[2:]
        if uuid.UUID(sub_id) > uuid.UUID(peer_id):
            kept, gained = gained, kept
        peer = {
            "id": peer_id,
            "consumer_group": group_id,
            "client_agent": "consumer-b",
            "assigned": [{"topic": topic, "partition": p} for p in gained],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 2,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == peer
        assert joined.json() == {
            **peer,
            "interests": [{**peer["interests"][0], "unmatched_types": []}],
        }
        current = await client.get(f"/subscriptions/{sub_id}")
        assert (current.status_code, current.headers["content-type"]) == (200, "application/json")
        assert current.json() == {
            **member,
            "topology_version": 2,
            "assigned": [{"topic": topic, "partition": p} for p in kept],
        }
        first_parts = {a["partition"] for a in current.json()["assigned"]}
        second_parts = {a["partition"] for a in peer["assigned"]}
        assert first_parts.isdisjoint(second_parts)
        assert first_parts | second_parts == set(partitions)
        retained = current.json()["assigned"][0]["partition"]
        positions = {topic: [{"partition": retained, "value": "earliest"}]}
        refused = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": member["topology_version"], "positions": positions},
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            412,
            "application/problem+json",
        )
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": "Operation precondition not met",
            "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "violations": [
                    {
                        "type": "topology_version_mismatch",
                        "subject": sub_id,
                        "description": (
                            "subscription topology changed; re-read the subscription and re-seek"
                        ),
                    }
                ],
            },
        }
        async with client.stream(
            "GET",
            "/events:stream",
            params={"subscription": sub_id},
            headers={"Accept": "multipart/mixed"},
        ) as refused:
            assert refused.status_code == 409
            await refused.aread()
        assert (refused.status_code, refused.headers["content-type"]) == (
            409,
            "application/problem+json",
        )
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 409,
            "detail": "Operation precondition not met",
            "instance": f"{API_BASE}/events:stream",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "violations": [
                    {
                        "type": "positions_not_set",
                        "subject": f"{topic}:{p}",
                        "description": (
                            f"call POST /v1/subscriptions/{sub_id}:seek "
                            "before re-opening the stream"
                        ),
                    }
                    for p in kept
                ],
            },
        }
        reread = await client.get(f"/subscriptions/{sub_id}")
        assert (reread.status_code, reread.headers["content-type"]) == (200, "application/json")
        assert reread.json() == current.json()
        requested = [{"partition": p, "value": "earliest"} for p in [retained]]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": reread.json()["topology_version"],
                "positions": {topic: requested},
            },
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in [retained]]},
        }


@pytest.mark.backend_sensitive
async def test_seek_any_value_in_retention_range_accepted(api, prefilled_topic):
    """scenario: consumer/positions/1.10-positive-seek-any-value-in-range.md

    Any integer in [RF-1, head] is accepted.  TOPIC_PREFILLED has RF-1=0 and
    the head the fixture recorded, so both boundaries are known; the sequence
    of the middle seeded event confirms an interior value is also accepted.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        middle = prefilled_topic.events[len(prefilled_topic.events) // 2]["sequence"]
        for value in (0, middle, prefilled_topic.head):
            resp = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": value}]}
                },
            )
            assert resp.status_code == 200
            assert resp.json()["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": value}]


@pytest.mark.backend_sensitive
async def test_seek_at_timestamp_returns_resolved_integer(api, prefilled_topic):
    """scenario: consumer/positions/1.11-positive-seek-at-timestamp.md

    "at:<ISO-8601>" resolves to the first event at or after that timestamp.
    A far-future timestamp clamps to the head, which the fixture recorded, so
    the assertion is concrete.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "at:2099-01-01T00:00:00Z"}]}
            },
        )
    assert resp.status_code == 200
    body = resp.json()
    assert isinstance(body, dict)
    assert list(body["positions"]) == [TOPIC_PREFILLED]
    assert body["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": prefilled_topic.head}]


@pytest.mark.backend_sensitive
async def test_seek_timestamp_before_retention_clamps_to_rf(api, prefilled_topic):
    """scenario: consumer/positions/1.12-positive-seek-at-timestamp-before-retention.md

    A timestamp before the retention floor clamps to RF-1.  TOPIC_PREFILLED
    has its seeded events but RF=1 (no eviction in this session), so
    RF-1=0 and a pre-epoch timestamp resolves to exactly 0.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "at:1970-01-01T00:00:00Z"}]}
            },
        )
    assert resp.status_code == 200
    body = resp.json()
    assert isinstance(body, dict)
    assert list(body["positions"]) == [TOPIC_PREFILLED]
    assert body["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": 0}]


@pytest.mark.backend_sensitive
async def test_seek_timestamp_beyond_hwm_clamps_to_hwm(api, prefilled_topic):
    """scenario: consumer/positions/1.13-positive-seek-at-timestamp-beyond-hwm.md

    A timestamp beyond the head clamps to the head (same as "latest"). The
    fixture recorded TOPIC_PREFILLED's head, so the assertion is concrete; no
    need to first resolve "latest" and compare indirectly.
    """
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
                        "topic": TOPIC_PREFILLED,
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = str(sub_resp.json()["id"])

        resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "at:2099-12-31T23:59:59Z"}]}
            },
        )
    assert resp.status_code == 200
    body = resp.json()
    assert isinstance(body, dict)
    assert list(body["positions"]) == [TOPIC_PREFILLED]
    assert body["positions"][TOPIC_PREFILLED] == [{"partition": 0, "value": prefilled_topic.head}]


# The index of the seeded event the consumer stops after.
RESUME_AFTER = 39


@pytest.mark.backend_sensitive
@pytest.mark.timeout(30, func_only=True)
async def test_seek_resume_from_cursor(api, test_env, prefilled_topic):
    """scenario: consumer/positions/1.14-positive-seek-resume-from-cursor.md"""
    topic = TOPIC_PREFILLED
    event_type = EVENT_TYPE_PREFILLED
    partitions = [0]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_uuid = group_id.removeprefix("gts.cf.core.events.consumer_group.v1~")
        assert group_id == f"gts.cf.core.events.consumer_group.v1~{uuid.UUID(group_uuid)}"
        read = await client.get(f"/consumer-groups/{group_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == created.json() == group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        sub_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{sub_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == member
        assert joined.json() == {
            **member,
            "interests": [{**member["interests"][0], "unmatched_types": []}],
        }
        requested = [{"partition": p, "value": "earliest"} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {topic: requested}},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in partitions]},
        }
        original_stream = await stack.enter_async_context(AsyncExitStack())
        reader = await original_stream.enter_async_context(_member_stream(api, sub_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": 0, "last_examined": 0}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        deadline = asyncio.get_running_loop().time() + 10
        for expected in prefilled_topic.events[:40]:
            remaining = deadline - asyncio.get_running_loop().time()
            assert remaining > 0
            frame = await reader.await_kind("event", timeout=remaining)
            assert frame == {"kind": "event", "payload": expected}
        persisted = frame["payload"]["sequence"]
        assert persisted == prefilled_topic.events[39]["sequence"]
        await original_stream.aclose()
        left = await client.delete(f"/subscriptions/{sub_id}")
        assert (left.status_code, left.content) == (204, b"")
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        replacement_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{replacement_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        replacement = {
            "id": replacement_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == replacement
        assert joined.json() == {
            **replacement,
            "interests": [{**replacement["interests"][0], "unmatched_types": []}],
        }
        assert replacement_id != sub_id
        requested = [{"partition": p, "value": persisted} for p in partitions]
        seek = await client.post(
            f"/subscriptions/{replacement_id}:seek",
            json={
                "topology_version": replacement["topology_version"],
                "positions": {topic: requested},
            },
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": persisted} for p in partitions]},
        }
        reader = await stack.enter_async_context(_member_stream(api, replacement_id, "sse"))
        baseline = await reader.next_frame(timeout=5)
        topology = {
            "kind": "topology",
            "topology_version": 1,
            "assigned": [
                {"topic": topic, "partition": p, "offset": persisted, "last_examined": persisted}
                for p in partitions
            ],
        }
        assert baseline == ("topology", topology)
        resumed = await reader.await_kind("event", timeout=5)
        assert resumed == {"kind": "event", "payload": prefilled_topic.events[40]}


async def test_seek_after_join_rebalance_recovers(api):
    """scenario: consumer/positions/1.16-positive-seek-after-join-rebalance.md"""
    topic = TOPIC_4P
    event_type = EVENT_TYPE_4P
    partitions = list(range(4))
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [event_type],
    }
    async with api() as client, AsyncExitStack() as stack:
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_uuid = group_id.removeprefix("gts.cf.core.events.consumer_group.v1~")
        assert group_id == f"gts.cf.core.events.consumer_group.v1~{uuid.UUID(group_uuid)}"
        read = await client.get(f"/consumer-groups/{group_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == created.json() == group
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-a",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        sub_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{sub_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "consumer-a",
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 1,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == member
        assert joined.json() == {
            **member,
            "interests": [{**member["interests"][0], "unmatched_types": []}],
        }
        joined = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "consumer-b",
                "interests": [interest],
            },
        )
        assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
        peer_id = _created_subscription_id(joined)
        read = await client.get(f"/subscriptions/{peer_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        assert peer_id != sub_id
        kept, gained = partitions[:2], partitions[2:]
        if uuid.UUID(sub_id) > uuid.UUID(peer_id):
            kept, gained = gained, kept
        peer = {
            "id": peer_id,
            "consumer_group": group_id,
            "client_agent": "consumer-b",
            "assigned": [{"topic": topic, "partition": p} for p in gained],
            "interests": [{**interest, "barrier_mode": "respect", "filter": None}],
            "topology_version": 2,
            "created_at": _subscription_created_at(read.json()),
        }
        assert read.json() == peer
        assert joined.json() == {
            **peer,
            "interests": [{**peer["interests"][0], "unmatched_types": []}],
        }
        current = await client.get(f"/subscriptions/{sub_id}")
        assert (current.status_code, current.headers["content-type"]) == (200, "application/json")
        assert current.json() == {
            **member,
            "topology_version": 2,
            "assigned": [{"topic": topic, "partition": p} for p in kept],
        }
        first_parts = {a["partition"] for a in current.json()["assigned"]}
        second_parts = {a["partition"] for a in peer["assigned"]}
        assert first_parts.isdisjoint(second_parts)
        assert first_parts | second_parts == set(partitions)
        retained = current.json()["assigned"][0]["partition"]
        positions = {topic: [{"partition": retained, "value": "earliest"}]}
        refused = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": member["topology_version"], "positions": positions},
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            412,
            "application/problem+json",
        )
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": "Operation precondition not met",
            "instance": f"{API_BASE}/subscriptions/{sub_id}:seek",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "violations": [
                    {
                        "type": "topology_version_mismatch",
                        "subject": sub_id,
                        "description": (
                            "subscription topology changed; re-read the subscription and re-seek"
                        ),
                    }
                ],
            },
        }
        async with client.stream(
            "GET",
            "/events:stream",
            params={"subscription": sub_id},
            headers={"Accept": "multipart/mixed"},
        ) as refused:
            assert refused.status_code == 409
            await refused.aread()
        assert (refused.status_code, refused.headers["content-type"]) == (
            409,
            "application/problem+json",
        )
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 409,
            "detail": "Operation precondition not met",
            "instance": f"{API_BASE}/events:stream",
            "trace_id": TRACE_ID,
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "violations": [
                    {
                        "type": "positions_not_set",
                        "subject": f"{topic}:{p}",
                        "description": (
                            f"call POST /v1/subscriptions/{sub_id}:seek "
                            "before re-opening the stream"
                        ),
                    }
                    for p in kept
                ],
            },
        }
        reread = await client.get(f"/subscriptions/{sub_id}")
        assert (reread.status_code, reread.headers["content-type"]) == (200, "application/json")
        assert reread.json() == current.json()
        requested = [{"partition": p, "value": "earliest"} for p in [retained]]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "topology_version": reread.json()["topology_version"],
                "positions": {topic: requested},
            },
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        assert seek.json() == {
            "positions": {topic: [{"partition": p, "value": 0} for p in [retained]]},
        }


@pytest.mark.timeout(30, func_only=True)
async def test_seek_after_leave_rebalance_recovers(api, test_env):
    """scenario: consumer/positions/1.17-positive-seek-after-leave-rebalance.md"""

    topic = TOPIC_4P
    partitions = [0, 1, 2, 3]
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_4P],
    }
    read_interest = {**interest, "barrier_mode": "respect", "filter": None}
    async with api() as client:
        created = await client.post("/consumer-groups")
        assert (created.status_code, created.headers["content-type"]) == (201, "application/json")
        group_id = created.headers["location"].removeprefix(f"{API_BASE}/consumer-groups/")
        assert created.headers["location"] == f"{API_BASE}/consumer-groups/{group_id}"
        group_prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{group_prefix}{uuid.UUID(group_id.removeprefix(group_prefix))}"
        created_body = created.json()
        assert isinstance(created_body, dict)
        assert "created_at" in created_body
        group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(created_body),
        }
        assert created_body == group
        group_read = await client.get(f"/consumer-groups/{group_id}")
        assert (group_read.status_code, group_read.headers["content-type"]) == (
            200, "application/json",
        )
        assert group_read.json() == group

        members = {}
        assignments = {}
        for version, agent in enumerate(("consumer-a", "consumer-b"), start=1):
            joined = await client.post(
                "/subscriptions",
                json={"consumer_group": group_id, "client_agent": agent, "interests": [interest]},
            )
            assert (joined.status_code, joined.headers["content-type"]) == (201, "application/json")
            member_id = _created_subscription_id(joined)
            assert member_id not in members
            ranks = sorted([*members, member_id], key=uuid.UUID)
            ranges = (partitions,) if version == 1 else (partitions[:2], partitions[2:])
            assignments = dict(zip(ranks, ranges, strict=True))
            joined_body = joined.json()
            assert isinstance(joined_body, dict)
            assert "created_at" in joined_body
            member = {
                "id": member_id,
                "consumer_group": group_id,
                "client_agent": agent,
                "interests": [read_interest],
                "assigned": [{"topic": topic, "partition": p} for p in assignments[member_id]],
                "topology_version": version,
                "created_at": _subscription_created_at(joined_body),
            }
            assert joined_body == {
                **member,
                "interests": [{**read_interest, "unmatched_types": []}],
            }
            assert type(joined_body["topology_version"]) is int
            assert all(type(row["partition"]) is int for row in joined_body["assigned"])
            members[member_id] = member
            for current_id, current_parts in assignments.items():
                read = await client.get(f"/subscriptions/{current_id}")
                assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
                current = read.json()
                assert current == {
                    **members[current_id],
                    "assigned": [{"topic": topic, "partition": p} for p in current_parts],
                    "topology_version": version,
                }
                assert type(current["topology_version"]) is int
                assert all(type(row["partition"]) is int for row in current["assigned"])

        failed_precondition = {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "detail": "Operation precondition not met",
            "trace_id": TRACE_ID,
        }
        resource_type = "gts.cf.core.events.subscription.v1~"

        survivor_id, peer_id = members
        retained = assignments[survivor_id][0]
        target_path = f"/subscriptions/{survivor_id}:seek"
        positions = {topic: [{"partition": retained, "value": "earliest"}]}

        left = await client.delete(f"/subscriptions/{peer_id}")
        assert (left.status_code, left.content) == (204, b"")
        stale = await client.post(
            target_path, json={"topology_version": 2, "positions": positions},
        )
        assert (stale.status_code, stale.headers["content-type"]) == (
            412, "application/problem+json",
        )
        assert stale.json() == {
            **failed_precondition,
            "status": 412,
            "instance": f"{API_BASE}{target_path}",
            "context": {
                "resource_type": resource_type,
                "violations": [{
                    "type": "topology_version_mismatch",
                    "subject": survivor_id,
                    "description": (
                        "subscription topology changed; re-read the subscription and re-seek"
                    ),
                }],
            },
        }

        async with client.stream(
            "GET", "/events:stream", params={"subscription": survivor_id},
            headers={"Accept": "multipart/mixed"},
        ) as refused:
            assert (refused.status_code, refused.headers["content-type"]) == (
                409, "application/problem+json",
            )
            await refused.aread()
        missing = {
            **failed_precondition,
            "status": 409,
            "instance": f"{API_BASE}/events:stream",
            "context": {
                "resource_type": resource_type,
                "violations": [
                    {
                        "type": "positions_not_set",
                        "subject": f"{topic}:{partition}",
                        "description": (
                            f"call POST /v1/subscriptions/{survivor_id}:seek "
                            "before re-opening the stream"
                        ),
                    }
                    for partition in partitions
                ],
            },
        }
        assert refused.json() == missing

        read = await client.get(f"/subscriptions/{survivor_id}")
        assert (read.status_code, read.headers["content-type"]) == (200, "application/json")
        current = read.json()
        assert current == {
            **members[survivor_id],
            "assigned": [{"topic": topic, "partition": p} for p in partitions],
            "topology_version": 3,
        }
        assert type(current["topology_version"]) is int
        assert all(type(row["partition"]) is int for row in current["assigned"])

        seek = await client.post(
            target_path,
            json={"topology_version": current["topology_version"], "positions": positions},
        )
        assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
        resolved = seek.json()
        assert resolved == {"positions": {topic: [{"partition": retained, "value": 0}]}}
        position, = resolved["positions"][topic]
        assert type(position["partition"]) is int
        assert type(position["value"]) is int

        async with client.stream(
            "GET", "/events:stream", params={"subscription": survivor_id},
            headers={"Accept": "multipart/mixed"},
        ) as refused:
            assert (refused.status_code, refused.headers["content-type"]) == (
                409, "application/problem+json",
            )
            await refused.aread()
        assert refused.json() == {
            **missing,
            "context": {
                "resource_type": resource_type,
                "violations": [
                    {
                        "type": "positions_not_set",
                        "subject": f"{topic}:{partition}",
                        "description": (
                            f"call POST /v1/subscriptions/{survivor_id}:seek "
                            "before re-opening the stream"
                        ),
                    }
                    for partition in partitions if partition != retained
                ],
            },
        }


@pytest.mark.timeout(20, func_only=True)
async def test_seek_subscription_not_owned(api):
    """scenario: consumer/positions/1.18-negative-seek-subscription-not-owned.md"""
    async with api(TOKEN_PRINCIPAL_A) as owner, api(TOKEN_PRINCIPAL_B) as peer:
        created = await owner.post("/consumer-groups")
        assert created.status_code == 201
        assert created.headers["content-type"] == "application/json"
        location = created.headers["location"]
        group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
        prefix = "gts.cf.core.events.consumer_group.v1~"
        assert group_id == f"{prefix}{uuid.UUID(group_id.removeprefix(prefix))}"
        group_path = f"/consumer-groups/{group_id}"
        assert location == f"{API_BASE}{group_path}"

        group_body = created.json()
        assert isinstance(group_body, dict)
        assert isinstance(group_body.get("created_at"), str)
        expected_group = {
            "id": group_id,
            "kind": "anonymous",
            "tenant": DEFAULT_TENANT_ID,
            "owner_principal": DEFAULT_SUBJECT_ID,
            "description": None,
            "created_at": _subscription_created_at(group_body),
        }
        assert group_body == expected_group
        owner_group = await owner.get(group_path)
        assert owner_group.status_code == 200
        assert owner_group.headers["content-type"] == "application/json"
        assert owner_group.json() == expected_group

        interest = {
            "topic": TOPIC_STREAM,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_STREAM],
        }
        joined = await owner.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            },
        )
        assert joined.status_code == 201
        assert joined.headers["content-type"] == "application/json"
        sub_id = _created_subscription_id(joined)
        sub_path = f"/subscriptions/{sub_id}"
        joined_body = joined.json()
        assert isinstance(joined_body, dict)
        assert isinstance(joined_body.get("created_at"), str)
        member_interest = {**interest, "barrier_mode": "respect", "filter": None}
        expected_member = {
            "id": sub_id,
            "consumer_group": group_id,
            "client_agent": "e2e-test",
            "interests": [member_interest],
            "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(joined_body),
        }
        assert joined_body == {
            **expected_member,
            "interests": [{**member_interest, "unmatched_types": []}],
        }
        assert type(joined_body["topology_version"]) is int
        assert type(joined_body["assigned"][0]["partition"]) is int

        owner_member = await owner.get(sub_path)
        assert owner_member.status_code == 200
        assert owner_member.headers["content-type"] == "application/json"
        owner_member_body = owner_member.json()
        assert owner_member_body == expected_member
        assert type(owner_member_body["topology_version"]) is int
        assert type(owner_member_body["assigned"][0]["partition"]) is int

        peer_member = await peer.get(sub_path)
        assert peer_member.status_code == 200
        assert peer_member.headers["content-type"] == "application/json"
        peer_member_body = peer_member.json()
        assert peer_member_body == expected_member
        assert type(peer_member_body["topology_version"]) is int
        assert type(peer_member_body["assigned"][0]["partition"]) is int

        seek_path = f"{sub_path}:seek"
        seek_body = {
            "topology_version": 1,
            "positions": {TOPIC_STREAM: [{"partition": 0, "value": "earliest"}]},
        }

        refused = await peer.post(seek_path, json=seek_body)
        assert refused.status_code == 403
        assert refused.headers["content-type"] == "application/problem+json"
        assert refused.json() == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}{seek_path}",
            "trace_id": TRACE_ID,
            "context": {
                "reason": f"subscription '{sub_id}' was joined by another principal",
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        }

        async with owner.stream(
            "GET", "/events:stream",
            params={"subscription": sub_id},
            headers={"Accept": "multipart/mixed"},
        ) as unseeded:
            assert unseeded.status_code == 409
            assert unseeded.headers["content-type"] == "application/problem+json"
            await unseeded.aread()
            assert unseeded.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
                "title": "Failed Precondition",
                "status": 409,
                "detail": "Operation precondition not met",
                "instance": f"{API_BASE}/events:stream",
                "trace_id": TRACE_ID,
                "context": {
                    "resource_type": "gts.cf.core.events.subscription.v1~",
                    "violations": [{
                        "type": "positions_not_set",
                        "subject": f"{TOPIC_STREAM}:0",
                        "description": (
                            f"call POST /v1/subscriptions/{sub_id}:seek "
                            "before re-opening the stream"
                        ),
                    }],
                },
            }

        segments = await owner.get(
            "/topics/segments",
            params={"topic": TOPIC_STREAM, "partition": 0},
        )
        assert segments.status_code == 200
        assert segments.headers["content-type"] == "application/json"
        manifest = segments.json()
        assert isinstance(manifest, dict)
        assert set(manifest) == {
            "topic", "partition", "start_sequence", "end_sequence",
            "start_time", "end_time", "segments",
        }
        assert manifest["topic"] == TOPIC_STREAM
        assert type(manifest["partition"]) is int
        assert manifest["partition"] == 0
        start, end = manifest["start_sequence"], manifest["end_sequence"]
        assert type(start) is int
        assert type(end) is int
        assert 0 <= start <= end
        assert isinstance(manifest["segments"], list)
        for field in ("start_time", "end_time"):
            if manifest[field] is not None:
                _subscription_created_at({"created_at": manifest[field]})
        earliest = max(start - 1, 0)

        repaired = await owner.post(seek_path, json=seek_body)
        assert repaired.status_code == 200
        assert repaired.headers["content-type"] == "application/json"
        repaired_body = repaired.json()
        assert repaired_body == {"positions": {TOPIC_STREAM: [{"partition": 0, "value": earliest}]}}
        cursor = repaired_body["positions"][TOPIC_STREAM][0]
        assert type(cursor["partition"]) is int
        assert type(cursor["value"]) is int
