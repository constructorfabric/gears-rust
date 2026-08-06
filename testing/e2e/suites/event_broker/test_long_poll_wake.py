"""Long-poll wake-up e2e test (design.md D6/D8, task 11.3).

Confirms an open `:sse` stream wakes promptly when an event is published,
rather than only noticing on its next heartbeat - `heartbeat_interval_secs`
defaults to 5s (`config.rs`), so a bound tight enough to fail on a
heartbeat-only wake (well under 5s) but loose enough not to flake is the
whole point of this test; it is not just "does the event eventually arrive."
"""

from __future__ import annotations

import time
import uuid
from datetime import datetime, timezone

import httpx

from .conftest import EVENT_TYPE_LONGPOLL, SUBJECT_TYPE, TOPIC_LONGPOLL, SseFrameReader


async def test_long_poll_wakes_promptly_on_publish(api, test_env):
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        tenant_id = str(uuid.uuid4())
        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_LONGPOLL,
                        "tenant_id": tenant_id,
                        "types": [EVENT_TYPE_LONGPOLL],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = sub_resp.json()["id"]

        seek_resp = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={
                "partition_positions": [
                    {"topic": TOPIC_LONGPOLL, "partition": 0, "value": "earliest"}
                ]
            },
        )
        assert seek_resp.status_code == 200

    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription_id={sub_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)
            topology_kind, _ = await reader.next_frame(timeout=5)
            assert topology_kind == "topology"

            # The stream is now idle, long-polling on
            # `DeliveryNotifier::wait_for_notification` - nothing has been
            # published yet.
            event_id = str(uuid.uuid4())
            publish_started = time.monotonic()
            async with api() as publish_client:
                publish_resp = await publish_client.post(
                    "/events",
                    json={
                        "id": event_id,
                        "type": EVENT_TYPE_LONGPOLL,
                        "tenant_id": tenant_id,
                        "source": "e2e-test",
                        "subject": "s1",
                        "subject_type": SUBJECT_TYPE,
                        "occurred_at": datetime.now(timezone.utc).isoformat(),
                    },
                )
            assert publish_resp.status_code == 202

            event_kind, event_data = await reader.next_frame(timeout=3)
            elapsed = time.monotonic() - publish_started
            assert event_kind == "event"
            assert event_data["payload"]["id"] == event_id
            assert elapsed < 3, (
                f"event took {elapsed:.2f}s to arrive after publish - the "
                "notification wake-up may not be firing (heartbeat_interval_secs "
                "defaults to 5s, so a heartbeat-only fallback would not make it "
                "under this bound)"
            )
