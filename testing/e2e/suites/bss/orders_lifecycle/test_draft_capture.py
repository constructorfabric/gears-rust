"""Live HTTP/PostgreSQL E2E for the Orders Lifecycle draft milestone (S2-12).

Every test targets one seam of ``artifacts/orders-lifecycle-s2-20261006/E2E_ACCEPTANCE.md``
against the running ``cf-gears-example-server`` over TCP: the api-gateway (AuthN middleware,
identity-keyed throttling), the authz-resolver with the rules PDP and the real
``PolicyEnforcer``, the Orders application service, the transition engine, the secure
repositories on the restricted runtime login, the sealed audit writer, the managed Event Broker
producer and response serialization. Persisted side effects are read back through ``psql``,
never through an application backdoor.

Pure validation tables stay in the gear's domain/PostgreSQL suites; here each seam is one
scenario with its own data.
"""

from __future__ import annotations

import uuid

import pytest

from .conftest import (
    ACCEPTED_PROOF,
    API,
    Buyer,
    FOREIGN_TENANT,
    NEW_SALE,
    PAYER_TENANT,
    RESOURCE_TENANT,
    SELLER_TENANT,
    OrdersEnv,
    create_body,
    create_draft,
    line_body,
    new_key,
    write_headers,
)

PROBLEM = "application/problem+json"
RESOURCE_EXHAUSTED = "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"


def problem(r, status: int, code: str | None = None) -> dict:
    assert r.status_code == status, f"{r.status_code} {r.text}"
    assert r.headers["content-type"].startswith(PROBLEM), r.headers.get("content-type")
    body = r.json()
    if code is not None:
        assert body["error_code"] == code, body
        assert body["error_domain"] == "orders-lifecycle.v1", body
    return body


# ── Authenticated draft creation ───────────────────────────────────────────


@pytest.mark.smoke
def test_authenticated_create_persists_trusted_identity_and_evidence(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    key = new_key()
    r = api.post(f"{API}/orders", json=create_body(), headers={"Idempotency-Key": key})
    assert r.status_code == 201, r.text
    view = r.json()
    order_id = view["order"]["order_id"]
    assert r.headers["etag"] == '"1"'
    assert r.headers["location"] == f"{API}/orders/{order_id}"
    assert view["version"]["version"] == 1
    assert view["draft_revision"] == 0
    assert view["order"]["state"] == "draft"
    assert view["lines"] == []
    assert view["order"]["order_number"]
    assert (
        view["order"]["resource_tenant_id"],
        view["order"]["seller_tenant_id"],
        view["order"]["payer_tenant_id"],
    ) == (RESOURCE_TENANT, SELLER_TENANT, PAYER_TENANT)

    # Persisted facts agree with the authenticated identity: the aggregate row, version 1,
    # draft revision 0 and one committed audit row whose actor is the trusted subject.
    row = env.psql(
        "SELECT state || '|' || current_version || '|' || draft_revision || '|' || resource_tenant_id"
        " || '|' || seller_tenant_id || '|' || payer_tenant_id"
        f" FROM bss_orders__order WHERE order_id = '{order_id}'"
    )
    assert row == f"draft|1|0|{RESOURCE_TENANT}|{SELLER_TENANT}|{PAYER_TENANT}"
    audit = env.psql(
        "SELECT actor || '|' || actor_class || '|' || sequence"
        f" FROM bss_orders__transition_audit WHERE order_id = '{order_id}' AND sequence IS NOT NULL"
    )
    assert audit == f"{buyer.subject_id}|user|1"
    assert env.count(
        f"SELECT count(*) FROM bss_orders__order_version WHERE order_id = '{order_id}'"
    ) == 1

    # The same key replays the stored outcome byte-for-byte; create is eventless.
    replay = api.post(f"{API}/orders", json=create_body(), headers={"Idempotency-Key": key})
    assert replay.status_code == 201
    assert replay.json() == view
    assert env.count("SELECT count(*) FROM toolkit_outbox_body") == 0


def test_caller_supplied_actor_fields_cannot_replace_trusted_identity(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    for body in (
        create_body(actor_subject_id=str(uuid.uuid4())),
        create_body(sales_path="partner"),
        create_body(created_by=buyer.subject_id),
    ):
        r = api.post(f"{API}/orders", json=body, headers={"Idempotency-Key": new_key()})
        problem(r, 400, "REQUEST_INVALID")
    # Nothing was created for these attempts.
    assert env.count(
        "SELECT count(*) FROM bss_orders__transition_audit WHERE requested_order_ref IS NULL"
        " AND order_id IS NULL"
    ) == 0


# ── Draft authoring wire and revision contract ─────────────────────────────


@pytest.mark.smoke
def test_authoring_round_trip_keeps_identities_revisions_and_persisted_membership(
    env: OrdersEnv, buyer: Buyer
):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    path = f"{API}/orders/{order_id}"

    added = api.post(f"{path}/lines", json=line_body(0), headers=write_headers(1))
    assert added.status_code == 200, added.text
    result = added.json()
    line_id = result["line_id"]
    assert (result["version"], result["draft_revision"], result["state"]) == (1, 1, "draft")
    assert added.headers["etag"] == '"1"'

    patched = api.patch(
        f"{path}/lines/{line_id}",
        json={"expected_draft_revision": 1, "fields": {"currency": "USD"}},
        headers=write_headers(1),
    )
    assert patched.status_code == 200, patched.text
    assert patched.json()["draft_revision"] == 2

    header = api.patch(
        path,
        json={"expected_draft_revision": 2, "fields": {"contract_id": str(uuid.uuid4())}},
        headers=write_headers(1),
    )
    assert header.status_code == 200, header.text
    assert header.json()["draft_revision"] == 3

    removed = api.request(
        "DELETE", f"{path}/lines/{line_id}", json={"expected_draft_revision": 3},
        headers=write_headers(1),
    )
    assert removed.status_code == 200, removed.text
    assert removed.json()["draft_revision"] == 4

    # Persisted: the identity stays reserved after removal, membership is empty, and the
    # aggregate is at version 1 / revision 4.
    assert env.count(
        f"SELECT count(*) FROM bss_orders__order_line_identity WHERE line_id = '{line_id}'"
    ) == 1
    assert env.count(
        f"SELECT count(*) FROM bss_orders__draft_content WHERE order_id = '{order_id}'"
    ) == 0
    assert env.psql(
        f"SELECT current_version || '|' || draft_revision FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == "1|4"
    read = api.get(path)
    assert read.status_code == 200
    assert read.json()["draft_revision"] == 4 and read.json()["lines"] == []


def test_retry_and_conflict_wire_behavior(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    lines = f"{API}/orders/{order_id}/lines"

    key = new_key()
    body = line_body(0)
    first = api.post(lines, json=body, headers=write_headers(1, key))
    assert first.status_code == 200, first.text
    replay = api.post(lines, json=body, headers=write_headers(1, key))
    assert replay.status_code == 200 and replay.json() == first.json()
    assert replay.headers["etag"] == first.headers["etag"]

    # Same key, changed payload: the canonical mismatch problem; nothing new is written.
    mismatch = api.post(lines, json=line_body(0, currency="USD"), headers=write_headers(1, key))
    body_m = problem(mismatch, 409, "IDEMPOTENCY_MISMATCH")
    assert body_m["type"].startswith("gts://gts.cf.")

    # A stale draft revision: a settled version-conflict with its permitted diagnostics.
    stale = api.post(lines, json=line_body(0), headers=write_headers(1))
    body_s = problem(stale, 409, "VERSION_CONFLICT")
    assert body_s["context"]["data"]["draft_revision"] == 1

    # A missing If-Match is the pre-authorization 428 with the canonical code.
    missing = api.post(lines, json=line_body(1), headers={"Idempotency-Key": new_key()})
    problem(missing, 428, "EXPECTED_VERSION_REQUIRED")

    assert env.count(
        f"SELECT count(*) FROM bss_orders__draft_content WHERE order_id = '{order_id}'"
    ) == 1
    # Both the committed write and the settled conflict are audited; the mismatch and the 428
    # are not committed transitions.
    assert env.count(
        f"SELECT count(*) FROM bss_orders__transition_audit WHERE order_id = '{order_id}'"
        " AND sequence IS NOT NULL"
    ) == 2


# ── Coherent authorized inspection ─────────────────────────────────────────


@pytest.mark.smoke
def test_reads_compose_from_the_store_page_in_sql_and_log_before_disclosure(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    path = f"{API}/orders/{order_id}"
    ids = []
    for revision in range(3):
        r = api.post(f"{path}/lines", json=line_body(revision), headers=write_headers(1))
        assert r.status_code == 200, r.text
        ids.append(r.json()["line_id"])

    detail = api.get(path)
    assert detail.status_code == 200, detail.text
    assert detail.headers["etag"] == '"1"'
    assert [line["line_id"] for line in detail.json()["lines"]] == ids
    assert detail.json()["draft_revision"] == 3
    # The ETag is the version a write sends back as If-Match.
    ok = api.patch(
        path,
        json={"expected_draft_revision": 3, "fields": {"contract_id": None}},
        headers=write_headers(1, **{"If-Match": detail.headers["etag"]}),
    )
    assert ok.status_code == 200, ok.text

    page1 = api.get(f"{path}/lines", params={"page_size": 2})
    assert page1.status_code == 200, page1.text
    assert [line["line_id"] for line in page1.json()["lines"]] == ids[:2]
    assert page1.json()["current_version"] == 1 and page1.json().get("next_cursor")
    page2 = api.get(f"{path}/lines", params={"page_size": 2, "cursor": page1.json()["next_cursor"]})
    assert page2.status_code == 200
    assert [line["line_id"] for line in page2.json()["lines"]] == ids[2:]
    assert page2.json().get("next_cursor") is None

    listing = api.get(f"{API}/orders", params={"page_size": 200})
    assert listing.status_code == 200, listing.text
    assert any(o["order"]["order_id"] == order_id for o in listing.json()["orders"])
    assert all(o["order"]["resource_tenant_id"] == RESOURCE_TENANT for o in listing.json()["orders"])

    problem(api.get(f"{API}/orders", params={"page_size": 0}), 400, "PAGE_SIZE_EXCEEDED")
    problem(api.get(f"{API}/orders", params={"cursor": "not-a-token"}), 400, "CURSOR_INVALID")

    # An own-tenant read without a proof logs nothing (08 §4.4); the seller's direct
    # cross-tenant read is logged as served before the payload (asserted below by its row).
    assert env.count(
        f"SELECT count(*) FROM bss_orders__read_access_log WHERE order_id = '{order_id}'"
    ) == 0
    seller = env.client("seller").get(path)
    assert seller.status_code == 200, seller.text
    assert seller.json()["order"]["order_id"] == order_id
    assert env.psql(
        "SELECT operation || '|' || outcome || '|' || actor_class"
        f" FROM bss_orders__read_access_log WHERE order_id = '{order_id}'"
    ) == "get|served|user"


# ── Real authorization and tenant isolation ────────────────────────────────


@pytest.mark.smoke
def test_authorization_matrix_and_tenant_isolation(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    path = f"{API}/orders/{order_id}"
    patch = {"expected_draft_revision": 0, "fields": {"contract_id": None}}

    # A foreign tenant sees nothing: hidden targets answer order-not-found on reads and writes,
    # and an untargeted list is refused for the actor.
    foreign = env.client("foreign")
    problem(foreign.get(path), 404, "ORDER_NOT_FOUND")
    problem(foreign.patch(path, json=patch, headers=write_headers(1)), 404, "ORDER_NOT_FOUND")
    problem(foreign.get(f"{API}/orders"), 403, "OPERATION_NOT_PERMITTED_FOR_ACTOR")
    problem(
        foreign.post(f"{API}/orders", json=create_body(), headers={"Idempotency-Key": new_key()}),
        403,
        "OPERATION_NOT_PERMITTED_FOR_ACTOR",
    )

    # Same-tenant membership alone grants nothing: the buyer's colleague is hidden too.
    problem(env.client("member").get(path), 404, "ORDER_NOT_FOUND")

    # The seller operator may read but not author: a readable target refuses the action.
    seller = env.client("seller")
    assert seller.get(path).status_code == 200
    problem(seller.patch(path, json=patch, headers=write_headers(1)), 403, "OPERATION_NOT_PERMITTED_FOR_ACTOR")
    problem(
        seller.post(f"{API}/orders", json=create_body(), headers={"Idempotency-Key": new_key()}),
        403,
        "OPERATION_NOT_PERMITTED_FOR_ACTOR",
    )
    # The payer's reader sees the order through the payer axis only.
    assert env.client("payer").get(path).status_code == 200

    # The buyer's complete arrangement is authorized: a payer outside the payer-use set is
    # refused for the actor, and a foreign resource tenant is refused, with nothing created.
    for body in (
        create_body(payer_tenant_id=FOREIGN_TENANT),
        create_body(resource_tenant_id=FOREIGN_TENANT),
    ):
        problem(
            api.post(f"{API}/orders", json=body, headers={"Idempotency-Key": new_key()}),
            403,
            "OPERATION_NOT_PERMITTED_FOR_ACTOR",
        )
    # Nothing leaked and nothing changed under the hidden/forbidden attempts.
    assert env.psql(
        f"SELECT draft_revision FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == "0"
    assert env.count(
        f"SELECT count(*) FROM bss_orders__order WHERE resource_tenant_id = '{FOREIGN_TENANT}'"
        f" OR payer_tenant_id = '{FOREIGN_TENANT}'"
    ) == 0


def test_service_principals_need_finite_explicit_constraints(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    path = f"{API}/orders/{order_id}"
    # The Workflow principal's finite scope names no order of this run: hidden target, and its
    # list is the (empty) finite set — never a tenant-wide disclosure.
    workflow = env.client("workflow")
    problem(workflow.get(path), 404, "ORDER_NOT_FOUND")
    listing = workflow.get(f"{API}/orders")
    assert listing.status_code == 200, listing.text
    assert listing.json()["orders"] == []
    # A service principal whose policy has no finite order-ID path is an integration failure:
    # fail closed, sanitized, never a grant.
    tenant_only = env.client("subscriptions")
    r = tenant_only.get(path)
    assert r.status_code == 500, r.text
    assert r.headers["content-type"].startswith(PROBLEM)
    # Sanitized: no tenant, policy or path detail beyond the request's own instance URL.
    assert SELLER_TENANT not in r.text and "seller_tenant_id" not in r.text and "rule" not in r.text
    # Service-only Workflow seams are not mounted: unreachable from any token.
    for seam in ("begin-fulfillment", "spawn-signal", "approval-reflection", "workflow-cancel", "submit", "cancel", "hold"):
        r = workflow.post(f"{path}/{seam}", json={}, headers=write_headers(1))
        assert r.status_code in (404, 405), f"{seam}: {r.status_code} {r.text}"
    # A service read of a Workflow seam through the buyer's human path does not exist either.
    r = api.post(f"{path}/begin-fulfillment", json={}, headers=write_headers(1))
    assert r.status_code in (404, 405)


def test_delegation_proof_header_is_forwarded_never_echoed(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    path = f"{API}/orders/{order_id}"
    partner = env.client("partner")
    # Without a proof the delegated path is not satisfied: hidden target.
    problem(partner.get(path), 404, "ORDER_NOT_FOUND")
    # An accepted proof reference discloses the order; the header is never echoed, and the
    # served row records the reference as supplied.
    ok = partner.get(path, headers={"X-Delegation-Proof-Ref": ACCEPTED_PROOF})
    assert ok.status_code == 200, ok.text
    assert "x-delegation-proof-ref" not in {k.lower() for k in ok.headers}
    assert ACCEPTED_PROOF not in ok.text
    assert env.psql(
        "SELECT outcome || '|' || delegation_proof_ref FROM bss_orders__read_access_log"
        f" WHERE order_id = '{order_id}' AND actor_class = 'user' AND outcome = 'served'"
        " ORDER BY accessed_at DESC LIMIT 1"
    ) == f"served|{ACCEPTED_PROOF}"
    # A wrong proof on a targeted read is operational-only: the public answer stays
    # order-not-found; an untargeted list discloses the proof reason.
    problem(partner.get(path, headers={"X-Delegation-Proof-Ref": "e2e-proof-bad"}), 404, "ORDER_NOT_FOUND")
    problem(partner.get(f"{API}/orders", headers={"X-Delegation-Proof-Ref": "e2e-proof-bad"}), 403, "DELEGATION_PROOF_INVALID")
    # A malformed reference is request-invalid before authorization and logs nothing.
    before = env.count(f"SELECT count(*) FROM bss_orders__read_access_log WHERE order_id = '{order_id}'")
    problem(partner.get(path, headers={"X-Delegation-Proof-Ref": "bad value"}), 400, "REQUEST_INVALID")
    assert env.count(f"SELECT count(*) FROM bss_orders__read_access_log WHERE order_id = '{order_id}'") == before


# ── Pre-engine throttling (D-185) ──────────────────────────────────────────


def test_gateway_caller_zone_throttles_before_the_engine(env: OrdersEnv, buyer: Buyer):
    """The identity-keyed `rl_orders_caller_write` zone (3/s, burst 20) on the live gateway.

    The zone is keyed per authenticated subject and refills at 3/s, so this test's own buyer
    sends a burst of 40 writes: at least the overflow beyond the burst plus the refill meets
    the gateway's canonical 429 with its policy headers, and every throttled attempt wrote
    nothing. The gateway runs first; its own 429s name `rate_limit` in the violation and carry
    no `RateLimit-Remaining` (the zone decorates only admitted responses with it). Whatever the
    gateway admits beyond the gear-local per-(caller, order) fallback's budget (10/min on this
    host) is refused by that limiter with its `per-order request limit exceeded` violation; it is
    proven on its own in `test_per_order_edge_limiter_is_keyed_per_order_over_http`.
    """
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    lines = f"{API}/orders/{order_id}/lines"
    audits_before = env.count(
        f"SELECT count(*) FROM bss_orders__transition_audit WHERE order_id = '{order_id}'"
    )
    outcomes = []
    for _ in range(40):
        r = api.post(lines, json=line_body(None), headers=write_headers(1))
        outcomes.append(r)
    admitted = [r for r in outcomes if r.status_code != 429]
    throttled = [r for r in outcomes if r.status_code == 429]
    assert throttled, [r.status_code for r in outcomes]
    for r in throttled:
        assert r.headers["content-type"].startswith(PROBLEM)
        assert r.json()["type"] == RESOURCE_EXHAUSTED
        assert r.json()["context"]["violations"][0]["subject"] == "throttling"
        assert "retry-after" in r.headers
    descriptions = [r.json()["context"]["violations"][0]["description"] for r in throttled]
    gateway = [r for r, d in zip(throttled, descriptions) if d == "rate_limit limit exceeded"]
    edge = [r for r, d in zip(throttled, descriptions) if d == "per-order request limit exceeded"]
    assert len(gateway) + len(edge) == len(throttled), descriptions
    assert gateway, "the gateway caller zone rejected with its own violation"
    for r in gateway:
        assert "ratelimit-policy" in r.headers and "ratelimit-remaining" not in r.headers
        assert r.json()["detail"] == "throttling limit exceeded (rate_limit)"
    # The gateway admitted its burst (20 plus the refill); everything it admitted beyond the edge
    # limiter's budget was refused by the fallback, never by the engine.
    assert len(admitted) <= 10, [r.status_code for r in outcomes]
    assert edge, "the fallback refused what the gateway admitted beyond 10"
    assert all(r.status_code == 409 for r in admitted), [r.status_code for r in admitted]
    # Every admitted attempt (a settled version-conflict, revision omitted) wrote exactly one
    # audit row; a throttled attempt wrote none.
    audits_after = env.count(
        f"SELECT count(*) FROM bss_orders__transition_audit WHERE order_id = '{order_id}'"
    )
    assert audits_after - audits_before == len(admitted)
    assert env.count(
        f"SELECT count(*) FROM bss_orders__draft_content WHERE order_id = '{order_id}'"
    ) == 0


# ── Readiness and persistence ──────────────────────────────────────────────


@pytest.mark.timeout(120)
def test_readiness_is_real_and_reads_survive_a_restart(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    """A restart (stop, start, wait for /healthz then /readyz) is bounded by its own budget."""
    ready = api.get("/readyz")
    assert ready.status_code == 200, ready.text
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    r = api.post(f"{API}/orders/{order_id}/lines", json=line_body(0), headers=write_headers(1))
    assert r.status_code == 200, r.text
    before = api.get(f"{API}/orders/{order_id}").json()
    env.server.restart()
    after = api.get(f"{API}/orders/{order_id}")
    assert after.status_code == 200, after.text
    assert after.json() == before
    assert env.count(
        f"SELECT count(*) FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == 1


# ── Failed disclosures and transactions ────────────────────────────────────


def test_required_log_failure_serves_no_protected_data(env: OrdersEnv, buyer: Buyer):
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    path = f"{API}/orders/{order_id}"
    seller = env.client("seller")
    env.psql("REVOKE INSERT ON bss_orders__read_access_log FROM bss_orders_runtime")
    try:
        r = seller.get(path)
        assert r.status_code == 503, f"{r.status_code} {r.text}"
        assert r.headers["content-type"].startswith(PROBLEM)
        body = r.json()
        assert body["detail"] == "read-store-unavailable", body
        assert "permission denied" not in r.text and "order_number" not in r.text, body
        assert "etag" not in r.headers
    finally:
        env.psql("GRANT INSERT ON bss_orders__read_access_log TO bss_orders_runtime")
    ok = seller.get(path)
    assert ok.status_code == 200, ok.text


def test_failed_audit_rolls_back_the_admitted_write_over_http(env: OrdersEnv, buyer: Buyer):
    api = buyer.client
    view = create_draft(env, buyer)
    order_id = view["order"]["order_id"]
    lines = f"{API}/orders/{order_id}/lines"
    key = new_key()
    env.psql("REVOKE INSERT ON bss_orders__transition_audit FROM bss_orders_runtime")
    try:
        r = api.post(lines, json=line_body(0), headers=write_headers(1, key))
        assert r.status_code in (500, 503), r.text
        assert r.headers["content-type"].startswith(PROBLEM)
        assert "transition_audit" not in r.text and "permission denied" not in r.text
    finally:
        env.psql("GRANT INSERT ON bss_orders__transition_audit TO bss_orders_runtime")
    assert env.count(
        f"SELECT count(*) FROM bss_orders__draft_content WHERE order_id = '{order_id}'"
    ) == 0
    assert env.psql(
        f"SELECT draft_revision FROM bss_orders__order WHERE order_id = '{order_id}'"
    ) == "0"
    retried = api.post(lines, json=line_body(0), headers=write_headers(1, key))
    assert retried.status_code == 200, retried.text
    assert retried.json()["draft_revision"] == 1


def test_per_order_edge_limiter_is_keyed_per_order_over_http(env: OrdersEnv, buyer: Buyer):
    """The Q-26 fallback (D-185): `(subject_id, orderId)` at 10/min on this host, before the
    gateway's burst of 20 is reached, so every 429 here is the gear-local limiter's.

    Every request to an existing-order write counts (admitted, boundary-refused and replayed
    alike); a refusal is the canonical `resource_exhausted` Problem with `Retry-After` and the
    limiter's own `per-order request limit exceeded` violation. The gateway admitted it, so the
    zone's `RateLimit-Remaining` is still present (the gateway's own rejections never carry it).
    It writes nothing, and the same buyer's other order keeps its own budget.
    """
    api = buyer.client
    first = create_draft(env, buyer)["order"]["order_id"]
    second = create_draft(env, buyer)["order"]["order_id"]
    path = f"{API}/orders/{first}"
    key = new_key()
    committed = line_body(0)
    outcomes = []
    # 1: a committed line add; 2: its byte-identical replay; 3: a 428 (no If-Match); 4..10:
    # stale-revision conflicts. All ten count against (buyer, first).
    outcomes.append(api.post(f"{path}/lines", json=committed, headers=write_headers(1, key)))
    outcomes.append(api.post(f"{path}/lines", json=committed, headers=write_headers(1, key)))
    outcomes.append(api.post(f"{path}/lines", json=line_body(1), headers={"Idempotency-Key": new_key()}))
    for _ in range(7):
        outcomes.append(api.post(f"{path}/lines", json=line_body(0), headers=write_headers(1)))
    assert [r.status_code for r in outcomes] == [200, 200, 428] + [409] * 7, [
        (r.status_code, r.text[:120]) for r in outcomes
    ]
    # Ten requests spent ten of the gateway's twenty: the eleventh is the edge limiter's call.
    assert int(outcomes[-1].headers["ratelimit-remaining"]) >= 7, dict(outcomes[-1].headers)
    audits = env.count(
        f"SELECT count(*) FROM bss_orders__transition_audit WHERE order_id = '{first}'"
    )
    lines = env.count(f"SELECT count(*) FROM bss_orders__draft_content WHERE order_id = '{first}'")
    # The eleventh request on the first order, well-formed and current, is refused at the edge.
    r = api.post(f"{path}/lines", json=line_body(1), headers=write_headers(1))
    assert r.status_code == 429, f"{r.status_code} {r.text}"
    assert r.headers["content-type"].startswith(PROBLEM)
    # 10/min: one token every 6 s, so the hint is at most 6 s and never 0.
    assert 1 <= int(r.headers["retry-after"]) <= 6, dict(r.headers)
    assert "ratelimit-remaining" in r.headers, dict(r.headers)
    body = r.json()
    assert body["type"] == RESOURCE_EXHAUSTED
    assert body["detail"] == "Orders per-order request limit exceeded", body
    assert body["context"]["violations"][0]["subject"] == "throttling"
    assert body["context"]["violations"][0]["description"] == "per-order request limit exceeded"
    assert body["context"]["violations"][0]["retry_after_seconds"] == int(r.headers["retry-after"])
    # A replay of the committed key is refused the same way: it never reaches the registry.
    replay = api.post(f"{path}/lines", json=committed, headers=write_headers(1, key))
    assert replay.status_code == 429, replay.text
    assert replay.json()["context"]["violations"][0]["description"] == "per-order request limit exceeded"
    assert env.count(
        f"SELECT count(*) FROM bss_orders__transition_audit WHERE order_id = '{first}'"
    ) == audits
    assert env.count(
        f"SELECT count(*) FROM bss_orders__draft_content WHERE order_id = '{first}'"
    ) == lines
    # Keyed per order: the same buyer's second order is admitted and committed.
    other = api.post(f"{API}/orders/{second}/lines", json=line_body(0), headers=write_headers(1))
    assert other.status_code == 200, other.text
    assert other.json()["draft_revision"] == 1


def test_malformed_path_parameters_are_canonical_problems(env: OrdersEnv, buyer: Buyer):
    """A non-UUID `orderId`/`lineId` on any mounted route is the platform's canonical
    `invalid_argument` Problem (toolkit `extract::Path`), not a plain-text 400, and it is
    answered before authorization, the limiter and the store: no audit or access-log row.
    """
    api = buyer.client
    audits = env.count("SELECT count(*) FROM bss_orders__transition_audit")
    logs = env.count("SELECT count(*) FROM bss_orders__read_access_log")
    attempts = [
        api.get(f"{API}/orders/not-a-uuid"),
        api.get(f"{API}/orders/not-a-uuid/lines"),
        api.patch(f"{API}/orders/not-a-uuid", json={"fields": {"contract_id": None}}, headers=write_headers(1)),
        api.post(f"{API}/orders/not-a-uuid/lines", json=line_body(0), headers=write_headers(1)),
        api.patch(f"{API}/orders/{uuid.uuid4()}/lines/not-a-uuid", json={"fields": {"currency": "USD"}}, headers=write_headers(1)),
        api.request("DELETE", f"{API}/orders/not-a-uuid/lines/nope", headers=write_headers(1)),
    ]
    for r in attempts:
        assert r.status_code == 400, f"{r.request.method} {r.request.url}: {r.status_code} {r.text}"
        assert r.headers["content-type"].startswith(PROBLEM), r.headers.get("content-type")
        body = r.json()
        assert body["type"].endswith("cf.core.err.invalid_argument.v1~"), body
        assert body["context"]["field_violations"][0]["field"] == "path", body
    assert env.count("SELECT count(*) FROM bss_orders__transition_audit") == audits
    assert env.count("SELECT count(*) FROM bss_orders__read_access_log") == logs
