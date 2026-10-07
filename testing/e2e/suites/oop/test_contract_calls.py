"""ToolKit contract call over REST across the OoP boundary, plus input handling.

A single edge request to the consumer's exposed charge route drives
edge -> consumer process -> provider process: the consumer resolves the
`PaymentApi` ToolKit contract (`#[toolkit::contract]`) from the SEPARATE provider
(its binary does not link it) and forwards the charge over REST, discovered via
the DirectoryService. The happy path proves the contract call works across
processes; the input-rejection cases prove the receiving gear decodes/validates
the body itself and rejects bad input as a 4xx rather than proxying it on to
surface later as a 500. (Topology: see conftest.)
"""
from __future__ import annotations

import httpx
import pytest

from .conftest import REQUEST_TIMEOUT

CHARGE_ROUTE = "/api-contracts-consumer/v1/charge"
VALID_BODY = {"amount_cents": 1000, "currency": "USD", "description": "oop-e2e charge"}


@pytest.mark.smoke
def test_oop_to_oop_charge_over_rest(oop_cluster, auth):
    """Seam: the consumer resolves the provider over REST and forwards a charge.

    The charge can only succeed if it travelled consumer-process -> provider-process
    over REST (the consumer binary does not link the provider), returning a real
    pending payment.
    """
    r = httpx.post(
        f"{oop_cluster}{CHARGE_ROUTE}",
        headers={**auth, "Content-Type": "application/json"},
        json=VALID_BODY,
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 200, f"expected 200, got {r.status_code}: {r.text}"
    body = r.json()
    assert body.get("payment_id"), body
    assert body.get("status") == "pending", body


def test_malformed_body_rejected_by_gear(oop_cluster, auth):
    """Seam: a syntactically broken body is rejected at the receiving gear.

    The edge proxies the raw bytes to the gear process, whose JSON decoder fails
    them before any handler runs — so the request never fans out to a further OoP
    hop. Syntactically invalid JSON is a 400 Bad Request, rendered as RFC 9457
    `problem+json`, never a 500.
    """
    r = httpx.post(
        f"{oop_cluster}{CHARGE_ROUTE}",
        headers={**auth, "Content-Type": "application/json"},
        content=b"{not valid json",
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 400, f"expected 400, got {r.status_code}: {r.text}"
    assert "application/problem+json" in r.headers.get("content-type", ""), r.headers


def test_incomplete_body_rejected_by_gear(oop_cluster, auth):
    """Seam: a body missing required fields is rejected at the receiving gear.

    Well-formed JSON that omits required `ChargeRequest` fields is a 422
    Unprocessable Entity. The 422 pins the rejection to the gear specifically:
    only the receiving process knows the request schema — the edge proxies bytes
    and has no idea what fields a charge needs — so a schema-validation failure
    cannot originate at the edge. Never a 500, and never a silently-defaulted
    charge forwarded to the provider.
    """
    incomplete = {k: VALID_BODY[k] for k in ("amount_cents",)}
    r = httpx.post(
        f"{oop_cluster}{CHARGE_ROUTE}",
        headers={**auth, "Content-Type": "application/json"},
        json=incomplete,
        timeout=REQUEST_TIMEOUT,
    )
    assert r.status_code == 422, f"expected 422, got {r.status_code}: {r.text}"
    assert "application/problem+json" in r.headers.get("content-type", ""), r.headers
