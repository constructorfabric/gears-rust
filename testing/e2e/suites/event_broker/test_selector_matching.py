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
import re
import uuid
from contextlib import AsyncExitStack, contextmanager
from collections.abc import Iterator
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    ALL_EVENT_TYPES_PATTERN,
    API_BASE,
    DEFAULT_SUBJECT_ID,
    DEFAULT_TENANT_ID,
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
    REQUEST_TIMEOUT,
    SUBJECT_TYPE,
    SUBJECT_TYPE_SEL_AUDITOR,
    SUBJECT_TYPE_SEL_CUSTOMER,
    SUBJECT_TYPE_SEL_CUSTOMER_PREFIX,
    SUBJECT_TYPE_SEL_MEMBER,
    SUBJECT_TYPE_SEL_PARTNER,
    SUBJECT_TYPE_SEL_SUPPLIER,
    TOKEN_PRINCIPAL_B,
    TOPIC_SEL_EXACT,
    TOPIC_SEL_FAMILY,
    TOPIC_SEL_LATE,
    TOPIC_SEL_NOMATCH,
    TOPIC_SEL_PREFIX,
    TOPIC_SEL_SUBJECT,
    TRACEPARENT,
    MultipartReader,
    _EXTRA_ENTITIES,
    _api_for_env,
    _derived_event_type,
    _patch_config,
    group_seek_positions,
    public_headers,
    wait_provisioned,
)
from .topology import (
    OWNER_TENANT,
    REGISTRY_PATH,
    FlightControlBrokerEnv,
    registry_entity,
)
from .test_consumer_subscriptions import _created_subscription_id

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

@contextmanager
def _admission_pair(
    test_env, tmp_path, *, narrowing_policy: bool = False, additional_consume_grants=(),
) -> Iterator[FlightControlBrokerEnv]:
    namespace = f"evbk_e2e_admission_{uuid.uuid4().hex[:8]}"
    with FlightControlBrokerEnv(
        tmp_path,
        broker_config=lambda text, home, sidecars: _patch_config(
            text, home, sidecars, namespace,
        ),
        documents=_EXTRA_ENTITIES,
        narrowing_policy=narrowing_policy,
        additional_event_types=(EVENT_TYPE_SEL_LATE_ALLOWED, EVENT_TYPE_SEL_LATE_DENIED),
        additional_grants=additional_consume_grants,
        sidecars=test_env.sidecars,
    ) as pair:
        wait_provisioned(f"{pair.base_url}{API_BASE}", {TOPIC_SEL_LATE: EVENT_TYPE_SEL_LATE})
        yield pair


@pytest.fixture(scope="function")
def admission_env(test_env, tmp_path) -> Iterator[FlightControlBrokerEnv]:
    with _admission_pair(test_env, tmp_path) as pair:
        yield pair


@pytest.fixture(scope="function")
def denied_admission_env(test_env, tmp_path) -> Iterator[FlightControlBrokerEnv]:
    with _admission_pair(
        test_env,
        tmp_path,
        narrowing_policy=True,
        additional_consume_grants=(
            {
                "subject_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "action": "consume",
                "resource": TOPIC_SEL_LATE,
                "event_types": [EVENT_TYPE_SEL_LATE],
            },
        ),
    ) as pair:
        yield pair


def _client(server: FlightControlBrokerEnv) -> httpx.AsyncClient:
    return httpx.AsyncClient(
        base_url=f"{server.base_url}{API_BASE}",
        timeout=REQUEST_TIMEOUT,
        headers={**public_headers(), "traceparent": TRACEPARENT},
        trust_env=False,
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
        headers=public_headers(),
        trust_env=False,
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


async def _admit_event_type(server: FlightControlBrokerEnv, document: dict) -> None:
    """Admit a document on Core and wait for the worker to resolve it."""
    expected = registry_entity(document)
    type_id = expected["gts_id"]
    topic = document["x-gts-traits"]["topic"]
    clock = asyncio.get_running_loop().time
    deadline = clock() + ADMISSION_STEP_S

    def remaining() -> float:
        budget = deadline - clock()
        assert budget > 0, f"admission of {type_id} exceeded {ADMISSION_STEP_S}s"
        return min(REQUEST_TIMEOUT, budget)

    async with httpx.AsyncClient(
        base_url=server.flight_control_base_url, headers=server.platform_headers, trust_env=False,
    ) as registry:
        resp = await registry.post(
            REGISTRY_PATH,
            headers={"Idempotency-Key": str(uuid.uuid4())},
            json={"items": [{"gts_id": type_id, "content": document}]},
            timeout=remaining(),
        )
        assert resp.status_code == 202, server.redact(resp.text)
        location = resp.url.join(resp.headers["location"])
        assert (location.scheme, location.host, location.port) == (
            resp.url.scheme, resp.url.host, resp.url.port,
        )
        while True:
            polled = await registry.get(location, timeout=remaining())
            assert polled.status_code == 200, server.redact(polled.text)
            operation = polled.json()
            if operation["status"] == "completed":
                break
            await asyncio.sleep(min(0.05, remaining()))
        assert operation["items"] == [
            {"gts_id": type_id, "status": "succeeded", "resource_version": 1, "error": None}
        ]
        active = await registry.get(
            f"{REGISTRY_PATH}/{type_id}", params={"$select": "content"}, timeout=remaining(),
        )
        assert (active.status_code, active.headers.get("content-type"), active.json()) == (
            200, "application/json", expected,
        )
        elapsed = ADMISSION_STEP_S - (deadline - clock())
        print(f"Core admitted {type_id} at version 1 after {elapsed:.3f}s")

    deadline = clock() + ADMISSION_STEP_S
    async with _client(server) as client:
        while True:
            listed = await client.get(
                "/event-types", params={"$filter": f"topic eq '{topic}'"}, timeout=remaining(),
            )
            assert listed.status_code == 200, listed.text
            if type_id in [item["id"] for item in listed.json()["items"]]:
                print(
                    f"worker resolved {type_id} after "
                    f"{ADMISSION_STEP_S - (deadline - clock()):.3f}s"
                )
                return
            await asyncio.sleep(min(0.2, remaining()))


def _stream_instant(value: str) -> tuple[datetime, int]:
    assert isinstance(value, str), value
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?Z",
        value,
    ), value
    clock = datetime.fromisoformat(value.replace("Z", "+00:00")).replace(microsecond=0)
    return clock, int(value[:-1].partition(".")[2].ljust(9, "0"))


async def _read_case_window(
    reader: MultipartReader, high_water: int, sentinel: str, deadline: float,
) -> tuple[list[dict], list[dict]]:
    history: list[dict] = []
    delivered: list[dict] = []
    clock = asyncio.get_running_loop().time
    while True:
        remaining = deadline - clock()
        assert remaining > 0, f"no sentinel {sentinel}; got {history + delivered}"
        frame = await reader.await_kind("event", timeout=remaining)
        assert set(frame) == {"kind", "payload"}, frame
        match frame:
            case {"kind": "event", "payload": {"sequence": int(sequence), "id": str(event_id)}}:
                assert type(sequence) is int and sequence > 0
            case unexpected:
                pytest.fail(f"unexpected event frame: {unexpected}")
        if sequence <= high_water:
            assert not delivered, frame
            history.append(frame)
            continue
        delivered.append(frame)
        if event_id == sentinel:
            return history, delivered


@pytest.mark.timeout(30, func_only=True)
async def test_exact_selector_delivers_only_its_type(api, test_env):
    """scenario: consumer/stream/1.21-positive-exact-type-selector.md

    An interest naming one type with no derived types skips a sibling type
    published before it.
    """
    topic = TOPIC_SEL_EXACT
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_EXACT],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000168{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_EXACT_SIBLING, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_EXACT, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[1]]


@pytest.mark.timeout(30, func_only=True)
async def test_base_selector_delivers_its_derived_types(api, test_env):
    """scenario: consumer/stream/1.22-positive-base-type-selector-covers-derived-types.md

    An interest naming a base type is delivered the base and both types derived
    from it, and not a type of another family.
    """
    topic = TOPIC_SEL_FAMILY
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_BASE],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000169{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_OTHER, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_RUSH, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[1], stored[2], stored[3], stored[4]]


@pytest.mark.timeout(30, func_only=True)
async def test_selector_delivers_types_two_levels_below_it(api, test_env):
    """scenario: consumer/stream/1.23-positive-selector-covers-two-levels-below.md

    An interest naming a base type is delivered a type derived from it and a
    type derived from that one.
    """
    topic = TOPIC_SEL_FAMILY
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_BASE],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000170{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_PRIORITY, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[0], stored[1], stored[2]]


@pytest.mark.timeout(30, func_only=True)
async def test_derived_selector_delivers_neither_its_base_nor_a_sibling(api, test_env):
    """scenario: consumer/stream/1.24-positive-derived-type-selector.md

    An interest naming a derived type is delivered that type and the type
    derived from it, and neither its base nor a sibling derived from the same
    base.
    """
    topic = TOPIC_SEL_FAMILY
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_EXPRESS],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000171{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_RUSH, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_PRIORITY, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[2], stored[3], stored[4]]


@pytest.mark.timeout(30, func_only=True)
async def test_prefix_wildcard_delivers_only_types_under_it(api, test_env):
    """scenario: consumer/stream/1.25-positive-prefix-wildcard-selector.md

    An interest naming a namespace with a trailing ``*`` is delivered the types
    under that namespace and not a type of the same topic outside it.
    """
    topic = TOPIC_SEL_PREFIX
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_PREFIX_PATTERN],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000172{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_PREFIX_CREATED, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_PREFIX_OUTSIDER, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_PREFIX_PLACED, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[0], stored[2]]


@pytest.mark.timeout(30, func_only=True)
async def test_selector_matching_no_type_delivers_nothing(api, test_env):
    """scenario: consumer/stream/1.27-positive-selector-matching-no-type.md

    A JOIN whose wildcard matches no type of the topic succeeds and reports the
    wildcard in ``unmatched_types``; the stream delivers none of the topic's
    events to it. The sentinel comes through a second interest that names only
    the sentinel type.
    """
    topic = TOPIC_SEL_NOMATCH
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_NOMATCH_PATTERN],
        },
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_NOMATCH_SENTINEL],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000173{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_NOMATCH, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_NOMATCH, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_NOMATCH_SENTINEL, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[EVENT_TYPE_SEL_NOMATCH_PATTERN], []]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[2]]


@pytest.mark.timeout(30, func_only=True)
async def test_event_matched_by_two_selectors_is_delivered_once(api, test_env):
    """scenario: consumer/stream/1.28-positive-overlapping-selectors-deliver-once.md

    An event that both selectors of one interest match (the base type and the
    derived type itself) is delivered as one event frame.
    """
    topic = TOPIC_SEL_FAMILY
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_BASE, EVENT_TYPE_SEL_EXPRESS],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000174{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_EXPRESS, SUBJECT_TYPE),
                (EVENT_TYPE_SEL_BASE, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[0], stored[1]]


@pytest.mark.timeout(30, func_only=True)
async def test_derived_type_denied_by_scope_is_not_delivered(_scoped_api):
    """scenario: consumer/stream/1.29-negative-derived-type-denied-by-scope.md"""
    topic = TOPIC_SEL_FAMILY
    interest = {
        "topic": topic,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SEL_BASE],
    }
    events = [
        {
            "id": "16e20000-0000-4000-8000-000001750001",
            "type": EVENT_TYPE_SEL_EXPRESS,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:31Z",
        },
        {
            "id": "16e20000-0000-4000-8000-000001750002",
            "type": EVENT_TYPE_SEL_BASE,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:32Z",
        },
    ]

    async with (
        _scoped_api(TOKEN_PRINCIPAL_B) as restricted,
        _scoped_api() as observer,
        AsyncExitStack() as stack,
    ):
        groups = []
        subscriptions = []
        readers = []
        starts = []
        clock = asyncio.get_running_loop().time
        deadline = clock() + STREAM_DEADLINE_S
        for client, principal in (
            (restricted, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
            (observer, DEFAULT_SUBJECT_ID),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers["content-type"]) == (201, "application/json")
            location = group.headers["location"]
            group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
            prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(prefix)
            issued = group_id.removeprefix(prefix)
            assert str(uuid.UUID(issued)) == issued
            assert location == f"{API_BASE}/consumer-groups/{group_id}"
            group_body = group.json()
            assert isinstance(group_body, dict)
            _stream_instant(group_body.get("created_at"))
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": principal,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == expected_group
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers["content-type"]) == (
                200, "application/json",
            )
            assert group_read.json() == expected_group
            groups.append(group_id)

            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "worker/1.0.0",
                    "interests": [interest],
                },
            )
            assert (join.status_code, join.headers["content-type"]) == (201, "application/json")
            subscription = _created_subscription_id(join)
            subscriptions.append(subscription)
            body = join.json()
            assert isinstance(body, dict)
            _stream_instant(body.get("created_at"))
            assert body == {
                "id": subscription,
                "consumer_group": group_id,
                "client_agent": "worker/1.0.0",
                "interests": [{
                    **interest, "barrier_mode": "respect", "filter": None, "unmatched_types": [],
                }],
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": body["created_at"],
            }
            assert type(body["topology_version"]) is int
            assert type(body["assigned"][0]["partition"]) is int

            seek = await client.post(
                f"/subscriptions/{subscription}:seek",
                json={
                    "topology_version": body["topology_version"],
                    "positions": {topic: [{"partition": 0, "value": "earliest"}]},
                },
            )
            assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
            positions = seek.json()
            assert positions == {"positions": {topic: [{"partition": 0, "value": 0}]}}
            position, = positions["positions"][topic]
            assert type(position["partition"]) is int
            start = position["value"]
            assert type(start) is int
            starts.append(start)

            response = await stack.enter_async_context(
                client.stream(
                    "GET", "/events:stream", params={"subscription": subscription},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+", response.headers["content-type"],
            )
            reader = MultipartReader(response)
            left = deadline - clock()
            assert left > 0
            topology = await reader.next_frame(timeout=left)
            assert topology == {
                "kind": "topology",
                "topology_version": body["topology_version"],
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            assert type(topology["topology_version"]) is int
            assigned, = topology["assigned"]
            for field in ("partition", "offset", "last_examined"):
                assert type(assigned[field]) is int
            readers.append(reader)

        assert groups[0] != groups[1]
        assert subscriptions[0] != subscriptions[1]
        restricted_reader, observer_reader = readers
        for event in events:
            published = await observer.post("/events", json=event)
            assert (published.status_code, published.content) == (202, b"")

        stored = []
        for _ in events:
            left = deadline - clock()
            assert left > 0
            stored.append(await observer_reader.await_kind("event", timeout=left))
        left = deadline - clock()
        assert left > 0
        first_visible = await restricted_reader.await_kind("event", timeout=left)

    sequences = []
    for frame, event in zip(stored, events, strict=True):
        match frame:
            case {"kind": "event", "payload": {"sequence": int(sequence)} as payload}:
                assert type(sequence) is int
            case unexpected:
                pytest.fail(f"unexpected stored event frame: {unexpected}")
        assert type(payload.get("partition")) is int
        assert _stream_instant(payload.get("occurred_at")) == _stream_instant(event["occurred_at"])
        _stream_instant(payload.get("sequence_time"))
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": None,
                "partition": 0,
                "sequence": sequence,
                "sequence_time": payload["sequence_time"],
            },
        }
        sequences.append(sequence)
    assert max(starts) < sequences[0] < sequences[1]
    assert first_visible == stored[1]
    assert type(first_visible["payload"]["partition"]) is int
    assert type(first_visible["payload"]["sequence"]) is int


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
    tenant_id = OWNER_TENANT
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


@pytest.mark.timeout(ADMISSION_TEST_TIMEOUT_S, func_only=True)
async def test_type_admitted_after_join_and_denied_is_not_delivered(denied_admission_env):
    """scenario: consumer/stream/1.31-negative-type-admitted-after-join-denied.md"""
    topic = TOPIC_SEL_LATE
    interest = {
        "topic": topic,
        "subject_tenant": OWNER_TENANT,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SEL_LATE],
    }
    events = [
        {
            "id": "16e20000-0000-4000-8000-000001770001",
            "type": EVENT_TYPE_SEL_LATE_DENIED,
            "subject_tenant": OWNER_TENANT,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:31Z",
        },
        {
            "id": "16e20000-0000-4000-8000-000001770002",
            "type": EVENT_TYPE_SEL_LATE,
            "subject_tenant": OWNER_TENANT,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-01-19T10:20:32Z",
        },
    ]

    api = _api_for_env(denied_admission_env)
    clock = asyncio.get_running_loop().time
    async with (
        asyncio.timeout(ADMISSION_TEST_DEADLINE_S),
        api(TOKEN_PRINCIPAL_B) as restricted,
        api() as observer,
        AsyncExitStack() as stack,
    ):
        listed = await observer.get("/event-types", params={"$filter": f"topic eq '{topic}'"})
        assert (listed.status_code, listed.headers["content-type"]) == (200, "application/json")
        assert listed.json() == {
            "items": [{
                "id": EVENT_TYPE_SEL_LATE,
                "topic": topic,
                "description": None,
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": "/subject_tenant",
                "data_schema": {"allOf": [
                    {"type": ["object", "null"]}, {"type": ["object", "null"]},
                ]},
            }],
            "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
        }

        groups = []
        subscriptions = []
        readers = []
        starts = []
        topology_deadline = clock() + STREAM_DEADLINE_S
        for client, principal in (
            (restricted, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
            (observer, DEFAULT_SUBJECT_ID),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers["content-type"]) == (201, "application/json")
            location = group.headers["location"]
            group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
            prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(prefix)
            issued = group_id.removeprefix(prefix)
            assert str(uuid.UUID(issued)) == issued
            assert location == f"{API_BASE}/consumer-groups/{group_id}"
            group_body = group.json()
            assert isinstance(group_body, dict)
            _stream_instant(group_body.get("created_at"))
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": OWNER_TENANT,
                "owner_principal": principal,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == expected_group
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers["content-type"]) == (
                200, "application/json",
            )
            assert group_read.json() == expected_group
            groups.append(group_id)

            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "worker/1.0.0",
                    "interests": [interest],
                },
            )
            assert (join.status_code, join.headers["content-type"]) == (201, "application/json")
            subscription = _created_subscription_id(join)
            subscriptions.append(subscription)
            joined = join.json()
            assert isinstance(joined, dict)
            _stream_instant(joined.get("created_at"))
            member_interest = {**interest, "barrier_mode": "respect", "filter": None}
            expected_member = {
                "id": subscription,
                "consumer_group": group_id,
                "client_agent": "worker/1.0.0",
                "interests": [member_interest],
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": joined["created_at"],
            }
            assert joined == {
                **expected_member,
                "interests": [{**member_interest, "unmatched_types": []}],
            }
            assert type(joined["topology_version"]) is int
            assert type(joined["assigned"][0]["partition"]) is int

            member = await client.get(f"/subscriptions/{subscription}")
            assert (member.status_code, member.headers["content-type"]) == (
                200, "application/json",
            )
            member_body = member.json()
            assert member_body == expected_member
            assert type(member_body["topology_version"]) is int
            assert type(member_body["assigned"][0]["partition"]) is int

            seek = await client.post(
                f"/subscriptions/{subscription}:seek",
                json={
                    "topology_version": joined["topology_version"],
                    "positions": {topic: [{"partition": 0, "value": "earliest"}]},
                },
            )
            assert (seek.status_code, seek.headers["content-type"]) == (200, "application/json")
            positions = seek.json()
            assert positions == {"positions": {topic: [{"partition": 0, "value": 0}]}}
            position, = positions["positions"][topic]
            assert type(position["partition"]) is int
            start = position["value"]
            assert type(start) is int
            starts.append(start)

            response = await stack.enter_async_context(
                client.stream(
                    "GET", "/events:stream", params={"subscription": subscription},
                    headers={"Accept": "multipart/mixed"},
                    timeout=httpx.Timeout(REQUEST_TIMEOUT, read=STREAM_DEADLINE_S),
                )
            )
            assert response.status_code == 200
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+", response.headers["content-type"],
            )
            reader = MultipartReader(response)
            remaining = topology_deadline - clock()
            assert remaining > 0, "stream topology deadline expired"
            topology = await reader.next_frame(timeout=remaining)
            assert topology == {
                "kind": "topology",
                "topology_version": joined["topology_version"],
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            assert type(topology["topology_version"]) is int
            assigned, = topology["assigned"]
            for field in ("partition", "offset", "last_examined"):
                assert type(assigned[field]) is int
            readers.append(reader)

        assert groups[0] != groups[1]
        assert subscriptions[0] != subscriptions[1]
        restricted_reader, observer_reader = readers
        await _admit_event_type(
            denied_admission_env,
            _derived_event_type(EVENT_TYPE_SEL_LATE_DENIED, topic, parent=EVENT_TYPE_SEL_LATE),
        )
        for event in events:
            published = await observer.post("/events", json=event)
            assert (published.status_code, published.content) == (202, b"")

        deadline = clock() + STREAM_DEADLINE_S
        stored = []
        for _ in events:
            remaining = deadline - clock()
            assert remaining > 0, "observer delivery deadline expired"
            stored.append(await observer_reader.await_kind("event", timeout=remaining))
        remaining = deadline - clock()
        assert remaining > 0, "restricted sentinel deadline expired"
        first_visible = await restricted_reader.await_kind("event", timeout=remaining)

    sequences = []
    for frame, event in zip(stored, events, strict=True):
        match frame:
            case {"kind": "event", "payload": {"sequence": int(sequence)} as payload}:
                assert type(sequence) is int
            case unexpected:
                pytest.fail(f"unexpected stored event frame: {unexpected}")
        assert type(payload.get("partition")) is int
        assert _stream_instant(payload.get("occurred_at")) == _stream_instant(event["occurred_at"])
        _stream_instant(payload.get("sequence_time"))
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": None,
                "partition": 0,
                "sequence": sequence,
                "sequence_time": payload["sequence_time"],
            },
        }
        sequences.append(sequence)
    assert max(starts) < sequences[0] < sequences[1]
    assert first_visible == stored[1]
    assert type(first_visible["payload"]["partition"]) is int
    assert type(first_visible["payload"]["sequence"]) is int


@pytest.mark.timeout(30, func_only=True)
async def test_base_subject_type_selector_delivers_derived_subjects(api, test_env):
    """scenario: consumer/stream/1.32-positive-base-subject-type-selector.md

    An interest whose ``subject_types`` names a base subject type is delivered
    events about it and about a subject type derived from it, and not about
    another subject type.
    """
    topic = TOPIC_SEL_SUBJECT
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_SUBJECT],
            "subject_types": [SUBJECT_TYPE_SEL_CUSTOMER],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000178{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_MEMBER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_SUPPLIER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[0], stored[1], stored[3]]


@pytest.mark.timeout(30, func_only=True)
async def test_trailing_wildcard_subject_selector_delivers_only_its_prefix(api, test_env):
    """scenario: consumer/stream/1.33-positive-trailing-wildcard-subject-selector.md

    An interest whose ``subject_types`` names a base subject type with a trailing
    ``*`` under one vendor is delivered events about a derived subject type of
    that vendor only: not about the bare base, a derived subject type of
    another vendor, or another subject type.
    """
    topic = TOPIC_SEL_SUBJECT
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_SUBJECT],
            "subject_types": [SUBJECT_TYPE_SEL_CUSTOMER_PREFIX],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000179{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_MEMBER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_SUPPLIER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_PARTNER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_MEMBER),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[]]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[0], stored[4]]


@pytest.mark.timeout(30, func_only=True)
async def test_subject_selector_matching_no_subject_delivers_nothing(api, test_env):
    """scenario: consumer/stream/1.34-positive-subject-selector-matching-no-subject.md

    A JOIN whose ``subject_types`` names a subject type the event type does not
    allow succeeds with nothing in ``unmatched_types``; the stream delivers
    none of the type's events to it. The sentinel comes through a second
    interest that names only the sentinel type.
    """
    topic = TOPIC_SEL_SUBJECT
    target_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_SUBJECT],
            "subject_types": [SUBJECT_TYPE_SEL_AUDITOR],
        },
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_SEL_SUBJECT_SENTINEL],
        },
    ]
    broad_interests = [
        {
            "topic": topic,
            "subject_tenant": DEFAULT_TENANT_ID,
            "ownership_filter": "entire_tenant",
            "types": [ALL_EVENT_TYPES_PATTERN],
        },
    ]
    events = [
        {
            "id": f"16e20000-0000-4000-8000-00000180{index:04d}",
            "type": event_type,
            "subject_tenant": DEFAULT_TENANT_ID,
            "source": "e2e-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": f"2026-01-19T10:20:{30 + index}Z",
        }
        for index, (event_type, subject_type) in enumerate(
            [
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_CUSTOMER),
                (EVENT_TYPE_SEL_SUBJECT, SUBJECT_TYPE_SEL_SUPPLIER),
                (EVENT_TYPE_SEL_SUBJECT_SENTINEL, SUBJECT_TYPE),
            ],
            start=1,
        )
    ]

    async with api() as client, api() as stream_client, AsyncExitStack() as stack:
        groups = []
        cursors = []
        readers = []
        for position, interests, unmatched_types in (
            ("earliest", target_interests, [[], []]),
            ("latest", broad_interests, [[]]),
        ):
            group = await client.post("/consumer-groups")
            assert (group.status_code, group.headers.get("content-type")) == (
                201, "application/json",
            )
            group_prefix = f"{API_BASE}/consumer-groups/"
            group_id = group.headers["location"].removeprefix(group_prefix)
            assert group.headers["location"] == f"{group_prefix}{group_id}"
            gts_prefix = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(gts_prefix)
            group_uuid = group_id.removeprefix(gts_prefix)
            assert str(uuid.UUID(group_uuid)) == group_uuid
            group_read = await client.get(f"/consumer-groups/{group_id}")
            assert (group_read.status_code, group_read.headers.get("content-type")) == (
                200, "application/json",
            )
            group_body = group_read.json()
            _stream_instant(group_body["created_at"])
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": DEFAULT_TENANT_ID,
                "owner_principal": DEFAULT_SUBJECT_ID,
                "description": None,
                "created_at": group_body["created_at"],
            }
            assert group_body == group.json() == expected_group
            join = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": "e2e-test",
                    "interests": interests,
                },
            )
            assert (join.status_code, join.headers.get("content-type")) == (201, "application/json")
            sub_id = _created_subscription_id(join)
            sub_read = await client.get(f"/subscriptions/{sub_id}")
            assert (sub_read.status_code, sub_read.headers.get("content-type")) == (
                200, "application/json",
            )
            sub_body = sub_read.json()
            _stream_instant(sub_body["created_at"])
            expected_interests = [
                {**interest, "barrier_mode": "respect", "filter": None} for interest in interests
            ]
            expected_sub = {
                "id": sub_id,
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": expected_interests,
                "assigned": [{"topic": topic, "partition": 0}],
                "topology_version": 1,
                "created_at": sub_body["created_at"],
            }
            assert sub_body == expected_sub
            assert join.json() == {
                **expected_sub,
                "interests": [
                    {**interest, "unmatched_types": unmatched}
                    for interest, unmatched in zip(expected_interests, unmatched_types, strict=True)
                ],
            }
            seek = await client.post(
                f"/subscriptions/{sub_id}:seek",
                json={
                    "topology_version": 1,
                    "positions": {topic: [{"partition": 0, "value": position}]},
                },
            )
            assert (seek.status_code, seek.headers.get("content-type")) == (200, "application/json")
            seek_body = seek.json()
            positions = seek_body["positions"]
            assert set(positions) == {topic}
            match positions[topic]:
                case [{"partition": 0, "value": int(start)}]:
                    assert type(start) is int
                    assert start >= 0
                case unexpected:
                    pytest.fail(f"unexpected seek positions: {unexpected}")
            if position == "earliest":
                assert start == 0
            assert seek_body == {"positions": {topic: [{"partition": 0, "value": start}]}}
            response = await stack.enter_async_context(
                stream_client.stream(
                    "GET", "/events:stream", params={"subscription": sub_id},
                    headers={"Accept": "multipart/mixed"},
                )
            )
            assert response.status_code == 200, response.status_code
            assert re.fullmatch(
                r"multipart/mixed; boundary=[^\s;]+",
                response.headers["content-type"],
            )
            reader = MultipartReader(response)
            topology = await reader.next_frame(timeout=5)
            assert topology == {
                "kind": "topology",
                "topology_version": 1,
                "assigned": [
                    {"topic": topic, "partition": 0, "offset": start, "last_examined": start}
                ],
            }
            groups.append(group_id)
            cursors.append((sub_id, start))
            readers.append(reader)

        assert groups[0] != groups[1]
        (target_id, target_start), (broad_id, high_water) = cursors
        assert target_id != broad_id
        target, broad = readers
        for event in events:
            publish = await client.post("/events", json=event)
            assert (publish.status_code, publish.content) == (202, b"")

        deadline = asyncio.get_running_loop().time() + STREAM_DEADLINE_S
        sentinel = events[-1]["id"]
        history, delivered = await _read_case_window(target, high_water, sentinel, deadline)
        broad_history, stored = await _read_case_window(broad, high_water, sentinel, deadline)

    assert broad_history == []
    case_ids = {event["id"] for event in events}
    assert all(frame["payload"]["id"] not in case_ids for frame in history)
    for start, frames in ((target_start, history + delivered), (high_water, stored)):
        sequences = [frame["payload"]["sequence"] for frame in frames]
        assert all(type(sequence) is int for sequence in sequences), sequences
        assert sequences == sorted(set(sequences)), sequences
        assert all(sequence > start for sequence in sequences), sequences

    assert [frame["payload"]["id"] for frame in stored] == [event["id"] for event in events]
    for frame, event in zip(stored, events, strict=True):
        payload = frame["payload"]
        assert type(payload["sequence"]) is int
        assert payload["sequence"] > 0
        assert _stream_instant(payload["occurred_at"]) == _stream_instant(event["occurred_at"])
        _stream_instant(payload["sequence_time"])
        assert frame == {
            "kind": "event",
            "payload": {
                **event,
                "occurred_at": payload["occurred_at"],
                "trace_parent": None,
                "data": event.get("data"),
                "partition": 0,
                "sequence": payload["sequence"],
                "sequence_time": payload["sequence_time"],
            },
        }
    assert delivered == [stored[2]]
