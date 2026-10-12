//! Tests for the reversal + `MAPPING_CORRECTION` flow.

use bss_ledger_sdk::{AccountClass, LineView, MappingStatus};
use chrono::NaiveDate;

use super::*;
use time::OffsetDateTime;

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn line(account: Uuid, class: AccountClass, side: Side, amount: i64) -> LineView {
    LineView {
        line_id: Uuid::now_v7(),
        entry_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        account_id: account,
        account_class: class,
        gl_code: None,
        side,
        money: money(amount),
        invoice_id: Some("INV-1".to_owned()),
        due_date: Some(naive(2026, 7, 1)),
        revenue_stream: if class == AccountClass::Revenue {
            Some("subscription".to_owned())
        } else {
            None
        },
        mapping_status: MappingStatus::Resolved,
        functional_money: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        ar_status: None,
    }
}

/// Like [`line`] but cross-currency: carries a functional (EUR) translation, so a
/// reversal must copy it onto the flipped leg (the carry-forward fix).
fn fx_line(
    account: Uuid,
    class: AccountClass,
    side: Side,
    amount: i64,
    functional: i64,
) -> LineView {
    LineView {
        functional_money: Some(
            PostedMoney::try_new(
                Decimal::new(functional, 2),
                CurrencySpec::try_new("EUR".to_owned(), 2).unwrap(),
            )
            .unwrap(),
        ),
        ..line(account, class, side, amount)
    }
}

/// An original `INVOICE_POST` entry: DR AR 1200 / CR Revenue 1000 / CR Tax 200.
fn original_invoice() -> EntryView {
    let ar = Uuid::now_v7();
    let rev = Uuid::now_v7();
    let tax = Uuid::now_v7();
    EntryView {
        entry_id: Uuid::now_v7(),
        tenant_id: Uuid::now_v7(),
        period_id: "202606".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: SourceDocType::InvoicePost,
        source_business_id: "INV-1".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: now(),
        effective_at: naive(2026, 6, 1),
        posted_by_actor_id: Uuid::now_v7(),
        origin: "SYSTEM".to_owned(),
        correlation_id: Uuid::now_v7(),
        created_seq: 1,
        lines: vec![
            line(ar, AccountClass::Ar, Side::Debit, 1200),
            line(rev, AccountClass::Revenue, Side::Credit, 1000),
            line(tax, AccountClass::TaxPayable, Side::Credit, 200),
        ],
    }
}

#[test]
fn reversal_flips_sides_keeps_amounts_positive_and_sets_reverses() {
    let original = original_invoice();
    let actor = Uuid::now_v7();
    let corr = Uuid::now_v7();
    let reversal = build_reversal(
        &original,
        "202607".to_owned(),
        naive(2026, 7, 2),
        actor,
        corr,
    )
    .expect("reversal of an invoice-post must build");

    assert_eq!(reversal.source_doc_type, SourceDocType::Reversal);
    assert_eq!(
        reversal.reverses_entry_id,
        Some(original.entry_id),
        "reverses_entry_id must point at the original"
    );
    assert_eq!(
        reversal.reverses_period_id.as_deref(),
        Some("202606"),
        "reverses_period_id must carry the original's period"
    );
    assert_eq!(
        reversal.period_id, "202607",
        "the reversal posts into the supplied period"
    );
    assert_eq!(
        reversal.source_business_id,
        format!("reverses={}", original.entry_id)
    );

    // Same accounts, flipped sides, positive amounts.
    assert_eq!(reversal.lines.len(), original.lines.len());
    for (orig, rev) in original.lines.iter().zip(reversal.lines.iter()) {
        assert_eq!(rev.account_id, orig.account_id, "same account");
        assert_eq!(rev.money, orig.money, "amount unchanged");
        assert!(
            rev.money.amount() > Decimal::ZERO,
            "reversal amount stays positive"
        );
        let flipped = match orig.side {
            Side::Debit => Side::Credit,
            Side::Credit => Side::Debit,
        };
        assert_eq!(rev.side, flipped, "side flipped");
    }

    // The reversal nets to zero on its own (DR 1000 + DR 200 / CR 1200).
    let net: Decimal = reversal
        .lines
        .iter()
        .map(|l| match l.side {
            Side::Debit => l.money.amount(),
            Side::Credit => -l.money.amount(),
        })
        .sum();
    assert_eq!(net, Decimal::ZERO, "the reversal is itself balanced");
}

#[test]
fn reversal_carries_functional_forward_and_nets_to_zero() {
    // A cross-currency original (USD transaction, EUR functional at 0.9): DR AR
    // 1200/1080 / CR Revenue 1000/900 / CR Tax 200/180. The reversal must copy each
    // leg's functional (positive) onto the flipped side so the functional column
    // nets to zero — the fix for the silent transaction-vs-functional drift on a
    // cross-currency reversal (it must NOT post functional-NULL).
    let ar = Uuid::now_v7();
    let rev_acct = Uuid::now_v7();
    let tax = Uuid::now_v7();
    let mut original = original_invoice();
    original.lines = vec![
        fx_line(ar, AccountClass::Ar, Side::Debit, 1200, 1080),
        fx_line(rev_acct, AccountClass::Revenue, Side::Credit, 1000, 900),
        fx_line(tax, AccountClass::TaxPayable, Side::Credit, 200, 180),
    ];

    let reversal = build_reversal(
        &original,
        "202607".to_owned(),
        naive(2026, 7, 2),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .expect("cross-currency reversal must build");

    // Every leg carries the ORIGINAL functional (positive) + currency, side flipped.
    for (orig, rev) in original.lines.iter().zip(reversal.lines.iter()) {
        assert_eq!(
            rev.functional_money, orig.functional_money,
            "functional carried at the original rate (positive, unchanged)"
        );
        assert_eq!(
            rev.functional_money.as_ref().map(|m| m.currency().code()),
            Some("EUR"),
            "functional currency carried"
        );
    }

    // Functional column nets to zero (DR 900 + DR 180 / CR 1080) — no drift, no
    // synthesized FX gain/loss.
    let func_net: Decimal = reversal
        .lines
        .iter()
        .map(|l| {
            let f = l
                .functional_money
                .as_ref()
                .expect("cross-ccy leg carries functional")
                .amount();
            match l.side {
                Side::Debit => f,
                Side::Credit => -f,
            }
        })
        .sum();
    assert_eq!(
        func_net,
        Decimal::ZERO,
        "the reversal's functional column is balanced"
    );
}

#[test]
fn reverse_of_a_reversal_is_rejected() {
    let mut already_a_reversal = original_invoice();
    already_a_reversal.source_doc_type = SourceDocType::Reversal;
    let err = build_reversal(
        &already_a_reversal,
        "202607".to_owned(),
        naive(2026, 7, 2),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .expect_err("reversing a reversal must be rejected");
    assert_eq!(err, ReversalError::CannotReverseReversal);
}

#[test]
fn reverse_of_an_entry_with_a_reusable_credit_line_is_rejected() {
    // The read-back `LineView` does not carry `credit_grant_event_type`, so a
    // faithful reversal of a REUSABLE_CREDIT line cannot be reconstructed — the
    // guard must fail fast rather than abort at the DB CHECK.
    let mut with_credit = original_invoice();
    with_credit.lines.push(line(
        Uuid::now_v7(),
        AccountClass::ReusableCredit,
        Side::Credit,
        500,
    ));
    let err = build_reversal(
        &with_credit,
        "202607".to_owned(),
        naive(2026, 7, 2),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .expect_err("reversing an entry with a REUSABLE_CREDIT line must be rejected");
    assert_eq!(err, ReversalError::CreditGrantNotReconstructible);
}

#[test]
fn correction_id_is_deterministic_for_the_same_pair() {
    let original = Uuid::now_v7();
    let reversal = Uuid::now_v7();
    assert_eq!(
        correction_id(original, reversal),
        correction_id(original, reversal),
        "the same (original, reversal) pair must hash identically"
    );
    // 64 hex chars (SHA-256).
    assert_eq!(correction_id(original, reversal).len(), 64);
}

#[test]
fn correction_id_differs_for_different_inputs() {
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let c = Uuid::now_v7();
    assert_ne!(
        correction_id(a, b),
        correction_id(a, c),
        "a different reversal id must yield a different correction id"
    );
    // Order-sensitive: swapping the pair changes the id.
    assert_ne!(
        correction_id(a, b),
        correction_id(b, a),
        "correction_id must be order-sensitive"
    );
}

#[test]
fn mapping_correction_keys_on_invoice_and_correction_id() {
    let original = original_invoice();
    let reversal_entry_id = Uuid::now_v7();
    let correction = correction_id(original.entry_id, reversal_entry_id);
    let corrected = build_mapping_correction(
        &original,
        reversal_entry_id,
        "INV-1",
        "202607".to_owned(),
        naive(2026, 7, 2),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(corrected.source_doc_type, SourceDocType::MappingCorrection);
    assert_eq!(
        corrected.source_business_id,
        format!("INV-1:{correction}"),
        "MAPPING_CORRECTION keys on invoice_id:correction_id"
    );
    assert_eq!(
        corrected.reverses_entry_id,
        Some(reversal_entry_id),
        "the correction points back at the reversal it follows"
    );
}

use bss_ledger_sdk::money::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;

/// Preserve these legacy scale-2 fixture economics as explicit major-unit money.
fn money(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

#[test]
fn reversal_copies_distinct_stored_specs_and_exact_values() {
    let mut original = original_invoice();
    for l in &mut original.lines {
        l.money = PostedMoney::try_new(
            l.money.amount(),
            CurrencySpec::try_new("USD".to_owned(), 3).unwrap(),
        )
        .unwrap();
        l.functional_money = Some(
            PostedMoney::try_new(
                l.money.amount() * Decimal::new(9, 1),
                CurrencySpec::try_new("EUR".to_owned(), 4).unwrap(),
            )
            .unwrap(),
        );
    }
    let reversed = build_reversal(
        &original,
        "202607".to_owned(),
        naive(2026, 7, 2),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .unwrap();
    for (stored, copied) in original.lines.iter().zip(&reversed.lines) {
        assert_eq!(copied.money, stored.money);
        assert_eq!(copied.functional_money, stored.functional_money);
        assert_eq!(copied.money.currency().scale(), 3);
        assert_eq!(
            copied.functional_money.as_ref().unwrap().currency().scale(),
            4
        );
        assert_eq!(copied.side, flip(stored.side));
    }
}

#[test]
fn reversal_rejects_header_code_or_transaction_scale_conflict() {
    let mut original = original_invoice();
    original.entry_currency = "EUR".to_owned();
    assert_eq!(
        build_reversal(
            &original,
            "202607".to_owned(),
            naive(2026, 7, 2),
            Uuid::now_v7(),
            Uuid::now_v7()
        )
        .unwrap_err(),
        ReversalError::Money(MoneyError::CurrencyMismatch)
    );
    original.entry_currency = "USD".to_owned();
    original.lines[0].money = PostedMoney::try_new(
        Decimal::new(1200, 2),
        CurrencySpec::try_new("USD".to_owned(), 3).unwrap(),
    )
    .unwrap();
    assert_eq!(
        build_reversal(
            &original,
            "202607".to_owned(),
            naive(2026, 7, 2),
            Uuid::now_v7(),
            Uuid::now_v7()
        )
        .unwrap_err(),
        ReversalError::Money(MoneyError::ScaleMismatch)
    );
}

#[test]
fn mapping_correction_rejects_money_that_conflicts_with_header() {
    let original = original_invoice();
    let mut corrected = flip_line(&original.lines[0]);
    corrected.money = PostedMoney::try_new(
        corrected.money.amount(),
        CurrencySpec::try_new("EUR".to_owned(), 2).unwrap(),
    )
    .unwrap();
    assert_eq!(
        build_mapping_correction(
            &original,
            Uuid::now_v7(),
            "INV-1",
            "202607".to_owned(),
            naive(2026, 7, 2),
            Uuid::now_v7(),
            Uuid::now_v7(),
            vec![corrected]
        )
        .unwrap_err(),
        ReversalError::Money(MoneyError::CurrencyMismatch)
    );
}

#[test]
fn reversal_admits_functional_only_lines_in_another_currency() {
    // FX revaluation posts zero-amount lines in the functional currency next to
    // the entry currency; the posting engine admits them, so a reversal must too.
    let mut original = original_invoice();
    let eur = |cents: i64| {
        PostedMoney::try_new(
            Decimal::new(cents, 2),
            CurrencySpec::try_new("EUR".to_owned(), 2).unwrap(),
        )
        .unwrap()
    };
    let mut functional_only = line(Uuid::now_v7(), AccountClass::Ar, Side::Debit, 0);
    functional_only.money = eur(0);
    functional_only.functional_money = Some(eur(500));
    original.lines.push(functional_only);
    let reversal = build_reversal(
        &original,
        "202607".to_owned(),
        naive(2026, 7, 1),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .unwrap();
    assert_eq!(reversal.lines.len(), original.lines.len());
    // A non-zero line in another currency is still refused.
    let mut foreign = original_invoice();
    foreign.lines[0].money = eur(1200);
    assert!(matches!(
        build_reversal(
            &foreign,
            "202607".to_owned(),
            naive(2026, 7, 1),
            Uuid::now_v7(),
            Uuid::now_v7(),
        ),
        Err(ReversalError::Money(MoneyError::CurrencyMismatch))
    ));
}
