"""E2E seam tests for GET /account-management/v1/admin/context.

The admin panel fetches this at startup to discover the principal, home
tenant, admin mode, and capability hints. These tests pin the wire
contract and the platform-vs-tenant projection across the role tokens.
"""
import uuid

import httpx
import pytest

from .conftest import ADMIN_CONTEXT_PATH, ROOT_TENANT_ID, REQUEST_TIMEOUT


async def _get(base_url: str, headers: dict | None) -> httpx.Response:
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        return await client.get(f"{base_url}{ADMIN_CONTEXT_PATH}", headers=headers or {})


def _assert_context_shape(body: dict) -> None:
    uuid.UUID(body["subject_id"])
    uuid.UUID(body["subject_tenant_id"])
    assert body["admin_mode"] in ("platform", "tenant")
    assert isinstance(body["capabilities"], list)
    assert all(isinstance(c, str) for c in body["capabilities"])
    assert isinstance(body["non_production_auth"], bool)


async def test_requires_authentication(base_url):
    """The .authenticated() route gate rejects an unauthenticated caller."""
    resp = await _get(base_url, headers=None)
    assert resp.status_code == 401, resp.text


async def test_default_role_projects_tenant_mode(base_url, headers_default):
    """An untyped token has no role marker -> least-privileged tenant mode,
    and is NOT flagged as the non-production role stub."""
    resp = await _get(base_url, headers_default)
    assert resp.status_code == 200, resp.text
    body = resp.json()
    _assert_context_shape(body)
    assert body["admin_mode"] == "tenant"
    assert body["non_production_auth"] is False
    assert body["subject_tenant_id"] == ROOT_TENANT_ID


async def test_platform_admin_projection(base_url, headers_platform_admin):
    """platform_admin -> platform mode + full capability set + stub flag."""
    resp = await _get(base_url, headers_platform_admin)
    assert resp.status_code == 200, resp.text
    body = resp.json()
    _assert_context_shape(body)
    assert body["subject_type"] == "platform_admin"
    assert body["admin_mode"] == "platform"
    assert body["non_production_auth"] is True
    caps = set(body["capabilities"])
    # Platform admin can mutate tenants and run lifecycle actions.
    assert {"tenants:read", "tenants:write", "tenants:suspend", "gears:read"} <= caps


async def test_tenant_admin_projection(base_url, headers_tenant_admin):
    """tenant_admin -> tenant mode + least-privileged caps (no tenant writes)."""
    resp = await _get(base_url, headers_tenant_admin)
    assert resp.status_code == 200, resp.text
    body = resp.json()
    _assert_context_shape(body)
    assert body["subject_type"] == "tenant_admin"
    assert body["admin_mode"] == "tenant"
    assert body["non_production_auth"] is True
    caps = set(body["capabilities"])
    assert "tenants:read" in caps
    # A tenant admin must NOT advertise tenant write/suspend capabilities.
    assert "tenants:write" not in caps
    assert "tenants:suspend" not in caps
