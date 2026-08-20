"""E2E fixtures for the event-broker gear (design.md D8, task group 11).

event-broker has its OWN standalone binary (`cf-gears-event-broker-server`),
not a feature flag on `cf-gears-example-server` - gated on `E2E_BINARY`
exactly like mini-chat/usage-collector, so running the shared `make
e2e-local` suite never boots a second server for routes it doesn't serve.

Run it with: make e2e-event-broker

`types-registry`'s config-seeded `entities:` are committed to queryable
storage once, at boot (`post_init`), with no live-refresh path (see
`config/event-broker-standalone.yaml`'s own header comment) - registering a
NEW topic at runtime via `POST /types-registry/v1/entities` would NOT become
visible to event-broker's own specification cache without a full restart.
So every test that shares the session-scoped `test_env` server gets its own
PRE-PROVISIONED topic baked into the config at session-setup time (below),
rather than registering one for itself at test time - there is no other way
to get per-test topic isolation against a server that boots once for the
whole session.
"""

from __future__ import annotations

import asyncio
import json
import os
import tempfile
from pathlib import Path

import httpx
import pytest

from lib.orchestrator import GearTestEnv

# ── Constants ─────────────────────────────────────────────────────────────

HERE = Path(__file__).resolve().parent
PROJECT_ROOT = Path(__file__).resolve().parents[4]
CONFIG = PROJECT_ROOT / "config" / "event-broker-standalone.yaml"

SERVER_PORT = 8089
API_BASE = "/event-broker/v1"
REQUEST_TIMEOUT = 5.0

# One dedicated topic + event type per test that needs the shared session
# server, per this module's own doc comment above - never share a topic
# across two tests, since a fresh subscription always replays a topic's
# whole history from the start (`domain/delivery.rs`'s `stream()` seeds its
# replay cursor from the subscription's own join-time `assigned.offset`,
# which is always `0`, not from whatever `seek()` last persisted).
SUBJECT_TYPE = "gts.cf.e2e.event_broker.subject.v1~"
TOPIC_HAPPY = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.happy.v1"
EVENT_TYPE_HAPPY = "gts.cf.core.events.event_type.v1~cf.e2e.event_broker.happy.v1"
TOPIC_LONGPOLL = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.longpoll.v1"
EVENT_TYPE_LONGPOLL = "gts.cf.core.events.event_type.v1~cf.e2e.event_broker.longpoll.v1"

_EXTRA_ENTITIES = [
    {"id": TOPIC_HAPPY, "partitions": 1, "created_at": "2026-01-01T00:00:00Z"},
    {
        "id": EVENT_TYPE_HAPPY,
        "topic_id": TOPIC_HAPPY,
        "allowed_subject_types": [SUBJECT_TYPE],
        "data_schema": {},
        "created_at": "2026-01-01T00:00:00Z",
    },
    {"id": TOPIC_LONGPOLL, "partitions": 1, "created_at": "2026-01-01T00:00:00Z"},
    {
        "id": EVENT_TYPE_LONGPOLL,
        "topic_id": TOPIC_LONGPOLL,
        "allowed_subject_types": [SUBJECT_TYPE],
        "data_schema": {},
        "created_at": "2026-01-01T00:00:00Z",
    },
]

_TEMP_HOME = tempfile.mkdtemp(prefix="cf-gears-e2e-event-broker-")


# ── Environment gate ──────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def _require_dedicated_binary():
    if not os.environ.get("E2E_BINARY"):
        pytest.skip(
            "E2E_BINARY not set — run these tests via: make e2e-event-broker",
            allow_module_level=True,
        )


def pytest_collection_modifyitems(items):
    """Exclude this suite's own session-fixture (server boot) startup cost
    from pytest.ini's global 10s per-test timeout - matches
    `usage_collector/conftest.py`'s identical hook and its own doc comment
    for the full rationale (`func_only=True` bounds only the test body, not
    fixture setup; both sides resolve their paths for the same reason).
    """
    for item in items:
        if HERE not in Path(str(item.fspath)).resolve().parents:
            continue
        if item.get_closest_marker("timeout") is None:
            item.add_marker(pytest.mark.timeout(func_only=True))


# ── Test environment ──────────────────────────────────────────────────────

def _patch_config(config_text: str, env: GearTestEnv) -> str:
    config_text = config_text.replace(
        'home_dir: "~/.cf-gears-event-broker"',
        f'home_dir: "{_TEMP_HOME}"',
    )
    config_text = config_text.replace(
        'bind_addr: "127.0.0.1:8080"',
        f'bind_addr: "127.0.0.1:{SERVER_PORT}"',
    )
    extra = "".join(f"        - {json.dumps(e)}\n" for e in _EXTRA_ENTITIES)
    return config_text.replace("entities:\n", "entities:\n" + extra)


@pytest.fixture(scope="session")
def gear_test_env() -> GearTestEnv:
    return GearTestEnv(
        binary="cf-gears-event-broker-server",
        config_path=CONFIG,
        config_patch=_patch_config,
        port=SERVER_PORT,
        health_path="/healthz",
        health_timeout=30,
        env={"RUST_LOG": os.environ.get("RUST_LOG", "info,event_broker=debug")},
        log_suffix="event-broker",
    )


# ── HTTP helpers ──────────────────────────────────────────────────────────

@pytest.fixture
def api(test_env):
    """Async client factory bound to the running server's event-broker API."""

    def _client() -> httpx.AsyncClient:
        return httpx.AsyncClient(
            base_url=f"{test_env.base_url}{API_BASE}",
            timeout=REQUEST_TIMEOUT,
        )

    return _client


class SseFrameReader:
    """Reads SSE frames (`event: <kind>\\ndata: <json>\\n\\n`) one at a time
    off a streaming `httpx.Response`, buffering across chunks - a real
    socket gives no guarantee that one frame arrives as exactly one
    `aiter_bytes()` read, or that two frames don't arrive in the same one.
    """

    def __init__(self, response: httpx.Response):
        self._iter = response.aiter_bytes()
        self._buf = ""

    async def next_frame(self, timeout: float = 5.0) -> tuple[str, dict]:
        async def _read():
            while True:
                idx = self._buf.find("\n\n")
                if idx != -1:
                    raw, self._buf = self._buf[:idx], self._buf[idx + 2 :]
                    assert raw.startswith("event: "), f"malformed SSE block: {raw!r}"
                    event, _, data = raw[len("event: ") :].partition("\ndata: ")
                    return event, json.loads(data)
                chunk = await self._iter.__anext__()
                self._buf += chunk.decode("utf-8")

        return await asyncio.wait_for(_read(), timeout=timeout)
