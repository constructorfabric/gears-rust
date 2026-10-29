"""E2E integration-seam tests for the file-storage control plane.

Each test targets exactly one seam that unit tests cannot see (real HTTP +
real PostgreSQL + real AuthZ wiring). Domain invariants and the migration SQL
are covered by the Rust suite (``gears/file-storage/file-storage``) and are
deliberately NOT retested here.

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

pytestmark = pytest.mark.usefixtures("require_file_storage_mounted")

# Every public control-plane route. s2s callbacks (finalize / part report) are
# internal-token routes and are exercised by the lifecycle suites.
# (method, path, json body). Writes use an empty body or a random id so the
# probe cannot change state; PUT /policy is covered by GET /policy (same path).
_ROUTES = [
    ("GET", "/files", None),
    ("POST", "/files", {}),
    ("GET", "/files/{id}", None),
    ("PATCH", "/files/{id}", {}),
    ("DELETE", "/files/{id}", None),
    ("POST", "/files/{id}/bind", {}),
    ("GET", "/files/{id}/download-url", None),
    ("GET", "/files/{id}/versions", None),
    ("POST", "/files/{id}/versions", None),
    ("DELETE", "/files/{id}/versions/{id}", None),
    ("POST", "/files/{id}/multipart", {}),
    ("GET", "/files/{id}/multipart/{id}", None),
    ("POST", "/files/{id}/multipart/{id}/complete", None),
    ("DELETE", "/files/{id}/multipart/{id}", None),
    ("POST", "/files/{id}/migrate", {}),
    ("POST", "/files/{id}/transfer", {}),
    ("GET", "/storages", None),
    ("GET", "/storages/{id}", None),
    ("GET", "/policy", None),
    ("GET", "/policy/effective", None),
    ("GET", "/retention-rules", None),
    ("POST", "/retention-rules", {}),
    ("DELETE", "/retention-rules/{id}", None),
]


@pytest.mark.asyncio
async def test_route_smoke_endpoints_registered(base_url, api_base, auth_headers):
    """Seam: route registration — handlers mounted on the expected method + path.

    A handler missing from ``gear.rs`` passes every unit test but 404s/405s here.
    A 404 is accepted only for a path with an ``{id}`` parameter when the
    problem ``detail`` names the random id we sent: that is the handler's own
    "resource not found", proving the route matched (``instance`` is ignored,
    it echoes the request path even for an unmatched route).
    """
    probe = str(uuid.uuid4())

    def names_probe(r):
        try:
            return probe in str(r.json().get("detail", ""))
        except ValueError:
            return False

    async def call(method, path, body):
        url = f"{base_url}{api_base}{path.replace('{id}', probe)}"
        async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
            return method, path, await client.request(method, url, json=body, headers=auth_headers)

    results = await asyncio.gather(*(call(*route) for route in _ROUTES))
    unregistered = [
        f"{method} {path} -> {r.status_code}"
        for method, path, r in results
        if r.status_code == 405
        or (r.status_code == 404 and not ("{id}" in path and names_probe(r)))
    ]
    assert not unregistered, f"endpoints not registered: {unregistered}"


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
    # Lock the wire format: GET /storages returns a cursor page
    # (Page<StorageDto>: `{items, page_info}`), never a bare JSON array.
    # Accepting alternative envelopes here would let a breaking shape change
    # slip through this seam.
    data = r.json()
    assert isinstance(data, dict), f"GET /storages must return a page object, got: {data!r}"
    items = data.get("items")
    assert isinstance(items, list) and items, f"GET /storages must list at least one backend, got: {data!r}"
    for item in items:
        assert isinstance(item.get("id"), str), f"backend without a string id: {item!r}"
        assert isinstance(item.get("capabilities"), dict), f"backend without capabilities: {item!r}"
    page_info = data.get("page_info")
    assert isinstance(page_info, dict) and "limit" in page_info, f"missing page_info.limit in: {data!r}"


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
