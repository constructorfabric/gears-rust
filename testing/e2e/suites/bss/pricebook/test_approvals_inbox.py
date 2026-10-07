"""The approvals inbox over HTTP: one list, count, card and vote over two gears' units.

``bss-approvals`` serves ``/bss-approvals/v1/approval-units`` over the approval units of the gears
its config names (``config/e2e-local.yaml``: pricing and products). It reads each gear through
that gear's own doors as the caller, merges their pages by ``submitted_at`` and then the unit id,
sums their counts, finds a card's owner by asking every gear, and passes a vote through to the
owner's vote door with the body and the ``Idempotency-Key`` as received (approvals AP-D-1 to
AP-D-4; pricing D-490; products P-D-250).

Tenant A holds the units of every other test of this suite, so the scenario reads its own units
as the newest pending ones and narrows the counts by the ids it created.
"""

import datetime
import os
import uuid

import httpx
import pytest

from .conftest import PRICING, PRODUCTS, REQUEST_TIMEOUT
from .test_pricebook_flow import _book, _key, _monthly_entry, _products_quorum_zero

INBOX = "/bss-approvals/v1"


@pytest.fixture(scope="module", autouse=True)
def require_inbox_mounted():
    """Skip the module on a server that does not serve the inbox (an authenticated 404)."""
    base_url = os.getenv("E2E_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    with httpx.Client(timeout=REQUEST_TIMEOUT) as client:
        r = client.get(
            f"{base_url}{INBOX}/approval-units/counts",
            headers={"Authorization": f"Bearer {token}"},
        )
    if r.status_code == 404:
        pytest.skip(f"{INBOX} is not served: no `bss-approvals` feature or config block.")


@pytest.fixture
def tenant_b():
    """A principal of another tenant: the inbox shows it none of tenant A's units."""
    client = httpx.Client(
        base_url=os.getenv("E2E_BASE_URL", "http://localhost:8086"),
        timeout=REQUEST_TIMEOUT,
        headers={"Authorization": "Bearer e2e-token-tenant-b"},
    )
    try:
        yield client
    finally:
        client.close()


BOTH_OK = [{"name": "pricing", "status": "ok"}, {"name": "products", "status": "ok"}]


def _ok(r: httpx.Response) -> dict:
    assert r.status_code == 200, r.text
    return r.json()


def _sku(api, code: str) -> str:
    r = api.post(f"{PRODUCTS}/skus", json={"code": code, "name": code, "type": "recurring"})
    assert r.status_code == 201, r.text
    return r.json()["id"]


def _draft_price(api, entry: str, start: str) -> str:
    r = api.post(
        f"{PRICING}/price-book-entries/{entry}/prices",
        json={"price": {"amount": "30.00"}, "eligibility": "all", "effective_from": start},
        headers=_key(),
    )
    assert r.status_code == 201, r.text
    return r.json()["items"][0]["id"]


def _priced_entry(api, sku: str, run: str, n: int) -> tuple[str, str]:
    """A EUR book with a monthly entry of ``sku`` and one draft price: (book, price)."""
    book = _book(api, "EUR", f"{run}-inbox{n}")
    r = _monthly_entry(api, book, sku, "flat")
    assert r.status_code == 201, r.text
    start = (datetime.datetime.now(datetime.timezone.utc).date() + datetime.timedelta(days=30)).isoformat()
    return book, _draft_price(api, r.json()["id"], start)


def _inbox_counts(client, **narrowing: str) -> dict:
    return _ok(client.get(f"{INBOX}/approval-units/counts", params=narrowing))


def _gear_counts(client, gear: str, **narrowing: str) -> dict:
    return _ok(client.get(f"{gear}/approval-units/counts", params=narrowing))


def _instant(text: str) -> datetime.datetime:
    return datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))


def _restore_override(api, gear: str, kind: str, quorum) -> None:
    """Put a kind's quorum override back as found: removed when there was none."""
    r = api.get(f"{gear}/approval-policy")
    assert r.status_code == 200, r.text
    tag = {"If-Match": r.headers["etag"]}
    if quorum is None:
        r = api.delete(f"{gear}/approval-policy/{kind}", headers=tag)
        assert r.status_code in (200, 404), r.text
    else:
        r = api.put(f"{gear}/approval-policy", json={"kind": kind, "quorum": quorum}, headers=tag)
        assert r.status_code == 200, r.text


#: The unit fields the inbox card takes from the owning gear's card as they are.
SHARED = (
    "id",
    "kind",
    "ref_type",
    "ref_id",
    "state",
    "generation",
    "quorum_required",
    "submitted_by",
    "submit_note",
    "decided_note",
    "snapshot",
    "caller_can_approve",
)


def _same_unit(inbox: dict, door: dict, source: str) -> None:
    assert inbox["source"] == source, inbox
    assert {k: inbox[k] for k in SHARED} == {k: door[k] for k in SHARED}, (inbox, door)
    assert _instant(inbox["submitted_at"]) == _instant(door["submitted_at"]), (inbox, door)


@pytest.mark.timeout(120)
def test_the_inbox_lists_counts_and_votes_on_both_gears_units_through_their_doors(
    api, reviewer, tenant_b
):
    """AP-D-2 to AP-D-4 on the real binary, over two pricing units and one products unit.

    A ``prices`` unit (quorum 1), then a ``sku_publish`` unit (a ``sku_publish`` override of 1),
    then a second ``prices`` unit wait in tenant A. The inbox lists them newest first in one page,
    and one per page through its cursor, each naming its gear, and names both gears ``ok``. Its
    counts are the two gears' counts summed, and a ``book_id`` narrowing keeps pricing's unit and
    no products unit. Its card is the owning gear's card with that gear's name, products'
    ``impact_live`` as ``subject_live``, and pricing's ``impact``; tenant B and an unknown id read
    404. The submitter's approve through the inbox is the door's ``SOD_VIOLATION``. The
    reviewer's approves through the inbox apply both kinds as the doors do; the same
    ``Idempotency-Key`` and body at pricing's own door replay the inbox's receipt, and the last
    unit, approved at that door, answers a receipt of the same shape. Both overrides are restored
    as found.
    """
    run = uuid.uuid4().hex[:8]
    # Each override this test writes, as found: none, or its quorum.
    found = {
        (gear, kind): _ok(api.get(f"{gear}/approval-policy"))["overrides"].get(kind)
        for gear, kind in ((PRICING, "prices"), (PRODUCTS, "sku_publish"))
    }
    try:
        # A published SKU priced in two books, each with one draft price.
        _products_quorum_zero(api)
        priced = _sku(api, f"E2E-INBOX-{run}")
        r = api.post(f"{PRODUCTS}/skus/{priced}/submit", json={})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is True, r.text
        book1, price1 = _priced_entry(api, priced, run, 1)
        book2, _ = _priced_entry(api, priced, run, 2)
        r = api.get(f"{PRICING}/approval-policy")
        assert r.status_code == 200, r.text
        r = api.put(
            f"{PRICING}/approval-policy",
            json={"kind": "prices", "quorum": 1},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text

        # Pricing, products, pricing: three pending units, oldest first.
        r = api.post(f"{PRICING}/prices/{price1}/submit", json={}, headers=_key())
        assert r.status_code == 201, r.text
        first = r.json()["unit"]["id"]
        r = api.get(f"{PRODUCTS}/approval-policy")
        assert r.status_code == 200, r.text
        r = api.put(
            f"{PRODUCTS}/approval-policy",
            json={"kind": "sku_publish", "quorum": 1},
            headers={"If-Match": r.headers["etag"]},
        )
        assert r.status_code == 200, r.text
        pending_sku = _sku(api, f"E2E-INBOX-NEW-{run}")
        r = api.post(f"{PRODUCTS}/skus/{pending_sku}/submit", json={})
        assert r.status_code == 200, r.text
        assert r.json()["applied"] is False, r.text
        middle = r.json()["unit"]["id"]
        r = api.post(f"{PRICING}/price-books/{book2}/publish-changes", json={}, headers=_key())
        assert r.status_code == 201, r.text
        assert r.json()["applied"] is False, r.text
        last = r.json()["unit"]["id"]
        newest_first = [last, middle, first]

        # The list: both gears' units in one page, newest first, each naming its gear.
        page = _ok(api.get(f"{INBOX}/approval-units", params={"state": "pending", "limit": 3}))
        assert [u["id"] for u in page["items"]] == newest_first, page
        assert [u["source"] for u in page["items"]] == ["pricing", "products", "pricing"], page
        assert page["sources"] == BOTH_OK, page
        # One per page: the cursor carries the order and each gear's key.
        page = _ok(
            api.get(
                f"{INBOX}/approval-units",
                params={"state": "pending", "limit": 1, "$orderby": "submitted_at desc"},
            )
        )
        walked = [u["id"] for u in page["items"]]
        cursor = page["next_cursor"]
        for _ in range(2):
            assert cursor is not None, page
            page = _ok(
                api.get(
                    f"{INBOX}/approval-units",
                    params={"state": "pending", "limit": 1, "cursor": cursor},
                )
            )
            walked.extend(u["id"] for u in page["items"])
            cursor = page.get("next_cursor")
        assert walked == newest_first, page
        # A cursor answers only the narrowing it was issued for.
        first_page = _ok(
            api.get(f"{INBOX}/approval-units", params={"state": "pending", "limit": 1})
        )
        r = api.get(f"{INBOX}/approval-units", params={"cursor": first_page["next_cursor"]})
        assert r.status_code == 400, r.text
        assert "FILTER_MISMATCH" in r.text, r.text

        # The counts: the two gears' counts summed, under the same narrowing.
        pricing = _gear_counts(api, PRICING, state="pending")
        products = _gear_counts(api, PRODUCTS, state="pending")
        assert _inbox_counts(api, state="pending") == {
            "by_state": {
                state: pricing["by_state"][state] + products["by_state"][state]
                for state in ("pending", "approved", "rejected", "withdrawn")
            },
            "by_kind": {**pricing["by_kind"], **products["by_kind"]},
            "total": pricing["total"] + products["total"],
            "sources": BOTH_OK,
        }
        assert pricing["by_kind"]["prices"] >= 2 and products["by_kind"]["sku_publish"] >= 1
        assert _inbox_counts(api, book_id=book1) == {
            "by_state": {"pending": 1, "approved": 0, "rejected": 0, "withdrawn": 0},
            "by_kind": {
                "prices": 1,
                "plan_revision": 0,
                "sku_publish": 0,
                "sku_change": 0,
                "sku_retire": 0,
            },
            "total": 1,
            "sources": BOTH_OK,
        }
        assert _inbox_counts(api, ref_id=pending_sku)["by_kind"]["sku_publish"] == 1

        # The card: the owning gear's card under its name, read as the reader.
        for client, can_approve in ((api, False), (reviewer, True)):
            card = _ok(client.get(f"{INBOX}/approval-units/{middle}"))
            door = _ok(client.get(f"{PRODUCTS}/approval-units/{middle}"))
            _same_unit(card, door, "products")
            assert card["caller_can_approve"] is can_approve, card
            assert card["subject_live"] == door["impact_live"], (card, door)
            assert card["ref_id"] == pending_sku, card
        card = _ok(reviewer.get(f"{INBOX}/approval-units/{first}"))
        door = _ok(reviewer.get(f"{PRICING}/approval-units/{first}"))
        _same_unit(card, door, "pricing")
        assert card["impact"] == door["impact"] and card["impact"] is not None, (card, door)
        assert card["subject_live"] is None, card
        for client, unit in ((tenant_b, middle), (tenant_b, first), (api, str(uuid.uuid4()))):
            r = client.get(f"{INBOX}/approval-units/{unit}")
            assert r.status_code == 404, r.text

        # The submitter's own approve is the products door's refusal, passed through.
        r = api.post(
            f"{INBOX}/approval-units/{middle}/approve", json={"generation": 1}, headers=_key()
        )
        assert r.status_code == 403, r.text
        assert "SOD_VIOLATION" in r.text, r.text

        # The reviewer approves a pricing unit through the inbox; pricing's own door replays
        # the receipt for the same key and body (one idempotency row, AP-D-4).
        key = _key()
        via_inbox = _ok(
            reviewer.post(
                f"{INBOX}/approval-units/{first}/approve", json={"generation": 1}, headers=key
            )
        )
        assert via_inbox["outcome"] == "applied", via_inbox
        assert (via_inbox["unit"]["id"], via_inbox["unit"]["state"]) == (first, "approved")
        replay = reviewer.post(
            f"{PRICING}/approval-units/{first}/approve", json={"generation": 1}, headers=key
        )
        assert _ok(replay) == via_inbox
        assert _ok(reviewer.get(f"{PRICING}/approval-units/{first}"))["state"] == "approved"
        assert _ok(reviewer.get(f"{INBOX}/approval-units/{first}"))["state"] == "approved"

        # ...and the products unit: the SKU publishes.
        r = reviewer.post(
            f"{INBOX}/approval-units/{middle}/approve", json={"generation": 1}, headers=_key()
        )
        receipt = _ok(r)
        assert receipt["outcome"] == "applied", receipt
        assert receipt["unit"]["state"] == "approved", receipt
        sku = _ok(api.get(f"{PRODUCTS}/skus/{pending_sku}"))
        assert sku["sku"]["lifecycle"] == "published", sku

        # The last unit at pricing's own door: a receipt of the inbox's shape.
        direct = _ok(
            reviewer.post(
                f"{PRICING}/approval-units/{last}/approve", json={"generation": 1}, headers=_key()
            )
        )
        assert direct["outcome"] == "applied", direct
        assert set(direct) == set(via_inbox), (direct, via_inbox)
        pending = _ok(api.get(f"{INBOX}/approval-units", params={"state": "pending"}))["items"]
        assert not {u["id"] for u in pending} & set(newest_first), pending
    finally:
        for (gear, kind), quorum in found.items():
            _restore_override(api, gear, kind, quorum)
