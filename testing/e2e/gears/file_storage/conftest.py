"""E2E fixtures for the file-storage gear.

The file-storage control-plane REST surface (``/api/file-storage/v1``) is
implemented in milestone M5. Until
those routes are mounted, the whole module skips gracefully — we do NOT want N
red failures for endpoints that intentionally do not exist yet. Once M5 lands,
the probe below starts returning non-404 and every seam test runs for real.
"""
import os

import httpx
import pytest

REQUEST_TIMEOUT = 5.0

# Control-plane base path (api.md §"P1 — Control plane").
API_BASE = "/api/file-storage/v1"


@pytest.fixture(scope="session", autouse=True)
def require_file_storage_mounted():
    """Skip the whole module unless the control-plane is reachable and mounted.

    Keys on ``GET /storages`` (read-only backend discovery — the simplest P1
    control endpoint). A 404 means the gear's routes are not mounted (the gear
    is an opt-in cargo feature and may not be built into this server); a
    connection error means the server is down. Either way: skip, don't fail.

    The probe is **authenticated**: the API gateway returns 401 (not 404) for an
    unauthenticated request to any unknown path, so an auth-less probe could not
    tell "gear absent" (should skip) from "auth required" (gear present). With a
    valid token an unknown route yields a clean 404.

    Reads base URL and token from the environment directly (matching the shared
    ``base_url`` / ``auth_headers`` fixture defaults) so this can stay
    session-scoped.
    """
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    url = f"{base_url}{API_BASE}/storages"
    try:
        with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
            r = client.get(url, headers={"Authorization": f"Bearer {token}"})
    except httpx.HTTPError as exc:
        pytest.skip(f"cf-gears-server not reachable at {base_url}: {exc}")
    if r.status_code == 404:
        pytest.skip(
            "file-storage REST endpoints are not mounted — the gear is an "
            "opt-in feature not built into this server."
        )


@pytest.fixture
def api_base():
    return API_BASE


@pytest.fixture
def gts_file_type():
    """A syntactically valid GTS file type accepted at upload time."""
    return os.getenv(
        "E2E_FS_GTS_TYPE",
        "gts.cf.fstorage.file.type.v1~x.e2e.test.v1~",
    )
