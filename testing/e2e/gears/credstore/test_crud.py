"""CredStore E2E: CRUD semantics of the REST API.

Covers the core lifecycle (PUT upsert / POST create-only / GET / DELETE),
response headers on the value-bearing GET, reference validation, the single
404 surface, and authentication.
"""
import httpx
import pytest


class TestCrudLifecycle:
    """Create / read / update / delete through the gateway."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_put_get_delete_roundtrip(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """PUT creates, GET returns value + metadata, DELETE removes."""
        ref = cleanup(tenant_a_headers, unique_ref())

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "e2e-demo-value-1", "sharing": "tenant"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 200, resp.text
            body = resp.json()
            assert body["value"] == "e2e-demo-value-1"
            meta = body["metadata"]
            assert meta["sharing"] == "tenant"
            assert meta["is_inherited"] is False
            assert meta["version"] == 1

            resp = await client.delete(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 404

    @pytest.mark.asyncio
    async def test_put_upsert_overwrites_value(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """A second PUT replaces the value (idempotent upsert)."""
        ref = cleanup(tenant_a_headers, unique_ref())

        async with httpx.AsyncClient(timeout=10.0) as client:
            for value in ("first", "second"):
                resp = await client.put(
                    f"{secrets_url}/{ref}",
                    headers=tenant_a_headers,
                    json={"value": value, "sharing": "tenant"},
                )
                assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 200
            assert resp.json()["value"] == "second"

    @pytest.mark.asyncio
    async def test_post_is_create_only_with_location_and_conflict(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """POST returns 201 + Location; a duplicate POST returns 409."""
        ref = cleanup(tenant_a_headers, unique_ref())

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={"reference": ref, "value": "v1", "sharing": "tenant"},
            )
            assert resp.status_code == 201, resp.text
            assert resp.headers.get("location", "").endswith(f"/secrets/{ref}")

            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={"reference": ref, "value": "v2", "sharing": "tenant"},
            )
            assert resp.status_code == 409, resp.text

            # The conflicting POST must not have overwritten the value.
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["value"] == "v1"

    @pytest.mark.asyncio
    async def test_reference_reusable_after_delete(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """A completed delete releases the reference for re-creation."""
        ref = cleanup(tenant_a_headers, unique_ref())

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={"reference": ref, "value": "old", "sharing": "tenant"},
            )
            assert resp.status_code == 201, resp.text
            resp = await client.delete(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 204, resp.text

            # Happy-path delete completes its saga fully: POST succeeds again.
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={"reference": ref, "value": "new", "sharing": "tenant"},
            )
            assert resp.status_code == 201, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["value"] == "new"
            # A fresh secret starts a fresh version counter.
            assert resp.json()["metadata"]["version"] == 1

    @pytest.mark.asyncio
    async def test_delete_is_not_idempotent_at_http_level(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """The second DELETE of the same reference is a 404."""
        ref = cleanup(tenant_a_headers, unique_ref())

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v", "sharing": "tenant"},
            )
            resp = await client.delete(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 204
            resp = await client.delete(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 404


class TestResponseHeaders:
    """Confidentiality / concurrency headers on the value-bearing GET."""

    @pytest.mark.asyncio
    async def test_get_returns_etag_and_no_store(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """GET carries a strong generation-bound ETag and Cache-Control: no-store."""
        import re

        ref = cleanup(tenant_a_headers, unique_ref())

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v", "sharing": "tenant"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.status_code == 200
            # Strong validator: "<row-uuid>.<version>", version 1 on create.
            assert re.fullmatch(
                r'"[0-9a-f-]{36}\.1"', resp.headers.get("etag", "")
            ), resp.headers.get("etag")
            assert resp.headers.get("cache-control") == "no-store"


class TestValidationAndErrors:
    """Reference validation, 404 surface, authentication."""

    @pytest.mark.asyncio
    async def test_invalid_reference_rejected(self, secrets_url, tenant_a_headers):
        """A reference outside [a-zA-Z0-9_-]+ is a 400, not a 404."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            # POST validates the body-supplied reference.
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={"reference": "has:colon", "value": "v", "sharing": "tenant"},
            )
            assert resp.status_code == 400, resp.text

            # Path-supplied reference on GET (percent-encoded colon).
            resp = await client.get(
                f"{secrets_url}/has%3Acolon", headers=tenant_a_headers
            )
            assert resp.status_code == 400, resp.text

    @pytest.mark.asyncio
    async def test_get_missing_secret_is_404(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """A never-created reference yields the canonical 404."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(
                f"{secrets_url}/{unique_ref()}", headers=tenant_a_headers
            )
            assert resp.status_code == 404

    @pytest.mark.asyncio
    async def test_missing_token_is_401(self, secrets_url, unique_ref):
        """All routes are authenticated."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(f"{secrets_url}/{unique_ref()}")
            assert resp.status_code == 401
