"""Shared helpers for OAGW E2E tests."""
import re
import uuid
from typing import Optional

import httpx

# ---------------------------------------------------------------------------
# OAGW GTS type catalog — mirrors the Rust catalog in domain/type_catalog.rs
# ---------------------------------------------------------------------------

# Schema GTS identifiers (7)
UPSTREAM_SCHEMA = "gts.x.core.oagw.upstream.v1~"
ROUTE_SCHEMA = "gts.x.core.oagw.route.v1~"
PROTOCOL_SCHEMA = "gts.x.core.oagw.protocol.v1~"
AUTH_PLUGIN_SCHEMA = "gts.x.core.oagw.auth_plugin.v1~"
GUARD_PLUGIN_SCHEMA = "gts.x.core.oagw.guard_plugin.v1~"
TRANSFORM_PLUGIN_SCHEMA = "gts.x.core.oagw.transform_plugin.v1~"
PROXY_SCHEMA = "gts.x.core.oagw.proxy.v1~"

# Protocol instances (2)
HTTP_PROTOCOL_ID = "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1"
GRPC_PROTOCOL_ID = "gts.x.core.oagw.protocol.v1~x.core.oagw.grpc.v1"

# Auth plugin instances (6)
NOOP_AUTH_PLUGIN_ID = "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.noop.v1"
APIKEY_AUTH_PLUGIN_ID = "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.apikey.v1"
BASIC_AUTH_PLUGIN_ID = "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.basic.v1"
BEARER_AUTH_PLUGIN_ID = "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.bearer.v1"
OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID = "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred.v1"
OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID = "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred_basic.v1"

# Guard plugin instances (3)
TIMEOUT_GUARD_PLUGIN_ID = "gts.x.core.oagw.guard_plugin.v1~x.core.oagw.timeout.v1"
CORS_GUARD_PLUGIN_ID = "gts.x.core.oagw.guard_plugin.v1~x.core.oagw.cors.v1"
REQUIRED_HEADERS_GUARD_PLUGIN_ID = "gts.x.core.oagw.guard_plugin.v1~x.core.oagw.required_headers.v1"

# Transform plugin instances (3)
LOGGING_TRANSFORM_PLUGIN_ID = "gts.x.core.oagw.transform_plugin.v1~x.core.oagw.logging.v1"
METRICS_TRANSFORM_PLUGIN_ID = "gts.x.core.oagw.transform_plugin.v1~x.core.oagw.metrics.v1"
REQUEST_ID_TRANSFORM_PLUGIN_ID = "gts.x.core.oagw.transform_plugin.v1~x.core.oagw.request_id.v1"

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


async def register_oagw_types(
    client: httpx.AsyncClient,
    base_url: str,
    headers: dict,
) -> httpx.Response:
    """Register all OAGW GTS schemas and instances via the types-registry REST API.

    Idempotent — safe to call when types are already registered at startup.
    """
    entities = []
    for gts_id in OAGW_SCHEMAS:
        entities.append({
            "$id": f"gts://{gts_id}",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        })
    for gts_id in OAGW_INSTANCES:
        entities.append({"$id": gts_id})

    resp = await client.post(
        f"{base_url}/types-registry/v1/entities",
        headers={**headers, "content-type": "application/json"},
        json={"entities": entities},
    )
    return resp


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


def parse_gts_uuid(gts_id: str) -> str:
    """Extract the UUID from a GTS identifier (e.g., 'gts.x.core.oagw.upstream.v1~<uuid>')."""
    if "~" in gts_id:
        return gts_id.rsplit("~", 1)[-1]
    # Fallback: try regex for both hyphenated and non-hyphenated UUIDs.
    match = re.search(r"[0-9a-f]{32}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", gts_id, re.IGNORECASE)
    if not match:
        raise ValueError(f"No UUID found in GTS identifier: {gts_id}")
    return match.group(0)


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
    # Parse host/port from mock_url.
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

    resp = await client.post(
        f"{base_url}/oagw/v1/upstreams",
        headers={**headers, "content-type": "application/json"},
        json=body,
    )
    resp.raise_for_status()
    return resp.json()


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
    upstream_uuid = parse_gts_uuid(upstream_id)

    body: dict = {
        "upstream_id": upstream_uuid,
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
