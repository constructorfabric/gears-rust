"""Storage backend scenarios (feature 0007 §11), on whichever backend the
suite runs on (`EB_E2E_BACKEND`).

S1 and S5 are here. S3 (tail wake) is
`test_publish_consume.py::test_publish_then_consume_happy_path`, which opens
the stream at the tail and then publishes; S4 (crash after an answered
publish) is `test_restart_durability.py`, on a durable backend. S6 (a crash
between the backend commit and the outbox ack) has no e2e hook to stop the
process at that point; the gear's `redelivery_after_append_is_a_duplicate` and
the conformance suite's dedup scenarios cover it. S2 (`Prefer: wait`) answers
501 and has no test.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timezone

import httpx

from .conftest import (
    EVENT_TYPE_FRESH,
    EVENT_TYPE_ORDER,
    SUBJECT_TYPE,
    TOPIC_FRESH,
    TOPIC_ORDER,
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
