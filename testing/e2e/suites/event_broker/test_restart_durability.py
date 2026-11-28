"""Restart the worker against its original stores and the same Flight Control host."""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from .conftest import (
    API_BASE,
    DURABLE,
    SUBJECT_TYPE,
    SseFrameReader,
    patch_backend,
    public_headers,
    wait_provisioned,
    _derived_event_type,
    _topic_instance,
)
from .topology import OWNER_TENANT, FlightControlBrokerEnv, replace_once
TOPIC = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.restart.v1"
EVENT_TYPE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.restart.v1~"


def _restart_env(root: Path, sidecars: dict) -> FlightControlBrokerEnv:
    namespace = f"evbk_e2e_restart_{uuid.uuid4().hex[:8]}"

    def config(text, home, handles):
        text = patch_backend(text, handles, namespace, home)
        return replace_once(
            text,
            "      topics:\n",
            f'      topics:\n        "{TOPIC}":\n          partitions: 1\n',
        )

    return FlightControlBrokerEnv(
        root,
        broker_config=config,
        documents=[_topic_instance(TOPIC), _derived_event_type(EVENT_TYPE, TOPIC)],
        sidecars=sidecars,
    )


@pytest.mark.skipif(
    not DURABLE,
    reason="needs a durable storage backend: the memory backend loses events with the "
    "process; runs with any other EB_E2E_BACKEND (make e2e-event-broker-<backend>)",
)
@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_restart_preserves_events_and_consumer_groups_but_not_subscriptions(
    test_env, tmp_path,
):
    with _restart_env(tmp_path, test_env.sidecars) as env:
        base_url = f"{env.base_url}{API_BASE}"
        wait_provisioned(base_url, {TOPIC: EVENT_TYPE})
        async with httpx.AsyncClient(
            base_url=base_url, timeout=5.0, headers=public_headers(),
        ) as client:
            group_resp = await client.post("/consumer-groups")
            assert group_resp.status_code == 201
            group_body_before = group_resp.json()
            group_id = group_body_before["id"]

            tenant_id = OWNER_TENANT
            sub1_resp = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": [
                        {"topic": TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE]}
                    ],
                },
            )
            assert sub1_resp.status_code == 201
            sub1_id = sub1_resp.json()["id"]

            seek1 = await client.post(
                f"/subscriptions/{sub1_id}:seek",
                json={
                    "topology_version": 1, "positions": {TOPIC: [{"partition": 0, "value": "earliest"}]}
                },
            )
            assert seek1.status_code == 200

        event_id = str(uuid.uuid4())
        async with httpx.AsyncClient(
            base_url=base_url, timeout=None, headers=public_headers(),
        ) as stream_client:
            async with stream_client.stream(
                "GET", f"/events:sse?subscription={sub1_id}"
            ) as stream1_resp:
                assert stream1_resp.status_code == 200
                reader1 = SseFrameReader(stream1_resp)
                kind, _ = await reader1.next_frame(timeout=5)
                assert kind == "topology"

                async with httpx.AsyncClient(
                    base_url=base_url, timeout=5.0, headers=public_headers(),
                ) as pub_client:
                    publish_resp = await pub_client.post(
                        "/events",
                        json={
                            "id": event_id,
                            "type": EVENT_TYPE,
                            "subject_tenant": tenant_id,
                            "source": "e2e-test",
                            "subject": "s1",
                            "subject_type": SUBJECT_TYPE,
                            "occurred_at": datetime.now(timezone.utc).isoformat(),
                        },
                    )
                assert publish_resp.status_code == 202

                # The Cursor row this test goes on to check across the
                # restart was written by the `:seek` above, not by consuming:
                # SEEK is the cursor's only writer. Reading the event here
                # proves the stream is live before the process is killed.
                kind, data = await reader1.next_frame(timeout=5)
                assert kind == "event"
                before = data["payload"]
                assert before["id"] == event_id
                assert before["sequence"] == 1

        env.restart_broker()
        base_url = f"{env.base_url}{API_BASE}"
        wait_provisioned(base_url, {TOPIC: EVENT_TYPE})
        async with httpx.AsyncClient(
            base_url=base_url, timeout=5.0, headers=public_headers(),
        ) as client:
            # ConsumerGroupRepo is SQLite-backed - unchanged, must still exist.
            group_resp_after = await client.get(f"/consumer-groups/{group_id}")
            assert group_resp_after.status_code == 200
            assert group_resp_after.json() == group_body_before

            # SubscriptionRepo is ClusterCacheV1-backed - ephemeral under the
            # standalone cache provider, so it must NOT survive.
            sub1_resp_after = await client.get(f"/subscriptions/{sub1_id}")
            assert sub1_resp_after.status_code == 404

            # A brand-new subscription under the same consumer group,
            # deliberately not seeked: `stream()` only rejects an unseeded
            # partition (409 PositionsNotSet) when `find_cursor` finds
            # nothing. Getting 200 here is the proof the Cursor row from
            # before the restart survived.
            sub2_resp = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": [
                        {"topic": TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE]}
                    ],
                },
            )
            assert sub2_resp.status_code == 201
            sub2_id = sub2_resp.json()["id"]

        async with httpx.AsyncClient(
            base_url=base_url, timeout=None, headers=public_headers(),
        ) as stream_client:
            async with stream_client.stream(
                "GET", f"/events:sse?subscription={sub2_id}"
            ) as stream2_resp:
                assert stream2_resp.status_code == 200
                reader2 = SseFrameReader(stream2_resp)
                kind, _ = await reader2.next_frame(timeout=5)
                assert kind == "topology"

                # The event row itself - not just the cursor row - is still
                # in the storage backend: a fresh subscription's in-memory
                # replay cursor starts from its own join-time
                # `assigned.offset` (always 0), so it re-reads from the start
                # rather than skipping ahead.
                kind, data = await reader2.next_frame(timeout=5)
                assert kind == "event"
                assert data["payload"] == before


@pytest.mark.skipif(
    not DURABLE,
    reason="needs a durable storage backend: the memory backend loses events with the "
    "process; runs with any other EB_E2E_BACKEND (make e2e-event-broker-<backend>)",
)
@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_a_publish_replayed_after_restart_is_stored_once(test_env, tmp_path):
    """feature 0007 AC-9 - a retry of an applied message stores nothing, after a
    restart too.

    A chained producer's first event is published and stored; the server is
    stopped and started again on the same home and medium; the producer replays
    that event (a client that never saw its answer) and then sends the next one.
    The replay is answered as a duplicate, and a consumer reading the partition
    from the start gets each event once: the first exactly as it was stored
    before the restart, then the second above it.
    """
    tenant_id = OWNER_TENANT

    def chained(event_id: str, producer_id: str, previous: int, sequence: int) -> dict:
        return {
            "id": event_id,
            "type": EVENT_TYPE,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": f"s{sequence}",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": datetime.now(timezone.utc).isoformat(),
            "meta": {"version": 1, "producer": producer_id, "previous": previous, "sequence": sequence},
        }

    async def read_from_start(client, count: int) -> list[dict]:
        group = await client.post("/consumer-groups")
        assert group.status_code == 201
        sub = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group.json()["id"],
                "client_agent": "e2e-test",
                "interests": [{"topic": TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE]}],
            },
        )
        assert sub.status_code == 201
        sub_id = sub.json()["id"]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {TOPIC: [{"partition": 0, "value": "earliest"}]}},
        )
        assert seek.status_code == 200
        async with httpx.AsyncClient(
            base_url=base_url, timeout=None, headers=public_headers(),
        ) as stream_client:
            async with stream_client.stream("GET", f"/events:sse?subscription={sub_id}") as resp:
                assert resp.status_code == 200
                reader = SseFrameReader(resp)
                await reader.await_kind("topology", timeout=5)
                payloads = [
                    (await reader.await_kind("event", timeout=10))["payload"] for _ in range(count)
                ]
                # Nothing else is delivered: a stored replay would follow here.
                with pytest.raises((AssertionError, asyncio.TimeoutError)):
                    await reader.await_kind("event", timeout=3)
        return payloads

    first = str(uuid.uuid4())
    with _restart_env(tmp_path, test_env.sidecars) as env:
        base_url = f"{env.base_url}{API_BASE}"
        wait_provisioned(base_url, {TOPIC: EVENT_TYPE})
        async with httpx.AsyncClient(
            base_url=base_url, timeout=5.0, headers=public_headers(),
        ) as client:
            reg = await client.post("/producers", json={"mode": "chained", "client_agent": "e2e-test/1.0"})
            assert reg.status_code == 201
            producer_id = reg.json()["id"]
            first_event = chained(first, producer_id, 0, 1)
            published = await client.post("/events", json=first_event)
            assert published.status_code == 202
            [stored] = await read_from_start(client, 1)
            assert stored["id"] == first

        env.restart_broker()
        base_url = f"{env.base_url}{API_BASE}"
        wait_provisioned(base_url, {TOPIC: EVENT_TYPE})
        async with httpx.AsyncClient(
            base_url=base_url, timeout=5.0, headers=public_headers(),
        ) as client:
            replay = await client.post("/events", json=first_event)
            assert replay.status_code == 200
            assert replay.text == ""

            second = str(uuid.uuid4())
            published = await client.post("/events", json=chained(second, producer_id, 1, 2))
            assert published.status_code == 202

            after = await read_from_start(client, 2)
            assert after[0] == stored
            assert after[1]["id"] == second
            assert after[1]["sequence"] > stored["sequence"]
