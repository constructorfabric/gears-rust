"""Live HTTP/PostgreSQL E2E for submit over the development delivery (Orders Stage 3, M3b).

Every test is a buyer (or a caller who is not one) driving
``POST /bss-orders/v1/orders/{orderId}/submit`` (D-238) on boot 2 of the suite: the real Pricing
and Products answer the catalog ports for the plan seeded on boot 1, the development stubs answer
the nine undelivered ports from the per-run fixture (M3B-7), and the development
``SellabilityStub`` issues Pricing's acceptance receipts (``gear.rs`` ``dev_acceptance``).
Assertions read the response by its wire names (``OrdersTransitionResult`` in
``orders/src/api/rest/dto.rs``, the settled Problem otherwise) and the store through ``env.psql``.

What a submit writes (``infra/gate/submit.rs``, ``infra/gate/commit.rs``, ``infra/engine``): a
commit appends version 2 (``create`` wrote version 1), one ``commercial_attempt`` with a receipt
per line, the pinned ``order_line`` rows, ``resolved_total`` rows (one per line plus the order
roll-up carrying the TCV), the overlap claim, the gate run's ``gate_outcome`` rows naming the
version, one committed audit row and one ``OrderSubmitted`` outbox message. A gate refusal
writes the run's rows (no version), one resolved refused audit row and the settlement; no
attempt. A refusal decided before the engine (authorization, a stale ``If-Match`` found at the
reservation, a key mismatch) writes one refused audit row and settles nothing.

Suite-level facts this file relies on:

* the overlap claim of a committed order blocks every later submit for the same payer, resource
  and overlap key (``uq_bss_orders__inflight_overlap_claim__0``; the fixture has one key and one
  confirmed payer), so an autouse fixture releases the open claims of the orders each test
  created, after it, as the store would when an order leaves flight (``bss_orders__claim_guard``
  admits exactly that update); a ``holds_claims`` test opts out and releases its own;
* the ``buyer`` fixture rotates eight buyers over the whole session; the throttle test is first
  in this file so its buyer has made no submit before it (no earlier file submits), and in the
  full run it is not the Preview throttle test's buyer. The ninth test of this file draws the
  throttle test's buyer again, while its submit bucket is still nearly empty, so the ninth is the
  seller test, whose buyer only authors the draft (the seller and the foreign caller submit).

Reason tokens, error codes and statuses are read from ``orders-sdk/contracts/catalog.json``,
never written as literals here.
"""

from __future__ import annotations

import json
import logging
import time
from collections import Counter

import pytest
import yaml

from .conftest import (
    API,
    CONFIG,
    FOREIGN_TENANT,
    PAYER_TENANT,
    RESOURCE_TENANT,
    PRODUCTS,
    PROJECT_ROOT,
    SELLER_SERVICE_SUBJECT,
    SELLER_SUBJECT,
    SELLER_TENANT,
    Buyer,
    Catalog,
    OrdersEnv,
    create_draft,
    line_from_catalog,
    new_key,
    preview_body,
    seed_pricing,
    submit,
    write_headers,
)

LOG = logging.getLogger(__name__)

PREVIEW = f"{API}/orders/preview"
PROBLEM = "application/problem+json"
RESOURCE_EXHAUSTED = "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
CATALOG_JSON = PROJECT_ROOT / "gears" / "bss" / "orders" / "orders-sdk" / "contracts" / "catalog.json"
# `governor`'s `Quota::per_minute(n)` replenishes one token every 60 / n seconds.
SECONDS_PER_MINUTE = 60.0
# The gateway caller zone refills at 3/s (`rl_orders_caller_write`, the gear fragment); a pause of
# one refill interval after each draft the throttle test creates keeps the zone's bucket full.
GATEWAY_REFILL_SECONDS = 0.34
# Drafts the throttle test creates beyond the budget: the limiter may admit one more per elapsed
# refill interval (6 s at 10/min), and the submits take well under that.
SPARE_DRAFTS = 3
# The Stage 2 `events.rs` query (`UNACKNOWLEDGED`): sequenced messages the leased processor has not
# acknowledged. `processed_seq` only grows, so its sum counts every acknowledged message even after
# the vacuum deleted the body (`toolkit_outbox_body` is not a stable count).
UNACKNOWLEDGED = (
    "SELECT count(*) FROM toolkit_outbox_outgoing o"
    " LEFT JOIN toolkit_outbox_processor p ON p.partition_id = o.partition_id"
    " WHERE o.seq > COALESCE(p.processed_seq, 0)"
)
PROCESSED = "SELECT COALESCE(sum(processed_seq), 0)::bigint FROM toolkit_outbox_processor"
INCOMING = "SELECT count(*) FROM toolkit_outbox_incoming"
DEAD_LETTERS = "SELECT count(*) FROM toolkit_outbox_dead_letters"
EVENT_DEADLINE_SECONDS = 10.0
# The versions a first submit names: `create` commits version 1 (`ck_bss_orders__order_version__1`);
# the submit appends the next candidate (`high_water + 1`) and keeps the draft revision (only a
# draft edit increments it, `contributions::apply`).
CREATED_VERSION = 1
SUBMITTED_VERSION = 2
# An `If-Match` the draft is not at (it stays at version 1 until submitted).
STALE_VERSION = CREATED_VERSION + 1
# The audit chain of a draft with one line: create (1), the line add (2), the submit (3).
SUBMIT_SEQUENCE = 3


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


def _draft_with_line(env: OrdersEnv, buyer: Buyer, catalog: Catalog) -> tuple[str, str]:
    """A draft of ``buyer`` with one line on the seeded plan: version 1, draft revision 1."""
    order_id = create_draft(env, buyer)["order"]["order_id"]
    r = buyer.client.post(
        f"{API}/orders/{order_id}/lines",
        json=line_from_catalog(catalog, 0),
        headers=write_headers(CREATED_VERSION),
    )
    assert r.status_code == 200, f"{r.status_code} {r.text}"
    assert (r.json()["version"], r.json()["draft_revision"]) == (CREATED_VERSION, 1), r.json()
    return order_id, r.json()["line_id"]


def _release_claims(env: OrdersEnv, orders: str) -> None:
    """Release the open overlap claims of ``orders`` (a SQL set of order ids): ``released_at``
    is the one column the claim guard lets change, once."""
    env.psql(
        "UPDATE bss_orders__inflight_overlap_claim SET released_at = now()"
        f" WHERE released_at IS NULL AND order_id IN ({orders})"
    )


@pytest.fixture(autouse=True)
def _released_claims(env: OrdersEnv, request):
    """Each test submits against a key no earlier commit of the run still holds: after the test,
    the open claims of the orders it created are released. Tests run one at a time, so those are
    the orders created at or after the database time read before the test (``create`` stamps
    ``created_at`` from the database clock). A ``holds_claims`` test releases its own."""
    if request.node.get_closest_marker("holds_claims"):
        yield
        return
    started = env.psql("SELECT now()::text")
    yield
    _release_claims(
        env,
        f"SELECT order_id FROM bss_orders__order WHERE created_at >= '{started}'::timestamptz",
    )


def _await_acknowledged(env: OrdersEnv, target: int) -> None:
    """Poll until ``target`` messages were acknowledged and nothing is queued, then assert the
    count is exactly ``target`` (no other message was produced)."""
    deadline = time.monotonic() + EVENT_DEADLINE_SECONDS
    while True:
        processed = env.count(PROCESSED)
        if processed >= target and env.count(UNACKNOWLEDGED) == 0 and env.count(INCOMING) == 0:
            break
        assert time.monotonic() < deadline, (
            f"{processed} of {target} messages acknowledged after {EVENT_DEADLINE_SECONDS}s"
        )
        time.sleep(0.1)
    assert processed == target, (processed, target)


def _submit_per_minute() -> int:
    """The suite's submit budget: the ``development_delivery`` overlay of ``config.yaml``."""
    overlay = yaml.safe_load(CONFIG.read_text())["development_delivery"]
    return int(overlay["bss-orders"]["config"]["gate"]["submit_per_caller_per_minute"])


def _counts(env: OrdersEnv, order_id: str) -> dict[str, int]:
    """Every row a submit of ``order_id`` may write, by table."""
    where = f"WHERE order_id = '{order_id}'"
    return {
        "versions": env.count(f"SELECT count(*) FROM bss_orders__order_version {where}"),
        "attempts": env.count(f"SELECT count(*) FROM bss_orders__commercial_attempt {where}"),
        "lines": env.count(f"SELECT count(*) FROM bss_orders__order_line {where}"),
        "totals": env.count(f"SELECT count(*) FROM bss_orders__resolved_total {where}"),
        "gate": env.count(f"SELECT count(*) FROM bss_orders__gate_outcome {where}"),
        "claims": env.count(f"SELECT count(*) FROM bss_orders__inflight_overlap_claim {where}"),
        "audits": env.count(
            "SELECT count(*) FROM bss_orders__transition_audit"
            f" WHERE order_id = '{order_id}' OR requested_order_ref = '{order_id}'"
        ),
        "keys": env.count(
            f"SELECT count(*) FROM bss_orders__idempotency {where} AND operation = 'submit'"
        ),
    }


def _refused_audits(env: OrdersEnv, order_id: str, reason: str) -> int:
    """Refused submit audit rows of ``order_id`` with ``reason``: resolved rows name the order,
    unresolved ones carry it as ``requested_order_ref`` (``ck_…transition_audit__7``/``__8``)."""
    return env.count(
        "SELECT count(*) FROM bss_orders__transition_audit"
        f" WHERE (order_id = '{order_id}' OR requested_order_ref = '{order_id}')"
        f" AND trigger = 'submit' AND outcome = 'refused' AND reason = '{reason}'"
        " AND sequence IS NULL"
    )


def _gate_projection(env: OrdersEnv, where: str) -> str:
    """One run's stored outcomes as comparable text, ordered: every identity and verdict column
    except the run's own ids (a Preview reserves fresh line ids; producer results carry run
    detail). ``line_id`` contributes only whether the outcome is line-scoped."""
    return env.psql(
        "SELECT string_agg(row, ',' ORDER BY row) FROM ("
        " SELECT predicate || '|' || (line_id IS NULL) || '|' || COALESCE(item_id::text, '-')"
        " || '|' || has_catalog_selection || '|' || COALESCE(catalog_scope_key, '-')"
        " || '|' || verdict || '|' || COALESCE(reason, '-') || '|' || applicability"
        " || '|' || mapping_version AS row"
        f" FROM bss_orders__gate_outcome WHERE {where}) AS projected"
    )


def _projection_diff(previewed: str, submitted: str) -> str:
    """Per predicate, the projected rows only the Preview run or only the submit run stored
    (``_gate_projection`` rows are ``predicate|…``, comma-joined)."""

    def by_predicate(projection: str) -> dict[str, Counter]:
        rows: dict[str, Counter] = {}
        for row in filter(None, (projection or "").split(",")):
            rows.setdefault(row.split("|", 1)[0], Counter())[row] += 1
        return rows

    preview, submitted_rows = by_predicate(previewed), by_predicate(submitted)
    lines = []
    for predicate in sorted(preview.keys() | submitted_rows.keys()):
        only_preview = preview.get(predicate, Counter()) - submitted_rows.get(predicate, Counter())
        only_submit = submitted_rows.get(predicate, Counter()) - preview.get(predicate, Counter())
        if only_preview or only_submit:
            lines.append(
                f"{predicate}: only in Preview {sorted(only_preview.elements())},"
                f" only in submit {sorted(only_submit.elements())}"
            )
    return "\n".join(lines)


def _sku_sellable(env: OrdersEnv, sku_id: str, sellable: bool) -> None:
    """Change the seeded SKU's ``sellable`` through Products as the catalog author; quorum 0
    (``seed_pricing``) applies the change at once, and the card reads it back."""
    author = env.client("catalog-author")
    r = author.post(f"{PRODUCTS}/skus/{sku_id}/changes", json={"sellable": sellable})
    assert r.status_code == 200, f"{r.status_code} {r.text}"
    assert r.json()["applied"] is True, r.json()
    card = author.get(f"{PRODUCTS}/skus/{sku_id}")
    assert card.status_code == 200, card.text
    assert card.json()["sku"]["sellable"] is sellable, card.json()


def _restore_sellable(env: OrdersEnv, sku_id: str) -> BaseException | None:
    """Make ``sku_id`` sellable again; a failure is logged and returned, never raised here."""
    try:
        _sku_sellable(env, sku_id, True)
    except Exception as error:  # noqa: BLE001 — the caller decides whether it may mask
        LOG.error("restoring SKU %s to sellable failed: %r", sku_id, error)
        return error
    return None


@pytest.fixture
def unsellable_sku(env: OrdersEnv, catalog: Catalog):
    """The seeded SKU made unsellable through Products for one test, always restored after.

    The change runs inside the fixture's own guard, so a change that applied before its
    read-back failed is restored too. A restore failure never masks the error before it: after a
    failed change it is logged and the change's own error is raised; after the test it is logged
    and raised from teardown, which pytest reports apart from the test's own outcome.
    """
    try:
        _sku_sellable(env, catalog.sku_id, False)
    except BaseException:
        _restore_sellable(env, catalog.sku_id)
        raise
    yield catalog.sku_id
    error = _restore_sellable(env, catalog.sku_id)
    if error is not None:
        raise error


# ── The per-caller limiter (F03 §4.6, D-52) ────────────────────────────────


@pytest.mark.timeout(60, func_only=True)
def test_submit_is_throttled_per_caller(env: OrdersEnv, buyer: Buyer):
    """N submits per caller per minute (``submit_per_caller_per_minute`` of the suite's
    ``development_delivery`` overlay); request N+1 meets the gear's per-caller 429 with
    ``Retry-After``. First in this file, so no submit of this buyer precedes it in the session.

    Each submit names its own empty draft: the per-(caller, order) edge limiter (10/min) never
    sees a second request for one order, and an empty draft is refused ``no-lines`` by the engine
    without any port call, reservation or claim (``submit.rs`` step 5). The limiter admits before
    any read (step 1). Setup stays inside the gateway caller zone (burst 20, 3/s): the N+3 drafts
    are created one refill interval apart, then the submits (at most N+3 < 20) go back to back.
    """
    budget = _submit_per_minute()
    refill = SECONDS_PER_MINUTE / budget
    drafts = []
    for _ in range(budget + SPARE_DRAFTS):
        drafts.append(create_draft(env, buyer)["order"]["order_id"])
        time.sleep(GATEWAY_REFILL_SECONDS)

    admitted = []
    refused = None
    started = time.monotonic()
    for order_id in drafts:
        r = submit(buyer, order_id, CREATED_VERSION, 0)
        if r.status_code == 429:
            refused = (order_id, r)
            break
        _refused(r, "no-lines")
        admitted.append(order_id)
    elapsed = time.monotonic() - started
    assert refused is not None, f"{len(admitted)} submits admitted in {elapsed:.1f}s"
    refills = int(elapsed // refill)
    assert budget <= len(admitted) <= budget + refills, (len(admitted), budget, elapsed)
    if refills == 0:
        assert len(admitted) == budget

    refused_order, r = refused
    assert r.headers["content-type"].startswith(PROBLEM)
    retry_after = int(r.headers["retry-after"])
    assert 1 <= retry_after <= int(refill) + 1, dict(r.headers)
    problem = r.json()
    assert problem["type"] == RESOURCE_EXHAUSTED, problem
    assert problem["detail"] == "Orders per-caller request limit exceeded", problem
    violation = problem["context"]["violations"][0]
    assert (violation["subject"], violation["description"]) == (
        "throttling",
        "per-caller request limit exceeded",
    ), violation
    # Every admitted submit settled its `no-lines` refusal with one audit row; the throttled one
    # reached nothing.
    for order_id in admitted:
        assert _refused_audits(env, order_id, "no-lines") == 1, order_id
    assert _counts(env, refused_order) == {
        "versions": 1, "attempts": 0, "lines": 0, "totals": 0, "gate": 0, "claims": 0,
        "audits": 1, "keys": 0,
    }


# ── The committed submit ───────────────────────────────────────────────────


@pytest.mark.smoke
@pytest.mark.timeout(30, func_only=True)
def test_submit_commits_a_version_with_pin_totals_audit_and_one_event(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    order_id, line_id = _draft_with_line(env, buyer, catalog)
    processed = env.count(PROCESSED)
    dead = env.count(DEAD_LETTERS)

    r = submit(buyer, order_id, CREATED_VERSION, 1)
    assert r.status_code == 200, f"{r.status_code} {r.text}"
    assert r.headers["content-type"].startswith("application/json"), r.headers.get("content-type")
    assert r.json() == {
        "order_id": order_id,
        "state": "submitted",
        "version": SUBMITTED_VERSION,
        "draft_revision": 1,
        "assessment_id": r.json()["assessment_id"],
    }, r.json()
    assert r.headers["etag"] == f'"{SUBMITTED_VERSION}"'
    assert "idempotency-replayed" not in r.headers

    # The list summary reports the committed current version without a draft revision.
    listing = buyer.client.get(
        f"{API}/orders", params={"limit": 200, "$filter": "state eq 'submitted'"}
    )
    assert listing.status_code == 200, listing.text
    [summary] = [o for o in listing.json()["items"] if o["order"]["order_id"] == order_id]
    assert summary["current_version"] == SUBMITTED_VERSION, summary
    assert "draft_revision" not in summary, summary
    assert env.psql(
        "SELECT state || '|' || current_version || '|' || version_allocation_high_water"
        f" || '|' || draft_revision FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"submitted|{SUBMITTED_VERSION}|{SUBMITTED_VERSION}|1"

    # One new version, superseding the created one, by the buyer, in the fixture's market.
    assert env.psql(
        "SELECT string_agg(version || ':' || COALESCE(supersedes_version::text, '-') || ':' || reason,"
        f" ',' ORDER BY version) FROM bss_orders__order_version WHERE order_id = '{order_id}'"
    ) == f"{CREATED_VERSION}:-:create,{SUBMITTED_VERSION}:{CREATED_VERSION}:submit"
    assert env.psql(
        "SELECT actor || '|' || market_currency FROM bss_orders__order_version"
        f" WHERE order_id = '{order_id}' AND version = {SUBMITTED_VERSION}"
    ) == f"{buyer.subject_id}|{catalog.currency}"

    # One committed attempt for the candidate, frozen under the seller's commercial identity
    # (D-194), with the one line's accepted receipt.
    assert env.psql(
        "SELECT status || '|' || operation || '|' || candidate_version || '|'"
        " || previous_committed_version || '|' || prepared_draft_revision || '|'"
        " || commercial_subject_id || '|' || commercial_subject_tenant_id || '|'"
        " || (SELECT string_agg(k, ',') FROM jsonb_object_keys(receipt_results) AS k) || '|'"
        f" || (receipt_results -> '{line_id}' ? 'accepted')"
        f" FROM bss_orders__commercial_attempt WHERE order_id = '{order_id}'"
    ) == (
        f"committed|submit|{SUBMITTED_VERSION}|{CREATED_VERSION}|1|{SELLER_SERVICE_SUBJECT}"
        f"|{SELLER_TENANT}|{line_id}|true"
    )
    # The admitted line: the schema-2 pin naming this row, the acceptance and the claimed key
    # (`write_fixture` keys the seeded SKU's overlap scope as `e2e-scope-<sku>`).
    assert env.count(
        "SELECT count(*) FROM bss_orders__order_line l"
        " JOIN bss_orders__commercial_attempt a ON a.attempt_id = l.commercial_attempt_id"
        f" WHERE l.order_id = '{order_id}' AND l.version = {SUBMITTED_VERSION}"
        f" AND l.line_id = '{line_id}' AND l.pricing_acceptance_id IS NOT NULL"
        " AND l.order_pin->>'schema_version' = '2'"
        f" AND l.order_pin->'accepted_version_ref'->>'line_id' = '{line_id}'"
        f" AND l.overlap_scope_key IS NULL AND l.catalog_subscription_product_key IS NULL AND l.overlap_item_keys->0->>'overlap_scope_key' = 'e2e-scope-{catalog.sku_id}'"
    ) == 1
    assert env.count(
        "SELECT count(*) FROM bss_orders__inflight_overlap_claim"
        f" WHERE order_id = '{order_id}' AND version = {SUBMITTED_VERSION}"
        f" AND overlap_scope_key = 'e2e-scope-{catalog.sku_id}' AND released_at IS NULL"
    ) == 1

    # The resolved totals of the version: the line row and the order roll-up, which alone
    # carries the TCV (`totals::total_rows`; `ck_…resolved_total__6`).
    assert env.psql(
        "SELECT string_agg(scope || ':' || charge_kind || ':' || amount_status || ':' || currency"
        " || ':' || (tcv_exact IS NOT NULL), ',' ORDER BY scope)"
        f" FROM bss_orders__resolved_total WHERE order_id = '{order_id}'"
        f" AND version = {SUBMITTED_VERSION}"
    ) == (
        f"line:recurring:committed:{catalog.currency}:false,"
        f"order:recurring:committed:{catalog.currency}:true"
    )

    # One committed audit row: the submit by the buyer, draft → submitted, chained third.
    assert env.psql(
        "SELECT string_agg(outcome || '|' || actor || '|' || actor_class || '|' || from_state"
        " || '|' || to_state || '|' || version || '|' || sequence, ',')"
        f" FROM bss_orders__transition_audit WHERE order_id = '{order_id}' AND trigger = 'submit'"
    ) == f"committed|{buyer.subject_id}|user|draft|submitted|{SUBMITTED_VERSION}|{SUBMIT_SEQUENCE}"

    assert env.psql(
        "SELECT DISTINCT run_id::text FROM bss_orders__gate_outcome"
        f" WHERE order_id = '{order_id}' AND version = {SUBMITTED_VERSION}"
    ) == r.json()["assessment_id"]
    # The run's diagnostics name the committed version; every required outcome passed.
    assert env.psql(
        "SELECT count(DISTINCT run_id) || '|' || bool_and(verdict = 'passed')"
        f" FROM bss_orders__gate_outcome WHERE order_id = '{order_id}'"
        f" AND version = {SUBMITTED_VERSION} AND applicability = 'required'"
    ) == "1|true"
    assert env.psql(
        "SELECT status || '|' || outcome FROM bss_orders__idempotency"
        f" WHERE order_id = '{order_id}' AND operation = 'submit'"
    ) == "settled|success"

    # Exactly one message (`OrderSubmitted`), delivered and acknowledged within the deadline.
    _await_acknowledged(env, processed + 1)
    assert env.count(DEAD_LETTERS) == dead


# ── Replay of a committed key ──────────────────────────────────────────────


@pytest.mark.timeout(30, func_only=True)
def test_submit_replays_the_same_key_without_new_rows_or_events(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """The same key and body after a commit is the stored outcome, byte-identical, marked
    ``Idempotency-Replayed: true``, and writes nothing. The recovery probe runs before the
    draft-only read (``submit.rs`` step 3 before step 4; D-188 step 1, E2), so the committed
    order, no longer a draft, is replayed rather than answered unavailable.
    """
    order_id, _ = _draft_with_line(env, buyer, catalog)
    key = new_key()
    processed = env.count(PROCESSED)
    first = submit(buyer, order_id, CREATED_VERSION, 1, key)
    assert first.status_code == 200, f"{first.status_code} {first.text}"
    _await_acknowledged(env, processed + 1)
    before = _counts(env, order_id)
    assert (before["versions"], before["attempts"], before["keys"]) == (2, 1, 1), before

    replay = submit(buyer, order_id, CREATED_VERSION, 1, key)
    # Nothing new: no row in any table a submit writes, and no message at any outbox stage (the
    # enqueue commits with the transition, so a new message would already be queued or counted).
    assert _counts(env, order_id) == before
    assert env.count(PROCESSED) == processed + 1
    assert env.count(INCOMING) == 0 and env.count(UNACKNOWLEDGED) == 0
    assert env.psql(
        "SELECT status || '|' || outcome FROM bss_orders__idempotency"
        f" WHERE order_id = '{order_id}' AND operation = 'submit'"
    ) == "settled|success"

    assert replay.status_code == 200, f"{replay.status_code} {replay.text}"
    assert replay.content == first.content
    assert replay.headers["etag"] == first.headers["etag"]
    assert replay.headers["idempotency-replayed"] == "true"


# ── Same key, another payload ──────────────────────────────────────────────


@pytest.mark.timeout(30, func_only=True)
def test_submit_refuses_a_different_payload_under_the_same_key(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """The draft revision is a fingerprint input (E4), so the same key sent with another
    ``expected_draft_revision`` is a mismatch the recovery probe finds before any draft read or
    assessment (``submit.rs`` step 3): the engine answers ``idempotency-mismatch`` under the lock
    with resolved evidence, and the registry keeps the first settlement.

    Under a committed key (the stronger case: the order is no longer a draft, so a probe after
    the draft-only read would answer 503), and under a key settled by a refusal (a stale
    ``expected_draft_revision`` settles ``version-conflict`` and leaves the draft as it was)."""
    # A key settled by the commit, re-sent with an older revision.
    order_id, _ = _draft_with_line(env, buyer, catalog)
    key = new_key()
    first = submit(buyer, order_id, CREATED_VERSION, 1, key)
    assert first.status_code == 200, f"{first.status_code} {first.text}"
    before = _counts(env, order_id)

    body = _refused(submit(buyer, order_id, CREATED_VERSION, 0, key), "idempotency-mismatch")
    assert "assessment" not in body.get("context", {}).get("data", {}), body
    assert _refused_audits(env, order_id, "idempotency-mismatch") == 1
    assert env.psql(
        "SELECT status || '|' || outcome FROM bss_orders__idempotency"
        f" WHERE order_id = '{order_id}' AND operation = 'submit'"
    ) == "settled|success"
    after = _counts(env, order_id)
    assert after == {**before, "audits": before["audits"] + 1}, (before, after)
    assert (after["versions"], after["attempts"]) == (2, 1), after
    assert env.psql(
        f"SELECT state || '|' || current_version FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"submitted|{SUBMITTED_VERSION}"

    # A key settled by a refusal, re-sent with the current revision.
    order_id, _ = _draft_with_line(env, buyer, catalog)
    key = new_key()
    _refused(submit(buyer, order_id, CREATED_VERSION, 0, key), "version-conflict")

    body = _refused(submit(buyer, order_id, CREATED_VERSION, 1, key), "idempotency-mismatch")
    assert "assessment" not in body.get("context", {}).get("data", {}), body
    assert _refused_audits(env, order_id, "version-conflict") == 1
    assert _refused_audits(env, order_id, "idempotency-mismatch") == 1
    assert env.psql(
        "SELECT status || '|' || outcome || '|' || outcome_reason FROM bss_orders__idempotency"
        f" WHERE order_id = '{order_id}' AND operation = 'submit'"
    ) == "settled|refused|version-conflict"
    counts = _counts(env, order_id)
    assert (counts["versions"], counts["attempts"], counts["gate"]) == (1, 0, 0), counts
    assert env.psql(
        f"SELECT state || '|' || current_version FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"draft|{CREATED_VERSION}"


# ── Stale draft revision and stale version ─────────────────────────────────


def test_submit_refuses_a_stale_draft_revision_and_a_stale_version(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """A stale ``expected_draft_revision`` skips the gate (``submit.rs`` step 5) and the engine
    settles ``version-conflict`` with the current version and revision (``transition::admit``),
    replayed as stored under the same key. A stale ``If-Match`` with the current revision runs
    the gate, and the reservation's admissibility recheck (E1) refuses ``precondition-failed``
    unsettled: the whole reservation rolls back, so no attempt, candidate or claim survives."""
    order_id, _ = _draft_with_line(env, buyer, catalog)

    key = new_key()
    stale = submit(buyer, order_id, CREATED_VERSION, 0, key)
    body = _refused(stale, "version-conflict")
    assert body["context"]["data"] == {"current_version": CREATED_VERSION, "draft_revision": 1}, body
    assert "idempotency-replayed" not in stale.headers
    replay = submit(buyer, order_id, CREATED_VERSION, 0, key)
    assert replay.status_code == stale.status_code and replay.content == stale.content
    assert replay.headers["idempotency-replayed"] == "true"
    assert _refused_audits(env, order_id, "version-conflict") == 1
    assert _counts(env, order_id) == {
        "versions": 1, "attempts": 0, "lines": 0, "totals": 0, "gate": 0, "claims": 0,
        "audits": 3, "keys": 1,
    }

    _refused(submit(buyer, order_id, STALE_VERSION, 1), "precondition-failed")
    assert _refused_audits(env, order_id, "precondition-failed") == 1
    assert _counts(env, order_id) == {
        "versions": 1, "attempts": 0, "lines": 0, "totals": 0, "gate": 0, "claims": 0,
        "audits": 4, "keys": 1,
    }
    assert env.psql(
        "SELECT state || '|' || current_version || '|' || version_allocation_high_water"
        f" FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"draft|{CREATED_VERSION}|{CREATED_VERSION}"


# ── Preview, then submit the same basket ───────────────────────────────────


@pytest.mark.timeout(30, func_only=True)
def test_submit_after_preview_yields_the_same_assessment(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """Preview and submit run the same orchestrated gate (``Operation::Preview`` vs ``Submit``)
    and store the run through the same projection (``Vector::stored_projection``); on a passing
    basket every outcome is ``required`` in both (the Preview-only ``tcv-only`` and
    ``preview-forecast`` rows arise only on withheld terms or a missing policy). The Preview
    run's rows (no order) and the submit run's rows (the committed version) therefore project to
    the same set; only the line ids differ (a Preview reserves its own)."""
    r = buyer.client.post(PREVIEW, json=preview_body(catalog))
    assert r.status_code == 200, f"{r.status_code} {r.text}"
    preview = r.json()
    assert preview["assessment"]["failures"] == [], preview["assessment"]

    order_id, _ = _draft_with_line(env, buyer, catalog)
    committed = submit(buyer, order_id, CREATED_VERSION, 1)
    assert committed.status_code == 200, f"{committed.status_code} {committed.text}"
    assert committed.json()["version"] == SUBMITTED_VERSION

    previewed = _gate_projection(
        env, f"run_id = '{preview['assessment_id']}' AND order_id IS NULL"
    )
    submitted = _gate_projection(
        env, f"order_id = '{order_id}' AND version = {SUBMITTED_VERSION}"
    )
    assert previewed, "the Preview run stored its outcomes"
    assert submitted == previewed, (
        "the submit run's outcomes differ from the Preview run's, per predicate:\n"
        + _projection_diff(previewed, submitted)
    )
    # The same set as the Preview reported it on the wire.
    assert env.count(
        f"SELECT count(*) FROM bss_orders__gate_outcome WHERE order_id = '{order_id}'"
    ) == len(preview["assessment"]["outcomes"])


# ── A new key on a committed order ─────────────────────────────────────────


@pytest.mark.timeout(30, func_only=True)
def test_a_new_key_on_a_committed_order_is_not_admissible(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """A second submit of an already committed order under a new key: the recovery probe misses
    (``submit.rs`` step 3), the authorized read finds no draft (``ReadService::get_draft``), and
    the engine settles the state table's ``not-admissible`` with ``{state, trigger}``
    (``transition::admit`` looks the row up before ``If-Match``), never the draft read's 503. No
    gate runs and nothing commercial is written; the same new key replays the stored refusal."""
    order_id, _ = _draft_with_line(env, buyer, catalog)
    committed = submit(buyer, order_id, CREATED_VERSION, 1)
    assert committed.status_code == 200, f"{committed.status_code} {committed.text}"
    before = _counts(env, order_id)

    key = new_key()
    first = submit(buyer, order_id, SUBMITTED_VERSION, 1, key)
    body = _refused(first, "not-admissible")
    assert body["context"]["data"] == {"state": "submitted", "trigger": "submit"}, body
    assert "idempotency-replayed" not in first.headers

    replay = submit(buyer, order_id, SUBMITTED_VERSION, 1, key)
    assert replay.status_code == first.status_code, f"{replay.status_code} {replay.text}"
    assert replay.content == first.content
    assert replay.headers["idempotency-replayed"] == "true"

    # One resolved refused audit row and one more settled key; no gate row, version or attempt.
    assert _refused_audits(env, order_id, "not-admissible") == 1
    after = _counts(env, order_id)
    assert after == {**before, "audits": before["audits"] + 1, "keys": before["keys"] + 1}, (
        before,
        after,
    )
    assert env.psql(
        "SELECT string_agg(status || '|' || outcome || '|' || COALESCE(outcome_reason, '-'),"
        " ',' ORDER BY outcome DESC) FROM bss_orders__idempotency"
        f" WHERE order_id = '{order_id}' AND operation = 'submit'"
    ) == "settled|success|-,settled|refused|not-admissible"
    assert env.psql(
        f"SELECT state || '|' || current_version FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"submitted|{SUBMITTED_VERSION}"


# ── An open overlap claim (D-180) ──────────────────────────────────────────


@pytest.mark.holds_claims
@pytest.mark.timeout(45, func_only=True)
def test_an_open_overlap_claim_blocks_a_second_commit_on_the_same_scope(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """The first submit holds the tuple. The second fails the gate's friendly predicate-9
    pre-check before Pricing receipts or candidate reservation. Its report and stored rows
    agree, while the holder's claim remains live. The transactional claim still closes races.
    """
    scope = f"e2e-scope-{catalog.sku_id}"
    open_on_scope = (
        "SELECT count(*) FROM bss_orders__inflight_overlap_claim"
        f" WHERE overlap_scope_key = '{scope}' AND released_at IS NULL"
    )
    assert env.count(open_on_scope) == 0, "an earlier test left a claim open on the key"
    holder, _ = _draft_with_line(env, buyer, catalog)
    contender, _ = _draft_with_line(env, buyer, catalog)
    try:
        r = submit(buyer, holder, CREATED_VERSION, 1)
        assert r.status_code == 200, f"{r.status_code} {r.text}"
        assert env.count(open_on_scope) == 1

        body = _refused(
            submit(buyer, contender, CREATED_VERSION, 1), "order-in-flight-for-key"
        )
        data = body["context"]["data"]
        report = data["assessment"]
        rows = [o for o in report["outcomes"] if o["predicate"] == "orders.in-flight-per-key"]
        assert rows and all((o["verdict"], o.get("reason")) == ("failed", "order-in-flight-for-key") for o in rows), rows
        assert env.count(
            "SELECT count(*) FROM bss_orders__gate_outcome"
            f" WHERE order_id = '{contender}' AND predicate = 'orders.in-flight-per-key'"
            " AND verdict = 'failed' AND reason = 'order-in-flight-for-key'"
        ) == len(rows)

        assert _refused_audits(env, contender, "order-in-flight-for-key") == 1
        counts = _counts(env, contender)
        assert counts["gate"] > 0, counts
        assert {k: v for k, v in counts.items() if k not in ("gate", "audits")} == {
            "versions": 1, "attempts": 0, "lines": 0, "totals": 0, "claims": 0, "keys": 1,
        }, counts
        assert env.count(
            f"SELECT count(*) FROM bss_orders__gate_outcome WHERE order_id = '{contender}'"
            " AND version IS NOT NULL"
        ) == 0
        assert env.psql(
            "SELECT state || '|' || current_version || '|' || version_allocation_high_water"
            f" FROM bss_orders__order WHERE order_id = '{contender}'"
        ) == f"draft|{CREATED_VERSION}|{CREATED_VERSION}"
        assert env.psql(
            "SELECT status || '|' || outcome || '|' || outcome_reason FROM bss_orders__idempotency"
            f" WHERE order_id = '{contender}' AND operation = 'submit'"
        ) == "settled|refused|order-in-flight-for-key"
        # The holder's claim is untouched by the refusal.
        assert env.count(
            "SELECT count(*) FROM bss_orders__inflight_overlap_claim"
            f" WHERE order_id = '{holder}' AND released_at IS NULL"
        ) == 1
    finally:
        _release_claims(env, f"'{holder}', '{contender}'")


# ── Callers who may not submit ─────────────────────────────────────────────


def test_seller_cannot_submit(env: OrdersEnv, buyer: Buyer, seller: Buyer, catalog: Catalog):
    """A targeted operation (``authorize_target``, from the recovery probe before any draft read
    or port call, ``submit.rs`` step 3): the seller operator holds no ``submit`` grant, and its
    follow-up read admits the target, so the denial is the 403 for the actor
    (``targeted_denial``), recorded as one unresolved refused audit row. A foreign tenant's
    caller fails the same follow-up read, so the denial is the non-disclosing 404
    ``order-not-found``, recorded the same way (as capture's writes record theirs). Nothing
    commercial is written."""
    order_id, _ = _draft_with_line(env, buyer, catalog)

    _refused(submit(seller, order_id, CREATED_VERSION, 1), "operation-not-permitted-for-actor")
    assert env.count(
        "SELECT count(*) FROM bss_orders__transition_audit"
        f" WHERE requested_order_ref = '{order_id}' AND order_id IS NULL AND trigger = 'submit'"
        " AND outcome = 'refused' AND reason = 'operation-not-permitted-for-actor'"
        f" AND actor = '{SELLER_SUBJECT}'"
    ) == 1

    foreign = env.client("foreign").post(
        f"{API}/orders/{order_id}/submit",
        json={"expected_draft_revision": 1},
        headers=write_headers(CREATED_VERSION),
    )
    _refused(foreign, "order-not-found")
    assert env.count(
        "SELECT count(*) FROM bss_orders__transition_audit"
        f" WHERE requested_order_ref = '{order_id}' AND order_id IS NULL AND trigger = 'submit'"
        " AND outcome = 'refused' AND reason = 'order-not-found'"
    ) == 1
    assert env.count(
        f"SELECT count(*) FROM bss_orders__order WHERE resource_tenant_id = '{FOREIGN_TENANT}'"
    ) == 0

    assert _counts(env, order_id) == {
        "versions": 1, "attempts": 0, "lines": 0, "totals": 0, "gate": 0, "claims": 0,
        "audits": 4, "keys": 0,
    }
    assert env.psql(
        f"SELECT state || '|' || current_version FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"draft|{CREATED_VERSION}"


@pytest.mark.timeout(30, func_only=True)
def test_submit_refuses_unsupported_authored_term_before_reservation(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """An authored calendar/day remainder stays a draft diagnostic and cannot fall back to
    seller policy. The composite gate refuses before allocating any commercial attempt."""
    order_id = create_draft(env, buyer)["order"]["order_id"]
    line = line_from_catalog(catalog, 0)
    line["term_duration"] = {"kind": "calendar", "years": 1, "months": 0, "days": 10, "microseconds": 0}
    authored = buyer.client.post(
        f"{API}/orders/{order_id}/lines", json=line, headers=write_headers(CREATED_VERSION)
    )
    assert authored.status_code == 200, f"{authored.status_code} {authored.text}"
    body = _refused(submit(buyer, order_id, CREATED_VERSION, 1), "billing-terms-unsupported")
    rows = body["context"]["data"]["assessment"]["outcomes"]
    assert any(
        row["predicate"] == "orders.pin-composition"
        and row["verdict"] == "failed"
        and row.get("reason") == "billing-terms-unsupported"
        for row in rows
    ), rows
    assert env.count(
        "SELECT count(*) FROM bss_orders__commercial_attempt"
        f" WHERE order_id = '{order_id}'"
    ) == 0
    assert env.psql(
        f"SELECT state || '|' || current_version || '|' || version_allocation_high_water"
        f" FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"draft|{CREATED_VERSION}|{CREATED_VERSION}"


# ── An unsellable line (catalog.sku-sellable) ──────────────────────────────


@pytest.mark.timeout(30, func_only=True)
def test_submit_refuses_an_unsellable_line_with_the_gate_report(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog, unsellable_sku: str
):
    """The seeded SKU is made unsellable through Products for this test only and restored after
    (the ``unsellable_sku`` fixture).

    Retiring is not available: Products refuses to retire a SKU a live price entry references
    (409 ``SKU_REFERENCED``, the pricebook suite's ``…blocks_retirement``). A second SKU cannot be
    used either: the stub fixture states terms, evaluation and the overlap key for the seeded
    revision and SKU only, and an unstated port is ``unevaluable``, which outranks a non-local
    failure as the primary (``Vector::primary``). A ``sellable: false`` change keeps the SKU
    published and its price resolvable (Pricing's ``resolve`` does not read ``sellable``), and the
    gate reads the SKU live through Products' ``get_sku`` (no cache): ``catalog.sku-sellable``
    fails ``sku-not-sellable`` under ``catalog-predicate-failed`` (``pricing_map::sellable``).
    Last in this file, so no later submit sees the changed SKU even if the restore fails (the
    fixture logs that failure and reports it without masking the test's own).

    The refusal settles through the engine's composite gate (step 7): the run's rows name the
    order and no version, one resolved refused audit row, and the settled Problem carries the
    complete gate report in ``context.data.assessment`` (D-233). No attempt, version or claim.
    """
    reason = "catalog-predicate-failed"
    order_id, line_id = _draft_with_line(env, buyer, catalog)
    body = _refused(submit(buyer, order_id, CREATED_VERSION, 1), reason)

    report = body["context"]["data"]["assessment"]
    sellable = [
        o for o in report["outcomes"]
        if o["predicate"] == "catalog.sku-sellable" and o.get("line_id") == line_id
    ]
    assert sellable and all((o["verdict"], o.get("reason")) == ("failed", reason) for o in sellable), sellable
    assert any(
        f["predicate"] == "catalog.sku-sellable" and f["reason"] == reason and f.get("line_id") == line_id
        for f in report["failures"]
    ), report["failures"]

    run = report["assessment_id"]
    assert env.count(
        "SELECT count(*) FROM bss_orders__gate_outcome"
        f" WHERE run_id = '{run}' AND order_id = '{order_id}' AND version IS NULL"
    ) == len(report["outcomes"])
    assert env.count(
        "SELECT count(*) FROM bss_orders__gate_outcome"
        f" WHERE run_id = '{run}' AND predicate = 'catalog.sku-sellable' AND line_id = '{line_id}'"
        f" AND verdict = 'failed' AND reason = '{reason}'"
    ) == len(sellable)
    assert env.count(
        "SELECT count(*) FROM bss_orders__transition_audit"
        f" WHERE order_id = '{order_id}' AND trigger = 'submit' AND outcome = 'refused'"
        f" AND reason = '{reason}' AND sequence IS NULL AND actor = '{buyer.subject_id}'"
    ) == 1
    counts = _counts(env, order_id)
    assert counts == {
        "versions": 1, "attempts": 0, "lines": 0, "totals": 0, "gate": len(report["outcomes"]),
        "claims": 0, "audits": 3, "keys": 1,
    }, counts
    assert env.psql(
        "SELECT status || '|' || outcome || '|' || outcome_reason FROM bss_orders__idempotency"
        f" WHERE order_id = '{order_id}' AND operation = 'submit'"
    ) == f"settled|refused|{reason}"
    assert env.psql(
        f"SELECT state || '|' || current_version FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == f"draft|{CREATED_VERSION}"


@pytest.mark.timeout(180)
@pytest.mark.parametrize("mixed", [False, True], ids=["one-time-only", "mixed-recurring"])
def test_non_recurring_receipts_commit_without_overlap_claims(env: OrdersEnv, buyer: Buyer, catalog: Catalog, mixed: bool):
    """Q-C(a): a one-time line has a receipt/pin and NULL key; only recurring lines claim.

    Seed this test's own one-time plan and temporarily extend the development owner fixture.
    Restore the fixture and restart in finally so every existing scenario keeps its catalog.
    Usage-only coverage lives in gate_assess/gate_submit_shape and the PostgreSQL submit tier:
    a published usage SKU additionally needs the Usage Types owner (#5376 fixture boundary).
    """
    keyless = seed_pricing(env, sku_kind="one_time")
    assert env.gate_fixture is not None
    original = env.gate_fixture.read_bytes()
    fixture = json.loads(original)
    fixture["terms"][keyless.plan_revision_id] = fixture["terms"][catalog.plan_revision_id]
    evaluation = json.loads(json.dumps(fixture["evaluation"][catalog.plan_revision_id]))
    evaluation["declaration"].update(charge_kind="one_time", amount_basis="finite_term", period_evidence={}, recurring_by_cycle={})
    fixture["evaluation"][keyless.plan_revision_id] = evaluation

    # Producer-authored component roll-ups and TCV: the aggregate alone cannot declare
    # which charge kinds are committed, and Orders never derives a carrier or adds money.
    def supplied_totals(numerator: str, denominator: str, display: str):
        return {
            **{
                name: {
                    "exact": {"numerator": numerator, "denominator": denominator,
                              "currency": catalog.currency, "currency_scale": 2},
                    "display": display,
                }
                for name in ("gross", "net")
            },
            "discount": {"status": "unavailable"},
        }

    recurring = fixture["evaluation"][catalog.plan_revision_id]
    order = fixture["evaluation_totals"]
    order["order_declaration"] = None
    order["order_committed_components"] = [
        {"totals": evaluation["totals"], "declaration": evaluation["declaration"]},
    ]
    if mixed:
        order["order_committed_components"].append(
            {"totals": recurring["totals"], "declaration": recurring["declaration"]}
        )
        order["order_totals"] = supplied_totals("21", "1", "21.00")
    else:
        order["order_tcv_carrier"] = {
            "totals": supplied_totals("0", "1", "0.00"),
            "declaration": {
                **recurring["declaration"],
                "amount_basis": "finite_term",
                "period_evidence": {"source": "e2e-owner-fixture", "purpose": "tcv-carrier"},
                "recurring_by_cycle": {},
            },
        }
    fixture["tcv"] = {
        "exact": {"numerator": "273" if mixed else "21", "denominator": "2",
                  "currency": catalog.currency, "currency_scale": 2},
        "display": "136.50" if mixed else "10.50",
        "posting_estimate": "136.50" if mixed else "10.50",
        "basis": "mixed" if mixed else "finite_term",
        "evidence": {"source": "e2e-owner-fixture", "policy_version": "e2e-charges-1"},
    }
    try:
        env.gate_fixture.write_text(json.dumps(fixture))
        env.server.restart()
        order_id, line_id = _draft_with_line(env, buyer, keyless)
        if mixed:
            r = buyer.client.post(f"{API}/orders/{order_id}/lines", json=line_from_catalog(catalog, 1), headers=write_headers(CREATED_VERSION))
            assert r.status_code == 200, r.text
        r = submit(buyer, order_id, CREATED_VERSION, 2 if mixed else 1)
        assert r.status_code == 200, r.text
        assert r.json()["state"] == "submitted", r.json()
        assert env.count(
            "SELECT count(*) FROM bss_orders__order_line"
            f" WHERE order_id='{order_id}' AND version={SUBMITTED_VERSION} AND line_id='{line_id}'"
            " AND pricing_acceptance_id IS NOT NULL AND order_pin->>'schema_version'='2'"
            " AND overlap_scope_key IS NULL AND catalog_subscription_product_key IS NULL AND overlap_item_keys = '[]'::jsonb"
        ) == 1
        assert env.count(
            "SELECT count(*) FROM bss_orders__inflight_overlap_claim"
            f" WHERE order_id='{order_id}' AND released_at IS NULL"
        ) == int(mixed)
        assert env.count(
            "SELECT count(*) FROM bss_orders__gate_outcome"
            f" WHERE order_id='{order_id}' AND line_id='{line_id}'"
            " AND predicate IN ('orders.overlap-uniqueness', 'orders.in-flight-per-key') AND verdict='passed'"
        ) == 2
    finally:
        env.gate_fixture.write_bytes(original)
        env.server.restart()


@pytest.mark.timeout(180)
@pytest.mark.parametrize(
    ("malformed", "reason"),
    [("evaluation", "evaluation-unavailable"), ("profile", "catalog-predicates-unavailable")],
)
def test_malformed_gate_envelope_refuses_before_commercial_reservation(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog, malformed: str, reason: str
):
    """A valid JSON fixture with an invalid producer envelope fails closed over HTTP."""
    assert env.gate_fixture is not None
    original = env.gate_fixture.read_bytes()
    fixture = json.loads(original)
    if malformed == "evaluation":
        # The line amount currency differs from the frozen market/order envelope.
        fixture["evaluation"][catalog.plan_revision_id]["totals"]["gross"]["exact"]["currency"] = "USD" if catalog.currency != "USD" else "EUR"
    else:
        fixture["assessment_profile"] = "foreign-assessment/1"
    baseline = None
    if malformed == "evaluation":
        preview = buyer.client.post(PREVIEW, json=preview_body(catalog))
        assert preview.status_code == 200, preview.text
        baseline = preview.json()
        assert baseline["assessment"]["failures"] == []
    try:
        env.gate_fixture.write_text(json.dumps(fixture))
        env.server.restart()
        order_id, _ = _draft_with_line(env, buyer, catalog)
        result = submit(buyer, order_id, CREATED_VERSION, 1)
        body = _refused(result, reason)
        report = body["context"]["data"]["assessment"]
        if malformed == "evaluation":
            # Evaluation is an engine input, not a predicate: preserve the preflight vector.
            assert report["failures"] == [], report
            assert baseline is not None
            def outcomes_without_line_identity(assessment):
                return [
                    {**o, "line_id": o.get("line_id") is not None}
                    for o in assessment["outcomes"]
                ]
            assert outcomes_without_line_identity(report) == outcomes_without_line_identity(baseline["assessment"])
            assert _gate_projection(env, f"run_id = '{report['assessment_id']}'") == _gate_projection(
                env, f"run_id = '{baseline['assessment_id']}'"
            )
        else:
            assert any(f["reason"] == reason for f in report["failures"]), report
        assert env.count(f"SELECT count(*) FROM bss_orders__commercial_attempt WHERE order_id='{order_id}'") == 0
        assert env.count(f"SELECT count(*) FROM bss_orders__order_line WHERE order_id='{order_id}'") == 0
        assert env.count(f"SELECT count(*) FROM bss_orders__resolved_total WHERE order_id='{order_id}'") == 0
        assert env.count(f"SELECT count(*) FROM bss_orders__inflight_overlap_claim WHERE order_id='{order_id}'") == 0
        assert env.count(f"SELECT count(*) FROM bss_orders__order_version WHERE order_id='{order_id}'") == 1
        run = report["assessment_id"]
        assert env.count(
            "SELECT count(*) FROM bss_orders__gate_outcome"
            f" WHERE run_id='{run}' AND order_id='{order_id}' AND version IS NULL"
        ) == len(report["outcomes"])
        reasons = env.count(f"SELECT count(*) FROM bss_orders__gate_outcome WHERE order_id='{order_id}' AND reason='{reason}'")
        assert reasons == 0 if malformed == "evaluation" else reasons >= 1
        assert _refused_audits(env, order_id, reason) == 1
        assert env.psql(
            "SELECT status || '|' || outcome || '|' || outcome_reason FROM bss_orders__idempotency"
            f" WHERE order_id='{order_id}' AND operation='submit'"
        ) == f"settled|refused|{reason}"
        assert env.psql(
            f"SELECT state || '|' || current_version FROM bss_orders__order WHERE order_id='{order_id}'"
        ) == f"draft|{CREATED_VERSION}"
    finally:
        env.gate_fixture.write_bytes(original)
        env.server.restart()


@pytest.mark.timeout(180)
@pytest.mark.parametrize("scenario", ["retained", "reserved", "empty", "foreign-tuple", "incomplete-membership"])
def test_proposed_protected_occupancy_admission(env: OrdersEnv, buyer: Buyer, catalog: Catalog, scenario: str):
    """Serialized proposed evidence traverses the real Submit gate and transaction boundary."""
    assert env.gate_fixture is not None
    original = env.gate_fixture.read_bytes()
    fixture = json.loads(original)
    [scope] = fixture["occupancy"]
    observation = {
        "profile": "protected_full_tuple_v1", "demand_unit": "line_scope_v1",
        "observation_revision": "e2e-protected-snapshot-1",
        "scope": {"payer_tenant_id": PAYER_TENANT, "resource_tenant_id": RESOURCE_TENANT,
                  "overlap_scope_key": scope},
        "policy": {"source": "catalog", "reference": "e2e-policy", "version": "1"},
        "active_count": 0, "retained_suspended_count": int(scenario == "retained"),
        "order_reserved_count": int(scenario == "reserved"),
        "max_concurrent_active": 1, "own_membership": None,
    }
    if scenario == "foreign-tuple":
        observation["scope"]["resource_tenant_id"] = FOREIGN_TENANT
    elif scenario == "incomplete-membership":
        # Submit has no reusable stored reservation context, so even an empty own assertion refuses.
        observation["own_membership"] = {
            "complete": False,
            "context": {"order_id": FOREIGN_TENANT, "version": 1, "payer_tenant_id": PAYER_TENANT,
                        "resource_tenant_id": RESOURCE_TENANT, "line_ids": []}, "rights": [],
        }
    fixture["occupancy"][scope] = observation
    try:
        env.gate_fixture.write_text(json.dumps(fixture))
        env.server.restart()
        order_id, line_id = _draft_with_line(env, buyer, catalog)
        result = submit(buyer, order_id, CREATED_VERSION, 1)
        if scenario == "empty":
            assert result.status_code == 200, result.text
            assert _counts(env, order_id)["claims"] == 1
        else:
            reason = "overlap-cardinality-exceeded" if scenario in ("retained", "reserved") else "overlap-presence-unevaluable"
            body = _refused(result, reason)
            rows = body["context"]["data"]["assessment"]["outcomes"]
            overlap = [r for r in rows if r["predicate"] == "orders.overlap-uniqueness" and r.get("line_id") == line_id]
            assert len(overlap) == 1 and overlap[0]["reason"] == reason, overlap
            counts = _counts(env, order_id)
            assert counts["versions"] == 1 and counts["attempts"] == counts["claims"] == 0, counts
    finally:
        env.gate_fixture.write_bytes(original)
        env.server.restart()


def test_submitted_detail_and_lines_return_persisted_commercial_snapshot(
    env: OrdersEnv, buyer: Buyer, catalog: Catalog
):
    """The real HTTP read DTOs disclose the saved current version after submit."""
    order_id, line_id = _draft_with_line(env, buyer, catalog)
    submitted = submit(buyer, order_id, CREATED_VERSION, 1)
    assert submitted.status_code == 200, submitted.text
    detail = buyer.client.get(f"{API}/orders/{order_id}")
    assert detail.status_code == 200, detail.text
    assert detail.headers["etag"] == f'"{SUBMITTED_VERSION}"'
    body = detail.json()
    assert body["order"]["state"] == "submitted"
    assert body["version"]["version"] == SUBMITTED_VERSION
    assert "draft_revision" not in body
    [line] = body["lines"]
    assert line["line_id"] == line_id
    persisted = json.loads(env.psql(
        "SELECT jsonb_build_object('selected_items', selected_items, 'order_pin', order_pin,"
        " 'commercial_attempt_id', commercial_attempt_id) FROM bss_orders__order_line"
        f" WHERE order_id='{order_id}' AND version={SUBMITTED_VERSION} AND line_id='{line_id}'"
    ))
    assert line["selected_items"] == persisted["selected_items"]
    assert line["committed"] == {
        "commercial_attempt_id": persisted["commercial_attempt_id"],
        "order_pin": persisted["order_pin"],
    }
    assert line["committed"]["order_pin"]["assessment_id"] == submitted.json()["assessment_id"]
    totals = json.loads(env.psql(
        "SELECT jsonb_agg(to_jsonb(t) - 'order_id' - 'version' ORDER BY scope, line_id, charge_kind)"
        f" FROM bss_orders__resolved_total t WHERE order_id='{order_id}' AND version={SUBMITTED_VERSION}"
    ))
    assert body["resolved_totals"] == totals
    page = buyer.client.get(f"{API}/orders/{order_id}/lines", params={"limit": 1})
    assert page.status_code == 200, page.text
    assert page.headers["etag"] == detail.headers["etag"]
    assert page.json()["items"] == [line]
    assert page.json()["current_version"] == SUBMITTED_VERSION
    assert "draft_revision" not in page.json()
    assert "next_cursor" not in page.json()["page_info"]
