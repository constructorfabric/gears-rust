"""A server started while its storage backend is down (feature 0005 C2,
feature 0007 §2.1 `start`).

Boots its OWN private server, like `test_restart_durability.py`, against a
backend server of its own that the test stops before the boot and starts
again afterwards. Runs on the backends that live in a sidecar container: an
in-process medium cannot be taken down from outside.
"""

from __future__ import annotations

import asyncio
import subprocess
import time
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone

import httpx
import pytest

from lib.sidecars import LABEL_KEY

from .conftest import (
    API_BASE,
    BACKEND,
    SUBJECT_TYPE,
    TRACE_ID,
    TRACEPARENT,
    SseFrameReader,
    _SIDECARS,
    _await_log_line,
    _derived_event_type,
    _topic_instance,
    patch_backend,
    public_headers,
    wait_provisioned,
)
from .topology import (
    OWNER_TENANT,
    RECHECK,
    STARTUP_TIMEOUT,
    FlightControlBrokerEnv,
    _event_fields,
    own_sidecars,
    replace_once,
)

TOPIC = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.outage.v1"
EVENT_TYPE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.outage.v1~"


@contextmanager
def _outage_env(root, *, down_at_start):
    sidecar_type = _SIDECARS[BACKEND]
    suffix = uuid.uuid4().hex[:8]
    sidecar = type(
        f"Outage{sidecar_type.__name__}",
        (sidecar_type,),
        {"NAME_PREFIX": f"eb-e2e-outage-{BACKEND}", "FIXED_HOST_PORT": True},
    )(label=f"{LABEL_KEY}=eb-e2e-outage-{BACKEND}-{suffix}")

    def config(text, home, sidecars):
        text = patch_backend(text, sidecars, f"evbk_e2e_outage_{suffix}", home)
        return replace_once(
            text,
            "      topics:\n",
            f'      topics:\n        "{TOPIC}":\n          partitions: 1\n',
        )

    env = FlightControlBrokerEnv(
        root, broker_config=config,
        documents=[_topic_instance(TOPIC), _derived_event_type(EVENT_TYPE, TOPIC)],
        health_path="/healthz",
    )
    with own_sidecars([sidecar], redact=env.redact) as sidecars:
        env.sidecars = sidecars
        if down_at_start:
            _docker("stop", sidecar.container_id)
        with env:
            yield env, sidecar


def _docker(*args: str) -> None:
    subprocess.run(["docker", *args], check=True, capture_output=True, timeout=120)


def _event(tenant_id: str) -> dict:
    return {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": datetime.now(timezone.utc).isoformat(),
    }


@pytest.mark.skipif(
    BACKEND not in _SIDECARS,
    reason="needs a backend server in a sidecar container the test can stop; "
    "runs with EB_E2E_BACKEND=postgres, mysql, mariadb or kafka",
)
@pytest.mark.backend_sensitive
@pytest.mark.timeout(150, func_only=True)
async def test_a_server_started_with_its_backend_down_serves_once_it_is_up(test_env, tmp_path):
    """The live worker attaches its routes while storage is down.

    With routes attached and storage still down, a publish to the unprovisioned
    topic answers 503. When storage returns, the same worker provisions the
    topic and delivers a published event.
    """
    with _outage_env(tmp_path, down_at_start=True) as (env, sidecar):
        broker_pid = env.broker_pid
        identity = env.redact(f"Event Broker pid={broker_pid} at {env.base_url}")
        deadline = time.monotonic() + STARTUP_TIMEOUT
        attached = "OoP gear routes attached (now serving)"
        last_attach = None
        while time.monotonic() < deadline:
            last_attach = _event_fields(env.log()).get(attached, {}).get("gear")
            assert env.broker_running(), env.redact(
                f"{identity} exited before final route attachment with last gear {last_attach!r}"
            )
            if last_attach == "event-broker" and time.monotonic() < deadline:
                break
            await asyncio.sleep(min(RECHECK, max(0, deadline - time.monotonic())))
        else:
            raise AssertionError(env.redact(
                f"{identity} did not report final route attachment: "
                f"last attachment gear={last_attach!r}"
            ))

        base_url = f"{env.base_url}{API_BASE}"
        tenant_id = OWNER_TENANT
        async with httpx.AsyncClient(
            timeout=10, headers={**public_headers(), "traceparent": TRACEPARENT}, trust_env=False,
        ) as client:
            refused = await client.post(f"{base_url}/events", json=_event(tenant_id))
            assert (refused.status_code, refused.headers.get("content-type")) == (
                503, "application/problem+json",
            ), env.redact(
                f"{base_url}/events on Event Broker pid={env.broker_pid} returned "
                f"HTTP {refused.status_code}, {refused.headers.get('content-type')!r}"
            )
            assert refused.json() == {
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
                "title": "Service Unavailable",
                "status": 503,
                "detail": "Service temporarily unavailable",
                "instance": "/event-broker/v1/events",
                "trace_id": TRACE_ID,
                "context": {},
            }
            _await_log_line(env, f"topic '{TOPIC}' is not provisioned on its storage backend yet")

        _docker("start", sidecar.container_id)
        sidecar._wait_ready()

        env.wait_ready(deadline=time.monotonic() + 90)
        wait_provisioned(base_url, {TOPIC: EVENT_TYPE})

        async with httpx.AsyncClient(
            base_url=base_url, timeout=10, headers=public_headers(),
        ) as client:
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
            assert seek.json() == {"positions": {TOPIC: [{"partition": 0, "value": 0}]}}
            event = _event(tenant_id)
            published = await client.post("/events", json=event)
            assert published.status_code == 202

        async with httpx.AsyncClient(
            base_url=base_url, timeout=None, headers=public_headers(),
        ) as stream_client:
            async with stream_client.stream("GET", f"/events:sse?subscription={sub_id}") as resp:
                assert resp.status_code == 200
                reader = SseFrameReader(resp)
                await reader.await_kind("topology", timeout=5)
                payload = (await reader.await_kind("event", timeout=15))["payload"]
        assert payload == {
            **{k: event[k] for k in ("id", "type", "subject_tenant", "source", "subject", "subject_type")},
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": 1,
            "sequence_time": payload["sequence_time"],
        }
        current_pid = env.broker_pid
        assert (current_pid, env.broker_running()) == (broker_pid, True), env.redact(
            f"{identity} did not stay live through delivery (current pid={current_pid})"
        )


@pytest.mark.skipif(
    BACKEND not in _SIDECARS,
    reason="needs a backend server in a sidecar container the test can stop; "
    "runs with EB_E2E_BACKEND=postgres, mysql, mariadb or kafka",
)
@pytest.mark.backend_sensitive
@pytest.mark.timeout(150, func_only=True)
async def test_sync_publish_times_out_to_202_when_the_backend_is_down(test_env, tmp_path):
    """A synchronous publish (`Prefer: wait=N`) to a provisioned topic whose
    backend is then unreachable is admitted and durably enqueued (the producer
    chain lives in the gear database, not the event backend), but the outbox
    append never succeeds, so the broker answers a deterministic `202 Accepted`
    once N elapses.

    This is the deterministic companion to the `wait=0` single-publish test: on
    a live backend that test races the drain, whereas a stopped backend makes a
    timed-out wait certain.
    """
    with _outage_env(tmp_path, down_at_start=False) as (env, sidecar):
        base_url = f"{env.base_url}{API_BASE}"
        env.wait_ready(deadline=time.monotonic() + 90)
        wait_provisioned(base_url, {TOPIC: EVENT_TYPE})

        # Take the backend down now that the topic is provisioned.
        _docker("stop", sidecar.container_id)

        tenant_id = OWNER_TENANT
        # The 30-second client deadline includes the worker's full 20-second
        # publish hold while storage is down.
        async with httpx.AsyncClient(timeout=30, headers=public_headers()) as client:
            resp = await client.post(
                f"{base_url}/events",
                headers={"Prefer": "wait=20"},
                json=_event(tenant_id),
            )
        # Admitted and enqueued, but the backend append cannot complete while the
        # backend is down, so the wait elapses to 202.
        assert resp.status_code == 202
        assert resp.text == "", "202 Accepted must carry no body"
        assert resp.headers.get("preference-applied") == "wait=20"

        # Bring the backend back so the container stops cleanly in teardown.
        _docker("start", sidecar.container_id)
        sidecar._wait_ready()
