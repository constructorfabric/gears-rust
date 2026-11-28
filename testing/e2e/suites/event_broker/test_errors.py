"""RFC-9457 error envelope scenarios (errors/1.01-1.08).

Each test asserts the full Problem Details envelope. Some cases echo
server-generated detail and instance. The 401, 409, and 412 cases assert
exact values for both fields. Clients send a fixed W3C traceparent, which the
canonical error layer reflects as TRACE_ID.

The unauthenticated case (1.02) runs as an ordinary test against the mounted
route. The permission-denied case (1.03) carries an xfail. The 500 Internal
case (1.08) is xfail because it requires fault injection.
"""

from __future__ import annotations

import re
import uuid
from datetime import datetime, timezone

import pytest

from .conftest import (
    API_BASE,
    DEFAULT_TENANT_ID,
    DEFAULT_SUBJECT_ID,
    EVENT_TYPE_SEL_PREFIX_PATTERN,
    EVENT_TYPE_STREAM,
    SKIP_LIMITS,
    SUBJECT_TYPE,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TOPIC_SEL_PREFIX,
    TOPIC_STREAM,
    TRACE_ID,
)
from .test_consumer_subscriptions import _created_subscription_id, _subscription_created_at

async def test_problem_details_envelope_shape(api):
    """scenario: errors/1.01-positive-problem-details-envelope.md"""
    group_id = "gts.cf.core.events.consumer_group.v1~deadbeef-0000-0000-0000-000000000000"
    async with api() as client:
        resp = await client.get(f"/consumer-groups/{group_id}")
    assert resp.status_code == 404
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": f"consumer group '{group_id}' is not registered",
        "instance": f"{API_BASE}/consumer-groups/{group_id}",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
            "resource_name": group_id,
        },
    }


async def test_401_unauthenticated_envelope(api):
    """scenario: errors/1.02-negative-401-unauthenticated.md"""
    async with api(None) as client:
        request = client.build_request(
            "POST", "/consumer-groups", json={"description": "no token"}
        )
        assert request.url.path == "/event-broker/v1/consumer-groups"
        assert request.headers["content-type"] == "application/json"
        assert request.headers["traceparent"] == (
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        )
        assert "authorization" not in request.headers
        assert "x-toolkit-internal-token" not in request.headers
        resp = await client.send(request)
    assert resp.status_code == 401
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
        "title": "Unauthenticated",
        "status": 401,
        "detail": "Authentication required",
        "instance": "/event-broker/v1/consumer-groups",
        "trace_id": TRACE_ID,
        "context": {"reason": "MISSING_BEARER"},
    }


async def test_403_permission_denied_envelope(_action_grants_api):
    """scenario: errors/1.03-negative-403-unauthorized.md"""
    tenant_id = DEFAULT_TENANT_ID
    caller_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
    interest = {
        "topic": TOPIC_SEL_PREFIX,
        "subject_tenant": tenant_id,
        "ownership_filter": "entire_tenant",
        "types": [EVENT_TYPE_SEL_PREFIX_PATTERN],
    }
    async with (
        _action_grants_api(TOKEN_PRINCIPAL_B) as caller,
        _action_grants_api(TOKEN_PRINCIPAL_A) as observer,
    ):
        groups = []
        for creator, owner_id in ((caller, caller_id), (observer, DEFAULT_SUBJECT_ID)):
            created = await creator.post("/consumer-groups")
            assert (created.status_code, created.headers["content-type"]) == (
                201, "application/json",
            )
            location = created.headers["location"]
            group_id = location.removeprefix(f"{API_BASE}/consumer-groups/")
            group_family = "gts.cf.core.events.consumer_group.v1~"
            assert group_id.startswith(group_family)
            issued_uuid = group_id.removeprefix(group_family)
            assert str(uuid.UUID(issued_uuid)) == issued_uuid
            assert location == f"{API_BASE}/consumer-groups/{group_id}"
            body = created.json()
            assert isinstance(body, dict)
            created_at = body.get("created_at")
            assert isinstance(created_at, str)
            assert re.fullmatch(
                r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
                created_at,
            )
            datetime.fromisoformat(created_at.replace("Z", "+00:00"))
            expected_group = {
                "id": group_id,
                "kind": "anonymous",
                "tenant": tenant_id,
                "owner_principal": owner_id,
                "description": None,
                "created_at": created_at,
            }
            assert body == expected_group
            groups.append(expected_group)
        target_group, control_group = groups
        group_id = target_group["id"]
        control_group_id = control_group["id"]
        assert group_id != control_group_id
        group_path = f"/consumer-groups/{group_id}"
        group_filter = {"$filter": f"consumer_group eq '{group_id}'"}

        control = await observer.post(
            "/subscriptions",
            json={
                "consumer_group": control_group_id,
                "client_agent": "worker/1.0.0",
                "interests": [interest],
            },
        )
        assert (control.status_code, control.headers["content-type"]) == (201, "application/json")
        control_id = _created_subscription_id(control)
        control_body = control.json()
        assert isinstance(control_body, dict)
        assert isinstance(control_body.get("created_at"), str)
        control_interest = {**interest, "barrier_mode": "respect", "filter": None}
        expected_control = {
            "id": control_id,
            "consumer_group": control_group_id,
            "client_agent": "worker/1.0.0",
            "interests": [control_interest],
            "assigned": [{"topic": interest["topic"], "partition": 0}],
            "topology_version": 1,
            "created_at": _subscription_created_at(control_body),
        }
        assert control_body == {
            **expected_control,
            "interests": [{**control_interest, "unmatched_types": []}],
        }
        assert type(control_body["topology_version"]) is int
        assert type(control_body["assigned"][0]["partition"]) is int

        page_info = {"next_cursor": None, "prev_cursor": None, "limit": 25}

        visible = await caller.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{control_group_id}'"},
        )
        assert (visible.status_code, visible.headers["content-type"]) == (200, "application/json")
        visible_page = visible.json()
        assert visible_page == {"items": [expected_control], "page_info": page_info}
        assert type(visible_page["page_info"]["limit"]) is int
        assert type(visible_page["items"][0]["topology_version"]) is int
        assert type(visible_page["items"][0]["assigned"][0]["partition"]) is int

        before_group = await caller.get(group_path)
        assert (before_group.status_code, before_group.headers["content-type"]) == (
            200, "application/json",
        )
        assert before_group.json() == target_group

        before_members = await caller.get("/subscriptions", params=group_filter)
        assert (before_members.status_code, before_members.headers["content-type"]) == (
            200, "application/json",
        )
        before_page = before_members.json()
        assert before_page == {"items": [], "page_info": page_info}
        assert type(before_page["page_info"]["limit"]) is int

        refused = await caller.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "worker/1.0.0",
                "interests": [interest],
            },
        )
        assert (refused.request.method, refused.request.url.path) == (
            "POST", f"{API_BASE}/subscriptions",
        )
        assert (refused.status_code, refused.headers["content-type"]) == (
            403, "application/problem+json",
        )
        problem = refused.json()
        assert problem == {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": f"{API_BASE}/subscriptions",
            "trace_id": TRACE_ID,
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        }
        assert type(problem["status"]) is int

        after_members = await caller.get("/subscriptions", params=group_filter)
        assert (after_members.status_code, after_members.headers["content-type"]) == (
            200, "application/json",
        )
        after_page = after_members.json()
        assert after_page == {"items": [], "page_info": page_info}
        assert type(after_page["page_info"]["limit"]) is int

        after_group = await caller.get(group_path)
        assert (after_group.status_code, after_group.headers["content-type"]) == (
            200, "application/json",
        )
        assert after_group.json() == target_group


async def test_404_not_found_envelope(api):
    """scenario: errors/1.04-negative-404-not-found.md"""
    sub_id = "99999999-8888-7777-6666-555555555555"
    async with api() as client:
        resp = await client.get(
            "/events:stream",
            params={"subscription": sub_id},
            headers={"Accept": "multipart/mixed"},
        )
    assert resp.status_code == 404
    assert resp.headers["content-type"] == "application/problem+json"
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": f"subscription '{sub_id}' does not exist",
        "instance": f"{API_BASE}/events:stream",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "resource_name": sub_id,
        },
    }


async def test_409_failed_precondition_envelope(api):
    """scenario: errors/1.05-negative-409-conflict.md

    Opening a stream before SEEKing produces a 409 PositionsNotSet, which is
    the canonical FailedPrecondition error.  The scenario file's status field
    reflects the implementation's 409 HTTP override (the category default is
    400 but ``error.rs`` applies ``Http::status_code(409)`` for this code).
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
                        "subject_tenant": DEFAULT_TENANT_ID,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )
        assert sub_resp.status_code == 201
        assert sub_resp.headers["content-type"] == "application/json"
        location = sub_resp.headers["location"]
        sub_id = location.removeprefix(f"{API_BASE}/subscriptions/")
        assert str(uuid.UUID(sub_id)) == sub_id
        assert location == f"{API_BASE}/subscriptions/{sub_id}"
        read_resp = await client.get(f"/subscriptions/{sub_id}")

        stream_resp = await client.get(
            f"/events:stream?subscription={sub_id}",
            headers={"Accept": "multipart/mixed"},
        )
    assert read_resp.status_code == 200
    assert read_resp.headers["content-type"] == "application/json"
    read_body = read_resp.json()
    created_at = read_body["created_at"]
    assert re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z",
        created_at,
    )
    datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    interest = {
        "topic": TOPIC_STREAM,
        "subject_tenant": DEFAULT_TENANT_ID,
        "ownership_filter": "entire_tenant",
        "barrier_mode": "respect",
        "types": [EVENT_TYPE_STREAM],
        "filter": None,
    }
    expected = {
        "id": sub_id,
        "consumer_group": group_id,
        "client_agent": "e2e-test",
        "interests": [interest],
        "assigned": [{"topic": TOPIC_STREAM, "partition": 0}],
        "topology_version": 1,
        "created_at": created_at,
    }
    assert read_body == expected
    assert sub_resp.json() == {**expected, "interests": [{**interest, "unmatched_types": []}]}
    assert stream_resp.status_code == 409
    assert stream_resp.headers["content-type"] == "application/problem+json"
    body = stream_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 409,
        "detail": "Operation precondition not met",
        "instance": f"{API_BASE}/events:stream",
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.subscription.v1~",
            "violations": [
                {
                    "type": "positions_not_set",
                    "subject": f"{TOPIC_STREAM}:0",
                    "description": (
                        f"call POST /v1/subscriptions/{sub_id}:seek "
                        "before re-opening the stream"
                    ),
                }
            ]
        },
    }


async def test_412_sequence_violation_envelope(api):
    """scenario: errors/1.06-negative-412-sequence-violation.md

    A chained publish that neither advances the chain (its ``meta.previous``
    is not the broker's stored ``last_sequence``) nor resends the chain head
    gets a 412.
    """
    async with api() as client:
        reg_resp = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-test"},
        )
        assert reg_resp.status_code == 201
        assert reg_resp.headers["content-type"] == "application/json"
        location = reg_resp.headers["location"]
        producer_id = location.removeprefix(f"{API_BASE}/producers/")
        assert str(uuid.UUID(producer_id)) == producer_id
        assert location == f"{API_BASE}/producers/{producer_id}"
        assert reg_resp.json() == {
            "id": producer_id,
            "mode": "chained",
            "client_agent": "e2e-test",
        }

        first_resp = await client.post(
            "/events",
            json={
                "id": "b3468dd8-2345-4d74-a47c-7bf7013c0112",
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-01-01T00:00:00Z",
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 0,
                    "sequence": 1,
                },
            },
        )
        assert first_resp.status_code == 202
        assert first_resp.content == b""

        resp = await client.post(
            "/events",
            json={
                "id": "74d953a8-c68e-43a8-acc8-a1efafe60112",
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": DEFAULT_TENANT_ID,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-01-01T00:00:00Z",
                "meta": {
                    "version": 1,
                    "producer": producer_id,
                    "previous": 999,
                    "sequence": 1000,
                },
            },
        )
        cursors = await client.get(f"/producers/{producer_id}/cursors")
    assert resp.status_code == 412
    assert resp.headers["content-type"] == "application/problem+json"
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 412,
        "detail": "Operation precondition not met",
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "violations": [
                {
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": (
                        f"index=0 producer={producer_id} topic={TOPIC_STREAM} last_sequence=1"
                    ),
                }
            ],
            "resource_type": "gts.cf.core.events.topic.v1~",
            "resource_name": TOPIC_STREAM,
        },
    }

    assert cursors.status_code == 200
    assert cursors.headers["content-type"] == "application/json"
    assert cursors.json() == {
        "id": producer_id,
        "client_agent": "e2e-test",
        "topics": [{"topic": TOPIC_STREAM, "last_sequence": 1}],
    }


@SKIP_LIMITS
async def test_429_rate_limited_envelope(api):
    """scenario: errors/1.07-negative-429-rate-limited.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = None
        # Flood until we get 429 or exhaust attempts.
        for _ in range(200):
            resp = await client.post(
                "/events",
                json={
                    "id": str(uuid.uuid4()),
                    "type": EVENT_TYPE_STREAM,
                    "subject_tenant": tenant_id,
                    "source": "e2e-test",
                    "subject": "s1",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": datetime.now(timezone.utc).isoformat(),
                },
            )
            if resp.status_code == 429:
                break
    assert resp is not None
    assert resp.status_code == 429
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
        "title": "Resource Exhausted",
        "status": 429,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }
    assert "retry-after" in {k.lower() for k in resp.headers}


@pytest.mark.xfail(
    reason="triggering a genuine 500 requires fault injection not available in E2E",
    strict=False,
)
async def test_500_internal_error_envelope(api):
    """scenario: errors/1.08-negative-500-internal.md"""
    # No reliable way to force an internal error from the outside in E2E;
    # this test documents the expected shape for when one does occur.
    async with api() as client:
        resp = await client.get("/admin/force-internal-error")  # hypothetical endpoint
    assert resp.status_code == 500
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        "title": "Internal",
        "status": 500,
        "detail": "An internal error occurred. Please retry later.",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }
