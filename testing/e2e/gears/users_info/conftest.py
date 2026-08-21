"""Pytest configuration and fixtures for users-info E2E tests."""
import os

import httpx
import pytest

REQUEST_TIMEOUT = 10.0

# Matches static-authn-plugin's e2e token config (config/e2e-local.yaml) and
# the shared default in ../../conftest.py's `auth_headers` fixture.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"


@pytest.fixture(scope="session", autouse=True)
def _check_users_info_reachable():
    """Skip all users-info tests if the service is not reachable."""
    url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    try:
        httpx.get(
            f"{url}/users-info/v1/users",
            timeout=5.0,
            headers={"Authorization": "Bearer e2e-token-tenant-a"},
        )
        # Any response (even 401/403) means the service is up.
    except httpx.ConnectError:
        pytest.skip(
            f"users-info service not running at {url}",
            allow_module_level=True,
        )
    except (httpx.TimeoutException, OSError):
        pytest.skip(
            f"users-info service not reachable at {url}",
            allow_module_level=True,
        )

