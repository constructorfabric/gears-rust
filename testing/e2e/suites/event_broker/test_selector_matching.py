"""Type and subject-type selector scenarios (consumer/stream/1.21-1.25, 1.27-1.34).

Every test runs on a one-partition topic of its own type family and in a fresh
interest tenant. It publishes the events its selector must skip, then a
sentinel event its subscription admits, and reads the stream up to that
sentinel: the topic has one partition and the events were stored in publish
order, so an event missing from the frames before the sentinel was skipped,
not late. Each test compares the whole list of delivered events, as
``(type, subject_type, id)``, with ``==``.

The subject-type selector on an exact type is ``test_consumer_stream.py``'s
``test_subject_types_selector`` (consumer/stream/1.18). consumer/stream/1.26
(a selector without a minor version) has no test here: the registry refuses
an event type's second minor version without the compatibility waiver, since
the event base's ``type`` member refers to the type's own id and so narrows
with every version.

The two tests that admit a type while the server runs (consumer/stream/1.30,
1.31) use a server of this module's own, ``admission_env``: it opens the
platform plane and re-reads the registry every second, settings the session
server does without.
"""

from __future__ import annotations

import asyncio
import os
import subprocess
import tempfile
import uuid
from collections.abc import Iterator
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pytest

from lib.orchestrator import GearTestEnv, RunningTestEnv, _log_path, _prepare_config, _wait_healthy

from .conftest import (
    API_BASE,
    CONFIG,
    EVENT_TYPE_SEL_BASE,
    EVENT_TYPE_SEL_EXACT,
    EVENT_TYPE_SEL_EXACT_SIBLING,
    EVENT_TYPE_SEL_EXPRESS,
    EVENT_TYPE_SEL_LATE,
    EVENT_TYPE_SEL_LATE_ALLOWED,
    EVENT_TYPE_SEL_LATE_DENIED,
    EVENT_TYPE_SEL_NOMATCH,
    EVENT_TYPE_SEL_NOMATCH_PATTERN,
    EVENT_TYPE_SEL_NOMATCH_SENTINEL,
    EVENT_TYPE_SEL_OTHER,
    EVENT_TYPE_SEL_PREFIX_CREATED,
    EVENT_TYPE_SEL_PREFIX_OUTSIDER,
    EVENT_TYPE_SEL_PREFIX_PATTERN,
    EVENT_TYPE_SEL_PREFIX_PLACED,
    EVENT_TYPE_SEL_PRIORITY,
    EVENT_TYPE_SEL_RUSH,
    EVENT_TYPE_SEL_SUBJECT,
    EVENT_TYPE_SEL_SUBJECT_SENTINEL,
    PROJECT_ROOT,
    REQUEST_TIMEOUT,
    SERVER_PORT,
    SUBJECT_TYPE,
    SUBJECT_TYPE_SEL_AUDITOR,
    SUBJECT_TYPE_SEL_CUSTOMER,
    SUBJECT_TYPE_SEL_CUSTOMER_PREFIX,
    SUBJECT_TYPE_SEL_MEMBER,
    SUBJECT_TYPE_SEL_PARTNER,
    SUBJECT_TYPE_SEL_SUPPLIER,
    TOPIC_SEL_EXACT,
    TOPIC_SEL_FAMILY,
    TOPIC_SEL_LATE,
    TOPIC_SEL_NOMATCH,
    TOPIC_SEL_PREFIX,
    TOPIC_SEL_SUBJECT,
    TRACEPARENT,
    XFAIL_NO_ACTION_AWARE_PDP,
    MultipartReader,
    _COMMITTED_ENTRY,
    _TEMP_HOME,
    _derived_event_type,
    _patch_config,
    group_seek_positions,
    patch_backend,
    wait_provisioned,
)

# One budget for reading a stream up to its sentinel, and the read timeout of the
# stream request itself: heartbeats arrive every 5 s, so a stream that never
# delivers the sentinel fails here rather than at the test timeout.
STREAM_DEADLINE_S = 15.0
# How long each step of admitting a type while the server runs may take: the
# registry operation, then the broker listing the type after a specification
# refresh.
ADMISSION_STEP_S = 15.0
# One deadline for a whole runtime-admission test: its two admission steps, then
# the stream read up to the sentinel.
ADMISSION_TEST_DEADLINE_S = 2 * ADMISSION_STEP_S + STREAM_DEADLINE_S
# The pytest timeout of those tests: 15 s above their deadline, room for a
# cancelled flow to close its stream and clients, so the deadline fails first.
ADMISSION_TEST_TIMEOUT_S = ADMISSION_TEST_DEADLINE_S + 15.0

# The runtime-admission server (`admission_env`): its port, which no other server
# of the e2e suites binds, and the storage-backend namespace that keeps its
# events apart from the session server's on a shared sidecar.
ADMISSION_PORT = 8093
ADMISSION_NAMESPACE = "evbk_e2e_admission"
# The namespace the session config patch (`_patch_config`) gives its backend.
SESSION_NAMESPACE = "evbk_e2e"
# The platform-plane token the runtime-admission server's api-gateway accepts
# (`POST /types-registry/v2/platform/entities`).
INTERNAL_TOKEN = "eb-e2e-internal-token"
# How often the runtime-admission server re-reads the registry: a type admitted
# while it runs is resolvable within one interval.
SPECIFICATION_REFRESH_INTERVAL_S = 1


def _patch_admission_config(config_text: str, env: GearTestEnv, home_dir: str) -> str:
    """The session config (``_patch_config``) moved to ``ADMISSION_PORT``,
    ``home_dir`` and ``ADMISSION_NAMESPACE``, so the two servers share no port,
    file or backend medium, plus a one-second specification refresh and the
    api-gateway's inbound authenticator for the platform plane."""
    config_text = _patch_config(config_text, env)
    moves = [
        (f'bind_addr: "127.0.0.1:{SERVER_PORT}"', f'bind_addr: "127.0.0.1:{ADMISSION_PORT}"'),
        (f'home_dir: "{Path(_TEMP_HOME).as_posix()}"', f'home_dir: "{Path(home_dir).as_posix()}"'),
        (
            patch_backend(_COMMITTED_ENTRY, env.sidecars, SESSION_NAMESPACE, _TEMP_HOME),
            patch_backend(_COMMITTED_ENTRY, env.sidecars, ADMISSION_NAMESPACE, home_dir),
        ),
    ]
    for session, admission in moves:
        assert config_text.count(session) == 1, session
        config_text = config_text.replace(session, admission)
    workers = (
        "      workers:\n"
        f"        specification_refresh_interval_secs: {SPECIFICATION_REFRESH_INTERVAL_S}\n"
    )
    anchor = "      backends:"
    assert config_text.count(anchor) == 1 and "      workers:" not in config_text
    config_text = config_text.replace(anchor, workers + anchor, 1)
    # The platform plane: without an inbound authenticator the gateway refuses
    # every platform route, the registry's entity admission included.
    docs = "      enable_docs: true\n"
    assert config_text.count(docs) == 1 and "internal_auth:" not in config_text
    return config_text.replace(
        docs,
        docs
        + "      internal_auth:\n"
        + "        provider: shared_secret\n"
        + f'        secret: "{INTERNAL_TOKEN}"\n',
    )


def _stop(proc: subprocess.Popen) -> None:
    """Stops ``proc``, killing it if it outlives a graceful stop."""
    proc.terminate()
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


@pytest.fixture(scope="module")
def admission_env(test_env) -> Iterator[RunningTestEnv]:
    """The runtime-admission tests' own server (``_patch_admission_config``),
    on the session's storage backend and sidecar.

    The process is stopped on every exit path: after the module's tests, and
    at once when it fails to become ready. Its home lives under the suite's
    temp home; its generated config file is removed once it has stopped.
    """
    home_dir = tempfile.mkdtemp(prefix="admission-", dir=_TEMP_HOME)
    env = GearTestEnv(
        config_path=CONFIG,
        config_patch=lambda text, env: _patch_admission_config(text, env, home_dir),
        port=ADMISSION_PORT,
        health_path="/readyz",
        env={"RUST_LOG": os.environ.get("RUST_LOG", "info,event_broker=debug")},
        sidecars=list(test_env.sidecars.values()),
        log_suffix="event-broker-admission",
    )
    base_url = f"http://localhost:{ADMISSION_PORT}"
    config_path = _prepare_config(env)
    try:
        with open(_log_path(env), "w") as log:
            proc = subprocess.Popen(
                [os.environ["E2E_BINARY"], "--config", str(config_path), "run"],
                cwd=str(PROJECT_ROOT),
                stdout=log,
                stderr=subprocess.STDOUT,
                env={**os.environ, **env.env},
            )
        try:
            _wait_healthy(env)
            wait_provisioned(f"{base_url}{API_BASE}", {TOPIC_SEL_LATE: EVENT_TYPE_SEL_LATE})
            yield RunningTestEnv(base_url=base_url, env=env, sidecars=test_env.sidecars)
        finally:
            _stop(proc)
    finally:
        config_path.unlink()


def _client(server: RunningTestEnv) -> httpx.AsyncClient:
    """A client of ``server``'s event-broker API, built as the ``api`` fixture
    builds one for the session server."""
    return httpx.AsyncClient(
        base_url=f"{server.base_url}{API_BASE}",
        timeout=REQUEST_TIMEOUT,
        headers={"traceparent": TRACEPARENT},
    )


async def _join_and_seek(client: httpx.AsyncClient, interests: list[dict]) -> dict:
    """Creates a group, JOINs it with ``interests`` and SEEKs every assigned
    partition to earliest. Returns the JOIN ``201`` body."""
    group_resp = await client.post("/consumer-groups")
    assert group_resp.status_code == 201, group_resp.text
    sub_resp = await client.post(
        "/subscriptions",
        json={
            "consumer_group": group_resp.json()["id"],
            "client_agent": "e2e-test",
            "interests": interests,
        },
    )
    assert sub_resp.status_code == 201, sub_resp.text
    joined = sub_resp.json()
    seek_resp = await client.post(
        f"/subscriptions/{joined['id']}:seek",
        json={
            "topology_version": joined["topology_version"],
            "positions": group_seek_positions(
                {"topic": a["topic"], "partition": a["partition"], "value": "earliest"}
                for a in joined["assigned"]
            ),
        },
    )
    assert seek_resp.status_code == 200, seek_resp.text
    return joined


async def _publish(client: httpx.AsyncClient, event_type: str, tenant_id: str, subject_type: str) -> str:
    """Publishes one event of ``event_type`` about ``subject_type`` for
    ``tenant_id`` and returns its id."""
    event_id = str(uuid.uuid4())
    resp = await client.post(
        "/events",
        json={
            "id": event_id,
            "type": event_type,
            "subject_tenant": tenant_id,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": datetime.now(timezone.utc).isoformat(),
        },
    )
    assert resp.status_code == 202, resp.text
    return event_id


async def _delivered_through_sentinel(test_env, sub_id: str, publish) -> list[tuple[str, str, str]]:
    """Opens the multipart stream of ``sub_id``, awaits its topology frame,
    calls ``publish()`` (which publishes and returns the sentinel's id), and
    returns ``(type, subject_type, id)`` of every event frame up to and
    including the sentinel. Only heartbeats may come between them."""
    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1",
        timeout=httpx.Timeout(REQUEST_TIMEOUT, read=STREAM_DEADLINE_S),
    ) as stream_client:
        async with stream_client.stream(
            "GET",
            f"/events:stream?subscription={sub_id}",
            headers={"Accept": "multipart/mixed"},
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = MultipartReader(stream_resp)
            await reader.await_kind("topology")
            sentinel_id = await publish()
            delivered = []
            deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
            while not delivered or delivered[-1][2] != sentinel_id:
                remaining = deadline - asyncio.get_running_loop().time()
                assert remaining > 0, f"the sentinel was not delivered; delivered: {delivered}"
                frame = await reader.next_frame(timeout=remaining)
                if frame["kind"] == "heartbeat":
                    continue
                assert frame["kind"] == "event", frame
                payload = frame["payload"]
                delivered.append((payload["type"], payload["subject_type"], payload["id"]))
    return delivered


async def _admit_event_type(server: RunningTestEnv, document: dict) -> None:
    """Admits ``document`` to ``server``'s registry while it runs and waits
    until its broker lists it among its topic's event types. The waits have no
    bound of their own: the caller's deadline bounds them."""
    type_id = document["$id"].removeprefix("gts://")
    topic = document["x-gts-traits"]["topic"]
    async with httpx.AsyncClient(
        base_url=f"{server.base_url}/types-registry/v2/platform",
        headers={"X-ToolKit-Internal-Token": INTERNAL_TOKEN},
        timeout=REQUEST_TIMEOUT,
    ) as registry:
        resp = await registry.post(
            "/entities",
            headers={"Idempotency-Key": str(uuid.uuid4())},
            json={"items": [{"gts_id": type_id, "content": document}]},
        )
        assert resp.status_code == 202, resp.text
        location = resp.headers["location"]
        while True:
            polled = await registry.get(httpx.URL(str(resp.url)).join(location))
            assert polled.status_code == 200, polled.text
            operation = polled.json()
            if operation["status"] == "completed":
                break
            await asyncio.sleep(0.05)
    assert operation["items"] == [
        {"gts_id": type_id, "status": "succeeded", "resource_version": 1, "error": None}
    ]
    async with httpx.AsyncClient(
        base_url=f"{server.base_url}/event-broker/v1", timeout=REQUEST_TIMEOUT
    ) as client:
        while True:
            listed = await client.get("/event-types", params={"$filter": f"topic eq '{topic}'"})
            assert listed.status_code == 200, listed.text
            if type_id in [item["id"] for item in listed.json()["items"]]:
                return
            await asyncio.sleep(0.2)


@pytest.mark.timeout(30, func_only=True)
async def test_exact_selector_delivers_only_its_type(api, test_env):
    """scenario: consumer/stream/1.21-positive-exact-type-selector.md

    An interest naming one type with no derived types skips a sibling type
    published before it.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_EXACT, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_EXACT]}],
        )
        ids = []

        async def publish():
            ids.append(await _publish(client, EVENT_TYPE_SEL_EXACT_SIBLING, tenant_id, SUBJECT_TYPE))
            ids.append(await _publish(client, EVENT_TYPE_SEL_EXACT, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [(EVENT_TYPE_SEL_EXACT, SUBJECT_TYPE, ids[1])]


@pytest.mark.timeout(30, func_only=True)
async def test_base_selector_delivers_its_derived_types(api, test_env):
    """scenario: consumer/stream/1.22-positive-base-type-selector-covers-derived-types.md

    An interest naming a base type is delivered the base and both types derived
    from it, and not a type of another family.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_FAMILY, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_BASE]}],
        )
        ids = []

        async def publish():
            for event_type in [
                EVENT_TYPE_SEL_OTHER,
                EVENT_TYPE_SEL_BASE,
                EVENT_TYPE_SEL_EXPRESS,
                EVENT_TYPE_SEL_RUSH,
                EVENT_TYPE_SEL_BASE,
            ]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE, ids[1]),
        (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE, ids[2]),
        (EVENT_TYPE_SEL_RUSH, SUBJECT_TYPE, ids[3]),
        (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE, ids[4]),
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_selector_delivers_types_two_levels_below_it(api, test_env):
    """scenario: consumer/stream/1.23-positive-selector-covers-two-levels-below.md

    An interest naming a base type is delivered a type derived from it and a
    type derived from that one.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_FAMILY, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_BASE]}],
        )
        ids = []

        async def publish():
            for event_type in [EVENT_TYPE_SEL_EXPRESS, EVENT_TYPE_SEL_PRIORITY, EVENT_TYPE_SEL_BASE]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE, ids[0]),
        (EVENT_TYPE_SEL_PRIORITY, SUBJECT_TYPE, ids[1]),
        (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE, ids[2]),
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_derived_selector_delivers_neither_its_base_nor_a_sibling(api, test_env):
    """scenario: consumer/stream/1.24-positive-derived-type-selector.md

    An interest naming a derived type is delivered that type and the type
    derived from it, and neither its base nor a sibling derived from the same
    base.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_FAMILY, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_EXPRESS]}],
        )
        ids = []

        async def publish():
            for event_type in [
                EVENT_TYPE_SEL_BASE,
                EVENT_TYPE_SEL_RUSH,
                EVENT_TYPE_SEL_EXPRESS,
                EVENT_TYPE_SEL_PRIORITY,
                EVENT_TYPE_SEL_EXPRESS,
            ]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE, ids[2]),
        (EVENT_TYPE_SEL_PRIORITY, SUBJECT_TYPE, ids[3]),
        (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE, ids[4]),
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_prefix_wildcard_delivers_only_types_under_it(api, test_env):
    """scenario: consumer/stream/1.25-positive-prefix-wildcard-selector.md

    An interest naming a namespace with a trailing ``*`` is delivered the types
    under that namespace and not a type of the same topic outside it.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [
                {
                    "topic": TOPIC_SEL_PREFIX,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_PREFIX_PATTERN],
                }
            ],
        )
        ids = []

        async def publish():
            for event_type in [
                EVENT_TYPE_SEL_PREFIX_CREATED,
                EVENT_TYPE_SEL_PREFIX_OUTSIDER,
                EVENT_TYPE_SEL_PREFIX_PLACED,
            ]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_PREFIX_CREATED, SUBJECT_TYPE, ids[0]),
        (EVENT_TYPE_SEL_PREFIX_PLACED, SUBJECT_TYPE, ids[2]),
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_selector_matching_no_type_delivers_nothing(api, test_env):
    """scenario: consumer/stream/1.27-positive-selector-matching-no-type.md

    A JOIN whose wildcard matches no type of the topic succeeds and reports the
    wildcard in ``unmatched_types``; the stream delivers none of the topic's
    events to it. The sentinel comes through a second interest that names only
    the sentinel type.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [
                {
                    "topic": TOPIC_SEL_NOMATCH,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_NOMATCH_PATTERN],
                },
                {
                    "topic": TOPIC_SEL_NOMATCH,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_NOMATCH_SENTINEL],
                },
            ],
        )
        assert joined == {
            "id": joined["id"],
            "consumer_group": joined["consumer_group"],
            "client_agent": "e2e-test",
            "interests": [
                {
                    "topic": TOPIC_SEL_NOMATCH,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [EVENT_TYPE_SEL_NOMATCH_PATTERN],
                    "filter": None,
                    "unmatched_types": [EVENT_TYPE_SEL_NOMATCH_PATTERN],
                },
                {
                    "topic": TOPIC_SEL_NOMATCH,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [EVENT_TYPE_SEL_NOMATCH_SENTINEL],
                    "filter": None,
                    "unmatched_types": [],
                },
            ],
            "assigned": [{"topic": TOPIC_SEL_NOMATCH, "partition": 0}],
            "topology_version": 1,
            "created_at": joined["created_at"],
        }
        ids = []

        async def publish():
            for event_type in [
                EVENT_TYPE_SEL_NOMATCH,
                EVENT_TYPE_SEL_NOMATCH,
                EVENT_TYPE_SEL_NOMATCH_SENTINEL,
            ]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [(EVENT_TYPE_SEL_NOMATCH_SENTINEL, SUBJECT_TYPE, ids[2])]


@pytest.mark.timeout(30, func_only=True)
async def test_event_matched_by_two_selectors_is_delivered_once(api, test_env):
    """scenario: consumer/stream/1.28-positive-overlapping-selectors-deliver-once.md

    An event that both selectors of one interest match (the base type and the
    derived type itself) is delivered as one event frame.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [
                {
                    "topic": TOPIC_SEL_FAMILY,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_BASE, EVENT_TYPE_SEL_EXPRESS],
                }
            ],
        )
        ids = []

        async def publish():
            for event_type in [EVENT_TYPE_SEL_EXPRESS, EVENT_TYPE_SEL_BASE]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE, ids[0]),
        (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE, ids[1]),
    ]


@XFAIL_NO_ACTION_AWARE_PDP
@pytest.mark.timeout(30, func_only=True)
async def test_derived_type_denied_by_scope_is_not_delivered(api, test_env):
    """scenario: consumer/stream/1.29-negative-derived-type-denied-by-scope.md

    An interest naming a base type is not delivered a derived type the
    caller's consume scope leaves out; the base, which the scope names, is.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_FAMILY, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_BASE]}],
        )
        ids = []

        async def publish():
            for event_type in [EVENT_TYPE_SEL_EXPRESS, EVENT_TYPE_SEL_BASE]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [(EVENT_TYPE_SEL_BASE, SUBJECT_TYPE, ids[1])]


@pytest.mark.timeout(ADMISSION_TEST_TIMEOUT_S, func_only=True)
async def test_type_admitted_after_join_is_delivered(admission_env):
    """scenario: consumer/stream/1.30-positive-type-admitted-after-join.md

    A type derived from the interest's base type and admitted to the registry
    after the JOIN is delivered once the broker resolves it.

    The flow runs under one deadline (45 s), which fails at the await it stops
    or at the stream's own sentinel assert; the pytest timeout (60 s) is 15 s
    above it, so it fires only after that deadline would have. The server
    starts in ``admission_env``, outside both.
    """
    tenant_id = str(uuid.uuid4())
    async with asyncio.timeout(ADMISSION_TEST_DEADLINE_S), _client(admission_env) as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_LATE, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_LATE]}],
        )
        ids = []

        async def publish():
            await _admit_event_type(
                admission_env,
                _derived_event_type(
                    EVENT_TYPE_SEL_LATE_ALLOWED, TOPIC_SEL_LATE, parent=EVENT_TYPE_SEL_LATE
                ),
            )
            for event_type in [EVENT_TYPE_SEL_LATE_ALLOWED, EVENT_TYPE_SEL_LATE]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(admission_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_LATE_ALLOWED, SUBJECT_TYPE, ids[0]),
        (EVENT_TYPE_SEL_LATE, SUBJECT_TYPE, ids[1]),
    ]


@XFAIL_NO_ACTION_AWARE_PDP
@pytest.mark.timeout(ADMISSION_TEST_TIMEOUT_S, func_only=True)
async def test_type_admitted_after_join_and_denied_is_not_delivered(admission_env):
    """scenario: consumer/stream/1.31-negative-type-admitted-after-join-denied.md

    A type derived from the interest's base type, admitted after the JOIN and
    denied by the caller's own answer for it, is not delivered; the base is.

    The flow runs under one deadline (45 s), which fails at the await it stops
    or at the stream's own sentinel assert; the pytest timeout (60 s) is 15 s
    above it, so it fires only after that deadline would have. The server
    starts in ``admission_env``, outside both.
    """
    tenant_id = str(uuid.uuid4())
    async with asyncio.timeout(ADMISSION_TEST_DEADLINE_S), _client(admission_env) as client:
        joined = await _join_and_seek(
            client,
            [{"topic": TOPIC_SEL_LATE, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE_SEL_LATE]}],
        )
        ids = []

        async def publish():
            await _admit_event_type(
                admission_env,
                _derived_event_type(
                    EVENT_TYPE_SEL_LATE_DENIED, TOPIC_SEL_LATE, parent=EVENT_TYPE_SEL_LATE
                ),
            )
            for event_type in [EVENT_TYPE_SEL_LATE_DENIED, EVENT_TYPE_SEL_LATE]:
                ids.append(await _publish(client, event_type, tenant_id, SUBJECT_TYPE))
            return ids[-1]

        delivered = await _delivered_through_sentinel(admission_env, joined["id"], publish)

    assert delivered == [(EVENT_TYPE_SEL_LATE, SUBJECT_TYPE, ids[1])]


@pytest.mark.timeout(30, func_only=True)
async def test_base_subject_type_selector_delivers_derived_subjects(api, test_env):
    """scenario: consumer/stream/1.32-positive-base-subject-type-selector.md

    An interest whose ``subject_types`` names a base subject type is delivered
    events about it and about a subject type derived from it, and not about
    another subject type.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [
                {
                    "topic": TOPIC_SEL_SUBJECT,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_SUBJECT],
                    "subject_types": [SUBJECT_TYPE_SEL_CUSTOMER],
                }
            ],
        )
        ids = []

        async def publish():
            for subject_type in [
                SUBJECT_TYPE_SEL_MEMBER,
                SUBJECT_TYPE_SEL_CUSTOMER,
                SUBJECT_TYPE_SEL_SUPPLIER,
                SUBJECT_TYPE_SEL_CUSTOMER,
            ]:
                ids.append(await _publish(client, EVENT_TYPE_SEL_SUBJECT, tenant_id, subject_type))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_MEMBER, ids[0]),
        (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER, ids[1]),
        (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER, ids[3]),
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_trailing_wildcard_subject_selector_delivers_only_its_prefix(api, test_env):
    """scenario: consumer/stream/1.33-positive-trailing-wildcard-subject-selector.md

    An interest whose ``subject_types`` names a base subject type with a trailing
    ``*`` under one vendor is delivered events about a derived subject type of
    that vendor only: not about the bare base, a derived subject type of
    another vendor, or another subject type.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [
                {
                    "topic": TOPIC_SEL_SUBJECT,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_SUBJECT],
                    "subject_types": [SUBJECT_TYPE_SEL_CUSTOMER_PREFIX],
                }
            ],
        )
        ids = []

        async def publish():
            for subject_type in [
                SUBJECT_TYPE_SEL_MEMBER,
                SUBJECT_TYPE_SEL_SUPPLIER,
                SUBJECT_TYPE_SEL_CUSTOMER,
                SUBJECT_TYPE_SEL_PARTNER,
                SUBJECT_TYPE_SEL_MEMBER,
            ]:
                ids.append(await _publish(client, EVENT_TYPE_SEL_SUBJECT, tenant_id, subject_type))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [
        (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_MEMBER, ids[0]),
        (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_MEMBER, ids[4]),
    ]


@pytest.mark.timeout(30, func_only=True)
async def test_subject_selector_matching_no_subject_delivers_nothing(api, test_env):
    """scenario: consumer/stream/1.34-positive-subject-selector-matching-no-subject.md

    A JOIN whose ``subject_types`` names a subject type the event type does not
    allow succeeds with nothing in ``unmatched_types``; the stream delivers
    none of the type's events to it. The sentinel comes through a second
    interest that names only the sentinel type.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        joined = await _join_and_seek(
            client,
            [
                {
                    "topic": TOPIC_SEL_SUBJECT,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_SUBJECT],
                    "subject_types": [SUBJECT_TYPE_SEL_AUDITOR],
                },
                {
                    "topic": TOPIC_SEL_SUBJECT,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_SEL_SUBJECT_SENTINEL],
                },
            ],
        )
        assert joined == {
            "id": joined["id"],
            "consumer_group": joined["consumer_group"],
            "client_agent": "e2e-test",
            "interests": [
                {
                    "topic": TOPIC_SEL_SUBJECT,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [EVENT_TYPE_SEL_SUBJECT],
                    "subject_types": [SUBJECT_TYPE_SEL_AUDITOR],
                    "filter": None,
                    "unmatched_types": [],
                },
                {
                    "topic": TOPIC_SEL_SUBJECT,
                    "subject_tenant": tenant_id,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [EVENT_TYPE_SEL_SUBJECT_SENTINEL],
                    "filter": None,
                    "unmatched_types": [],
                },
            ],
            "assigned": [{"topic": TOPIC_SEL_SUBJECT, "partition": 0}],
            "topology_version": 1,
            "created_at": joined["created_at"],
        }
        ids = []

        async def publish():
            for event_type, subject_type in [
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_SUPPLIER),
                (EVENT_TYPE_SEL_SUBJECT_SENTINEL, SUBJECT_TYPE),
            ]:
                ids.append(await _publish(client, event_type, tenant_id, subject_type))
            return ids[-1]

        delivered = await _delivered_through_sentinel(test_env, joined["id"], publish)

    assert delivered == [(EVENT_TYPE_SEL_SUBJECT_SENTINEL, SUBJECT_TYPE, ids[2])]
