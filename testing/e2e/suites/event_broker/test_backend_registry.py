"""The backend registry and routing (feature 0005 §2.1-§2.4): S1, S2 and S8.

Each test boots its OWN private server, like `test_backend_outage.py`, with
the committed config's `backends` and `backend_routing` replaced. The entries
are in-process media (memory, SQLite) or an address nothing listens on, so
these run whatever `EB_E2E_BACKEND` the suite was started with.
"""

from __future__ import annotations

import re
import sqlite3
import subprocess
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from .conftest import (
    API_BASE,
    MEMORY_BACKEND,
    POSTGRES_BACKEND,
    SQLITE_BACKEND,
    SUBJECT_TYPE,
    TRACE_ID,
    _await_log_line,
    _derived_event_type,
    _topic_instance,
    public_headers,
    wait_provisioned,
)
from .topology import OWNER_TENANT, FlightControlBrokerEnv, replace_once

TOPIC_BASE = "gts.cf.core.events.topic.v1~"
MAIN_TOPIC = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.registry_main.v1"
MAIN_TYPE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.registry_main.v1~"
AUDIT_TOPIC = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.registry_audit.v1"
AUDIT_TYPE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.registry_audit.v1~"
# How long a refused configuration may take to stop the process.
REFUSAL_TIMEOUT_S = 30


def _patch(config_text: str, backends: str) -> str:
    config_text, replaced = re.subn(
        r"      backends:\n.*?(?=      # Per-topic settings)",
        lambda _: backends,
        config_text,
        flags=re.S,
    )
    assert replaced == 1, "expected one worker backend and routing block"
    return replace_once(
        config_text,
        "      topics:\n",
        "      topics:\n"
        f'        "{MAIN_TOPIC}":\n          partitions: 1\n'
        f'        "{AUDIT_TOPIC}":\n          partitions: 1\n',
    )


def _env(root: Path, backends: str, *, refusal: bool = False) -> FlightControlBrokerEnv:
    return FlightControlBrokerEnv(
        root,
        broker_config=lambda text, _home, _sidecars: _patch(text, backends),
        documents=[
            _topic_instance(MAIN_TOPIC),
            _derived_event_type(MAIN_TYPE, MAIN_TOPIC),
            _topic_instance(AUDIT_TOPIC),
            _derived_event_type(AUDIT_TYPE, AUDIT_TOPIC),
        ],
        refusal=refusal,
    )


def _event(event_type: str, tenant_id: str) -> dict:
    return {
        "id": str(uuid.uuid4()),
        "type": event_type,
        "subject_tenant": tenant_id,
        "source": "e2e-test",
        "subject": "s1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": datetime.now(timezone.utc).isoformat(),
    }


def _end_sequence(client: httpx.Client, topic: str) -> int | None:
    resp = client.get("/topics/segments", params={"topic": topic, "partition": "0"})
    assert resp.status_code == 200, resp.text
    return resp.json()["end_sequence"]


def _await_end_sequence(client: httpx.Client, topic: str, expected: int) -> None:
    deadline = time.monotonic() + 15
    while _end_sequence(client, topic) != expected:
        assert time.monotonic() < deadline, f"{topic} never reached sequence {expected}"
        time.sleep(0.1)


# ── S1: a configuration breaking a startup rule refuses to start ──────────

_REFUSED = [
    pytest.param(
        f'      backends:\n        main: {{ type: "gts.cf.core.events.backend.v1~cf.e2e.unknown.backend.v1~" }}\n'
        f'      backend_routing:\n        "{TOPIC_BASE}": main\n\n',
        "which no provider linked into this build serves",
        "C1",
        id="C1-unknown-backend-type",
    ),
    pytest.param(
        f'      backends:\n        main: {{ type: "{MEMORY_BACKEND}" }}\n'
        f'      backend_routing:\n        "{TOPIC_BASE}": main\n        "{AUDIT_TOPIC}": archive\n\n',
        f"the route for '{AUDIT_TOPIC}' names no registry entry",
        "C3",
        id="C3-route-to-absent-entry",
    ),
    pytest.param(
        f'      backends:\n        main: {{ type: "{MEMORY_BACKEND}" }}\n'
        f'      backend_routing:\n        "{AUDIT_TOPIC}": main\n\n',
        f"there is no route for `{TOPIC_BASE}`",
        "C4",
        id="C4-missing-instance-less-route",
    ),
    pytest.param(
        f'      backends:\n        main: {{ type: "{MEMORY_BACKEND}" }}\n'
        f'        spare: {{ type: "{MEMORY_BACKEND}", store: "spare" }}\n'
        f'      backend_routing:\n        "{TOPIC_BASE}": main\n\n',
        "entry 'spare' is named by no route",
        "C5",
        id="C5-unreferenced-entry",
    ),
]


@pytest.mark.parametrize(("backends", "message", "rule"), _REFUSED)
@pytest.mark.timeout(60, func_only=True)
def test_a_configuration_breaking_a_startup_rule_refuses_to_start(
    tmp_path, backends, message, rule,
):
    """S1: the process stops before serving, and its log names the rule."""
    with _env(tmp_path, backends, refusal=True) as env:
        try:
            exit_code = env.wait_for_broker_exit(REFUSAL_TIMEOUT_S)
        except subprocess.TimeoutExpired:
            pytest.fail(f"the worker kept running with a configuration breaking {rule}")
        log = env.log()
    assert exit_code != 0, log[-2000:]
    assert message in log, log[-2000:]
    assert f"feature 0005 section 2.4, {rule}" in log, log[-2000:]


# ── S2: two backends, each topic stored by its own ────────────────────────


@pytest.mark.timeout(120, func_only=True)
def test_two_registry_entries_each_store_the_topics_routed_to_them(tmp_path):
    """S2: with two SQLite entries and the audit topic routed to the second,
    each topic is provisioned and stored in its own entry's file, and both
    read back through the API."""
    main_db = tmp_path / "main.db"
    audit_db = tmp_path / "audit.db"
    backends = (
        "      backends:\n"
        f'        main: {{ type: "{SQLITE_BACKEND}", path: "{main_db.as_posix()}" }}\n'
        f'        audit: {{ type: "{SQLITE_BACKEND}", path: "{audit_db.as_posix()}" }}\n'
        "      backend_routing:\n"
        f'        "{TOPIC_BASE}": main\n'
        f'        "{AUDIT_TOPIC}": audit\n\n'
    )
    with _env(tmp_path, backends) as env:
        base_url = f"{env.base_url}{API_BASE}"
        wait_provisioned(base_url, {MAIN_TOPIC: MAIN_TYPE, AUDIT_TOPIC: AUDIT_TYPE})
        tenant_id = OWNER_TENANT
        with httpx.Client(base_url=base_url, timeout=10, headers=public_headers()) as client:
            for event_type in (MAIN_TYPE, MAIN_TYPE, AUDIT_TYPE):
                resp = client.post("/events", json=_event(event_type, tenant_id))
                assert resp.status_code == 202, resp.text
            _await_end_sequence(client, MAIN_TOPIC, 2)
            _await_end_sequence(client, AUDIT_TOPIC, 1)

    def stored(db: Path) -> dict[str, int]:
        with sqlite3.connect(db) as conn:
            return dict(
                conn.execute(
                    "SELECT l.topic_id, COUNT(e.sequence) FROM evbk__log AS l "
                    "LEFT JOIN evbk__event AS e ON e.topic_uuid = l.topic_uuid "
                    "WHERE l.topic_id IN (?, ?) GROUP BY l.topic_id",
                    (MAIN_TOPIC, AUDIT_TOPIC),
                ).fetchall()
            )

    assert stored(main_db) == {MAIN_TOPIC: 2}
    assert stored(audit_db) == {AUDIT_TOPIC: 1}


# ── S8: an unreachable backend at start ───────────────────────────────────


@pytest.mark.timeout(120, func_only=True)
def test_an_unreachable_backend_does_not_stop_the_start(tmp_path):
    """S8 / AC-11: with the audit topic routed to a PostgreSQL entry nothing
    listens for, the server starts, the main entry's topic is published to and
    read back, and a publish to the audit topic answers 503."""
    backends = (
        "      backends:\n"
        f'        main: {{ type: "{MEMORY_BACKEND}" }}\n'
        f'        audit: {{ type: "{POSTGRES_BACKEND}", url: "postgres://eb@127.0.0.1:1/eb", schema: "evbk" }}\n'
        "      backend_routing:\n"
        f'        "{TOPIC_BASE}": main\n'
        f'        "{AUDIT_TOPIC}": audit\n\n'
    )
    with _env(tmp_path, backends) as env:
        assert env.broker_running(), "the worker stopped with one backend unreachable"
        base_url = f"{env.base_url}{API_BASE}"
        wait_provisioned(base_url, {MAIN_TOPIC: MAIN_TYPE})
        tenant_id = OWNER_TENANT
        traceparent = {"traceparent": f"00-{TRACE_ID}-b7ad6b7169203331-01"}
        with httpx.Client(
            base_url=base_url, timeout=10, headers={**public_headers(), **traceparent},
        ) as client:
            published = client.post("/events", json=_event(MAIN_TYPE, tenant_id))
            assert published.status_code == 202, published.text
            _await_end_sequence(client, MAIN_TOPIC, 1)

            refused = client.post("/events", json=_event(AUDIT_TYPE, tenant_id))
            assert (refused.status_code, refused.json()) == (
                503,
                {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
                    "title": "Service Unavailable",
                    "status": 503,
                    "detail": "Service temporarily unavailable",
                    "instance": "/event-broker/v1/events",
                    "trace_id": TRACE_ID,
                    "context": {},
                },
            )
            _await_log_line(env, f"topic '{AUDIT_TOPIC}' is not provisioned on its storage backend yet")
