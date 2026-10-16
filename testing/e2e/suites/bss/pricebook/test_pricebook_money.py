"""Decimal money on the pricing wire, over the real binary (money ADR; D-527, D-528).

Every price decimal is a JSON string of plain decimal text, kept and echoed in canonical text. An
amount has at most 12 fractional places (a rate takes 12 too), and a minimum fee is a multiple of the increment
of the price's currency scale. The binary links other gears, so this also shows that no other
gear changes how pricing reads a JSON number. The currency table endpoint returns the scale a new
price uses (D-537).
"""

import datetime
import uuid

import pytest

from .conftest import PRICING, PRODUCTS
from .test_pricebook_flow import _book, _key, _monthly_entry, _products_quorum_zero


@pytest.fixture
def entries(api) -> dict:
    """A published recurring SKU with a flat and a per-unit monthly entry in one EUR book."""
    run = uuid.uuid4().hex[:8]
    _products_quorum_zero(api)
    r = api.post(
        f"{PRODUCTS}/skus",
        json={"code": f"E2E-MONEY-{run}", "name": f"E2E money {run}", "type": "recurring"},
    )
    assert r.status_code == 201, r.text
    sku = r.json()["id"]
    r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
    assert r.status_code == 200, r.text
    assert r.json()["applied"] is True, r.text
    book = _book(api, "EUR", f"money-{run}")
    made = {model: _monthly_entry(api, book, sku, model) for model in ("flat", "per_unit")}
    assert [m.status_code for m in made.values()] == [201, 201], [m.text for m in made.values()]
    return {model: m.json()["id"] for model, m in made.items()}


def _draft(api, entry: str, body: dict):
    today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    return api.post(
        f"{PRICING}/price-book-entries/{entry}/prices",
        json={"eligibility": "all", "effective_from": today, **body},
        headers=_key(),
    )


def _violation(r) -> dict:
    assert r.status_code == 400, r.text
    [violation] = r.json()["context"]["field_violations"]
    return violation


def test_a_json_number_is_refused_for_the_price_and_the_fee(api, entries):
    v = _violation(_draft(api, entries["flat"], {"price": {"amount": 30.0}}))
    assert (v["field"], v["reason"]) == ("price", "AMOUNT_INVALID"), v
    assert "never JSON numbers" in v["description"], v
    v = _violation(
        _draft(api, entries["per_unit"], {"price": {"rate": "2.50"}, "min_fee": 1.5})
    )
    assert (v["field"], v["reason"]) == ("min_fee", "MIN_FEE_INVALID"), v
    assert "never JSON numbers" in v["description"], v


def test_a_price_keeps_twelve_places_exactly_and_refuses_a_thirteenth(api, entries):
    r = _draft(api, entries["flat"], {"price": {"amount": "30.123456789012"}})
    assert r.status_code == 201, r.text
    [price] = r.json()["items"]
    assert (price["price_json"], price["currency"], price["currency_scale"]) == (
        {"amount": "30.123456789012"},
        "EUR",
        2,
    ), price
    r = _draft(api, entries["per_unit"], {"price": {"rate": "0.000000000001"}})
    assert r.status_code == 201, r.text
    assert r.json()["items"][0]["price_json"] == {"rate": "0.000000000001"}, r.text
    v = _violation(_draft(api, entries["flat"], {"price": {"amount": "30.1234567890123"}}))
    assert (v["field"], v["reason"]) == ("price", "AMOUNT_OUT_OF_RANGE"), v


def test_decimal_text_is_plain_and_not_negative(api, entries):
    for amount, words in (("3e1", "no exponent"), ("-1", "must not be negative")):
        v = _violation(_draft(api, entries["flat"], {"price": {"amount": amount}}))
        assert (v["field"], v["reason"]) == ("price", "AMOUNT_INVALID"), v
        assert words in v["description"], v


def test_a_minimum_fee_is_a_multiple_of_the_currency_increment(api, entries):
    v = _violation(
        _draft(api, entries["per_unit"], {"price": {"rate": "2.50"}, "min_fee": "1.505"})
    )
    assert (v["field"], v["reason"]) == ("min_fee", "MIN_FEE_INVALID"), v
    r = _draft(api, entries["per_unit"], {"price": {"rate": "2.50"}, "min_fee": "1.50"})
    assert r.status_code == 201, r.text
    [price] = r.json()["items"]
    assert (price["price_json"], price["min_fee"], price["currency_scale"]) == (
        {"rate": "2.5"},
        "1.5",
        2,
    ), price


def test_the_currency_table_gives_the_scale_a_new_price_takes(api, entries):
    r = api.get(f"{PRICING}/currencies")
    assert r.status_code == 200, r.text
    items = r.json()["items"]
    codes = [c["code"] for c in items]
    assert codes == sorted(set(codes)), "each code once, in code order"
    assert sum(c["iso"] for c in items) == 165, "every active ISO 4217 code"
    table = {c["code"]: c for c in items}
    assert table["EUR"] == {"code": "EUR", "scale": 2, "iso": True}, table["EUR"]
    assert (table["JPY"]["scale"], table["KWD"]["scale"]) == (0, 3)
    assert "XTS" not in table, "ISO lists XTS without a scale"
    r = _draft(api, entries["flat"], {"price": {"amount": "30"}})
    assert r.status_code == 201, r.text
    [price] = r.json()["items"]
    assert (price["currency"], price["currency_scale"]) == ("EUR", table["EUR"]["scale"]), price
