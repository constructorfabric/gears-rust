"""Operational tests - properties of the running broker that no scenario defines.

Every other file in this suite asserts the documented HTTP contract, and every
test in them names the scenario it covers in its docstring. The tests here have
no scenario on purpose: they assert operational properties - delivery latency,
survival across a restart - that the contract does not specify at all.

``test_restart_durability.py`` belongs to this category too but deliberately
keeps both its own file and its own name. It boots a private server so it can
stop and restart the binary mid-test, and that damages the shared session server
for every test collected after it - so it is only safe while its filename sorts
last in this directory. Renaming it into a ``test_operational_*`` form moved it
earlier in collection and broke 39 downstream tests. Leave the name alone until
that test is made genuinely isolated; new operational tests go here instead.

Conventions for anything added here:

- Put the bound in a module constant and say, in the constant's comment, where
  the figure came from and what it does NOT prove. A number not derived from a
  scenario or from DESIGN.md is a working figure, not a guarantee, and has to be
  labelled as one so a later reader does not mistake it for contract.
- Keep each test's subject narrow enough that a failure names one property.
- If a scenario is later written for one of these properties, move that test out
  of this file and tag it with the scenario, so this file stays exactly the set
  of things the contract is silent about.
"""

from __future__ import annotations

import asyncio
import time
from datetime import datetime, timezone

import pytest

from .conftest import (
    DEFAULT_TENANT_ID,
    EVENT_TYPE_LONGPOLL,
    SUBJECT_TYPE,
    TOPIC_LONGPOLL,
    SseFrameReader,
)
from .test_consumer_stream import (
    _check_owner_request,
    _live_event_fields,
    _live_group,
    _live_heartbeat,
    _live_join,
    _live_manifest_bounds,
    _live_seek_earliest,
    _live_sse_frame,
    _live_stream,
    _live_topology,
)
from .test_publish_consume import _stream_instant

# Ceiling on publish-ack -> delivery for an event published onto an already-open
# stream. A working figure, not a documented guarantee: no scenario states a
# delivery-latency bound, and DESIGN.md fixes only the 30s long-poll max timeout.
#
# It sits ABOVE the 5s heartbeat cadence, so it proves delivery happens at all
# and nothing more - it cannot tell an active wake-up apart from delivery that
# rode the next heartbeat. Proving the wake-up itself needs a sub-5s bound, and
# that number has to come from a scenario before a test can assert it.
DELIVERY_BOUND = 15.0


@pytest.mark.backend_sensitive
@pytest.mark.timeout(60, func_only=True)
async def test_published_event_reaches_open_stream_within_bound(api, test_env):
    """An event published while the TA SSE stream is open arrives within 15 seconds."""
    topic = TOPIC_LONGPOLL
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_LONGPOLL],
    }
    event = {
        "id": "29e20000-0000-4000-8002-000000000001",
        "type": EVENT_TYPE_LONGPOLL,
        "subject_tenant": DEFAULT_TENANT_ID,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-19T10:20:30Z",
    }
    async with api() as client:
        bounds = await _live_manifest_bounds(client, topic, [0])
        # The mixed-topic batch is rejected before any LONGPOLL event is stored.
        assert bounds == {0: (0, 0)}
        offsets = {0: bounds[0][0]}
        group_id = await _live_group(client)
        sub_id = await _live_join(client, group_id, interest, [0])
        await _live_seek_earliest(client, sub_id, topic, offsets)

        async with _live_stream(api, sub_id) as response:
            assert response.headers["content-type"] == "text/event-stream"
            reader = SseFrameReader(response)
            clock = asyncio.get_running_loop().time
            _live_topology(await _live_sse_frame(reader, clock() + 5), topic, offsets)
            began_at = datetime.now(timezone.utc).replace(microsecond=0)
            published = await client.post("/events", json=event)
            acknowledged_at = time.monotonic()
            _check_owner_request(published)
            assert (published.status_code, published.content) == (202, b"")

            while True:
                remaining = DELIVERY_BOUND - (time.monotonic() - acknowledged_at)
                assert remaining > 0, "delivery deadline elapsed"
                name, frame = await reader.next_frame(timeout=remaining)
                received_at = time.monotonic()
                assert isinstance(frame, dict)
                assert name == frame.get("kind"), (name, frame)
                if name != "heartbeat":
                    break
                _live_heartbeat(frame)

            payload = _live_event_fields(frame, began_at)
            assert payload["sequence"] > offsets[0]
            assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
            assert frame == {
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
            elapsed = received_at - acknowledged_at
            assert elapsed < DELIVERY_BOUND, f"delivery took {elapsed:.6f}s"
            print(f"ack-to-event: {elapsed:.9f}s ({acknowledged_at=:.9f}, {received_at=:.9f})")
