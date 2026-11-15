"""E2E fixtures for the event-broker gear (design.md D8, task group 11).

event-broker has its OWN standalone binary (`cf-gears-event-broker-server`),
not a feature flag on `cf-gears-example-server` - gated on `E2E_BINARY`
exactly like mini-chat/usage-collector, so running the shared `make
e2e-local` suite never boots a second server for routes it doesn't serve.

Run it with: make e2e-event-broker

`types-registry` seeds the config's `entities:` into its database catalogue
at boot, and the broker reads that catalogue at start and then once per
`workers.specification_refresh_interval_secs` (see
`config/event-broker-standalone.yaml`'s header comment). A topic admitted at
runtime through `POST /types-registry/v2/platform/entities` (internal token)
resolves at the first refresh after its admission, while
`POST /types-registry/v1/entities` writes only the legacy catalogue, which
the broker does not read. So every test that shares the session-scoped
`test_env` server gets its own PRE-PROVISIONED topic baked into the config at
session-setup time (below), rather than waiting out a refresh per test.
"""

from __future__ import annotations

import asyncio
import json
import os
import tempfile
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from lib.orchestrator import GearTestEnv, _log_path
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
# A second subject type, admitted only by the subject-selector topic's event type.
SUBJECT_TYPE_OTHER = "gts.cf.e2e.event_broker.subject_other.v1~"

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
# Its age bound, TOPIC_RETENTION_AGE_S, is longer than the other age-bounded
# topics' RETENTION_DURATION_S: its test reads one event after the old ones
# aged out, and that read needs room before the event ages out too.
TOPIC_RETENTION = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.retention.v1"
EVENT_TYPE_RETENTION = "gts.cf.core.events.event.v1~cf.e2e.event_broker.retention.v1~"
RETENTION_DURATION_S = 8
TOPIC_RETENTION_AGE_S = 12
RETENTION_SEGMENT_EVENTS = 1000

# Drained topic: RETENTION_DURATION_S, for the test that lets every stored
# event age out and checks "latest" still names the last one.
TOPIC_DRAIN = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.drain.v1"
EVENT_TYPE_DRAIN = "gts.cf.core.events.event.v1~cf.e2e.event_broker.drain.v1~"

# Slow-publish topic: RETENTION_DURATION_S, for the test that publishes one
# batch for longer than that and reads it back whole. A topic of its own: a
# stream seeked to "earliest" on a topic holding other tests' events is sent
# `control:progress` frames for them.
TOPIC_SLOW_PUBLISH = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.slow_publish.v1"
EVENT_TYPE_SLOW_PUBLISH = "gts.cf.core.events.event.v1~cf.e2e.event_broker.slow_publish.v1~"

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

# Keyed topic: its event type partitions on `/data/k`, a member a publisher sets,
# so a test can send a partition key over the broker's 1024-byte cap.
TOPIC_KEYED = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.keyed.v1"
EVENT_TYPE_KEYED = "gts.cf.core.events.event.v1~cf.e2e.event_broker.keyed.v1~"
KEYED_POINTER = "/data/k"

# Subject-selector topic: one partition, and an event type admitting both
# SUBJECT_TYPE and SUBJECT_TYPE_OTHER, so a test publishes one event of each in
# order and checks that a subject-type selector delivers only its own.
TOPIC_SUBJECT_SELECTOR = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.subject_selector.v1"
EVENT_TYPE_SUBJECT_SELECTOR = "gts.cf.core.events.event.v1~cf.e2e.event_broker.subject_selector.v1~"

# Selector-matching topics (`test_selector_matching.py`): one partition each, and
# one per type family, so a test publishes excluded events and then an admitted
# sentinel on the same partition, and no family sees another's types.
#
# Exact selector: a type and a sibling of it.
TOPIC_SEL_EXACT = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.sel_exact.v1"
EVENT_TYPE_SEL_EXACT = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_exact.v1~"
EVENT_TYPE_SEL_EXACT_SIBLING = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_exact_sibling.v1~"
# A type family: a base, two types derived from it, one derived from the first of
# those (two levels below the base), and a type of another family.
TOPIC_SEL_FAMILY = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.sel_family.v1"
EVENT_TYPE_SEL_BASE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_base.v1~"
EVENT_TYPE_SEL_EXPRESS = EVENT_TYPE_SEL_BASE + "cf.e2e.event_broker.sel_express.v1~"
EVENT_TYPE_SEL_RUSH = EVENT_TYPE_SEL_BASE + "cf.e2e.event_broker.sel_rush.v1~"
EVENT_TYPE_SEL_PRIORITY = EVENT_TYPE_SEL_EXPRESS + "cf.e2e.event_broker.sel_priority.v1~"
EVENT_TYPE_SEL_OTHER = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_other.v1~"
# Prefix wildcard: two types under the `cf.e2e.sel_prefix` namespace and an
# outsider on the same topic. The suite's own namespace would cover the outsider
# too, so the types under the prefix use a namespace of their own.
TOPIC_SEL_PREFIX = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.sel_prefix.v1"
EVENT_TYPE_SEL_PREFIX_PATTERN = "gts.cf.core.events.event.v1~cf.e2e.sel_prefix.*"
EVENT_TYPE_SEL_PREFIX_CREATED = "gts.cf.core.events.event.v1~cf.e2e.sel_prefix.created.v1~"
EVENT_TYPE_SEL_PREFIX_PLACED = "gts.cf.core.events.event.v1~cf.e2e.sel_prefix.placed.v1~"
EVENT_TYPE_SEL_PREFIX_OUTSIDER = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_prefix_outsider.v1~"
# No match: a wildcard no type of the topic falls under, and a sentinel type a
# second interest names.
TOPIC_SEL_NOMATCH = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.sel_nomatch.v1"
EVENT_TYPE_SEL_NOMATCH_PATTERN = "gts.cf.core.events.event.v1~cf.e2e.sel_nothing.*"
EVENT_TYPE_SEL_NOMATCH = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_nomatch.v1~"
EVENT_TYPE_SEL_NOMATCH_SENTINEL = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_nomatch_sentinel.v1~"
# Late types: the base is seeded; the types derived from it are admitted by the
# tests while the server runs, after their JOIN.
TOPIC_SEL_LATE = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.sel_late.v1"
EVENT_TYPE_SEL_LATE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_late.v1~"
EVENT_TYPE_SEL_LATE_ALLOWED = EVENT_TYPE_SEL_LATE + "cf.e2e.event_broker.sel_late_allowed.v1~"
EVENT_TYPE_SEL_LATE_DENIED = EVENT_TYPE_SEL_LATE + "cf.e2e.event_broker.sel_late_denied.v1~"
# Subject-type selectors: an event type allowing a base subject type (and so its
# derived subject types) and an outsider, and a sentinel type a second interest
# names. One derived subject type sits under the `cf` vendor, the other under
# another vendor; subject types need no registration, publish checks only the
# allowed patterns.
TOPIC_SEL_SUBJECT = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.sel_subject.v1"
EVENT_TYPE_SEL_SUBJECT = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_subject.v1~"
EVENT_TYPE_SEL_SUBJECT_SENTINEL = "gts.cf.core.events.event.v1~cf.e2e.event_broker.sel_subject_sentinel.v1~"
SUBJECT_TYPE_SEL_CUSTOMER = "gts.cf.e2e.event_broker.sel_customer.v1~"
SUBJECT_TYPE_SEL_MEMBER = SUBJECT_TYPE_SEL_CUSTOMER + "cf.e2e.event_broker.sel_member.v1~"
SUBJECT_TYPE_SEL_PARTNER = SUBJECT_TYPE_SEL_CUSTOMER + "fabrikam.e2e.event_broker.sel_member.v1~"
SUBJECT_TYPE_SEL_CUSTOMER_PREFIX = SUBJECT_TYPE_SEL_CUSTOMER + "cf.*"
SUBJECT_TYPE_SEL_SUPPLIER = "gts.cf.e2e.event_broker.sel_supplier.v1~"
SUBJECT_TYPE_SEL_AUDITOR = "gts.cf.e2e.event_broker.sel_auditor.v1~"

# Ownership-filter topics: one partition each, written only by their own test,
# so a stream read from "earliest" holds exactly the events that test published.
# TOPIC_OWNERSHIP carries events of the default tenant with and without an owner;
# TOPIC_SUBTREE carries events of tenants other than the caller's.
TOPIC_OWNERSHIP = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.ownership.v1"
EVENT_TYPE_OWNERSHIP = "gts.cf.core.events.event.v1~cf.e2e.event_broker.ownership.v1~"
TOPIC_SUBTREE = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.subtree.v1"
EVENT_TYPE_SUBTREE = "gts.cf.core.events.event.v1~cf.e2e.event_broker.subtree.v1~"

# The identity the api-gateway injects under `auth_disabled: true`
# (toolkit_security::constants, libs/toolkit-security/src/constants.rs).
DEFAULT_TENANT_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
DEFAULT_SUBJECT_ID = "11111111-6a88-4768-9dfc-6bcd5187d9ed"

# Shared xfail marker for tests that require real auth enforcement.
# The standalone config sets ``auth_disabled: true``; these tests document the
# expected behaviour and will pass once a multi-tenant config is wired.
XFAIL_AUTH_DISABLED = pytest.mark.xfail(
    reason="standalone config sets auth_disabled: true; tenant isolation is not enforced",
    strict=True,
)

# For tests whose scenario denies one event type to the caller: the suite's
# policy allows every type, so the event is delivered and the test fails.
XFAIL_NO_ACTION_AWARE_PDP = pytest.mark.xfail(
    reason="the suite runs no action-aware PDP; denying one event type needs one",
    strict=True,
)

# Static-token identities for a static-tokens config of this suite. Each token
# is bound in that config's `static-authn-plugin` token list:
# Principal A: a fixed non-nil subject in the shared e2e tenant.
TOKEN_PRINCIPAL_A = "eb-e2e-principal-a"
# Principal B: a second fixed non-nil subject in the same tenant as A.
TOKEN_PRINCIPAL_B = "eb-e2e-principal-b"
# The nil-subject identity: subject id is the nil UUID, tenant the shared one.
TOKEN_NIL_SUBJECT = "eb-e2e-nil-subject"
# The nil-tenant identity: a fixed non-nil subject whose tenant id is the nil
# UUID, which the shipped policies refuse.
TOKEN_NIL_TENANT = "eb-e2e-nil-tenant"


def _api_gateway_auth_disabled(config_text: str) -> bool:
    """Whether the config's ``api-gateway`` module sets ``auth_disabled: true``.

    Reads the module block (the ``  api-gateway:`` line and every line indented
    deeper below it); the session config patch (``_patch_config``) never
    touches that block, so the file decides it.
    """
    lines = config_text.splitlines()
    try:
        start = lines.index("  api-gateway:")
    except ValueError:
        return False
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and len(line) - len(line.lstrip()) <= 2:
            break
        if stripped.split("#", 1)[0].strip() == "auth_disabled: true":
            return True
    return False


# Read once at import from CONFIG, the config `gear_test_env` boots: its
# ``api-gateway.config.auth_disabled`` (config/event-broker-standalone.yaml:218).
API_GATEWAY_AUTH_DISABLED = _api_gateway_auth_disabled(CONFIG.read_text(encoding="utf-8"))

# For tests that send a static token: under an auth-disabled config the gateway
# ignores the token and injects its own default identity, so the test is a
# strict xfail; under any other config it runs unmarked.
XFAIL_UNLESS_STATIC_TOKENS = (
    pytest.mark.xfail(
        reason="config sets api-gateway auth_disabled: true; static tokens are not read",
        strict=True,
    )
    if API_GATEWAY_AUTH_DISABLED
    else (lambda test: test)
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
    TOPIC_SLOW_PUBLISH: 1,
    TOPIC_BYTES: 1,
    TOPIC_PROGRESS: 1,
    TOPIC_KEYED: 1,
    TOPIC_SUBJECT_SELECTOR: 1,
    TOPIC_SEL_EXACT: 1,
    TOPIC_SEL_FAMILY: 1,
    TOPIC_SEL_PREFIX: 1,
    TOPIC_SEL_NOMATCH: 1,
    TOPIC_SEL_LATE: 1,
    TOPIC_SEL_SUBJECT: 1,
    TOPIC_OWNERSHIP: 1,
    TOPIC_SUBTREE: 1,
}

# Topics bounded tighter than the default retention: their `retention` block.
_TOPIC_RETENTION = {
    TOPIC_RETENTION: {"duration": f"{TOPIC_RETENTION_AGE_S}s"},
    TOPIC_DRAIN: {"duration": f"{RETENTION_DURATION_S}s"},
    TOPIC_SLOW_PUBLISH: {"duration": f"{RETENTION_DURATION_S}s"},
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


def _await_log_line(env: GearTestEnv, text: str, offset: int = 0) -> None:
    """Waits for `text` in the server log written after byte `offset`: a 503
    body names no cause, so the log line is what tells which one fired."""
    deadline = time.monotonic() + 10
    while True:
        with open(_log_path(env), encoding="utf-8", errors="replace") as fh:
            fh.seek(offset)
            log = fh.read()
        if text in log:
            return
        assert time.monotonic() < deadline, f"{text!r} not in the server log: {log[-2000:]}"
        time.sleep(0.2)


def _topic_instance(topic_id: str) -> dict:
    """A topic as `types-registry` holds it: an instance of the topic base."""
    return {"id": topic_id, "description": "a topic this E2E suite publishes to"}


def _derived_event_type(
    type_id: str,
    topic_id: str,
    subject_types: list[str] | None = None,
    parent: str = "gts.cf.core.events.event.v1~",
) -> dict:
    """An event type as `types-registry` holds it: a derived *type schema*.

    Its traits are what govern the type - the topic its events land on, the
    subject types they may declare, and the member of an event that decides its
    partition. `subject_types` lists the allowed subject types; `None` means
    `[SUBJECT_TYPE]`. `data` narrows to the base's own `["object", "null"]`: an
    empty schema would be *wider* than the base, and registration refuses a
    derived type that widens. `parent` is the type it derives from, the event
    base unless it sits deeper in a chain; its traits are set in full all the
    same.
    """
    if subject_types is None:
        subject_types = [SUBJECT_TYPE]
    return {
        "$id": f"gts://{type_id}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "topic": topic_id,
            "allowed_subject_types": subject_types,
            "partition_key": "/subject_tenant",
        },
        "type": "object",
        "allOf": [
            {"$ref": f"gts://{parent}"},
            {"type": "object", "properties": {"data": {"type": ["object", "null"]}}},
        ],
    }


def _keyed_event_type(type_id: str, topic_id: str) -> dict:
    """An event type partitioned on ``data.k`` (``KEYED_POINTER``).

    Registration admits a pointer only to a member the schema declares, so its
    ``data`` declares ``k`` as a string; that is narrower than the base.
    """
    return {
        "$id": f"gts://{type_id}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "topic": topic_id,
            "allowed_subject_types": [SUBJECT_TYPE],
            "partition_key": KEYED_POINTER,
        },
        "type": "object",
        "allOf": [
            {"$ref": "gts://gts.cf.core.events.event.v1~"},
            {
                "type": "object",
                "properties": {
                    "data": {
                        "type": ["object", "null"],
                        "properties": {"k": {"type": "string"}},
                    }
                },
            },
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
            "partition_key": "/subject_tenant",
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
    _topic_instance(TOPIC_SLOW_PUBLISH),
    _derived_event_type(EVENT_TYPE_SLOW_PUBLISH, TOPIC_SLOW_PUBLISH),
    _topic_instance(TOPIC_BYTES),
    _derived_event_type(EVENT_TYPE_BYTES, TOPIC_BYTES),
    _topic_instance(TOPIC_PROGRESS),
    _derived_event_type(EVENT_TYPE_PROGRESS, TOPIC_PROGRESS),
    _topic_instance(TOPIC_KEYED),
    _keyed_event_type(EVENT_TYPE_KEYED, TOPIC_KEYED),
    _topic_instance(TOPIC_SUBJECT_SELECTOR),
    _derived_event_type(
        EVENT_TYPE_SUBJECT_SELECTOR,
        TOPIC_SUBJECT_SELECTOR,
        subject_types=[SUBJECT_TYPE, SUBJECT_TYPE_OTHER],
    ),
    _topic_instance(TOPIC_SEL_EXACT),
    _derived_event_type(EVENT_TYPE_SEL_EXACT, TOPIC_SEL_EXACT),
    _derived_event_type(EVENT_TYPE_SEL_EXACT_SIBLING, TOPIC_SEL_EXACT),
    _topic_instance(TOPIC_SEL_FAMILY),
    _derived_event_type(EVENT_TYPE_SEL_BASE, TOPIC_SEL_FAMILY),
    _derived_event_type(EVENT_TYPE_SEL_EXPRESS, TOPIC_SEL_FAMILY, parent=EVENT_TYPE_SEL_BASE),
    _derived_event_type(EVENT_TYPE_SEL_RUSH, TOPIC_SEL_FAMILY, parent=EVENT_TYPE_SEL_BASE),
    _derived_event_type(EVENT_TYPE_SEL_PRIORITY, TOPIC_SEL_FAMILY, parent=EVENT_TYPE_SEL_EXPRESS),
    _derived_event_type(EVENT_TYPE_SEL_OTHER, TOPIC_SEL_FAMILY),
    _topic_instance(TOPIC_SEL_PREFIX),
    _derived_event_type(EVENT_TYPE_SEL_PREFIX_CREATED, TOPIC_SEL_PREFIX),
    _derived_event_type(EVENT_TYPE_SEL_PREFIX_PLACED, TOPIC_SEL_PREFIX),
    _derived_event_type(EVENT_TYPE_SEL_PREFIX_OUTSIDER, TOPIC_SEL_PREFIX),
    _topic_instance(TOPIC_SEL_NOMATCH),
    _derived_event_type(EVENT_TYPE_SEL_NOMATCH, TOPIC_SEL_NOMATCH),
    _derived_event_type(EVENT_TYPE_SEL_NOMATCH_SENTINEL, TOPIC_SEL_NOMATCH),
    _topic_instance(TOPIC_SEL_LATE),
    _derived_event_type(EVENT_TYPE_SEL_LATE, TOPIC_SEL_LATE),
    _topic_instance(TOPIC_SEL_SUBJECT),
    _derived_event_type(
        EVENT_TYPE_SEL_SUBJECT,
        TOPIC_SEL_SUBJECT,
        subject_types=[SUBJECT_TYPE_SEL_CUSTOMER, SUBJECT_TYPE_SEL_SUPPLIER],
    ),
    _derived_event_type(EVENT_TYPE_SEL_SUBJECT_SENTINEL, TOPIC_SEL_SUBJECT),
    _topic_instance(TOPIC_OWNERSHIP),
    _derived_event_type(EVENT_TYPE_OWNERSHIP, TOPIC_OWNERSHIP),
    _topic_instance(TOPIC_SUBTREE),
    _derived_event_type(EVENT_TYPE_SUBTREE, TOPIC_SUBTREE),
]

_TEMP_HOME = tempfile.mkdtemp(prefix="cf-gears-e2e-event-broker-")

# ── Storage backend ───────────────────────────────────────────────────────
#
# The suite runs against any linked storage backend, chosen by `EB_E2E_BACKEND`:
# `memory` (the default), `sqlite` (a file in the server's home directory), or a
# server in a sidecar container - `postgres`, `mysql`, `mariadb` (both served by
# the MySQL backend) or `kafka`.
# The make targets are `e2e-event-broker` (the whole suite) and
# `e2e-event-broker-<backend>` (the `backend_sensitive` tests only).
# Tests that need events to outlive the process run on every durable backend.
# The committed config names sqlite with a file in the real home, so every
# server the suite starts has that entry replaced (`patch_backend`), memory
# included.

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
    if BACKEND == "memory":
        return MEMORY_BACKEND, ""
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


# The committed config's one registry entry, `backends.main`: a file in the
# real home, which no test server may open.
_COMMITTED_ENTRY = (
    "        main:\n"
    f'          type: "{SQLITE_BACKEND}"\n'
    '          path: "${HOME}/.cf-gears-event-broker/event-broker/events.db"\n'
)


def patch_backend(config_text: str, sidecars, namespace: str, home_dir: str) -> str:
    """Point the committed config's one registry entry, `backends.main`, at `BACKEND`.

    Always replaces the entry, memory included. `sidecars` is whatever the
    caller holds - `GearTestEnv.sidecars` (a list) or `RunningTestEnv.sidecars`
    (a dict); `namespace` and `home_dir` are the server's own (see
    `_backend_block`).
    """
    assert config_text.count(_COMMITTED_ENTRY) == 1, (
        "the committed config no longer names the sqlite backend entry the "
        "backend switch replaces"
    )
    backend_type, settings = _backend_block(sidecars, namespace, home_dir)
    return config_text.replace(
        _COMMITTED_ENTRY, f'        main:\n          type: "{backend_type}"\n' + settings
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
            "E2E_BINARY not set - run these tests via: make e2e-event-broker",
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
                        "subject_tenant": str(uuid.uuid4()),
                        "source": "e2e-probe",
                        "subject": "probe",
                        "subject_type": SUBJECT_TYPE,
                        "occurred_at": datetime.now(timezone.utc).isoformat(),
                        "data": {"strict_field": "probe"},
                        "meta": {"version": 1, "producer": str(uuid.uuid4()), "previous": 0, "sequence": 1},
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
    TOPIC_SLOW_PUBLISH: EVENT_TYPE_SLOW_PUBLISH,
    TOPIC_BYTES: EVENT_TYPE_BYTES,
    TOPIC_PROGRESS: EVENT_TYPE_PROGRESS,
    TOPIC_KEYED: EVENT_TYPE_KEYED,
    TOPIC_SUBJECT_SELECTOR: EVENT_TYPE_SUBJECT_SELECTOR,
    TOPIC_SEL_EXACT: EVENT_TYPE_SEL_EXACT,
    TOPIC_SEL_FAMILY: EVENT_TYPE_SEL_BASE,
    TOPIC_SEL_PREFIX: EVENT_TYPE_SEL_PREFIX_CREATED,
    TOPIC_SEL_NOMATCH: EVENT_TYPE_SEL_NOMATCH,
    TOPIC_SEL_LATE: EVENT_TYPE_SEL_LATE,
    TOPIC_SEL_SUBJECT: EVENT_TYPE_SEL_SUBJECT_SENTINEL,
    TOPIC_OWNERSHIP: EVENT_TYPE_OWNERSHIP,
    TOPIC_SUBTREE: EVENT_TYPE_SUBTREE,
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
            "subject_tenant": PREFILLED_TENANT_ID,
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
                        "subject_tenant": PREFILLED_TENANT_ID,
                        "ownership_filter": "entire_tenant",
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
        async with stream_client.stream("GET", f"/events:sse?subscription={sub_id}") as resp:
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
