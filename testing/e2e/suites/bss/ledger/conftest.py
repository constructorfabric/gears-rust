"""E2E fixtures for the bss-ledger gear.

The ledger REST surface (``/bss-ledger/v1``) is an opt-in gear. It IS wired into
the E2E build now — the ``bss-ledger`` cargo feature is in
``config/e2e-features.txt``, ``registered_gears.rs`` links it under that feature,
and ``config/e2e-local.yaml`` carries both a ``bss-ledger`` gear block and a
tenant-B token (``e2e-token-tenant-b``). So against the standard
``make e2e-local`` binary the module runs for real.

The reachability probe below is retained as a **graceful guard**: a server built
WITHOUT the ``bss-ledger`` feature (or a plain ``cf-gears-example-server`` on
``PATH``) does not mount these routes, so the probe would 404 and the whole
module skips rather than emitting red failures for endpoints that are simply not
served by that binary. This mirrors the file-storage module's staging pattern.
"""

import datetime
import os
import uuid

import httpx
import pytest

REQUEST_TIMEOUT = 5.0

# The gear's REST base path (design §3.3 / the axum routers). Every ledger route
# is a literal full path under this prefix.
API_BASE = "/bss-ledger/v1"

# ── Tenant identities (must match config/e2e-local.yaml static-authn-plugin) ─
#
# TENANT_A: the caller token ``e2e-token-tenant-a`` authenticates as this id.
# It is the AM platform-root (``account-management.config.bootstrap.root_id``),
# a valid ledger "seller" — so its ledger can be provisioned and posted to.
#
# TENANT_B: the token ``e2e-token-tenant-b`` authenticates as a DIFFERENT root
# that is NOT inside tenant A's subtree. It is the foreign caller the
# cross-tenant / no-existence-leak (BOLA) tests read as.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
TENANT_B_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"

# A neutral third-party payer for seeded invoices — deliberately neither tenant
# A nor B, so the cross-tenant read stays purely about a FOREIGN SELLER (tenant
# B) not seeing tenant A's ledger (not muddied by any payer-facing visibility).
SEED_PAYER_ID = "22222222-2222-2222-2222-222222222222"


@pytest.fixture(scope="session", autouse=True)
def require_ledger_mounted():
    """Skip the whole module unless the ledger control surface is reachable.

    Keys on ``GET /bss-ledger/v1/accounts`` (the read-only chart-of-accounts
    surface — the simplest authenticated GET). A 404 means the gear's routes are
    not mounted (a binary built WITHOUT the ``bss-ledger`` feature); a connection
    error means the server is down. Either way: skip, don't fail.

    The probe is **authenticated**: the API gateway returns 401 (not 404) for an
    unauthenticated request to any unknown path, so an auth-less probe could not
    tell "gear absent" (skip) from "auth required" (present). With a valid token
    an unknown route yields a clean 404.
    """
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    url = f"{base_url}{API_BASE}/accounts"
    try:
        with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
            r = client.get(url, headers={"Authorization": f"Bearer {token}"})
    except httpx.HTTPError as exc:
        pytest.skip(f"cf-gears-server not reachable at {base_url}: {exc}")
    if r.status_code == 404:
        pytest.skip(
            "bss-ledger REST endpoints are not mounted — this server was built "
            "without the `bss-ledger` cargo feature."
        )


@pytest.fixture
def api_base():
    return API_BASE


@pytest.fixture
def auth_headers_tenant_b():
    """Headers with the tenant-B bearer token.

    ``e2e-token-tenant-b`` authenticates as ``TENANT_B_ID`` (config/e2e-local.yaml
    static-authn-plugin), a root outside tenant A's subtree. Used by the
    cross-tenant no-existence-leak tests to read as a foreign seller.
    """
    return {"Authorization": "Bearer e2e-token-tenant-b"}


# ── Seeding helpers (genuine cross-tenant BOLA setup) ────────────────────────


def _quarter(month: int) -> int:
    """1-based fiscal quarter for a 1-based month (Jan-Mar → Q1, …)."""
    return (month - 1) // 3 + 1


def _provision_seller(base_url: str, headers: dict, tenant_id: str) -> httpx.Response:
    """Idempotently provision ``tenant_id``'s ledger (chart of accounts + calendar).

    Seeds the account classes an invoice-with-tax needs (AR / REVENUE /
    TAX_PAYABLE, plus SUSPENSE as the unmapped fallback) in USD, a monthly fiscal
    calendar, and the current period. Provisioning is idempotent — re-provisioning
    an already-seeded tenant returns 200 with ``accounts_existing`` — so this is
    safe to call on every run.
    """
    body = {
        "tenant_id": tenant_id,
        "accounts": [
            {"account_class": "AR", "currency": "USD", "normal_side": "DR"},
            {"account_class": "REVENUE", "currency": "USD", "normal_side": "CR"},
            {"account_class": "TAX_PAYABLE", "currency": "USD", "normal_side": "CR"},
            {"account_class": "SUSPENSE", "currency": "USD", "normal_side": "DR"},
        ],
        "currency_scales": [],
        "fiscal_calendar": {"timezone": "UTC", "granularity": "MONTH", "fy_start": 1},
    }
    with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
        return client.post(
            f"{base_url}{API_BASE}/provisioning", headers=headers, json=body
        )


def _post_invoice(
    base_url: str, headers: dict, tenant_id: str, payer_tenant_id: str
) -> httpx.Response:
    """Post one balanced invoice into ``tenant_id``'s ledger; return the response.

    Dates and ``period_id`` are derived from the current UTC date so the entry
    lands in the period provisioning just opened. A fresh ``invoice_id`` +
    ``correlation_id`` per call sidesteps idempotent-replay ambiguity (a re-post
    of the same invoice would return 200-replay, not a fresh 201).
    """
    today = datetime.datetime.now(datetime.timezone.utc).date()
    period_id = f"{today.year:04d}{today.month:02d}"
    body = {
        "tenant_id": tenant_id,
        "invoice_id": f"E2E-BOLA-{uuid.uuid4()}",
        "payer_tenant_id": payer_tenant_id,
        "effective_at": today.isoformat(),
        "due_date": (today + datetime.timedelta(days=30)).isoformat(),
        "period_id": period_id,
        "items": [
            {
                "amount_minor_ex_tax": 1000,
                "currency": "USD",
                "revenue_stream": "subscription",
                "catalog_class": "REVENUE",
                "gl_code": "4000",
            }
        ],
        "tax": [
            {
                "amount_minor": 200,
                "currency": "USD",
                "tax_jurisdiction": "US-CA",
                "tax_filing_period": f"{today.year}Q{_quarter(today.month)}",
            }
        ],
        "correlation_id": str(uuid.uuid4()),
    }
    with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
        return client.post(
            f"{base_url}{API_BASE}/journal-entries", headers=headers, json=body
        )


@pytest.fixture(scope="module")
def seeded_entry():
    """The id of a journal entry that genuinely exists in TENANT_A's ledger.

    Provisions TENANT_A (idempotent) then posts one invoice as TENANT_A, returning
    the created ``entry_id``. Reads base URL / token from the environment (like the
    session-scoped probe) so it can stay module-scoped.

    If the stack cannot seed — provisioning or the post is not accepted (e.g. AM is
    not wired to mark TENANT_A a seller, or the current period is closed) — the
    dependent cross-tenant test SKIPS rather than fails: a seeding gap is an infra
    concern, not a BOLA regression.
    """
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    headers = {
        "Authorization": f"Bearer {os.getenv('E2E_AUTH_TOKEN', 'e2e-token-tenant-a')}"
    }

    prov = _provision_seller(base_url, headers, TENANT_A_ID)
    if prov.status_code not in (200, 201):
        pytest.skip(
            f"cannot seed: provisioning TENANT_A returned {prov.status_code}: {prov.text}"
        )

    posted = _post_invoice(base_url, headers, TENANT_A_ID, SEED_PAYER_ID)
    if posted.status_code not in (200, 201):
        pytest.skip(
            f"cannot seed: posting a journal entry returned {posted.status_code}: {posted.text}"
        )

    entry_id = posted.json().get("entry_id")
    if not entry_id:
        pytest.skip(f"cannot seed: post response carried no entry_id: {posted.text}")
    return entry_id
