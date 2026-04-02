# Created: 2026-04-16 by Constructor Tech
"""Pytest configuration and fixtures for resource-group E2E tests."""
import os
import uuid
import time

import httpx
import pytest

REQUEST_TIMEOUT = 5.0  # per-request hard timeout for all E2E calls


# ── Environment-driven fixtures ──────────────────────────────────────────

@pytest.fixture
def rg_base_url():
    """Resource-group service base URL."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8087")


@pytest.fixture
def rg_headers():
    """Standard headers with auth token for resource-group requests."""
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    return {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}",
    }


# ── Reachability check ───────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def _check_rg_reachable():
    """Skip all resource-group tests if the service is not reachable."""
    url = os.getenv("E2E_BASE_URL", "http://localhost:8087")
    try:
        httpx.get(
            f"{url}/resource-group/v1/groups",
            timeout=5.0,
            headers={"Authorization": "Bearer e2e-token-tenant-a"},
        )
        # Any response (even 401/403) means the service is up.
    except httpx.ConnectError:
        pytest.skip(
            f"Resource-group service not running at {url}",
            allow_module_level=True,
        )
    except (httpx.TimeoutException, OSError):
        pass


# ── Test data helpers ────────────────────────────────────────────────────

_counter = int(time.time() * 1000) % 1000000


def unique_type_code(name: str) -> str:
    """Generate a unique RG type code to avoid collisions between test runs."""
    global _counter
    _counter += 1
    return f"gts.cf.core.rg.type.v1~x.e2etest.{name}{_counter}.v1~"


@pytest.fixture
def create_type(rg_base_url, rg_headers):
    """Factory fixture: create a GTS type and return its code."""
    created_codes = []

    async def _create(name: str, can_be_root: bool = True, allowed_parents=None, allowed_memberships=None):
        code = unique_type_code(name)
        payload = {
            "code": code,
            "can_be_root": can_be_root,
            "allowed_parents": allowed_parents or [],
            "allowed_memberships": allowed_memberships or [],
        }
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{rg_base_url}/types-registry/v1/types",
                headers=rg_headers,
                json=payload,
            )
            assert resp.status_code == 201, (
                f"Failed to create type '{code}': {resp.status_code} {resp.text}"
            )
            created_codes.append(code)
            return resp.json()

    return _create


@pytest.fixture
def create_group(rg_base_url, rg_headers):
    """Factory fixture: create a resource group and return its data."""
    created_ids = []

    async def _create(type_code: str, name: str, parent_id: str = None, metadata=None):
        payload = {
            "type": type_code,
            "name": name,
        }
        if parent_id:
            payload["parent_id"] = parent_id
        if metadata is not None:
            payload["metadata"] = metadata

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                f"{rg_base_url}/resource-group/v1/groups",
                headers=rg_headers,
                json=payload,
            )
            assert resp.status_code == 201, (
                f"Failed to create group '{name}': {resp.status_code} {resp.text}"
            )
            data = resp.json()
            created_ids.append(data["id"])
            return data

    return _create


# ── Shared helpers ──────────────────────────────────────────────────────


def assert_group_shape(data: dict):
    """Verify JSON wire format matches OpenAPI GroupDto contract."""
    uuid.UUID(data["id"])
    assert isinstance(data["type"], str)
    assert isinstance(data["name"], str)
    hier = data["hierarchy"]
    uuid.UUID(hier["tenant_id"])
    if hier.get("parent_id") is not None:
        uuid.UUID(hier["parent_id"])
    if data.get("metadata") is not None:
        assert isinstance(data["metadata"], dict)
