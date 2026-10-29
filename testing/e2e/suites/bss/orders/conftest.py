"""E2E fixtures for the bss-orders gear (S2-12 draft milestone, extended by Stage 3 milestone 3b, D-234).

This suite is self-managed: it owns a PostgreSQL container and the server process, because
the gear requires PostgreSQL, a configured fail-closed AuthZ policy (the e2e policy AuthZ plugin) and
the Event Broker, none of which the shared ``make e2e-local`` server carries. It gates on
``E2E_BINARY`` (set by ``make e2e-orders`` / ``make e2e-local
SUITE=bss-orders``) and Docker, and skips otherwise.

Session setup, in order:

1. start the pinned ``postgres`` sidecar;
2. provision the schema-owner login ``orders_admin`` and the ``orders`` database;
3. run ``cf-gears-example-server --config <cfg> migrate`` **as the admin login** (the toolkit
   applies every gear's migrations, including the Orders role migrations);
4. provision the restricted logins the gear is handed: ``orders_runtime`` (member of
   ``bss_orders_runtime`` only) and one ``LOGIN`` per worker class (``bss_orders_<class>``
   only), per the provisioning contract in ``gears/bss/orders/README.md``;
5. start the server **as the runtime login** and wait for ``/healthz`` (liveness) and then
   ``/readyz`` (the gear's readiness: producer bound, engine mounted, store answering);
6. boot 1 seeds the seller's catalog in the real Pricing and Products over REST
   (:func:`seed_pricing`), and the suite writes the development stub fixture keyed by the seeded
   ids (:func:`write_fixture`, M3B-7);
7. boot 2: stop the server, re-render the config with the ``development_delivery`` gate block
   (``dev_stubs``, the fixture path, the seller's commercial identity) and start it again (M3B-5);
8. yield the environment; tests talk HTTP and read persisted side effects through ``psql``.

Milestone 3b adds the development delivery: the build line (``e2e.yaml``) enables
``bss_orders/gate-dev-stubs`` and the suite runs the server **twice** after ``migrate``. Boot 1 has no
gate block, so the seed can author the seller's real Products and Pricing data over REST (a SKU
with its invoice inputs, a plan published under approval quorum 0) as a catalog-author user.
The per-run stub fixture is then written, keyed by the seeded ids. Boot 2 adds the ``gate``
overlay (``dev_stubs``, the fixture, the seller's commercial identity, Preview and submit
throttles) from the ``development_delivery`` block of ``config.yaml``. ``test_preview.py`` and
``test_submit.py`` run on boot 2 using the suite-local PostgreSQL sidecar.

The gear's configuration is the checked-in fragment
``gears/bss/orders/config/e2e-orders.yaml`` deep-merged into
``config.yaml`` here, so the identities, partition count, producer grants and throttling zones
the gear's own tests pin are the ones the live server runs.
"""

from __future__ import annotations

import datetime
import json
import os
import secrets
import subprocess
import tempfile
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import httpx
import pytest
import yaml

from lib.sidecars import skip_without_docker

from .postgres_sidecar import PostgresSidecar

HERE = Path(__file__).resolve().parent
PROJECT_ROOT = Path(__file__).resolve().parents[5]
CONFIG = HERE / "config.yaml"
FRAGMENT = PROJECT_ROOT / "gears" / "bss" / "orders" / "config" / "e2e-orders.yaml"
# The development stub fixture's template (Task 8b); write_fixture() rekeys it by the seeded ids.
FIXTURE_TEMPLATE = PROJECT_ROOT / "gears" / "bss" / "orders" / "config" / "gate-dev-stubs.example.json"

SERVER_PORT = 8090
BASE_URL = f"http://127.0.0.1:{SERVER_PORT}"
API = "/bss-orders/v1"
PRICING = "/bss-pricing/v1"
PRODUCTS = "/bss-products/v1"
REQUEST_TIMEOUT = 5.0
# Liveness/readiness waits: a cold start registers every gear's GTS entities and binds the
# managed producer; these bound session setup only (per-test budgets stay at pytest.ini's).
LIVENESS_TIMEOUT = 90
READINESS_TIMEOUT = 60

# ── Identities (must match config.yaml and the gear fragment) ───────────────
ROOT_TENANT = "00000000-df51-5b42-9538-d2b56b7ee953"
RESOURCE_TENANT = "a0000000-0000-4000-8000-00000000000a"
SELLER_TENANT = "5e11e000-0000-4000-8000-000000000005"
PAYER_TENANT = "9a9e0000-0000-4000-8000-000000000009"
FOREIGN_TENANT = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
PARTNER_TENANT = "d0000000-0000-4000-8000-00000000000d"
SERVICE_TENANT = "0e0e0000-0000-4000-8000-0000000000aa"

WORKFLOW_SUBJECT = "0e0e0000-0000-4000-8000-000000000002"
SELLER_SUBJECT = "55555555-6a88-4768-9dfc-6bcd5187d9ed"
# Seeds the seller's catalog in Pricing and Products; no Orders rule names it.
CATALOG_AUTHOR_SUBJECT = "55555556-6a88-4768-9dfc-6bcd5187d9ed"
# The seller's commercial identity (D-194): the s2s credential's service subject. Distinct from
# the Orders event producer (``…0005``, the fragment's ``events.producer_subject_id``).
SELLER_SERVICE_SUBJECT = "0e0e0000-0000-4000-8000-000000000006"

# Eight buyers of resource tenant A with identical grants: the gateway caller zone is keyed per
# authenticated subject, so every test authors as its own buyer and spends its own budget.
BUYERS = {f"buyer-{i}": f"1111111{i}-6a88-4768-9dfc-6bcd5187d9ed" for i in range(1, 9)}

TOKENS = {
    **{name: f"e2e-orders-{name}" for name in BUYERS},
    "member": "e2e-orders-member",
    "seller": "e2e-orders-seller",
    "payer": "e2e-orders-payer",
    "foreign": "e2e-orders-foreign",
    "partner": "e2e-orders-partner",
    "workflow": "e2e-orders-workflow",
    "subscriptions": "e2e-orders-subscriptions",
    "catalog-author": "e2e-orders-catalog-author",
}
ACCEPTED_PROOF = "e2e-proof-ok"
FIXTURE_CONTRACT = "00000000-0000-0000-0000-0000000000c1"

NEW_SALE = "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1"

# A line's contract effective date: 30 days after the session's UTC today, computed once, so
# the date stays in the future whenever the suite runs.
CONTRACT_EFFECTIVE_DATE = (
    datetime.datetime.now(datetime.timezone.utc).date() + datetime.timedelta(days=30)
).isoformat()

# PostgreSQL provisioning (test identities only; the container is disposable).
ADMIN_USER, ADMIN_PASSWORD = "orders_admin", "orders-admin-e2e"
RUNTIME_USER, RUNTIME_PASSWORD = "orders_runtime", "orders-runtime-e2e"
WORKER_CLASSES = ("maintenance", "retention", "verifier", "checkpoint")
DB_NAME = "orders"


# ── Environment gate ───────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def _require_dedicated_binary():
    if not os.environ.get("E2E_BINARY"):
        pytest.skip(
            "E2E_BINARY not set — run these tests via: make e2e-orders",
            allow_module_level=True,
        )
    skip_without_docker()


def pytest_collection_modifyitems(items):
    """Charge session setup (container, migrate, two boots) to nothing but its own bounds.

    As in the usage-collector suite: ``func_only`` keeps pytest.ini's 10 s hard kill on each
    test body while the session fixture's startup cost is not counted against the first test.
    Filtered to items under this directory, because the hook sees every collected item.
    """
    for item in items:
        if HERE not in Path(str(item.fspath)).resolve().parents:
            continue
        if item.get_closest_marker("timeout") is None:
            item.add_marker(pytest.mark.timeout(func_only=True))


def pytest_configure(config):
    """Register this suite's markers (as the other suites' conftests do)."""
    config.addinivalue_line(
        "markers",
        "holds_claims: the test keeps its orders' overlap claims open and releases them itself "
        "(test_submit.py's ``_released_claims`` skips it)",
    )


# ── Configuration assembly ─────────────────────────────────────────────────

def _deep_merge(base: Any, overlay: Any) -> Any:
    """Recursive merge; mappings merge by key, lists concatenate, scalars override.

    Lists concatenate (unlike ``run_e2e.py``'s overlay merge) because the fragment contributes
    *additional* AuthZ rules and grants to the host policy rather than replacing it.
    """
    if isinstance(base, dict) and isinstance(overlay, dict):
        merged = dict(base)
        for key, value in overlay.items():
            merged[key] = _deep_merge(base[key], value) if key in base else value
        return merged
    if isinstance(base, list) and isinstance(overlay, list):
        return [*base, *overlay]
    return overlay


def _render_config(
    pg_port: str, pg_user: str, home: Path, seller_secret: str, fixture: Path | None = None
) -> Path:
    """Render one run's config; with ``fixture``, the development delivery's gate block too.

    ``development_delivery`` is never a server key: it is removed, and merged into ``gears`` only
    for boot 2 (``fixture`` given). The static AuthN plugin does not expand ``${VAR}``, so the
    seller's per-run client secret is substituted into its s2s credential here.
    """
    base = yaml.safe_load(CONFIG.read_text())
    fragment = yaml.safe_load(FRAGMENT.read_text())
    delivery = base.pop("development_delivery")
    base["gears"] = _deep_merge(base["gears"], fragment)
    if fixture is not None:
        base["gears"] = _deep_merge(base["gears"], delivery)
    text = yaml.safe_dump(base, sort_keys=False)
    text = (
        text.replace("__E2E_PG_PORT__", pg_port)
        .replace("__E2E_PG_USER__", pg_user)
        .replace("__E2E_HOME__", str(home))
        .replace("__E2E_ORDERS_SELLER_SECRET__", seller_secret)
    )
    if fixture is not None:
        text = text.replace("__E2E_FIXTURE__", str(fixture))
    out = tempfile.NamedTemporaryFile(
        prefix=f"e2e-orders-{pg_user}-", suffix=".yaml", mode="w", delete=False
    )
    out.write(text)
    out.close()
    return Path(out.name)


# ── Server process ─────────────────────────────────────────────────────────

@dataclass
class OrdersServer:
    """The server under test: its binary, rendered run-config and process environment."""

    binary: Path
    config: Path
    env: dict[str, str]
    log: Path
    proc: subprocess.Popen | None = None

    def start(self) -> None:
        log_fh = open(self.log, "a")  # noqa: SIM115 — closed with the process
        self.proc = subprocess.Popen(
            [str(self.binary), "--config", str(self.config), "run"],
            cwd=str(PROJECT_ROOT),
            stdout=log_fh,
            stderr=subprocess.STDOUT,
            env=self.env,
        )
        self.wait_live()
        self.wait_ready()

    def stop(self) -> None:
        if self.proc is None:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=10)
        self.proc = None

    def restart(self) -> None:
        self.stop()
        self.start()

    def _tail(self) -> str:
        return self.log.read_text()[-4000:] if self.log.exists() else ""

    def _wait(self, path: str, timeout: int, label: str) -> None:
        deadline = time.monotonic() + timeout
        last = "no response"
        while time.monotonic() < deadline:
            if self.proc is not None and self.proc.poll() is not None:
                pytest.fail(
                    f"server exited with {self.proc.returncode} before {label}\n{self._tail()}"
                )
            try:
                r = httpx.get(f"{BASE_URL}{path}", timeout=3)
                if r.status_code == 200:
                    return
                last = f"{r.status_code} {r.text[:300]}"
            except httpx.HTTPError as exc:
                last = repr(exc)
            time.sleep(0.5)
        pytest.fail(f"server not {label} within {timeout}s ({last})\n{self._tail()}")

    def wait_live(self) -> None:
        self._wait("/healthz", LIVENESS_TIMEOUT, "live")

    def wait_ready(self) -> None:
        self._wait("/readyz", READINESS_TIMEOUT, "ready")


@dataclass(frozen=True)
class Catalog:
    """The seller's seeded catalog (M3B-4): one published monthly flat plan on a EUR book."""

    plan_id: str
    plan_revision_id: str
    item_id: str
    sku_id: str
    currency: str


@dataclass
class OrdersEnv:
    """What the tests see: the base URL, the store, the server and the seeded catalog."""

    base_url: str
    pg: PostgresSidecar
    server: OrdersServer
    clients: dict[str, httpx.Client] = field(default_factory=dict)
    catalog: Catalog | None = None
    gate_fixture: Path | None = None

    def psql(self, sql: str) -> str:
        """Run ``sql`` against the Orders database as the container superuser."""
        return self.pg.psql(sql, db=DB_NAME)

    def count(self, sql: str) -> int:
        return int(self.psql(sql) or "0")

    def client(self, who: str = "buyer") -> httpx.Client:
        if who not in self.clients:
            self.clients[who] = httpx.Client(
                base_url=self.base_url,
                timeout=REQUEST_TIMEOUT,
                headers={"Authorization": f"Bearer {TOKENS[who]}"},
            )
        return self.clients[who]


def _resolve_binary() -> Path:
    binary = Path(os.environ["E2E_BINARY"])
    if not binary.is_absolute():
        binary = PROJECT_ROOT / binary
    if not binary.exists():
        pytest.fail(f"E2E_BINARY={binary} does not exist")
    return binary


def _provision_admin(pg: PostgresSidecar) -> None:
    pg.psql(
        f"CREATE ROLE {ADMIN_USER} LOGIN PASSWORD '{ADMIN_PASSWORD}' CREATEROLE;\n"
        f"CREATE DATABASE {DB_NAME} OWNER {ADMIN_USER};"
    )


def _provision_restricted_logins(pg: PostgresSidecar) -> dict[str, str]:
    """The runtime login and one login per worker class, each a member of exactly one group."""
    statements = [
        f"CREATE ROLE {RUNTIME_USER} LOGIN PASSWORD '{RUNTIME_PASSWORD}' INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE;",
        f"GRANT bss_orders_runtime TO {RUNTIME_USER};",
    ]
    dsns: dict[str, str] = {}
    for cls in WORKER_CLASSES:
        password = secrets.token_hex(8)
        statements.append(
            f"CREATE ROLE e2e_{cls} LOGIN PASSWORD '{password}' INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE;"
        )
        statements.append(f"GRANT bss_orders_{cls} TO e2e_{cls};")
        dsns[f"ORDERS_{cls.upper()}_DSN"] = (
            f"postgres://e2e_{cls}:{password}@127.0.0.1:{pg.dsn_port}/{DB_NAME}"
        )
    pg.psql("\n".join(statements), db=DB_NAME)
    return dsns


def _run_migrate(binary: Path, config: Path, env: dict[str, str], log: Path) -> None:
    with open(log, "a") as log_fh:
        result = subprocess.run(
            [str(binary), "--config", str(config), "migrate"],
            cwd=str(PROJECT_ROOT),
            stdout=log_fh,
            stderr=subprocess.STDOUT,
            env=env,
            timeout=300,
            check=False,
        )
    if result.returncode != 0:
        pytest.fail(f"`migrate` failed with {result.returncode}\n{log.read_text()[-4000:]}")


@pytest.fixture(scope="session")
def orders_env() -> OrdersEnv:
    binary = _resolve_binary()
    pg = PostgresSidecar()
    pg.start()
    try:
        home = Path(tempfile.mkdtemp(prefix="e2e-orders-home-"))
        logs_dir = PROJECT_ROOT / "testing" / "e2e" / "logs"
        logs_dir.mkdir(parents=True, exist_ok=True)
        log = logs_dir / f"cf-gears-e2e-{SERVER_PORT}-orders.log"
        log.write_text("")

        _provision_admin(pg)
        seller_secret = secrets.token_hex(16)
        secrets_env = {
            "ORDERS_AUDIT_MINIMIZATION_KEY": secrets.token_hex(24),
            # The Orders gate's commercial identity secret (`${ORDERS_SELLER_CLIENT_SECRET}`); the
            # static AuthN credential carries the same value, substituted by _render_config.
            "ORDERS_SELLER_CLIENT_SECRET": seller_secret,
            "ORDERS_PG_PASSWORD": ADMIN_PASSWORD,
            "RUST_LOG": os.environ.get("RUST_LOG", "info,bss_orders=debug"),
        }
        # Workers are configured in the fragment, so their DSN references must resolve even for
        # the migrate run (config validation precedes the DB phase); the roles do not exist yet,
        # which is fine: `migrate` never opens them.
        placeholder_dsns = {
            f"ORDERS_{cls.upper()}_DSN": f"postgres://pending:pending@127.0.0.1:{pg.dsn_port}/{DB_NAME}"
            for cls in WORKER_CLASSES
        }
        migrate_env = {**os.environ, **secrets_env, **placeholder_dsns}
        _run_migrate(
            binary, _render_config(pg.dsn_port, ADMIN_USER, home, seller_secret), migrate_env, log
        )

        dsns = _provision_restricted_logins(pg)
        run_env = {**os.environ, **secrets_env, **dsns, "ORDERS_PG_PASSWORD": RUNTIME_PASSWORD}
        server = OrdersServer(
            binary=binary,
            config=_render_config(pg.dsn_port, RUNTIME_USER, home, seller_secret),
            env=run_env,
            log=log,
        )
        env = OrdersEnv(base_url=BASE_URL, pg=pg, server=server)
        try:
            # Boot 1: the real catalog, seeded over REST; the gate block is not configured yet.
            server.start()
            catalog = seed_pricing(env)
            fixture = write_fixture(catalog, home)
            env.gate_fixture = fixture
            # Boot 2: the development delivery over the fixture keyed by the seeded ids.
            server.stop()
            server.config = _render_config(
                pg.dsn_port, RUNTIME_USER, home, seller_secret, fixture=fixture
            )
            server.start()
            env.catalog = catalog
            yield env
        finally:
            for client in env.clients.values():
                client.close()
            server.stop()
    finally:
        pg.stop()


# ── Catalog seed and stub fixture (M3B-4, M3B-5, M3B-7) ────────────────────

def _ok(r: httpx.Response, *statuses: int) -> dict[str, Any]:
    assert r.status_code in statuses, f"{r.request.method} {r.request.url}: {r.status_code} {r.text}"
    return r.json()


def _quorum_zero(api: httpx.Client, prefix: str) -> None:
    """The gear's default quorum 0, written under the policy's own ETag (P-D-205)."""
    r = api.get(f"{prefix}/approval-policy")
    _ok(r, 200)
    _ok(
        api.put(
            f"{prefix}/approval-policy", json={"quorum": 0}, headers={"If-Match": r.headers["etag"]}
        ),
        200,
    )


def seed_pricing(env: OrdersEnv, *, sku_kind: str = "recurring") -> Catalog:
    """Seed one published recurring plan in the seller's catalog, as the pricebook suite does.

    As the catalog author (seller tenant): Products quorum 0, a category and a recurring SKU
    published at submit; Pricing quorum 0, a EUR book, a monthly flat entry and a 30.00 price from
    today published with ``publish-changes``; a plan on the book whose rev 1, sold from today with
    the SKU and entry as its item, publishes at submit. The SKU carries its GL code, tax category,
    invoice line template and billing timing, and the resolved item must show all four. The ids are then read back the way a
    consumer reads them: ``GET /plans?sku_id=`` names the current revision and ``GET /resolve``
    its one item.
    """
    assert sku_kind in ("recurring", "one_time")
    api = env.client("catalog-author")
    run = uuid.uuid4().hex[:8]
    today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()

    def key() -> dict[str, str]:
        return {"Idempotency-Key": str(uuid.uuid4())}

    _quorum_zero(api, PRODUCTS)
    category = _ok(
        api.post(f"{PRODUCTS}/categories", json={"code": f"e2e-orders-{run}", "name": f"E2E orders {run}"}),
        201,
    )["id"]
    sku = _ok(
        api.post(
            f"{PRODUCTS}/skus",
            json={
                "code": f"E2E-ORDERS-{run}".upper(),
                "name": f"E2E orders {run}",
                "category_id": category,
                "type": sku_kind,
                # The SKU version's invoice inputs: Pricing's acceptance projection requires the
                # template, GL code and tax category (no tenant default is seeded), and resolve
                # takes the SKU version's before the tenant's (D-421).
                "gl_code": "4000",
                "tax_category": "standard",
                "invoice_line_template": f"E2E orders {run}",
                "billing_timing": "advance",
            },
        ),
        201,
    )["id"]
    assert _ok(api.post(f"{PRODUCTS}/skus/{sku}/submit", json={}), 200)["applied"] is True

    _quorum_zero(api, PRICING)
    book = _ok(
        api.post(
            f"{PRICING}/price-books",
            json={"code": f"eur-orders-{run}", "name": f"EUR orders {run}", "currency": "EUR"},
            headers=key(),
        ),
        201,
    )["id"]
    entry = _ok(
        api.post(
            f"{PRICING}/price-books/{book}/entries",
            json={"sku_id": sku, "period": "month" if sku_kind == "recurring" else None, "model": "flat"},
            headers=key(),
        ),
        201,
    )["id"]
    _ok(
        api.post(
            f"{PRICING}/price-book-entries/{entry}/prices",
            json={"price": {"amount": "30.00"}, "eligibility": "all", "effective_from": today},
            headers=key(),
        ),
        201,
    )
    published = _ok(api.post(f"{PRICING}/price-books/{book}/publish-changes", json={}, headers=key()), 201)
    assert published["applied"] is True, published

    # The plan: a price is sold only through a published plan revision naming its entry.
    plan = _ok(
        api.post(
            f"{PRICING}/plans",
            json={"code": f"PLAN-ORDERS-{run}".upper(), "name": f"Orders plan {run}", "book_id": book},
            headers=key(),
        ),
        201,
    )
    revision = plan["revisions"][0]["id"]
    r = api.get(f"{PRICING}/plan-revisions/{revision}")
    _ok(r, 200)
    _ok(
        api.patch(
            f"{PRICING}/plan-revisions/{revision}",
            json={"available_from": today},
            headers={"If-Match": r.headers["etag"]},
        ),
        200,
    )
    _ok(
        api.post(
            f"{PRICING}/plan-revisions/{revision}/items",
            json={"sku_id": sku, "price_book_entry_id": entry},
            headers=key(),
        ),
        201,
    )
    submitted = _ok(api.post(f"{PRICING}/plan-revisions/{revision}/submit", json={}, headers=key()), 201)
    assert submitted["revision"]["state"] == "published", submitted

    [row] = _ok(api.get(f"{PRICING}/plans", params={"sku_id": sku}), 200)["items"]
    current = row["current"]["revision_id"]
    assert (row["id"], current) == (plan["id"], revision), row
    resolved = _ok(api.get(f"{PRICING}/resolve", params={"plan_revision_id": current, "date": today}), 200)
    [item] = resolved["items"]
    assert item["sku_id"] == sku, resolved
    # Fail fast: an item without these inputs is refused at acceptance (`project_resolution`),
    # far from the seed that caused it.
    missing = [
        name
        for name in ("invoice_line_template", "gl_code", "tax_category", "billing_timing")
        if (item.get(name) or {}).get("value") is None
    ]
    assert not missing, f"seeded item resolves without invoice inputs {missing}: {item}"
    return Catalog(
        plan_id=row["id"],
        plan_revision_id=current,
        item_id=item["item_id"],
        sku_id=sku,
        currency=resolved["currency"],
    )


def _with_currency(value: Any, currency: str) -> Any:
    """``value`` with every ``currency`` member replaced (the template prices in USD)."""
    if isinstance(value, dict):
        return {
            k: (currency if k == "currency" else _with_currency(v, currency)) for k, v in value.items()
        }
    if isinstance(value, list):
        return [_with_currency(v, currency) for v in value]
    return value


def write_fixture(catalog: Catalog, home: Path) -> Path:
    """Write the development stub fixture (M3B-7) keyed by the seeded ids; return its path.

    From ``gate-dev-stubs.example.json``: the payer profile of ``PAYER_TENANT`` in the catalog's
    currency with a confirmed seller relationship, and of ``FOREIGN_TENANT`` without one; an
    active, payer-eligible ``FIXTURE_CONTRACT``; the seeded SKU's overlap key with occupancy limit 2
    at 0 active; terms and evaluation for the seeded revision; acceptance hold 600 s; assessment
    profile ``orders-gate/1``; totals, TCV and tax as the template, in the catalog's currency.
    """
    template = _with_currency(json.loads(FIXTURE_TEMPLATE.read_text()), catalog.currency)
    [profile] = template["payer_profiles"].values()
    [terms] = template["terms"].values()
    [evaluation] = template["evaluation"].values()
    [occupancy] = template["occupancy"].values()
    scope = f"e2e-scope-{catalog.sku_id}"
    fixture = {
        **template,
        "payer_profiles": {
            PAYER_TENANT: {**profile, "in_relationship_with_seller": True},
            FOREIGN_TENANT: {**profile, "in_relationship_with_seller": False},
        },
        "contracts": {FIXTURE_CONTRACT: template["contracts"][FIXTURE_CONTRACT]},
        "item_classifications": {catalog.sku_id: {
            "catalog_subscription_product_key": f"e2e-product-{catalog.sku_id}",
            "overlap_scope_key": scope,
            "classification": {"source": "orders-e2e-fixture", "reference": catalog.sku_id, "version": "1"},
        }},
        "occupancy": {scope: {**occupancy, "active_count": 0, "max_concurrent_active": 2}},
        "terms": {catalog.plan_revision_id: terms},
        "acceptance_policy": {**template["acceptance_policy"], "hold_seconds": 600},
        "assessment_profile": "orders-gate/1",
        "evaluation": {catalog.plan_revision_id: evaluation},
    }
    path = home / "gate-dev-stubs.json"
    path.write_text(json.dumps(fixture, indent=2))
    return path


# ── Request helpers ────────────────────────────────────────────────────────

def new_key() -> str:
    return f"e2e-{uuid.uuid4()}"


def create_body(**overrides: Any) -> dict[str, Any]:
    body = {
        "resource_tenant_id": RESOURCE_TENANT,
        "seller_tenant_id": SELLER_TENANT,
        "payer_tenant_id": PAYER_TENANT,
        "category": NEW_SALE,
    }
    body.update(overrides)
    return body


def line_body(revision: int | None, **overrides: Any) -> dict[str, Any]:
    body: dict[str, Any] = {
        "plan_id": str(uuid.uuid4()),
        "plan_revision_id": str(uuid.uuid4()),
        "selected_items": [{"item_id": str(uuid.uuid4()), "quantity": "2"}],
        "currency": "EUR",
        "contract_effective_date": CONTRACT_EFFECTIVE_DATE,
        "term_duration": {"kind": "periods", "count": 12},
        "billing_cycle": "month",
    }
    if revision is not None:
        body["expected_draft_revision"] = revision
    body.update(overrides)
    return body


def write_headers(version: int, key: str | None = None, **extra: str) -> dict[str, str]:
    headers = {"If-Match": f'"{version}"', "Idempotency-Key": key or new_key()}
    headers.update(extra)
    return headers


def line_from_catalog(catalog: Catalog, revision: int | None, quantity: str = "1", **overrides: Any) -> dict[str, Any]:
    """A Stage 2 ``line_body`` on the seeded plan: its revision, its one item, monthly, 12 periods."""
    return line_body(
        revision,
        plan_id=catalog.plan_id,
        plan_revision_id=catalog.plan_revision_id,
        selected_items=[{"item_id": catalog.item_id, "quantity": quantity}],
        currency=catalog.currency,
        billing_cycle="month",
        term_duration={"kind": "periods", "count": 12},
        **overrides,
    )


def preview_body(catalog: Catalog, lines: int = 1, payer: str = PAYER_TENANT) -> dict[str, Any]:
    """A Preview basket (``OrdersPreviewBasket``: the three tenant axes, no category) with
    ``payer`` and ``lines`` catalog lines."""
    return {
        "resource_tenant_id": RESOURCE_TENANT,
        "seller_tenant_id": SELLER_TENANT,
        "payer_tenant_id": payer,
        "lines": [line_from_catalog(catalog, None) for _ in range(lines)],
    }


def submit(
    buyer: Buyer,
    order_id: str,
    version: int,
    draft_revision: int,
    key: str | None = None,
) -> httpx.Response:
    """``POST /orders/{id}/submit`` (D-234) as ``buyer`` at ``version`` and ``draft_revision``."""
    return buyer.client.post(
        f"{API}/orders/{order_id}/submit",
        json={"expected_draft_revision": draft_revision},
        headers=write_headers(version, key),
    )


def create_draft(env: OrdersEnv, buyer: "Buyer", **overrides: Any) -> dict[str, Any]:
    """Create one draft as ``buyer`` and return the created view (asserting the 201)."""
    r = buyer.client.post(
        f"{API}/orders", json=create_body(**overrides), headers={"Idempotency-Key": new_key()}
    )
    assert r.status_code == 201, f"{r.status_code} {r.text}"
    return r.json()


@dataclass
class Buyer:
    """One authoring identity: its client and trusted subject id."""

    name: str
    subject_id: str
    client: httpx.Client


_buyer_cursor = {"next": 0}


@pytest.fixture
def env(orders_env: OrdersEnv) -> OrdersEnv:
    return orders_env


@pytest.fixture
def buyer(orders_env: OrdersEnv) -> Buyer:
    """A buyer identity used by this test alone (round-robin over the eight configured)."""
    names = list(BUYERS)
    name = names[_buyer_cursor["next"] % len(names)]
    _buyer_cursor["next"] += 1
    return Buyer(name=name, subject_id=BUYERS[name], client=orders_env.client(name))


@pytest.fixture
def catalog(orders_env: OrdersEnv) -> Catalog:
    """The seller's catalog seeded on boot 1."""
    assert orders_env.catalog is not None, "boot 1 seeds the catalog"
    return orders_env.catalog


@pytest.fixture
def seller(orders_env: OrdersEnv) -> Buyer:
    """The seller operator (reads, holds and resumes; never authors), shaped as a ``Buyer``."""
    return Buyer(name="seller", subject_id=SELLER_SUBJECT, client=orders_env.client("seller"))
