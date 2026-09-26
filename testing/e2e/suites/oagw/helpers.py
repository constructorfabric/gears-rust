"""Shared helpers for OAGW E2E tests."""
import re
import uuid
from typing import Optional

import httpx

# ---------------------------------------------------------------------------
# OAGW GTS type catalog — mirrors the Rust catalog in domain/type_catalog.rs
# ---------------------------------------------------------------------------

# Schema GTS identifiers (7)
UPSTREAM_SCHEMA = "gts.cf.core.oagw.upstream.v1~"
ROUTE_SCHEMA = "gts.cf.core.oagw.route.v1~"
PROTOCOL_SCHEMA = "gts.cf.core.oagw.protocol.v1~"
AUTH_PLUGIN_SCHEMA = "gts.cf.core.oagw.auth_plugin.v1~"
GUARD_PLUGIN_SCHEMA = "gts.cf.core.oagw.guard_plugin.v1~"
TRANSFORM_PLUGIN_SCHEMA = "gts.cf.core.oagw.transform_plugin.v1~"
PROXY_SCHEMA = "gts.cf.core.oagw.proxy.v1~"

# Protocol instances (2)
HTTP_PROTOCOL_ID = "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
GRPC_PROTOCOL_ID = "gts.cf.core.oagw.protocol.v1~cf.core.oagw.grpc.v1"

# Auth plugin instances (6)
NOOP_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.noop.v1"
APIKEY_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"
BASIC_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1"
BEARER_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.bearer.v1"
OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1"
OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1"

# Guard plugin instances (3)
TIMEOUT_GUARD_PLUGIN_ID = "gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1"
CORS_GUARD_PLUGIN_ID = "gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.cors.v1"
REQUIRED_HEADERS_GUARD_PLUGIN_ID = "gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.required_headers.v1"

# Transform plugin instances (3)
LOGGING_TRANSFORM_PLUGIN_ID = "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1"
METRICS_TRANSFORM_PLUGIN_ID = "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.metrics.v1"
REQUEST_ID_TRANSFORM_PLUGIN_ID = "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1"

# Grouped for assertions
OAGW_SCHEMAS = [
    UPSTREAM_SCHEMA, ROUTE_SCHEMA, PROTOCOL_SCHEMA,
    AUTH_PLUGIN_SCHEMA, GUARD_PLUGIN_SCHEMA, TRANSFORM_PLUGIN_SCHEMA,
    PROXY_SCHEMA,
]

OAGW_INSTANCES = [
    HTTP_PROTOCOL_ID, GRPC_PROTOCOL_ID,
    NOOP_AUTH_PLUGIN_ID, APIKEY_AUTH_PLUGIN_ID, BASIC_AUTH_PLUGIN_ID,
    BEARER_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
    TIMEOUT_GUARD_PLUGIN_ID, CORS_GUARD_PLUGIN_ID, REQUIRED_HEADERS_GUARD_PLUGIN_ID,
    LOGGING_TRANSFORM_PLUGIN_ID, METRICS_TRANSFORM_PLUGIN_ID,
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
]

ALL_OAGW_GTS_IDS = OAGW_SCHEMAS + OAGW_INSTANCES


async def list_oagw_types(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
) -> list[dict]:
    """List all OAGW entities registered in the types-registry via REST API.

    Queries with namespace=oagw to scope to OAGW entities.
    Returns the list of entity dicts from the response.
    """
    resp = await client.get(
        f"{base_url}/types-registry/v1/entities",
        headers=headers,
        params={"namespace": "oagw"},
    )
    resp.raise_for_status()
    return resp.json().get("entities", [])


def unique_alias(prefix: str = "e2e") -> str:
    """Generate a unique alias to avoid cross-test collisions."""
    short = uuid.uuid4().hex[:8]
    return f"{prefix}-{short}"


def _build_upstream_body(
    mock_url: str,
    alias: Optional[str] = None,
    upstream_headers: Optional[dict] = None,
    **kwargs,
) -> dict:
    """Build the upstream request body from a mock URL and optional overrides."""
    from urllib.parse import urlparse
    parsed = urlparse(mock_url)
    host = parsed.hostname or "127.0.0.1"
    scheme = parsed.scheme or "http"
    port = parsed.port or (443 if scheme == "https" else 80)

    body: dict = {
        "server": {
            "endpoints": [{"host": host, "port": port, "scheme": scheme}],
        },
        "protocol": HTTP_PROTOCOL_ID,
        "enabled": True,
        "tags": [],
    }
    if alias is not None:
        body["alias"] = alias
    if upstream_headers is not None:
        body["headers"] = upstream_headers

    body.update(kwargs)
    return body


async def create_upstream(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    mock_url: str,
    alias: Optional[str] = None,
    upstream_headers: Optional[dict] = None,
    **kwargs,
) -> dict:
    """Create an upstream via the Management API and return the response JSON.

    ``upstream_headers`` maps to the upstream resource ``headers`` field
    (e.g., ``{"request": {"passthrough": "all"}}``).  It is accepted as a
    separate parameter to avoid colliding with the HTTP ``headers`` argument.

    ``kwargs`` are merged into the request body (e.g., ``enabled=False``,
    ``auth={...}``, ``rate_limit={...}``).
    """
    body = _build_upstream_body(mock_url, alias, upstream_headers, **kwargs)

    resp = await client.post(
        f"{base_url}/oagw/v1/upstreams",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )
    resp.raise_for_status()
    return resp.json()


async def create_upstream_raw(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    mock_url: str,
    alias: Optional[str] = None,
    **kwargs,
) -> httpx.Response:
    """Create an upstream and return the raw Response (no raise_for_status).

    Use this when testing error paths (e.g., validation failures returning 400).
    """
    body = _build_upstream_body(mock_url, alias, **kwargs)

    return await client.post(
        f"{base_url}/oagw/v1/upstreams",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )


async def update_upstream_raw(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    upstream_id: str,
    mock_url: str,
    alias: Optional[str] = None,
    **kwargs,
) -> httpx.Response:
    """Replace an upstream via PUT and return the raw Response (no raise_for_status)."""
    body = _build_upstream_body(mock_url, alias, **kwargs)

    return await client.put(
        f"{base_url}/oagw/v1/upstreams/{upstream_id}",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )


async def create_route(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    upstream_id: str,
    methods: list[str],
    path: str,
    **kwargs,
) -> dict:
    """Create a route via the Management API and return the response JSON."""
    body: dict = {
        "upstream_id": upstream_id,
        "match": {
            "http": {
                "methods": methods,
                "path": path,
            },
        },
        "enabled": True,
        "tags": [],
        "priority": 0,
    }
    body.update(kwargs)

    resp = await client.post(
        f"{base_url}/oagw/v1/routes",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )
    resp.raise_for_status()
    return resp.json()


async def update_upstream(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    upstream_id: str,
    mock_url: str,
    alias: Optional[str] = None,
    **kwargs,
) -> dict:
    """Replace an upstream via PUT and return the response JSON.

    Builds a full replacement body from ``mock_url`` (same as
    ``create_upstream``).  ``kwargs`` are merged into the body
    (e.g., ``enabled=False``, ``auth={...}``).
    """
    body = _build_upstream_body(mock_url, alias, **kwargs)

    resp = await client.put(
        f"{base_url}/oagw/v1/upstreams/{upstream_id}",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )
    resp.raise_for_status()
    return resp.json()


async def update_route(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    route_id: str,
    methods: list[str],
    path: str,
    **kwargs,
) -> dict:
    """Replace a route via PUT and return the response JSON.

    ``kwargs`` are merged into the body (e.g., ``priority=5``,
    ``tags=["v2"]``, ``enabled=False``).
    """
    body: dict = {
        "match": {
            "http": {
                "methods": methods,
                "path": path,
            },
        },
        "enabled": True,
        "tags": [],
        "priority": 0,
    }
    body.update(kwargs)

    resp = await client.put(
        f"{base_url}/oagw/v1/routes/{route_id}",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )
    resp.raise_for_status()
    return resp.json()


async def delete_upstream(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    upstream_id: str,
) -> httpx.Response:
    """Delete an upstream via the Management API."""
    return await client.delete(
        f"{base_url}/oagw/v1/upstreams/{upstream_id}",
        headers=headers,
    )


# ---------------------------------------------------------------------------
# Error identity assertions
# ---------------------------------------------------------------------------

ERR_TYPE_PREFIX = "gts://gts.cf.core.errors.err.v1~cf.core.err."


def err_type(category: str) -> str:
    """Canonical Problem Details ``type`` URI for an error category."""
    return f"{ERR_TYPE_PREFIX}{category}.v1~"


def assert_problem(
    resp: httpx.Response,
    status: int,
    *,
    esrc: Optional[str] = "gateway",
    category: Optional[str] = None,
    reason: Optional[str] = None,
    resource_type: Optional[str] = None,
    detail_contains: Optional[str] = None,
) -> dict:
    """Assert ``resp`` is a Problem Details error with the given identity.

    ``esrc=None`` asserts that ``x-oagw-error-source`` is absent, i.e. the
    response was produced by a layer in front of OAGW. Returns the parsed body.
    """
    assert resp.status_code == status, (
        f"expected {status}, got {resp.status_code}: {resp.text[:500]}"
    )
    ct = resp.headers.get("content-type", "")
    assert ct.startswith("application/problem+json"), f"content-type {ct!r}: {resp.text[:300]}"
    assert resp.headers.get("x-oagw-error-source") == esrc, (
        f"x-oagw-error-source {resp.headers.get('x-oagw-error-source')!r} != {esrc!r}"
    )
    body = resp.json()
    assert body.get("status") == status, body
    if category is not None:
        assert body.get("type") == err_type(category), body
    context = body.get("context") or {}
    if reason is not None:
        reasons = [context.get("reason")] + [
            v.get("reason") for v in context.get("field_violations") or []
        ]
        assert reason in reasons, body
    if resource_type is not None:
        assert context.get("resource_type") == resource_type, body
    if detail_contains is not None:
        assert detail_contains in (body.get("detail") or ""), body
    return body


# ---------------------------------------------------------------------------
# Listing and cleanup
# ---------------------------------------------------------------------------

async def list_all(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    collection: str,
    page_size: int = 100,
    **params,
) -> list[dict]:
    """Page through ``GET /oagw/v1/{collection}`` and return every item."""
    # The server caps a page at 100; a bigger page_size would end after one page.
    assert 0 < page_size <= 100, page_size
    items: list[dict] = []
    seen: set[str] = set()
    skip = 0
    while True:
        resp = await client.get(
            f"{base_url}/oagw/v1/{collection}",
            headers=headers,
            params={**params, "limit": page_size, "offset": skip},
        )
        assert resp.status_code == 200, resp.text[:500]
        page = resp.json()
        ids = {item["id"] for item in page}
        assert not ids & seen, f"offset {skip} returned items already listed"
        seen |= ids
        items.extend(page)
        if len(page) < page_size:
            return items
        skip += page_size


class Cleanup:
    """Collects upstreams to delete at teardown (see the ``cleanup`` fixture).

    Deletion runs in reverse creation order so children go before parents,
    and each delete must return 204 (or 404 when the test already deleted it).
    """

    def __init__(self) -> None:
        self._upstreams: list[tuple[dict, str]] = []

    def upstream(self, headers: dict, upstream: dict) -> dict:
        self._upstreams.append((headers, upstream["id"]))
        return upstream

    async def run(self, base_url: str) -> None:
        failures = []
        async with httpx.AsyncClient(timeout=10.0) as client:
            for headers, uid in reversed(self._upstreams):
                try:
                    resp = await delete_upstream(client, base_url, headers, uid)
                except httpx.HTTPError as exc:
                    failures.append(f"{uid}: {exc!r}")
                    continue
                if resp.status_code not in (204, 404):
                    failures.append(f"{uid}: {resp.status_code} {resp.text[:200]}")
        assert not failures, f"cleanup failed: {failures}"
