"""E2E tests for Gateway Scope Enforcement middleware.

These tests verify that the API Gateway correctly enforces token scope requirements
at the gateway level, rejecting requests early with 403 Forbidden when scopes are
insufficient.

Prerequisites:
- Run via: make e2e-local SUITE=scope-enforcement
  (applies testing/e2e/suites/scope_enforcement/e2e.yaml over config/e2e-local.yaml
  and sets E2E_SCOPE_ENFORCEMENT=1)
- The overlay defines tokens with specific scopes for testing

Test scenarios:
1. First-party apps (token_scopes: ["*"]) always pass
2. Tokens with matching scopes pass
3. Tokens without required scopes get 403 Forbidden
4. Glob patterns work correctly for route matching
"""
import httpx
import pytest

from .conftest import make_headers


class TestScopeEnforcementBasic:
    """Basic scope enforcement tests."""

    @pytest.mark.asyncio
    async def test_full_access_token_passes(
        self, base_url, tenant_id, token_full_access
    ):
        """First-party app with '*' scope should always pass scope checks."""
        headers = make_headers(tenant_id, token_full_access)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            # Should pass scope check (may fail later for other reasons, but not 403 from scope)
            assert resp.status_code != 403 or "scope" not in resp.text.lower()

    @pytest.mark.asyncio
    async def test_matching_scope_passes(
        self, base_url, tenant_id, token_users_read
    ):
        """Token with 'users:read' scope should access /users-info/v1/users."""
        headers = make_headers(tenant_id, token_users_read)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            # Should pass scope check - 200 OK or other non-403 status
            assert resp.status_code != 403 or "scope" not in resp.text.lower()

    @pytest.mark.asyncio
    async def test_admin_scope_passes(
        self, base_url, tenant_id, token_users_admin
    ):
        """Token with 'users:admin' scope should access /users-info/v1/users."""
        headers = make_headers(tenant_id, token_users_admin)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            # Should pass scope check
            assert resp.status_code != 403 or "scope" not in resp.text.lower()


class TestScopeEnforcementDenied:
    """Tests for scope enforcement denial (403 Forbidden)."""

    @pytest.mark.asyncio
    async def test_wrong_scope_returns_403(
        self, base_url, tenant_id, token_cities_admin
    ):
        """Token with 'cities:admin' scope should be denied access to /users-info/v1/users."""
        headers = make_headers(tenant_id, token_cities_admin)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            assert resp.status_code == 403
            body = resp.json()
            assert body["status"] == 403
            assert body["title"] == "Permission Denied"
            assert body["context"]["reason"] == "INSUFFICIENT_SCOPES"

    @pytest.mark.asyncio
    async def test_unrelated_scope_returns_403(
        self, base_url, tenant_id, token_no_scopes
    ):
        """Token with unrelated scopes should be denied access to /users-info/v1/users."""
        headers = make_headers(tenant_id, token_no_scopes)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            assert resp.status_code == 403
            body = resp.json()
            assert body["status"] == 403
            assert body["title"] == "Permission Denied"
            assert body["context"]["reason"] == "INSUFFICIENT_SCOPES"


class TestScopeEnforcementGlobPatterns:
    """Tests for glob pattern matching in scope enforcement."""

    @pytest.mark.asyncio
    async def test_single_star_matches_path_segment(
        self, base_url, tenant_id, token_users_read
    ):
        """Single * pattern should match single path segment.

        Route config: /users-info/v1/users/* requires users:read or users:admin
        """
        headers = make_headers(tenant_id, token_users_read)

        async with httpx.AsyncClient(timeout=30.0) as client:
            # Should match /users-info/v1/users/{id}
            resp = await client.get(
                f"{base_url}/users-info/v1/users/11111111-1111-1111-1111-111111111111",
                headers=headers,
            )
            # Should pass scope check (may get 404 for non-existent user, but not 403)
            assert resp.status_code != 403 or "scope" not in resp.text.lower()

    @pytest.mark.asyncio
    async def test_single_star_denied_wrong_scope(
        self, base_url, tenant_id, token_cities_admin
    ):
        """Single * pattern should deny access with wrong scope."""
        headers = make_headers(tenant_id, token_cities_admin)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users/11111111-1111-1111-1111-111111111111",
                headers=headers,
            )
            assert resp.status_code == 403
            body = resp.json()
            assert body["status"] == 403
            assert body["title"] == "Permission Denied"
            assert body["context"]["reason"] == "INSUFFICIENT_SCOPES"

    @pytest.mark.asyncio
    async def test_cities_exact_path_with_correct_scope(
        self, base_url, tenant_id, token_cities_admin
    ):
        """Exact path match should pass with correct scope.

        Route config: /users-info/v1/cities requires cities:admin
        """
        headers = make_headers(tenant_id, token_cities_admin)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/cities",
                headers=headers,
            )
            # Should pass scope check
            assert resp.status_code != 403 or "scope" not in resp.text.lower()

    @pytest.mark.asyncio
    async def test_cities_exact_path_denied_wrong_scope(
        self, base_url, tenant_id, token_users_read
    ):
        """Exact path match should deny access with wrong scope."""
        headers = make_headers(tenant_id, token_users_read)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/cities",
                headers=headers,
            )
            assert resp.status_code == 403
            body = resp.json()
            assert body["status"] == 403
            assert body["title"] == "Permission Denied"
            assert body["context"]["reason"] == "INSUFFICIENT_SCOPES"


class TestScopeEnforcementEdgeCases:
    """Edge case tests for scope enforcement."""

    @pytest.mark.asyncio
    async def test_no_auth_header_returns_401(self, base_url, tenant_id):
        """Request without Authorization header should get 401, not 403."""
        headers = {"x-tenant-id": tenant_id}

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            # Should be 401 Unauthorized (auth middleware), not 403 (scope enforcement)
            assert resp.status_code == 401

    @pytest.mark.asyncio
    async def test_invalid_token_returns_401(self, base_url, tenant_id):
        """Request with invalid token should get 401, not 403."""
        headers = make_headers(tenant_id, "invalid-token-xyz")

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{base_url}/users-info/v1/users",
                headers=headers,
            )
            # Should be 401 Unauthorized (auth middleware), not 403 (scope enforcement)
            assert resp.status_code == 401

    @pytest.mark.asyncio
    async def test_unconfigured_route_passes(
        self, base_url, tenant_id, token_no_scopes
    ):
        """Routes not configured in gateway_scope_checks should pass scope enforcement.

        The /docs endpoint is not configured, so any authenticated request should pass.
        """
        headers = make_headers(tenant_id, token_no_scopes)

        async with httpx.AsyncClient(timeout=10.0) as client:
            # /docs is typically public and not in scope config
            resp = await client.get(
                f"{base_url}/docs",
                headers=headers,
            )
            # Should not be 403 from scope enforcement
            assert resp.status_code != 403
