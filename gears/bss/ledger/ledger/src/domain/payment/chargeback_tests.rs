use super::*;

/// A fee-0 net: the cash leg equals the disputed (gross) amount — `net = gross`
/// when no PSP fee. The cash-hold tests size their legs at the `net` passed as the
/// builder's 2nd arg (Model N); with fee 0 that is the same 10.00 as `disputed`.
fn net_no_fee() -> PostedMoney {
    money("10", "USD", 2)
}

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        text.parse().unwrap(),
        bss_ledger_sdk::CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn base(variant: DisputeVariant, phase: DisputePhase) -> ChargebackInput {
    ChargebackInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "PAY-1".to_owned(),
        dispute_id: "DSP-1".to_owned(),
        cycle: 1,
        phase,
        variant,
        disputed_amount: money("10", "USD", 2),
        invoice_id: None,
        effective_at: None,
    }
}

/// Debit total in major units; every chargeback has exactly one debit leg.
fn sum_dr(entry: &PostEntry) -> Decimal {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| l.money.amount())
        .sum()
}

/// Σ of the credit-side line amounts.
fn sum_cr(entry: &PostEntry) -> Decimal {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| l.money.amount())
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
    // Model N: the cash legs are sized at the 2nd arg (`net`); fee 0 ⇒ net = 10.00.
    let entry = build_chargeback_entry(&inp, &net_no_fee()).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::Chargeback);
    // business id is the snake_case composite `dispute_id:cycle:phase`.
    assert_eq!(entry.source_business_id, "DSP-1:1:OPENED");
    assert_eq!(entry.reverses_entry_id, None);
    assert_eq!(entry.lines.len(), 2);

    // DR DISPUTE_HOLD (cash parked in the hold), sized at net.
    let hold = &entry.lines[0];
    assert_eq!(hold.account_class, AccountClass::DisputeHold);
    assert_eq!(hold.side, Side::Debit);
    assert_eq!(hold.money, net_no_fee());
    assert_eq!(hold.invoice_id, None);
    assert_eq!(hold.ar_status, None);

    // CR CASH_CLEARING (cash leaves clearing), sized at net.
    let cash = &entry.lines[1];
    assert_eq!(cash.account_class, AccountClass::CashClearing);
    assert_eq!(cash.side, Side::Credit);
    assert_eq!(cash.money, net_no_fee());

    // Balanced.
    assert_eq!(sum_dr(&entry), net_no_fee().amount());
    assert_eq!(sum_cr(&entry), net_no_fee().amount());

    // Every line carries the payer, currency, and seller.
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.money.currency().code(), "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
    }
}

#[test]
fn ar_reclass_opened_reclasses_active_to_disputed() {
    let mut inp = base(DisputeVariant::ArReclass, DisputePhase::Opened);
    inp.invoice_id = Some("INV-7".to_owned());
    // AR-reclass ignores the 2nd arg (no PSP fee ⇒ gross = net = the receivable).
    let entry = build_chargeback_entry(&inp, &net_no_fee()).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::Chargeback);
    assert_eq!(entry.source_business_id, "DSP-1:1:OPENED");
    assert_eq!(entry.lines.len(), 2);

    // DR AR DISPUTED (the disputed portion).
    let disputed = &entry.lines[0];
    assert_eq!(disputed.account_class, AccountClass::Ar);
    assert_eq!(disputed.side, Side::Debit);
    assert_eq!(disputed.money, net_no_fee());
    assert_eq!(disputed.invoice_id.as_deref(), Some("INV-7"));
    assert_eq!(disputed.ar_status.as_deref(), Some(AR_STATUS_DISPUTED));

    // CR AR ACTIVE (removed from the active portion).
    let active = &entry.lines[1];
    assert_eq!(active.account_class, AccountClass::Ar);
    assert_eq!(active.side, Side::Credit);
    assert_eq!(active.money, net_no_fee());
    assert_eq!(active.invoice_id.as_deref(), Some("INV-7"));
    assert_eq!(active.ar_status.as_deref(), Some(AR_STATUS_ACTIVE));

    // Both AR legs share the SAME (payer, invoice) grain and net ZERO on
    // balance_minor (DR raises, CR lowers AR by the same amount).
    assert_eq!(sum_dr(&entry), net_no_fee().amount());
    assert_eq!(sum_cr(&entry), net_no_fee().amount());
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.money.currency().code(), "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
        assert_eq!(l.invoice_id.as_deref(), Some("INV-7"));
    }
}

#[test]
fn ar_reclass_opened_without_invoice_is_rejected() {
    // invoice_id stays None — an AR reclass has no receivable to move.
    let inp = base(DisputeVariant::ArReclass, DisputePhase::Opened);
    let err = build_chargeback_entry(&inp, &net_no_fee()).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn zero_amount_is_rejected() {
    let mut inp = base(DisputeVariant::CashHold, DisputePhase::Opened);
    inp.disputed_amount = money("0", "USD", 2);
    let err = build_chargeback_entry(&inp, &net_no_fee()).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn negative_amount_is_rejected() {
    let mut inp = base(DisputeVariant::CashHold, DisputePhase::Opened);
    inp.disputed_amount = money("-0.01", "USD", 2);
    let err = build_chargeback_entry(&inp, &net_no_fee()).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

// ── won (both variants) ──────────────────────────────────────────────────────

#[test]
fn cash_hold_won_releases_hold_to_clearing() {
    let inp = base(DisputeVariant::CashHold, DisputePhase::Won);
    // Model N: the released cash legs are sized at net; fee 0 ⇒ net = 10.00.
    let entry = build_chargeback_entry(&inp, &net_no_fee()).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:WON");
    assert_eq!(entry.lines.len(), 2);

    // DR CASH_CLEARING (cash back to clearing) + CR DISPUTE_HOLD (release hold).
    let cash = line(&entry, AccountClass::CashClearing, Side::Debit);
    assert_eq!(cash.money, net_no_fee());
    let hold = line(&entry, AccountClass::DisputeHold, Side::Credit);
    assert_eq!(hold.money, net_no_fee());
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
    assert_eq!(sum_dr(&entry), net_no_fee().amount());
    assert_eq!(sum_cr(&entry), net_no_fee().amount());
    // No cash clawed back on a won.
    assert_eq!(
        clawed_back_on_post(&inp, &net_no_fee()).unwrap(),
        money("0", "USD", 2)
    );
}

#[test]
fn ar_reclass_won_reclasses_disputed_to_active() {
    let mut inp = base(DisputeVariant::ArReclass, DisputePhase::Won);
    inp.invoice_id = Some("INV-7".to_owned());
    // AR-reclass ignores the 2nd arg.
    let entry = build_chargeback_entry(&inp, &net_no_fee()).unwrap();
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
    assert_eq!(sum_dr(&entry), net_no_fee().amount());
    assert_eq!(sum_cr(&entry), net_no_fee().amount());
    assert_eq!(
        clawed_back_on_post(&inp, &net_no_fee()).unwrap(),
        money("0", "USD", 2)
    );
}

#[test]
fn ar_reclass_won_without_invoice_is_rejected() {
    let inp = base(DisputeVariant::ArReclass, DisputePhase::Won);
    let err = build_chargeback_entry(&inp, &net_no_fee()).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

// ── lost (both variants) ─────────────────────────────────────────────────────

#[test]
fn cash_hold_lost_forfeits_hold_as_loss() {
    let inp = base(DisputeVariant::CashHold, DisputePhase::Lost);
    // Model N: the forfeiture legs are sized at net; fee 0 ⇒ net = 10.00.
    let entry = build_chargeback_entry(&inp, &net_no_fee()).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:LOST");
    assert_eq!(entry.lines.len(), 2);

    // DR DISPUTE_LOSS_EXPENSE (forfeit) + CR DISPUTE_HOLD (release hold). The
    // withheld cash left CASH_CLEARING at open, so clearing is NOT touched here.
    let loss = line(&entry, AccountClass::DisputeLossExpense, Side::Debit);
    assert_eq!(loss.money, net_no_fee());
    let hold = line(&entry, AccountClass::DisputeHold, Side::Credit);
    assert_eq!(hold.money, net_no_fee());
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::CashClearing),
        "cash-hold lost must not touch CASH_CLEARING (funds left at open)"
    );
    assert_eq!(sum_dr(&entry), net_no_fee().amount());
    assert_eq!(sum_cr(&entry), net_no_fee().amount());
    // The held (net) funds are clawed back; CASH_CLEARING is never touched.
    assert_eq!(
        clawed_back_on_post(&inp, &net_no_fee()).unwrap(),
        net_no_fee()
    );
}

#[test]
fn cash_hold_legs_are_sized_at_net_not_gross() {
    // The spec's worked example (Model N): a CASH_HOLD dispute over a payment
    // settled at gross 1.00 with a PSP fee of 0.03 ⇒ CASH_CLEARING only ever held
    // `net = 0.97`. The disputed (gross) claim is 100, but EVERY cash leg
    // (opened/won/lost) is sized at the `net` the orchestrator threads in (0.97),
    // NOT the gross — sizing at gross would underflow CASH_CLEARING by the fee.
    let gross = money("1", "USD", 2);
    let net = money("0.97", "USD", 2); // 1.00 − 0.03 fee

    // opened: DR DISPUTE_HOLD 0.97 / CR CASH_CLEARING 0.97.
    let mut opened = base(DisputeVariant::CashHold, DisputePhase::Opened);
    opened.disputed_amount = gross.clone();
    let entry = build_chargeback_entry(&opened, &net).unwrap();
    let hold = line(&entry, AccountClass::DisputeHold, Side::Debit);
    let cash = line(&entry, AccountClass::CashClearing, Side::Credit);
    assert_eq!(
        hold.money, net,
        "opened DISPUTE_HOLD sized at net, not gross"
    );
    assert_eq!(cash.money, net, "opened CASH_CLEARING credit sized at net");
    assert_eq!(sum_dr(&entry), net.amount());
    assert_eq!(sum_cr(&entry), net.amount());

    // won: DR CASH_CLEARING 0.97 / CR DISPUTE_HOLD 0.97 (the reverse, also net).
    let mut won = base(DisputeVariant::CashHold, DisputePhase::Won);
    won.disputed_amount = gross.clone();
    let entry = build_chargeback_entry(&won, &net).unwrap();
    assert_eq!(
        line(&entry, AccountClass::CashClearing, Side::Debit).money,
        net
    );
    assert_eq!(
        line(&entry, AccountClass::DisputeHold, Side::Credit).money,
        net
    );
    assert_eq!(sum_dr(&entry), net.amount());
    assert_eq!(sum_cr(&entry), net.amount());
    assert_eq!(
        clawed_back_on_post(&won, &net).unwrap(),
        money("0", "USD", 2),
        "a won claws nothing back"
    );

    // lost: DR DISPUTE_LOSS_EXPENSE 0.97 / CR DISPUTE_HOLD 0.97; clawed_back = net.
    let mut lost = base(DisputeVariant::CashHold, DisputePhase::Lost);
    lost.disputed_amount = gross.clone();
    let entry = build_chargeback_entry(&lost, &net).unwrap();
    assert_eq!(
        line(&entry, AccountClass::DisputeLossExpense, Side::Debit).money,
        net
    );
    assert_eq!(
        line(&entry, AccountClass::DisputeHold, Side::Credit).money,
        net
    );
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::CashClearing),
        "cash-hold lost posts no CASH_CLEARING leg"
    );
    assert_eq!(sum_dr(&entry), net.amount());
    assert_eq!(sum_cr(&entry), net.amount());
    // The dispute-loss leg is `net` (0.97); the fee (0.03) was already expensed at
    // settle, so the total loss is net + fee = gross (1.00). `clawed_back` bumps by
    // net, not gross.
    assert_eq!(
        clawed_back_on_post(&lost, &net).unwrap(),
        net,
        "(Lost, CashHold) claws back net (0.97), not gross"
    );
}

#[test]
fn ar_reclass_lost_writes_receivable_off_to_loss() {
    let mut inp = base(DisputeVariant::ArReclass, DisputePhase::Lost);
    inp.invoice_id = Some("INV-7".to_owned());
    // AR-reclass = funds not_moved (invoice/ACH, NO PSP fee), so the 2nd arg is
    // ignored. A lost dispute writes the receivable off to loss — no cash leg.
    let entry = build_chargeback_entry(&inp, &net_no_fee()).unwrap();
    assert_eq!(entry.source_business_id, "DSP-1:1:LOST");
    // Exactly two legs: book the loss + write the disputed receivable off.
    assert_eq!(entry.lines.len(), 2);

    // DR DISPUTE_LOSS_EXPENSE at the disputed amount (the loss the seller eats).
    let loss = line(&entry, AccountClass::DisputeLossExpense, Side::Debit);
    assert_eq!(loss.money, net_no_fee());
    // CR AR with ar_status = DISPUTED at the disputed amount: a LONE credit AR
    // line that nets −D on BOTH balance_minor and disputed_minor (the projector
    // routes the signed DISPUTED delta onto both), so no extra balance leg.
    let ar = line(&entry, AccountClass::Ar, Side::Credit);
    assert_eq!(ar.ar_status.as_deref(), Some(AR_STATUS_DISPUTED));
    assert_eq!(ar.invoice_id.as_deref(), Some("INV-7"));
    assert_eq!(ar.money, net_no_fee());

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
    assert_eq!(sum_dr(&entry), net_no_fee().amount());
    assert_eq!(sum_cr(&entry), net_no_fee().amount());
    assert_eq!(
        clawed_back_on_post(&inp, &net_no_fee()).unwrap(),
        money("0", "USD", 2)
    );
}

#[test]
fn ar_reclass_lost_without_invoice_is_rejected() {
    let inp = base(DisputeVariant::ArReclass, DisputePhase::Lost);
    let err = build_chargeback_entry(&inp, &net_no_fee()).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn partial_is_deferred_behind_a_flag() {
    for variant in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
        let inp = base(variant, DisputePhase::Partial);
        let err = build_chargeback_entry(&inp, &net_no_fee()).unwrap_err();
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

/// All supported phases retain their accounting legs for fractional major units.
#[test]
fn fractional_phase_variant_matrix_retains_legs_and_clawback() {
    for phase in [DisputePhase::Opened, DisputePhase::Won, DisputePhase::Lost] {
        for variant in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
            for cash in ["12", "15"] {
                let mut input = base(variant, phase);
                input.disputed_amount = money("12.34", "EUR", 2);
                input.invoice_id = Some("INV-FRACTION".to_owned());
                input.effective_at = Some(time::macros::datetime!(2026-10-09 12:00 UTC));
                let supplied = money(cash, "EUR", 2);
                let entry = build_chargeback_entry(&input, &supplied).unwrap();
                let expected = if variant == DisputeVariant::CashHold && cash == "12" {
                    money("12", "EUR", 2)
                } else {
                    input.disputed_amount.clone()
                };
                let (dr_class, dr_status, cr_class, cr_status) = match (phase, variant) {
                    (DisputePhase::Opened, DisputeVariant::CashHold) => (
                        AccountClass::DisputeHold,
                        None,
                        AccountClass::CashClearing,
                        None,
                    ),
                    (DisputePhase::Won, DisputeVariant::CashHold) => (
                        AccountClass::CashClearing,
                        None,
                        AccountClass::DisputeHold,
                        None,
                    ),
                    (DisputePhase::Lost, DisputeVariant::CashHold) => (
                        AccountClass::DisputeLossExpense,
                        None,
                        AccountClass::DisputeHold,
                        None,
                    ),
                    (DisputePhase::Opened, DisputeVariant::ArReclass) => (
                        AccountClass::Ar,
                        Some(AR_STATUS_DISPUTED),
                        AccountClass::Ar,
                        Some(AR_STATUS_ACTIVE),
                    ),
                    (DisputePhase::Won, DisputeVariant::ArReclass) => (
                        AccountClass::Ar,
                        Some(AR_STATUS_ACTIVE),
                        AccountClass::Ar,
                        Some(AR_STATUS_DISPUTED),
                    ),
                    (DisputePhase::Lost, DisputeVariant::ArReclass) => (
                        AccountClass::DisputeLossExpense,
                        None,
                        AccountClass::Ar,
                        Some(AR_STATUS_DISPUTED),
                    ),
                    _ => unreachable!(),
                };
                assert_eq!(entry.lines.len(), 2);
                assert_eq!(entry.lines[0].account_class, dr_class);
                assert_eq!(entry.lines[1].account_class, cr_class);
                assert_eq!(entry.lines[0].side, Side::Debit);
                assert_eq!(entry.lines[1].side, Side::Credit);
                assert_eq!(entry.lines[0].ar_status.as_deref(), dr_status);
                assert_eq!(entry.lines[1].ar_status.as_deref(), cr_status);
                assert_eq!(entry.source_business_id, input.business_id());
                assert_eq!(entry.entry_currency, "EUR");
                assert_eq!(entry.tenant_id, input.tenant_id);
                assert_eq!(
                    entry.effective_at,
                    NaiveDate::from_ymd_opt(2026, 10, 9).unwrap()
                );
                assert_eq!(entry.reverses_entry_id, None);
                assert_eq!(entry.reverses_period_id, None);
                assert_eq!(sum_dr(&entry), expected.amount());
                assert_eq!(sum_cr(&entry), expected.amount());
                for posted in &entry.lines {
                    assert_eq!(posted.money, expected);
                    assert_eq!(posted.functional_money, None);
                    assert_eq!(posted.payer_tenant_id, input.payer_tenant_id);
                    assert_eq!(posted.seller_tenant_id, Some(input.tenant_id));
                    assert_eq!(posted.account_id, Uuid::nil());
                    assert_eq!(posted.mapping_status, MappingStatus::Resolved);
                    assert_eq!(
                        posted.invoice_id.as_deref(),
                        if posted.account_class == AccountClass::Ar {
                            Some("INV-FRACTION")
                        } else {
                            None
                        }
                    );
                }
                assert_eq!(
                    clawed_back_on_post(&input, &supplied).unwrap(),
                    if phase == DisputePhase::Lost && variant == DisputeVariant::CashHold {
                        expected
                    } else {
                        money("0", "EUR", 2)
                    }
                );
            }
        }
    }
}

#[test]
fn money_metadata_precedes_zero_sign_and_phase_rejection() {
    for phase in [
        DisputePhase::Opened,
        DisputePhase::Won,
        DisputePhase::Lost,
        DisputePhase::Partial,
    ] {
        for variant in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
            for disputed in ["0", "-0.01", "12.34"] {
                let mut input = base(variant, phase);
                input.disputed_amount = money(disputed, "EUR", 2);
                // Includes an unused zero on AR reclass and no required invoice.
                for cash in ["0", "-0.01", "12"] {
                    let wrong_code = money(cash, "USD", 2);
                    let wrong_scale = money(cash, "EUR", 3);
                    assert!(matches!(
                        build_chargeback_entry(&input, &wrong_code),
                        Err(DomainError::CurrencyMismatch(_))
                    ));
                    assert!(matches!(
                        clawed_back_on_post(&input, &wrong_code),
                        Err(DomainError::CurrencyMismatch(_))
                    ));
                    assert!(matches!(
                        build_chargeback_entry(&input, &wrong_scale),
                        Err(DomainError::InconsistentScale(_))
                    ));
                    assert!(matches!(
                        clawed_back_on_post(&input, &wrong_scale),
                        Err(DomainError::InconsistentScale(_))
                    ));
                }
            }
        }
    }
}

#[test]
fn cash_hold_rejects_nonpositive_cash_in_every_supported_phase() {
    for phase in [DisputePhase::Opened, DisputePhase::Won, DisputePhase::Lost] {
        let input = base(DisputeVariant::CashHold, phase);
        for cash in ["0", "-0.01"] {
            let supplied = money(cash, "USD", 2);
            assert!(matches!(
                build_chargeback_entry(&input, &supplied),
                Err(DomainError::InvalidRequest(_))
            ));
            if phase == DisputePhase::Lost {
                assert!(matches!(
                    clawed_back_on_post(&input, &supplied),
                    Err(DomainError::InvalidRequest(_))
                ));
            }
        }
    }
}

#[test]
fn ar_reclass_ignores_cash_amount_but_keeps_invoice_requirement() {
    for phase in [DisputePhase::Opened, DisputePhase::Won, DisputePhase::Lost] {
        let mut input = base(DisputeVariant::ArReclass, phase);
        for cash in ["0", "-10", "9999999999999999999999999999"] {
            let supplied = money(cash, "USD", 2);
            input.invoice_id = None;
            assert!(matches!(
                build_chargeback_entry(&input, &supplied),
                Err(DomainError::InvalidRequest(_))
            ));
            input.invoice_id = Some("INV-AR".to_owned());
            let entry = build_chargeback_entry(&input, &supplied).unwrap();
            assert!(
                entry
                    .lines
                    .iter()
                    .all(|line| line.money == input.disputed_amount)
            );
            assert_eq!(
                clawed_back_on_post(&input, &supplied).unwrap(),
                money("0", "USD", 2)
            );
        }
    }
}

#[test]
fn outcomes_use_supplied_original_stored_hold_after_payment_reduction() {
    let mut input = base(DisputeVariant::CashHold, DisputePhase::Opened);
    input.disputed_amount = money("12.34", "EUR", 2);
    let opening_net = money("12", "EUR", 2);
    let opened = build_chargeback_entry(&input, &opening_net).unwrap();
    let stored_hold = opened.lines[0].money.clone();
    let later_payment_net = money("0", "EUR", 2);
    for phase in [DisputePhase::Won, DisputePhase::Lost] {
        input.phase = phase;
        let outcome = build_chargeback_entry(&input, &stored_hold).unwrap();
        assert!(outcome.lines.iter().all(|line| line.money == stored_hold));
        assert_eq!(sum_cr(&outcome), sum_dr(&opened));
        assert!(matches!(
            build_chargeback_entry(&input, &later_payment_net),
            Err(DomainError::InvalidRequest(_))
        ));
        assert_eq!(
            clawed_back_on_post(&input, &stored_hold).unwrap(),
            if phase == DisputePhase::Lost {
                stored_hold.clone()
            } else {
                money("0", "EUR", 2)
            }
        );
    }
}

#[test]
fn stored_scales_and_final_posted_bounds_are_preserved() {
    for (code, scale, disputed, cash) in [
        ("JPY", 0, "12", "10"),
        ("KWD", 3, "12.345", "12.001"),
        ("EUR", 3, "12.345", "12.001"), // historical scale, no registry replacement
        ("TOKEN", 8, "0.12345678", "0.10000001"),
        (
            "TOKEN",
            28,
            "0.0000000000000000000000000002",
            "0.0000000000000000000000000001",
        ),
        (
            "EUR",
            2,
            "9999999999999999999999999999",
            "9999999999999999999999999999",
        ),
    ] {
        for variant in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
            for phase in [DisputePhase::Opened, DisputePhase::Won, DisputePhase::Lost] {
                let mut input = base(variant, phase);
                input.disputed_amount = money(disputed, code, scale);
                input.invoice_id = Some("INV-SCALE".to_owned());
                let supplied = money(cash, code, scale);
                let entry = build_chargeback_entry(&input, &supplied).unwrap();
                let expected = if variant == DisputeVariant::CashHold {
                    supplied.clone()
                } else {
                    input.disputed_amount.clone()
                };
                assert!(entry.lines.iter().all(|line| line.money == expected));
                assert_eq!(
                    clawed_back_on_post(&input, &supplied).unwrap(),
                    if phase == DisputePhase::Lost && variant == DisputeVariant::CashHold {
                        expected
                    } else {
                        money("0", code, scale)
                    }
                );
            }
        }
    }
}

#[test]
fn partial_remains_flagged_and_zero_clawback_uses_disputed_spec() {
    for variant in [DisputeVariant::CashHold, DisputeVariant::ArReclass] {
        let mut input = base(variant, DisputePhase::Partial);
        input.disputed_amount = money("0.009", "KWD", 3);
        let cash = money("0.007", "KWD", 3);
        assert!(matches!(
            build_chargeback_entry(&input, &cash),
            Err(DomainError::InvalidDisputeTransition(_))
        ));
        assert_eq!(
            clawed_back_on_post(&input, &cash).unwrap(),
            money("0", "KWD", 3)
        );
    }
}
