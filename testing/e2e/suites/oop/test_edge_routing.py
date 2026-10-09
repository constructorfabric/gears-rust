"""Edge routing and tenant-plane gating of OoP gear routes.

Everything here is a property of the reverse-proxy route table the edge syncs
from the DirectoryService plus the auth layer in front of it: which paths/methods
reach which gear, which are anonymous, and how unknown or unauthenticated
requests are handled. These only exist because the gears run as separate
processes discovered through the directory (topology: see conftest).
"""
from __future__ import annotations

import httpx
import pytest

from .conftest import REQUEST_TIMEOUT


@pytest.mark.smoke
def test_hello_anonymous_cross_process_proxy(oop_cluster):
    """Seam: edge reverse-proxies an anonymous route to a separate gear process.

    `served_by` is the serving process id — proof the request crossed the
    process boundary (edge -> hello-oop) rather than being handled in-process.
    """
    r = httpx.get(f"{oop_cluster}/hello/v1/ping", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    body = r.json()
    assert body.get("message") == "pong", body
    assert "hello-oop" in str(body.get("served_by", "")), body


def test_missing_bearer_rejected_at_edge(oop_cluster):
    """Seam: tenant-plane gating — an authenticated route needs a bearer."""
    r = httpx.post(
        f"{oop_cluster}/api-contracts-consumer/v1/charge",
        json={"amount_cents": 1000, "currency": "USD", "description": "no-token"},
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 401, f"expected 401, got {r.status_code}: {r.text}"


def test_unexposed_route_not_published(oop_cluster, auth):
    """Seam: a route not marked `.exposed()` is never synced into the edge table.

    The provider's charge route (`POST /api-contracts/v1/payments/charge`) is not
    `.exposed()`, so the edge never publishes it — even though the provider serves
    it (exercised indirectly by the consumer path in test_contract_calls). With
    a valid bearer the auth layer passes and the unknown path falls through to the
    reverse-proxy fallback: 404 ("no upstream route registered").
    """
    r = httpx.post(
        f"{oop_cluster}/api-contracts/v1/payments/charge",
        headers={**auth, "Content-Type": "application/json"},
        json={"amount_cents": 1000, "currency": "USD", "description": "direct"},
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 404, f"expected 404, got {r.status_code}: {r.text}"


def test_unknown_route_under_proxy_is_404(oop_cluster, auth):
    """Seam: proxy fallback — an authenticated but unrouted path 404s.

    With a valid bearer the edge auth layer passes; the path matches no route the
    directory ever published, so it falls through to the reverse-proxy fallback,
    which answers 404 rather than 502/500. The generic form of
    test_unexposed_route_not_published.
    """
    r = httpx.get(
        f"{oop_cluster}/no-such-gear/v1/nope",
        headers=auth,
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 404, f"expected 404, got {r.status_code}: {r.text}"


def test_unknown_route_without_bearer_is_401(oop_cluster):
    """Seam: tenant-plane gating precedes the proxy fallback.

    An unrouted path is unknown to the edge, so it is not in the synced anonymous
    set; `require_auth_by_default` makes the auth layer reject it with 401 BEFORE
    the proxy fallback can turn it into a 404. Order matters: auth is the outer
    layer, and the anonymous set is itself populated from the directory sync.
    """
    r = httpx.get(f"{oop_cluster}/no-such-gear/v1/nope", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 401, f"expected 401, got {r.status_code}: {r.text}"


def test_wrong_method_on_proxied_route_is_405(oop_cluster, auth):
    """Seam: the proxied route is method-aware, not matched on path alone.

    `hello.ping` is published by the OoP gear as GET-only. A POST to the same
    path is rejected with 405 Method Not Allowed — the HTTP method is part of the
    route contract end to end (the synced table / the gear it proxies to), not
    ignored. A non-405 would mean the path matched regardless of method (a POST
    wrongly handled) or the method was dropped — both wrong. (POST without a
    bearer is 401 instead: only the GET is in the anonymous set.)
    """
    r = httpx.post(f"{oop_cluster}/hello/v1/ping", headers=auth, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 405, f"expected 405, got {r.status_code}: {r.text}"
