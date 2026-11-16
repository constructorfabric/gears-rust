"""Consumer group CRUD scenarios (consumer/groups/1.01-1.10, consumer/subscriptions/1.16-1.17).

Each test creates its own group to avoid cross-test interference.
``groups/1.08`` (named group JOIN) is xfail: the gear does not implement
named consumer groups yet, and a JOIN naming one answers 501.
``groups/1.09`` (cross-tenant ownership) tests are xfail: the standalone
config sets ``auth_disabled: true``, so tenant isolation cannot be exercised.
``groups/1.10`` and ``subscriptions/1.16`` (creator-only DELETE and JOIN) send two
static tokens and are strict xfail under ``auth_disabled``.
``subscriptions/1.17`` (JOIN without ``consume`` on the group) needs an
action-aware policy, which no config of this suite runs, so it is strict xfail.
``groups/1.06`` validation tests (client_agent length and grammar, description
encoding) run live; the server enforces each rule.
"""

from __future__ import annotations

import uuid

import pytest

from .conftest import (
    API_BASE,
    EVENT_TYPE_STREAM,
    SUBJECT_TYPE,
    TOKEN_PRINCIPAL_A,
    TOKEN_PRINCIPAL_B,
    TOPIC_STREAM,
    TRACE_ID,
    XFAIL_AUTH_DISABLED,
    XFAIL_UNLESS_STATIC_TOKENS,
)


async def test_create_anonymous_group_returns_201(api):
    """scenario: consumer/groups/1.01-positive-create-anonymous-group.md"""
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "order-fulfilment workers"},
        )
    assert resp.status_code == 201
    body = resp.json()
    assert body == {
        "id": body["id"],
        "kind": "anonymous",
        "tenant": body["tenant"],
        "owner_principal": body["owner_principal"],
        "description": "order-fulfilment workers",
        "created_at": body["created_at"],
    }
    assert body["id"].startswith("gts.cf.core.events.consumer_group.v1~")
    assert "location" in {k.lower() for k in resp.headers}


async def test_create_anonymous_group_no_body_returns_201(api):
    """scenario: consumer/groups/1.01 - the request body is optional."""
    async with api() as client:
        resp = await client.post("/consumer-groups")
    assert resp.status_code == 201
    body = resp.json()
    assert body["id"].startswith("gts.cf.core.events.consumer_group.v1~")
    assert body["kind"] == "anonymous"
    assert body["description"] is None


async def test_get_group_by_id_returns_200(api):
    """scenario: consumer/groups/1.02-positive-get-group-by-id.md"""
    async with api() as client:
        create_resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "test group"},
        )
        assert create_resp.status_code == 201
        created = create_resp.json()
        group_id = created["id"]

        get_resp = await client.get(f"/consumer-groups/{group_id}")
    assert get_resp.status_code == 200
    assert get_resp.json() == created


async def test_list_groups_returns_paged_list(api):
    """scenario: consumer/groups/1.03-positive-list-groups.md"""
    async with api() as client:
        # Create a group so there is at least one to list.
        create_resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "list-test group"},
        )
        assert create_resp.status_code == 201
        created = create_resp.json()
        group_id = created["id"]

        list_resp = await client.get("/consumer-groups")
    assert list_resp.status_code == 200
    body = list_resp.json()
    assert body == {
        "items": body["items"],
        "page_info": body["page_info"],
    }
    assert isinstance(body["items"], list)
    assert isinstance(body["page_info"], dict)

    # The created group must appear in the list with its full shape intact.
    found = next((item for item in body["items"] if item["id"] == group_id), None)
    assert found is not None, f"group {group_id} not found in list"
    assert found == created


async def test_delete_empty_group_returns_204(api):
    """scenario: consumer/groups/1.04-positive-delete-empty-group.md"""
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

        del_resp = await client.delete(f"/consumer-groups/{group_id}")
    assert del_resp.status_code == 204
    assert del_resp.text == ""


async def test_delete_group_with_active_members_returns_409(api):
    """scenario: consumer/groups/1.05-negative-delete-group-with-active-members.md"""
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

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
        assert sub_resp.status_code == 201

        del_resp = await client.delete(f"/consumer-groups/{group_id}")
    assert del_resp.status_code == 409
    body = del_resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
        "title": "Failed Precondition",
        "status": 409,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
            "violations": [
                {
                    "type": "consumer_group_has_active_members",
                    "subject": group_id,
                    "description": body["context"]["violations"][0]["description"],
                }
            ]
        },
    }


async def test_client_agent_too_long_returns_400(api):
    """scenario: consumer/groups/1.06-negative-invalid-client-agent.md

    A ``client_agent`` of 257 bytes (one past the 256-byte cap) is rejected
    with 400.  The error names the field, states the bound and the measured
    length, and carries no part of the submitted value.
    """
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "x" * 257},
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "client_agent",
                    "description": "must be 1-256 bytes, got 257",
                    "reason": "field_too_long",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_client_agent_breaking_the_user_agent_grammar_returns_400(api):
    """scenario: consumer/groups/1.06-negative-invalid-client-agent.md

    A ``client_agent`` that is printable ASCII within 256 bytes but ends in
    ``/`` with no version breaks the RFC 9110 User-Agent grammar and is
    rejected with 400.  The error names the field and the grammar, and
    carries no part of the submitted value.
    """
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "agent/"},
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "client_agent",
                    "description": (
                        "must follow the RFC 9110 User-Agent grammar: "
                        "product *( SP ( product / comment ) )"
                    ),
                    "reason": "user_agent_grammar",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_description_non_ascii_returns_400(api):
    """scenario: consumer/groups/1.06-negative-invalid-client-agent.md

    A ``description`` carrying bytes outside the printable-ASCII range
    (0x20-0x7E) is rejected with 400.  The error names the field and the
    ascii_only rule; it does not reproduce the submitted value.
    """
    async with api() as client:
        resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": "é\x01"},
        )
    assert resp.status_code == 400
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "field_violations": [
                {
                    "field": "description",
                    "description": "must contain only printable ASCII (0x20-0x7E)",
                    "reason": "ascii_only",
                }
            ],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


async def test_get_unknown_group_returns_404(api):
    """scenario: consumer/groups/1.07-negative-get-unknown-group.md"""
    fake_id = "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000"
    async with api() as client:
        resp = await client.get(f"/consumer-groups/{fake_id}")
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
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
            "resource_name": fake_id,
        },
    }


@pytest.mark.xfail(
    reason=(
        "named consumer groups are not implemented: a JOIN naming one answers "
        "501 (domain/local_broker.rs), so seeding one would not make it testable"
    ),
    strict=False,
)
async def test_named_group_join_without_create_step(api):
    """scenario: consumer/groups/1.08-positive-named-group-join.md

    A named consumer group has a well-known GTS identifier registered via
    types_registry at startup.  The consumer JOINs directly without a prior
    POST /consumer-groups.  This test is xfail until the gear implements named
    groups; JOIN currently answers 501 Not Implemented.
    """
    named_group_id = "gts.cf.core.events.consumer_group.v1~cf.e2e.event_broker.named_group.v1"
    tenant_id = str(uuid.uuid4())
    async with api() as client:
        sub_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": named_group_id,
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
    assert sub_resp.status_code == 201


@XFAIL_AUTH_DISABLED
async def test_cross_tenant_get_anonymous_group_returns_403(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    Tenant A creates an anonymous group; tenant B's GET on that group must
    return 403, not 200.  In standalone mode auth is disabled so the server
    returns 200 - hence strict xfail.
    """
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

        # In a real deployment this request would carry a different tenant's
        # bearer token.  Standalone auth is disabled so this is indistinguishable
        # from the owning tenant - the test is here to document the contract.
        get_resp = await client.get(f"/consumer-groups/{group_id}")
    assert get_resp.status_code == 403
    body = get_resp.json()
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
async def test_cross_tenant_delete_anonymous_group_returns_403(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    Tenant A creates an anonymous group; tenant B's DELETE on that group must
    return 403 and leave the group intact.
    """
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

        del_resp = await client.delete(f"/consumer-groups/{group_id}")
    assert del_resp.status_code == 403
    body = del_resp.json()
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
async def test_list_excludes_other_tenant_anonymous_groups(api):
    """scenario: consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md

    Tenant A's anonymous groups must not appear in tenant B's group listing.
    """
    async with api() as client:
        create_resp = await client.post("/consumer-groups")
        assert create_resp.status_code == 201
        group_id = create_resp.json()["id"]

        # Listing as a different tenant: the group created above must be absent.
        list_resp = await client.get("/consumer-groups")
    assert list_resp.status_code == 200
    items = list_resp.json()["items"]
    assert not any(item["id"] == group_id for item in items)


@XFAIL_UNLESS_STATIC_TOKENS
async def test_join_anonymous_group_not_owned(api):
    """scenario: consumer/subscriptions/1.16-negative-join-anonymous-group-not-owned.md

    Principal A creates a fresh group and JOINs it with one interest on its own
    tenant (the tenant its group records); principal B, a subject of the same
    tenant, JOINs A's group and is refused ``ConsumerGroupNotOwned``, and A's
    subscription stays the group's only one. Under ``auth_disabled`` the
    gateway ignores both tokens and injects one default identity, so B's JOIN
    answers 201.
    """
    principal_a = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_A}"}
    principal_b = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_B}"}
    async with api() as client:
        group_resp = await client.post("/consumer-groups", headers=principal_a)
        assert group_resp.status_code == 201, group_resp.text
        group = group_resp.json()
        join_body = {
            "consumer_group": group["id"],
            "client_agent": "e2e-test",
            "interests": [
                {
                    "topic": TOPIC_STREAM,
                    "subject_tenant": group["tenant"],
                    "ownership_filter": "entire_tenant",
                    "types": [EVENT_TYPE_STREAM],
                }
            ],
        }
        sub_resp = await client.post("/subscriptions", headers=principal_a, json=join_body)
        assert sub_resp.status_code == 201, sub_resp.text
        sub_id = str(sub_resp.json()["id"])

        join_resp = await client.post("/subscriptions", headers=principal_b, json=join_body)
        list_resp = await client.get(
            "/subscriptions",
            headers=principal_a,
            params={"$filter": f"consumer_group eq '{group['id']}'"},
        )

    assert join_resp.status_code == 403, join_resp.text
    assert join_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "reason": f"consumer group '{group['id']}' was created by another principal",
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
        },
    }
    assert list_resp.status_code == 200, list_resp.text
    assert [str(item["id"]) for item in list_resp.json()["items"]] == [sub_id]


@XFAIL_AUTH_DISABLED
async def test_join_consumer_group_not_authorized(api):
    """scenario: consumer/subscriptions/1.17-negative-join-consumer-group-not-authorized.md

    The creator of a fresh group JOINs it with one interest on its own tenant
    (the tenant its group records) under a policy that does not grant
    ``consume`` on consumer groups, and is refused
    ``ConsumerGroupNotAuthorized`` with no subscription created. Only an
    action-aware policy refuses it: under the static-authz policy of this
    suite's config the JOIN answers 201, with or without ``auth_disabled``.
    The stream and SEEK exchanges need a grant revoked mid-test and are
    covered in process (``domain/group_check_tests.rs``).
    """
    async with api() as client:
        group_resp = await client.post(
            "/consumer-groups",
            json={"client_agent": "e2e-test", "description": f"e2e-1.17-{uuid.uuid4()}"},
        )
        assert group_resp.status_code == 201, group_resp.text
        group = group_resp.json()
        join_resp = await client.post(
            "/subscriptions",
            json={
                "consumer_group": group["id"],
                "client_agent": "e2e-test",
                "interests": [
                    {
                        "topic": TOPIC_STREAM,
                        "subject_tenant": group["tenant"],
                        "ownership_filter": "entire_tenant",
                        "types": [EVENT_TYPE_STREAM],
                    }
                ],
            },
        )
        list_resp = await client.get(
            "/subscriptions",
            params={"$filter": f"consumer_group eq '{group['id']}'"},
        )

    assert join_resp.status_code == 403, join_resp.text
    assert join_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}/subscriptions",
        "trace_id": TRACE_ID,
        "context": {
            "reason": "access denied by policy",
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
        },
    }
    assert list_resp.status_code == 200, list_resp.text
    assert list_resp.json()["items"] == []


@XFAIL_UNLESS_STATIC_TOKENS
async def test_delete_anonymous_group_not_owned(api):
    """scenario: consumer/groups/1.10-negative-delete-anonymous-group-not-owned.md

    Principal A creates a fresh group; principal B, a subject of the same
    tenant, DELETEs it and is refused ``ConsumerGroupNotOwned``, and A still
    reads the group. Under ``auth_disabled`` the gateway ignores both tokens
    and injects one default identity, so B's DELETE answers 204.
    """
    principal_a = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_A}"}
    principal_b = {"Authorization": f"Bearer {TOKEN_PRINCIPAL_B}"}
    async with api() as client:
        group_resp = await client.post("/consumer-groups", headers=principal_a)
        assert group_resp.status_code == 201, group_resp.text
        group_id = group_resp.json()["id"]

        del_resp = await client.delete(f"/consumer-groups/{group_id}", headers=principal_b)
        read_resp = await client.get(f"/consumer-groups/{group_id}", headers=principal_a)

    assert del_resp.status_code == 403, del_resp.text
    assert del_resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": f"{API_BASE}/consumer-groups/{group_id}",
        "trace_id": TRACE_ID,
        "context": {
            "reason": f"consumer group '{group_id}' was created by another principal",
            "resource_type": "gts.cf.core.events.consumer_group.v1~",
        },
    }
    assert read_resp.status_code == 200, read_resp.text
