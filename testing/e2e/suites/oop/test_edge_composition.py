"""Flight-control edge composition — its built-in endpoints are served.

flight-control runs the api-gateway in one process with service-discovery,
grpc-hub, authn-resolver, static-authn-plugin, and types-registry. These smoke
the built-in edge endpoints the composed binary serves; api-gateway's own tests
exercise the gateway in ISOLATION, so they do not cover this composition.

Whether the edge should AGGREGATE proxied gears' specs/health into /openapi.json
or /health is an open design question (DESIGN vs. ADR-0007) and is deliberately
not asserted here.
"""
from __future__ import annotations

import httpx
import pytest

from .conftest import REQUEST_TIMEOUT


@pytest.mark.smoke
def test_edge_healthz(oop_cluster):
    """Composition: the flight-control edge serves liveness."""
    r = httpx.get(f"{oop_cluster}/healthz", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"


@pytest.mark.smoke
def test_edge_readyz_ready(oop_cluster):
    """Composition: the edge reports ready once the cluster is booted.

    `oop_cluster` only yields after route sync, so the edge's own readiness
    (its in-process healthcheck registry) must have resolved to 200.
    """
    r = httpx.get(f"{oop_cluster}/readyz", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"


def test_edge_health_reports_healthy(oop_cluster):
    """Composition: `/health` is wired and the composed edge is healthy."""
    r = httpx.get(f"{oop_cluster}/health", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    body = r.json()
    assert body.get("status") == "healthy", body


@pytest.mark.smoke
def test_edge_openapi_served(oop_cluster):
    """Composition: the edge serves an OpenAPI doc covering the gears it composes.

    `enable_docs: true` publishes `/openapi.json` anonymously. Beyond "it is
    served", the doc must contain the routes of the system gears flight-control
    composes (service-discovery, types-registry) — proof that the composition
    registered them through the edge, which no isolated api-gateway unit test
    covers. Whether proxied OoP gears should ALSO appear is the open aggregation
    question (see the module docstring); this test does not assert either way.
    """
    r = httpx.get(f"{oop_cluster}/openapi.json", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    assert "application/json" in r.headers.get("content-type", ""), r.headers
    body = r.json()
    assert str(body.get("openapi", "")).startswith("3."), body.get("openapi")
    assert body.get("info", {}).get("title") == "CF/Gears Flight Control API", body.get("info")
    paths = body.get("paths", {})
    assert any(p.startswith("/types-registry/") for p in paths), sorted(paths)
    assert any(p.startswith("/service-discovery/") for p in paths), sorted(paths)


def test_edge_docs_served_anonymously(oop_cluster):
    """Composition: the human docs page is served anonymously as HTML."""
    r = httpx.get(f"{oop_cluster}/docs", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    assert "text/html" in r.headers.get("content-type", ""), r.headers
