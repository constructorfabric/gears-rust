"""Pytest configuration and fixtures for Gateway Scope Enforcement E2E tests."""
from __future__ import annotations

import os
from typing import Optional

import httpx
import pytest


@pytest.fixture
def base_url():
    """API Gateway base URL."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


@pytest.fixture
def tenant_id():
    """Fixed tenant UUID for test isolation."""
    return "00000000-df51-5b42-9538-d2b56b7ee953"


# Token fixtures for different scope scenarios
@pytest.fixture
def token_full_access():
    """Token with full access (first-party app with '*' scope)."""
    return "token-full-access"


@pytest.fixture
def token_users_read():
    """Token with users:read scope only."""
    return "token-users-read"


@pytest.fixture
def token_users_admin():
    """Token with users:admin scope."""
    return "token-users-admin"


@pytest.fixture
def token_cities_admin():
    """Token with cities:admin scope (no users access)."""
    return "token-cities-admin"


@pytest.fixture
def token_no_scopes():
    """Token with no relevant scopes."""
    return "token-no-scopes"


def make_headers(tenant_id: str, token: str | None = None) -> dict:
    """Build request headers with tenant and optional auth token."""
    headers = {"x-tenant-id": tenant_id}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


@pytest.fixture(scope="session", autouse=True)
def _check_scope_enforcement_enabled():
    """Skip all tests if scope enforcement is not enabled.

    These tests require a server running with the scope-enforcement overlay
    (testing/e2e/suites/scope_enforcement/e2e.yaml, applied over
    config/e2e-local.yaml). Run via: make e2e-local SUITE=scope-enforcement
    (which sets E2E_SCOPE_ENFORCEMENT=1).
    """
    if not os.getenv("E2E_SCOPE_ENFORCEMENT"):
        pytest.skip(
            "Scope enforcement tests require E2E_SCOPE_ENFORCEMENT=1 — run via "
            "make e2e-local SUITE=scope-enforcement",
            allow_module_level=True,
        )
