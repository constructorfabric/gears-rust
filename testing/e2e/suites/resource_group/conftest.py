# Created: 2026-04-16 by Constructor Tech
"""Pytest configuration and fixtures for resource-group E2E tests."""
import os
import pathlib
import sqlite3
import uuid
import time
from typing import Optional

import httpx
import pytest

from ..types_registry.helpers import PLATFORM_API, RECEIPT, get_entity, submit_and_poll

REQUEST_TIMEOUT = 5.0  # per-request hard timeout for all E2E calls
RG_BASE_TYPE = "gts.cf.core.rg.type.v1~"


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


# ── Root tenant seed (direct SQLite) ────────────────────────────────────

# Tenant IDs must match static-authn-plugin token config in e2e yaml.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
TENANT_B_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
TENANT_TYPE_CODE = "gts.cf.core.rg.type.v1~x.e2etest._.tenant.v1~"


RG_MEMBERSHIP_BASE = "gts.cf.core.rg.membership.v1~"


def membership_schema_id(group_code: str) -> str:
    return RG_MEMBERSHIP_BASE + group_code.removeprefix(RG_BASE_TYPE)


def rg_type_schema(
    code: str, parent_type: Optional[dict] = None, *, membership_type: Optional[str] = None,
) -> dict:
    """The concrete group definition published through Types Registry."""
    binding = membership_schema_id(code) if membership_type is None else membership_type
    return {
        "$id": f"gts://{code}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits-schema": {"type": "object", "properties": {"membership_type": {"const": binding}}},
        "x-gts-traits": {"membership_type": binding},
        "allOf": [
            {"$ref": f"gts://{RG_BASE_TYPE}"},
            {"properties": {
                "parent_type": parent_type if parent_type is not None else {"type": "null"},
            }},
        ],
    }


def membership_schema(group_code: str, resource_type: Optional[dict] = None) -> dict:
    if resource_type is None:
        # No valid GTS resource type matches the empty string.
        resource_type = {"type": "string", "x-gts-type-ref": "gts.*", "maxLength": 0}
    return {
        "$id": f"gts://{membership_schema_id(group_code)}",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            {"$ref": f"gts://{RG_MEMBERSHIP_BASE}"},
            {"properties": {"resource_type": resource_type}},
        ],
    }


def _platform_headers() -> dict:
    """The platform plane: the internal token alone (see the root conftest)."""
    return {"X-ToolKit-Internal-Token": os.getenv("E2E_INTERNAL_TOKEN", "e2e-internal-token")}


async def publish_rg_type(
    base_url: str,
    code: str,
    *,
    parent_type: Optional[dict] = None,
    resource_type: Optional[dict] = None,
) -> str:
    """Publish an RG type through the types registry's platform API and wait
    for its admission. Existing declarations use a version-matched revision
    so successive suite runs sharing a registry can publish the same type."""
    async with httpx.AsyncClient(
        base_url=base_url.rstrip("/") + "/", headers=_platform_headers(), timeout=10.0
    ) as platform:
        candidates = [
            {
                "gts_id": membership_schema_id(code),
                "content": membership_schema(code, resource_type),
            },
            {"gts_id": code, "content": rg_type_schema(code, parent_type)},
        ]
        for candidate in candidates:
            type_id = candidate["gts_id"]
            current = await get_entity(platform, PLATFORM_API, type_id, select="origin")
            if current.status_code == 200:
                candidate["expected_resource_version"] = current.json()["origin"]["resource_version"]
            else:
                assert current.status_code == 404, (
                    f"types-registry read of '{type_id}': {current.status_code} {current.text}"
                )
        operation = await submit_and_poll(platform, PLATFORM_API, candidates, RECEIPT)
    for item in operation["items"]:
        assert item["status"] in ("succeeded", "unchanged"), (
            f"types-registry refused a definition for RG type '{code}': {item}"
        )
    return code


@pytest.fixture(scope="session", autouse=True)
def _seed_root_tenants():
    """Seed root tenant groups directly in SQLite.

    Root tenants cannot be created via API because tr-authz-plugin requires
    an existing hierarchy to authorize requests. This seeds the minimum data
    needed: two root tenant groups + closure self-rows. Their type is published
    to the types registry (resource-group keeps no type catalogue); the rows
    store its GTS identifier.

    Idempotent: uses INSERT OR IGNORE, and republishes the type under its
    current registry version so identical content is unchanged.
    """
    import asyncio

    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8087")
    asyncio.run(publish_rg_type(base_url, TENANT_TYPE_CODE))

    home = os.getenv("APP__SERVER__HOME_DIR") or os.getenv("CF_GEARS_HOME", "~/.cf-gears")
    db_path = pathlib.Path(home).expanduser() / "resource-group" / "resource_group.db"
    if not db_path.exists():
        return  # server not started or different DB path

    def uuid_to_blob(uuid_str: str) -> bytes:
        """Convert UUID string to 16-byte binary (SeaORM SQLite format)."""
        return uuid.UUID(uuid_str).bytes

    conn = sqlite3.connect(str(db_path))
    try:
        tid_a = uuid_to_blob(TENANT_A_ID)
        tid_b = uuid_to_blob(TENANT_B_ID)

        # 1. Resolve the group type into RG's numeric dictionary.
        conn.execute("INSERT OR IGNORE INTO gts_type (schema_id) VALUES (?)", (TENANT_TYPE_CODE,))
        type_id = conn.execute("SELECT id FROM gts_type WHERE schema_id = ?", (TENANT_TYPE_CODE,)).fetchone()[0]

        # 2. Root tenant A (UUIDs as BLOB to match SeaORM format)
        conn.execute(
            "INSERT OR IGNORE INTO resource_group (id, gts_type_id, name, tenant_id) VALUES (?, ?, ?, ?)",
            (tid_a, type_id, "e2e-root-tenant-a", tid_a),
        )
        conn.execute(
            "INSERT OR IGNORE INTO resource_group_closure (ancestor_id, descendant_id, depth) VALUES (?, ?, 0)",
            (tid_a, tid_a),
        )

        # 3. Root tenant B
        conn.execute(
            "INSERT OR IGNORE INTO resource_group (id, gts_type_id, name, tenant_id) VALUES (?, ?, ?, ?)",
            (tid_b, type_id, "e2e-root-tenant-b", tid_b),
        )
        conn.execute(
            "INSERT OR IGNORE INTO resource_group_closure (ancestor_id, descendant_id, depth) VALUES (?, ?, 0)",
            (tid_b, tid_b),
        )

        conn.commit()
        # Flush WAL so the server (separate process) sees our writes immediately
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        conn.close()


# ── Test data helpers ────────────────────────────────────────────────────

_counter = int(time.time() * 1000) % 1000000


def unique_type_code(name: str) -> str:
    """Generate a unique RG type code to avoid collisions between test runs."""
    global _counter
    _counter += 1
    return f"gts.cf.core.rg.type.v1~x.e2etest._.{name}{_counter}.v1~"


@pytest.fixture
def create_type(rg_base_url):
    """Publish a group and its membership schema; return the group type ID."""
    async def _create(
        name: str, *, parent_type: Optional[dict] = None, resource_type: Optional[dict] = None,
    ) -> str:
        return await publish_rg_type(
            rg_base_url, unique_type_code(name), parent_type=parent_type, resource_type=resource_type,
        )

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
