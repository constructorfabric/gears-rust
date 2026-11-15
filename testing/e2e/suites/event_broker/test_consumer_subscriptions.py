"""Subscription (JOIN/LEAVE) scenarios (consumer/subscriptions/1.01-1.15, 1.18-1.24).

Each test manages its own consumer group.  Tests that need multiple members
(rebalance tests 1.11, 1.12) use TOPIC_4P (4 partitions) so there are enough
partitions to distribute.  The at-capacity rejection (1.13) uses TOPIC_STREAM
(1 partition): with one member already holding the sole partition, a second
JOIN has nowhere to assign and is refused 429.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timezone

import httpx
import pytest

from .conftest import (
    API_BASE,
    DEFAULT_TENANT_ID,
    SKIP_LIMITS,
    group_seek_positions,
    EVENT_TYPE_4P,
    EVENT_TYPE_STREAM,
    EVENT_TYPE_LONGPOLL,
    SUBJECT_TYPE,
    TOPIC_4P,
    TOPIC_STREAM,
    TOPIC_LONGPOLL,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    XFAIL_AUTH_DISABLED,
    XFAIL_UNLESS_STATIC_TOKENS,
    SseFrameReader,
    TRACE_ID,
)


def _interest(topic: str, event_type: str, tenant_id: str) -> dict:
    return {"topic": topic, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [event_type]}


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


async def test_cold_join_fresh_group_returns_201(api):
    """scenario: consumer/subscriptions/1.01-positive-cold-join-fresh-group.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
    assert sub_resp.status_code == 201
    body = sub_resp.json()
    assert body == {
        "id": body["id"],
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": tenant_id,
                "ownership_filter": "entire_tenant",
                "barrier_mode": body["interests"][0]["barrier_mode"],
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
                "unmatched_types": [],
            }
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": body["created_at"],
    }


async def test_join_unregistered_type_reported(api):
    """scenario: consumer/subscriptions/1.18-positive-join-unregistered-type-reported.md"""
    unregistered = "gts.cf.core.events.event.v1~cf.e2e.event_broker.never_registered.v1~"
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, unregistered, tenant_id)],
            },
        )
    assert sub_resp.status_code == 201, sub_resp.text
    body = sub_resp.json()
    assert body == {
        "id": body["id"],
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": tenant_id,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [unregistered],
                "filter": None,
                "unmatched_types": [unregistered],
            }
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": body["created_at"],
    }


async def test_join_multi_topic_interests_returns_201(api):
    """scenario: consumer/subscriptions/1.02-positive-join-multi-topic-interests.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id),
                    _interest(TOPIC_LONGPOLL, EVENT_TYPE_LONGPOLL, tenant_id),
                ],
            },
        )
    assert sub_resp.status_code == 201
    body = sub_resp.json()
    # Both topics → both partitions assigned (one each).
    assigned_topics = {a["topic"] for a in body["assigned"]}
    assert TOPIC_STREAM in assigned_topics
    assert TOPIC_LONGPOLL in assigned_topics


async def test_join_with_typed_filter_returns_201(api):
    """scenario: consumer/subscriptions/1.03-positive-join-with-typed-filter.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                        "filter": {
                            "engine": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
                            "expression": "event.data.total_cents > 100000",
                        },
                    }
                ],
            },
        )
    assert sub_resp.status_code == 201, sub_resp.text
    body = sub_resp.json()
    interest = body["interests"][0]
    assert interest["filter"] == {
        "engine": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
        "expression": "event.data.total_cents > 100000",
    }


async def test_parallelism_multiple_subscriptions(api, test_env):
    """scenario: consumer/subscriptions/1.04-positive-parallelism-multiple-subscriptions.md

    Two subscriptions in the same group share TOPIC_4P's four partitions.

    The first member's post-rebalance assignment comes from the ``topology``
    frame the broker pushes onto its open stream, which is how a consumer
    actually learns it has lost partitions.  Its JOIN response cannot be used:
    that describes the assignment at join time, when it was the sole member
    holding all four, and it is never revised in place.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub1_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-1",
                "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, tenant_id)],
            },
        )
        assert sub1_resp.status_code == 201
        sub1 = sub1_resp.json()
        sub1_id = str(sub1["id"])

        seek_resp = await client.post(
            f"/subscriptions/{sub1_id}:seek",
            json={
                "topology_version": 1, "positions": group_seek_positions({"topic": a["topic"], "partition": a["partition"], "value": "earliest"} for a in sub1["assigned"])
            },
        )
        assert seek_resp.status_code == 200

    async with httpx.AsyncClient(
        base_url=f"{test_env.base_url}/event-broker/v1", timeout=None
    ) as stream_client:
        async with stream_client.stream(
            "GET", f"/events:sse?subscription={sub1_id}"
        ) as stream_resp:
            assert stream_resp.status_code == 200
            reader = SseFrameReader(stream_resp)

            initial = await reader.await_kind("topology")
            assert len(initial["assigned"]) == 4, "sole member should hold every partition"

            async with api() as client2:
                sub2_resp = await client2.post(
                    "/subscriptions",
                    json={
                        "consumer_group": group_id,
                        "client_agent": "e2e-test-2",
                        "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, tenant_id)],
                    },
                )
            assert sub2_resp.status_code == 201
            sub2 = sub2_resp.json()

            rebalanced = await reader.await_kind("topology")

    assert rebalanced["topology_version"] > initial["topology_version"]
    assert sub2["topology_version"] == rebalanced["topology_version"]

    sub1_partitions = {(a["topic"], a["partition"]) for a in rebalanced["assigned"]}
    sub2_partitions = {(a["topic"], a["partition"]) for a in sub2["assigned"]}
    assert sub1_partitions and sub2_partitions, "each member must hold a partition"
    # Every partition is placed exactly once - the split covers all four and
    # assigns none of them twice.
    assert sub1_partitions | sub2_partitions == {(TOPIC_4P, p) for p in range(4)}
    assert not sub1_partitions & sub2_partitions


async def test_leave_subscription_returns_204(api):
    """scenario: consumer/subscriptions/1.05-positive-leave-subscription.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = sub_resp.json()["id"]

        del_resp = await client.delete(f"/subscriptions/{sub_id}")
    assert del_resp.status_code == 204
    assert del_resp.text == ""


@pytest.mark.xfail(
    reason="auth_disabled: true in standalone; topic authorization not enforced",
    strict=False,
)
async def test_join_unauthorized_topic_returns_403(api):
    """scenario: consumer/subscriptions/1.06-negative-join-unauthorized-topic.md"""
    tenant_id = str(uuid.uuid4())
    unauthorized_topic = (
        "gts.cf.core.events.topic.v1~cf.e2e.event_broker.unauthorized.v1"
    )
    unauthorized_type = (
        "gts.cf.core.events.event.v1~cf.e2e.event_broker.unauthorized.v1~"
    )
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(unauthorized_topic, unauthorized_type, tenant_id)],
            },
        )
    assert sub_resp.status_code == 403
    body = sub_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


async def test_too_many_interests_returns_400(api):
    """scenario: consumer/subscriptions/1.07-negative-join-too-many-interests.md

    More than 64 interests in one JOIN is rejected 400.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)
                    for _ in range(65)
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "TooManyInterests: interests count 65 exceeds the maximum of 64",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "TooManyInterests: interests count 65 exceeds the maximum of 64",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_with_a_nil_interest_tenant_returns_400(api):
    """scenario: consumer/subscriptions/1.20-negative-join-nil-interest-tenant.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(
                        TOPIC_STREAM,
                        EVENT_TYPE_STREAM,
                        "00000000-0000-0000-0000-000000000000",
                    )
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidTenant: interest 0: the interest's tenant must not be the nil UUID",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidTenant: interest 0: the interest's tenant must not be the nil UUID",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_with_too_many_types_returns_400(api):
    """scenario: consumer/subscriptions/1.21-negative-join-too-many-types.md"""
    tenant_id = str(uuid.uuid4())
    too_many_types = [
        f"gts.cf.core.events.event.v1~x.e2e.toomany.t{i}.v1~" for i in range(33)
    ]
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id),
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": too_many_types,
                    },
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "TooManyTypes: interest 1: types count 33 exceeds the maximum of 32",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "TooManyTypes: interest 1: types count 33 exceeds the maximum of 32",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_with_too_many_subject_types_returns_400(api):
    """scenario: consumer/subscriptions/1.22-negative-join-too-many-subject-types.md"""
    tenant_id = str(uuid.uuid4())
    too_many_subject_types = [
        f"gts.cf.e2e.event_broker.toomany_s{i}.v1~" for i in range(33)
    ]
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    _interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id),
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                        "subject_types": too_many_subject_types,
                    },
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "TooManySubjectTypes: interest 1: subject types count 33 exceeds the maximum of 32",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "TooManySubjectTypes: interest 1: subject types count 33 exceeds the maximum of 32",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_only_mine_default(api):
    """scenario: consumer/subscriptions/1.14-positive-join-only-mine.md

    An interest that names neither ``ownership_filter`` nor ``subject_tenant``
    reads the caller's own events: the JOIN echoes ``only_mine`` and the
    caller's tenant, the default identity's under ``auth_disabled``.
    """
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [{"topic": TOPIC_STREAM, "types": [EVENT_TYPE_STREAM]}],
            },
        )
    assert sub_resp.status_code == 201, sub_resp.text
    body = sub_resp.json()
    assert body == {
        "id": body["id"],
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "ownership_filter": "only_mine",
                "barrier_mode": "respect",
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
                "unmatched_types": [],
            }
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": body["created_at"],
    }


async def test_join_only_mine_foreign_tenant(api):
    """scenario: consumer/subscriptions/1.19-negative-join-only-mine-foreign-tenant.md

    ``only_mine`` reads only the caller's own tenant, so an interest that keeps
    the default filter and names another tenant is refused 400, and no
    subscription is created.
    """
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": str(uuid.uuid4()),
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidTenant: interest 0: only_mine reads the caller's own tenant",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidTenant: interest 0: only_mine reads the caller's own tenant",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_join_max_depth_rejected(api):
    """scenario: consumer/subscriptions/1.15-negative-join-max-depth-rejected.md

    ``max_depth`` is not an interest field: a JOIN that sends it is refused
    400, naming the field and its replacement, and no subscription is
    created.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "max_depth": 0,
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 400
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidBody: 'max_depth' is not supported; use 'ownership_filter'",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidBody: 'max_depth' is not supported; use 'ownership_filter'",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


@XFAIL_AUTH_DISABLED
async def test_join_subject_type_not_authorized(api):
    """scenario: consumer/subscriptions/1.24-negative-join-subject-type-not-authorized.md

    The interest selects an exact subject type the consume scope excludes, on
    a topic with a registered event type, so the JOIN is refused
    ``SubjectTypeNotAuthorized``, naming the subject type. Under
    ``auth_disabled`` the scope has no subject-type rule, so the JOIN answers
    201.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                        "subject_types": [SUBJECT_TYPE],
                    },
                ],
            },
        )

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert sub_resp.status_code == 403
    assert sub_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "reason": f"calling principal lacks consume on subject type '{SUBJECT_TYPE}'",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_leave_unknown_subscription_returns_404(api):
    """scenario: consumer/subscriptions/1.08-negative-leave-unknown-subscription.md"""
    fake_sub_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.delete(f"/subscriptions/{fake_sub_id}")
    assert resp.status_code == 404
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "resource_name": fake_sub_id,
        },
    }


async def test_list_subscriptions_returns_paged_list(api):
    """scenario: consumer/subscriptions/1.09-positive-list-subscriptions.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
        assert sub_resp.status_code == 201
        created = sub_resp.json()

        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    assert list_resp.status_code == 200
    # The listing echoes the subscription without the JOIN-only `unmatched_types`.
    assert list_resp.json() == {
        "items": [
            {
                "id": created["id"],
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "barrier_mode": "respect",
                        "types": [EVENT_TYPE_STREAM],
                        "filter": None,
                    }
                ],
                "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
                "topology_version": 1,
                "created_at": created["created_at"],
            }
        ],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


async def test_read_subscription_returns_200(api):
    """scenario: consumer/subscriptions/1.10-positive-read-subscription.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
        assert sub_resp.status_code == 201
        sub_id = sub_resp.json()["id"]

        get_resp = await client.get(f"/subscriptions/{sub_id}")
    assert get_resp.status_code == 200
    body = get_resp.json()
    assert body == {
        "id": str(sub_id),
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [
            {
                "topic": TOPIC_STREAM,
                "subject_tenant": tenant_id,
                "ownership_filter": "entire_tenant",
                "barrier_mode": body["interests"][0]["barrier_mode"],
                "types": [EVENT_TYPE_STREAM],
                "filter": None,
            }
        ],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": body["created_at"],
    }


async def test_second_join_triggers_rebalance(api):
    """scenario: consumer/subscriptions/1.11-positive-second-join-triggers-rebalance.md

    With TOPIC_4P (4 partitions) and one existing member holding all 4, a second
    JOIN causes the broker to rebalance: each member should receive 2 partitions.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub1_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-1",
                "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, tenant_id)],
            },
        )
        assert sub1_resp.status_code == 201
        assert len(sub1_resp.json()["assigned"]) == 4  # first member holds all

        sub2_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-2",
                "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, tenant_id)],
            },
        )
        assert sub2_resp.status_code == 201
        sub2 = sub2_resp.json()

        # After rebalance, each member holds 2 partitions.
        sub1_after = await client.get(f"/subscriptions/{sub1_resp.json()['id']}")
        assert sub1_after.status_code == 200

    sub1_assigned = len(sub1_after.json()["assigned"])
    sub2_assigned = len(sub2["assigned"])
    assert sub1_assigned + sub2_assigned == 4
    assert sub1_assigned == 2
    assert sub2_assigned == 2


async def test_third_join_triggers_rebalance(api):
    """scenario: consumer/subscriptions/1.12-positive-third-join-triggers-rebalance.md

    Three members in a 4-partition group: one holds 2, two hold 1 each (or
    similar split; the broker balances as evenly as possible).
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        subs = []
        for i in range(3):
            sub_resp = await client.post(
                "/subscriptions",
                json={
                    "consumer_group": group_id,
                    "client_agent": f"e2e-test-{i + 1}",
                    "interests": [_interest(TOPIC_4P, EVENT_TYPE_4P, tenant_id)],
                },
            )
            assert sub_resp.status_code == 201
            subs.append(sub_resp.json())

        # Re-read all subscriptions after the final rebalance.
        assigned = []
        for s in subs:
            get = await client.get(f"/subscriptions/{s['id']}")
            assert get.status_code == 200
            assigned.append(len(get.json()["assigned"]))

    assert sum(assigned) == 4
    assert all(n >= 1 for n in assigned)


@SKIP_LIMITS
async def test_join_group_at_capacity_returns_429(api):
    """scenario: consumer/subscriptions/1.13-negative-join-group-at-capacity.md

    TOPIC_STREAM has 1 partition.  One member joins first and holds the only
    partition.  A second JOIN has no partition to assign and is refused 429
    GroupAtCapacity.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub1_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-1",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
        assert sub1_resp.status_code == 201
        assert len(sub1_resp.json()["assigned"]) == 1

        sub2_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test-2",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, tenant_id)],
            },
        )
    assert sub2_resp.status_code == 429
    body = sub2_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
        "title": "Resource Exhausted",
        "status": 429,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }
    assert "retry-after" in {k.lower() for k in sub2_resp.headers}


@XFAIL_AUTH_DISABLED
async def test_cross_tenant_join_anonymous_group_returns_403(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    Tenant A creates an anonymous group; tenant B's POST /subscriptions
    targeting that group must return 403.  In standalone mode auth is disabled
    so the server returns 201 - hence strict xfail.
    """
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

        # In a real deployment this request would carry a different tenant's
        # bearer token.  The group's owner_tenant_id would not match the caller's
        # tenant and the server would reject the JOIN with 403.
        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )
    assert sub_resp.status_code == 403
    body = sub_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


@XFAIL_UNLESS_STATIC_TOKENS
async def test_leave_subscription_not_owned(api):
    """scenario: consumer/subscriptions/1.23-negative-leave-subscription-not-owned.md

    Principal A JOINs a fresh group with one interest on its own tenant (the
    tenant its group records); principal B, a subject of the same tenant,
    LEAVEs A's subscription and is refused ``SubscriptionNotOwned``, and A
    still reads it. Under ``auth_disabled`` the gateway ignores both tokens and
    injects one default identity, so B's LEAVE answers 204.
    """
    principal_a = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_A}"}
    principal_b = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_B}"}
    async with api() as client:
        group_resp = await client.post("/consumer-groups", headers=principal_a)
        assert group_resp.status_code == 201, group_resp.text
        group = group_resp.json()
        sub_resp = await client.post(
            "/subscriptions",
            headers=principal_a,
            json={
                "consumer_group": group["id"],
                "client_agent": "e2e-test",
                "interests": [_interest(TOPIC_STREAM, EVENT_TYPE_STREAM, group["tenant"])],
            },
        )
        assert sub_resp.status_code == 201, sub_resp.text
        sub_id = str(sub_resp.json()["id"])

        leave_resp = await client.delete(f"/subscriptions/{sub_id}", headers=principal_b)
        read_resp = await client.get(f"/subscriptions/{sub_id}", headers=principal_a)

    assert leave_resp.status_code == 403, leave_resp.text
    assert leave_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}/subscriptions/{sub_id}",
        "trace_id": TRACE_ID,
        "context": {
            "reason": f"subscription '{sub_id}' was joined by another principal",
            "resource_type": "gts.cf.core.events.subscription.v1~",
        },
    }
    assert read_resp.status_code == 200, read_resp.text
