"""Pytest fixtures for Quota Enforcement and Quota Resolver E2E tests.

The suite runs on a focused server built from ``e2e.yaml``: QE, its SQL
storage plugin over SQLite, the Quota Resolver in front of it, the standalone
cluster profile, and an E2E-owned GTS catalog (two counter metrics and a user
quota subject type, which the resolver maps user principals to). Tokens map
to static identities (static-authn-plugin)::

    e2e-token-tenant-a  -> user 11111111-...ed of tenant 00000000-df51-...953
    e2e-token-tenant-b  -> tenant bbbbbbbb-...

Tests create Quotas under a fresh subject id where they can, so they are
order-independent. The quota-state tests use the fixed user and tenant of the
tokens, on a metric of their own; the suite starts on an empty database
(`clean_home` in `e2e.yaml`), so their Quotas do not pile up across runs.
"""
from __future__ import annotations

import os
import time
import uuid

import httpx
import pytest

TENANT_A = "00000000-df51-5b42-9538-d2b56b7ee953"
TENANT_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
PREFIX = "/quota-enforcement/v1"

USER_A = "11111111-6a88-4768-9dfc-6bcd5187d9ed"

METRIC = "gts.cf.core.quota.metric.v1~cf.e2e.qe.tokens.v1~"
STATE_METRIC = "gts.cf.core.quota.metric.v1~cf.e2e.qe.state.v1~"
USER_SUBJECT_TYPE = "gts.cf.core.quota.subj.v1~cf.e2e.qe.user.v1~"
TENANT_SUBJECT_TYPE = "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~"
USER_PRINCIPAL_TYPE = "gts.cf.core.security.subject_user.v1~"
UNMAPPED_PRINCIPAL_TYPE = "gts.cf.core.security.subject_service.v1~"

GEAR = "quota-enforcement"
# Under the suite's 10 s pytest timeout, which also covers fixture setup.
READY_DEADLINE_SECS = 8.0
READY_POLL_SECS = 0.5


@pytest.fixture(scope="session", autouse=True)
def _check_quota_enforcement_enabled():
    """Skip all tests unless the focused QE server runs.

    The shared ``make e2e-local`` server seeds no quota types. Run via:
    make e2e-local SUITE=quota-enforcement (which sets E2E_QUOTA_ENFORCEMENT=1).
    """
    if not os.getenv("E2E_QUOTA_ENFORCEMENT"):
        pytest.skip(
            "Quota Enforcement tests require E2E_QUOTA_ENFORCEMENT=1 — run via "
            "make e2e-local SUITE=quota-enforcement",
            allow_module_level=True,
        )


def qe_component(base_url: str) -> dict | None:
    """The quota-enforcement component of ``GET /health``, if reported."""
    # 503 still carries the JSON report.
    resp = httpx.get(f"{base_url}/health", timeout=5.0)
    for component in resp.json().get("components", []):
        if component.get("gear") == GEAR:
            return component
    return None


@pytest.fixture(scope="session", autouse=True)
def qe_ready(_check_quota_enforcement_enabled):
    """Wait until QE bootstrap reports healthy; fail with its message if not.

    ``/healthz`` only says the process is up; QE bootstraps in the background
    and serves 503 until it is Ready.
    """
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    deadline = time.monotonic() + READY_DEADLINE_SECS
    component = None
    while time.monotonic() < deadline:
        component = qe_component(base_url)
        if component is not None and component.get("status") == "healthy":
            return component
        time.sleep(READY_POLL_SECS)
    pytest.fail(f"quota-enforcement never became healthy: {component}")


@pytest.fixture
def base_url():
    """API Gateway base URL."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


@pytest.fixture
def qe_url(base_url):
    """Quota Enforcement REST prefix."""
    return f"{base_url}{PREFIX}"


def _bearer(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture
def tenant_a_headers():
    """Headers for tenant A (the e2e root tenant)."""
    return _bearer(os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a"))


@pytest.fixture
def tenant_b_headers():
    """Headers for tenant B, unrelated to tenant A."""
    return _bearer("e2e-token-tenant-b")


@pytest.fixture
def subject():
    """A fresh user subject id, so no other test's Quota applies."""
    return f"e2e-qe-{uuid.uuid4().hex[:12]}"


def quota_body(
    subject: str, cap: int = 100, metric: str = METRIC, subject_type: str = USER_SUBJECT_TYPE, **over
) -> dict:
    """A hard monthly consumption Quota on a user subject (or another subject type)."""
    body = {
        "tenant_id": TENANT_A,
        "subject": {"type": subject_type, "id": subject},
        "metric": metric,
        "quota_type": "gts.cf.core.qe.quota_type.v1~cf.core.qe.consumption.v1",
        "period": "gts.cf.core.qe.period_type.v1~cf.core.qe.month.v1",
        "enforcement_mode": "gts.cf.core.qe.enforcement_type.v1~cf.core.qe.hard.v1",
        "cap": cap,
        "notification_thresholds": [],
        "source": "gts.cf.core.qe.source_type.v1~cf.core.qe.operator.v1",
    }
    body.update(over)
    return body


def debit_body(subject: str, amount: int) -> dict:
    """A debit on the user subject in region ``eu`` with a fresh key."""
    return {
        "attribution": {
            "tenant_id": TENANT_A,
            "subjects": [{"type": USER_SUBJECT_TYPE, "id": subject}],
            "metric": {"type": METRIC, "metadata": {"region": "eu"}},
        },
        "amount": amount,
        "idempotency_key": f"e2e-qe-{uuid.uuid4().hex}",
    }


def subject_filter(subject: str) -> dict:
    """``GET /quotas`` ``$filter`` naming one user subject."""
    return {
        "$filter": (
            f"tenant_id eq {TENANT_A} and subject_type eq '{USER_SUBJECT_TYPE}' "
            f"and subject_id eq '{subject}'"
        )
    }


@pytest.fixture
def qr_url(base_url):
    """Quota Resolver REST prefix."""
    return f"{base_url}/quota-resolver/v1"
