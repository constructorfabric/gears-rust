"""E2E fixtures for the bss-orders-lifecycle gear (S2-12 draft milestone).

This suite is self-managed: it owns a PostgreSQL container and the server process, because
the gear requires PostgreSQL, a configured fail-closed AuthZ policy (the rules provider) and
the Event Broker, none of which the shared ``make e2e-local`` server carries. It gates on
``E2E_BINARY`` (set by ``make e2e-orders-lifecycle`` / ``make e2e-local
SUITE=bss-orders-lifecycle``) and Docker, and skips otherwise.

Session setup, in order:

1. start the pinned ``postgres`` sidecar;
2. provision the schema-owner login ``orders_admin`` and the ``orders`` database;
3. run ``cf-gears-example-server --config <cfg> migrate`` **as the admin login** (the toolkit
   applies every gear's migrations, including the Orders role migrations);
4. provision the restricted logins the gear is handed: ``orders_runtime`` (member of
   ``bss_orders_runtime`` only) and one ``LOGIN`` per worker class (``bss_orders_<class>``
   only), per the provisioning contract in ``gears/bss/orders-lifecycle/README.md``;
5. start the server **as the runtime login** and wait for ``/healthz`` (liveness) and then
   ``/readyz`` (the gear's readiness: producer bound, engine mounted, store answering);
6. yield the environment; tests talk HTTP and read persisted side effects through ``psql``.

The gear's configuration is the checked-in fragment
``gears/bss/orders-lifecycle/config/e2e-orders-lifecycle.yaml`` deep-merged into
``config.yaml`` here, so the identities, partition count, producer grants and throttling zones
the gear's own tests pin are the ones the live server runs.
"""

from __future__ import annotations

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

from lib.sidecars import PostgresSidecar, skip_without_docker

HERE = Path(__file__).resolve().parent
PROJECT_ROOT = Path(__file__).resolve().parents[5]
CONFIG = HERE / "config.yaml"
FRAGMENT = PROJECT_ROOT / "gears" / "bss" / "orders-lifecycle" / "config" / "e2e-orders-lifecycle.yaml"

SERVER_PORT = 8090
BASE_URL = f"http://127.0.0.1:{SERVER_PORT}"
API = "/bss-orders-lifecycle/v1"
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
}
ACCEPTED_PROOF = "e2e-proof-ok"

NEW_SALE = "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1"

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
            "E2E_BINARY not set — run these tests via: make e2e-orders-lifecycle",
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


def _render_config(pg_port: str, pg_user: str, home: Path) -> Path:
    base = yaml.safe_load(CONFIG.read_text())
    fragment = yaml.safe_load(FRAGMENT.read_text())
    base["gears"] = _deep_merge(base["gears"], fragment)
    text = yaml.safe_dump(base, sort_keys=False)
    text = (
        text.replace("__E2E_PG_PORT__", pg_port)
        .replace("__E2E_PG_USER__", pg_user)
        .replace("__E2E_HOME__", str(home))
    )
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


@dataclass
class OrdersEnv:
    """What the tests see: the base URL, the store and the server."""

    base_url: str
    pg: PostgresSidecar
    server: OrdersServer
    clients: dict[str, httpx.Client] = field(default_factory=dict)

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
        log = logs_dir / f"cf-gears-e2e-{SERVER_PORT}-orders-lifecycle.log"
        log.write_text("")

        _provision_admin(pg)
        secrets_env = {
            "ORDERS_AUDIT_MINIMIZATION_KEY": secrets.token_hex(24),
            "ORDERS_PG_PASSWORD": ADMIN_PASSWORD,
            "RUST_LOG": os.environ.get("RUST_LOG", "info,bss_orders_lifecycle=debug"),
        }
        # Workers are configured in the fragment, so their DSN references must resolve even for
        # the migrate run (config validation precedes the DB phase); the roles do not exist yet,
        # which is fine: `migrate` never opens them.
        placeholder_dsns = {
            f"ORDERS_{cls.upper()}_DSN": f"postgres://pending:pending@127.0.0.1:{pg.dsn_port}/{DB_NAME}"
            for cls in WORKER_CLASSES
        }
        migrate_env = {**os.environ, **secrets_env, **placeholder_dsns}
        _run_migrate(binary, _render_config(pg.dsn_port, ADMIN_USER, home), migrate_env, log)

        dsns = _provision_restricted_logins(pg)
        run_env = {**os.environ, **secrets_env, **dsns, "ORDERS_PG_PASSWORD": RUNTIME_PASSWORD}
        server = OrdersServer(
            binary=binary,
            config=_render_config(pg.dsn_port, RUNTIME_USER, home),
            env=run_env,
            log=log,
        )
        server.start()
        env = OrdersEnv(base_url=BASE_URL, pg=pg, server=server)
        try:
            yield env
        finally:
            for client in env.clients.values():
                client.close()
            server.stop()
    finally:
        pg.stop()


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
        "contract_effective_date": "2026-12-01",
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
