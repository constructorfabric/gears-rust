"""E2E fixtures for the file-storage gear."""
import os

import httpx
import pytest

from .fs_flow import REQUEST_TIMEOUT as FLOW_TIMEOUT

REQUEST_TIMEOUT = 5.0

# Control-plane base path (api.md §"P1 — Control plane").
API_BASE = "/api/file-storage/v1"


@pytest.fixture(scope="session")
def require_file_storage_mounted():
    """Skip the shared-server seam tests when the server is down; fail when the gear is absent.

    Applied explicitly by ``test_file_storage_seams.py`` (the lifecycle suites
    run their own private server and do not use it). The probe is
    authenticated: an unauthenticated request to an unknown path yields 401,
    which would hide a missing gear.

    ``file-storage`` is listed in ``e2e.yaml`` (the gear is enabled in this
    environment), so a 404 from ``GET /storages`` means its routes are not
    mounted -- a regression that must fail, not skip. Only an unreachable
    server skips the module.
    """
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    try:
        with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
            r = client.get(
                f"{base_url}{API_BASE}/storages",
                headers={"Authorization": f"Bearer {token}"},
            )
    except httpx.HTTPError as exc:
        pytest.skip(f"cf-gears-server not reachable at {base_url}: {exc}")
    if r.status_code == 404:
        pytest.fail(
            f"file-storage is enabled in e2e.yaml but GET {API_BASE}/storages "
            f"returned 404 -- control-plane routes are not mounted"
        )


@pytest.fixture
def api_base():
    return API_BASE


@pytest.fixture
def gts_file_type():
    """A syntactically valid GTS file type accepted at upload time."""
    return os.getenv(
        "E2E_FS_GTS_TYPE",
        "gts.cf.fstorage.file.type.v1~x.e2e.file.type.v1~",
    )


@pytest.fixture
def fs_client(fs_base_url, auth_headers):
    """Authenticated client for a lifecycle suite's private server (``fs_base_url``)."""
    with httpx.Client(
        base_url=fs_base_url,
        headers=auth_headers,
        timeout=FLOW_TIMEOUT,
        follow_redirects=False,
    ) as client:
        yield client
