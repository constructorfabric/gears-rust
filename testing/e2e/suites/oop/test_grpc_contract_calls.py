"""ToolKit contract calls over gRPC across the OoP boundary.

Every request here goes edge -> api-contracts-grpc-consumer process -> gRPC ->
api-contracts provider process. The consumer declares
`#[toolkit::consumes(contract = PaymentApi, from = "api-contracts", transport = grpc)]`
and does not link the provider, so a successful answer can only come from the
provider's grpc-hub, found through the DirectoryService by the service name
`api_contracts.payment.v1.PaymentApi`. (Topology: see conftest.)

Covered seams:
  * unary calls both ways (charge, get_invoice);
  * a canonical error carried in the gRPC Problem trailer and rendered by the
    consumer as the same problem+json (404, 400 with the field violation, 400
    with precondition violations);
  * server streaming: `list_payments` read to the end, and `stream_payments`
    (fallible open) relayed to the HTTP client as NDJSON, with an open-time
    rejection arriving as a Problem before any line;
  * the tenant survives the gRPC hop: the provider authenticates the forwarded
    bearer, so tenant B does not list tenant A's payments;
  * REST and gRPC are two transports of one provider, not two providers.
"""
from __future__ import annotations

import json
import uuid

import httpx
import pytest

from .conftest import REQUEST_TIMEOUT

BASE = "/api-contracts-grpc-consumer/v1"
REST_CHARGE = "/api-contracts-consumer/v1/charge"
PROBLEM = "application/problem+json"


def _charge(base_url: str, headers: dict, description: str, amount: int = 1000) -> dict:
    r = httpx.post(
        f"{base_url}{BASE}/charge",
        headers={**headers, "Content-Type": "application/json"},
        json={"amount_cents": amount, "currency": "USD", "description": description},
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 200, f"charge: expected 200, got {r.status_code}: {r.text}"
    return r.json()


def _payment_ids(base_url: str, headers: dict) -> set[str]:
    r = httpx.get(f"{base_url}{BASE}/payments", headers=headers, timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"list: expected 200, got {r.status_code}: {r.text}"
    return {p["payment_id"] for p in r.json()}


def _feed(base_url: str, headers: dict, params: dict | None = None) -> httpx.Response:
    return httpx.get(
        f"{base_url}{BASE}/payments/feed",
        headers=headers,
        params=params or {},
        timeout=REQUEST_TIMEOUT,
    )


def _problem(r: httpx.Response, status: int) -> dict:
    assert r.status_code == status, f"expected {status}, got {r.status_code}: {r.text}"
    assert PROBLEM in r.headers.get("content-type", ""), r.headers
    return r.json()


# ── unary ────────────────────────────────────────────────────────────────────


@pytest.mark.smoke
def test_charge_over_grpc(oop_cluster, auth):
    """A charge reaches the provider over gRPC and comes back as a pending payment."""
    body = _charge(oop_cluster, auth, "grpc charge")
    assert body.get("payment_id"), body
    assert body.get("status") == "pending", body


def test_invoice_round_trip_over_grpc(oop_cluster, auth):
    """A payment charged over gRPC is read back over gRPC with the same fields."""
    payment_id = _charge(oop_cluster, auth, "grpc invoice", amount=4242)["payment_id"]

    r = httpx.get(f"{oop_cluster}{BASE}/invoices/{payment_id}", headers=auth,
                  timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    invoice = r.json()
    assert invoice["payment_id"] == payment_id, invoice
    assert invoice["amount_cents"] == 4242, invoice
    assert invoice["currency"] == "USD", invoice
    assert invoice["description"] == "grpc invoice", invoice


def test_unknown_invoice_is_404_problem(oop_cluster, auth):
    """The provider's NotFound crosses gRPC in the Problem trailer and stays a 404.

    A bare `NOT_FOUND` status would also become a 404, but with the resource
    type unknown; the provider's resource type and name in the context can only
    have come from the trailer.
    """
    missing = str(uuid.uuid4())
    r = httpx.get(f"{oop_cluster}{BASE}/invoices/{missing}", headers=auth,
                  timeout=REQUEST_TIMEOUT)
    problem = _problem(r, 404)
    assert "not_found" in problem.get("type", ""), problem
    context = problem.get("context", {})
    assert context.get("resource_type") == "gts.cf.demo.api_contracts.payment.v1~", problem
    assert context.get("resource_name") == missing, problem


# ── server streaming ─────────────────────────────────────────────────────────


def test_list_payments_streams_over_grpc(oop_cluster, auth_tenant_a):
    """`list_payments` is a gRPC server stream; every payment of the caller arrives."""
    before = _payment_ids(oop_cluster, auth_tenant_a)
    charged = {
        _charge(oop_cluster, auth_tenant_a, f"grpc list {i}", amount=100 + i)["payment_id"]
        for i in range(3)
    }
    after = _payment_ids(oop_cluster, auth_tenant_a)
    assert charged <= after, f"missing from the stream: {charged - after}"
    assert after - before == charged, after - before


def test_payment_feed_relays_the_stream_as_ndjson(oop_cluster, auth_tenant_a):
    """`stream_payments` items reach the HTTP client one JSON line each.

    The filter travels in the gRPC request and the provider applies it. The
    test reads the whole body, so it does not show that lines leave as items
    arrive; the relay's error line is covered by the consumer's unit tests.
    """
    eur = {
        _charge_currency(oop_cluster, auth_tenant_a, "EUR", f"grpc feed {i}")
        for i in range(2)
    }
    _charge(oop_cluster, auth_tenant_a, "grpc feed usd")

    r = _feed(oop_cluster, auth_tenant_a, {"currency": "EUR"})
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    assert r.headers.get("content-type", "").startswith("application/x-ndjson"), r.headers
    lines = [json.loads(line) for line in r.text.splitlines() if line.strip()]
    assert all("error" not in line for line in lines), lines
    assert {line["currency"] for line in lines} == {"EUR"}, lines
    assert eur <= {line["payment_id"] for line in lines}, lines


def _charge_currency(base_url: str, headers: dict, currency: str, description: str) -> str:
    r = httpx.post(
        f"{base_url}{BASE}/charge",
        headers={**headers, "Content-Type": "application/json"},
        json={"amount_cents": 500, "currency": currency, "description": description},
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 200, f"charge: expected 200, got {r.status_code}: {r.text}"
    return r.json()["payment_id"]


def test_payment_feed_rejected_filter_fails_at_open(oop_cluster, auth):
    """A filter the provider rejects fails the open, before any item.

    The gRPC status arrives in the response headers with the Problem trailer;
    the consumer turns it into a 400 problem+json that still names the field.
    """
    problem = _problem(_feed(oop_cluster, auth, {"currency": "usd"}), 400)
    assert "invalid_argument" in problem.get("type", ""), problem
    assert "currency" in json.dumps(problem), problem


def test_payment_feed_precondition_carries_every_violation(oop_cluster, auth):
    """A FailedPrecondition with two violations crosses gRPC with both of them."""
    problem = _problem(_feed(oop_cluster, auth, {"status": "failed"}), 400)
    assert "failed_precondition" in problem.get("type", ""), problem
    text = json.dumps(problem)
    assert "payments:0" in text and "payments:3" in text, problem


# ── identity across the hop ──────────────────────────────────────────────────


def test_tenant_is_preserved_over_grpc(oop_cluster, auth_tenant_a, auth_tenant_b):
    """The provider authenticates the bearer the consumer forwards over gRPC.

    Payments are scoped by the caller's tenant, on the stream and on the unary
    invoice lookup alike. If the provider took the token at face value, without
    a tenant, both tenants would see the same payments.
    """
    a_payment = _charge(oop_cluster, auth_tenant_a, "tenant a only")["payment_id"]
    b_payment = _charge(oop_cluster, auth_tenant_b, "tenant b only")["payment_id"]

    a_ids = _payment_ids(oop_cluster, auth_tenant_a)
    b_ids = _payment_ids(oop_cluster, auth_tenant_b)
    assert a_payment in a_ids and a_payment not in b_ids, (a_ids, b_ids)
    assert b_payment in b_ids and b_payment not in a_ids, (a_ids, b_ids)

    def invoice(headers: dict, payment_id: str) -> httpx.Response:
        return httpx.get(f"{oop_cluster}{BASE}/invoices/{payment_id}", headers=headers,
                         timeout=REQUEST_TIMEOUT)

    assert invoice(auth_tenant_a, a_payment).status_code == 200
    # Another tenant's invoice is the same 404 as an unknown one.
    problem = _problem(invoice(auth_tenant_b, a_payment), 404)
    assert "not_found" in problem.get("type", ""), problem


def test_unknown_token_is_rejected_by_the_provider(oop_cluster):
    """The edge and the consumer accept any bearer; the provider does not.

    Both run static-authn `accept_all`, so the only place a non-empty token can
    be refused is the provider, on its side of the gRPC hop. The refusal comes
    back as a 401 problem with the AuthN reason, not a 500.
    """
    r = httpx.post(
        f"{oop_cluster}{BASE}/charge",
        headers={"Authorization": "Bearer not-a-known-token",
                 "Content-Type": "application/json"},
        json={"amount_cents": 1, "currency": "USD", "description": "unknown token"},
        timeout=REQUEST_TIMEOUT,
    )
    problem = _problem(r, 401)
    assert "unauthenticated" in problem.get("type", ""), problem
    assert problem.get("context", {}).get("reason") == "AUTHN_FAILED", problem


# ── one provider, two transports ─────────────────────────────────────────────


def test_rest_and_grpc_reach_the_same_provider(oop_cluster, auth):
    """A payment charged through the REST consumer is readable over gRPC."""
    r = httpx.post(
        f"{oop_cluster}{REST_CHARGE}",
        headers={**auth, "Content-Type": "application/json"},
        json={"amount_cents": 777, "currency": "USD", "description": "via rest"},
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 200, f"REST charge: expected 200, got {r.status_code}: {r.text}"
    payment_id = r.json()["payment_id"]

    r = httpx.get(f"{oop_cluster}{BASE}/invoices/{payment_id}", headers=auth,
                  timeout=REQUEST_TIMEOUT)
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    assert r.json()["amount_cents"] == 777, r.json()
