"""Usage-collector e2e conftest — single-server topology.

Instance (gateway): hyperspot-server with usage-collector + timescaledb plugin.
"""

from __future__ import annotations

import os
from pathlib import Path

import httpx
import pytest

from lib.orchestrator import ModuleTestEnv


MODULE_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = MODULE_DIR.parents[3]  # testing/e2e/modules/usage_collector -> repo root

_CONFIG_PATH = PROJECT_ROOT / "config" / "e2e-usage-collector.yaml"
_GATEWAY_PORT = 8086
_TIMESCALEDB_PORT = 5433


# ── Binary guard ─────────────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def _check_usage_collector_binary():
    """Skip all usage-collector tests when E2E_BINARY is not set.

    Usage-collector tests require a binary built with the TimescaleDB storage
    plugin, which is not included in the shared e2e binary. Build and run via:
    make e2e-usage-collector
    """
    if not os.environ.get("E2E_BINARY"):
        pytest.skip(
            "E2E_BINARY not set — run usage-collector tests via: make e2e-usage-collector",
            allow_module_level=True,
        )


# ── ModuleTestEnv fixtures ────────────────────────────────────────────────────

@pytest.fixture(scope="session")
def module_test_env():
    """Gateway: usage-collector + timescaledb plugin."""
    from .timescaledb_sidecar import TimescaleDbSidecar

    return ModuleTestEnv(
        config_path=_CONFIG_PATH,
        sidecars=[TimescaleDbSidecar(port=_TIMESCALEDB_PORT)],
        port=_GATEWAY_PORT,
        health_path="/healthz",
        health_timeout=90,
        log_suffix="uc-gateway",
    )


# ── HTTP client fixtures ──────────────────────────────────────────────────────

@pytest.fixture
async def gateway_client(test_env):
    """Pre-configured async HTTP client targeting the gateway."""
    async with httpx.AsyncClient(
        base_url=test_env.base_url,
        headers={"Authorization": "Bearer e2e-token-tenant-a"},
        timeout=30.0,
    ) as client:
        yield client
