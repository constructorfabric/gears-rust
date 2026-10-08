"""Self-hosted (Profile 2) OoP coverage: flight-control spawns
`runtime.type: oop` gears as child processes, the edge proxies to them, and
host shutdown stops the spawned worker.

Each test boots its own throwaway host via the `self_hosted_host` fixture, so
nothing is shared between tests. Complements `suites/oop`, which covers the
worker-side seams via manually launched processes.
"""
from __future__ import annotations

import socket
import time

import httpx

from .conftest import BASE_URL, HOST_LOG, REQUEST_TIMEOUT, WORKER_PORT


def _port_open(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.5)
        return s.connect_ex(("127.0.0.1", port)) == 0


def test_self_hosted_spawns_and_registers(self_hosted_host):
    """The host's oop_spawn phase launched hello-oop and it self-registered:
    its own listener is up and the edge proxies its route."""
    log = HOST_LOG.read_text(errors="replace")
    assert "Spawned OoP gear via backend" in log and 'gear=hello' in log, (
        f"host log shows no hello spawn:\n{log[-2000:]}"
    )
    assert _port_open(WORKER_PORT)


def test_self_hosted_edge_proxy(self_hosted_host):
    r = httpx.get(f"{BASE_URL}/hello/v1/ping", timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200
    body = r.json()
    assert body["message"] == "pong"
    assert "hello-oop" in body["served_by"]


def test_self_hosted_shutdown_stops_worker(self_hosted_host):
    """SIGTERM to the host must stop the spawned child — the OoP backend's
    cancel-token path, not an orphaned worker."""
    assert _port_open(WORKER_PORT)
    self_hosted_host.terminate()
    self_hosted_host.wait(timeout=15)
    assert self_hosted_host.returncode is not None

    deadline = time.monotonic() + 15
    while _port_open(WORKER_PORT) and time.monotonic() < deadline:
        time.sleep(0.5)
    assert not _port_open(WORKER_PORT), (
        "worker port still listening after host shutdown — spawned child was orphaned"
    )
