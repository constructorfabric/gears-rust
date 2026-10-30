"""Live HTTP/PostgreSQL E2E for Preview over the development delivery (Orders Stage 3, M3b).

Every test is a buyer (or a caller who is not one) driving ``POST /bss-orders/v1/orders/preview``
on boot 2 of the suite: the real Pricing and Products answer the catalog ports for the plan seeded
on boot 1, and the development stubs answer the nine undelivered ports from the per-run fixture
``write_fixture`` keys by the seeded ids (M3B-7). Assertions read the response body by its wire
names (``OrdersPreviewResult`` in ``orders/src/api/rest/dto.rs``) and the store through
``env.psql``.

Preview is nonbinding: a gate run whose failures are reported **inside** a 200 result
(``assessment.failures``, D-233 shape) rather than as a refusal. ``PreviewService::run``
(``orders/src/infra/gate/preview.rs``) refuses only before the run (``request-invalid``, the
per-caller limiter, the axis-specific authorization) or when the run, the date basis or the
diagnostics write cannot complete (sanitized 503). An unconfirmed payer relationship (D-232) and a
port the fixture does not state (``Unavailable(Missing)``, D-231) are therefore asserted as 200
assessments naming the registered reason, with the reason's catalog row checked to be the
refusal submit would answer.

Reason tokens, error codes and statuses are read from ``orders-sdk/contracts/catalog.json``,
never written as literals here.
"""

from __future__ import annotations

import json
import time

import pytest
import yaml

from .conftest import (
    API,
    CONFIG,
    FOREIGN_TENANT,
    FIXTURE_CONTRACT,
    PARTNER_TENANT,
    PAYER_TENANT,
    PROJECT_ROOT,
    RESOURCE_TENANT,
    SELLER_TENANT,
    Buyer,
    Catalog,
    OrdersEnv,
    preview_body,
)

PREVIEW = f"{API}/orders/preview"
PROBLEM = "application/problem+json"
RESOURCE_EXHAUSTED = "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
CATALOG_JSON = PROJECT_ROOT / "gears" / "bss" / "orders" / "orders-sdk" / "contracts" / "catalog.json"
# The only registered `tcvWithheld.reason` (DESIGN reason table, D-125): an annotation of a
# successful response, not an error row, so it is not in catalog.json `errors[]`. Pinned by
# `TCV_WITHHELD_REASON` in `orders/src/domain/gate/report.rs` and `gate_preview_shape.rs`.
TCV_WITHHELD_REASON = "preview-term-or-cycle-missing"
# The predicates D-232 item 6 withholds while the payer relationship is unconfirmed (per line).
PAYER_WITHHELD = (
    "orders.market-consistency",
    "orders.overlap-uniqueness",
    "orders.in-flight-per-key",
    "orders.pin-composition",
)
# The nine undelivered ports of D-231 the development stubs answer (`GatePort::ALL` minus the
# three catalog ports, `orders/src/domain/gate/ports.rs`); twelve ports in all.
STUBBED_PORTS = 9
GATE_PORTS = 12
# `governor`'s `Quota::per_minute(n)` replenishes one token every 60 / n seconds.
SECONDS_PER_MINUTE = 60.0
# The gateway caller zone refills at 3/s (`rl_orders_caller_write`, the gear fragment); a pause of
# one refill interval before each request past the gear's budget keeps the gateway's own 429 out.
GATEWAY_REFILL_SECONDS = 0.34


# ── Private helpers (not in conftest.py) ───────────────────────────────────


def _registered(reason: str) -> dict:
    """The catalog row of ``reason``: its ``error_code``, ``http_status`` and Problem type."""
    rows = json.loads(CATALOG_JSON.read_text())["errors"]
    [row] = [r for r in rows if r["reason"] == reason]
    return row


def _refused(r, reason: str) -> dict:
    """Assert ``r`` is the registered Problem of ``reason`` (status, code, domain, type)."""
    row = _registered(reason)
    assert r.status_code == row["http_status"], f"{r.status_code} {r.text}"
    assert r.headers["content-type"].startswith(PROBLEM), r.headers.get("content-type")
    body = r.json()
    assert body["error_code"] == row["error_code"], body
    assert body["error_domain"] == row["error_domain"], body
    assert body["type"] == row["type"], body
    return body


def _preview(buyer: Buyer, body: dict) -> dict:
    """Preview ``body`` as ``buyer``; assert the 200 and return the result."""
    r = buyer.client.post(PREVIEW, json=body)
    assert r.status_code == 200, f"{r.status_code} {r.text}"
    assert r.headers["content-type"].startswith("application/json"), r.headers.get("content-type")
    return r.json()


def _gate_rows(env: OrdersEnv, run_id: str) -> int:
    return env.count(f"SELECT count(*) FROM bss_orders__gate_outcome WHERE run_id = '{run_id}'")


def _outcomes(result: dict, predicate: str) -> list[dict]:
    return [o for o in result["assessment"]["outcomes"] if o["predicate"] == predicate]


def _assert_money(money: dict, currency: str) -> None:
    """``money`` is an ``OrdersMoney``: canonical integer text over a positive denominator, the
    given currency, and a display text."""
    exact = money["exact"]
    assert exact["currency"] == currency, money
    assert exact["numerator"].lstrip("-").isdigit() and exact["denominator"].isdigit(), money
    assert int(exact["denominator"]) > 0 and isinstance(exact["currency_scale"], int), money
    assert isinstance(money["display"], str) and money["display"], money


def _preview_per_minute() -> int:
    """The suite's Preview budget: the ``development_delivery`` overlay of ``config.yaml``."""
    overlay = yaml.safe_load(CONFIG.read_text())["development_delivery"]
    return int(overlay["bss-orders"]["config"]["gate"]["preview_per_caller_per_minute"])


# ── The happy path ─────────────────────────────────────────────────────────


@pytest.mark.smoke
def test_preview_happy_path_returns_lines_totals_and_tcv_and_writes_only_diagnostics(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    orders_before = env.count("SELECT count(*) FROM bss_orders__order")
    audits_before = env.count("SELECT count(*) FROM bss_orders__transition_audit")
    body = preview_body(catalog)
    result = _preview(buyer, body)

    # The assessment passed: no blocking failure, every required outcome passed, and the stored
    # assessment identity is the report's.
    assessment = result["assessment"]
    assert result["assessment_id"] == assessment["assessment_id"]
    assert assessment["failures"] == [], assessment["failures"]
    assert assessment["mapping_version"], assessment
    required = [o for o in assessment["outcomes"] if o["applicability"] == "required"]
    assert required and all(o["verdict"] == "passed" for o in required), required

    # One line, its pin composed on the seeded revision (the pin-composition row of that line
    # passed), and no unexpected wire member.
    [line] = result["lines"]
    assert line["pin"] == {"kind": "composed"}, line
    [pin_row] = [o for o in _outcomes(result, "orders.pin-composition") if o.get("line_id") == line["line_id"]]
    assert pin_row["verdict"] == "passed", pin_row

    # Totals and TCV: present, in EUR, and shaped as `OrdersMoney` (exact fraction + display). The
    # fixture's figures are placeholders (not derived from the seeded price), so no amount is
    # compared; per-line tax is not asserted (the template keeps its own line id).
    assert catalog.currency == "EUR"
    total = result["resolved_total"]
    assert total["exclusions"] == {}
    assert result["tcv_basis"] == "rolling_annualized"
    assert total["currency"] == catalog.currency
    assert isinstance(total["currency_scale"], int) and isinstance(total["usage_flagged"], bool)
    assert total["discount"]["kind"] in ("available", "unavailable"), total
    for money in (total["gross"], total["net"], result["tcv"]):
        _assert_money(money, catalog.currency)
    assert "tcvWithheld" not in result

    # The expected fulfillment time is present exactly when a line is deferred, and is the
    # latest deferred activation date at midnight UTC (`report::preview_result`).
    deferrals = [ln["deferral"] for ln in result["lines"] if "deferral" in ln]
    if deferrals:
        assert result["expected_fulfillment_time"].startswith(f"{max(deferrals)}T00:00:00"), result
    else:
        assert "expected_fulfillment_time" not in result, result

    # Only diagnostics are written: one gate_outcome row per reported outcome under the run id,
    # owned by the caller and attached to no order; no order and no audit row.
    run_id = result["assessment_id"]
    assert _gate_rows(env, run_id) == len(assessment["outcomes"])
    assert env.psql(
        "SELECT DISTINCT subject_id || '|' || resource_tenant_id || '|' || seller_tenant_id"
        " || '|' || payer_tenant_id || '|' || (order_id IS NULL)::int"
        f" FROM bss_orders__gate_outcome WHERE run_id = '{run_id}'"
    ) == f"{buyer.subject_id}|{RESOURCE_TENANT}|{SELLER_TENANT}|{PAYER_TENANT}|1"
    assert env.count("SELECT count(*) FROM bss_orders__order") == orders_before
    assert env.count("SELECT count(*) FROM bss_orders__transition_audit") == audits_before

    # A second identical Preview is a new run: a new assessment and line identity.
    again = _preview(buyer, body)
    assert again["assessment_id"] != run_id
    assert again["lines"][0]["line_id"] != line["line_id"]
    assert _gate_rows(env, again["assessment_id"]) == len(again["assessment"]["outcomes"])
    assert env.count("SELECT count(*) FROM bss_orders__order") == orders_before


# ── TCV withheld (D-125) ───────────────────────────────────────────────────


def test_preview_withholds_tcv_when_a_line_has_no_term(env: OrdersEnv, buyer: Buyer, catalog: Catalog):
    body = preview_body(catalog)
    del body["lines"][0]["term_duration"]
    result = _preview(buyer, body)

    [line] = result["lines"]
    assert result["tcvWithheld"] == {"reason": TCV_WITHHELD_REASON, "lineIds": [line["line_id"]]}, result
    assert "tcv" not in result, result
    assert "tcv_basis" not in result, result
    # A withheld TCV is an annotation of a successful response, never a refusal: every required
    # outcome still passed (a tcv-only row may be unevaluable and listed in `failures`, which
    # names every outcome that did not pass, blocking or not).
    required = [o for o in result["assessment"]["outcomes"] if o["applicability"] == "required"]
    assert required and all(o["verdict"] == "passed" for o in required), required
    assert _gate_rows(env, result["assessment_id"]) == len(result["assessment"]["outcomes"])


# ── An unconfirmed payer relationship (D-232, D-233, D-234) ────────────────


def test_preview_reports_an_unconfirmed_payer_relationship(env: OrdersEnv, buyer: Buyer, catalog: Catalog):
    """``write_fixture`` states ``FOREIGN_TENANT``'s payer profile with no seller relationship.

    The policy names no payer (D-215), so the buyer is authorized; the gate's predicate 1 fails
    with ``payer-relationship-unconfirmed`` and withholds every payer-dependent row under the same
    reason (D-232 item 6). Preview reports it in its 200 assessment, the same all-failures report
    submit carries in ``context.data.assessment`` of the registered 400.
    """
    reason = "payer-relationship-unconfirmed"
    row = _registered(reason)
    assert (row["error_code"], row["http_status"]) == ("PAYER_RELATIONSHIP_UNCONFIRMED", 400), row

    result = _preview(buyer, preview_body(catalog, payer=FOREIGN_TENANT))
    assessment = result["assessment"]
    [line] = result["lines"]
    [axis] = _outcomes(result, "orders.axis-validity")
    assert (axis["verdict"], axis.get("reason")) == ("failed", reason), axis
    assert reason in {f["reason"] for f in assessment["failures"]}, assessment["failures"]
    for predicate in PAYER_WITHHELD:
        rows = [o for o in _outcomes(result, predicate) if o.get("line_id") == line["line_id"]]
        assert rows, predicate
        assert all((o["verdict"], o.get("reason")) == ("unevaluable", reason) for o in rows), rows
    # The catalog pin may compose (the catalog is not payer-dependent), but its row is withheld:
    # required, under predicate 1's reason, never the port's `catalog-pin-composition-unavailable`.
    [pin_row] = [o for o in _outcomes(result, "orders.pin-composition") if o.get("line_id") == line["line_id"]]
    assert pin_row["applicability"] == "required", pin_row

    # The diagnostics are stored exactly as reported, under the foreign payer.
    run_id = result["assessment_id"]
    assert _gate_rows(env, run_id) == len(assessment["outcomes"])
    assert env.count(
        "SELECT count(*) FROM bss_orders__gate_outcome"
        f" WHERE run_id = '{run_id}' AND payer_tenant_id = '{FOREIGN_TENANT}'"
        f" AND predicate = 'orders.axis-validity' AND verdict = 'failed' AND reason = '{reason}'"
    ) == 1


@pytest.mark.parametrize(
    ("contract_id", "verdict", "reason"),
    [
        (FIXTURE_CONTRACT, "passed", None),
        ("00000000-0000-0000-0000-0000000000c2", "unevaluable", "contract-resolution-unavailable"),
    ],
)
def test_preview_checks_the_order_contract(
    buyer: Buyer, catalog: Catalog, contract_id: str, verdict: str, reason: str | None
):
    result = _preview(buyer, {**preview_body(catalog), "contract_id": contract_id})
    [contract] = _outcomes(result, "orders.contract-active")
    assert contract.get("line_id") is None, contract
    assert (contract["verdict"], contract.get("reason")) == (verdict, reason), contract
    failures = result["assessment"]["failures"]
    if reason is None:
        assert failures == [], failures
    else:
        assert _registered(reason)["http_status"] == 503
        assert any(f["reason"] == reason and f.get("line_id") is None for f in failures), failures


# ── A port the fixture does not state (D-231 `Unavailable(Missing)`) ───────


def test_preview_reports_unavailable_when_a_fixture_port_is_missing(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """``write_fixture`` states payer profiles for ``PAYER_TENANT`` and ``FOREIGN_TENANT`` only.

    A basket paid by ``PARTNER_TENANT`` (a real tenant, authorized because the policy names no
    payer) makes the stubbed identity-profile port answer ``Unavailable(Missing)``: D-231's
    inventory maps that port to ``identity-party-unavailable``, which predicate 4-Orders reports
    unevaluable for the line (``assess``: the identity port outranks the assessment). The basket
    keeps the seeded plan revision, because a revision the fixture does not state is first refused
    by the real Pricing ``resolve`` as unresolvable, never as a missing stub.
    """
    reason = "identity-party-unavailable"
    row = _registered(reason)
    assert (row["canonical_category"], row["http_status"]) == ("ServiceUnavailable", 503), row

    result = _preview(buyer, preview_body(catalog, payer=PARTNER_TENANT))
    [line] = result["lines"]
    market = [
        o for o in _outcomes(result, "orders.market-consistency") if o.get("line_id") == line["line_id"]
    ]
    assert [(o["verdict"], o.get("reason")) for o in market] == [("unevaluable", reason)], market
    failures = result["assessment"]["failures"]
    assert any(f["reason"] == reason and f.get("line_id") == line["line_id"] for f in failures), failures
    run_id = result["assessment_id"]
    assert env.count(
        "SELECT count(*) FROM bss_orders__gate_outcome"
        f" WHERE run_id = '{run_id}' AND verdict = 'unevaluable' AND reason = '{reason}'"
    ) >= 1


# ── Callers who may not purchase ───────────────────────────────────────────


def test_preview_denies_seller_only_and_foreign_callers(
    env: OrdersEnv, buyer: Buyer, seller: Buyer, catalog: Catalog
):
    """As create in the Stage 2 matrix (``test_authorization_matrix_and_tenant_isolation``): a new
    arrangement has no target to hide, so every denial is the 403 for the actor; Preview writes
    no audit row on denial and runs no gate, so no diagnostics row either."""
    reason = "operation-not-permitted-for-actor"
    rows_before = env.count("SELECT count(*) FROM bss_orders__gate_outcome")
    audits_before = env.count("SELECT count(*) FROM bss_orders__transition_audit")
    body = preview_body(catalog)
    # The seller operator reads but never purchases.
    _refused(seller.client.post(PREVIEW, json=body), reason)
    # A foreign tenant's caller holds no Orders grant at all.
    _refused(env.client("foreign").post(PREVIEW, json=body), reason)
    # A buyer previewing for a resource tenant it is not authorized on.
    _refused(buyer.client.post(PREVIEW, json={**body, "resource_tenant_id": FOREIGN_TENANT}), reason)
    assert env.count("SELECT count(*) FROM bss_orders__gate_outcome") == rows_before
    assert env.count("SELECT count(*) FROM bss_orders__transition_audit") == audits_before


# ── The per-caller limiter (F03 §4.6, D-52) ────────────────────────────────


@pytest.mark.timeout(60, func_only=True)
def test_preview_is_throttled_per_caller(env: OrdersEnv, buyer: Buyer, catalog: Catalog):
    """N previews per caller per minute (``preview_per_caller_per_minute`` of the suite's
    ``development_delivery`` overlay); request N+1 meets the gear's per-caller 429 with
    ``Retry-After``. The ``buyer`` fixture hands this test a buyer no other test of this file
    previews with (round-robin over eight); the budget is a ``governor`` bucket that refills one
    token every 60/N s, so a slow run may admit one more per elapsed refill interval, never fewer
    than N. ``orders_throttle_rejections_total`` is not observable over HTTP."""
    budget = _preview_per_minute()
    refill = SECONDS_PER_MINUTE / budget
    body = preview_body(catalog)
    runs = (
        "SELECT count(DISTINCT run_id) FROM bss_orders__gate_outcome"
        f" WHERE subject_id = '{buyer.subject_id}'"
    )
    runs_before = env.count(runs)
    admitted = 0
    refused = None
    started = time.monotonic()
    for attempt in range(budget + 10):
        if attempt >= budget:
            time.sleep(GATEWAY_REFILL_SECONDS)
        r = buyer.client.post(PREVIEW, json=body)
        if r.status_code == 429:
            refused = r
            break
        assert r.status_code == 200, f"attempt {attempt + 1}: {r.status_code} {r.text}"
        admitted += 1
    elapsed = time.monotonic() - started
    assert refused is not None, f"{admitted} previews admitted in {elapsed:.1f}s"
    refills = int(elapsed // refill)
    assert budget <= admitted <= budget + refills, (admitted, budget, elapsed)
    if refills == 0:
        assert admitted == budget

    assert refused.headers["content-type"].startswith(PROBLEM)
    retry_after = int(refused.headers["retry-after"])
    assert 1 <= retry_after <= int(refill) + 1, dict(refused.headers)
    problem = refused.json()
    assert problem["type"] == RESOURCE_EXHAUSTED, problem
    assert problem["detail"] == "Orders per-caller request limit exceeded", problem
    violation = problem["context"]["violations"][0]
    assert (violation["subject"], violation["description"]) == (
        "throttling",
        "per-caller request limit exceeded",
    ), violation
    # Every admitted run stored its diagnostics; the refused one ran nothing.
    assert env.count(runs) - runs_before == admitted


# ── Readiness (S3-14) ──────────────────────────────────────────────────────


def test_readiness_lists_stubbed_ports_and_closed_breakers(env: OrdersEnv):
    """``/readyz`` is the Stage 2 readiness test's endpoint and answers the status only; the
    gate manifest's one-line summary rides the Orders check's message on ``/health``
    (``GateReadiness::summary``): the catalog bound, no provider missing, the nine undelivered
    ports stubbed, and a breaker section that names only breakers that are not closed (empty
    when every breaker is closed)."""
    api = env.client("seller")
    assert api.get("/readyz").status_code == 200
    health = api.get("/health")
    assert health.status_code == 200, health.text
    [orders] = [c for c in health.json()["components"] if c["check"] == "bss-orders"]
    assert orders["status"] == "healthy", orders
    message = orders["message"]
    assert "gate: catalog bound" in message, message
    assert f"0 of {GATE_PORTS} ports provider-missing" in message, message
    assert f"{STUBBED_PORTS} stubbed (development only)" in message, message
    assert "breaker" not in message, message
