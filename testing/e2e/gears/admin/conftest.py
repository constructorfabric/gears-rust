"""Pytest fixtures for the admin-panel backend E2E tests.

Covers the admin-panel's one new backend surface:
``GET /account-management/v1/admin/context``. The endpoint projects the
caller's security context into an admin mode + capability hints; the role
markers come from the ``static-authn-plugin`` ``subject_type`` field.

Tokens (see ``config/e2e-local.yaml`` static-authn-plugin static_tokens):
- ``e2e-token-tenant-a``       — no subject_type, default (tenant) role.
- ``e2e-token-platform-admin`` — subject_type=platform_admin.
- ``e2e-token-tenant-admin``   — subject_type=tenant_admin.

All requests flow through the shared gateway process at
``http://localhost:8086``; any HTTP response means "service up".
"""
import os

import httpx
import pytest

REQUEST_TIMEOUT = 5.0

# Platform structural root — every admin token below is homed here.
ROOT_TENANT_ID = "00000000-df51-5b42-9538-d2b56b7ee953"

ADMIN_CONTEXT_PATH = "/account-management/v1/admin/context"


@pytest.fixture
def base_url():
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


def _headers(token: str) -> dict:
    return {"Content-Type": "application/json", "Authorization": f"Bearer {token}"}


@pytest.fixture
def headers_default():
    """Untyped token — exercises the default (least-privileged) projection."""
    return _headers("e2e-token-tenant-a")


@pytest.fixture
def headers_platform_admin():
    return _headers("e2e-token-platform-admin")


@pytest.fixture
def headers_tenant_admin():
    return _headers("e2e-token-tenant-admin")


@pytest.fixture(scope="session", autouse=True)
def _check_reachable():
    """Skip the suite if the gateway is not up (mirrors AM/RG guards)."""
    url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    try:
        httpx.get(
            f"{url}{ADMIN_CONTEXT_PATH}",
            timeout=5.0,
            headers={"Authorization": "Bearer e2e-token-tenant-a"},
        )
    except httpx.ConnectError:
        pytest.skip(f"Gateway not running at {url}", allow_module_level=True)
    except (httpx.TimeoutException, OSError):
        pytest.skip(f"Gateway not reachable at {url}", allow_module_level=True)
