"""E2E integration-seam tests for the file-storage control plane.

Each test targets exactly one seam that unit tests cannot see (real HTTP +
real PostgreSQL + real AuthZ wiring). Domain invariants, CHECK constraints, and
the migration SQL are covered by the Rust unit/integration suite
(``gears/file-storage/file-storage``) and are deliberately NOT retested here.

The whole module is gated by ``require_file_storage_mounted`` (see conftest):
it skips until the M5 endpoints are mounted, then runs for real.

Seam map (api.md §"P1 — Control plane"):
  - route registration                → test_route_smoke_endpoints_registered
  - backend discovery JSON shape       → test_list_storages_returns_capabilities
  - signed upload-URL issuance         → test_create_file_returns_signed_upload_url
  - error middleware (RFC 9457)        → test_unknown_file_returns_problem_json
  - AuthN wiring (auth required)       → test_unauthenticated_request_is_rejected
"""
import asyncio
import uuid

import httpx
import pytest

REQUEST_TIMEOUT = 5.0


@pytest.mark.asyncio
async def test_route_smoke_endpoints_registered(base_url, api_base, auth_headers):
    """Seam: route registration — handlers mounted on the expected paths.

    A handler missing from ``gear.rs`` passes every unit test but 404s here.
    """
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        responses = await asyncio.gather(
            client.request("OPTIONS", f"{base_url}{api_base}/files", headers=auth_headers),
            client.request("OPTIONS", f"{base_url}{api_base}/storages", headers=auth_headers),
        )
    for r in responses:
        assert r.status_code != 404, f"endpoint not registered: {r.request.url}"


@pytest.mark.asyncio
async def test_list_storages_returns_capabilities(base_url, api_base, auth_headers):
    """Seam: backend discovery JSON wire format (GET /storages).

    Verifies the read-only capability surface that consumers use to discover
    available backends (cpt-cf-file-storage-fr-backend-capabilities).
    """
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        r = await client.get(f"{base_url}{api_base}/storages", headers=auth_headers)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    assert r.headers.get("content-type", "").startswith("application/json")
    # Lock the wire format: GET /storages returns a top-level JSON array
    # (Json(Vec<StorageDto>)). Accepting alternative envelopes here would let a
    # breaking shape change slip through this seam.
    data = r.json()
    assert isinstance(data, list), f"GET /storages must return a JSON array, got: {data!r}"


@pytest.mark.asyncio
async def test_create_file_returns_signed_upload_url(base_url, api_base, auth_headers, gts_file_type):
    """Seam: signed upload-URL issuance (POST /files).

    The control plane must return ``{file_id, version_id, upload_url}`` and the
    upload_url must point at the sidecar, never a backend (backend opacity).
    """
    body = {
        "owner_kind": "user",
        "owner_id": str(uuid.uuid4()),
        "name": f"e2e-{uuid.uuid4()}.txt",
        "gts_file_type": gts_file_type,
        "mime_type": "text/plain",
    }
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        r = await client.post(f"{base_url}{api_base}/files", json=body, headers=auth_headers)
    assert r.status_code == 201, f"expected 201, got {r.status_code}: {r.text}"
    data = r.json()
    for field in ("file_id", "version_id", "upload_url"):
        assert field in data, f"missing field {field!r} in: {data!r}"
    assert isinstance(data["file_id"], str)
    # Backend opacity: the signed URL must route through the sidecar data-plane
    # (`/api/file-storage-data/...`), never expose a raw backend URL.
    assert "/api/file-storage-data/" in data["upload_url"], (
        f"upload_url must target the sidecar data-plane, got: {data['upload_url']!r}"
    )


@pytest.mark.asyncio
async def test_unknown_file_returns_problem_json(base_url, api_base, auth_headers):
    """Seam: error middleware — DomainError → 404 + application/problem+json.

    Unit tests check the DomainError→Problem mapping; only HTTP proves the
    Content-Type header and that no internals leak.
    """
    missing = uuid.uuid4()
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        r = await client.get(f"{base_url}{api_base}/files/{missing}", headers=auth_headers)
    assert r.status_code == 404, f"expected 404, got {r.status_code}: {r.text}"
    assert "application/problem+json" in r.headers.get("content-type", "")
    body = r.json()
    assert body.get("status") == 404
    for leaked in ("stack", "trace", "backtrace"):
        assert leaked not in body, f"internal field {leaked!r} leaked in error body"


@pytest.mark.asyncio
async def test_unauthenticated_request_is_rejected(base_url, api_base):
    """Seam: AuthN wiring — the control-plane surface is auth-required (§5.3).

    FileStorage P1 has no anonymous namespace; every endpoint goes through
    platform authentication. With no token, a protected read must be rejected.
    """
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
        r = await client.get(f"{base_url}{api_base}/files/{uuid.uuid4()}")
    assert r.status_code in (401, 403), f"expected 401/403 without auth, got {r.status_code}"
