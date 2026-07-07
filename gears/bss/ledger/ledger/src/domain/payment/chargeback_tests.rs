use super::*;

/// A fee-0 net: the cash leg equals the disputed (gross) amount — `net = gross`
/// when no PSP fee. The cash-hold tests size their legs at the `net` passed as the
/// builder's 2nd arg (Model N); with fee 0 that is the same 1000 as `disputed`.
const NET_NO_FEE: i64 = 1000;

fn base(variant: DisputeVariant, phase: DisputePhase) -> ChargebackInput {
    ChargebackInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "PAY-1".to_owned(),
        dispute_id: "DSP-1".to_owned(),
        cycle: 1,
        phase,
        variant,
        disputed_amount_minor: 1000,
        invoice_id: None,
        currency: "USD".to_owned(),
        effective_at: None,
    }
}

/// Σ of the debit-side line amounts (widened, mirroring the engine's i128).
fn sum_dr(entry: &PostEntry) -> i128 {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| i128::from(l.amount_minor))
        .sum()
}

/// Σ of the credit-side line amounts.
fn sum_cr(entry: &PostEntry) -> i128 {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| i128::from(l.amount_minor))
        .sum()
}

/// The single line of `class` + `side` (panics if not exactly one — guards a
/// test against a leg silently appearing/vanishing).
#[allow(clippy::panic)] // a test assertion helper — a missing leg should fail loud
fn line(entry: &PostEntry, class: AccountClass, side: Side) -> &PostLine {
    let mut it = entry
        .lines
        .iter()
        .filter(|l| l.account_class == class && l.side == side);
    let found = it.next().unwrap_or_else(|| {
        panic!("expected a {side:?} {class:?} line");
    });
    assert!(
        it.next().is_none(),
        "expected exactly one {side:?} {class:?} line"
    );
    found
}

#[test]
fn cash_hold_opened_moves_cash_into_hold() {
    let inp = base(DisputeVariant::CashHold, DisputePhase::Opened);
    // Model N: the cash legs are sized at the 2nd arg (`net`); fee 0 ⇒ net = 1000.
    let entry = build_chargeback_entry(&inp, NET_NO_FEE).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::Chargeback);
    // business id is the snake_case composite `dispute_id:cycle:phase`.
    assert_eq!(entry.source_business_id, "DSP-1:1:OPENED");
    assert_eq!(entry.reverses_entry_id, None);
    assert_eq!(entry.lines.len(), 2);

    // DR DISPUTE_HOLD (cash parked in the hold), sized at net.
    let hold = &entry.lines[0];
    assert_eq!(hold.account_class, AccountClass::DisputeHold);
    assert_eq!(hold.side, Side::Debit);
    assert_eq!(hold.amount_minor, NET_NO_FEE);
    assert_eq!(hold.invoice_id, None);
    assert_eq!(hold.ar_status, None);

    // CR CASH_CLEARING (cash leaves clearing), sized at net.
    let cash = &entry.lines[1];
    assert_eq!(cash.account_class, AccountClass::CashClearing);
    assert_eq!(cash.side, Side::Credit);
    assert_eq!(cash.amount_minor, NET_NO_FEE);

    // Balanced.
    assert_eq!(sum_dr(&entry), i128::from(NET_NO_FEE));
    assert_eq!(sum_cr(&entry), i128::from(NET_NO_FEE));

    // Every line carries the payer, currency, and seller.
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.currency, "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
    }
}

#[test]
fn ar_reclass_opened_reclasses_active_to_disputed() {
    let mut inp = base(DisputeVariant::ArReclass, DisputePhase::Opened);
    inp.invoice_id = Some("INV-7".to_owned());
    // AR-reclass ignores the 2nd arg (no PSP fee ⇒ gross = net = the receivable).
    let entry = build_chargeback_entry(&inp, NET_NO_FEE).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::Chargeback);
    assert_eq!(entry.source_business_id, "DSP-1:1:OPENED");
    assert_eq!(entry.lines.len(), 2);

    // DR AR DISPUTED (the disputed portion).
    let disputed = &entry.lines[0];
    assert_eq!(disputed.account_class, AccountClass::Ar);
    assert_eq!(disputed.side, Side::Debit);
    assert_eq!(disputed.amount_minor, 1000);
    assert_eq!(disputed.invoice_id.as_deref(), Some("INV-7"));
    assert_eq!(disputed.ar_status.as_deref(), Some(AR_STATUS_DISPUTED));

    // CR AR ACTIVE (removed from the active portion).
    let active = &entry.lines[1];
    assert_eq!(active.account_class, AccountClass::Ar);
    assert_eq!(active.side, Side::Credit);
    assert_eq!(active.amount_minor, 1000);
    assert_eq!(active.invoice_id.as_deref(), Some("INV-7"));
    assert_eq!(active.ar_status.as_deref(), Some(AR_STATUS_ACTIVE));

    // Both AR legs share the SAME (payer, invoice) grain and net ZERO on
    // balance_minor (DR raises, CR lowers AR by the same amount).
    assert_eq!(sum_dr(&entry), 1000);
    assert_eq!(sum_cr(&entry), 1000);
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.currency, "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
        assert_eq!(l.invoice_id.as_deref(), Some("INV-7"));
    }
}

#[test]
fn ar_reclass_opened_without_invoice_is_rejected() {
    // invoice_id stays None — an AR reclass has no receivable to move.
    let inp = base(DisputeVariant::ArReclass, DisputePhase::Opened);
    let err = build_chargeback_entry(&inp, NET_NO_FEE).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn zero_amount_is_rejected() {
    let mut inp = base(DisputeVariant::CashHold, DisputePhase::Opened);
    inp.disputed_amount_minor = 0;
    let err = build_chargeback_entry(&inp, NET_NO_FEE).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn negative_amount_is_rejected() {
    let mut inp = base(DisputeVariant::CashHold, DisputePhase::Opened);
    inp.disputed_amount_minor = -1;
    let err = build_chargeback_entry(&inp, NET_NO_FEE).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

// ── won (both variants) ──────────────────────────────────────────────────────

#[test]
fn cash_hold_won_releases_hold_to_clearing() {
    let inp = base(DisputeVariant::CashHold, DisputePhase::Won);
    // Model N: the released cash legs are sized at net; fee 0 ⇒ net = 1000.
    let entry = build_chargeback_entry(&inp, NET_NO_FEE).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:WON");
    assert_eq!(entry.lines.len(), 2);

    // DR CASH_CLEARING (cash back to clearing) + CR DISPUTE_HOLD (release hold).
    let cash = line(&entry, AccountClass::CashClearing, Side::Debit);
    assert_eq!(cash.amount_minor, NET_NO_FEE);
    let hold = line(&entry, AccountClass::DisputeHold, Side::Credit);
    assert_eq!(hold.amount_minor, NET_NO_FEE);
    // No AR, no loss leg.
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::Ar)
    );
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::DisputeLossExpense)
    );
    assert_eq!(sum_dr(&entry), i128::from(NET_NO_FEE));
    assert_eq!(sum_cr(&entry), i128::from(NET_NO_FEE));
    // No cash clawed back on a won.
    assert_eq!(clawed_back_on_post(&inp, NET_NO_FEE), 0);
}

#[test]
fn ar_reclass_won_reclasses_disputed_to_active() {
    let mut inp = base(DisputeVariant::ArReclass, DisputePhase::Won);
    inp.invoice_id = Some("INV-7".to_owned());
    // AR-reclass ignores the 2nd arg.
    let entry = build_chargeback_entry(&inp, NET_NO_FEE).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:WON");
    assert_eq!(entry.lines.len(), 2);

    // DR AR ACTIVE (restore) + CR AR DISPUTED (clear the disputed slice). The
    // reverse of opened: the DISPUTED leg is now the CREDIT (−D on disputed_minor).
    let active = line(&entry, AccountClass::Ar, Side::Debit);
    assert_eq!(active.ar_status.as_deref(), Some(AR_STATUS_ACTIVE));
    assert_eq!(active.invoice_id.as_deref(), Some("INV-7"));
    let disputed = line(&entry, AccountClass::Ar, Side::Credit);
    assert_eq!(disputed.ar_status.as_deref(), Some(AR_STATUS_DISPUTED));
    assert_eq!(disputed.invoice_id.as_deref(), Some("INV-7"));
    // Balanced, AR-class-neutral, no cash leg.
    assert_eq!(sum_dr(&entry), i128::from(NET_NO_FEE));
    assert_eq!(sum_cr(&entry), i128::from(NET_NO_FEE));
    assert_eq!(clawed_back_on_post(&inp, NET_NO_FEE), 0);
}

#[test]
fn ar_reclass_won_without_invoice_is_rejected() {
    let inp = base(DisputeVariant::ArReclass, DisputePhase::Won);
    let err = build_chargeback_entry(&inp, NET_NO_FEE).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

// ── lost (both variants) ─────────────────────────────────────────────────────

#[test]
fn cash_hold_lost_forfeits_hold_as_loss() {
    let inp = base(DisputeVariant::CashHold, DisputePhase::Lost);
    // Model N: the forfeiture legs are sized at net; fee 0 ⇒ net = 1000.
    let entry = build_chargeback_entry(&inp, NET_NO_FEE).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:LOST");
    assert_eq!(entry.lines.len(), 2);

    // DR DISPUTE_LOSS_EXPENSE (forfeit) + CR DISPUTE_HOLD (release hold). The
    // withheld cash left CASH_CLEARING at open, so clearing is NOT touched here.
    let loss = line(&entry, AccountClass::DisputeLossExpense, Side::Debit);
    assert_eq!(loss.amount_minor, NET_NO_FEE);
    let hold = line(&entry, AccountClass::DisputeHold, Side::Credit);
    assert_eq!(hold.amount_minor, NET_NO_FEE);
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::CashClearing),
        "cash-hold lost must not touch CASH_CLEARING (funds left at open)"
    );
    assert_eq!(sum_dr(&entry), i128::from(NET_NO_FEE));
    assert_eq!(sum_cr(&entry), i128::from(NET_NO_FEE));
    // The held (net) funds are clawed back; CASH_CLEARING is never touched.
    assert_eq!(clawed_back_on_post(&inp, NET_NO_FEE), NET_NO_FEE);
}

#[test]
fn cash_hold_legs_are_sized_at_net_not_gross() {
    // The spec's worked example (Model N): a CASH_HOLD dispute over a payment
    // settled at gross 100 with a PSP fee of 3 ⇒ CASH_CLEARING only ever held
    // `net = 97`. The disputed (gross) claim is 100, but EVERY cash leg
    // (opened/won/lost) is sized at the `net` the orchestrator threads in (97),
    // NOT the gross — sizing at gross would underflow CASH_CLEARING by the fee.
    const GROSS: i64 = 100;
    const NET: i64 = 97; // 100 − 3 fee

    // opened: DR DISPUTE_HOLD 97 / CR CASH_CLEARING 97.
    let mut opened = base(DisputeVariant::CashHold, DisputePhase::Opened);
    opened.disputed_amount_minor = GROSS;
    let entry = build_chargeback_entry(&opened, NET).unwrap();
    let hold = line(&entry, AccountClass::DisputeHold, Side::Debit);
    let cash = line(&entry, AccountClass::CashClearing, Side::Credit);
    assert_eq!(
        hold.amount_minor, NET,
        "opened DISPUTE_HOLD sized at net, not gross"
    );
    assert_eq!(
        cash.amount_minor, NET,
        "opened CASH_CLEARING credit sized at net"
    );
    assert_eq!(sum_dr(&entry), i128::from(NET));
    assert_eq!(sum_cr(&entry), i128::from(NET));

    // won: DR CASH_CLEARING 97 / CR DISPUTE_HOLD 97 (the reverse, also net).
    let mut won = base(DisputeVariant::CashHold, DisputePhase::Won);
    won.disputed_amount_minor = GROSS;
    let entry = build_chargeback_entry(&won, NET).unwrap();
    assert_eq!(
        line(&entry, AccountClass::CashClearing, Side::Debit).amount_minor,
        NET
    );
    assert_eq!(
        line(&entry, AccountClass::DisputeHold, Side::Credit).amount_minor,
        NET
    );
    assert_eq!(sum_dr(&entry), i128::from(NET));
    assert_eq!(sum_cr(&entry), i128::from(NET));
    assert_eq!(
        clawed_back_on_post(&won, NET),
        0,
        "a won claws nothing back"
    );

    // lost: DR DISPUTE_LOSS_EXPENSE 97 / CR DISPUTE_HOLD 97; clawed_back = net.
    let mut lost = base(DisputeVariant::CashHold, DisputePhase::Lost);
    lost.disputed_amount_minor = GROSS;
    let entry = build_chargeback_entry(&lost, NET).unwrap();
    assert_eq!(
        line(&entry, AccountClass::DisputeLossExpense, Side::Debit).amount_minor,
        NET
    );
    assert_eq!(
        line(&entry, AccountClass::DisputeHold, Side::Credit).amount_minor,
        NET
    );
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::CashClearing),
        "cash-hold lost posts no CASH_CLEARING leg"
    );
    assert_eq!(sum_dr(&entry), i128::from(NET));
    assert_eq!(sum_cr(&entry), i128::from(NET));
    // The dispute-loss leg is `net` (97); the fee (3) was already expensed at
    // settle, so the total loss is net + fee = gross (100). `clawed_back` bumps by
    // net, not gross.
    assert_eq!(
        clawed_back_on_post(&lost, NET),
        NET,
        "(Lost, CashHold) claws back net (97), not gross"
    );
}

#[test]
fn ar_reclass_lost_writes_receivable_off_to_loss() {
    let mut inp = base(DisputeVariant::ArReclass, DisputePhase::Lost);
    inp.invoice_id = Some("INV-7".to_owned());
    // AR-reclass = funds not_moved (invoice/ACH, NO PSP fee), so the 2nd arg is
    // ignored. A lost dispute writes the receivable off to loss — no cash leg.
    let entry = build_chargeback_entry(&inp, NET_NO_FEE).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:LOST");
    // Exactly two legs: book the loss + write the disputed receivable off.
    assert_eq!(entry.lines.len(), 2);

    // DR DISPUTE_LOSS_EXPENSE at the disputed amount (the loss the seller eats).
    let loss = line(&entry, AccountClass::DisputeLossExpense, Side::Debit);
    assert_eq!(loss.amount_minor, 1000);
    // CR AR with ar_status = DISPUTED at the disputed amount: a LONE credit AR
    // line that nets −D on BOTH balance_minor and disputed_minor (the projector
    // routes the signed DISPUTED delta onto both), so no extra balance leg.
    let ar = line(&entry, AccountClass::Ar, Side::Credit);
    assert_eq!(ar.ar_status.as_deref(), Some(AR_STATUS_DISPUTED));
    assert_eq!(ar.invoice_id.as_deref(), Some("INV-7"));
    assert_eq!(ar.amount_minor, 1000);

    // NO CASH_CLEARING leg (nothing was ever collected to claw back).
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::CashClearing),
        "a write-off posts no CASH_CLEARING leg"
    );
    // NO DR AR ACTIVE leg — the write-off does NOT re-open the receivable to
    // active; the sole AR line is the lone CR DISPUTED above.
    assert!(
        entry
            .lines
            .iter()
            .all(|l| !(l.account_class == AccountClass::Ar && l.side == Side::Debit)),
        "a write-off posts no DR AR ACTIVE leg"
    );

    // Balanced; a write-off claws nothing back (no cash ever moved).
    assert_eq!(sum_dr(&entry), 1000);
    assert_eq!(sum_cr(&entry), 1000);
    assert_eq!(clawed_back_on_post(&inp, NET_NO_FEE), 0);
}

#[test]
fn ar_reclass_lost_without_invoice_is_rejected() {
    let inp = base(DisputeVariant::ArReclass, DisputePhase::Lost);
    let err = build_chargeback_entry(&inp, NET_NO_FEE).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn partial_is_deferred_behind_a_flag() {
    for variant in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
        let inp = base(variant, DisputePhase::Partial);
        let err = build_chargeback_entry(&inp, NET_NO_FEE).unwrap_err();
        assert!(
            matches!(err, DomainError::InvalidDisputeTransition(_)),
            "partial must be InvalidDisputeTransition, got {err:?}"
        );
    }
}

#[test]
fn business_id_uses_the_cycle_and_phase() {
    let mut inp = base(DisputeVariant::CashHold, DisputePhase::Opened);
    inp.cycle = 2;
    assert_eq!(inp.business_id(), "DSP-1:2:OPENED");
}

#[test]
fn funds_at_open_selects_the_variant() {
    assert_eq!(FundsAtOpen::Withheld.variant(), DisputeVariant::CashHold);
    assert_eq!(FundsAtOpen::NotMoved.variant(), DisputeVariant::ArReclass);
}

#[test]
fn enum_literals_round_trip() {
    for v in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
        assert_eq!(DisputeVariant::parse(v.as_str()), Some(v));
    }
    for p in [
        DisputePhase::Opened,
        DisputePhase::Won,
        DisputePhase::Lost,
        DisputePhase::Partial,
    ] {
        assert_eq!(DisputePhase::parse(p.as_str()), Some(p));
    }
    for f in [FundsAtOpen::Withheld, FundsAtOpen::NotMoved] {
        assert_eq!(FundsAtOpen::parse(f.as_str()), Some(f));
    }
    assert_eq!(DisputeVariant::parse("NOPE"), None);
    assert_eq!(DisputePhase::parse("NOPE"), None);
    assert_eq!(FundsAtOpen::parse("nope"), None);
}

#[test]
fn parse_is_case_insensitive() {
    // Every wire literal is accepted in any case — the REST DTO documents the
    // lowercase form, the stored/journal form is the canonical `as_str` case, and
    // a client should not 400 over casing.
    assert_eq!(DisputePhase::parse("opened"), Some(DisputePhase::Opened));
    assert_eq!(DisputePhase::parse("Won"), Some(DisputePhase::Won));
    assert_eq!(DisputePhase::parse("LoSt"), Some(DisputePhase::Lost));
    assert_eq!(
        DisputeVariant::parse("cash_hold"),
        Some(DisputeVariant::CashHold)
    );
    assert_eq!(
        DisputeVariant::parse("Ar_Reclass"),
        Some(DisputeVariant::ArReclass)
    );
    assert_eq!(FundsAtOpen::parse("WITHHELD"), Some(FundsAtOpen::Withheld));
    assert_eq!(FundsAtOpen::parse("Not_Moved"), Some(FundsAtOpen::NotMoved));
}
