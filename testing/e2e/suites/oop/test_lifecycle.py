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

Each of the last two runs for both consumers: over REST, and over gRPC, where
readiness waits for the provider's gRPC service in the directory. The gRPC
consumer also has:

  * readiness that follows the gRPC service, not the provider's REST endpoint;
  * a provider that moves to another gRPC port: the client follows it through
    the directory without a restart;
  * the circuit breaker: a provider that stops answering opens it (configured
    in the consumer's `clients` section), and it closes once the provider
    answers again.

Each test restores the processes it touched before yielding to any later test.
"""
from __future__ import annotations

import httpx
import pytest

from .conftest import PROVIDER_GRPC_PORT, PROVIDER_GRPC_PORT_MOVED, REQUEST_TIMEOUT, TOKEN

# These tests stop and start processes and wait for the directory, the edge and
# the readiness probe (whose backoff reaches 25 s); the suite-wide 30 s would
# cut a slow runner short before the waits report what they were waiting for.
pytestmark = pytest.mark.timeout(120)

CONSUMER = "api-contracts-consumer-oop"
GRPC_CONSUMER = "api-contracts-grpc-consumer-oop"
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


def _readyz_gated_on(url: str, dep: str) -> bool:
    """True iff /readyz is 503 and lists `dep` among its unresolved dependencies."""
    try:
        r = httpx.get(f"{url}/readyz", timeout=REQUEST_TIMEOUT)
        if r.status_code != 503:
            return False
        return dep in r.json().get("unresolved_deps", [])
    except (httpx.HTTPError, ValueError, AttributeError):
        return False


def _restore_provider_and_consumers(cluster) -> None:
    """Start whatever is stopped, then wait until both consumers charge through
    the provider again: 200, not only ready. Right after a provider restart its
    grpc-hub listens before the gRPC service is installed, and a call in that
    window gets `UNIMPLEMENTED`; the next test must not start in it."""
    if not cluster.is_running(PROVIDER):
        cluster.start_gear(PROVIDER)
    for consumer, gear in ((CONSUMER, "api-contracts-consumer"),
                           (GRPC_CONSUMER, "api-contracts-grpc-consumer")):
        if not cluster.is_running(consumer):
            cluster.start_gear(consumer)
        consumer_url = cluster.gear_url(consumer)
        cluster.wait(
            f"{consumer} ready again after lifecycle test",
            lambda: _readyz_status(consumer_url) == 200,
        )
        cluster.wait(
            f"{consumer} charges again after lifecycle test",
            lambda: cluster.status(
                "POST", f"/{gear}/v1/charge",
                headers={"Authorization": f"Bearer {TOKEN}"},
                json={"amount_cents": 1, "currency": "USD", "description": "restore"},
            ) == 200,
        )


@pytest.fixture
def provider_consumer_restored(cluster):
    """Restore the provider and both consumers (up and charging) after the test."""
    yield
    _restore_provider_and_consumers(cluster)


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
        "consumer reports not-ready, gated on its unresolved provider dependency",
        lambda: _readyz_gated_on(consumer_url, PROVIDER_DEP),
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


# ── the same over gRPC ────────────────────────────────────────────────────────


def test_grpc_consumer_readiness_gates_on_published_service(cluster, provider_consumer_restored):
    """Seam: a `transport = grpc` dependency gates /readyz until the provider is up.

    Stopping the provider removes its REST endpoint and its gRPC service alike,
    so this test alone cannot tell which one the probe waits for;
    `test_grpc_readiness_follows_the_grpc_service_not_rest` does.
    """
    consumer_url = cluster.gear_url(GRPC_CONSUMER)
    assert _readyz_status(consumer_url) == 200

    cluster.stop_gear(PROVIDER)
    cluster.stop_gear(GRPC_CONSUMER)
    cluster.start_gear(GRPC_CONSUMER)

    cluster.wait(
        "gRPC consumer reports not-ready, gated on the unpublished gRPC service",
        lambda: _readyz_gated_on(consumer_url, PROVIDER_DEP),
    )
    body = httpx.get(f"{consumer_url}/readyz", timeout=REQUEST_TIMEOUT).json()
    assert body["state"] == "starting" and body["ready"] is False, body

    cluster.start_gear(PROVIDER)
    cluster.wait(
        "gRPC consumer becomes ready once the provider publishes its gRPC service",
        lambda: _readyz_status(consumer_url) == 200,
    )


def test_grpc_charge_degrades_to_503_and_recovers(cluster, auth, provider_consumer_restored):
    """Seam: a gRPC contract call fails cleanly while the provider is gone.

    With the provider stopped the gRPC client finds no service in the
    directory, which must surface as a 503 problem+json, never a 500 or a hang.
    After the restart the client finds the service again. (Too few calls fail
    here to open the breaker; `test_grpc_breaker_opens_and_closes` covers it.)
    """
    path = "/api-contracts-grpc-consumer/v1/charge"
    url = f"{cluster.base_url}{path}"
    headers = {**auth, "Content-Type": "application/json"}
    body = {"amount_cents": 1000, "currency": "USD", "description": "grpc provider-down"}

    r = httpx.post(url, headers=headers, json=body, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"

    cluster.stop_gear(PROVIDER)
    cluster.wait(
        "gRPC charge degrades to 503 while the provider is down",
        lambda: cluster.status("POST", path, headers=auth, json=body) == 503,
    )
    r = httpx.post(url, headers=headers, json=body, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 503, f"expected 503, got {r.status_code}: {r.text}"
    assert "application/problem+json" in r.headers.get("content-type", ""), r.headers

    cluster.start_gear(PROVIDER)
    cluster.wait(
        "gRPC charge works again once the provider is back",
        lambda: cluster.status("POST", path, headers=auth, json=body) == 200,
    )
    r = httpx.post(url, headers=headers, json=body, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"


# ── gRPC-only behaviour ───────────────────────────────────────────────────────

GRPC_CHARGE = "/api-contracts-grpc-consumer/v1/charge"
REST_CHARGE = "/api-contracts-consumer/v1/charge"
CHARGE_BODY = {"amount_cents": 100, "currency": "USD", "description": "grpc lifecycle"}


def _charge_status(cluster, auth, path: str = GRPC_CHARGE) -> int:
    return cluster.status("POST", path, headers=auth, json=CHARGE_BODY)


def _grpc_listen(port_or_none: str):
    usual = f'listen_addr: "127.0.0.1:{PROVIDER_GRPC_PORT}"'
    return lambda text: text.replace(usual, f"listen_addr: {port_or_none}")


@pytest.fixture
def provider_usual_restored(cluster):
    """After a test that ran the provider on a variant config, run the usual one."""
    yield
    if cluster.is_running(PROVIDER):
        cluster.stop_gear(PROVIDER)
    cluster.start_gear(PROVIDER)
    _restore_provider_and_consumers(cluster)


def test_grpc_readiness_follows_the_grpc_service_not_rest(cluster, provider_usual_restored):
    """Seam: the gRPC dependency waits for the service name, not the REST endpoint.

    The provider comes back with grpc-hub not listening: its REST endpoint is in
    the directory, its gRPC service is not. The REST consumer turns ready and
    the gRPC consumer stays not-ready, gated on `api-contracts`.
    """
    no_grpc = cluster.variant_config(PROVIDER, "no-grpc", _grpc_listen('"none"'))
    rest_url = cluster.gear_url(CONSUMER)
    grpc_url = cluster.gear_url(GRPC_CONSUMER)

    cluster.stop_gear(PROVIDER)
    for consumer in (CONSUMER, GRPC_CONSUMER):
        cluster.stop_gear(consumer)
        cluster.start_gear(consumer)
    cluster.start_gear(PROVIDER, no_grpc)

    cluster.wait("REST consumer ready on the REST endpoint alone",
                 lambda: _readyz_status(rest_url) == 200)
    # The gRPC consumer restarted too; wait until its /readyz reports the gate.
    cluster.wait("gRPC consumer gated on the unpublished gRPC service",
                 lambda: _readyz_gated_on(grpc_url, PROVIDER_DEP))
    assert _readyz_gated_on(grpc_url, PROVIDER_DEP), "the gRPC consumer must still wait"

    cluster.stop_gear(PROVIDER)
    cluster.start_gear(PROVIDER)
    cluster.wait("gRPC consumer ready once the service is published",
                 lambda: _readyz_status(grpc_url) == 200)


def test_grpc_client_follows_a_provider_that_moved(cluster, auth, provider_usual_restored):
    """Seam: the gRPC client re-resolves the service after its address changes.

    The provider restarts with grpc-hub on another port. The consumer is not
    restarted: its first call to the old address fails, the client drops the
    cached lookup, and the directory gives it the new address.
    """
    moved = cluster.variant_config(
        PROVIDER, "moved", _grpc_listen(f'"127.0.0.1:{PROVIDER_GRPC_PORT_MOVED}"')
    )
    assert _charge_status(cluster, auth) == 200

    cluster.stop_gear(PROVIDER)
    cluster.start_gear(PROVIDER, moved)
    cluster.wait("gRPC charge reaches the provider on its new port",
                 lambda: _charge_status(cluster, auth) == 200)
    assert _charge_status(cluster, auth) == 200


def _breaker_config(text: str) -> str:
    """The gRPC consumer with a short call timeout and a breaker that opens
    after two failed calls, set in its `clients` section."""
    usual = "  api-contracts-grpc-consumer:\n    config: {}\n"
    assert usual in text
    return text.replace(usual, usual + (
        "    clients:\n"
        "      consumes:\n"
        "        api-contracts:\n"
        "          timeout: 500ms\n"
        "          circuit_breaker:\n"
        "            minimum_calls: 2\n"
        "            failure_ratio: 0.5\n"
        "            open_for: 2s\n"
    ))


@pytest.fixture
def grpc_consumer_usual_restored(cluster):
    yield
    if cluster.is_running(PROVIDER):
        cluster.resume_gear(PROVIDER)
    cluster.stop_gear(GRPC_CONSUMER)
    cluster.start_gear(GRPC_CONSUMER)
    _restore_provider_and_consumers(cluster)


def test_grpc_breaker_opens_and_closes(cluster, auth, grpc_consumer_usual_restored):
    """Seam: the client's circuit breaker across processes, set from YAML.

    The provider is paused (SIGSTOP): it is still in the directory and its port
    still accepts connections, but it answers nothing, so calls time out. Two
    timeouts open the breaker, and the next call is refused at once with a 503
    that says so and carries a retry hint. Once the provider answers again, the
    probe after `open_for` closes the breaker.
    """
    config = cluster.variant_config(GRPC_CONSUMER, "breaker", _breaker_config)
    cluster.stop_gear(GRPC_CONSUMER)
    cluster.start_gear(GRPC_CONSUMER, config)
    cluster.wait("gRPC consumer charges with the breaker config",
                 lambda: _charge_status(cluster, auth) == 200)

    url = f"{cluster.base_url}{GRPC_CHARGE}"
    headers = {**auth, "Content-Type": "application/json"}

    def charge() -> httpx.Response:
        return httpx.post(url, headers=headers, json=CHARGE_BODY, timeout=REQUEST_TIMEOUT)

    cluster.pause_gear(PROVIDER)
    refused = None
    for _ in range(10):
        r = charge()
        assert r.status_code != 200, "the paused provider cannot have answered"
        if "circuit breaker open" in r.text:
            refused = r
            break
    assert refused is not None, "the breaker never opened"
    assert refused.status_code == 503, refused.text
    assert "application/problem+json" in refused.headers.get("content-type", "")
    assert refused.json()["context"].get("retry_after_seconds", 0) >= 1, refused.text

    cluster.resume_gear(PROVIDER)
    cluster.wait("breaker closes once the provider answers again",
                 lambda: _charge_status(cluster, auth) == 200)
