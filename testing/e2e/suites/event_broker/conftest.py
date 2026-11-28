"""Event Broker fixtures for the worker and its separate Flight Control host."""

from __future__ import annotations

import asyncio
import json
import os
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from lib.sidecars import KafkaSidecar, MariaDbSidecar, MySqlSidecar, PostgresSidecar

from .topology import (
    OWNER_SUBJECT,
    OWNER_TENANT,
    OWNER_TOKEN,
    FlightControlBrokerEnv,
    own_sidecars,
    replace_once,
)

# ── Constants ─────────────────────────────────────────────────────────────

HERE = Path(__file__).resolve().parent
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
PREFILLED_TENANT_ID = OWNER_TENANT
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

# Shared names for scenarios that reference the session actor's identity.
DEFAULT_TENANT_ID = OWNER_TENANT
DEFAULT_SUBJECT_ID = OWNER_SUBJECT

# Shared markers for scenarios carried forward from the co-hosted fixture.
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

# Flight Control maps these bearers through the shipped static AuthN plugin.
# Principal A: a fixed non-nil subject in the shared e2e tenant.
TOKEN_PRINCIPAL_A = OWNER_TOKEN
# Principal B: a second fixed non-nil subject in the same tenant as A.
TOKEN_PRINCIPAL_B = "eb-e2e-principal-b"
TOKEN_BARRIER = "eb-e2e-barrier"
# The nil-subject identity: subject id is the nil UUID, tenant the shared one.
TOKEN_NIL_SUBJECT = "eb-e2e-nil-subject"
# The nil-tenant identity: a fixed non-nil subject whose tenant id is the nil
# UUID, which the shipped policies refuse.
TOKEN_NIL_TENANT = "eb-e2e-nil-tenant"


API_GATEWAY_AUTH_DISABLED = False

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


def _await_log_line(env: FlightControlBrokerEnv, text: str) -> None:
    deadline = time.monotonic() + 10
    while True:
        log = env.log()
        if text in log:
            return
        remaining = deadline - time.monotonic()
        assert remaining > 0, f"{text!r} not in the worker log: {log[-2000:]}"
        time.sleep(min(0.2, remaining))


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


def _backend_block(sidecars: dict, namespace: str, home_dir: Path) -> tuple[str, str]:
    """`(type, settings lines)` of the `backends.main` entry for `BACKEND`.

    `namespace` keeps one server's events apart from another's on a shared
    medium: the PostgreSQL schema, the MySQL database, the Kafka topic prefix,
    the SQLite file name in `home_dir`.
    """
    server = sidecars.get(BACKEND)
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


def patch_backend(config_text: str, sidecars: dict, namespace: str, home_dir: Path) -> str:
    """Point the worker's main entry at its own storage namespace."""
    backend_type, settings = _backend_block(sidecars, namespace, home_dir)
    return replace_once(
        config_text, _COMMITTED_ENTRY, f'        main:\n          type: "{backend_type}"\n'
        + settings,
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

def _patch_config(config_text: str, home_dir: Path, sidecars: dict, namespace: str) -> str:
    config_text = patch_backend(config_text, sidecars, namespace, home_dir)
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
    config_text = replace_once(config_text, "      topics:\n", "      topics:\n" + settings)
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
    return replace_once(config_text, anchor, streaming + anchor)


@pytest.fixture(scope="session")
def test_env(tmp_path_factory):
    root = tmp_path_factory.mktemp("event-broker-session")
    namespace = f"evbk_e2e_{uuid.uuid4().hex[:8]}"
    pair = FlightControlBrokerEnv(
        root,
        broker_config=lambda text, home, sidecars: _patch_config(
            text, home, sidecars, namespace,
        ),
        documents=_EXTRA_ENTITIES,
    )
    with own_sidecars(backend_sidecars(), redact=pair.redact) as sidecars:
        pair.sidecars = sidecars
        with pair:
            yield pair


@pytest.fixture(scope="session")
def _scoped_env(tmp_path_factory, test_env):
    root = tmp_path_factory.mktemp("event-broker-scoped-session")
    namespace = f"evbk_e2e_{uuid.uuid4().hex[:8]}"
    subject = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
    with FlightControlBrokerEnv(
        root,
        broker_config=lambda text, home, sidecars: _patch_config(
            text, home, sidecars, namespace,
        ),
        documents=_EXTRA_ENTITIES,
        narrowing_policy=True,
        additional_grants=(
            {
                "subject_id": subject,
                "action": "consume",
                "resource": TOPIC_SEL_EXACT,
                "event_types": [EVENT_TYPE_SEL_EXACT_SIBLING],
            },
            {
                "subject_id": subject,
                "action": "consume",
                "resource": TOPIC_SUBJECT_SELECTOR,
                "subject_types": [SUBJECT_TYPE_OTHER],
            },
            {
                "subject_id": subject,
                "action": "consume",
                "resource": TOPIC_SEL_FAMILY,
                "event_types": [EVENT_TYPE_SEL_BASE, EVENT_TYPE_SEL_RUSH],
            },
        ),
        sidecars=test_env.sidecars,
    ) as pair:
        wait_provisioned(
            f"{pair.base_url}{API_BASE}",
            {
                TOPIC_SEL_EXACT: EVENT_TYPE_SEL_EXACT,
                TOPIC_SUBJECT_SELECTOR: EVENT_TYPE_SUBJECT_SELECTOR,
                TOPIC_SEL_FAMILY: EVENT_TYPE_SEL_BASE,
            },
        )
        yield pair


def public_headers(token: str | None = OWNER_TOKEN) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"} if token is not None else {}


PROVISION_TIMEOUT = 60.0
PROBE_EVENT_ID = "00000000-0000-0000-0000-000000000101"
PROBE_PRODUCER_ID = "00000000-0000-0000-0000-000000000102"


def wait_provisioned(base_url: str, event_types: dict[str, str]) -> None:
    """Verify each admitted topic is readable and reaches producer lookup."""
    deadline = time.monotonic() + PROVISION_TIMEOUT

    def remaining() -> float:
        budget = deadline - time.monotonic()
        if budget <= 0:
            raise AssertionError(
                f"topic {topic!r} for event type {event_type!r} did not provision within "
                f"{PROVISION_TIMEOUT}s (last observation: {last_observation})"
            ) from None
        return min(REQUEST_TIMEOUT, budget)

    def request(send, path, **kwargs) -> httpx.Response:
        try:
            return send(path, timeout=remaining(), **kwargs)
        except httpx.TimeoutException:
            remaining()
            raise

    with httpx.Client(
        base_url=base_url,
        headers={**public_headers(), "traceparent": TRACEPARENT},
        trust_env=False,
    ) as client:
        for topic, event_type in event_types.items():
            last_observation = "no probe response"
            while True:
                response = request(
                    client.post,
                    "/events",
                    json={
                        "id": PROBE_EVENT_ID,
                        "type": event_type,
                        "subject_tenant": OWNER_TENANT,
                        "source": "e2e-probe",
                        "subject": "probe",
                        "subject_type": SUBJECT_TYPE,
                        "occurred_at": "2026-01-01T00:00:00Z",
                        "data": {"strict_field": "probe", "k": "probe"},
                        "meta": {
                            "version": 1,
                            "producer": PROBE_PRODUCER_ID,
                            "previous": 0,
                            "sequence": 1,
                        },
                    },
                )
                if response.status_code == 503:
                    last_observation = "HTTP 503 (unavailable)"
                else:
                    reply = (
                        response.status_code,
                        response.headers.get("content-type"),
                        response.json(),
                    )
                    if reply != (
                        404,
                        "application/problem+json",
                        {
                            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                            "title": "Not Found",
                            "status": 404,
                            "detail": f"event type '{event_type}' is not registered",
                            "instance": "/event-broker/v1/events",
                            "trace_id": TRACE_ID,
                            "context": {
                                "resource_type": "gts.cf.core.events.event_type.v1~",
                                "resource_name": event_type,
                            },
                        },
                    ):
                        break
                    last_observation = "HTTP 404 (event type is not registered)"
                time.sleep(min(0.2, remaining()))
            assert reply == (
                404,
                "application/problem+json",
                {
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                    "title": "Not Found",
                    "status": 404,
                    "detail": "the producer is not registered or its registration has expired",
                    "instance": "/event-broker/v1/events",
                    "trace_id": TRACE_ID,
                    "context": {
                        "resource_type": "gts.cf.core.events.producer.v1~",
                        "resource_name": PROBE_PRODUCER_ID,
                    },
                },
            )
            last_observation = "HTTP 404 (unknown producer)"
            admitted = request(
                client.get,
                "/topics/segments",
                params={"topic": topic, "partition": 0},
            )
            last_observation = f"topic read returned HTTP {admitted.status_code}"
            remaining()
            assert admitted.status_code == 200, admitted.text
            manifest = admitted.json()
            assert isinstance(manifest, dict), manifest
            assert (manifest.get("topic"), manifest.get("partition")) == (topic, 0)


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


@pytest.fixture(scope="session")
def _action_grants_env(tmp_path_factory, test_env):
    root = tmp_path_factory.mktemp("event-broker-action-grants")
    namespace = f"evbk_e2e_{uuid.uuid4().hex[:8]}"
    with FlightControlBrokerEnv(
        root,
        broker_config=lambda text, home, sidecars: _patch_config(
            text, home, sidecars, namespace,
        ),
        documents=_EXTRA_ENTITIES,
        narrowing_policy=True,
        sidecars=test_env.sidecars,
    ) as pair:
        wait_provisioned(f"{pair.base_url}{API_BASE}", _TOPIC_EVENT_TYPES)
        yield pair


@pytest.fixture(scope="session", autouse=True)
def _topics_provisioned(test_env):
    wait_provisioned(f"{test_env.base_url}{API_BASE}", _TOPIC_EVENT_TYPES)


# ── HTTP helpers ──────────────────────────────────────────────────────────

def _api_for_env(test_env):
    """Async client factory bound to the running server's event-broker API."""

    def _client(token: str | None = OWNER_TOKEN) -> httpx.AsyncClient:
        return httpx.AsyncClient(
            base_url=f"{test_env.base_url}{API_BASE}",
            timeout=REQUEST_TIMEOUT,
            headers={**public_headers(token), "traceparent": TRACEPARENT},
            trust_env=False,
        )

    return _client


@pytest.fixture
def api(test_env):
    return _api_for_env(test_env)


@pytest.fixture
def _action_grants_api(_action_grants_env):
    return _api_for_env(_action_grants_env)


@pytest.fixture
def _scoped_api(_scoped_env):
    return _api_for_env(_scoped_env)


EVENT_TYPE_DEMO_GREETING = "gts.cf.core.events.event.v1~cf.core.demo.greeting.v1~"
UNKNOWN_SEGMENTS_TOPIC = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.nonexistent.v1"


def _private_policy_api(test_env, tmp_path, grants, topic, event_type):
    namespace = f"evbk_e2e_policy_{uuid.uuid4().hex[:8]}"
    with FlightControlBrokerEnv(
        tmp_path,
        broker_config=lambda text, home, sidecars: _patch_config(
            text, home, sidecars, namespace,
        ),
        documents=_EXTRA_ENTITIES,
        narrowing_policy=True,
        additional_grants=grants,
        sidecars=test_env.sidecars,
    ) as pair:
        wait_provisioned(f"{pair.base_url}{API_BASE}", {topic: event_type})
        yield _api_for_env(pair)


@pytest.fixture
def _barrier_publish_api(test_env, tmp_path):
    principal = "dddddddd-dddd-dddd-dddd-dddddddddddd"
    yield from _private_policy_api(
        test_env, tmp_path,
        (
            {
                "subject_id": principal,
                "action": "produce",
                "resource": "gts.cf.core.events.request.v1~",
            },
            {
                "subject_id": principal,
                "action": "produce",
                "resource": EVENT_TYPE_DEMO_GREETING,
            },
        ),
        TOPIC_STREAM, EVENT_TYPE_STREAM,
    )


@pytest.fixture
def _unknown_segments_api(test_env, tmp_path):
    yield from _private_policy_api(
        test_env, tmp_path,
        ({"subject_id": OWNER_SUBJECT, "action": "consume", "resource": UNKNOWN_SEGMENTS_TOPIC},),
        TOPIC_STRICT, EVENT_TYPE_STRICT,
    )


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
    async with httpx.AsyncClient(
        base_url=base_url, timeout=REQUEST_TIMEOUT, headers=public_headers(), trust_env=False,
    ) as client:
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
    async with httpx.AsyncClient(
        base_url=base_url, timeout=None, headers=public_headers(), trust_env=False,
    ) as stream_client:
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
    async with httpx.AsyncClient(
        base_url=base_url, timeout=REQUEST_TIMEOUT, headers=public_headers(), trust_env=False,
    ) as client:
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
