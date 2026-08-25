"""Pytest fixtures for CredStore E2E tests.

The suite runs against the standard e2e server (``config/e2e-local.yaml``).
Tokens map to static identities (static-authn-plugin) inside the static
tenant tree (static-tr-plugin)::

    e2e-root (00000000-df51-...953)          <- e2e-token-tenant-a
      hierarchy-root (...0001)               <- e2e-token-hierarchy-root
        hierarchy-l1a (...0002)              <- e2e-token-hierarchy-l1a
        hierarchy-l1b (...0005)              <- e2e-token-hierarchy-l1b

Every test creates its own uniquely-named secrets (``unique_ref``) and
registers them for teardown (``cleanup``), so tests are order-independent and
re-runnable against a shared long-lived server.
"""
from __future__ import annotations

import os
import uuid

import httpx
import pytest

TENANT_A = "00000000-df51-5b42-9538-d2b56b7ee953"
HIERARCHY_ROOT = "00000000-0000-0000-0000-000000000001"
HIERARCHY_L1A = "00000000-0000-0000-0000-000000000002"
HIERARCHY_L1B = "00000000-0000-0000-0000-000000000005"


@pytest.fixture
def base_url():
    """API Gateway base URL."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


def _bearer(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture
def tenant_a_headers():
    """Headers for the e2e-root tenant (root of the whole tree)."""
    return _bearer(os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a"))


@pytest.fixture
def root_headers():
    """Headers for hierarchy-root (...0001) — parent of l1a and l1b."""
    return _bearer("e2e-token-hierarchy-root")


@pytest.fixture
def l1a_headers():
    """Headers for hierarchy-l1a (...0002), child of hierarchy-root."""
    return _bearer("e2e-token-hierarchy-l1a")


@pytest.fixture
def l1b_headers():
    """Headers for hierarchy-l1b (...0005), sibling of l1a."""
    return _bearer("e2e-token-hierarchy-l1b")


@pytest.fixture
def unique_ref():
    """Factory for unique secret references (safe on a shared server)."""

    def make(prefix: str = "e2e-cs") -> str:
        return f"{prefix}-{uuid.uuid4().hex[:12]}"

    return make


@pytest.fixture
def secrets_url(base_url):
    """CredStore secrets collection URL."""
    return f"{base_url}/credstore/v1/secrets"


@pytest.fixture
def cleanup(secrets_url):
    """Register (headers, ref) pairs; teardown deletes them best-effort.

    A private and a tenant/shared secret coexist under one reference and
    DELETE removes the caller's private one first, so deletion is retried
    until 404 (bounded).
    """
    registered: list[tuple[dict, str]] = []

    def register(headers: dict, ref: str) -> str:
        registered.append((headers, ref))
        return ref

    yield register

    with httpx.Client(timeout=10.0) as client:
        for headers, ref in registered:
            for _ in range(3):
                try:
                    resp = client.delete(
                        f"{secrets_url}/{ref}",
                        headers={**headers, "If-Match": "*"},
                    )
                except httpx.RequestError:
                    break
                if resp.status_code != 204:
                    break


@pytest.fixture(scope="session", autouse=True)
def _check_credstore_reachable():
    """Skip the whole suite when the e2e server is not running."""
    url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    try:
        # Any HTTP response (401/404 included) means the gateway is up.
        httpx.get(f"{url}/credstore/v1/secrets/e2e-reachability-probe", timeout=5.0)
    except httpx.ConnectError:
        pytest.skip(f"e2e server not running at {url}", allow_module_level=True)
    except Exception:
        # Timeout or transient error — still try to run the tests.
        pass
