"""E2E fixtures for the PriceBook flow across bss-products and bss-pricing.

Both gears are linked by the ``bss-pricing`` cargo feature, and pricing reserves
its SKU references in products through the in-process registry products
registers at boot. The suite drives the running ``cf-gears-server`` over HTTP
only.

The reachability probe below is a **graceful guard**, as in the ledger suite: a
server built without ``bss-pricing`` does not mount these routes, so the probe
404s and the whole module skips rather than failing for routes that binary does
not serve.
"""

import os

import httpx
import pytest

REQUEST_TIMEOUT = 10.0

PRICING = "/bss-pricing/v1"
PRODUCTS = "/bss-products/v1"

# The usage type the flow's usage variant registers in the usage collector.
USAGE_TYPE = "gts.cf.core.uc.usage_record.v1~cf.e2e.pricebook.storage.v1"


def _probe(client: httpx.Client, url: str, headers: dict) -> httpx.Response:
    try:
        return client.get(url, headers=headers)
    except httpx.HTTPError as exc:
        pytest.skip(f"cf-gears-server not reachable at {url}: {exc}")


@pytest.fixture(scope="session", autouse=True)
def require_pricebook_mounted():
    """Skip the module unless both gears answer an authenticated read.

    The probe is authenticated: the gateway answers 401 (not 404) for an
    unknown path without a token, so only an authenticated 404 says "not
    mounted".
    """
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    headers = {"Authorization": f"Bearer {token}"}
    with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
        for path in (f"{PRICING}/price-books", f"{PRODUCTS}/skus"):
            r = _probe(client, f"{base_url}{path}", headers)
            if r.status_code == 404:
                pytest.skip(
                    f"{path} is not mounted: this server was built without the "
                    "`bss-pricing` cargo feature."
                )


@pytest.fixture
def api():
    """A small JSON client bound to the server and the tenant-A token."""
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    client = httpx.Client(
        base_url=base_url,
        timeout=REQUEST_TIMEOUT,
        headers={"Authorization": f"Bearer {token}"},
    )
    try:
        yield client
    finally:
        client.close()


@pytest.fixture
def reviewer():
    """A second principal of tenant A (``config/e2e-local.yaml``), for a two-person approval."""
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_REVIEWER_TOKEN", "e2e-token-tenant-a-reviewer")
    client = httpx.Client(
        base_url=base_url,
        timeout=REQUEST_TIMEOUT,
        headers={"Authorization": f"Bearer {token}"},
    )
    try:
        yield client
    finally:
        client.close()
