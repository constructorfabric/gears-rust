"""A server started while its storage backend is down (feature 0005 C2,
feature 0007 §2.1 `start`).

Boots its OWN private server, like `test_restart_durability.py`, against a
backend server of its own that the test stops before the boot and starts
again afterwards. Runs on the backends that live in a sidecar container: an
in-process medium cannot be taken down from outside.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from lib.orchestrator import GearTestEnv, _log_path, _prepare_config, _wait_healthy
from lib.sidecars import LABEL_KEY

from .conftest import (
    BACKEND,
    CONFIG,
    PROJECT_ROOT,
    SUBJECT_TYPE,
    TRACE_ID,
    SseFrameReader,
    _SIDECARS,
    _derived_event_type,
    _topic_instance,
    patch_backend,
    wait_provisioned,
)

OUTAGE_PORT = 8091
BASE_URL = f"http://localhost:{OUTAGE_PORT}/event-broker/v1"
TOPIC = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.outage.v1"
EVENT_TYPE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.outage.v1~"
# How long the backend stays down after the server is up.
DOWN_FOR_S = 3


def _patch(config_text: str, home_dir: str, sidecars) -> str:
    config_text = config_text.replace(
        'home_dir: "~/.cf-gears-event-broker"', f'home_dir: "{Path(home_dir).as_posix()}"'
    )
    config_text = patch_backend(config_text, sidecars, "evbk_e2e_outage", home_dir)
    config_text = config_text.replace(
        'bind_addr: "127.0.0.1:8080"', f'bind_addr: "127.0.0.1:{OUTAGE_PORT}"'
    )
    config_text = config_text.replace(
        "      topics:\n", f'      topics:\n        "{TOPIC}":\n          partitions: 1\n', 1
    )
    extra = "".join(
        f"        - {json.dumps(e)}\n"
        for e in (_topic_instance(TOPIC), _derived_event_type(EVENT_TYPE, TOPIC))
    )
    return config_text.replace("entities:\n", "entities:\n" + extra)


def _docker(*args: str) -> None:
    subprocess.run(["docker", *args], check=True, capture_output=True, timeout=120)


def _event(tenant_id: str) -> dict:
    return {
        "id": str(uuid.uuid4()),
        "type": EVENT_TYPE,
        "tenant_id": tenant_id,
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
async def test_a_server_started_with_its_backend_down_serves_once_it_is_up(test_env):
    """The server starts with its backend unreachable: it is live but not ready,
    and a publish to a topic it could not provision answers 503. Once the
    backend comes up the topic is provisioned without a restart, the server
    turns ready, and a published event is delivered.
    """
    sidecar_type = _SIDECARS[BACKEND]
    # A label of its own: `start` reaps every container carrying its label, and
    # the session's sidecar carries the class default.
    sidecar = type(
        f"Outage{sidecar_type.__name__}",
        (sidecar_type,),
        {
            "NAME_PREFIX": f"eb-e2e-outage-{BACKEND}",
            "LABEL": f"{LABEL_KEY}=eb-e2e-outage-{BACKEND}",
            "FIXED_HOST_PORT": True,
        },
    )()
    sidecar.start()
    proc = None
    try:
        # Stopped, not removed: the same container comes back on the same port.
        _docker("stop", sidecar.container_id)

        home_dir = tempfile.mkdtemp(prefix="cf-gears-e2e-event-broker-outage-")
        env = GearTestEnv(
            config_path=CONFIG,
            config_patch=lambda text, _env: _patch(text, home_dir, [sidecar]),
            port=OUTAGE_PORT,
            health_path="/healthz",
            health_timeout=60,
            log_suffix="event-broker-outage",
        )
        config_path = _prepare_config(env)
        proc = subprocess.Popen(
            [os.environ["E2E_BINARY"], "--config", str(config_path), "run"],
            cwd=str(PROJECT_ROOT),
            stdout=open(_log_path(env), "a"),
            stderr=subprocess.STDOUT,
            env={**os.environ},
        )
        _wait_healthy(env)
        time.sleep(DOWN_FOR_S)

        origin = f"http://localhost:{OUTAGE_PORT}"
        tenant_id = str(uuid.uuid4())
        async with httpx.AsyncClient(timeout=10, headers={"traceparent": f"00-{TRACE_ID}-b7ad6b7169203331-01"}) as client:
            ready = await client.get(f"{origin}/readyz")
            assert (ready.status_code, ready.json()) == (
                503,
                {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
                    "title": "Internal",
                    "status": 503,
                    "detail": "An internal error occurred. Please retry later.",
                    "instance": "/readyz",
                    "trace_id": TRACE_ID,
                    "context": {},
                },
            )
            refused = await client.post(f"{BASE_URL}/events", json=_event(tenant_id))
            assert (refused.status_code, refused.json()) == (
                503,
                {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
                    "title": "Service Unavailable",
                    "status": 503,
                    "detail": f"topic '{TOPIC}' is not provisioned on its storage backend yet",
                    "instance": "/event-broker/v1/events",
                    "trace_id": TRACE_ID,
                    "context": {},
                },
            )

        _docker("start", sidecar.container_id)
        sidecar._wait_ready()

        deadline = time.monotonic() + 90
        while httpx.get(f"{origin}/readyz", timeout=5).status_code != 200:
            assert time.monotonic() < deadline, "the server never turned ready after the backend came up"
            time.sleep(0.5)
        wait_provisioned(BASE_URL, {TOPIC: EVENT_TYPE})

        async with httpx.AsyncClient(base_url=BASE_URL, timeout=10) as client:
            group = await client.post("/consumer-groups")
            assert group.status_code == 201
            sub = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group.json()["id"],
                    "client_agent": "e2e-test",
                    "interests": [{"topic": TOPIC, "tenant_id": tenant_id, "types": [EVENT_TYPE]}],
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

        async with httpx.AsyncClient(base_url=BASE_URL, timeout=None) as stream_client:
            async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as resp:
                assert resp.status_code == 200
                reader = SseFrameReader(resp)
                await reader.await_kind("topology", timeout=5)
                payload = (await reader.await_kind("event", timeout=15))["payload"]
        assert payload == {
            **{k: event[k] for k in ("id", "type", "tenant_id", "source", "subject", "subject_type")},
            "topic": TOPIC,
            "occurred_at": payload["occurred_at"],
            "trace_parent": None,
            "data": None,
            "partition": 0,
            "sequence": 1,
            "sequence_time": payload["sequence_time"],
        }
    finally:
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
        sidecar.stop()
