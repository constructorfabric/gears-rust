"""Pytest configuration and fixtures for users-info E2E tests."""
import os

import httpx
import pytest

REQUEST_TIMEOUT = 10.0

# Matches static-authn-plugin's e2e token config (config/e2e-local.yaml) and
# the shared default in ../../conftest.py's `auth_headers` fixture.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"

# Every client sends this valid W3C `traceparent`, so the wire `trace_id` the
# canonical error layer echoes is deterministic. `extract_trace_id` prefers the
# live OTel span, but the request span continues this inbound `traceparent`, so
# its trace-id equals the header's; with OTel off the header is used directly.
# Either way the value is TRACE_ID, the header's 32-hex trace-id segment. Uses
# the W3C spec's example ids.
TRACE_ID = "0af7651916cd43dd8448eb211c80319c"
TRACEPARENT = f"00-{TRACE_ID}-b7ad6b7169203331-01"


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

