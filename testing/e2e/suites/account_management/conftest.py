# Created: 2026-05-16 by Constructor Tech
"""Pytest configuration and fixtures for account-management E2E tests.

Cross-process seam tests for the AM gear's REST surface. Mirrors the
``testing/e2e/suites/resource_group/conftest.py`` shape and conventions
so the two suites share a single mental model.

The AM gear is wired into the same gateway process that hosts every
other system gear, so all requests flow through
``http://localhost:8086`` and carry a single static-authn-plugin token.
Each fixture is a thin wrapper around an ``httpx.AsyncClient`` POST/PUT
returning the parsed response body for downstream assertions.
"""
import os
import time
import uuid
from typing import Optional

import httpx
import pytest

REQUEST_TIMEOUT = 5.0  # per-request hard timeout for all E2E calls


# ── Tenant tokens (must match config/e2e-local.yaml static-authn-plugin) ─

# Root tenant — caller token "e2e-token-tenant-a" maps to this id.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
# Sibling root tenant — caller token "e2e-token-tenant-b" maps here.
TENANT_B_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"

# Chained GTS tenant-type identifier accepted by AM's tenant-type
# checker for child tenants under the platform root. Seeded in
# ``config/e2e-local.yaml`` under ``types-registry.config.entities``
# with ``allowed_parent_types: [platform, customer]`` so the suite can
# build platform -> customer -> customer hierarchies.
DEFAULT_TENANT_TYPE = "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"

# Chained GTS schema id used for tenant metadata seam tests. The
# trailing segment carries the canonical 5-token GTS shape
# ``vendor.package.namespace.type.vMAJOR`` so the types-registry's
# GTS id validator accepts it on register. The ``create_metadata_schema``
# factory ensures registration before tests that depend on it run.
DEFAULT_METADATA_SCHEMA_ID = "gts.cf.core.am.tenant_metadata.v1~x.e2etest.am.meta.v1~"


# ── Environment-driven fixtures ──────────────────────────────────────────


@pytest.fixture
def am_base_url():
    """Account-management service base URL (gateway-prefixed in production)."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


@pytest.fixture
def am_headers():
    """Standard headers with auth token for account-management requests."""
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    return {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}",
    }


@pytest.fixture
def am_headers_tenant_b():
    """Headers with the tenant-B bearer token.

    The B-token's identity is rooted at ``TENANT_B_ID`` per
    ``config/e2e-local.yaml`` (``static-authn-plugin`` static_tokens).
    AM's bootstrap seeds ONLY ``TENANT_A_ID`` as the platform root, so
    ``TENANT_B_ID`` is intentionally NOT an AM tenant row -- it exists
    only as a PEP subject. The cross-tenant scope-clamp test uses this
    fixture to attempt reads under ``TENANT_A_ID``'s subtree while the
    caller's PEP scope is rooted at ``TENANT_B_ID``; the real
    ``static-authz-plugin`` policy bundle MUST narrow the read to an
    empty subtree (collapsing to 404 / 403).
    """
    return {
        "Content-Type": "application/json",
        "Authorization": "Bearer e2e-token-tenant-b",
    }


# ── Reachability check ───────────────────────────────────────────────────


@pytest.fixture(scope="session", autouse=True)
def _check_am_reachable():
    """Skip all account-management tests if the service is not reachable.

    Mirrors RG's reachability guard. Any HTTP response (including 401 /
    403 / 404) is treated as "service up"; only transport errors short
    out the suite via ``pytest.skip`` at gear-load time.
    """
    url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    try:
        httpx.get(
            f"{url}/account-management/v1/tenants/{TENANT_A_ID}",
            timeout=5.0,
            headers={"Authorization": "Bearer e2e-token-tenant-a"},
        )
        # Any response (even 401/403/404) means the service is up.
    except httpx.ConnectError:
        pytest.skip(
            f"Account-management service not running at {url}",
            allow_module_level=True,
        )
    except (httpx.TimeoutException, OSError):
        pytest.skip(
            f"Account-management service not reachable at {url}",
            allow_module_level=True,
        )


# ── Test data helpers ────────────────────────────────────────────────────

_counter = int(time.time() * 1000) % 1000000


def unique_name(prefix: str) -> str:
    """Generate a unique tenant / user name to avoid run-to-run collisions."""
    global _counter
    _counter += 1
    return f"{prefix}-{_counter}"


# ── URL helpers (also used inside test bodies) ──────────────────────────


def _tenants(base: str) -> str:
    return f"{base}/account-management/v1/tenants"


def _tenant(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}"


def _children(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/children"


def _metadata(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/metadata"


def _metadata_entry(base: str, tid: str, type_id: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/metadata/{type_id}"


def _metadata_resolved(base: str, tid: str, type_id: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/metadata/{type_id}/resolved"


def _users(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/users"


def _user(base: str, tid: str, uid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/users/{uid}"


def _service_accounts(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/service-accounts"


def _service_account(base: str, tid: str, client_id: str) -> str:
    return (
        f"{base}/account-management/v1/tenants/{tid}"
        f"/service-accounts/{client_id}"
    )


def _service_account_rotate(base: str, tid: str, client_id: str) -> str:
    return (
        f"{base}/account-management/v1/tenants/{tid}"
        f"/service-accounts/{client_id}/rotate-secret"
    )


def _conversions(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/conversions"


def _conversion(base: str, tid: str, rid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/conversions/{rid}"


def _child_conversions(base: str, tid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/child-conversions"


def _child_conversion(base: str, tid: str, rid: str) -> str:
    return f"{base}/account-management/v1/tenants/{tid}/child-conversions/{rid}"


def _types(base: str) -> str:
    """Types-registry batch-register endpoint — used to register metadata schemas."""
    return f"{base}/types-registry/v1/entities"


# ── Factory fixtures ────────────────────────────────────────────────────


@pytest.fixture
def create_tenant(am_base_url, am_headers):
    """Factory fixture: create a child tenant and return its projection.

    Cleanup is intentionally best-effort: AM exposes only soft-delete via
    DELETE, and the retention sweep reaps the row eventually. Tests that
    must inspect post-delete state issue the DELETE inline rather than
    relying on the fixture's teardown.
    """
    created_ids: list[str] = []

    async def _create(
        name: str,
        parent_id: str = TENANT_A_ID,
        tenant_type: str = DEFAULT_TENANT_TYPE,
        self_managed: bool = False,
        provisioning_metadata: Optional[dict] = None,
        expect_status: int = 201,
    ) -> dict:
        payload = {
            "name": unique_name(name),
            "parent_id": parent_id,
            "tenant_type": tenant_type,
            "self_managed": self_managed,
        }
        if provisioning_metadata is not None:
            payload["provisioning_metadata"] = provisioning_metadata

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                _tenants(am_base_url),
                headers=am_headers,
                json=payload,
            )
            assert resp.status_code == expect_status, (
                f"Failed to create tenant '{name}': "
                f"{resp.status_code} {resp.text}"
            )
            if expect_status == 201:
                data = resp.json()
                created_ids.append(data["id"])
                return data
            return resp.json() if resp.content else {}

    yield _create

    # Best-effort soft-delete teardown -- ignore failures so a missing
    # AM dev-stack does not mask the real test outcome.
    for tid in reversed(created_ids):
        try:
            httpx.delete(
                _tenant(am_base_url, tid),
                headers=am_headers,
                timeout=5.0,
            )
        except (httpx.HTTPError, OSError):
            pass


@pytest.fixture
def create_metadata_schema(am_base_url, am_headers):
    """Factory fixture: register a metadata GTS schema in the types-registry.

    Returns the chained ``type_id`` (a string). Idempotent on the wire
    -- the types-registry batch endpoint always returns 200 with a
    per-item result; an already-registered entity surfaces as
    ``status=error`` with an "Entity already exists" message which the
    factory treats as success so suites can re-run without manual
    cleanup.
    """

    async def _register(
        type_id: str = DEFAULT_METADATA_SCHEMA_ID,
        inheritance_policy: str = "inherit",
    ) -> str:
        payload = {
            "entities": [
                {
                    "$id": f"gts://{type_id}",
                    # `$schema` is the JSON Schema dialect, NOT the parent type
                    # — derivation from gts.cf.core.am.tenant_metadata.v1~ is
                    # encoded by the `~`-chain in `$id`. gts 0.10.0 compiles the
                    # `x-gts-traits` under this dialect, so it must be a real
                    # meta-schema (draft-07, matching the base envelope).
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "description": "E2E test metadata schema",
                    "type": "object",
                    "x-gts-traits": {
                        "inheritance_policy": inheritance_policy,
                    },
                }
            ]
        }
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                _types(am_base_url),
                headers=am_headers,
                json=payload,
            )
            assert resp.status_code == 200, (
                f"types-registry register status {resp.status_code} "
                f"for '{type_id}': {resp.text}"
            )
            results = resp.json().get("results") or []
            assert results, (
                f"types-registry returned no results for '{type_id}': "
                f"{resp.text}"
            )
            outcome = results[0]
            if outcome.get("status") == "ok":
                return type_id
            err = (outcome.get("error") or "").lower()
            assert "already exists" in err, (
                f"Failed to register schema '{type_id}': {outcome}"
            )
            return type_id

    return _register


@pytest.fixture
def seed_idp_user(am_base_url, am_headers, create_tenant):
    """Factory fixture: provision an IdP user via AM, then deprovision on teardown.

    Pins the IdP plugin contract end-to-end: POST `/users` returns 201
    with the projected user body; teardown DELETEs the same user
    through the same plugin so the suite never leaks IdP state.
    """
    seeded: list[tuple[str, str]] = []  # (tenant_id, user_id)

    async def _seed(
        tenant_id: str,
        username: Optional[str] = None,
        email: Optional[str] = None,
        display_name: Optional[str] = None,
    ) -> dict:
        payload: dict = {"username": username or unique_name("e2e-user")}
        if email is not None:
            payload["email"] = email
        if display_name is not None:
            payload["display_name"] = display_name

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                _users(am_base_url, tenant_id),
                headers=am_headers,
                json=payload,
            )
            assert resp.status_code == 201, (
                f"Failed to seed user under tenant '{tenant_id}': "
                f"{resp.status_code} {resp.text}"
            )
            data = resp.json()
            seeded.append((tenant_id, data["id"]))
            return data

    yield _seed

    # Best-effort deprovision; deletion is idempotent (204 on either
    # path) so retries on a fresh teardown are safe.
    for tenant_id, user_id in reversed(seeded):
        try:
            httpx.delete(
                _user(am_base_url, tenant_id, user_id),
                headers=am_headers,
                timeout=5.0,
            )
        except (httpx.HTTPError, OSError):
            pass


# ── Shared shape assertions ─────────────────────────────────────────────


def assert_tenant_shape(data: dict) -> None:
    """Verify JSON wire format matches the AM TenantDto contract."""
    uuid.UUID(data["id"])
    assert isinstance(data["name"], str)
    assert data["status"] in ("active", "suspended", "deleted")
    if data.get("parent_id") is not None:
        uuid.UUID(data["parent_id"])
    assert isinstance(data["self_managed"], bool)
    assert isinstance(data["depth"], int)
    assert isinstance(data["created_at"], str)
    assert isinstance(data["updated_at"], str)
