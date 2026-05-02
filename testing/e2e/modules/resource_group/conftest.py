# Created: 2026-04-16 by Constructor Tech
"""Pytest configuration and fixtures for resource-group E2E tests."""
import json
import os
import pathlib
import sqlite3
import uuid
import time
from typing import Optional

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


# ── Root tenant seed (direct SQLite) ────────────────────────────────────

# Tenant IDs must match static-authn-plugin token config in e2e yaml.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
TENANT_B_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
TENANT_TYPE_CODE = "gts.cf.core.rg.type.v1~y.core.tn.tenant.v1~"


@pytest.fixture(scope="session", autouse=True)
def _seed_root_tenants():
    """Seed root tenant groups directly in SQLite.

    Root tenants cannot be created via API because tr-authz-plugin requires
    an existing hierarchy to authorize requests. This seeds the minimum data
    needed: one tenant type + two root tenant groups + closure self-rows.

    Idempotent: uses INSERT OR IGNORE.
    """
    home = os.path.expanduser(os.getenv("CYBERFABRIC_HOME", "~/.cyberfabric"))
    db_path = pathlib.Path(home) / "resource-group" / "resource_group.db"
    if not db_path.exists():
        return  # server not started or different DB path

    def uuid_to_blob(uuid_str: str) -> bytes:
        """Convert UUID string to 16-byte binary (SeaORM SQLite format)."""
        return uuid.UUID(uuid_str).bytes

    conn = sqlite3.connect(str(db_path))
    try:
        tid_a = uuid_to_blob(TENANT_A_ID)
        tid_b = uuid_to_blob(TENANT_B_ID)

        # 1. Tenant type (is_tenant=true, can_be_root=true)
        metadata_schema = json.dumps({
            "__can_be_root": True,
            "__is_tenant": True,
        })
        conn.execute(
            "INSERT OR IGNORE INTO gts_type (schema_id, metadata_schema) VALUES (?, ?)",
            (TENANT_TYPE_CODE, metadata_schema),
        )
        row = conn.execute(
            "SELECT id FROM gts_type WHERE schema_id = ?", (TENANT_TYPE_CODE,)
        ).fetchone()
        type_id = row[0]

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
    return f"gts.cf.core.rg.type.v1~x.e2etest.{name}{_counter}.v1~"


@pytest.fixture
def create_type(rg_base_url, rg_headers):
    """Factory fixture: create a GTS type and return its code."""
    created_codes = []

    async def _create(
        name: str,
        can_be_root: bool = True,
        allowed_parent_types: Optional[list[str]] = None,
        allowed_membership_types: Optional[list[str]] = None,
    ):
        code = unique_type_code(name)
        payload = {
            "code": code,
            "can_be_root": can_be_root,
            "allowed_parent_types": allowed_parent_types or [],
            "allowed_membership_types": allowed_membership_types or [],
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
