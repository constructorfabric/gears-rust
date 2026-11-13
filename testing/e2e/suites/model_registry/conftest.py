"""Pytest fixtures for Model Registry E2E tests.

The suite runs against the standard e2e server (``config/e2e-local.yaml``).
Tokens map to static identities (static-authn-plugin) inside the static
tenant tree (static-tr-plugin)::

    e2e-root (00000000-df51-...953)
      hierarchy-root (...0001)               <- e2e-token-hierarchy-root
        hierarchy-l1a (...0002)              <- e2e-token-hierarchy-l1a
          hierarchy-l2b (...0004)            <- e2e-token-hierarchy-l2b
        hierarchy-l1b (...0005)              <- e2e-token-hierarchy-l1b
        hierarchy-sm (...0007, self-managed) <- e2e-token-hierarchy-sm
          hierarchy-sm-child (...0008)       <- e2e-token-hierarchy-sm-child

Expected results come from ``gears/model-registry/docs`` (PRD, DESIGN, ADRs);
``SCENARIOS.md`` maps each test to its source. Every test creates providers
under a unique slug and registers them for teardown, so tests are
order-independent and re-runnable against a shared long-lived server.
"""
from __future__ import annotations

import os
import uuid

import httpx
import pytest

REQUEST_TIMEOUT = 5.0

HIERARCHY_ROOT = "00000000-0000-0000-0000-000000000001"
HIERARCHY_L1A = "00000000-0000-0000-0000-000000000002"
HIERARCHY_L2B = "00000000-0000-0000-0000-000000000004"
HIERARCHY_L1B = "00000000-0000-0000-0000-000000000005"
HIERARCHY_SM = "00000000-0000-0000-0000-000000000007"
HIERARCHY_SM_CHILD = "00000000-0000-0000-0000-000000000008"

ROOT_SUBJECT = "22222222-0000-0000-0000-000000000001"

# Provider types seeded by config/e2e-local.yaml.
OPEN_TYPE = "gts.cf.genai.model.provider.v1~cf.e2e._.open.v1~"
STRICT_TYPE = "gts.cf.genai.model.provider.v1~cf.e2e._.strict.v1~"


def _bearer(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture
def base_url():
    """API Gateway base URL."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


@pytest.fixture
def providers_url(base_url):
    return f"{base_url}/model-registry/v1/providers"


@pytest.fixture
def models_url(base_url):
    return f"{base_url}/model-registry/v1/models"


@pytest.fixture
def available_url(base_url):
    return f"{base_url}/model-registry/v1/available-models"


@pytest.fixture
def root_headers():
    """hierarchy-root (...0001): parent of l1a, l1b and sm."""
    return _bearer("e2e-token-hierarchy-root")


@pytest.fixture
def l1a_headers():
    """hierarchy-l1a (...0002), child of hierarchy-root."""
    return _bearer("e2e-token-hierarchy-l1a")


@pytest.fixture
def l1b_headers():
    """hierarchy-l1b (...0005), sibling of l1a."""
    return _bearer("e2e-token-hierarchy-l1b")


@pytest.fixture
def l2b_headers():
    """hierarchy-l2b (...0004), child of l1a."""
    return _bearer("e2e-token-hierarchy-l2b")


@pytest.fixture
def sm_headers():
    """hierarchy-sm (...0007), self-managed child of hierarchy-root."""
    return _bearer("e2e-token-hierarchy-sm")


@pytest.fixture
def sm_child_headers():
    """hierarchy-sm-child (...0008), below the self-managed barrier."""
    return _bearer("e2e-token-hierarchy-sm-child")


@pytest.fixture
async def client():
    async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as c:
        yield c


@pytest.fixture
def unique_slug():
    """Factory for unique provider slugs (safe on a shared server)."""

    def make(prefix: str = "e2e") -> str:
        return f"{prefix}-{uuid.uuid4().hex[:12]}"

    return make


@pytest.fixture
async def create_provider(client, providers_url, unique_slug):
    """Factory: create a provider, return its entry; teardown deletes it.

    Deleting a provider removes its models, so models need no teardown.
    """
    created: list[tuple[dict, str]] = []

    async def _create(headers: dict, **fields) -> dict:
        body = {
            "slug": unique_slug(),
            "name": "E2E provider",
            "gts_type": OPEN_TYPE,
            **fields,
        }
        resp = await client.post(providers_url, headers=headers, json=body)
        assert resp.status_code == 201, f"create provider: {resp.status_code} {resp.text}"
        entry = resp.json()
        created.append((headers, entry["id"]))
        return entry

    yield _create

    for headers, provider_id in reversed(created):
        try:
            await client.delete(
                f"{providers_url}/{provider_id}",
                headers={**headers, "If-Match": "*"},
            )
        except httpx.RequestError:
            break


def model_body(provider_id: str, name: str, **fields) -> dict:
    """A create-model body with the fields the docs give no default for."""
    return {
        "provider_id": provider_id,
        "provider_model_id": name,
        "display_name": name,
        "supported_api": ["completion"],
        "context_window": {"max_input_tokens": 8192},
        "provider_settings": {},
        "name": name,
        **fields,
    }


@pytest.fixture
def create_model(client, models_url):
    """Factory: create a model under a provider and return its entry."""

    async def _create(headers: dict, provider_id: str, name: str = "m1", **fields) -> dict:
        resp = await client.post(
            models_url, headers=headers, json=model_body(provider_id, name, **fields)
        )
        assert resp.status_code == 201, f"create model: {resp.status_code} {resp.text}"
        return resp.json()

    return _create


@pytest.fixture(scope="session", autouse=True)
def _check_model_registry_reachable():
    """Skip the whole suite when the e2e server is not running."""
    url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    try:
        # Any HTTP response (401 included) means the gateway is up.
        httpx.get(f"{url}/model-registry/v1/providers", timeout=5.0)
    except httpx.ConnectError:
        pytest.skip(f"e2e server not running at {url}", allow_module_level=True)
    except Exception:
        # Timeout or transient error — still try to run the tests.
        pass
