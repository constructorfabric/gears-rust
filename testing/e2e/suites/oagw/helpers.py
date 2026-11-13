"""Shared helpers for OAGW E2E tests."""
import os
import re
import uuid
from typing import Optional

import httpx

from ..types_registry.helpers import (
    PLATFORM_API,
    RECEIPT,
    get_entity,
    gts_uuid,
    submit_and_poll,
    walk,
)

# ---------------------------------------------------------------------------
# OAGW GTS type catalog — mirrors the Rust catalog in domain/type_catalog.rs
# ---------------------------------------------------------------------------

# Schema GTS identifiers (7)
UPSTREAM_SCHEMA = "gts.cf.core.oagw.upstream.v1~"
ROUTE_SCHEMA = "gts.cf.core.oagw.route.v1~"

# The sqlite profile's post-startup registry instances (`registry_fixtures`),
# and the OAGW resource IDs of the rows provisioned from them: a row's ID is
# its instance's registry UUID.
REGISTRY_UPSTREAM_GTS_ID = f"{UPSTREAM_SCHEMA}cf.e2e.oagw.registry_upstream.v1"
REGISTRY_ROUTE_GTS_ID = f"{ROUTE_SCHEMA}cf.e2e.oagw.registry_route.v1"
BARE_UUID_ROUTE_GTS_ID = f"{ROUTE_SCHEMA}cf.e2e.oagw.bare_uuid_route.v1"
REGISTRY_UPSTREAM_ID = f"{UPSTREAM_SCHEMA}{gts_uuid(REGISTRY_UPSTREAM_GTS_ID)}"
REGISTRY_ROUTE_ID = f"{ROUTE_SCHEMA}{gts_uuid(REGISTRY_ROUTE_GTS_ID)}"
BARE_UUID_ROUTE_ID = f"{ROUTE_SCHEMA}{gts_uuid(BARE_UUID_ROUTE_GTS_ID)}"
PROTOCOL_SCHEMA = "gts.cf.core.oagw.protocol.v1~"
AUTH_PLUGIN_SCHEMA = "gts.cf.core.oagw.auth_plugin.v1~"
GUARD_PLUGIN_SCHEMA = "gts.cf.core.oagw.guard_plugin.v1~"
TRANSFORM_PLUGIN_SCHEMA = "gts.cf.core.oagw.transform_plugin.v1~"
PROXY_SCHEMA = "gts.cf.core.oagw.proxy.v1~"

# Protocol instances (2)
HTTP_PROTOCOL_ID = "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
GRPC_PROTOCOL_ID = "gts.cf.core.oagw.protocol.v1~cf.core.oagw.grpc.v1"

# Auth plugin instances (7)
NOOP_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.noop.v1"
APIKEY_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"
BASIC_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1"
BEARER_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.bearer.v1"
OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1"
OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1"
AWS_SIGV4_AUTH_PLUGIN_ID = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1"
OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID = (
    "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_private_key_jwt.v1"
)

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
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID, AWS_SIGV4_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID,
    TIMEOUT_GUARD_PLUGIN_ID, CORS_GUARD_PLUGIN_ID, REQUIRED_HEADERS_GUARD_PLUGIN_ID,
    LOGGING_TRANSFORM_PLUGIN_ID, METRICS_TRANSFORM_PLUGIN_ID,
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
]

ALL_OAGW_GTS_IDS = OAGW_SCHEMAS + OAGW_INSTANCES


def registry_fixtures() -> list[tuple[str, dict]]:
    """The sqlite profile's upstream and two routes, as `(GTS ID, body)`.

    One route names its upstream by instance identifier, the other by the
    bare UUID of the row provisioned from it.
    """
    return [
        (
            REGISTRY_UPSTREAM_GTS_ID,
            {
                "id": REGISTRY_UPSTREAM_GTS_ID,
                "alias": "e2e-registry-upstream",
                "server": {"endpoints": [{"host": "127.0.0.1", "port": 19876, "scheme": "http"}]},
                "protocol": HTTP_PROTOCOL_ID,
                "tags": ["e2e-registry"],
            },
        ),
        (
            REGISTRY_ROUTE_GTS_ID,
            {
                "id": REGISTRY_ROUTE_GTS_ID,
                "upstream_id": REGISTRY_UPSTREAM_GTS_ID,
                "match": {"http": {"methods": ["GET"], "path": "/v1/models"}},
                "tags": ["e2e-registry"],
            },
        ),
        (
            BARE_UUID_ROUTE_GTS_ID,
            {
                "id": BARE_UUID_ROUTE_GTS_ID,
                "upstream_id": gts_uuid(REGISTRY_UPSTREAM_GTS_ID),
                "match": {"http": {"methods": ["GET"], "path": "/v1/bare-uuid"}},
            },
        ),
    ]


def platform_headers() -> dict:
    """The types registry's platform plane: the internal token alone."""
    return {"X-ToolKit-Internal-Token": os.getenv("E2E_INTERNAL_TOKEN", "e2e-internal-token")}


async def publish_registry_fixtures(base_url: str) -> None:
    """Publish `registry_fixtures` through the platform API and wait for
    admission. An existing instance is revised at its current version, so
    runs sharing a registry database republish the same content."""
    async with httpx.AsyncClient(
        base_url=base_url.rstrip("/") + "/", headers=platform_headers(), timeout=10.0
    ) as platform:
        candidates = []
        for gts_id, content in registry_fixtures():
            candidate = {"gts_id": gts_id, "content": content}
            current = await get_entity(platform, PLATFORM_API, gts_id, select="origin")
            if current.status_code == 200:
                candidate["expected_resource_version"] = current.json()["origin"]["resource_version"]
            else:
                assert current.status_code == 404, f"read of '{gts_id}': {current.status_code} {current.text}"
            candidates.append(candidate)
        operation = await submit_and_poll(platform, PLATFORM_API, candidates, RECEIPT)
    refused = [i for i in operation["items"] if i["status"] not in ("succeeded", "unchanged")]
    assert not refused, f"the types registry refused OAGW fixtures: {refused}"


async def list_oagw_types(base_url: str) -> list[dict]:
    """Every active OAGW entity in the types registry, through paged platform
    discovery (`gts_id`, `kind`, ...)."""
    async with httpx.AsyncClient(
        base_url=base_url.rstrip("/") + "/", headers=platform_headers(), timeout=10.0
    ) as platform:
        pages = await walk(
            platform, PLATFORM_API, {"pattern": "gts.cf.core.oagw.*", "limit": 100}
        )
    return [item for page in pages for item in page["items"]]


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


async def create_route_raw(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
    upstream_id: str,
    methods: list[str],
    path: str,
    **kwargs,
) -> httpx.Response:
    """Create an HTTP route and return the raw Response (no raise_for_status).

    ``kwargs`` are merged into the body; pass ``match=...`` for a gRPC route.
    """
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

    return await client.post(
        f"{base_url}/oagw/v1/routes",
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
    resp = await create_route_raw(client, base_url, headers, upstream_id, methods, path, **kwargs)
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


# A fixed W3C traceparent: a request that sends it gets a `trace_id` in its
# Problem Details body whatever the server's tracing setup.
TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def traced(headers: dict) -> dict:
    """``headers`` plus :data:`TRACEPARENT`."""
    return {**headers, "traceparent": TRACEPARENT}


def assert_problem_body(resp: httpx.Response, expected: dict) -> None:
    """Assert ``resp`` is a Problem Details response produced by OAGW's
    Management API whose body is exactly ``expected`` plus a ``trace_id``.

    Send the request with :func:`traced` headers. ``trace_id`` is only checked
    to be a W3C trace id: with server-side tracing it is the server span's.
    """
    assert resp.status_code == expected["status"], (
        f"expected {expected['status']}, got {resp.status_code}: {resp.text[:500]}"
    )
    ct = resp.headers.get("content-type", "")
    assert ct.startswith("application/problem+json"), f"content-type {ct!r}: {resp.text[:300]}"
    assert resp.headers.get("x-oagw-error-source") is None, resp.headers
    body = resp.json()
    trace_id = body.pop("trace_id", None)
    assert isinstance(trace_id, str) and re.fullmatch(r"[0-9a-f]{32}", trace_id), resp.text
    assert body == expected, resp.text


def assert_gateway_problem_body(resp: httpx.Response, expected: dict) -> None:
    """Assert ``resp`` is a Problem Details response the proxy produced
    (``X-OAGW-Error-Source: gateway``) whose body is exactly ``expected``
    plus a ``trace_id``. Send the request with :func:`traced` headers."""
    assert resp.status_code == expected["status"], (
        f"expected {expected['status']}, got {resp.status_code}: {resp.text[:500]}"
    )
    ct = resp.headers.get("content-type", "")
    assert ct.startswith("application/problem+json"), f"content-type {ct!r}: {resp.text[:300]}"
    assert resp.headers.get("x-oagw-error-source") == "gateway", resp.headers
    body = resp.json()
    trace_id = body.pop("trace_id", None)
    assert isinstance(trace_id, str) and re.fullmatch(r"[0-9a-f]{32}", trace_id), resp.text
    assert body == expected, resp.text


def service_unavailable(status: int, instance: str, retry_after: int, reason: str) -> dict:
    """The gateway's 502/503 body: canonical ``service_unavailable`` with a
    retry hint and a reason."""
    return {
        "type": err_type("service_unavailable"),
        "title": "Service Unavailable",
        "status": status,
        "detail": "Service temporarily unavailable",
        "instance": instance,
        "context": {"retry_after_seconds": retry_after, "reason": reason},
    }


def credential_not_found(instance: str) -> dict:
    """The gateway's 401 for a secret the upstream's auth config references
    that is missing or not accessible."""
    return {
        "type": err_type("unauthenticated"),
        "title": "Unauthenticated",
        "status": 401,
        "detail": "Authentication required",
        "instance": instance,
        "context": {"reason": "AUTH_CREDENTIAL_NOT_FOUND"},
    }


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
    """Collects upstreams and routes to delete at teardown (see the ``cleanup`` fixture).

    Deletion runs in reverse creation order so children go before parents,
    and each delete must return 204 (or 404 when the test already deleted it).
    A route needs registering only when deleting its upstream won't remove it,
    e.g. a route on an upstream the test doesn't own.
    """

    def __init__(self) -> None:
        self._resources: list[tuple[str, dict, str]] = []

    def upstream(self, headers: dict, upstream: dict) -> dict:
        self._resources.append(("upstreams", headers, upstream["id"]))
        return upstream

    def route(self, headers: dict, route: dict) -> dict:
        self._resources.append(("routes", headers, route["id"]))
        return route

    async def run(self, base_url: str) -> None:
        failures = []
        async with httpx.AsyncClient(timeout=10.0) as client:
            for collection, headers, rid in reversed(self._resources):
                try:
                    resp = await client.delete(
                        f"{base_url}/oagw/v1/{collection}/{rid}", headers=headers,
                    )
                except httpx.HTTPError as exc:
                    failures.append(f"{rid}: {exc!r}")
                    continue
                if resp.status_code not in (204, 404):
                    failures.append(f"{rid}: {resp.status_code} {resp.text[:200]}")
        assert not failures, f"cleanup failed: {failures}"
