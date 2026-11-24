"""Quota Resolver E2E: the two planes in front of the QE backend.

The resolver finds QE as its backend through the types registry, so every test
here crosses both gears. Platform-plane calls carry the internal token alone
(``platform_headers``); tenant-plane calls carry a user bearer. Each test
charges a fresh principal, so no other test's Quota applies.
"""
import uuid

import httpx
import pytest

from .conftest import (
    METRIC,
    STATE_METRIC,
    TENANT_A,
    TENANT_B,
    TENANT_SUBJECT_TYPE,
    UNMAPPED_PRINCIPAL_TYPE,
    USER_A,
    USER_PRINCIPAL_TYPE,
    USER_SUBJECT_TYPE,
    quota_body,
)


@pytest.fixture
def principal():
    """A fresh user principal id, so no other test's Quota applies."""
    return str(uuid.uuid4())


def principal_subject(principal: str, subject_type: str = USER_PRINCIPAL_TYPE) -> dict:
    return {"kind": "principal", "type": subject_type, "id": principal}


def item(amount: int) -> dict:
    return {"metric": {"type": METRIC, "metadata": {"region": "eu"}}, "amount": amount}


def charge(principal: str, amount: int, subject_type: str = USER_PRINCIPAL_TYPE, **over) -> dict:
    body = {
        "tenant_id": TENANT_A,
        "subjects": [principal_subject(principal, subject_type)],
        "items": [item(amount)],
        "idempotency_key": f"e2e-qr-{uuid.uuid4().hex}",
    }
    body.update(over)
    return body


def state_request(principal: str, subject_type: str = USER_PRINCIPAL_TYPE) -> dict:
    return {
        "tenant_id": TENANT_A,
        "subject": principal_subject(principal, subject_type),
        "metrics": [{"type": METRIC}],
    }


async def create_quota(
    client, qe_url, headers, subject: str, cap: int = 100, metric: str = METRIC
) -> str:
    resp = await client.post(
        f"{qe_url}/quotas", headers=headers, json=quota_body(subject, cap, metric)
    )
    assert resp.status_code == 201, resp.text
    return resp.json()["id"]


class TestPlatformDebit:
    """A debit for a mapped principal is decided by the QE Quota on its subject."""

    @pytest.mark.smoke
    async def test_debit_allows_then_denies_over_cap(
        self, qe_url, qr_url, tenant_a_headers, platform_headers, principal
    ):
        """60 of 100 is allowed; 50 more is denied as ``quota_exceeded``."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            await create_quota(client, qe_url, tenant_a_headers, principal)

            resp = await client.post(
                f"{qr_url}/platform/debit", headers=platform_headers, json=charge(principal, 60)
            )
            assert resp.status_code == 200, resp.text
            assert resp.json()["allowed"] is True, resp.text

            resp = await client.post(
                f"{qr_url}/platform/debit", headers=platform_headers, json=charge(principal, 50)
            )
            assert resp.status_code == 200, resp.text
            decision = resp.json()
            assert decision["allowed"] is False, decision
            assert decision["reason"] == "quota_exceeded", decision


class TestTenantIsolation:
    """A platform call reaches only the tenant it names."""

    async def test_a_charge_naming_another_tenant_does_not_reach_the_quota(
        self, qe_url, qr_url, tenant_a_headers, platform_headers, principal
    ):
        """A debit and a state read for tenant B leave tenant A's Quota unseen and untouched."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            quota_id = await create_quota(client, qe_url, tenant_a_headers, principal)

            resp = await client.post(
                f"{qr_url}/platform/debit",
                headers=platform_headers,
                json=charge(principal, 100, tenant_id=TENANT_B),
            )
            assert resp.status_code == 200, resp.text
            assert resp.json()["allowed"] is False, resp.text

            other = state_request(principal)
            other["tenant_id"] = TENANT_B
            resp = await client.post(
                f"{qr_url}/platform/quota-state", headers=platform_headers, json=other
            )
            assert resp.status_code == 200, resp.text
            assert resp.json()["items"] == [], resp.text

            resp = await client.post(
                f"{qr_url}/platform/quota-state",
                headers=platform_headers,
                json=state_request(principal),
            )
            assert resp.status_code == 200, resp.text
            [state] = resp.json()["items"]
            assert state["quota_id"] == quota_id, state
            assert (state["consumed"], state["remaining"]) == (0, 100), state


class TestPlatformLease:
    """A lease is committed below what it holds, then part of the charge is returned."""

    async def test_acquire_commit_return_is_reflected_in_quota_state(
        self, qe_url, qr_url, tenant_a_headers, platform_headers, principal
    ):
        """Hold 50, commit 30, return 10: 20 consumed, 80 remaining."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            quota_id = await create_quota(client, qe_url, tenant_a_headers, principal)

            resp = await client.post(
                f"{qr_url}/platform/acquire-lease",
                headers=platform_headers,
                json=charge(principal, 50, ttl_secs=60),
            )
            assert resp.status_code == 200, resp.text
            lease = resp.json()
            assert lease["outcome"] == "acquired", lease

            commit_key = f"e2e-qr-commit-{uuid.uuid4().hex}"
            resp = await client.post(
                f"{qr_url}/platform/commit-lease",
                headers=platform_headers,
                json={
                    "tenant_id": TENANT_A,
                    "token": lease["token"],
                    "amounts": [{"metric_type": METRIC, "amount": 30}],
                    "idempotency_key": commit_key,
                },
            )
            assert resp.status_code in (200, 204), resp.text

            resp = await client.post(
                f"{qr_url}/platform/return-amount",
                headers=platform_headers,
                json={
                    "tenant_id": TENANT_A,
                    "charge": {"operation": "lease_commit", "idempotency_key": commit_key},
                    "amounts": [{"metric_type": METRIC, "amount": 10}],
                    "idempotency_key": f"e2e-qr-return-{uuid.uuid4().hex}",
                },
            )
            assert resp.status_code in (200, 204), resp.text

            resp = await client.post(
                f"{qr_url}/platform/quota-state",
                headers=platform_headers,
                json=state_request(principal),
            )
            assert resp.status_code == 200, resp.text
            [state] = resp.json()["items"]
            assert state["quota_id"] == quota_id, state
            assert state["subject"] == {"type": USER_SUBJECT_TYPE, "id": principal}, state
            assert (state["cap"], state["consumed"], state["remaining"]) == (100, 20, 80), state


class TestUnmappedPrincipal:
    """A principal type with no ``principal_subject_types`` entry has no quotas."""

    async def test_debit_is_denied_and_quota_state_is_empty(
        self, qr_url, platform_headers, principal
    ):
        """``principal_not_mapped`` on debit, an empty page on quota state."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{qr_url}/platform/debit",
                headers=platform_headers,
                json=charge(principal, 1, UNMAPPED_PRINCIPAL_TYPE),
            )
            assert resp.status_code == 200, resp.text
            decision = resp.json()
            assert decision["allowed"] is False, decision
            assert decision["reason"] == "principal_not_mapped", decision

            resp = await client.post(
                f"{qr_url}/platform/quota-state",
                headers=platform_headers,
                json=state_request(principal, UNMAPPED_PRINCIPAL_TYPE),
            )
            assert resp.status_code == 200, resp.text
            assert resp.json()["items"] == [], resp.text


class TestTenantPlane:
    """The edge serves a user's own quota state and nothing that charges."""

    async def test_quota_state_returns_the_users_and_the_tenants_quotas(
        self, qe_url, qr_url, tenant_a_headers
    ):
        """Quotas on the caller's user subject and on the tenant subject both appear."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            user_quota = await create_quota(
                client, qe_url, tenant_a_headers, USER_A, metric=STATE_METRIC
            )
            resp = await client.post(
                f"{qe_url}/quotas",
                headers=tenant_a_headers,
                json=quota_body(
                    TENANT_A, 500, STATE_METRIC, subject_type=TENANT_SUBJECT_TYPE
                ),
            )
            assert resp.status_code == 201, resp.text
            tenant_quota = resp.json()["id"]

            resp = await client.post(
                f"{qr_url}/quota-state",
                headers=tenant_a_headers,
                json={"metrics": [{"type": STATE_METRIC}], "limit": 100},
            )
            assert resp.status_code == 200, resp.text
            subjects = {s["quota_id"]: s["subject"] for s in resp.json()["items"]}
            assert subjects[user_quota] == {"type": USER_SUBJECT_TYPE, "id": USER_A}, subjects
            assert subjects[tenant_quota] == {"type": TENANT_SUBJECT_TYPE, "id": TENANT_A}, subjects

    async def test_platform_route_is_unreachable_with_a_user_bearer(
        self, qr_url, tenant_a_headers, principal
    ):
        """A user bearer gets 401 on a platform route; only the internal token passes."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{qr_url}/platform/debit", headers=tenant_a_headers, json=charge(principal, 1)
            )
            assert resp.status_code == 401, resp.text
