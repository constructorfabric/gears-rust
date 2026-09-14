"""CredStore E2E: GTS secret types and their enforceable traits.

Types are chosen at creation via the optional ``type`` field — a **full GTS
type id** (default ``generic``; PR #4204 review C9: the catalog short name and
the raw type UUID are no longer accepted). Types are immutable and drive trait
enforcement: permitted sharing modes (``allow_sharing``), embedded value
schemas, and expiry.
"""
import httpx
import pytest


def _type_id(leaf: str) -> str:
    """Full GTS type id for a built-in secret type (leaf uses underscores)."""
    return f"gts.cf.core.credstore.secret.v1~cf.core.credstore.{leaf}.v1~"


GENERIC = _type_id("generic")
API_KEY = _type_id("api_key")
PERSONAL_TOKEN = _type_id("personal_token")
OAUTH2_CLIENT = _type_id("oauth2_client")
CONNECTION_STRING = _type_id("connection_string")
BEARER_TOKEN = _type_id("bearer_token")


class TestTypeSelection:
    """Type defaults, metadata echo, and validation."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_default_type_is_generic_and_echoed_in_metadata(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """An untyped secret is generic; a typed one reports its type."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-type"))
        typed_ref = cleanup(tenant_a_headers, unique_ref("e2e-type"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={"reference": ref, "value": "v", "sharing": "tenant"},
            )
            assert resp.status_code == 201, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["metadata"]["type"] == GENERIC

            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": typed_ref,
                    "value": "sk-123",
                    "sharing": "tenant",
                    "type": API_KEY,
                },
            )
            assert resp.status_code == 201, resp.text
            resp = await client.get(
                f"{secrets_url}/{typed_ref}", headers=tenant_a_headers
            )
            assert resp.json()["metadata"]["type"] == API_KEY

    @pytest.mark.asyncio
    async def test_unknown_type_rejected(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """A well-formed but unregistered GTS type id is a 400."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": unique_ref("e2e-type"),
                    "value": "v",
                    "sharing": "tenant",
                    "type": _type_id("no_such_type"),
                },
            )
            assert resp.status_code == 400, resp.text
            assert "UNKNOWN_SECRET_TYPE" in resp.text

    @pytest.mark.asyncio
    async def test_non_gts_type_spelling_rejected(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """A catalog short name / raw UUID is not a GTS type id → 400."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            for bad in ("api-key", "00000000-0000-0000-0000-000000000000"):
                resp = await client.post(
                    secrets_url,
                    headers=tenant_a_headers,
                    json={
                        "reference": unique_ref("e2e-type"),
                        "value": "v",
                        "sharing": "tenant",
                        "type": bad,
                    },
                )
                assert resp.status_code == 400, (bad, resp.text)

    @pytest.mark.asyncio
    async def test_type_is_immutable(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """Changing an existing secret's type is rejected; omitting keeps it."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-type"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": ref,
                    "value": "sk-1",
                    "sharing": "tenant",
                    "type": API_KEY,
                },
            )
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": "*"},
                json={"value": "sk-2", "sharing": "tenant", "type": GENERIC},
            )
            assert resp.status_code == 400, resp.text
            assert "TYPE_IMMUTABLE" in resp.text

            # Untyped overwrite inherits the stored type.
            resp = await client.put(
                f"{secrets_url}/{ref}",
                headers={**tenant_a_headers, "If-Match": "*"},
                json={"value": "sk-3", "sharing": "tenant"},
            )
            assert resp.status_code == 204, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            assert resp.json()["metadata"]["type"] == API_KEY
            assert resp.json()["value"] == "sk-3"


class TestAllowSharingTrait:
    """The flagship trait: per-type sharing-mode restrictions."""

    @pytest.mark.smoke
    @pytest.mark.asyncio
    async def test_personal_token_is_private_only(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """personal-token can never be tenant-wide or shared."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-ptok"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            for sharing in ("tenant", "shared"):
                resp = await client.post(
                    secrets_url,
                    headers=tenant_a_headers,
                    json={
                        "reference": ref,
                        "value": "tok",
                        "sharing": sharing,
                        "type": PERSONAL_TOKEN,
                    },
                )
                assert resp.status_code == 400, (sharing, resp.text)
                assert "SHARING_NOT_ALLOWED_FOR_TYPE" in resp.text

            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": ref,
                    "value": "tok",
                    "sharing": "private",
                    "type": PERSONAL_TOKEN,
                },
            )
            assert resp.status_code == 201, resp.text

    @pytest.mark.asyncio
    async def test_connection_string_is_tenant_only(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """connection-string cannot be shared down the hierarchy."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": unique_ref("e2e-dsn"),
                    "value": "postgres://u:p@host/db",
                    "sharing": "shared",
                    "type": CONNECTION_STRING,
                },
            )
            assert resp.status_code == 400, resp.text
            assert "SHARING_NOT_ALLOWED_FOR_TYPE" in resp.text


class TestValueSchemaTrait:
    """Structured types validate their value against an embedded schema."""

    @pytest.mark.asyncio
    async def test_oauth2_client_schema_enforced(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """oauth2-client requires client_id + client_secret JSON."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-oauth2"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            # Valid payload passes.
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": ref,
                    "value": '{"client_id": "cid", "client_secret": "s3cr3t"}',
                    "sharing": "tenant",
                    "type": OAUTH2_CLIENT,
                },
            )
            assert resp.status_code == 201, resp.text

            # Missing required field and non-JSON payloads are rejected —
            # and the error must not echo the secret material.
            for bad in ('{"client_id": "cid-only"}', "not-json-at-all"):
                resp = await client.post(
                    secrets_url,
                    headers=tenant_a_headers,
                    json={
                        "reference": unique_ref("e2e-oauth2"),
                        "value": bad,
                        "sharing": "tenant",
                        "type": OAUTH2_CLIENT,
                    },
                )
                assert resp.status_code == 400, resp.text
                assert "VALUE_SCHEMA_VIOLATION" in resp.text
                assert "cid-only" not in resp.text, "must not echo the value"


class TestExpiryTrait:
    """expirable types accept expires_at; expired secrets stop resolving."""

    @pytest.mark.asyncio
    async def test_expiry_rejected_for_non_expirable_type(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """generic (and api-key) secrets cannot carry expires_at."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": unique_ref("e2e-exp"),
                    "value": "v",
                    "sharing": "tenant",
                    "expires_at": "2099-01-01T00:00:00Z",
                },
            )
            assert resp.status_code == 400, resp.text
            assert "EXPIRY_NOT_SUPPORTED_FOR_TYPE" in resp.text

    @pytest.mark.asyncio
    async def test_expirable_type_roundtrip_and_past_expiry_rejected(
        self, secrets_url, tenant_a_headers, unique_ref, cleanup
    ):
        """bearer-token accepts a future expiry and echoes it; past is 400."""
        ref = cleanup(tenant_a_headers, unique_ref("e2e-exp"))

        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": ref,
                    "value": "tok",
                    "sharing": "tenant",
                    "type": BEARER_TOKEN,
                    "expires_at": "2099-01-01T00:00:00Z",
                },
            )
            assert resp.status_code == 201, resp.text
            resp = await client.get(f"{secrets_url}/{ref}", headers=tenant_a_headers)
            meta = resp.json()["metadata"]
            assert meta["type"] == BEARER_TOKEN
            assert meta["expires_at"].startswith("2099-01-01T00:00:00")

            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": unique_ref("e2e-exp"),
                    "value": "tok",
                    "sharing": "tenant",
                    "type": BEARER_TOKEN,
                    "expires_at": "2000-01-01T00:00:00Z",
                },
            )
            assert resp.status_code == 400, resp.text
            assert "EXPIRY_IN_THE_PAST" in resp.text

    @pytest.mark.asyncio
    async def test_malformed_expires_at_is_400(
        self, secrets_url, tenant_a_headers, unique_ref
    ):
        """expires_at must be RFC 3339."""
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.post(
                secrets_url,
                headers=tenant_a_headers,
                json={
                    "reference": unique_ref("e2e-exp"),
                    "value": "tok",
                    "sharing": "tenant",
                    "type": BEARER_TOKEN,
                    "expires_at": "tomorrow",
                },
            )
            assert resp.status_code == 400, resp.text
            assert "INVALID_EXPIRES_AT" in resp.text
