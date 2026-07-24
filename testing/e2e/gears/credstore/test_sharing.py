"""CredStore E2E: sharing modes and hierarchical resolution.

Uses the static tenant tree: hierarchy-root (...0001) is the parent of
hierarchy-l1a (...0002) and hierarchy-l1b (...0005). Each token carries a
distinct subject, so a parent's ``private`` secret is never visible to the
child tenants' subjects.
"""
import httpx
import pytest

from .conftest import HIERARCHY_ROOT


class TestHierarchicalResolution:
    """shared secrets are inherited downward; tenant/private are not."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_shared_secret_inherited_by_child(
        self, secrets_url, root_headers, l1a_headers, unique_ref, cleanup
    ):
        """A parent's shared secret resolves from the child with metadata."""
        ref = cleanup(root_headers, unique_ref("e2e-shared"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=root_headers,
                json={"value": "parent-shared-value", "sharing": "shared"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            assert resp.status_code == 200, resp.text
            body = resp.json()
            assert body["value"] == "parent-shared-value"
            meta = body["metadata"]
            assert meta["is_inherited"] is True
            assert meta["owner_tenant_id"] == HIERARCHY_ROOT
            assert meta["sharing"] == "shared"

            # The owner itself resolves it as its own (not inherited).
            resp = await client.get(f"{secrets_url}/{ref}", headers=root_headers)
            assert resp.json()["metadata"]["is_inherited"] is False

    @pytest.mark.asyncio
    async def test_tenant_secret_not_inherited_by_child(
        self, secrets_url, root_headers, l1a_headers, unique_ref, cleanup
    ):
        """tenant sharing stays within the owning tenant."""
        ref = cleanup(root_headers, unique_ref("e2e-tenant"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=root_headers,
                json={"value": "parent-tenant-value", "sharing": "tenant"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            assert resp.status_code == 404, resp.text

    @pytest.mark.asyncio
    async def test_private_secret_not_inherited_by_child(
        self, secrets_url, root_headers, l1a_headers, unique_ref, cleanup
    ):
        """A parent's private secret is invisible to a child subject."""
        ref = cleanup(root_headers, unique_ref("e2e-priv"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=root_headers,
                json={"value": "parent-private-value", "sharing": "private"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            assert resp.status_code == 404, resp.text

            # The owner still reads it.
            resp = await client.get(f"{secrets_url}/{ref}", headers=root_headers)
            assert resp.status_code == 200
            assert resp.json()["metadata"]["sharing"] == "private"

    @pytest.mark.asyncio
    async def test_resolution_is_upward_only(
        self, secrets_url, root_headers, l1a_headers, unique_ref, cleanup
    ):
        """A child's shared secret is not visible to its parent."""
        ref = cleanup(l1a_headers, unique_ref("e2e-up"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=l1a_headers,
                json={"value": "child-shared-value", "sharing": "shared"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=root_headers)
            assert resp.status_code == 404, resp.text

    @pytest.mark.asyncio
    async def test_sibling_does_not_leak(
        self, secrets_url, l1a_headers, l1b_headers, unique_ref, cleanup
    ):
        """A shared secret in one subtree is invisible to a sibling subtree."""
        ref = cleanup(l1a_headers, unique_ref("e2e-sib"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=l1a_headers,
                json={"value": "l1a-shared-value", "sharing": "shared"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=l1b_headers)
            assert resp.status_code == 404, resp.text


class TestShadowing:
    """The closest accessible secret wins; deletion restores fallback."""

    @pytest.mark.asyncio
    async def test_child_shadows_parent_and_fallback_after_delete(
        self, secrets_url, root_headers, l1a_headers, unique_ref, cleanup
    ):
        """Child's own secret wins over the parent's shared one."""
        ref = unique_ref("e2e-shadow")
        cleanup(root_headers, ref)
        cleanup(l1a_headers, ref)

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=root_headers,
                json={"value": "parent-value", "sharing": "shared"},
            )
            assert resp.status_code == 204, resp.text
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=l1a_headers,
                json={"value": "child-override", "sharing": "tenant"},
            )
            assert resp.status_code == 204, resp.text

            # The child resolves its own secret (shadowing).
            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            body = resp.json()
            assert body["value"] == "child-override"
            assert body["metadata"]["is_inherited"] is False

            # Deleting the override restores fallback to the parent's secret.
            resp = await client.delete(f"{secrets_url}/{ref}", headers=l1a_headers)
            assert resp.status_code == 204, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            body = resp.json()
            assert body["value"] == "parent-value"
            assert body["metadata"]["is_inherited"] is True

    @pytest.mark.asyncio
    async def test_deleting_shared_secret_revokes_child_access(
        self, secrets_url, root_headers, l1a_headers, unique_ref, cleanup
    ):
        """Descendants lose access immediately when the owner deletes."""
        ref = cleanup(root_headers, unique_ref("e2e-revoke"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=root_headers,
                json={"value": "to-be-revoked", "sharing": "shared"},
            )
            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            assert resp.status_code == 200

            resp = await client.delete(f"{secrets_url}/{ref}", headers=root_headers)
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=l1a_headers)
            assert resp.status_code == 404


class TestSharingClasses:
    """private and tenant/shared secrets coexist under one reference."""

    @pytest.mark.asyncio
    async def test_private_and_tenant_coexist_private_wins_for_owner(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """One reference holds both classes; the owner resolves private."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-coex"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "tenant-wide", "sharing": "tenant"},
            )
            assert resp.status_code == 204, resp.text
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "owner-only", "sharing": "private"},
            )
            assert resp.status_code == 204, resp.text

            # Private beats non-private for its owner.
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            body = resp.json()
            assert body["value"] == "owner-only"
            assert body["metadata"]["sharing"] == "private"

            # DELETE addresses the private class first, unmasking the tenant one.
            resp = await client.delete(
                f"{secrets_url}/{ref}", headers=tenant_a_headers
            )
            assert resp.status_code == 204, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            body = resp.json()
            assert body["value"] == "tenant-wide"
            assert body["metadata"]["sharing"] == "tenant"

    @pytest.mark.asyncio
    async def test_tenant_to_shared_transition_in_place(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """tenant -> shared is an in-place update of the same secret."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-trans"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v", "sharing": "tenant"},
            )
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers=tenant_a_headers,
                json={"value": "v", "sharing": "shared"},
            )
            assert resp.status_code == 204, resp.text

            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            meta = resp.json()["metadata"]
            assert meta["sharing"] == "shared"
            assert meta["version"] == 2, "in-place transition bumps the version"
