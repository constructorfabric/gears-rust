"""Auth & permissions scenarios (auth/1.01-1.13).

The standalone config ships with ``auth_disabled: true`` under
``api-gateway``, so bearer-token validation is not active and every auth
boundary is invisible.  Every test that expects a refusal is marked xfail and
records the correct expected behavior for when the config is wired with a real
authn stack.  The one positive test (auth/1.07) runs: the policy allows every
tenant, so its 202 holds either way.
"""

from __future__ import annotations

import uuid

import httpx
import pytest

from .conftest import (
    API_BASE,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    TOKEN_NIL_SUBJECT,
    TOKEN_NIL_TENANT,
    TOPIC_STREAM,
    TRACE_ID,
    XFAIL_AUTH_DISABLED,
    XFAIL_UNLESS_STATIC_TOKENS,
)

_AUTH_DISABLED_REASON = (
    "standalone config sets auth_disabled: true; "
    "bearer-token validation is not active"
)


@pytest.mark.xfail(reason=_AUTH_DISABLED_REASON, strict=False)
async def test_missing_bearer_token_returns_401(api):
    """scenario: auth/1.01-negative-missing-bearer-token.md"""
    async with api() as client:
        resp = await client.get(
            "/consumer-groups",
            headers={"Authorization": ""},  # explicitly empty
        )
    assert resp.status_code == 401
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
        "title": "Unauthenticated",
        "status": 401,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


@pytest.mark.xfail(reason=_AUTH_DISABLED_REASON, strict=False)
async def test_invalid_bearer_token_returns_401(api):
    """scenario: auth/1.02-negative-invalid-bearer-token.md"""
    async with api() as client:
        resp = await client.get(
            "/consumer-groups",
            headers={"Authorization": "Bearer this-is-not-a-valid-jwt"},
        )
    assert resp.status_code == 401
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
        "title": "Unauthenticated",
        "status": 401,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


@pytest.mark.xfail(reason=_AUTH_DISABLED_REASON, strict=False)
async def test_no_produce_permission_returns_403(api):
    """scenario: auth/1.03-negative-insufficient-permission-produce.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                "id": str(uuid.uuid4()),
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": tenant_id,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-01-01T00:00:00Z",
            },
            # A token with no topic:produce grant would be supplied here.
        )
    assert resp.status_code == 403
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


@pytest.mark.xfail(reason=_AUTH_DISABLED_REASON, strict=False)
async def test_no_consume_permission_returns_403(api):
    """scenario: auth/1.04-negative-insufficient-permission-consume.md"""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        tenant_id = str(uuid.uuid4())
        resp = await client.post(
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
            # A token with no consume grant at all would be supplied here.
        )
    assert resp.status_code == 403
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "reason": body["context"]["reason"],
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
        },
    }


@pytest.mark.xfail(reason=_AUTH_DISABLED_REASON, strict=False)
async def test_cross_tenant_anonymous_group_returns_403(api):
    """scenario: auth/1.05-negative-cross-tenant-anonymous-group.md

    Tenant B's principal attempts to JOIN a group minted by tenant A.  The
    broker rejects with 403 because the group's ``tenant_id`` (from A's
    SecurityContext at creation time) does not match B's.
    """
    async with api() as client:
        # Tenant A creates the group.
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        # Tenant B (different token) tries to JOIN it.
        tenant_b_id = str(uuid.uuid4())
        resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": tenant_b_id,
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
            # Tenant B's bearer token would be supplied here.
        )
    assert resp.status_code == 403
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": body["context"],
    }


@XFAIL_AUTH_DISABLED
async def test_publish_to_a_tenant_outside_the_subtree_returns_403(api):
    """scenario: auth/1.06-negative-tenant-outside-subtree.md

    The event's tenant is a fresh one, outside the caller's subtree, and the
    caller holds no barrier right: the tenant check and the barrier right are
    both denied, so the publish is refused ``TenantIdNotAuthorized``.
    """
    outside_tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                "id": str(uuid.uuid4()),
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": outside_tenant_id,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-01-01T00:00:00Z",
            },
        )
    assert resp.status_code == 403
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}/events",
        "trace_id": TRACE_ID,
        "context": {
            "reason": "access denied by policy",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_publish_across_a_barrier_with_the_grant_returns_202(api):
    """scenario: auth/1.07-positive-publish-across-barrier-with-grant.md

    Under ``auth_disabled`` the policy allows every tenant, so this proves the
    202 only. The grant decision itself (the tenant check denied, then the
    barrier right allowed, then the type check) is proven by the Rust tests in
    ``gears/system/event-broker/event-broker/src/domain/publish_authz_tests.rs``.
    """
    barrier_tenant_id = str(uuid.uuid4())
    async with api() as client:
        resp = await client.post(
            "/events",
            json={
                "id": str(uuid.uuid4()),
                "type": EVENT_TYPE_STREAM,
                "subject_tenant": barrier_tenant_id,
                "source": "e2e-test",
                "subject": "s1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-01-01T00:00:00Z",
            },
        )
    assert resp.status_code == 202
    assert resp.text == "", "202 Accepted must carry no body"


async def _join_refused(api, interest: dict) -> tuple[httpx.Response, httpx.Response]:
    """JOINs a fresh group with one interest; returns the JOIN response and the
    group's subscription listing read after it."""
    async with api() as client:
        group_resp = await client.post("/consumer-groups")
        assert group_resp.status_code == 201
        group_id = group_resp.json()["id"]

        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group_id,
                "client_agent": "e2e-test",
                "interests": [interest],
            },
        )
        list_resp = await client.get(
            "/subscriptions", params={"$filter": f"consumer_group eq '{group_id}'"}
        )
    return sub_resp, list_resp


@XFAIL_AUTH_DISABLED
async def test_join_tenant_outside_subtree(api):
    """scenario: auth/1.08-negative-join-tenant-outside-subtree.md

    The interest's tenant is a fresh one, outside the caller's subtree: the
    consume call on the topic and the tenant check are both denied, so the
    JOIN is refused ``TenantIdNotAuthorized``. Under ``auth_disabled`` the
    policy allows every tenant, so the JOIN answers 201.
    """
    sub_resp, list_resp = await _join_refused(
        api,
        {
            "topic": TOPIC_STREAM,
            "subject_tenant": str(uuid.uuid4()),
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_STREAM],
        },
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
            "reason": "access denied by policy",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


@XFAIL_AUTH_DISABLED
async def test_join_barrier_ignore_without_grant(api):
    """scenario: auth/1.09-negative-join-barrier-ignore-without-grant.md

    The interest asks ``barrier_mode: ignore`` and the caller holds no
    ``consume_ignore_barrier`` on the topic, so the JOIN is refused
    ``TenantIdNotAuthorized`` before the consume call. Under ``auth_disabled``
    the policy allows the barrier right, so the JOIN answers 201.
    """
    sub_resp, list_resp = await _join_refused(
        api,
        {
            "topic": TOPIC_STREAM,
            "subject_tenant": str(uuid.uuid4()),
            "ownership_filter": "entire_tenant",
            "barrier_mode": "ignore",
            "types": [EVENT_TYPE_STREAM],
        },
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
            "reason": "access denied by policy",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


@XFAIL_AUTH_DISABLED
async def test_join_event_type_excluded_by_scope(api):
    """scenario: auth/1.10-negative-join-event-type-excluded-by-scope.md

    The interest asks for an exact type registered on the topic, and the
    consume scope the policy answers admits none of its types, so the JOIN is
    refused ``EventTypeNotAuthorized``, naming the pattern. Under
    ``auth_disabled`` the scope has no type rule, so the JOIN answers 201.
    """
    sub_resp, list_resp = await _join_refused(
        api,
        {
            "topic": TOPIC_STREAM,
            "subject_tenant": str(uuid.uuid4()),
            "ownership_filter": "entire_tenant",
            "types": [EVENT_TYPE_STREAM],
        },
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
            "reason": f"calling principal lacks consume on event type '{EVENT_TYPE_STREAM}'",
            "resource_type": "gts.cf.core.events.event_type.v1~",
        },
    }
    assert list_resp.status_code == 200
    assert list_resp.json() == {
        "items": [],
        "page_info": {"next_cursor": None, "prev_cursor": None, "limit": 25},
    }


def _refused_with_no_identity(path: str) -> dict:
    """The broker's 403 for a caller with no identity, at ``path``."""
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}{path}",
        "trace_id": TRACE_ID,
        "context": {
            "reason": "the caller has no identity: its subject or tenant is the nil UUID",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


@XFAIL_UNLESS_STATIC_TOKENS
async def test_management_by_a_nil_caller_is_refused(api):
    """scenario: auth/1.11-negative-nil-caller-management.md

    A caller whose static token binds the nil subject lists subscriptions and
    consumer groups; each list answers 403 instead of an empty page. Under
    ``auth_disabled`` the gateway ignores the token and injects its default
    identity, so both lists answer 200.
    """
    headers = {"Authorization": f"Bearer {TOKEN_NIL_SUBJECT}"}
    async with api() as client:
        subscriptions = await client.get("/subscriptions", headers=headers)
    async with api() as client:
        groups = await client.get("/consumer-groups", headers=headers)

    assert subscriptions.status_code == 403, subscriptions.text
    assert subscriptions.json() == _refused_with_no_identity("/subscriptions")
    assert groups.status_code == 403, groups.text
    assert groups.json() == _refused_with_no_identity("/consumer-groups")


def _refused_by_policy(path: str, resource_type: str) -> dict:
    """The broker's 403 for a request the policy refuses, at ``path``."""
    return {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}{path}",
        "trace_id": TRACE_ID,
        "context": {
            "reason": "access denied by policy",
            "resource_type": resource_type,
        },
    }


@XFAIL_UNLESS_STATIC_TOKENS
async def test_segments_for_a_topic_the_caller_may_not_read_return_403_before_404(api):
    """scenario: auth/1.12-negative-segments-topic-not-authorized.md

    A caller whose static token binds the nil tenant asks for the segments of
    a registered topic and of an unregistered one; the policy refuses it both,
    so each answers 403 ``TopicNotAuthorized`` and the unregistered topic does
    not answer 404. Under ``auth_disabled`` the gateway ignores the token and
    injects its default identity, so the registered topic answers 200 and the
    unregistered one 404.
    """
    headers = {"Authorization": f"Bearer {TOKEN_NIL_TENANT}"}
    unregistered = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.unregistered.v1"
    async with api() as client:
        registered_resp = await client.get(
            "/topics/segments",
            params={"topic": TOPIC_STREAM, "partition": 0},
            headers=headers,
        )
    async with api() as client:
        unregistered_resp = await client.get(
            "/topics/segments",
            params={"topic": unregistered, "partition": 0},
            headers=headers,
        )

    refused = _refused_by_policy("/topics/segments", "gts.cf.core.events.topic.v1~")
    assert registered_resp.status_code == 403, registered_resp.text
    assert registered_resp.json() == refused
    assert unregistered_resp.status_code == 403, unregistered_resp.text
    assert unregistered_resp.json() == refused


@XFAIL_UNLESS_STATIC_TOKENS
async def test_producer_registration_the_tenant_check_refuses_returns_403(api):
    """scenario: auth/1.13-negative-producer-registration-tenant-not-authorized.md

    A caller whose static token binds the nil tenant registers a producer; the
    policy refuses its tenant check, so the registration answers 403
    ``TenantIdNotAuthorized``. Under ``auth_disabled`` the gateway ignores the
    token and injects its default identity, so the registration answers 201.
    """
    headers = {"Authorization": f"Bearer {TOKEN_NIL_TENANT}"}
    async with api() as client:
        resp = await client.post(
            "/producers",
            json={"mode": "chained", "client_agent": "e2e-nil-tenant"},
            headers=headers,
        )

    assert resp.status_code == 403, resp.text
    assert resp.json() == _refused_by_policy("/producers", "gts.cf.core.events.request.v1~")
