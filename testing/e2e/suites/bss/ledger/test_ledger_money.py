"""Decimal money on the ledger wire, over the real binary (money ADR).

Money is plain decimal text in major units with its currency and posting scale. The ledger never
rounds at its boundary, takes the scale its registry gives, keeps every ISO 4217 scale fixed, and
reads amounts back in canonical text. The binary links other gears, so this also shows that no
other gear changes how the ledger reads a JSON number.
"""

import httpx

from .conftest import (
    API_BASE,
    SEED_PAYER_ID,
    TENANT_A_ID,
    _money,
    _post_invoice,
    _provision_seller,
)

REQUEST_TIMEOUT = 10.0


def _violation(r: httpx.Response) -> dict:
    assert r.status_code == 400, f"expected 400, got {r.status_code}: {r.text}"
    [violation] = r.json()["context"]["field_violations"]
    return violation


def test_posted_money_reads_back_in_canonical_text_at_its_scale(seller):
    base_url, headers = seller
    posted = _post_invoice(
        base_url, headers, TENANT_A_ID, SEED_PAYER_ID, _money("10.00"), _money("2.00")
    )
    assert posted.status_code == 201, posted.text
    with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
        r = client.get(
            f"{base_url}{API_BASE}/journal-entries/{posted.json()['entry_id']}",
            params={"tenant_id": TENANT_A_ID},
            headers=headers,
        )
    assert r.status_code == 200, r.text
    lines = {line["account_class"]: (line["side"], line["money"]) for line in r.json()["lines"]}
    assert lines == {
        "AR": ("DR", _money("12")),
        "REVENUE": ("CR", _money("10")),
        "TAX_PAYABLE": ("CR", _money("2")),
    }, r.text


def test_an_amount_off_the_posting_scale_is_refused_never_rounded(seller):
    base_url, headers = seller
    v = _violation(_post_invoice(base_url, headers, TENANT_A_ID, SEED_PAYER_ID, _money("10.001")))
    assert (v["field"], v["reason"]) == ("amount", "INVALID_POSTING_INCREMENT"), v


def test_money_at_another_scale_than_the_registry_is_refused(seller):
    base_url, headers = seller
    v = _violation(
        _post_invoice(
            base_url,
            headers,
            TENANT_A_ID,
            SEED_PAYER_ID,
            _money("10.000", scale=3),
            _money("2.000", scale=3),
        )
    )
    assert (v["field"], v["reason"]) == ("lines", "CURRENCY_SCALE_MISMATCH"), v


def test_a_json_number_amount_is_refused(seller):
    base_url, headers = seller
    number = {"amount": 10.0, "currency": "USD", "currency_scale": 2}
    v = _violation(_post_invoice(base_url, headers, TENANT_A_ID, SEED_PAYER_ID, number))
    assert (v["field"], v["reason"]) == ("body", "invalid_json_body"), v
    assert "expected a string" in v["description"], v


def test_an_amount_that_is_not_plain_decimal_text_is_refused(seller):
    base_url, headers = seller
    r = _post_invoice(base_url, headers, TENANT_A_ID, SEED_PAYER_ID, _money("1e3"))
    assert r.status_code == 400, r.text
    assert r.json()["detail"] == "items.amount_ex_tax: invalid decimal", r.text


def test_provisioning_refuses_an_iso_code_at_another_scale(seller):
    base_url, headers = seller
    hungarian = _provision_seller(
        base_url, headers, TENANT_A_ID, [{"currency": "HUF", "currency_scale": 0}]
    )
    assert hungarian.status_code == 400, hungarian.text
    assert hungarian.json()["detail"] == (
        "currency HUF is an ISO 4217 code with scale 2; the registry cannot set scale 0 for it"
    ), hungarian.text


def test_a_code_without_an_iso_scale_posts_at_its_provisioned_scale(seller):
    base_url, headers = seller
    prov = _provision_seller(
        base_url,
        headers,
        TENANT_A_ID,
        [{"currency": "XTS", "currency_scale": 3}],
        accounts=[
            {"account_class": "AR", "currency": "XTS", "normal_side": "DR"},
            {"account_class": "TAX_PAYABLE", "currency": "XTS", "normal_side": "CR"},
            {"account_class": "SUSPENSE", "currency": "XTS", "normal_side": "DR"},
            {
                "account_class": "REVENUE",
                "currency": "XTS",
                "normal_side": "CR",
                "revenue_stream": "subscription",
            },
        ],
    )
    assert prov.status_code == 200, prov.text
    posted = _post_invoice(
        base_url,
        headers,
        TENANT_A_ID,
        SEED_PAYER_ID,
        _money("10.125", "XTS", 3),
        _money("2.000", "XTS", 3),
    )
    assert posted.status_code == 201, posted.text
    with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
        r = client.get(
            f"{base_url}{API_BASE}/journal-entries/{posted.json()['entry_id']}",
            params={"tenant_id": TENANT_A_ID},
            headers=headers,
        )
    assert r.status_code == 200, r.text
    lines = {line["account_class"]: line["money"] for line in r.json()["lines"]}
    assert lines == {
        "AR": _money("12.125", "XTS", 3),
        "REVENUE": _money("10.125", "XTS", 3),
        "TAX_PAYABLE": _money("2", "XTS", 3),
    }, r.text
    off_scale = _post_invoice(
        base_url,
        headers,
        TENANT_A_ID,
        SEED_PAYER_ID,
        _money("10.12", "XTS", 2),
        _money("2.00", "XTS", 2),
    )
    assert off_scale.status_code == 400, off_scale.text
    violation = _violation(off_scale)
    assert (violation["field"], violation["reason"]) == ("lines", "CURRENCY_SCALE_MISMATCH"), (
        off_scale.text
    )
