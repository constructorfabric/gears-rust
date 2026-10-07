"""The PriceBook flow over HTTP, across bss-products and bss-pricing.

Products publishes a SKU; pricing prices it in a EUR book through the reference
reservation (reserve, write, confirm in products' in-process registry), approves
one draft price with quorum 0 and serves it in the book export; products then
refuses to retire the SKU while pricing's reference lives.

Two variants. ``usage`` is the metered SKU the programme names: it needs a
usage-type catalog, and products' catalog is the usage collector it links, which
answers only with a storage plugin. On a binary without one it skips with the
collector's own answer. ``recurring`` needs no catalog and always runs the flow.

Phase 5 on the same wire: the model is the entry's (D-427), so an entry is created with
``model`` and a price with money only; the entry reads carry ``usage`` (D-428); the SKU card and
the SKU list carry pricing's ``usage`` through the port pricing registers in the ClientHub
(P-D-197) — these suites are its only proof on the real binary; and a SKU may have no category
(P-D-196).

Phase 6: products' policy is written under If-Match (P-D-205), a never-published draft is
deleted (P-D-206), and the usage-type picker is products' own route (P-D-207). A SKU's history
reads the lifecycle moves its audit rows carry since migration 000008 (P-D-213), the version in
force has its own path (P-D-214), and a category reads alone with its SKU count (P-D-215). Pricing
says where a SKU is priced and sold (D-434), a kind's quorum override is reset in both gears
(D-435, P-D-216), dimension values are edited one at a time with their use (D-436), and the
settings offer currencies and say who wrote them, with five rounding modes (D-437, D-438).

Phase 9: a plan item is a SKU and its entry (D-467) and a new plan's code is upper case (D-468); the
plans list names each plan's current revision and the one in effect (D-460); a plan submit and a
publish-changes carry a note (D-464); both gears count their approval units and page them newest
first (D-470, P-D-227), and a unit says whether its reader may approve it (D-471, P-D-228); an
entry names its next price (D-472), and a book's entries list reads its prices on a date (D-473).

Archive: a finished book is archived and releases its entries' SKU references (D-522), so the SKU
retires and is archived in turn (P-D-263); both lists hide what is archived unless asked.
"""

import datetime
import uuid

import pytest

from .conftest import PRICING, PRODUCTS, USAGE_TYPE

USAGE_TYPES = "/usage-collector/v1/usage-types"


def _key() -> dict:
    return {"Idempotency-Key": str(uuid.uuid4())}


def _products_quorum_zero(api) -> None:
    """Products' default quorum 0, written at the policy's own ETag (P-D-205).

    A PUT without If-Match is refused 400 first: the policy write asserts the policy it read.
    """
    r = api.put(f"{PRODUCTS}/approval-policy", json={"quorum": 0})
    assert r.status_code == 400, r.text
    r = api.get(f"{PRODUCTS}/approval-policy")
    assert r.status_code == 200, r.text
    r = api.put(
        f"{PRODUCTS}/approval-policy",
        json={"quorum": 0},
        headers={"If-Match": r.headers["etag"]},
    )
    assert r.status_code == 200, r.text


def _usage_type_or_skip(api) -> None:
    r = api.post(
        USAGE_TYPES,
        json={"gts_id": USAGE_TYPE, "kind": "counter", "metadata_fields": []},
    )
    if r.status_code not in (201, 409):
        pytest.skip(
            "no usage-type catalog on this binary, so no usage SKU can publish: "
            f"POST {USAGE_TYPES} answered {r.status_code}: {r.text}"
        )


def _derived_usage(api) -> dict:
    """A usage SKU sells the identity wrapper of the collector's storage type (P-D-259).

    The raw type stays the wrapper's input. The entry names the derived meter, its
    output unit and the stored digest.
    """
    _usage_type_or_skip(api)
    code = "e2estorage"
    declaration = {
        "output_unit": "GB",
        "granularity": "hour",
        "inputs": [
            {
                "name": "disk",
                "usage_type_ref": USAGE_TYPE,
                "granule_fold": "sum",
                "unit": "GB",
            }
        ],
        "formula": {"op": "input", "name": "disk"},
        "output_scale": 0,
        "output_round": "half_even",
    }
    created = api.post(
        f"{PRODUCTS}/derived-usage-types",
        json={"code": code, "name": "E2E storage", "declaration": declaration},
    )
    if created.status_code == 409:
        created = api.get(f"{PRODUCTS}/derived-usage-types/{code}/versions/1")
    assert created.status_code in (200, 201), created.text
    body = created.json()
    meter = f"products.derived/{code}@1"
    digest = body["digest"]
    return {
        "sku": {"type": "usage", "usage_type_ref": meter, "unit": "GB"},
        "entry": {
            "model": "per_unit",
            # D-514: the create sends the two choices. The server fills fold, reset and partial_window.
            "usage_rating_policy": {
                "rating_window": {"kind": "billing_cycle"},
                "aggregation_scope": "subscription_line",
            },
        },
        "price": {"price": {"rate": "0.10"}},
    }


# D-427: the model is the entry's; a price carries only its money, in that model.
VARIANTS = {
    "recurring": {
        "sku": {"type": "recurring"},
        "entry": {"period": "month", "model": "flat"},
        "price": {"price": {"amount": "30.00"}},
    },
}


@pytest.mark.timeout(120)
@pytest.mark.parametrize("variant", ["usage", "recurring"])
def test_a_priced_sku_publishes_its_price_and_blocks_retirement(api, variant):
    shape = _derived_usage(api) if variant == "usage" else VARIANTS[variant]
    run = uuid.uuid4().hex[:8]

    # Products: quorum 0, a category and a SKU, published at submit.
    _products_quorum_zero(api)
    r = api.post(
        f"{PRODUCTS}/categories",
        json={"code": f"e2e-{variant}-{run}", "name": f"E2E {variant} {run}"},
    )
    assert r.status_code == 201, r.text
    category = r.json()["id"]
    r = api.post(
        f"{PRODUCTS}/skus",
        json={
            "code": f"E2E-{variant.upper()}-{run}",
            "name": f"E2E {variant} {run}",
            "category_id": category,
            **shape["sku"],
        },
    )
    assert r.status_code == 201, r.text
    sku = r.json()["id"]
    r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
    assert r.status_code == 200, r.text
    assert r.json()["applied"] is True, r.text

    # Pricing: a EUR book and quorum 0 under the policy's own ETag.
    r = api.post(
        f"{PRICING}/price-books",
        json={"code": f"eur-{variant}-{run}", "name": f"EUR {run}", "currency": "EUR"},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    book = r.json()["id"]
    r = api.get(f"{PRICING}/approval-policy")
    assert r.status_code == 200, r.text
    r = api.put(
        f"{PRICING}/approval-policy",
        json={"quorum": 0},
        headers={"If-Match": r.headers["etag"]},
    )
    assert r.status_code == 200, r.text

    # An entry for the SKU: reserved, written and confirmed in one call; the key replays.
    key = _key()
    body = {"sku_id": sku, **shape["entry"]}
    r = api.post(f"{PRICING}/price-books/{book}/entries", json=body, headers=key)
    assert r.status_code == 201, r.text
    entry = r.json()
    assert entry["sku_id"] == sku
    assert entry["charge_kind"] == variant
    assert entry["reference_state"] == "confirmed", entry
    if variant == "usage":
        content = entry["usage_rating_policy"]["content"]
        assert content["fold"] == "SUM", content
        assert "quantity_semantics" not in content, content
        assert entry["usage_sku_version"] == 1, entry
        assert content["reset"] == "rating_window_start", content
        assert content["partial_window"] == "actual_quantity_full_thresholds", content
    replay = api.post(f"{PRICING}/price-books/{book}/entries", json=body, headers=key)
    assert replay.status_code == 201, replay.text
    assert replay.json() == entry, "the same Idempotency-Key replays the receipt"

    # One draft price, published with quorum 0.
    start = (datetime.datetime.now(datetime.timezone.utc).date() + datetime.timedelta(days=30)).isoformat()
    r = api.post(
        f"{PRICING}/price-book-entries/{entry['id']}/prices",
        json={**shape["price"], "eligibility": "all", "effective_from": start},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    price = r.json()["items"][0]
    assert price["state"] == "draft", price
    r = api.post(f"{PRICING}/price-books/{book}/publish-changes", json={}, headers=_key())
    assert r.status_code == 201, r.text
    receipt = r.json()
    assert receipt["applied"] is True, receipt
    assert receipt["unit"]["state"] == "approved", receipt

    # The export serves the approved price.
    r = api.get(f"{PRICING}/price-books/{book}/export")
    assert r.status_code == 200, r.text
    export = r.json()
    assert export["book"]["id"] == book
    assert [e["entry"]["id"] for e in export["entries"]] == [entry["id"]]
    prices = export["entries"][0]["prices"]
    assert [(x["id"], x["state"], x["effective_from"], x["price_json"]) for x in prices] == [
        (price["id"], "approved", start, shape["price"]["price"])
    ], prices

    # D-428: the entry read counts its approved price, and no plan names it.
    assert _usage(api, entry["id"]) == _entry_usage(scheduled=1), entry

    # Products refuses to retire a SKU a live entry references.
    r = api.post(f"{PRODUCTS}/skus/{sku}/retire", json={})
    assert r.status_code == 409, r.text
    assert "SKU_REFERENCED" in r.text, r.text
    r = api.get(f"{PRODUCTS}/skus/{sku}")
    assert r.status_code == 200, r.text
    assert r.json()["sku"]["lifecycle"] == "published", r.text
    # P-D-197: the card carries pricing's usage through the port pricing registers.
    assert r.json()["usage"] == _sku_usage(1, ["EUR"], approved=1), r.text


def _entry_usage(
    scheduled: int = 0,
    active: int = 0,
    superseded: int = 0,
    pending: int = 0,
    draft: int = 0,
    plans: int = 0,
    superseded_only: int = 0,
) -> dict:
    """An entry read's ``usage`` (D-428): its approved prices by where their window stands
    today, ``approved`` their sum (D-440)."""
    return {
        "prices": {
            "approved": scheduled + active + superseded,
            "pending": pending,
            "draft": draft,
            "scheduled": scheduled,
            "active": active,
            "superseded": superseded,
        },
        "plans": plans,
        "plans_superseded_only": superseded_only,
    }


def _usage(api, entry: str) -> dict:
    """``GET /price-book-entries/{id}``'s ``usage``."""
    r = api.get(f"{PRICING}/price-book-entries/{entry}")
    assert r.status_code == 200, r.text
    return r.json()["usage"]


def _sku_usage(
    entries: int,
    currencies: list[str],
    approved: int = 0,
    pending: int = 0,
    draft: int = 0,
    plans: int = 0,
) -> dict:
    """A SKU read's ``usage`` (P-D-197), as pricing's port fills it."""
    return {
        "entries": entries,
        "currencies": currencies,
        "prices": {"approved": approved, "pending": pending, "draft": draft},
        "plans": plans,
    }


def _sku_reads(api, sku: str, code: str) -> tuple[dict, dict]:
    """The SKU card and the SKU's one row of the list (``q`` = its unique code)."""
    r = api.get(f"{PRODUCTS}/skus/{sku}")
    assert r.status_code == 200, r.text
    card = r.json()
    r = api.get(f"{PRODUCTS}/skus", params={"q": code})
    assert r.status_code == 200, r.text
    rows = [row for row in r.json()["items"] if row["id"] == sku]
    assert len(rows) == 1, r.text
    return card, rows[0]


def _usage_filtered(api, code: str, **flags: str) -> list[str]:
    """The ids the SKU list keeps for ``q`` = a unique code and the usage filters (P-D-212)."""
    r = api.get(f"{PRODUCTS}/skus", params={"q": code, **flags})
    assert r.status_code == 200, r.text
    return [row["id"] for row in r.json()["items"]]


def _policy(api) -> tuple[dict, str]:
    r = api.get(f"{PRICING}/approval-policy")
    assert r.status_code == 200, r.text
    return r.json(), r.headers["etag"]


def _set_quorum(api, kind: str, quorum: int) -> None:
    _, tag = _policy(api)
    r = api.put(
        f"{PRICING}/approval-policy",
        json={"kind": kind, "quorum": quorum},
        headers={"If-Match": tag},
    )
    assert r.status_code == 200, r.text


def _checks(api, revision: str) -> dict:
    r = api.get(f"{PRICING}/plan-revisions/{revision}/checks")
    assert r.status_code == 200, r.text
    return r.json()


def _check(checks: dict, code: str) -> dict:
    return next(c for c in checks["checks"] if c["code"] == code)


def _revision(api, revision: str) -> dict:
    r = api.get(f"{PRICING}/plan-revisions/{revision}")
    assert r.status_code == 200, r.text
    return r.json()


def _resolve(api, revision: str, date: str, pins: str | None = None) -> dict:
    params = {"plan_revision_id": revision, "date": date}
    if pins is not None:
        params["pins"] = pins
    r = api.get(f"{PRICING}/resolve", params=params)
    assert r.status_code == 200, r.text
    return r.json()


def _binding(resolved: dict) -> dict:
    """The default chain's binding of the one item."""
    [item] = resolved["items"]
    chain = item["chains"][0]
    assert chain["dim_value"] is None, resolved
    assert chain["uncovered"] is False, resolved
    return chain["binding"]


def _unit(client, gear: str, unit: str) -> dict:
    """One approval unit's card, as ``client`` reads it."""
    r = client.get(f"{gear}/approval-units/{unit}")
    assert r.status_code == 200, r.text
    return r.json()


def _plan_row(api, sku: str) -> dict:
    """The one plan selling ``sku``, as ``GET /plans`` lists it."""
    r = api.get(f"{PRICING}/plans", params={"sku_id": sku})
    assert r.status_code == 200, r.text
    [row] = r.json()["items"]
    return row


def _current(
    revision: str, rev_no: int, state: str, skus: list[str], author: str, book: dict | None = None
) -> dict:
    """A plan's ``current`` revision (D-460): the draft or pending one, else the scheduled one,
    else the published one in effect; its items' SKUs in ascending order; and its book's identity
    (D-485, ask 54), compared when the caller names it."""
    expected = {
        "revision_id": revision,
        "rev_no": rev_no,
        "state": state,
        "item_count": len(skus),
        "sku_ids": sorted(skus),
        "created_by": author,
    }
    if book is not None:
        expected["book"] = book
    return expected


def _current_of(row: dict) -> dict:
    """``current`` without its book identity and its author's name, for the assertions that name
    neither. The name is always there (D-519): a string, or null when it is not available now, as
    on every write answer."""
    current = row["current"]
    assert "created_by_name" in current, current
    return {k: v for k, v in current.items() if k not in ("book", "created_by_name")}


@pytest.mark.timeout(120)
def test_a_plan_blocked_by_a_pending_price_publishes_copies_and_clones(api, reviewer):
    """Spec §8's worked example across the two gears, then the revision's life.

    A price waits in a ``prices`` unit (quorum 1): a plan on the EUR book naming its entry is
    red, ``ITEM_UNCOVERED`` naming that unit; the reviewer approves it and the checks turn green;
    the revision publishes at once (``plan_revision`` quorum 0) and the consumer read contract
    serves it: ``GET /resolve`` binds the approved price, with the SKU version products holds on
    the date (read through the real in-process registry), and ``GET /prices/{id}`` serves that
    price; a copy is rev 2, its item attached, and publishing it supersedes rev 1, which still
    resolves, and a renewal pinned to the price binds it again; a clone is a new draft on the
    same book; the entry a plan names cannot be deleted. The pricing policy is restored at the
    end.

    Along the way the entry read's ``usage`` (D-428) follows the price through draft, pending
    and approved, and counts the plan once while its draft, published and copied revisions name
    the entry (the superseded rev 1 is history); the clone is a second plan. The SKU card and
    the SKU list carry the same counts through the port pricing fills (P-D-197).

    Phase 9: the price unit says that its submitter may not approve it and a fresh reviewer may,
    and no one once it is decided (D-471); the plan read, ``GET /plans`` and the create and clone
    answers name the current revision and the one in effect (D-460); rev 1's submit carries a
    note onto its unit, and rev 2's, sent without a body, none (D-464).
    """
    run = uuid.uuid4().hex[:8]
    before, _ = _policy(api)
    found = {
        kind: before["overrides"].get(kind, before["default_quorum"])
        for kind in ("prices", "plan_revision")
    }
    try:
        # Products: a published recurring SKU.
        _products_quorum_zero(api)
        r = api.post(
            f"{PRODUCTS}/categories",
            json={"code": f"e2e-plan-{run}", "name": f"E2E plan {run}"},
        )
        assert r.status_code == 201, r.text
        r = api.post(
            f"{PRODUCTS}/skus",
            json={
                "code": f"E2E-PLAN-{run}",
                "name": f"E2E plan {run}",
                "category_id": r.json()["id"],
                "type": "recurring",
            },
        )
        assert r.status_code == 201, r.text
        sku = r.json()["id"]
        r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is True, r.text

        # Pricing: a EUR book with a monthly entry for the SKU.
        r = api.post(
            f"{PRICING}/price-books",
            json={"code": f"eur-plan-{run}", "name": f"EUR plan {run}", "currency": "EUR"},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        book = r.json()["id"]
        r = api.post(
            f"{PRICING}/price-books/{book}/entries",
            json={"sku_id": sku, "period": "month", "model": "flat"},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        entry = r.json()["id"]
        # D-427, D-428: the entry read carries its model and a usage of zeros.
        r = api.get(f"{PRICING}/price-book-entries/{entry}")
        assert r.status_code == 200, r.text
        assert r.json()["model"] == "flat", r.text
        assert r.json()["usage"] == _entry_usage(), r.text

        # A draft price, submitted into a prices unit that waits for one reviewer.
        _set_quorum(api, "prices", 1)
        _set_quorum(api, "plan_revision", 0)
        # Priced and sold from today (UTC): a plan revision approved before its sale date waits
        # for that date (D-449), and this flow publishes each revision at once.
        start = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
        r = api.post(
            f"{PRICING}/price-book-entries/{entry}/prices",
            json={
                "price": {"amount": "30.00"},
                "eligibility": "all",
                "effective_from": start,
            },
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        price = r.json()["items"][0]["id"]
        assert _usage(api, entry) == _entry_usage(draft=1)
        r = api.post(f"{PRICING}/prices/{price}/submit", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is False, r.text
        unit = r.json()["unit"]["id"]
        assert _usage(api, entry) == _entry_usage(pending=1)
        # D-471: the submitter may not approve its own unit, on the receipt and on the card; a
        # fresh reviewer may.
        assert r.json()["unit"]["caller_can_approve"] is False, r.text
        assert _unit(api, PRICING, unit)["caller_can_approve"] is False
        assert _unit(reviewer, PRICING, unit)["caller_can_approve"] is True

        # A plan on the book, sold from the price's start, with the entry as a paid item: red.
        r = api.post(
            f"{PRICING}/plans",
            json={"code": f"PLAN-{run}".upper(), "name": f"Plan {run}", "book_id": book},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        plan = r.json()["id"]
        rev1 = r.json()["revisions"][0]["id"]
        # D-460: the create answers its empty draft as the plan's current revision, and no
        # revision is in effect yet.
        author = r.json()["created_by"]
        assert _current_of(r.json()) == _current(rev1, 1, "draft", [], author), r.text
        assert r.json()["in_effect"] is None, r.text
        r = api.get(f"{PRICING}/plan-revisions/{rev1}")
        assert r.status_code == 200, r.text
        r = api.patch(
            f"{PRICING}/plan-revisions/{rev1}",
            json={"available_from": start},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        r = api.post(
            f"{PRICING}/plan-revisions/{rev1}/items",
            json={"sku_id": sku, "price_book_entry_id": entry},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        assert r.json()["reference_state"] == "confirmed", r.text
        # D-467: a plan item is a SKU and its entry; the item answer carries no treatment.
        assert "treatment" not in r.json(), r.text
        # D-460: GET /plans names the draft as current, with its item's SKU.
        row = _plan_row(api, sku)
        assert row["id"] == plan, row
        assert _current_of(row) == _current(rev1, 1, "draft", [sku], author), row
        assert row["in_effect"] is None, row
        # A draft revision naming the entry counts its plan.
        assert _usage(api, entry) == _entry_usage(pending=1, plans=1)
        checks = _checks(api, rev1)
        assert checks["ready"] is False, checks
        assert checks["sale_date"] == start, checks
        uncovered = _check(checks, "ITEM_UNCOVERED")
        assert uncovered["ok"] is False, checks
        assert uncovered["blocked_by"] == [unit], checks

        # The reviewer approves the price unit: the checks turn green.
        r = reviewer.post(
            f"{PRICING}/approval-units/{unit}/approve",
            json={"generation": 1},
            headers=_key(),
        )
        assert r.status_code == 200, r.text
        assert r.json()["outcome"] == "applied", r.text
        # A decided unit is no one's to approve (D-471).
        assert r.json()["unit"]["caller_can_approve"] is False, r.text
        assert _unit(reviewer, PRICING, unit)["caller_can_approve"] is False
        checks = _checks(api, rev1)
        assert checks["ready"] is True, checks
        assert _check(checks, "ITEM_UNCOVERED")["blocked_by"] == [], checks
        assert _usage(api, entry) == _entry_usage(active=1, plans=1)

        # Quorum 0 for plan_revision: the submit publishes rev 1 at once. It carries the
        # submitter's note onto its unit (D-464).
        note = f"First sale of plan {run}: one monthly SKU"
        r = api.post(
            f"{PRICING}/plan-revisions/{rev1}/submit", json={"note": note}, headers=_key()
        )
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is True, r.text
        assert r.json()["revision"]["state"] == "published", r.text
        assert r.json()["unit"]["submit_note"] == note, r.text
        assert _unit(api, PRICING, r.json()["unit"]["id"])["submit_note"] == note
        r = api.get(f"{PRICING}/plans/{plan}")
        assert r.status_code == 200, r.text
        assert r.json()["published_rev"] == 1, r.text
        # D-460: the published rev 1 is current and in effect, on the read and on the list.
        published = _current(rev1, 1, "published", [sku], author)
        assert (_current_of(r.json()), r.json()["in_effect"]) == (
            published,
            {"revision_id": rev1, "rev_no": 1, "sku_ids": [sku]},
        ), r.text
        row = _plan_row(api, sku)
        assert (row["current"], row["in_effect"]) == (
            r.json()["current"],
            r.json()["in_effect"],
        ), row

        # The read contract: a signup on the sale date binds the approved price.
        resolved = _resolve(api, rev1, start)
        assert resolved["state"] == "published", resolved
        assert (resolved["plan_id"], resolved["rev_no"], resolved["book_id"]) == (plan, 1, book)
        assert (resolved["currency"], resolved["currency_minor_digits"]) == ("EUR", 2), resolved
        [item] = resolved["items"]
        assert (item["sku_id"], item["price_book_entry_id"]) == (sku, entry), item
        assert (item["charge_kind"], item["period"]) == ("recurring", "month"), item
        for removed in ("treatment", "included_qty", "qty_min"):
            assert removed not in item, item
        version = item["sku_version"]
        assert version is not None, "products answered the dated read: " + str(item)
        assert version["published_version"] >= 1, item
        assert version["effective_from"] <= start, item
        binding = _binding(resolved)
        assert binding["price_id"] == price, binding
        assert binding["dim_used"] is None, binding
        assert binding["pinned_from"] is None, binding
        # D-427: the model is the item's (its entry's); the binding carries the money.
        assert (item["model"], binding["price"]) == ("flat", {"amount": "30.00"}), binding
        assert (binding["eligibility"], binding["effective_from"]) == ("all", start), binding

        # The pinned price read serves that price, as stored.
        r = api.get(f"{PRICING}/prices/{price}")
        assert r.status_code == 200, r.text
        pinned = r.json()
        assert pinned["price_id"] == price, pinned
        assert (pinned["sku_id"], pinned["price_book_entry_id"], pinned["book_id"]) == (
            sku,
            entry,
            book,
        ), pinned
        assert (pinned["charge_kind"], pinned["period"], pinned["currency"]) == (
            "recurring",
            "month",
            "EUR",
        ), pinned
        assert (pinned["price"], pinned["effective_from"]) == ({"amount": "30.00"}, start), pinned
        assert "status" not in pinned and "version" not in pinned, pinned

        # A copy is rev 2 whose item attaches; publishing it supersedes rev 1.
        r = api.post(f"{PRICING}/plans/{plan}/revisions", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["rev_no"] == 2, r.text
        rev2 = r.json()["id"]
        copied = _revision(api, rev2)["items"]
        assert [(i["sku_id"], i["reference_state"]) for i in copied] == [
            (sku, "confirmed")
        ], copied
        # The published rev 1 and its draft copy name the entry: one plan.
        assert _usage(api, entry) == _entry_usage(active=1, plans=1)
        # D-460: the draft copy is current; rev 1 is still the one in effect.
        row = _plan_row(api, sku)
        assert _current_of(row) == _current(rev2, 2, "draft", [sku], author), row
        assert row["in_effect"] == {"revision_id": rev1, "rev_no": 1, "sku_ids": [sku]}, row
        # A submit without a body carries no note (D-464).
        r = api.post(f"{PRICING}/plan-revisions/{rev2}/submit", headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["revision"]["state"] == "published", r.text
        assert r.json()["unit"]["submit_note"] is None, r.text
        assert _revision(api, rev1)["state"] == "superseded"
        r = api.get(f"{PRICING}/plans/{plan}")
        assert r.json()["published_rev"] == 2, r.text
        assert (_current_of(r.json()), r.json()["in_effect"]) == (
            _current(rev2, 2, "published", [sku], author),
            {"revision_id": rev2, "rev_no": 2, "sku_ids": [sku]},
        ), r.text
        # Rev 1 superseded is history; rev 2 published still names the entry: still one plan.
        assert _usage(api, entry) == _entry_usage(active=1, plans=1)

        # The superseded rev 1 still resolves, and a renewal pinned to the price binds it again.
        superseded = _resolve(api, rev1, start)
        assert superseded["state"] == "superseded", superseded
        assert _binding(superseded)["price_id"] == price, superseded
        renewal = _binding(_resolve(api, rev2, start, pins=price))
        assert (renewal["price_id"], renewal["pinned_from"]) == (price, price), renewal

        # A clone is a new plan whose draft rev 1 reads the same book.
        r = api.post(
            f"{PRICING}/plans/{plan}/clone",
            json={"code": f"PLAN-{run}-CLONE".upper(), "name": f"Plan {run} clone"},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        clone = r.json()
        assert clone["id"] != plan, clone
        assert clone["published_rev"] is None, clone
        assert [x["state"] for x in clone["revisions"]] == ["draft"], clone
        # D-460: the clone answers its new draft, with the items it copied, and nothing in effect.
        assert _current_of(clone) == _current(
            clone["revisions"][0]["id"], 1, "draft", [sku], author
        ), clone
        assert clone["in_effect"] is None, clone
        draft = _revision(api, clone["revisions"][0]["id"])
        assert draft["book_id"] == book, draft
        assert [i["sku_id"] for i in draft["items"]] == [sku], draft

        # The clone's draft names the entry too: two plans, on the entry read and the book list.
        counted = _entry_usage(active=1, plans=2)
        assert _usage(api, entry) == counted
        r = api.get(f"{PRICING}/price-books/{book}/entries")
        assert r.status_code == 200, r.text
        assert [(e["id"], e["model"], e["usage"]) for e in r.json()["items"]] == [
            (entry, "flat", counted)
        ], r.text

        # The SKU card and the SKU list carry the same counts from pricing's port.
        card, row = _sku_reads(api, sku, f"E2E-PLAN-{run}")
        expected = _sku_usage(1, ["EUR"], approved=1, plans=2)
        assert card["usage"] == expected, card
        assert row["usage"] == expected, row

        # The entry a plan names cannot be deleted.
        r = api.delete(f"{PRICING}/price-book-entries/{entry}")
        assert r.status_code == 409, r.text
        assert "ENTRY_IN_USE" in r.text, r.text
    finally:
        for kind, quorum in found.items():
            _set_quorum(api, kind, quorum)


def _book(api, currency: str, run: str) -> str:
    r = api.post(
        f"{PRICING}/price-books",
        json={
            "code": f"{currency.lower()}-nocat-{run}",
            "name": f"{currency} no category {run}",
            "currency": currency,
        },
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    return r.json()["id"]


def _monthly_entry(api, book: str, sku: str, model: str):
    return api.post(
        f"{PRICING}/price-books/{book}/entries",
        json={"sku_id": sku, "period": "month", "model": model},
        headers=_key(),
    )


def _draft_price(api, entry: dict, money: dict, start: str) -> None:
    """A draft price carries only its money; its model is echoed from its entry (D-427)."""
    r = api.post(
        f"{PRICING}/price-book-entries/{entry['id']}/prices",
        json={"price": money, "eligibility": "all", "effective_from": start},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    price = r.json()["items"][0]
    assert (price["state"], price["model"], price["price_json"]) == (
        "draft",
        entry["model"],
        money,
    ), price


@pytest.mark.timeout(120)
def test_a_sku_without_a_category_is_priced_in_two_models_and_its_reads_carry_its_usage(api):
    """P-D-196, D-427 and D-428 / P-D-197 over HTTP, on one SKU.

    A SKU created without a category is 201 with ``category_id: null``, publishes and is priced.
    The entry door wants a model its charge kind allows, the entry PATCH does not take one, and
    a price carries none (money that does not fit its entry's model is ``PRICE_MISSING``).
    Its one SKU x charge kind x period takes two entries of different models in one EUR book
    (201 both; the same model again is 409 ``ENTRY_KEY_TAKEN``), and a third in a USD book. A
    plan publishes rev 1 with the flat entry, and its copy rev 2 re-points the item to the
    per-unit entry: the plan names both entries, each entry counts it and the SKU counts it
    once. Publishing rev 2 leaves the flat entry named only by the superseded rev 1
    (``plans_superseded_only``), and each revision resolves in its own entry's model. The SKU
    card and the SKU list carry the counts through the port pricing fills. The pricing policy
    is restored at the end.
    """
    run = uuid.uuid4().hex[:8]
    code = f"E2E-NOCAT-{run}"
    before, _ = _policy(api)
    found = {
        kind: before["overrides"].get(kind, before["default_quorum"])
        for kind in ("prices", "plan_revision")
    }
    try:
        # Products: a recurring SKU with no category, published at quorum 0.
        _products_quorum_zero(api)
        r = api.post(
            f"{PRODUCTS}/skus",
            json={"code": code, "name": f"E2E no category {run}", "type": "recurring"},
        )
        assert r.status_code == 201, r.text
        assert r.json()["category_id"] is None, r.text
        sku = r.json()["id"]
        r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is True, r.text
        published = r.json()["sku"]
        assert (published["lifecycle"], published["category_id"]) == ("published", None), r.text
        # A category filter never matches a SKU without one, and such a SKU never keeps a
        # category in use; browse serves it.
        r = api.post(
            f"{PRODUCTS}/categories",
            json={"code": f"e2e-nocat-{run}", "name": f"E2E no category {run}"},
        )
        assert r.status_code == 201, r.text
        unused = r.json()["id"]
        r = api.get(
            f"{PRODUCTS}/skus", params={"q": code, "$filter": f"category_id eq {unused}"}
        )
        assert r.status_code == 200, r.text
        assert r.json()["items"] == [], r.text
        # The list speaks OData (P-D-210): `category_id eq null` is "no category", the old
        # parameters are refused, and the page carries the toolkit's page_info.
        r = api.get(f"{PRODUCTS}/skus", params={"q": code, "$filter": "category_id eq null"})
        assert r.status_code == 200, r.text
        assert [row["id"] for row in r.json()["items"]] == [sku], r.text
        assert r.json()["page_info"]["limit"] == 50, r.text
        r = api.get(f"{PRODUCTS}/skus", params={"q": code, "category": unused})
        assert r.status_code == 400, r.text
        # Its tab counts narrow alike, and drop a top-level lifecycle term (P-D-211).
        r = api.get(
            f"{PRODUCTS}/skus/counts",
            params={"q": code, "$filter": "lifecycle eq 'draft' and category_id eq null"},
        )
        assert r.status_code == 200, r.text
        assert r.json() == {
            "all": 1,
            "draft": 0,
            "published": 1,
            "deprecated": 0,
            "retired": 0,
            "in_review": 0,
            # P-D-263: the counts carry the archived SKUs apart.
            "archived": 0,
        }, r.text
        r = api.post(f"{PRODUCTS}/categories/{unused}/retire", json={})
        assert r.status_code == 200, r.text
        assert r.json()["status"] == "retired", r.text
        r = api.get(f"{PRODUCTS}/browse", params={"kind": "sku", "$filter": f"entity_id eq {sku}"})
        assert r.status_code == 200, r.text
        assert [row["entity_id"] for row in r.json()["rows"]] == [sku], r.text

        _set_quorum(api, "prices", 0)
        _set_quorum(api, "plan_revision", 0)
        eur = _book(api, "EUR", run)
        usd = _book(api, "USD", run)

        # The entry create requires a model its charge kind allows (D-427).
        r = api.post(
            f"{PRICING}/price-books/{eur}/entries",
            json={"sku_id": sku, "period": "month"},
            headers=_key(),
        )
        assert r.status_code == 400, r.text
        for model, refusal in (
            ("stair", "MODEL_INVALID"),
            ("graduated", "MODEL_KIND_CHARGEKIND_MISMATCH"),
        ):
            r = _monthly_entry(api, eur, sku, model)
            assert r.status_code == 400, r.text
            assert refusal in r.text, r.text

        # Two models of one SKU x kind x period are two entries of one book (D-427).
        made = [_monthly_entry(api, eur, sku, model) for model in ("flat", "per_unit")]
        assert [m.status_code for m in made] == [201, 201], [m.text for m in made]
        flat, per_unit = (m.json() for m in made)
        assert flat["id"] != per_unit["id"], made
        assert [
            (e["sku_id"], e["charge_kind"], e["period"], e["model"], e["reference_state"])
            for e in (flat, per_unit)
        ] == [
            (sku, "recurring", "month", "flat", "confirmed"),
            (sku, "recurring", "month", "per_unit", "confirmed"),
        ], made
        again = _monthly_entry(api, eur, sku, "flat")
        assert again.status_code == 409, again.text
        assert "ENTRY_KEY_TAKEN" in again.text, again.text
        # The model is fixed for the entry's life: the PATCH does not carry it.
        r = api.get(f"{PRICING}/price-book-entries/{flat['id']}")
        assert r.status_code == 200, r.text
        r = api.patch(
            f"{PRICING}/price-book-entries/{flat['id']}",
            json={"model": "per_unit"},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 400, r.text
        r = api.get(f"{PRICING}/price-book-entries/{flat['id']}")
        assert (r.json()["model"], r.json()["version"]) == ("flat", flat["version"]), r.text
        r = _monthly_entry(api, usd, sku, "flat")
        assert r.status_code == 201, r.text
        dollar = r.json()

        # A price carries no model, and its money must fit its entry's.
        # Priced and sold from today (UTC): a plan revision approved before its sale date waits
        # for that date (D-449), and this flow publishes each revision at once.
        start = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
        for body, refusal in (
            ({"model": "flat", "price": {"amount": "30.00"}}, None),
            ({"price": {"rate": "2.50"}}, "PRICE_MISSING"),
        ):
            r = api.post(
                f"{PRICING}/price-book-entries/{flat['id']}/prices",
                json={**body, "eligibility": "all", "effective_from": start},
                headers=_key(),
            )
            assert r.status_code == 400, r.text
            assert refusal is None or refusal in r.text, r.text

        # Each entry's price in its model; the EUR pair approved, the USD price left a draft.
        _draft_price(api, flat, {"amount": "30.00"}, start)
        _draft_price(api, per_unit, {"rate": "2.50"}, start)
        _draft_price(api, dollar, {"amount": "33.00"}, start)
        r = api.post(f"{PRICING}/price-books/{eur}/publish-changes", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is True, r.text
        r = api.get(f"{PRICING}/price-books/{eur}/entries")
        assert r.status_code == 200, r.text
        assert {e["id"]: (e["model"], e["usage"]) for e in r.json()["items"]} == {
            flat["id"]: ("flat", _entry_usage(active=1)),
            per_unit["id"]: ("per_unit", _entry_usage(active=1)),
        }, r.text

        # The SKU reads: three entries in two currencies, prices by state, no plan yet.
        card, row = _sku_reads(api, sku, code)
        assert (card["sku"]["category_id"], row["category_id"]) == (None, None), (card, row)
        unplanned = _sku_usage(3, ["EUR", "USD"], approved=2, draft=1)
        assert (card["usage"], row["usage"]) == (unplanned, unplanned), (card, row)
        # The list filters on the same facts (P-D-212): priced, in no plan yet.
        assert _usage_filtered(api, code, priced="true") == [sku]
        assert _usage_filtered(api, code, priced="false") == []
        assert _usage_filtered(api, code, in_plan="true") == []
        assert _usage_filtered(api, code, in_plan="false") == [sku]
        r = api.get(
            f"{PRODUCTS}/skus/counts", params={"q": code, "priced": "true", "in_plan": "false"}
        )
        assert r.status_code == 200, r.text
        assert (r.json()["all"], r.json()["published"]) == (1, 1), r.text

        # A plan publishes rev 1 with the flat entry.
        r = api.post(
            f"{PRICING}/plans",
            json={
                "code": f"PLAN-NOCAT-{run}".upper(),
                "name": f"Plan no category {run}",
                "book_id": eur,
            },
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        plan = r.json()["id"]
        rev1 = r.json()["revisions"][0]["id"]
        r = api.get(f"{PRICING}/plan-revisions/{rev1}")
        assert r.status_code == 200, r.text
        r = api.patch(
            f"{PRICING}/plan-revisions/{rev1}",
            json={"available_from": start},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        r = api.post(
            f"{PRICING}/plan-revisions/{rev1}/items",
            json={"sku_id": sku, "price_book_entry_id": flat["id"]},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        r = api.post(f"{PRICING}/plan-revisions/{rev1}/submit", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["revision"]["state"] == "published", r.text

        # Its copy rev 2 picks the per-unit entry: the item keeps its SKU, changes its entry.
        r = api.post(f"{PRICING}/plans/{plan}/revisions", json={}, headers=_key())
        assert r.status_code == 201, r.text
        rev2 = r.json()["id"]
        [item] = _revision(api, rev2)["items"]
        assert (item["sku_id"], item["price_book_entry_id"]) == (sku, flat["id"]), item
        r = api.patch(
            f"{PRICING}/plan-items/{item['id']}",
            json={"price_book_entry_id": per_unit["id"]},
            headers={"If-Match": f'"{item["version"]}"'},
        )
        assert r.status_code == 200, r.text
        assert r.json()["price_book_entry_id"] == per_unit["id"], r.text

        # The plan names both entries: each entry counts it, the SKU counts it once.
        assert _usage(api, flat["id"]) == _entry_usage(active=1, plans=1)
        assert _usage(api, per_unit["id"]) == _entry_usage(active=1, plans=1)
        assert _usage(api, dollar["id"]) == _entry_usage(draft=1)
        planned = _sku_usage(3, ["EUR", "USD"], approved=2, draft=1, plans=1)
        card, row = _sku_reads(api, sku, code)
        assert (card["usage"], row["usage"]) == (planned, planned), (card, row)
        assert _usage_filtered(api, code, in_plan="true") == [sku]
        assert _usage_filtered(api, code, priced="true", in_plan="false") == []

        # Rev 2 published supersedes rev 1: the flat entry is named by history only.
        r = api.post(f"{PRICING}/plan-revisions/{rev2}/submit", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["revision"]["state"] == "published", r.text
        assert _revision(api, rev1)["state"] == "superseded"
        assert _usage(api, flat["id"]) == _entry_usage(active=1, superseded_only=1)
        assert _usage(api, per_unit["id"]) == _entry_usage(active=1, plans=1)
        card, row = _sku_reads(api, sku, code)
        assert (card["usage"], row["usage"]) == (planned, planned), (card, row)

        # Each revision resolves in its own entry's model and money.
        for revision, entry, model, money in (
            (rev1, flat, "flat", {"amount": "30.00"}),
            (rev2, per_unit, "per_unit", {"rate": "2.50"}),
        ):
            resolved = _resolve(api, revision, start)
            [resolved_item] = resolved["items"]
            assert (resolved_item["price_book_entry_id"], resolved_item["model"]) == (
                entry["id"],
                model,
            ), resolved
            assert _binding(resolved)["price"] == money, resolved
    finally:
        for kind, quorum in found.items():
            _set_quorum(api, kind, quorum)


@pytest.mark.timeout(60)
def test_a_never_published_draft_is_deleted_and_the_picker_is_served(api):
    """P-D-206 and P-D-207 on the real binary.

    A draft is deleted by its author under If-Match (204), and its card is 404 after. The
    usage-type picker is products' own route: 200 with the catalog's provenance, or 503 when the
    linked collector has no storage to answer from; never a 404.
    """
    run = uuid.uuid4().hex[:8]
    r = api.post(
        f"{PRODUCTS}/skus",
        json={"code": f"E2E-DRAFT-{run}", "name": f"E2E draft {run}", "type": "recurring"},
    )
    assert r.status_code == 201, r.text
    sku = r.json()["id"]
    r = api.delete(f"{PRODUCTS}/skus/{sku}")
    assert r.status_code == 400, r.text
    r = api.get(f"{PRODUCTS}/skus/{sku}")
    assert r.status_code == 200, r.text
    r = api.delete(f"{PRODUCTS}/skus/{sku}", headers={"If-Match": r.headers["etag"]})
    assert r.status_code == 204, r.text
    assert api.get(f"{PRODUCTS}/skus/{sku}").status_code == 404

    r = api.get(f"{PRODUCTS}/usage-types", params={"limit": 5})
    assert r.status_code in (200, 503), r.text
    if r.status_code == 200:
        page = r.json()
        assert page["source"] == "usage_collector", page
        assert isinstance(page["items"], list), page
        assert "next_cursor" in page["page_info"], page


@pytest.mark.timeout(60)
def test_a_skus_history_its_version_in_force_and_its_category_read(api):
    """P-D-213, P-D-214 and P-D-215 on the real binary, whose chain ran migration 000008.

    A SKU published at quorum 0 reads its history oldest first with each act's lifecycle move and
    unit, walked one entry at a time across the submit and apply that share an instant; its
    versions are an array and the version in force has its own path; its category reads alone
    with an ETag and a SKU count, and the category list pages on OData.
    """
    run = uuid.uuid4().hex[:8]
    _products_quorum_zero(api)
    r = api.post(
        f"{PRODUCTS}/categories",
        json={"code": f"e2e-hist-{run}", "name": f"E2E history {run}", "sort_order": 7},
    )
    assert r.status_code == 201, r.text
    category = r.json()["id"]
    r = api.post(
        f"{PRODUCTS}/skus",
        json={
            "code": f"E2E-HIST-{run}",
            "name": f"E2E history {run}",
            "type": "recurring",
            "category_id": category,
        },
    )
    assert r.status_code == 201, r.text
    sku = r.json()["id"]
    r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
    assert r.status_code == 200, r.text
    assert r.json()["applied"] is True, r.text
    unit = r.json()["unit"]["id"]

    r = api.get(f"{PRODUCTS}/skus/{sku}/history")
    assert r.status_code == 200, r.text
    entries = r.json()["items"]
    assert [
        (e["action"], e["from_lifecycle"], e["to_lifecycle"], e["unit_id"], e["unit_kind"])
        for e in entries
    ] == [
        ("sku.create", None, "draft", None, None),
        ("approval.submit", "draft", "draft", unit, "sku_publish"),
        ("approval.applied", "draft", "published", unit, "sku_publish"),
    ], r.text
    assert entries[1]["at"] == entries[2]["at"], r.text
    walked, cursor = [], None
    while True:
        params = {"limit": 1} if cursor is None else {"limit": 1, "cursor": cursor}
        r = api.get(f"{PRODUCTS}/skus/{sku}/history", params=params)
        assert r.status_code == 200, r.text
        walked.extend(r.json()["items"])
        cursor = r.json()["page_info"]["next_cursor"]
        if cursor is None:
            break
    assert walked == entries, walked
    r = api.get(f"{PRODUCTS}/skus/{sku}/history", params={"$orderby": "at desc"})
    assert r.status_code == 400, r.text

    today = datetime.datetime.now(datetime.timezone.utc).date()
    r = api.get(f"{PRODUCTS}/skus/{sku}/versions")
    assert r.status_code == 200, r.text
    assert [v["published_version"] for v in r.json()] == [1], r.text
    r = api.get(f"{PRODUCTS}/skus/{sku}/versions", params={"as_of": today.isoformat()})
    assert r.status_code == 400, r.text
    r = api.get(f"{PRODUCTS}/skus/{sku}/versions/as-of", params={"date": today.isoformat()})
    assert r.status_code == 200, r.text
    assert r.json()["published_version"] == 1, r.text
    before = (today - datetime.timedelta(days=1)).isoformat()
    r = api.get(f"{PRODUCTS}/skus/{sku}/versions/as-of", params={"date": before})
    assert r.status_code == 404, r.text
    assert "NO_VERSION_IN_FORCE" in r.text, r.text

    r = api.get(f"{PRODUCTS}/categories/{category}")
    assert r.status_code == 200, r.text
    assert r.headers["etag"] == '"1"', r.headers
    assert (r.json()["code"], r.json()["sku_count"]) == (f"e2e-hist-{run}", 1), r.text
    r = api.get(
        f"{PRODUCTS}/categories", params={"$filter": f"code eq 'e2e-hist-{run}'"}
    )
    assert r.status_code == 200, r.text
    assert [(c["id"], c["sku_count"]) for c in r.json()["items"]] == [(category, 1)], r.text
    assert r.json()["page_info"]["limit"] == 200, r.text



def _settings(api) -> tuple[dict, str]:
    r = api.get(f"{PRICING}/settings")
    assert r.status_code == 200, r.text
    return r.json(), r.headers["etag"]


def _settings_body(read: dict) -> dict:
    """A settings read as a PUT body: without what the read adds (D-438)."""
    return {k: v for k, v in read.items() if k not in ("version", "updated_at", "updated_by")}


@pytest.mark.timeout(120)
def test_where_a_sku_is_priced_and_sold_and_the_settings_offer_currencies(api):
    """D-434 to D-438 and P-D-216 on the real binary, whose chain ran migration 000014.

    The settings PUT requires ``currencies``, knows five rounding modes and says who wrote it; a
    new book outside the offered currencies is 409. A kind's quorum override is reset by DELETE in
    both gears, and the default is never deleted. A SKU priced today in a EUR book and sold by a
    published plan reads where it is priced (its entry with its book, usage and the price in force)
    and where it is sold (the plan, and the plan item alone). The Price Books screen's reads
    (D-440 to D-442): the entry's prices with their status, the entry read's price in force, and
    its book in the book list (a page with ``page_info``, found by ``q`` and by ``sku_id``) with
    its stats. A dimension key's values are edited one at a time and each value carries its use.
    The settings and both policies are restored.
    """
    run = uuid.uuid4().hex[:8]
    code = f"E2E-SOLD-{run}"
    settings_before, _ = _settings(api)
    policy_before, _ = _policy(api)
    try:
        # The settings: currencies required, five rounding modes, the writer stamped.
        read, tag = _settings(api)
        body = _settings_body(read)
        without = {k: v for k, v in body.items() if k != "currencies"}
        r = api.put(f"{PRICING}/settings", json=without, headers={"If-Match": tag})
        assert r.status_code == 400, r.text
        r = api.put(
            f"{PRICING}/settings",
            json={**body, "default_rounding": "bankers", "currencies": []},
            headers={"If-Match": tag},
        )
        assert r.status_code == 400, r.text
        assert "ROUNDING_INVALID" in r.text, r.text
        r = api.put(
            f"{PRICING}/settings",
            json={**body, "default_rounding": "half_even", "currencies": ["EUR", "USD"]},
            headers={"If-Match": tag},
        )
        assert r.status_code == 200, r.text
        saved = r.json()
        assert (saved["currencies"], saved["default_rounding"]) == (["EUR", "USD"], "half_even")
        assert uuid.UUID(saved["updated_by"]) and saved["updated_at"], saved
        r = api.post(
            f"{PRICING}/price-books",
            json={"code": f"gbp-sold-{run}", "name": f"GBP {run}", "currency": "GBP"},
            headers=_key(),
        )
        assert r.status_code == 409, r.text
        assert "CURRENCY_NOT_OFFERED" in r.text, r.text

        # Products: an override reset by DELETE; the default is never deleted (P-D-216).
        _products_quorum_zero(api)
        r = api.get(f"{PRODUCTS}/approval-policy")
        r = api.put(
            f"{PRODUCTS}/approval-policy",
            json={"kind": "sku_retire", "quorum": 3},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        tag = r.headers["etag"]
        r = api.delete(f"{PRODUCTS}/approval-policy/*", headers={"If-Match": tag})
        assert r.status_code == 400, r.text
        assert "POLICY_DEFAULT_REQUIRED" in r.text, r.text
        r = api.delete(f"{PRODUCTS}/approval-policy/sku_retire", headers={"If-Match": tag})
        assert r.status_code == 200, r.text
        assert "sku_retire" not in r.json()["overrides"], r.text

        # Pricing: quorum 0 for both kinds while the SKU is priced and sold.
        _set_quorum(api, "prices", 0)
        _set_quorum(api, "plan_revision", 0)
        _, tag = _policy(api)
        r = api.delete(f"{PRICING}/approval-policy/*", headers={"If-Match": tag})
        assert r.status_code == 400, r.text
        assert "POLICY_DEFAULT_REQUIRED" in r.text, r.text

        # A recurring SKU, priced from today in a EUR book and sold by a published plan.
        r = api.post(
            f"{PRODUCTS}/skus",
            json={"code": code, "name": f"E2E sold {run}", "type": "recurring"},
        )
        assert r.status_code == 201, r.text
        sku = r.json()["id"]
        r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
        assert r.status_code == 200, r.text
        eur = _book(api, "EUR", run)
        r = _monthly_entry(api, eur, sku, "flat")
        assert r.status_code == 201, r.text
        entry = r.json()
        today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
        _draft_price(api, entry, {"amount": "30.00"}, today)
        r = api.post(f"{PRICING}/price-books/{eur}/publish-changes", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is True, r.text
        r = api.post(
            f"{PRICING}/plans",
            json={"code": f"PLAN-SOLD-{run}".upper(), "name": f"Plan sold {run}", "book_id": eur},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        plan = r.json()["id"]
        rev1 = r.json()["revisions"][0]["id"]
        r = api.post(
            f"{PRICING}/plan-revisions/{rev1}/items",
            json={"sku_id": sku, "price_book_entry_id": entry["id"]},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        item = r.json()["id"]
        r = api.post(f"{PRICING}/plan-revisions/{rev1}/submit", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["revision"]["state"] == "published", r.text

        # Where it is priced (D-434): the entry, its book, its usage and the price in force.
        r = api.get(f"{PRICING}/price-book-entries")
        assert r.status_code == 400, r.text
        r = api.get(f"{PRICING}/price-book-entries", params={"sku_id": sku})
        assert r.status_code == 200, r.text
        [row] = r.json()["items"]
        assert (row["id"], row["book_id"], row["currency"], row["model"]) == (
            entry["id"],
            eur,
            "EUR",
            "flat",
        ), row
        assert row["usage"] == _entry_usage(active=1, plans=1), row
        assert row["current_price"]["price_json"] == {"amount": "30.00"}, row
        assert row["current_price"]["status"] == "active", row
        # The Price Books screen (D-440): the entry's prices with their status today, and the
        # entry read's price in force and dated counts.
        r = api.get(f"{PRICING}/price-book-entries/{entry['id']}/prices")
        assert r.status_code == 200, r.text
        [price] = r.json()["items"]
        assert (price["id"], price["status"]) == (row["current_price"]["id"], "active"), r.text
        r = api.get(
            f"{PRICING}/price-book-entries/{entry['id']}/prices",
            params={"status": "scheduled,draft"},
        )
        assert (r.status_code, r.json()["items"]) == (200, []), r.text
        r = api.get(
            f"{PRICING}/price-book-entries/{entry['id']}/prices", params={"status": "live"}
        )
        assert r.status_code == 400, r.text
        assert "QUERY_INVALID" in r.text, r.text
        r = api.get(f"{PRICING}/price-book-entries/{entry['id']}")
        assert r.status_code == 200, r.text
        assert r.json()["usage"] == row["usage"], r.text
        assert r.json()["current_price"] == row["current_price"], r.text
        # The book list (D-442) is a page: its book by q and by sku_id, with its stats (D-441).
        stats = {
            "entries": 1,
            "skus": 1,
            "plans": 1,
            "plans_superseded_only": 0,
            "prices": {
                "draft": 0,
                "pending": 0,
                "approved": 1,
                "scheduled": 0,
                "active": 1,
                "superseded": 0,
                "rejected": 0,
            },
            "pending_units": 0,
        }
        for params in ({"q": f"eur-nocat-{run}".upper()}, {"sku_id": sku}):
            r = api.get(f"{PRICING}/price-books", params=params)
            assert r.status_code == 200, r.text
            page = r.json()
            assert [b["id"] for b in page["items"]] == [eur], page
            assert page["page_info"]["limit"] == 200, page
            [book] = page["items"]
            assert {k: v for k, v in book["stats"].items() if k != "last_change_at"} == stats, book
        r = api.get(f"{PRICING}/price-books/{eur}")
        assert r.status_code == 200, r.text
        assert r.json()["stats"] == book["stats"], r.text
        r = api.get(f"{PRICING}/price-books", params={"book": eur})
        assert r.status_code == 400, r.text
        # Where it is sold: the plan, in GET /plans' shape, and the plan item alone.
        r = api.get(f"{PRICING}/plans", params={"sku_id": sku})
        assert r.status_code == 200, r.text
        assert [p["id"] for p in r.json()["items"]] == [plan], r.text
        r = api.get(f"{PRICING}/plan-items/{item}")
        assert r.status_code == 200, r.text
        assert (r.json()["plan_id"], r.json()["rev_no"], r.json()["state"]) == (
            plan,
            1,
            "published",
        ), r.text
        assert r.headers["etag"] == f'"{r.json()["version"]}"', r.headers

        # Dimension values one at a time, each with its use (D-436).
        r = api.get(f"{PRICING}/dimension-keys")
        assert r.status_code == 200, r.text
        region = next(k for k in r.json()["items"] if k["key"] == "region")
        assert all(set(v) == {"value", "usage"} for v in region["values"]), r.text
        added = [f"e2e-{run}-a", f"e2e-{run}-b"]
        r = api.patch(
            f"{PRICING}/dimension-keys",
            json={"key": "region", "add": added},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        region = next(k for k in r.json()["items"] if k["key"] == "region")
        assert [v["value"] for v in region["values"]][-2:] == added, r.text
        assert all(v["usage"] == {"prices": 0} for v in region["values"][-2:]), r.text
        r = api.patch(
            f"{PRICING}/dimension-keys",
            json={"key": "region", "remove": added},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        region = next(k for k in r.json()["items"] if k["key"] == "region")
        assert not set(added) & {v["value"] for v in region["values"]}, r.text
    finally:
        # The pricing policy as it was: an override that was not there is reset (D-435).
        for kind in ("prices", "plan_revision"):
            if kind in policy_before["overrides"]:
                _set_quorum(api, kind, policy_before["overrides"][kind])
            else:
                _, tag = _policy(api)
                r = api.delete(f"{PRICING}/approval-policy/{kind}", headers={"If-Match": tag})
                assert r.status_code in (200, 404), r.text
        read, tag = _settings(api)
        restored = {**_settings_body(settings_before), "currencies": settings_before["currencies"]}
        r = api.put(f"{PRICING}/settings", json=restored, headers={"If-Match": tag})
        assert r.status_code == 200, r.text


def _counts(api, gear: str, **narrowing: str) -> dict:
    r = api.get(f"{gear}/approval-units/counts", params=narrowing)
    assert r.status_code == 200, r.text
    return r.json()


def _unit_ids(client, gear: str, **params) -> list[str]:
    r = client.get(f"{gear}/approval-units", params=params)
    assert r.status_code == 200, r.text
    return [u["id"] for u in r.json()["items"]]


def _newest_first_by_pages(client, gear: str, **narrowing: str) -> list[str]:
    """The narrowed list, newest first, one unit per page: the cursor carries its order."""
    r = client.get(
        f"{gear}/approval-units",
        params={**narrowing, "$orderby": "submitted_at desc", "limit": 1},
    )
    assert r.status_code == 200, r.text
    walked = [u["id"] for u in r.json()["items"]]
    cursor = r.json()["page_info"]["next_cursor"]
    while cursor is not None:
        r = client.get(f"{gear}/approval-units", params={**narrowing, "cursor": cursor})
        assert r.status_code == 200, r.text
        walked.extend(u["id"] for u in r.json()["items"])
        cursor = r.json()["page_info"]["next_cursor"]
    return walked


def _headline(entry: dict) -> tuple:
    """An entry read's price in force and next price (D-472): (id, status) or None each."""
    return tuple(
        None if p is None else (p["id"], p["status"])
        for p in (entry["current_price"], entry["next_price"])
    )


def _entry_reads(api, book: str, sku: str, entry: str, as_of: str | None = None) -> dict:
    """The entry as the book's list reads it on ``as_of`` (D-473). Without a date the single
    read and the SKU's entry list, both dated today, answer the same prices (D-472)."""
    params = {} if as_of is None else {"as_of": as_of}
    r = api.get(f"{PRICING}/price-books/{book}/entries", params=params)
    assert r.status_code == 200, r.text
    [listed] = r.json()["items"]
    assert listed["id"] == entry, r.text
    if as_of is None:
        r = api.get(f"{PRICING}/price-book-entries/{entry}")
        assert r.status_code == 200, r.text
        assert _headline(r.json()) == _headline(listed), r.text
        r = api.get(f"{PRICING}/price-book-entries", params={"sku_id": sku})
        assert r.status_code == 200, r.text
        [row] = r.json()["items"]
        assert _headline(row) == _headline(listed), r.text
    return listed


@pytest.mark.timeout(120)
def test_an_entry_names_its_next_price_and_its_book_reads_its_prices_on_a_date(api, reviewer):
    """D-470, D-472, D-473 and D-464 on the real binary, on one entry of one EUR book.

    A price from today waits in a ``prices`` unit (quorum 1): the entry has no price in force,
    and its next price is the pending one. The reviewer approves it, and a second price, from
    today + 30, is first the next draft, then the next pending price once publish-changes puts it
    into a second unit with the submitter's note. The book's units are counted by state and kind
    and page newest first, also one per page through the cursor; the counts take nothing but the
    narrowing. Once the reviewer approves the second unit, the entry names the scheduled successor
    as its next price on all three entry reads. The book's entries list read on a date answers the
    prices of that day: before both prices, and on the successor's start. A query key it does not
    know and a date that does not read are refused. The pricing policy is restored at the end.
    """
    run = uuid.uuid4().hex[:8]
    before, _ = _policy(api)
    found = {
        kind: before["overrides"].get(kind, before["default_quorum"])
        for kind in ("prices", "plan_revision")
    }
    try:
        # Products: a published recurring SKU. Pricing: a EUR book with its monthly entry.
        _products_quorum_zero(api)
        r = api.post(
            f"{PRODUCTS}/skus",
            json={"code": f"E2E-NEXT-{run}", "name": f"E2E next price {run}", "type": "recurring"},
        )
        assert r.status_code == 201, r.text
        sku = r.json()["id"]
        r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is True, r.text
        book = _book(api, "EUR", run)
        r = _monthly_entry(api, book, sku, "flat")
        assert r.status_code == 201, r.text
        entry = r.json()
        assert _headline(_entry_reads(api, book, sku, entry["id"])) == (None, None)

        # A price from today, submitted into a unit that waits for one reviewer: it is next.
        _set_quorum(api, "prices", 1)
        today = datetime.datetime.now(datetime.timezone.utc).date()
        r = api.post(
            f"{PRICING}/price-book-entries/{entry['id']}/prices",
            json={
                "price": {"amount": "30.00"},
                "eligibility": "all",
                "effective_from": today.isoformat(),
            },
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        first = r.json()["items"][0]["id"]
        r = api.post(f"{PRICING}/prices/{first}/submit", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is False, r.text
        unit1 = r.json()["unit"]["id"]
        assert _headline(_entry_reads(api, book, sku, entry["id"])) == (
            None,
            (first, "pending"),
        )
        r = reviewer.post(
            f"{PRICING}/approval-units/{unit1}/approve", json={"generation": 1}, headers=_key()
        )
        assert r.status_code == 200, r.text
        assert r.json()["outcome"] == "applied", r.text
        assert _headline(_entry_reads(api, book, sku, entry["id"])) == ((first, "active"), None)

        # Its successor from today + 30: the next draft, then the next pending price, submitted
        # by publish-changes with a note for the approver (D-464).
        later = (today + datetime.timedelta(days=30)).isoformat()
        r = api.post(
            f"{PRICING}/price-book-entries/{entry['id']}/prices",
            json={"price": {"amount": "35.00"}, "eligibility": "all", "effective_from": later},
            headers=_key(),
        )
        assert r.status_code == 201, r.text
        second = r.json()["items"][0]["id"]
        assert _headline(_entry_reads(api, book, sku, entry["id"])) == (
            (first, "active"),
            (second, "draft"),
        )
        note = f"Next year's price for {run}"
        r = api.post(
            f"{PRICING}/price-books/{book}/publish-changes", json={"note": note}, headers=_key()
        )
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is False, r.text
        unit2 = r.json()["unit"]["id"]
        assert r.json()["unit"]["submit_note"] == note, r.text
        assert _unit(reviewer, PRICING, unit2)["submit_note"] == note
        assert _headline(_entry_reads(api, book, sku, entry["id"])) == (
            (first, "active"),
            (second, "pending"),
        )

        # D-470: the book's units counted by state and kind, and paged newest first.
        assert _counts(api, PRICING, book_id=book) == {
            "by_state": {"pending": 1, "approved": 1, "rejected": 0, "withdrawn": 0},
            "by_kind": {"prices": 2, "plan_revision": 0},
            "total": 2,
        }
        assert _counts(api, PRICING, book_id=book, state="pending")["total"] == 1
        assert _counts(api, PRICING, ref_id=book, kind="plan_revision")["total"] == 0
        r = api.get(f"{PRICING}/approval-units/counts", params={"book_id": book, "limit": 1})
        assert r.status_code == 400, r.text
        assert "QUERY_INVALID" in r.text, r.text
        assert _unit_ids(api, PRICING, book_id=book) == [unit1, unit2]
        assert _unit_ids(api, PRICING, book_id=book, **{"$orderby": "submitted_at desc"}) == [
            unit2,
            unit1,
        ]
        assert _newest_first_by_pages(api, PRICING, book_id=book) == [unit2, unit1]
        r = api.get(
            f"{PRICING}/approval-units",
            params={"book_id": book, "$orderby": "submitted_at desc", "impact": "false"},
        )
        assert r.status_code == 200, r.text
        assert [(u["id"], u["impact"]) for u in r.json()["items"]] == [
            (unit2, None),
            (unit1, None),
        ], r.text
        r = api.get(
            f"{PRICING}/approval-units", params={"book_id": book, "$orderby": "state desc"}
        )
        assert r.status_code == 400, r.text

        # The reviewer approves it: the successor is scheduled, and it is the next price.
        r = reviewer.post(
            f"{PRICING}/approval-units/{unit2}/approve", json={"generation": 1}, headers=_key()
        )
        assert r.status_code == 200, r.text
        assert r.json()["outcome"] == "applied", r.text
        assert _counts(api, PRICING, book_id=book)["by_state"]["approved"] == 2
        listed = _entry_reads(api, book, sku, entry["id"])
        assert _headline(listed) == ((first, "active"), (second, "scheduled")), listed
        assert listed["next_price"]["effective_from"] == later, listed
        assert listed["usage"] == _entry_usage(scheduled=1, active=1), listed

        # D-473: the list on a date. Today's date is the default; before both prices nothing is
        # in force and the first is next; on the successor's start it is in force.
        assert _entry_reads(api, book, sku, entry["id"], today.isoformat()) == listed
        yesterday = (today - datetime.timedelta(days=1)).isoformat()
        dated = _entry_reads(api, book, sku, entry["id"], yesterday)
        assert _headline(dated) == (None, (first, "scheduled")), dated
        assert dated["usage"] == _entry_usage(scheduled=2), dated
        dated = _entry_reads(api, book, sku, entry["id"], later)
        assert _headline(dated) == ((second, "active"), None), dated
        assert dated["usage"] == _entry_usage(active=1, superseded=1), dated
        for params, refusal in (
            ({"asof": later}, "QUERY_INVALID"),
            ({"as_of": "2026-02-30"}, "DATE_INVALID"),
        ):
            r = api.get(f"{PRICING}/price-books/{book}/entries", params=params)
            assert r.status_code == 400, r.text
            assert refusal in r.text, r.text
    finally:
        for kind, quorum in found.items():
            _set_quorum(api, kind, quorum)


@pytest.mark.timeout(60)
def test_the_products_units_count_page_newest_first_and_say_who_may_approve(api, reviewer):
    """P-D-227 and P-D-228 on the real binary, on one SKU's two units.

    A SKU publishes at quorum 0; its retire waits for one reviewer (a ``sku_retire`` override of
    1) and carries the submitter's note. The SKU's units are counted by state and kind and page
    newest first, also one per page through the cursor; the counts take nothing but the
    narrowing. The submitter, who also created the SKU, may not approve the retire, on the receipt
    and on the card; a fresh reviewer may, on the card and in the list, and the decided publish is
    no one's to approve. The reviewer's approve applies the retire. The override is reset.
    """
    run = uuid.uuid4().hex[:8]
    _products_quorum_zero(api)
    try:
        r = api.post(
            f"{PRODUCTS}/skus",
            json={"code": f"E2E-UNITS-{run}", "name": f"E2E units {run}", "type": "recurring"},
        )
        assert r.status_code == 201, r.text
        sku = r.json()["id"]
        r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is True, r.text
        published = r.json()["unit"]["id"]
        assert r.json()["unit"]["caller_can_approve"] is False, r.text

        # The retire waits for one reviewer.
        r = api.get(f"{PRODUCTS}/approval-policy")
        assert r.status_code == 200, r.text
        r = api.put(
            f"{PRODUCTS}/approval-policy",
            json={"kind": "sku_retire", "quorum": 1},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        note = f"Retire {run}: replaced"
        r = api.post(f"{PRODUCTS}/skus/{sku}/retire", json={"note": note})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is False, r.text
        retire = r.json()["unit"]
        assert (retire["kind"], retire["state"], retire["submit_note"]) == (
            "sku_retire",
            "pending",
            note,
        ), retire
        # P-D-228: the submitter may not approve it; a fresh reviewer may.
        assert retire["caller_can_approve"] is False, retire
        assert _unit(api, PRODUCTS, retire["id"])["caller_can_approve"] is False
        assert _unit(reviewer, PRODUCTS, retire["id"])["caller_can_approve"] is True
        r = reviewer.get(f"{PRODUCTS}/approval-units", params={"ref_id": sku})
        assert r.status_code == 200, r.text
        assert [(u["id"], u["caller_can_approve"]) for u in r.json()["items"]] == [
            (published, False),
            (retire["id"], True),
        ], r.text

        # P-D-227: the SKU's units counted by state and kind, and paged newest first.
        assert _counts(api, PRODUCTS, ref_id=sku) == {
            "by_state": {"pending": 1, "approved": 1, "rejected": 0, "withdrawn": 0},
            "by_kind": {"sku_publish": 1, "sku_change": 0, "sku_retire": 1},
            "total": 2,
        }
        assert _counts(api, PRODUCTS, ref_id=sku, state="pending")["total"] == 1
        assert _counts(api, PRODUCTS, ref_id=sku, kind="sku_change")["total"] == 0
        r = api.get(
            f"{PRODUCTS}/approval-units/counts",
            params={"ref_id": sku, "$orderby": "submitted_at desc"},
        )
        assert r.status_code == 400, r.text
        assert _unit_ids(api, PRODUCTS, ref_id=sku) == [published, retire["id"]]
        assert _unit_ids(api, PRODUCTS, ref_id=sku, **{"$orderby": "submitted_at desc"}) == [
            retire["id"],
            published,
        ]
        assert _newest_first_by_pages(api, PRODUCTS, ref_id=sku) == [retire["id"], published]
        r = api.get(
            f"{PRODUCTS}/approval-units", params={"ref_id": sku, "$orderby": "kind desc"}
        )
        assert r.status_code == 400, r.text

        # The reviewer's approve applies the retire; the decided unit is no one's to approve.
        r = reviewer.post(
            f"{PRODUCTS}/approval-units/{retire['id']}/approve", json={"generation": 1}
        )
        assert r.status_code == 200, r.text
        assert r.json()["outcome"] == "applied", r.text
        assert r.json()["unit"]["caller_can_approve"] is False, r.text
        assert _unit(reviewer, PRODUCTS, retire["id"])["caller_can_approve"] is False
        r = api.get(f"{PRODUCTS}/skus/{sku}")
        assert r.status_code == 200, r.text
        assert r.json()["sku"]["lifecycle"] == "retired", r.text
    finally:
        r = api.get(f"{PRODUCTS}/approval-policy")
        assert r.status_code == 200, r.text
        r = api.delete(
            f"{PRODUCTS}/approval-policy/sku_retire", headers={"If-Match": r.headers["etag"]}
        )
        assert r.status_code in (200, 404), r.text


@pytest.mark.timeout(120)
def test_a_draft_item_may_wait_for_its_entry(api):
    """D-512 on the real binary.

    A draft accepts a SKU with no entry. The checks name it ITEM_ENTRY_MISSING and submit
    refuses. A PATCH sets the entry; with an approved price the checks are green and submit
    is accepted.
    """
    run = uuid.uuid4().hex[:8]
    _products_quorum_zero(api)
    r = api.post(
        f"{PRODUCTS}/skus",
        json={
            "code": f"E2E-WAIT-{run}".upper(),
            "name": f"E2E wait {run}",
            "type": "recurring",
        },
    )
    assert r.status_code == 201, r.text
    sku = r.json()["id"]
    r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
    assert r.status_code == 200, r.text
    assert r.json()["applied"] is True, r.text

    r = api.post(
        f"{PRICING}/price-books",
        json={"code": f"eur-wait-{run}", "name": f"EUR wait {run}", "currency": "EUR"},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    book = r.json()["id"]
    r = api.get(f"{PRICING}/approval-policy")
    assert r.status_code == 200, r.text
    r = api.put(
        f"{PRICING}/approval-policy",
        json={"quorum": 0},
        headers={"If-Match": r.headers["etag"]},
    )
    assert r.status_code == 200, r.text

    r = api.post(
        f"{PRICING}/plans",
        json={"code": f"WAIT-{run}".upper(), "name": f"Wait {run}", "book_id": book},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    rev = r.json()["revisions"][0]["id"]
    r = api.post(
        f"{PRICING}/plan-revisions/{rev}/items",
        json={"sku_id": sku},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    item = r.json()
    assert item["price_book_entry_id"] is None, item
    assert item["reference_state"] == "confirmed", item
    etag = r.headers["etag"]
    read = _revision(api, rev)
    assert read["items"][0]["price_book_entry_id"] is None, read
    checks = _checks(api, rev)
    missing = _check(checks, "ITEM_ENTRY_MISSING")
    assert missing["ok"] is False, checks
    assert missing["label"] == "Every item points at a price", missing
    r = api.post(f"{PRICING}/plan-revisions/{rev}/submit", json={}, headers=_key())
    assert r.status_code == 400, r.text
    assert "REVISION_CHECKS_RED" in r.text, r.text
    assert "ITEM_ENTRY_MISSING" in r.text, r.text

    r = api.post(
        f"{PRICING}/price-books/{book}/entries",
        json={"sku_id": sku, "period": "month", "model": "flat"},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    entry = r.json()["id"]
    start = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    r = api.post(
        f"{PRICING}/price-book-entries/{entry}/prices",
        json={
            "price": {"amount": "30.00"},
            "eligibility": "all",
            "effective_from": start,
        },
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    r = api.post(f"{PRICING}/price-books/{book}/publish-changes", json={}, headers=_key())
    assert r.status_code == 201, r.text
    assert r.json()["applied"] is True, r.text

    r = api.patch(
        f"{PRICING}/plan-items/{item['id']}",
        json={"price_book_entry_id": entry},
        headers={"If-Match": etag},
    )
    assert r.status_code == 200, r.text
    assert r.json()["price_book_entry_id"] == entry, r.text
    checks = _checks(api, rev)
    assert _check(checks, "ITEM_ENTRY_MISSING")["ok"] is True, checks
    assert _check(checks, "ITEM_UNCOVERED")["ok"] is True, checks
    assert checks["ready"] is True, checks
    r = api.post(f"{PRICING}/plan-revisions/{rev}/submit", json={}, headers=_key())
    assert r.status_code == 201, r.text
    assert "REVISION_CHECKS_RED" not in r.text, r.text


def _pricing_quorum_zero(api) -> None:
    """Pricing's default quorum 0, written at the policy's own ETag."""
    _, tag = _policy(api)
    r = api.put(f"{PRICING}/approval-policy", json={"quorum": 0}, headers={"If-Match": tag})
    assert r.status_code == 200, r.text


@pytest.mark.timeout(120)
def test_an_archived_book_releases_its_sku_so_the_sku_retires_and_archives(api):
    """Ask 58 (pricing D-522, products P-D-263).

    A finished book is archived: its entry's SKU reference is released, the entry reads
    ``released``, and the book leaves the list unless asked ``archived eq true``. Products then
    retires the SKU, which no live reference keeps, and archives it, and the SKU leaves its list
    the same way. Every date is a UTC date.
    """
    run = uuid.uuid4().hex[:8]
    today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()

    _products_quorum_zero(api)
    code = f"E2E-ARCHIVE-{run}"
    r = api.post(
        f"{PRODUCTS}/skus",
        json={"code": code, "name": f"E2E archive {run}", "type": "recurring"},
    )
    assert r.status_code == 201, r.text
    sku = r.json()["id"]
    r = api.post(f"{PRODUCTS}/skus/{sku}/submit", json={})
    assert r.status_code == 200, r.text
    assert r.json()["applied"] is True, r.text

    _pricing_quorum_zero(api)
    book_code = f"archive-{run}"
    r = api.post(
        f"{PRICING}/price-books",
        json={"code": book_code, "name": f"Archive {run}", "currency": "EUR"},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    book = r.json()["id"]
    r = _monthly_entry(api, book, sku, "flat")
    assert r.status_code == 201, r.text
    entry = r.json()
    assert entry["reference_state"] == "confirmed", entry
    _draft_price(api, entry, {"amount": "30.00"}, today)
    r = api.post(f"{PRICING}/price-books/{book}/publish-changes", json={}, headers=_key())
    assert r.status_code == 201, r.text
    assert r.json()["applied"] is True, r.text

    # The live entry keeps the SKU referenced.
    r = api.post(f"{PRODUCTS}/skus/{sku}/retire", json={})
    assert r.status_code == 409, r.text
    assert "SKU_REFERENCED" in r.text, r.text

    # Archive the finished book at its ETag.
    r = api.get(f"{PRICING}/price-books/{book}")
    assert r.status_code == 200, r.text
    r = api.post(
        f"{PRICING}/price-books/{book}/archive",
        json={},
        headers={"If-Match": r.headers["etag"]},
    )
    assert r.status_code == 200, r.text
    assert r.json()["archived_at"] is not None, r.text
    r = api.get(f"{PRICING}/price-book-entries/{entry['id']}")
    assert r.status_code == 200, r.text
    assert r.json()["reference_state"] == "released", r.text
    r = api.get(f"{PRICING}/price-books", params={"q": book_code})
    assert r.status_code == 200, r.text
    assert [b["id"] for b in r.json()["items"]] == [], r.text
    r = api.get(
        f"{PRICING}/price-books", params={"q": book_code, "$filter": "archived eq true"}
    )
    assert r.status_code == 200, r.text
    assert [b["id"] for b in r.json()["items"]] == [book], r.text
    # The archived book's entries are read-only.
    r = api.post(
        f"{PRICING}/price-book-entries/{entry['id']}/prices",
        json={"price": {"amount": "31.00"}, "eligibility": "all", "effective_from": today},
        headers=_key(),
    )
    assert r.status_code == 409, r.text
    assert "BOOK_ARCHIVED" in r.text, r.text

    # Products retires the SKU, then archives it.
    r = api.post(f"{PRODUCTS}/skus/{sku}/retire", json={})
    assert r.status_code == 200, r.text
    assert r.json()["applied"] is True, r.text
    r = api.get(f"{PRODUCTS}/skus/{sku}")
    assert r.status_code == 200, r.text
    assert r.json()["sku"]["lifecycle"] == "retired", r.text
    r = api.post(
        f"{PRODUCTS}/skus/{sku}/archive",
        headers={"If-Match": r.headers["etag"]},
    )
    assert r.status_code == 200, r.text
    assert r.json()["archived_at"] is not None, r.text
    assert _usage_filtered(api, code) == [], "an archived SKU leaves the list"
    r = api.get(f"{PRODUCTS}/skus", params={"q": code, "$filter": "archived eq true"})
    assert r.status_code == 200, r.text
    assert [s["id"] for s in r.json()["items"]] == [sku], r.text
    r = api.get(f"{PRODUCTS}/skus/{sku}")
    assert r.status_code == 200, "a read by id ignores the mark"
