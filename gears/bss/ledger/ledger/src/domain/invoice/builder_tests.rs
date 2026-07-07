//! Tests for the direct-split invoice-entry builder ([`super::build_invoice_entry`]).
//!
//! Variant A: the only lines are DR AR / CR Revenue (per stream) / CR Tax (per
//! breakdown) — NEVER a Contract-liability line. Money is pure `i64` summation:
//! `Σ DR == Σ CR` exactly.

use super::*;
use crate::domain::invoice::mapping::resolve;

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// An ex-tax item in `stream`, mapped to `REVENUE` via the Catalog class.
fn revenue_item(amount: i64, stream: &str) -> InvoiceItem {
    InvoiceItem {
        amount_minor_ex_tax: amount,
        deferred_minor: 0,
        currency: "USD".to_owned(),
        revenue_stream: stream.to_owned(),
        catalog_class: Some(AccountClass::Revenue),
        contract_class: None,
        gl_code: Some("4000".to_owned()),
        recognition: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
    }
}

fn tax(amount: i64, juris: &str, filing: &str) -> TaxBreakdown {
    TaxBreakdown {
        amount_minor: amount,
        currency: "USD".to_owned(),
        tax_jurisdiction: juris.to_owned(),
        tax_filing_period: filing.to_owned(),
        tax_rate_ref: None,
    }
}

/// An invoice over `items` + `tax`, with fixed identity fields.
fn invoice(items: Vec<InvoiceItem>, tax: Vec<TaxBreakdown>) -> PostedInvoice {
    PostedInvoice {
        invoice_id: "INV-1".to_owned(),
        payer_tenant_id: Uuid::now_v7(),
        resource_tenant_id: None,
        seller_tenant_id: Uuid::now_v7(),
        effective_at: naive(2026, 6, 1),
        due_date: Some(naive(2026, 7, 1)),
        period_id: "202606".to_owned(),
        items,
        tax,
        posted_by_actor_id: Uuid::now_v7(),
        correlation_id: Uuid::now_v7(),
    }
}

/// Build the entry for `inv`, mapping each item through the real resolver.
fn build(inv: &PostedInvoice) -> PostEntry {
    let mapped: Vec<_> = inv.items.iter().map(resolve).collect();
    build_invoice_entry(inv, &mapped)
}

/// `Σ DR == Σ CR` (the balance invariant), computed over the built lines.
fn nets_to_zero(entry: &PostEntry) -> bool {
    let net: i128 = entry
        .lines
        .iter()
        .map(|l| match l.side {
            Side::Debit => i128::from(l.amount_minor),
            Side::Credit => -i128::from(l.amount_minor),
        })
        .sum();
    net == 0
}

fn line_of(entry: &PostEntry, class: AccountClass, side: Side) -> Vec<&PostLine> {
    entry
        .lines
        .iter()
        .filter(|l| l.account_class == class && l.side == side)
        .collect()
}

#[test]
fn one_item_one_tax_builds_three_balanced_lines() {
    let inv = invoice(
        vec![revenue_item(1000, "subscription")],
        vec![tax(200, "US-CA", "2026Q2")],
    );
    let entry = build(&inv);

    assert_eq!(entry.lines.len(), 3, "DR AR + CR Revenue + CR Tax");
    assert_eq!(entry.source_doc_type, SourceDocType::InvoicePost);
    assert_eq!(entry.source_business_id, "INV-1");
    assert!(
        entry.reverses_entry_id.is_none(),
        "an invoice-post reverses nothing"
    );
    assert!(nets_to_zero(&entry), "Σ DR must equal Σ CR exactly");

    // DR AR = gross = item + tax = 1200, carries invoice_id + due_date.
    let ar = line_of(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(ar.len(), 1);
    assert_eq!(ar[0].amount_minor, 1200, "AR gross = 1000 + 200");
    assert_eq!(ar[0].invoice_id.as_deref(), Some("INV-1"));
    assert_eq!(ar[0].due_date, Some(naive(2026, 7, 1)));
    assert!(
        ar[0].revenue_stream.is_none(),
        "the AR line carries no revenue_stream"
    );

    // CR Revenue = 1000, stream set.
    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(rev.len(), 1);
    assert_eq!(rev[0].amount_minor, 1000, "Revenue = ex-tax item");
    assert_eq!(
        rev[0].revenue_stream.as_deref(),
        Some("subscription"),
        "every Revenue line must carry its stream"
    );

    // CR Tax = 200, dims set.
    let tax_lines = line_of(&entry, AccountClass::TaxPayable, Side::Credit);
    assert_eq!(tax_lines.len(), 1);
    assert_eq!(tax_lines[0].amount_minor, 200);
    assert_eq!(tax_lines[0].tax_jurisdiction.as_deref(), Some("US-CA"));
    assert_eq!(tax_lines[0].tax_filing_period.as_deref(), Some("2026Q2"));
}

#[test]
fn two_items_different_streams_group_into_two_revenue_lines() {
    let inv = invoice(
        vec![
            revenue_item(1000, "subscription"),
            revenue_item(500, "usage"),
        ],
        vec![],
    );
    let entry = build(&inv);

    // No tax ⇒ 1 AR + 2 Revenue = 3 lines.
    assert_eq!(entry.lines.len(), 3);
    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(rev.len(), 2, "one Revenue line per distinct stream");
    let mut by_stream: Vec<(String, i64)> = rev
        .iter()
        .map(|l| (l.revenue_stream.clone().unwrap(), l.amount_minor))
        .collect();
    by_stream.sort();
    assert_eq!(
        by_stream,
        vec![("subscription".to_owned(), 1000), ("usage".to_owned(), 500)]
    );

    // AR = 1500 (no tax), balanced.
    let ar = line_of(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(ar[0].amount_minor, 1500);
    assert!(nets_to_zero(&entry));
}

#[test]
fn two_items_same_stream_sum_into_one_revenue_line() {
    let inv = invoice(
        vec![
            revenue_item(1000, "subscription"),
            revenue_item(250, "subscription"),
        ],
        vec![],
    );
    let entry = build(&inv);

    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(rev.len(), 1, "same stream ⇒ one grouped Revenue line");
    assert_eq!(rev[0].amount_minor, 1250, "grouped sum of the stream");
    assert!(nets_to_zero(&entry));
}

#[test]
fn zero_tax_omits_the_tax_line() {
    let inv = invoice(vec![revenue_item(1000, "subscription")], vec![]);
    let entry = build(&inv);

    assert_eq!(entry.lines.len(), 2, "no tax ⇒ DR AR + CR Revenue only");
    assert!(
        line_of(&entry, AccountClass::TaxPayable, Side::Credit).is_empty(),
        "no Tax line when there is no tax"
    );
    let ar = line_of(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(ar[0].amount_minor, 1000, "AR = item only (no tax)");
    assert!(nets_to_zero(&entry));
}

#[test]
fn variant_a_never_posts_a_contract_liability_line() {
    // The defining Variant-A invariant: the whole amount is recognized now, so
    // there is NO deferred / Contract-liability leg, regardless of item count
    // or tax.
    let inv = invoice(
        vec![
            revenue_item(1000, "subscription"),
            revenue_item(500, "usage"),
        ],
        vec![tax(150, "US-CA", "2026Q2")],
    );
    let entry = build(&inv);

    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::ContractLiability),
        "Variant A must NEVER post a Contract-liability line"
    );
    assert!(nets_to_zero(&entry));
}

#[test]
fn multiple_tax_breakdowns_each_post_their_own_line() {
    let inv = invoice(
        vec![revenue_item(1000, "subscription")],
        vec![tax(120, "US-CA", "2026Q2"), tax(80, "US-NY", "2026Q2")],
    );
    let entry = build(&inv);

    let tax_lines = line_of(&entry, AccountClass::TaxPayable, Side::Credit);
    assert_eq!(tax_lines.len(), 2, "one CR Tax line per breakdown");
    // AR gross = 1000 + 120 + 80 = 1200.
    let ar = line_of(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(ar[0].amount_minor, 1200);
    assert!(nets_to_zero(&entry));
}

#[test]
fn unmapped_item_routes_to_a_suspense_credit_line() {
    // An item with no Catalog/Contract class maps to SUSPENSE/PENDING; the CR
    // leg books to SUSPENSE (not a guessed revenue account) and the entry still
    // balances against the AR debit.
    let mut unmapped = revenue_item(1000, "subscription");
    unmapped.catalog_class = None;
    unmapped.contract_class = None;
    let inv = invoice(vec![unmapped], vec![]);
    let entry = build(&inv);

    let suspense = line_of(&entry, AccountClass::Suspense, Side::Credit);
    assert_eq!(suspense.len(), 1, "the unmapped CR leg routes to SUSPENSE");
    assert_eq!(suspense[0].mapping_status, MappingStatus::Pending);
    assert!(
        line_of(&entry, AccountClass::Revenue, Side::Credit).is_empty(),
        "an unmapped item must NOT silently book to Revenue"
    );
    assert!(nets_to_zero(&entry));
}

#[test]
fn entry_currency_follows_the_invoice() {
    let inv = invoice(vec![revenue_item(1000, "subscription")], vec![]);
    let entry = build(&inv);
    assert_eq!(entry.entry_currency, "USD");
    assert!(entry.lines.iter().all(|l| l.currency == "USD"));
}

// ── Slice 4: the deferred split (CR REVENUE + CR CONTRACT_LIABILITY) ──────────

/// A revenue item that defers `deferred` of its `amount` to Contract-liability.
fn deferred_item(amount: i64, deferred: i64, stream: &str) -> InvoiceItem {
    let mut item = revenue_item(amount, stream);
    item.deferred_minor = deferred;
    item
}

#[test]
fn deferred_item_splits_credit_into_revenue_and_contract_liability() {
    // 1200 ex-tax, 900 deferred ⇒ CR Revenue 300 + CR Contract-liability 900,
    // same stream on both; DR AR still the full 1200 (no tax). Σ DR == Σ CR.
    let inv = invoice(vec![deferred_item(1200, 900, "subscription")], vec![]);
    let entry = build(&inv);

    let ar = line_of(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(ar[0].amount_minor, 1200, "AR is the full ex-tax amount");

    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(
        rev.len(),
        1,
        "one Revenue line for the recognized-now portion"
    );
    assert_eq!(rev[0].amount_minor, 300, "Revenue = amount − deferred");
    assert_eq!(rev[0].revenue_stream.as_deref(), Some("subscription"));

    let cl = line_of(&entry, AccountClass::ContractLiability, Side::Credit);
    assert_eq!(
        cl.len(),
        1,
        "one Contract-liability line for the deferred portion"
    );
    assert_eq!(cl[0].amount_minor, 900, "Contract-liability = deferred");
    assert_eq!(
        cl[0].revenue_stream.as_deref(),
        Some("subscription"),
        "the Contract-liability line carries the SAME stream as Revenue"
    );

    assert!(nets_to_zero(&entry), "the split keeps Σ DR == Σ CR exact");
}

#[test]
fn fully_deferred_item_emits_no_revenue_line() {
    // 1000 ex-tax, all deferred ⇒ recognized-now 0. NO Revenue line is emitted
    // (the engine rejects a zero-amount line); the Contract-liability carries the
    // whole credit, balanced against the AR/tax debit.
    let inv = invoice(vec![deferred_item(1000, 1000, "subscription")], vec![]);
    let entry = build(&inv);

    let cl = line_of(&entry, AccountClass::ContractLiability, Side::Credit);
    assert_eq!(cl[0].amount_minor, 1000, "the whole amount defers");
    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert!(
        rev.is_empty(),
        "a fully-deferred stream emits no Revenue line"
    );
    assert!(nets_to_zero(&entry));
}

#[test]
fn per_stream_deferral_groups_one_contract_liability_line_per_stream() {
    // Two streams, each partly deferred ⇒ one CR Revenue + one CR
    // Contract-liability PER stream (per-stream disaggregation, §3.5).
    let inv = invoice(
        vec![
            deferred_item(1000, 600, "subscription"),
            deferred_item(500, 200, "usage"),
        ],
        vec![],
    );
    let entry = build(&inv);

    let cl = line_of(&entry, AccountClass::ContractLiability, Side::Credit);
    assert_eq!(
        cl.len(),
        2,
        "one Contract-liability line per deferring stream"
    );
    let mut by_stream: Vec<(String, i64)> = cl
        .iter()
        .map(|l| (l.revenue_stream.clone().unwrap(), l.amount_minor))
        .collect();
    by_stream.sort();
    assert_eq!(
        by_stream,
        vec![("subscription".to_owned(), 600), ("usage".to_owned(), 200)]
    );
    // Recognized-now Revenue: 400 + 300.
    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    let rev_total: i64 = rev.iter().map(|l| l.amount_minor).sum();
    assert_eq!(rev_total, 700, "Σ recognized-now = (1000−600)+(500−200)");
    assert!(nets_to_zero(&entry));
}

#[test]
fn two_deferring_items_one_stream_merge_into_one_cl_line() {
    // Z4-1: TWO deferring items in the SAME revenue_stream
    // merge into ONE Contract-liability line whose amount is the Σ of both deferred
    // portions. The merged line seeds its source refs from the FIRST deferring item
    // — recognition mints one schedule PER item, so the second item's schedule
    // `source_invoice_item_ref` resolves to no journal line. AUDIT-ONLY today
    // (nothing dereferences it at recognition runtime; the runner posts
    // `invoice_item_ref: None` and tie-out joins by `entry_id`). This test locks the
    // current per-stream merge amount + balance; revisit the per-item linkage if
    // Slice-7 reconciliation starts joining schedule→CL by item-ref.
    let inv = invoice(
        vec![
            deferred_item(1000, 600, "subscription"),
            deferred_item(500, 200, "subscription"),
        ],
        vec![],
    );
    let entry = build(&inv);

    let cl = line_of(&entry, AccountClass::ContractLiability, Side::Credit);
    assert_eq!(
        cl.len(),
        1,
        "two deferring items in one stream → one merged Contract-liability line"
    );
    assert_eq!(
        cl[0].amount_minor, 800,
        "merged Contract-liability = 600 + 200 (Σ deferred for the stream)"
    );
    assert_eq!(cl[0].revenue_stream.as_deref(), Some("subscription"));

    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(
        rev.len(),
        1,
        "one grouped Revenue line for the merged recognized-now amount"
    );
    assert_eq!(
        rev[0].amount_minor, 700,
        "recognized-now = (1000−600) + (500−200)"
    );

    assert!(
        nets_to_zero(&entry),
        "the per-stream merge keeps Σ DR == Σ CR exact"
    );
}

#[test]
fn mixed_deferred_and_undeferred_same_stream_sum_correctly() {
    // Two items in one stream: one defers, one does not. Revenue =
    // recognized-now sum; Contract-liability = the single deferred amount.
    let inv = invoice(
        vec![
            deferred_item(1000, 600, "subscription"),
            revenue_item(400, "subscription"),
        ],
        vec![],
    );
    let entry = build(&inv);

    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(rev.len(), 1, "same stream ⇒ one grouped Revenue line");
    assert_eq!(rev[0].amount_minor, 800, "(1000−600) + 400 recognized now");
    let cl = line_of(&entry, AccountClass::ContractLiability, Side::Credit);
    assert_eq!(cl[0].amount_minor, 600, "only the deferred item's portion");
    assert!(nets_to_zero(&entry));
}

#[test]
fn deferred_zero_is_byte_identical_to_no_deferral() {
    // The byte-identical guarantee: an item with `deferred_minor = 0` builds the
    // EXACT same lines as one whose field is left at the default — no
    // Contract-liability line, same Revenue/AR/Tax. We compare against the
    // unchanged `revenue_item` helper (deferred defaults to 0).
    let with_zero = invoice(
        vec![{
            let mut i = revenue_item(1000, "subscription");
            i.deferred_minor = 0;
            i
        }],
        vec![tax(200, "US-CA", "2026Q2")],
    );
    let entry = build(&with_zero);

    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::ContractLiability),
        "deferred = 0 must emit NO Contract-liability line"
    );
    assert_eq!(
        entry.lines.len(),
        3,
        "DR AR + CR Revenue + CR Tax, as before"
    );
    let rev = line_of(&entry, AccountClass::Revenue, Side::Credit);
    assert_eq!(rev[0].amount_minor, 1000, "the whole amount recognizes now");
    let ar = line_of(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(ar[0].amount_minor, 1200);
    assert!(nets_to_zero(&entry));
}
