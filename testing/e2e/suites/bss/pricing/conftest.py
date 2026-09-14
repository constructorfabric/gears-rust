"""E2E fixtures for the bss-pricing gear.

The wiring is already paid: ``bss-pricing`` is in ``config/e2e-features.txt``,
``config/e2e-local.yaml`` carries a gear block (SQLite, defaults, the
in-repository fixture registry), and ``static-authz-plugin`` answers a valid
tenant with ``decision: true`` plus an ``in`` predicate on ``owner_tenant_id`` —
which is exactly the flat-``In`` shape the gear's PEP compiles and which
``require_constraints = true`` needs.

The reachability probe below is a **graceful guard**, not a wiring gap: a server
built WITHOUT the ``bss-pricing`` cargo feature does not mount these routes, so
the probe would 404 and the whole module skips rather than emitting red failures
for endpoints that binary simply does not serve.
"""

import os
import pathlib
import sqlite3
import uuid

import httpx
import pytest

REQUEST_TIMEOUT = 5.0

# The gear's REST base path (design 3.3 / D-140's route shape).
API_BASE = "/bss-pricing/v1"

# ── Tenant identities (must match config/e2e-local.yaml static-authn-plugin) ──
#
# TENANT_A is the caller ``e2e-token-tenant-a`` authenticates as; TENANT_B is a
# different root outside A's subtree, and is the foreign caller the cross-tenant
# isolation test reads as.
TENANT_A_ID = "00000000-df51-5b42-9538-d2b56b7ee953"
TENANT_B_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"

# The two principal ids inside tenant A. They are what the two-person rule is
# about: `chk_pricing_approval_distinct_principals` compares IDENTITIES, so a
# second token in a second tenant proves nothing about it — the record would not
# even be visible.
SUBMITTER_PRINCIPAL_ID = "11111111-6a88-4768-9dfc-6bcd5187d9ed"
REVIEWER_PRINCIPAL_ID = "44444444-6a88-4768-9dfc-6bcd5187d9ed"


def base_url() -> str:
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


def token_a() -> str:
    return os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")


@pytest.fixture(scope="session", autouse=True)
def require_pricing_mounted():
    """Skip the whole module unless the pricing routes are mounted.

    Keys on ``GET /bss-pricing/v1/catalog-version/frontier`` — the simplest
    authenticated GET, and the only route the gear served before this group.

    The probe is **authenticated**: the API gateway answers 401 (not 404) for an
    unauthenticated request to any unknown path, so an auth-less probe could not
    tell "gear absent" (skip) from "auth required" (present). With a valid token
    an unknown route yields a clean 404.
    """
    url = f"{base_url()}{API_BASE}/catalog-version/frontier"
    try:
        with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
            response = client.get(url, headers={"Authorization": f"Bearer {token_a()}"})
    except httpx.HTTPError as exc:
        pytest.skip(f"cf-gears-server not reachable at {base_url()}: {exc}")
    if response.status_code == 404:
        pytest.skip(
            "bss-pricing REST endpoints are not mounted — this server was built "
            "without the `bss-pricing` cargo feature."
        )


@pytest.fixture
def api_base() -> str:
    return API_BASE


@pytest.fixture
def auth_headers() -> dict:
    """Tenant A's bearer token."""
    return {"Authorization": f"Bearer {token_a()}"}


@pytest.fixture
def auth_headers_tenant_b() -> dict:
    """Tenant B's bearer token — a root outside tenant A's subtree."""
    return {"Authorization": "Bearer e2e-token-tenant-b"}


@pytest.fixture
def auth_headers_reviewer() -> dict:
    """A **second principal inside tenant A** — the independent approver.

    The two-person rule is a rule about identities, not roles
    (`design/05-governance.md` S4), so the only token that can exercise its
    positive arm is one that shares tenant A's scope and differs in
    `subject_id`. Tenant B's token cannot: it cannot see the record at all.
    Added to ``config/e2e-local.yaml`` for this suite; nothing else reads it.
    """
    return {"Authorization": "Bearer e2e-token-tenant-a-reviewer"}


@pytest.fixture
def auth_headers_nil_tenant() -> dict:
    """A token static-authz denies (the nil-UUID tenant), for the 403 probe."""
    return {"Authorization": "Bearer e2e-token-nil-tenant"}


@pytest.fixture
def client():
    with httpx.Client(base_url=base_url(), timeout=REQUEST_TIMEOUT) as http:
        yield http


@pytest.fixture
def idempotency_key() -> str:
    """A fresh client key per test, so a rerun is never answered a replay."""
    return str(uuid.uuid4())


# ── The audit trail, read where the deployment actually keeps it ─────────────
#
# The gear serves no audit endpoint — `pricing_audit_log` is an append-only,
# hash-chained store with no read route in the design set's endpoint map — so
# the only honest way to assert that a refusal LANDED a record is to read the
# deployment's own SQLite file. mini_chat's e2e does the same thing for the same
# reason, and read-only URI mode keeps this out of the server's way.


def audit_db_path() -> pathlib.Path:
    """Where ``config/e2e-local.yaml`` puts this gear's SQLite database."""
    home = os.path.expanduser(os.getenv("CF_GEARS_HOME", "~/.cf-gears"))
    return pathlib.Path(home) / "bss-pricing" / "bss_pricing.db"


def grant_window_coverage(price_id: str) -> str:
    """Give a price row the window ``inst-wc-required`` demands, and return its id.

    # Why this reaches the database instead of an endpoint

    S7 ``inst-wc-required`` makes publish fail for a billable row whose canonical
    scope key holds no active or scheduled ``PriceWindow``. Every plan this module
    assembles carries one billable row, so without a window **no publish here can
    reach materiality at all**.

    **The reason this is not a POST has been wrong twice, and both versions are
    withdrawn rather than left standing.** It first read that
    ``POST /bss-pricing/v1/prices/{priceId}/windows`` is "not mounted yet" and that "a
    fixture cannot use a door that does not exist"; the door exists, and
    :func:`test_the_window_route_is_mounted_and_requires_its_idempotency_key` drives
    it. It then read that the route can never author a plan's **first** window because
    the first publish needs a window and the window needs a publish — a *deadlock* —
    and that was false too: ``coverage::check`` ranges over the **billable** set, so a
    plan with **no price row** presents no key to find uncovered and publishes on its
    shape alone, which leaves a current revision for the window mutation to freeze.
    The order is forced, not blocked: empty publish, then the row, then the window,
    then the publish that freezes them. The in-crate
    ``rest_windows.rs::a_plans_first_window_is_authorable_through_the_routes_after_an_empty_publish``
    executes exactly that through the mounted routes.

    **What is true, and is the whole of why this fixture survives, is about this
    deployment rather than about the gear.** No ``CatalogVersionRegistryV1``
    implementation exists in this repository and ``config/e2e-local.yaml`` deliberately
    exposes no key that would supply one, so every commit arm fails closed at the
    version request — including the empty first publish's, which
    :func:`test_an_empty_first_publish_stops_at_the_absent_registry` drives to that
    503 and then reads the plan back still ``draft``. No plan here ever holds a current
    revision, so no window here can be authored through a route, and the two-publish
    sequence stops at step one.

    The window is therefore still written where the deployment keeps it, for the same
    reason :func:`audit_segment` reads the audit chain there: the honest alternative
    is not a cleaner fixture, it is no fixture. This weakens nothing — the gear's rule
    is untouched and still refuses an uncovered row, which
    :func:`test_a_billable_row_with_no_window_is_refused_on_the_wire` asserts on this
    very deployment. It is the same move the in-crate fixtures make by calling
    ``window_repo::schedule``, and they now say the same thing about why.

    **When the registry gear lands, this function is deleted and its callers post.**
    That is the whole of the migration, and nothing about it is a design decision:
    the sequence already works where a registry answers.

    # The uuids go in as **bytes** and the instants are never formatted

    ``SeaORM`` stores a ``Uuid`` in this ``SQLite`` mirror as a 16-byte **blob**,
    not as its hyphenated text — the column is declared ``text`` and ``SQLite`` is
    dynamically typed, so the declaration says nothing about what is in it. A
    string-bound ``price_id`` matches no row and the ``INSERT ... SELECT`` silently
    inserts nothing, which is why the row count is asserted rather than assumed.
    :func:`audit_segment` binds ``chain_id`` the same way for the same reason.

    # The instants come out of the row, not out of Python

    ``effective_to`` is ``NULL`` (open-ended), so an open-ended window needs no
    second instant at all. ``effective_from`` is the price row's own
    ``created_at_utc`` **with its year replaced**, in SQL —
    ``'2099-08-04' || substr(created_at_utc, 11)``.

    Nothing here formats a timestamp, and that is still the rule rather than a
    leftover: SeaORM writes ISO 8601 with a ``T`` separator and sub-second digits
    (``2026-08-04T18:18:35.728561+00:00``), the table's CHECKs compare these columns
    as **text**, and the activation sweep's due-read compares ``effective_from``
    against a bound instant the same way — so a value this fixture formatted itself
    would sort against the stored ones on a byte comparison nobody intended. The
    concatenation takes the ten-character ISO date off the front and keeps **every
    remaining byte** the store wrote: separator, precision, offset. The only literal
    is a calendar date.

    # Why the year moves, which is the whole reason this is not a plain copy

    ``created_at_utc`` is "a moment ago", so a window starting there is **already
    due**: production spawns the ``WindowActivationJob`` ticker, and it flips the
    window ``scheduled -> active`` on its next pass — within sixty seconds of boot,
    against a run that finishes in about thirteen seconds. The coverage assertion in
    ``test_the_coverage_report_answers_for_the_key_the_refusal_named`` reads
    ``intervals[0]["state"] == "scheduled"`` and was therefore **winning a race**,
    not asserting a fact. Dated 2099 the sweep cannot reach it: ``inst-ws-activate``
    fires on ``now >= effectiveFrom`` and no clock this suite runs under is inside
    the interval. It is the same correction ``tests/common/mod.rs``'s
    ``COVERAGE_FROM_UTC`` took on the in-crate side, for the same reason and to the
    same date.

    A ``scheduled`` window satisfies ``inst-wc-required`` whatever its date — the
    rule asks for an active **or scheduled** window — so nothing about the publish
    path weakens.
    """
    path = audit_db_path()
    if not path.exists():
        pytest.skip(f"the gear's SQLite database is not at {path}")
    window_id = str(uuid.uuid4())
    conn = sqlite3.connect(path, timeout=10)
    try:
        with conn:
            changed = conn.execute(
                """
                INSERT INTO pricing_price_window
                    (window_id, tenant_id, price_id, effective_from, effective_to,
                     state, reason_code, created_by, created_at)
                SELECT ?, tenant_id, price_id,
                       '2099-08-04' || substr(created_at_utc, 11), NULL,
                       'scheduled', 'e2eCoverage', created_by, created_at_utc
                  FROM pricing_price
                 WHERE price_id = ?
                """,
                (uuid.UUID(window_id).bytes, uuid.UUID(price_id).bytes),
            ).rowcount
        stored = conn.execute(
            "SELECT effective_from FROM pricing_price_window WHERE window_id = ?",
            (uuid.UUID(window_id).bytes,),
        ).fetchone()
    finally:
        conn.close()
    assert changed == 1, (
        f"no price row {price_id} to hang a window on; the seed did not author one"
    )
    # The premise of the concatenation above, asserted rather than assumed: the
    # stored instant is text whose first ten characters are an ISO date. A store
    # that wrote an epoch integer instead would produce a nonsense instant here and
    # a confusing coverage assertion three hundred lines away.
    assert stored is not None and str(stored[0]).startswith("2099-08-04T"), (
        f"the window's start is not the far-future instant this fixture wrote: {stored!r}"
    )
    return window_id


@pytest.fixture
def audit_segment():
    """Read one plan's audit segment, in ``seq`` order, as dicts.

    Keyed on the plan id because D-135 segments the chain on the audited
    subject's **aggregate**: a plan's segment holds every record of that plan
    and of no other, which is what makes "the deny landed on this plan's trail"
    an assertion rather than a search of a shared table.
    """

    def read(plan_id: str):
        path = audit_db_path()
        if not path.exists():
            pytest.skip(f"the gear's SQLite database is not at {path}")
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        conn.row_factory = sqlite3.Row
        try:
            rows = conn.execute(
                "SELECT * FROM pricing_audit_log WHERE chain_id = ? ORDER BY seq",
                (uuid.UUID(plan_id).bytes,),
            ).fetchall()
        finally:
            conn.close()
        return [dict(row) for row in rows]

    return read
