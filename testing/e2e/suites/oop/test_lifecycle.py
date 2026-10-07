"""Dynamic OoP lifecycle — behaviour that only appears when gears come and go.

Steady-state tests can't reach these: they drive a gear's process lifecycle
through the `cluster` controller and re-check the system.

  * Route (de)registration at the edge: a gracefully stopped gear deregisters
    from the DirectoryService and the edge drops its route; a restart restores it.
  * Eventual readiness (ADR-0005): a gear whose dependency is unresolved reports
    `/readyz` 503 (state=starting, listing the unresolved dep) and flips to 200
    once the dependency resolves.
  * Contract-call resilience (ADR-0005): with its provider stopped, a consumer
    charge degrades to a clean 503 (problem+json), not a 500 or a hang, and
    recovers once the provider returns.

Each test restores the processes it touched before yielding to any later test.
"""
from __future__ import annotations

import httpx
import pytest

from .conftest import REQUEST_TIMEOUT

CONSUMER = "api-contracts-consumer-oop"
PROVIDER = "api-contracts-oop"
# Gear name (not binary name) as it appears in /readyz `unresolved_deps`.
PROVIDER_DEP = "api-contracts"


# ── route (de)registration at the edge ───────────────────────────────────────


@pytest.fixture
def hello_restored(cluster):
    """Guarantee hello is running again after the test, even if it failed."""
    yield
    if not cluster.is_running("hello-oop"):
        cluster.start_gear("hello-oop")
        cluster.wait(
            "hello restored after lifecycle test",
            lambda: cluster.status("GET", "/hello/v1/ping") == 200,
        )


def test_gear_deregisters_and_reregisters_at_edge(cluster, auth, hello_restored):
    """Seam: the edge route table tracks a gear leaving and rejoining.

    Baseline: hello's anonymous route serves 200. After a graceful stop the gear
    deregisters from the DirectoryService and the edge drops the route — a
    bearer'd request then hits the proxy fallback (404), which distinguishes a
    removed route from a merely-dead upstream (that would 502). After a restart
    the gear re-registers and the edge restores the route (200 again).
    """
    assert cluster.status("GET", "/hello/v1/ping") == 200, "hello should serve at baseline"

    cluster.stop_gear("hello-oop")
    cluster.wait(
        "hello route deregistered at edge",
        lambda: cluster.status("GET", "/hello/v1/ping", headers=auth) == 404,
    )
    assert cluster.status("GET", "/hello/v1/ping", headers=auth) == 404

    cluster.start_gear("hello-oop")
    cluster.wait(
        "hello route re-registered at edge",
        lambda: cluster.status("GET", "/hello/v1/ping") == 200,
    )
    assert cluster.status("GET", "/hello/v1/ping") == 200


# ── eventual readiness gating (ADR-0005) ──────────────────────────────────────


def _readyz_status(url: str) -> int:
    try:
        return httpx.get(f"{url}/readyz", timeout=REQUEST_TIMEOUT).status_code
    except httpx.HTTPError:
        return 0


@pytest.fixture
def provider_consumer_restored(cluster):
    """Restore provider + consumer (up and ready) after the readiness test."""
    yield
    if not cluster.is_running(PROVIDER):
        cluster.start_gear(PROVIDER)
    if not cluster.is_running(CONSUMER):
        cluster.start_gear(CONSUMER)
    consumer_url = cluster.gear_url(CONSUMER)
    cluster.wait(
        "consumer ready again after readiness test",
        lambda: _readyz_status(consumer_url) == 200,
    )


def test_consumer_readiness_gates_on_provider_dependency(cluster, provider_consumer_restored):
    """Seam: /readyz gates on an unresolved dependency, then flips (ADR-0005).

    The consumer declares the provider as a dependency, so its readiness is gated
    on resolving it through the DirectoryService. Probed on the consumer's OWN
    port (the edge does not surface /readyz):

      * ready at baseline (200, ready=true);
      * with the provider withheld and the consumer rebooted, not-ready (503,
        state=starting) listing the unresolved `api-contracts` dependency;
      * ready again (200) once the provider is restored and the dep resolves.
    """
    consumer_url = cluster.gear_url(CONSUMER)

    r = httpx.get(f"{consumer_url}/readyz", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, r.text
    assert r.json()["ready"] is True, r.text

    # Withhold the dependency; reboot the consumer so it starts with it absent.
    cluster.stop_gear(PROVIDER)
    cluster.stop_gear(CONSUMER)
    cluster.start_gear(CONSUMER)

    cluster.wait(
        "consumer reports not-ready while its provider dependency is unresolved",
        lambda: _readyz_status(consumer_url) == 503,
    )
    r = httpx.get(f"{consumer_url}/readyz", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 503, r.text
    body = r.json()
    assert body["state"] == "starting", body
    assert body["ready"] is False, body
    assert PROVIDER_DEP in body.get("unresolved_deps", []), body

    # Resolve the dependency: readiness flips to ready.
    cluster.start_gear(PROVIDER)
    cluster.wait(
        "consumer becomes ready once its provider dependency resolves",
        lambda: _readyz_status(consumer_url) == 200,
    )
    r = httpx.get(f"{consumer_url}/readyz", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, r.text
    assert r.json()["ready"] is True, r.text


# ── contract-call resilience (ADR-0005) ───────────────────────────────────────


def test_consumer_charge_degrades_to_503_when_provider_down(
    cluster, auth, provider_consumer_restored
):
    """Seam: an OoP contract call fails gracefully when its provider is down.

    With the provider stopped, the consumer can no longer resolve `PaymentApi`
    over REST, so a charge must surface a clean 503 Service Unavailable (RFC 9457
    problem+json) — not a 500, and not a hang — per the generated client's
    contract (ADR-0005). It recovers to 200 once the provider returns.
    """
    charge_url = f"{cluster.base_url}/api-contracts-consumer/v1/charge"
    headers = {**auth, "Content-Type": "application/json"}
    body = {"amount_cents": 1000, "currency": "USD", "description": "provider-down"}

    # Baseline: the charge works.
    r = httpx.post(charge_url, headers=headers, json=body, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"

    # Provider down: the charge degrades to a 503 problem+json.
    cluster.stop_gear(PROVIDER)
    cluster.wait(
        "consumer charge degrades to 503 while the provider is down",
        lambda: cluster.status("POST", "/api-contracts-consumer/v1/charge",
                               headers=auth, json=body) == 503,
    )
    r = httpx.post(charge_url, headers=headers, json=body, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 503, f"expected 503, got {r.status_code}: {r.text}"
    assert "application/problem+json" in r.headers.get("content-type", ""), r.headers

    # Recovery: once the provider is back, charges succeed again.
    cluster.start_gear(PROVIDER)
    cluster.wait(
        "consumer charge works again once the provider is back",
        lambda: cluster.status("POST", "/api-contracts-consumer/v1/charge",
                               headers=auth, json=body) == 200,
    )
    r = httpx.post(charge_url, headers=headers, json=body, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
