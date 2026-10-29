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
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from lib.orchestrator import GearTestEnv
from lib.sidecars import KafkaSidecar, MariaDbSidecar, MySqlSidecar, PostgresSidecar

# ── Constants ─────────────────────────────────────────────────────────────

HERE = Path(__file__).resolve().parent
PROJECT_ROOT = Path(__file__).resolve().parents[4]
CONFIG = PROJECT_ROOT / "config" / "event-broker-standalone.yaml"

SERVER_PORT = 8089
API_BASE = "/event-broker/v1"
REQUEST_TIMEOUT = 5.0

# Every client sends this valid W3C `traceparent`, so the wire `trace_id` the
# canonical error layer echoes is deterministic. `extract_trace_id` prefers the
# live OTel span, but the request span continues this inbound `traceparent`, so
# its trace-id equals the header's; with OTel off the header is used directly.
# Either way the value is TRACE_ID, the header's 32-hex trace-id segment. Uses
# the W3C spec's example ids.
TRACE_ID = "0af7651916cd43dd8448eb211c80319c"
TRACEPARENT = f"00-{TRACE_ID}-b7ad6b7169203331-01"

SUBJECT_TYPE = "gts.cf.e2e.event_broker.subject.v1~"

# Active streaming topic: tests publish events here and verify delivery via
# the SSE or multipart stream. Each test generates a unique tenant_id so
# events from different tests never mix in the same subscription filter.
# A fresh subscription replays the topic's whole history from offset 0
# (`domain/delivery.rs`'s `stream()` seeds the cursor from the join-time
# `assigned.offset`, not from whatever seek() last persisted), so tests must
# use a unique tenant_id to isolate their own events from prior test runs.
TOPIC_STREAM = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.stream.v1"
EVENT_TYPE_STREAM = "gts.cf.core.events.event.v1~cf.e2e.event_broker.stream.v1~"

# The all-event-types wildcard pattern: a trailing `*` on the event base type,
# so it matches every derived event type. This is the interest the SDK emits for
# a topic-only subscription (`.topics([...])` with no explicit event-type
# patterns), and the shape whose delivery had no black-box coverage until the
# wildcard-delivery test below.
ALL_EVENT_TYPES_PATTERN = "gts.cf.core.events.event.v1~*"

# Long-poll topic: dedicated to heartbeat and wake-on-publish timing tests.
# Kept separate so its quiet periods don't race with TOPIC_STREAM's traffic.
TOPIC_LONGPOLL = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.longpoll.v1"
EVENT_TYPE_LONGPOLL = "gts.cf.core.events.event.v1~cf.e2e.event_broker.longpoll.v1~"

# 4-partition topic for rebalance tests (subscriptions/1.11, 1.12, 1.13;
# consumer/flows/1.01): needs enough partitions to split across multiple members.
TOPIC_4P = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.four_part.v1"
EVENT_TYPE_4P = "gts.cf.core.events.event.v1~cf.e2e.event_broker.four_part.v1~"

# Key-order topic (feature 0007 §11 S1): four partitions, so two keys may land
# on different ones and each key's order is checked within its own.
TOPIC_ORDER = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.key_order.v1"
EVENT_TYPE_ORDER = "gts.cf.core.events.event.v1~cf.e2e.event_broker.key_order.v1~"

# Never-published topic (feature 0007 §11 S5): a consumer joins and seeks it
# before any producer publishes, so its first event is sequence 1.
TOPIC_FRESH = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.fresh.v1"
EVENT_TYPE_FRESH = "gts.cf.core.events.event.v1~cf.e2e.event_broker.fresh.v1~"

# Strict-schema topic for schema-validation tests (producer/single/1.03):
# its event type requires data.strict_field as a string, so publishing
# without it triggers a 400 schema-validation failure.
TOPIC_STRICT = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.strict.v1"
EVENT_TYPE_STRICT = "gts.cf.core.events.event.v1~cf.e2e.event_broker.strict.v1~"

# Read-only topic for seek/cursor tests: seeded with PREFILLED_COUNT events
# at session bootstrap via the `prefilled_topic` fixture; no test ever
# publishes here. The medium is fresh each session, so "earliest" is 0 and
# "latest" is the last seeded event's sequence, which the fixture records.
TOPIC_PREFILLED = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.prefilled.v1"
EVENT_TYPE_PREFILLED = "gts.cf.core.events.event.v1~cf.e2e.event_broker.prefilled.v1~"
PREFILLED_TENANT_ID = "e2e00000-0000-0000-0000-000000000000"
PREFILLED_COUNT = 100
# How long the prefill fixture waits for its batch to become durable.
PREFILL_INGEST_TIMEOUT = 30.0

# Retention topic (backend retention, observed through SEEK and delivery): a
# short age bound, and the only topic whose events a backend removes during
# the session. RETENTION_SEGMENT_EVENTS old events fill exactly one storage
# segment on the backends that expire whole segments (the backend block below
# sets their segment size to it), so every backend removes exactly those.
TOPIC_RETENTION = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.retention.v1"
EVENT_TYPE_RETENTION = "gts.cf.core.events.event.v1~cf.e2e.event_broker.retention.v1~"
RETENTION_DURATION_S = 8
RETENTION_SEGMENT_EVENTS = 1000

# Drained topic: the same age bound as TOPIC_RETENTION, for the test that lets
# every stored event age out and checks "latest" still names the last one.
TOPIC_DRAIN = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.drain.v1"
EVENT_TYPE_DRAIN = "gts.cf.core.events.event.v1~cf.e2e.event_broker.drain.v1~"

# Byte-bounded topic: RETENTION_BYTES per partition and the default age, so
# only the byte bound removes anything during the session.
TOPIC_BYTES = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.bytes.v1"
EVENT_TYPE_BYTES = "gts.cf.core.events.event.v1~cf.e2e.event_broker.bytes.v1~"
RETENTION_BYTES = 100_000

# Progress topic: a producer fills it with events a narrowly filtered consumer
# rejects, so the consumer's frontier drifts and a `control:progress` frame is
# owed. PROGRESS_DRIFT_THRESHOLD and PROGRESS_MIN_INTERVAL_S are the streaming
# settings the generated config sets, below every other test's rejected scan.
TOPIC_PROGRESS = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.progress.v1"
EVENT_TYPE_PROGRESS = "gts.cf.core.events.event.v1~cf.e2e.event_broker.progress.v1~"
PROGRESS_DRIFT_THRESHOLD = 500
PROGRESS_MIN_INTERVAL_S = 1

# Shared xfail marker for tests that require real auth enforcement.
# The standalone config sets ``auth_disabled: true``; these tests document the
# expected behaviour and will pass once a multi-tenant config is wired.
XFAIL_AUTH_DISABLED = pytest.mark.xfail(
    reason="standalone config sets auth_disabled: true; tenant isolation is not enforced",
    strict=True,
)

# Quotas and caps - rate limits, consumer-group capacity, interest counts - are
# out of scope for now. The tests that pin their rejections are parked until
# those limits are implemented and made configurable.
SKIP_LIMITS = pytest.mark.skip(reason="limits are not implemented yet")

# One partition per topic: every test here asserts a single-partition
# assignment, and a topic carries no partition count of its own - it is
# `event-broker` configuration, written into the generated config below.
_TOPIC_PARTITIONS = {
    TOPIC_STREAM: 1,
    TOPIC_LONGPOLL: 1,
    TOPIC_4P: 4,
    TOPIC_STRICT: 1,
    TOPIC_PREFILLED: 1,
    TOPIC_ORDER: 4,
    TOPIC_FRESH: 1,
    TOPIC_RETENTION: 1,
    TOPIC_DRAIN: 1,
    TOPIC_BYTES: 1,
    TOPIC_PROGRESS: 1,
}

# Topics bounded tighter than the default retention: their `retention` block.
_TOPIC_RETENTION = {
    TOPIC_RETENTION: {"duration": f"{RETENTION_DURATION_S}s"},
    TOPIC_DRAIN: {"duration": f"{RETENTION_DURATION_S}s"},
    TOPIC_BYTES: {"size_bytes": RETENTION_BYTES},
}


def group_seek_positions(entries) -> dict:
    """Group flat ``{topic, partition, value}`` seek entries into the SEEK body's
    topic-keyed ``{topic: [{partition, value}]}`` shape (the wire's ``positions``
    field)."""
    out: dict = {}
    for e in entries:
        out.setdefault(e["topic"], []).append(
            {"partition": e["partition"], "value": e["value"]}
        )
    return out


def _topic_instance(topic_id: str) -> dict:
    """A topic as `types-registry` holds it: an instance of the topic base."""
    return {"id": topic_id, "description": "a topic this E2E suite publishes to"}


def _derived_event_type(type_id: str, topic_id: str) -> dict:
    """An event type as `types-registry` holds it: a derived *type schema*.

    Its traits are what govern the type - the topic its events land on, the
    subject types they may declare, and the member of an event that decides its
    partition. `data` narrows to the base's own `["object", "null"]`: an empty
    schema would be *wider* than the base, and registration refuses a derived
    type that widens.
    """
    return {
        "$id": f"gts://{type_id}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "topic": topic_id,
            "allowed_subject_types": [SUBJECT_TYPE],
            "partition_key": "/tenant_id",
        },
        "type": "object",
        "allOf": [
            {"$ref": "gts://gts.cf.core.events.event.v1~"},
            {"type": "object", "properties": {"data": {"type": ["object", "null"]}}},
        ],
    }


def _strict_event_type(type_id: str, topic_id: str) -> dict:
    """An event type that requires ``data.strict_field`` (string).

    Used by schema-validation tests to trigger a 400 when the field is absent.
    The constraint is NARROWER than the base (which allows any object or null),
    so registration should accept it.
    """
    return {
        "$id": f"gts://{type_id}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "topic": topic_id,
            "allowed_subject_types": [SUBJECT_TYPE],
            "partition_key": "/tenant_id",
        },
        "type": "object",
        "allOf": [
            {"$ref": "gts://gts.cf.core.events.event.v1~"},
            {
                "type": "object",
                "properties": {
                    "data": {
                        "type": "object",
                        "required": ["strict_field"],
                        "properties": {"strict_field": {"type": "string"}},
                    }
                },
            },
        ],
    }


_EXTRA_ENTITIES = [
    _topic_instance(TOPIC_STREAM),
    _derived_event_type(EVENT_TYPE_STREAM, TOPIC_STREAM),
    _topic_instance(TOPIC_LONGPOLL),
    _derived_event_type(EVENT_TYPE_LONGPOLL, TOPIC_LONGPOLL),
    _topic_instance(TOPIC_4P),
    _derived_event_type(EVENT_TYPE_4P, TOPIC_4P),
    _topic_instance(TOPIC_STRICT),
    _strict_event_type(EVENT_TYPE_STRICT, TOPIC_STRICT),
    _topic_instance(TOPIC_PREFILLED),
    _derived_event_type(EVENT_TYPE_PREFILLED, TOPIC_PREFILLED),
    _topic_instance(TOPIC_ORDER),
    _derived_event_type(EVENT_TYPE_ORDER, TOPIC_ORDER),
    _topic_instance(TOPIC_FRESH),
    _derived_event_type(EVENT_TYPE_FRESH, TOPIC_FRESH),
    _topic_instance(TOPIC_RETENTION),
    _derived_event_type(EVENT_TYPE_RETENTION, TOPIC_RETENTION),
    _topic_instance(TOPIC_DRAIN),
    _derived_event_type(EVENT_TYPE_DRAIN, TOPIC_DRAIN),
    _topic_instance(TOPIC_BYTES),
    _derived_event_type(EVENT_TYPE_BYTES, TOPIC_BYTES),
    _topic_instance(TOPIC_PROGRESS),
    _derived_event_type(EVENT_TYPE_PROGRESS, TOPIC_PROGRESS),
]

_TEMP_HOME = tempfile.mkdtemp(prefix="cf-gears-e2e-event-broker-")

# ── Storage backend ───────────────────────────────────────────────────────
#
# The suite runs against any linked storage backend, chosen by `EB_E2E_BACKEND`:
# `memory` (the default, what the committed config names), `sqlite` (a file in
# the server's home directory), or a server in a sidecar container -
# `postgres`, `mysql`, `mariadb` (both served by the MySQL backend) or `kafka`.
# The make targets are `e2e-event-broker` (the whole suite) and
# `e2e-event-broker-<backend>` (the `backend_sensitive` tests only).
# Tests that need events to outlive the process run on every durable backend.

MEMORY_BACKEND = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"
POSTGRES_BACKEND = "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~"
SQLITE_BACKEND = "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~"
MYSQL_BACKEND = "gts.cf.core.events.backend.v1~cf.core.backend.mysql.v1~"
KAFKA_BACKEND = "gts.cf.core.events.backend.v1~cf.core.backend.kafka.v1~"

# The sidecar each server-backed choice runs, by `EB_E2E_BACKEND` value.
_SIDECARS = {
    "postgres": PostgresSidecar,
    "mysql": MySqlSidecar,
    "mariadb": MariaDbSidecar,
    "kafka": KafkaSidecar,
}
BACKENDS = ("memory", "sqlite", *_SIDECARS)
BACKEND = os.environ.get("EB_E2E_BACKEND", "").strip() or "memory"
if BACKEND not in BACKENDS:
    raise RuntimeError(f"EB_E2E_BACKEND must be one of {BACKENDS}, not {BACKEND!r}")
DURABLE = BACKEND != "memory"


def backend_sidecars() -> list:
    """A fresh sidecar for `BACKEND`, or none for an in-process medium.

    Named `eb-e2e-<backend>-<6 hex>`, so `docker ps` shows who started it.
    """
    sidecar = _SIDECARS.get(BACKEND)
    if sidecar is None:
        return []
    named = type(f"EventBroker{sidecar.__name__}", (sidecar,), {"NAME_PREFIX": f"eb-e2e-{BACKEND}"})
    return [named()]


def _backend_block(sidecars, namespace: str, home_dir: str) -> tuple[str, str]:
    """`(type, settings lines)` of the `backends.main` entry for `BACKEND`.

    `namespace` keeps one server's events apart from another's on a shared
    medium: the PostgreSQL schema, the MySQL database, the Kafka topic prefix,
    the SQLite file name in `home_dir`.
    """
    sidecars = list(sidecars.values() if isinstance(sidecars, dict) else sidecars)
    server = next((sc for sc in sidecars if sc.name == BACKEND), None)
    indent = "          "
    if BACKEND == "postgres":
        # Segments of 1000 events, so the retention test's batch fills one and
        # its expiry is observable; PostgreSQL expires whole sealed segments.
        return POSTGRES_BACKEND, (
            f'{indent}url: "{server.url()}"\n'
            f'{indent}schema: "{namespace}"\n'
            f"{indent}segment:\n"
            f"{indent}  events: {RETENTION_SEGMENT_EVENTS}\n"
            f"{indent}maintenance:\n"
            f"{indent}  rescan_interval_ms: 1000\n"
        )
    if BACKEND == "sqlite":
        path = Path(home_dir) / f"{namespace}.db"
        return SQLITE_BACKEND, f'{indent}path: "{path.as_posix()}"\n'
    if BACKEND in ("mysql", "mariadb"):
        return MYSQL_BACKEND, (
            f'{indent}url: "{server.url()}"\n'
            f'{indent}database: "{namespace}"\n'
            f"{indent}segment:\n"
            f"{indent}  events: {RETENTION_SEGMENT_EVENTS}\n"
            f"{indent}poll_interval_ms: 100\n"
            f"{indent}maintenance:\n"
            f"{indent}  rescan_interval_ms: 1000\n"
        )
    if BACKEND == "kafka":
        # One broker, so one replica.
        return KAFKA_BACKEND, (
            f'{indent}bootstrap_servers: "{server.bootstrap()}"\n'
            f'{indent}topic_prefix: "{namespace}"\n'
            f"{indent}replication_factor: 1\n"
            f"{indent}min_insync_replicas: 1\n"
            f"{indent}poll_interval_ms: 100\n"
        )
    raise AssertionError(f"no backends entry for {BACKEND!r}")


def patch_backend(config_text: str, sidecars, namespace: str, home_dir: str) -> str:
    """Point the committed config's one registry entry, `backends.main`, at `BACKEND`.

    `sidecars` is whatever the caller holds - `GearTestEnv.sidecars` (a list)
    or `RunningTestEnv.sidecars` (a dict); `namespace` and `home_dir` are the
    server's own (see `_backend_block`).
    """
    if BACKEND == "memory":
        return config_text
    entry = f'        main:\n          type: "{MEMORY_BACKEND}"\n'
    assert config_text.count(entry) == 1, (
        "the committed config no longer names the memory backend where the "
        "backend switch expects it"
    )
    backend_type, settings = _backend_block(sidecars, namespace, home_dir)
    return config_text.replace(
        entry, f'        main:\n          type: "{backend_type}"\n' + settings
    )


# ── Backend-sensitive tests ───────────────────────────────────────────────
#
# The whole suite runs once, on the default backend: it pins the exact wire
# formats. `backend_sensitive` marks the tests whose outcome can differ by
# storage backend - appends, reads, position resolution, retention, watch,
# durability - and the per-backend targets (`e2e-event-broker-<backend>`) run
# only those.

def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "backend_sensitive: outcome depends on the storage backend; "
        "run by every e2e-event-broker-<backend> target",
    )


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
    config_text = patch_backend(config_text, env.sidecars, "evbk_e2e", _TEMP_HOME)
    config_text = config_text.replace(
        'home_dir: "~/.cf-gears-event-broker"',
        f'home_dir: "{Path(_TEMP_HOME).as_posix()}"',
    )
    config_text = config_text.replace(
        'bind_addr: "127.0.0.1:8080"',
        f'bind_addr: "127.0.0.1:{SERVER_PORT}"',
    )
    # Each topic's partition count is configuration, not part of the entity.
    settings = "".join(
        f'        "{topic}":\n          partitions: {count}\n'
        + (
            "          retention:\n"
            + "".join(f"            {k}: {v}\n" for k, v in _TOPIC_RETENTION[topic].items())
            if topic in _TOPIC_RETENTION
            else ""
        )
        for topic, count in _TOPIC_PARTITIONS.items()
    )
    config_text = config_text.replace("      topics:\n", "      topics:\n" + settings, 1)
    # The block is read whole, so the other settings restate their defaults.
    streaming = (
        "      streaming:\n"
        "        heartbeat_interval_secs: 5\n"
        "        read_batch_max_events: 256\n"
        "        read_batch_max_bytes: 1048576\n"
        f"        progress_drift_threshold: {PROGRESS_DRIFT_THRESHOLD}\n"
        f"        progress_min_interval_secs: {PROGRESS_MIN_INTERVAL_S}\n"
    )
    anchor = "      backends:"
    assert config_text.count(anchor) == 1 and "      streaming:" not in config_text
    config_text = config_text.replace(anchor, streaming + anchor, 1)
    extra = "".join(f"        - {json.dumps(e)}\n" for e in _EXTRA_ENTITIES)
    return config_text.replace("entities:\n", "entities:\n" + extra)


@pytest.fixture(scope="session")
def gear_test_env() -> GearTestEnv:
    return GearTestEnv(
        binary="cf-gears-event-broker-server",
        config_path=CONFIG,
        config_patch=_patch_config,
        port=SERVER_PORT,
        health_path="/readyz",
        health_timeout=30,
        env={"RUST_LOG": os.environ.get("RUST_LOG", "info,event_broker=debug")},
        sidecars=backend_sidecars(),
        log_suffix="event-broker",
    )


# How long a fresh server may take to provision its topics on the backend.
PROVISION_TIMEOUT = 60.0


def wait_provisioned(base_url: str, event_types: dict[str, str]) -> None:
    """Block until every topic of `event_types` (topic -> an event type of
    it) is provisioned on the storage backend.

    The server is already ready (`/readyz`), but provisioning goes on past the
    start phase: a backend may answer `Pending` first, and a remote one takes
    a round trip per topic, so a test could otherwise get a 503 publishing.
    The probe has no side effect: a publish naming an unregistered producer is
    refused 404 only after the topic's provisioning check, which answers 503
    before.
    """
    import time

    deadline = time.monotonic() + PROVISION_TIMEOUT

    def settle(probe, what: str) -> None:
        while True:
            resp = probe()
            if resp.status_code == 404:
                return
            assert resp.status_code == 503, resp.text
            assert time.monotonic() < deadline, (
                f"{what} not ready within {PROVISION_TIMEOUT}s: {resp.text}"
            )
            time.sleep(0.2)

    with httpx.Client(base_url=base_url, timeout=REQUEST_TIMEOUT) as client:
        for topic, event_type in event_types.items():
            settle(
                lambda: client.post(
                    "/events",
                    json={
                        "id": str(uuid.uuid4()),
                        "type": event_type,
                        "tenant_id": str(uuid.uuid4()),
                        "source": "e2e-probe",
                        "subject": "probe",
                        "subject_type": SUBJECT_TYPE,
                        "occurred_at": datetime.now(timezone.utc).isoformat(),
                        "data": {"strict_field": "probe"},
                        "meta": {"version": 1, "producer_id": str(uuid.uuid4()), "previous": 0, "sequence": 1},
                    },
                ),
                f"topic {topic}",
            )


# Every suite topic, with one event type of it for the provisioning probe.
_TOPIC_EVENT_TYPES = {
    TOPIC_STREAM: EVENT_TYPE_STREAM,
    TOPIC_LONGPOLL: EVENT_TYPE_LONGPOLL,
    TOPIC_4P: EVENT_TYPE_4P,
    TOPIC_STRICT: EVENT_TYPE_STRICT,
    TOPIC_PREFILLED: EVENT_TYPE_PREFILLED,
    TOPIC_ORDER: EVENT_TYPE_ORDER,
    TOPIC_FRESH: EVENT_TYPE_FRESH,
    TOPIC_RETENTION: EVENT_TYPE_RETENTION,
    TOPIC_DRAIN: EVENT_TYPE_DRAIN,
    TOPIC_BYTES: EVENT_TYPE_BYTES,
    TOPIC_PROGRESS: EVENT_TYPE_PROGRESS,
}


@pytest.fixture(scope="session", autouse=True)
def _topics_provisioned(_require_dedicated_binary, test_env):
    wait_provisioned(f"{test_env.base_url}{API_BASE}", _TOPIC_EVENT_TYPES)


# ── HTTP helpers ──────────────────────────────────────────────────────────

@pytest.fixture
def api(test_env):
    """Async client factory bound to the running server's event-broker API."""

    def _client() -> httpx.AsyncClient:
        return httpx.AsyncClient(
            base_url=f"{test_env.base_url}{API_BASE}",
            timeout=REQUEST_TIMEOUT,
            headers={"traceparent": TRACEPARENT},
        )

    return _client


@dataclass(frozen=True)
class Prefilled:
    """TOPIC_PREFILLED as the session seeded it.

    `events` are the delivered payloads in sequence order, one per published
    event; `head` is the last one's sequence, which "latest" resolves to.
    Sequences rise but may have gaps on some backends, so a test reads a
    position from here rather than computing it from PREFILLED_COUNT.
    """

    events: list[dict]

    @property
    def head(self) -> int:
        return self.events[-1]["sequence"]


@pytest.fixture(scope="session")
async def prefilled_topic(test_env) -> Prefilled:
    """Seed TOPIC_PREFILLED with PREFILLED_COUNT events at session startup.

    No other test publishes to TOPIC_PREFILLED, and the medium is fresh each
    session, so the floor is 0 and the head is the last seeded event's
    sequence.
    """
    base_url = f"{test_env.base_url}{API_BASE}"
    now = datetime.now(timezone.utc).isoformat()
    published = [
        {
            "id": str(uuid.uuid4()),
            "type": EVENT_TYPE_PREFILLED,
            "tenant_id": PREFILLED_TENANT_ID,
            "source": "e2e-prefill",
            "subject": "prefill-subject",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": now,
        }
        for _ in range(PREFILLED_COUNT)
    ]
    async with httpx.AsyncClient(base_url=base_url, timeout=REQUEST_TIMEOUT) as client:
        pub_resp = await client.post("/events:batch", json={"events": published})
        assert pub_resp.status_code == 202

        group = await client.post("/consumer-groups")
        assert group.status_code == 201
        sub = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group.json()["id"],
                "client_agent": "e2e-prefill",
                "interests": [
                    {
                        "topic": TOPIC_PREFILLED,
                        "tenant_id": PREFILLED_TENANT_ID,
                        "types": [EVENT_TYPE_PREFILLED],
                    }
                ],
            },
        )
        assert sub.status_code == 201
        sub_id = sub.json()["id"]
        seek = await client.post(
            f"/subscriptions/{sub_id}:seek",
            json={"topology_version": 1, "positions": {TOPIC_PREFILLED: [{"partition": 0, "value": "earliest"}]}},
        )
        assert seek.status_code == 200

    # `POST /events:batch` acks with 202 before the events are sequenced, so
    # the fixture reads the whole topic back: that both waits for every event
    # to land and records the sequence each one got. Temporary: a synchronous
    # publish that acks only once durable removes the wait, but is not
    # implemented yet.
    delivered: list[dict] = []
    async with httpx.AsyncClient(base_url=base_url, timeout=None) as stream_client:
        async with stream_client.stream("GET", f"/events:sse?subscription_id={sub_id}") as resp:
            assert resp.status_code == 200
            reader = SseFrameReader(resp)
            await reader.await_kind("topology", timeout=5)
            deadline = asyncio.get_running_loop().time() + PREFILL_INGEST_TIMEOUT
            while len(delivered) < PREFILLED_COUNT:
                remaining = deadline - asyncio.get_running_loop().time()
                assert remaining > 0, (
                    f"only {len(delivered)} of {PREFILLED_COUNT} prefill events were "
                    f"delivered within {PREFILL_INGEST_TIMEOUT}s"
                )
                delivered.append((await reader.await_kind("event", timeout=remaining))["payload"])
    async with httpx.AsyncClient(base_url=base_url, timeout=REQUEST_TIMEOUT) as client:
        assert (await client.delete(f"/subscriptions/{sub_id}")).status_code == 204

    assert [e["id"] for e in delivered] == [e["id"] for e in published]
    sequences = [e["sequence"] for e in delivered]
    assert sequences == sorted(set(sequences)) and sequences[0] >= 1, sequences
    return Prefilled(events=delivered)


class FrameReader:
    """Shared frame-waiting logic for the two streaming readers below.

    A subclass supplies ``next_frame`` plus the ``_kind``/``_body`` accessors
    that say where a frame's kind and its assertable body sit in whatever shape
    that reader returns - the two transports differ there, the waiting does not.
    """

    async def await_kind(self, kind: str, timeout: float = 15.0):
        """Return the body of the next ``kind`` frame, skipping heartbeats.

        A single budget spans the whole wait. Heartbeats arrive on a few-second
        cadence, so passing ``timeout`` to each read separately would let every
        heartbeat re-arm it and a stream that never delivers ``kind`` would hang
        until the test-level timeout instead of failing here.
        """
        deadline = asyncio.get_running_loop().time() + timeout
        while True:
            remaining = deadline - asyncio.get_running_loop().time()
            assert remaining > 0, f"no {kind} frame delivered, only heartbeats"
            frame = await self.next_frame(timeout=remaining)
            if self._kind(frame) == kind:
                return self._body(frame)
            assert self._kind(frame) == "heartbeat", frame


class MultipartReader(FrameReader):
    """Reads JSON frames from a ``multipart/mixed`` streaming response.

    Each part carries exactly one JSON object, so a frame is emitted as soon as
    its body decodes - NOT when the boundary that opens the next part arrives.
    A streaming server writes that boundary only once it has another frame to
    send, so keying on it would withhold every frame until the following one
    started arriving, leaving the reader a full frame behind the stream.

    Boundaries and part headers carry no ``{``, so scanning for the next JSON
    object is enough to skip them.
    """

    def __init__(self, response: httpx.Response):
        self._iter = response.aiter_bytes()
        self._buf = b""
        self._decoder = json.JSONDecoder()

    # A multipart frame carries `kind` inline, so the frame *is* the body.
    @staticmethod
    def _kind(frame: dict) -> str:
        return frame.get("kind")

    @staticmethod
    def _body(frame: dict) -> dict:
        return frame

    async def next_frame(self, timeout: float = 5.0) -> dict:
        async def _read():
            while True:
                start = self._buf.find(b"{")
                if start != -1:
                    try:
                        text = self._buf[start:].decode("utf-8")
                        frame, end = self._decoder.raw_decode(text)
                    except (UnicodeDecodeError, ValueError):
                        # Body is still partial - fall through and read more.
                        pass
                    else:
                        # `end` indexes characters; re-encode to advance the
                        # byte buffer correctly past any non-ASCII payload.
                        self._buf = self._buf[start + len(text[:end].encode("utf-8")):]
                        return frame
                chunk = await self._iter.__anext__()
                self._buf += chunk

        return await asyncio.wait_for(_read(), timeout=timeout)


class SseFrameReader(FrameReader):
    """Reads SSE frames (`event: <kind>\\ndata: <json>\\n\\n`) one at a time
    off a streaming `httpx.Response`, buffering across chunks - a real
    socket gives no guarantee that one frame arrives as exactly one
    `aiter_bytes()` read, or that two frames don't arrive in the same one.
    """

    def __init__(self, response: httpx.Response):
        self._iter = response.aiter_bytes()
        self._buf = ""

    # SSE splits the kind onto its own `event:` line, so a frame is a pair.
    @staticmethod
    def _kind(frame: tuple[str, dict]) -> str:
        return frame[0]

    @staticmethod
    def _body(frame: tuple[str, dict]) -> dict:
        return frame[1]

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
