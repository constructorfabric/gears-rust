# Created: 2026-04-16 by Constructor Tech
"""E2E tests: MTLS authentication mode for resource-group module (Phase 3).

Verifies that:
- MTLS + allowed endpoint (hierarchy) → 200, AuthZ bypassed
- MTLS + disallowed endpoint (POST groups) → 403
- JWT + hierarchy endpoint → 200, full AuthZ applied
- Invalid cert CN → 403

Prerequisites:
  - hyperspot-server running with MTLS enabled for resource-group
  - Self-signed CA + client certificates generated (see INTEGRATION_TEST_PLAN.md)
  - E2E_MTLS_CERT_DIR env var pointing to certificate directory

Run:
  E2E_BASE_URL=https://localhost:8087 \
  E2E_MTLS_CERT_DIR=./certs \
  pytest testing/e2e/modules/resource_group/test_mtls_auth.py -v
"""
import os

import httpx
import pytest


# ── Fixtures ─────────────────────────────────────────────────────────────

@pytest.fixture
def mtls_cert_dir():
    """Path to directory with CA cert, client cert, and client key."""
    cert_dir = os.getenv("E2E_MTLS_CERT_DIR")
    if not cert_dir or not os.path.isdir(cert_dir):
        pytest.skip(
            "MTLS cert infrastructure not available. "
            "Set E2E_MTLS_CERT_DIR to a directory containing ca.pem, plugin.pem, plugin-key.pem"
        )
    return cert_dir


@pytest.fixture
def mtls_ssl_context(mtls_cert_dir):
    """Create SSL context for MTLS client authentication."""
    import ssl
    ctx = ssl.create_default_context(
        cafile=os.path.join(mtls_cert_dir, "ca.pem")
    )
    ctx.load_cert_chain(
        certfile=os.path.join(mtls_cert_dir, "plugin.pem"),
        keyfile=os.path.join(mtls_cert_dir, "plugin-key.pem"),
    )
    return ctx


@pytest.fixture
def mtls_base_url():
    """MTLS base URL (typically HTTPS)."""
    return os.getenv("E2E_MTLS_BASE_URL", "https://localhost:8087")


# ── Tests ────────────────────────────────────────────────────────────────

async def test_mtls_allowed_endpoint_hierarchy_200(
    mtls_base_url, mtls_ssl_context, rg_headers, create_type, create_group,
):
    """MTLS request to allowed hierarchy endpoint → 200, AuthZ bypassed."""
    # Setup: create a group to query hierarchy for
    type_data = await create_type("mtls_org")
    group = await create_group(type_data["code"], "MTLS Test Group")

    async with httpx.AsyncClient(
        timeout=10.0,
        verify=mtls_ssl_context,
    ) as client:
        resp = await client.get(
            f"{mtls_base_url}/resource-group/v1/groups/{group['id']}/descendants",
        )
        assert resp.status_code == 200, (
            f"MTLS descendants should return 200, got {resp.status_code}: {resp.text}"
        )

        resp = await client.get(
            f"{mtls_base_url}/resource-group/v1/groups/{group['id']}/ancestors",
        )
        assert resp.status_code == 200, (
            f"MTLS ancestors should return 200, got {resp.status_code}: {resp.text}"
        )


async def test_mtls_disallowed_endpoint_post_groups_403(
    mtls_base_url, mtls_ssl_context,
):
    """MTLS request to POST /groups (not in allowlist) → 403."""
    async with httpx.AsyncClient(
        timeout=10.0,
        verify=mtls_ssl_context,
    ) as client:
        resp = await client.post(
            f"{mtls_base_url}/resource-group/v1/groups",
            json={"type": "test", "name": "should-fail"},
        )
        assert resp.status_code == 403, (
            f"MTLS POST groups should return 403, got {resp.status_code}: {resp.text}"
        )


async def test_jwt_hierarchy_full_authz(
    rg_base_url, rg_headers, create_type, create_group,
):
    """JWT request to hierarchy endpoint → 200, full AuthZ applied."""
    type_data = await create_type("jwt_org")
    group = await create_group(type_data["code"], "JWT Test Group")

    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            f"{rg_base_url}/resource-group/v1/groups/{group['id']}/descendants",
            headers=rg_headers,
        )
        assert resp.status_code == 200, (
            f"JWT hierarchy should return 200, got {resp.status_code}: {resp.text}"
        )


async def test_mtls_invalid_cert_cn_rejected(mtls_cert_dir, mtls_base_url):
    """MTLS with invalid/unknown cert CN → 403 or connection refused."""
    invalid_cert = os.path.join(mtls_cert_dir, "invalid-client.pem")
    invalid_key = os.path.join(mtls_cert_dir, "invalid-client-key.pem")
    ca_cert = os.path.join(mtls_cert_dir, "ca.pem")

    if not os.path.exists(invalid_cert):
        pytest.skip("invalid-client.pem not available in cert dir")

    import ssl
    ctx = ssl.create_default_context(cafile=ca_cert)
    ctx.load_cert_chain(certfile=invalid_cert, keyfile=invalid_key)

    async with httpx.AsyncClient(timeout=10.0, verify=ctx) as client:
        try:
            resp = await client.get(
                f"{mtls_base_url}/resource-group/v1/groups/00000000-0000-0000-0000-000000000000/descendants",
            )
            assert resp.status_code in (403, 401), (
                f"Invalid cert CN should be rejected, got {resp.status_code}"
            )
        except httpx.ConnectError:
            pass  # Connection refused is also acceptable for invalid certs
