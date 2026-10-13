# Created: 2026-04-16 by Constructor Tech
"""Pytest configuration and fixtures for resource-group E2E tests."""
import asyncio
import os
import uuid
import time
from typing import Optional
from urllib.parse import urljoin

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
        pytest.skip(
            f"Resource-group service not reachable at {url}",
            allow_module_level=True,
        )


# ── Tenant identity ─────────────────────────────────────────────────────

# Tenant IDs must match static-authn-plugin token config in e2e yaml.
# The suite runs under the `static-authz` profile, which does not require an
# existing tenant hierarchy, so no tenant groups are seeded.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
TENANT_B_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"


# ── Test data helpers ────────────────────────────────────────────────────

_counter = int(time.time() * 1000) % 1000000


def unique_type_code(name: str) -> str:
    """Generate a unique RG type code to avoid collisions between test runs."""
    global _counter
    _counter += 1
    return f"gts.cf.core.rg.type.v1~x.e2etest._.{name}{_counter}.v1~"


RG_TYPE_BASE = "gts.cf.core.rg.type.v1~"
DEFAULT_METADATA_SCHEMA = {
    "type": "object",
    "properties": {"self_managed": {"type": "boolean"}},
}


async def register_in_tr(client, base_url, headers, gts_id, content):
    """Register one entity via TR `POST /v2/entities` and wait for success."""
    resp = await client.post(
        f"{base_url}/types-registry/v2/entities",
        headers={**headers, "Idempotency-Key": str(uuid.uuid4())},
        json={"items": [{"gts_id": gts_id, "content": content}]},
    )
    assert resp.status_code == 202, f"TR registration of '{gts_id}': {resp.status_code} {resp.text}"
    location = urljoin(str(resp.url), resp.headers["location"])
    last = None
    for _ in range(100):
        polled = await client.get(location, headers=headers)
        assert polled.status_code == 200, polled.text
        last = polled.json()
        if last["status"] == "completed":
            item = last["items"][0]
            assert item["status"] == "succeeded", (
                f"TR registration of '{gts_id}' failed: {item.get('error')}"
            )
            return item
        await asyncio.sleep(0.05)
    raise AssertionError(f"TR operation for '{gts_id}' did not complete: {last}")


@pytest.fixture
def create_type(rg_base_url, rg_headers):
    """Factory fixture: register an RG type in Types Registry, return {"code": ...}."""

    async def _create(
        name: str,
        can_be_root: bool = True,
        allowed_parent_types: Optional[list[str]] = None,
        allowed_membership_types: Optional[list[str]] = None,
        metadata_schema: Optional[dict] = None,
    ):
        code = unique_type_code(name)
        content = {
            "$id": f"gts://{code}",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "can_be_root": can_be_root,
                "allowed_parent_types": allowed_parent_types or [],
                "allowed_membership_types": allowed_membership_types or [],
            },
            "allOf": [
                {"$ref": f"gts://{RG_TYPE_BASE}"},
                {
                    "type": "object",
                    "properties": {
                        "metadata": metadata_schema or DEFAULT_METADATA_SCHEMA,
                    },
                },
            ],
        }
        async with httpx.AsyncClient(timeout=10.0) as client:
            await register_in_tr(client, rg_base_url, rg_headers, code, content)
        return {"code": code}

    return _create


@pytest.fixture
def create_group(rg_base_url, rg_headers):
    """Factory fixture: create a resource group and return its data."""
    created_ids = []

    async def _create(
        type_code: str,
        name: str,
        parent_id: Optional[str] = None,
        metadata: Optional[dict] = None,
    ):
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
