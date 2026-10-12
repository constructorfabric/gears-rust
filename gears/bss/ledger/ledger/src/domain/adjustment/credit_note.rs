//! Credit-note domain (Slice 3, Phase 1 / Group C1) — the **pure** request shape
//! and the deterministic compensating-leg plan a credit note posts (design §4.2).
//! Backend-agnostic: no DB / txn / async I/O. The infra
//! [`CreditNoteHandler`](crate::infra::adjustment::credit_note_service) reads the
//! schedule state + the invoice's current open AR under the §4.7 lock order,
//! drives the [`RecognizedDeferredSplitter`](super::splitter) for the ex-tax
//! split, then calls [`build_credit_note_legs`] to derive the balanced leg plan
//! (which the handler maps onto posting lines + the per-stream schedule
//! reductions + the headroom/wallet writes).
//!
//! **The leg plan (design §4.2 legs table).** A credit note reduces recognized
//! revenue, the unreleased deferred balance, and the reversed tax against the
//! invoice's open AR (and, for a paid invoice, a reusable-credit remainder):
//!
//! | Line | Side | Account class |
//! |------|------|---------------|
//! | Reduce recognized revenue (ex-tax) | DR | `CONTRA_REVENUE` (or `GOODWILL`, AR-only) |
//! | Reduce unreleased deferred (ex-tax, per stream) | DR | `CONTRACT_LIABILITY` |
//! | Reverse tax | DR | `TAX_PAYABLE` |
//! | Reduce AR (incl. tax) — up to current open AR | CR | `AR` |
//! | Remainder beyond open AR (paid invoice, Rev2 / K-2) | CR | `REUSABLE_CREDIT` |
//!
//! The plan is **balanced by construction** (Σ DR == Σ CR): the debit side is
//! `recognized_ex_tax + deferred_ex_tax + tax` (= the note's incl-tax amount), and
//! the credit side splits that SAME total into `AR` (capped at open AR) +
//! `REUSABLE_CREDIT` (the remainder). [`build_credit_note_legs`] asserts the
//! balance as a domain invariant before returning.
//!
//! **Goodwill / AR-only (D3, §4.2).** A `goodwill` credit reduces no recognized
//! revenue and touches no schedule: its single debit is `GOODWILL` (NOT
//! `CONTRA_REVENUE`) for the whole ex-tax amount. The split must carry no deferred
//! part (a goodwill credit has no obligation to reduce); the handler passes a
//! zero-deferred split for it. The authoritative AR floor for goodwill is the
//! Slice 1 `ar_invoice_balance` NO-negative CHECK (NOT the `invoice_exposure`
//! headroom), so a goodwill credit that would over-reduce AR is rejected by that
//! CHECK in the post — not here.

use crate::domain::exact_money::{
    ExactAmount, map_exact_error, matching_spec, subtract_posted, sum_posted,
};
use bss_ledger_sdk::money::PostedMoney;
use bss_ledger_sdk::{AccountClass, Side};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::splitter::SplitResult;
use crate::domain::error::DomainError;
use crate::domain::invoice::builder::TaxBreakdown;

/// The `credit_grant_event_type` a paid-invoice credit-note remainder accrues to
/// in the reusable-credit wallet (design §4.2 / B-5). Stamped on the
/// `CR REUSABLE_CREDIT` leg so the projector seeds the wallet sub-grain under this
/// bucket; mirrors the `credit_grant_event_type` literal Slice 2 uses for grants.
pub const CREDIT_GRANT_EVENT_TYPE_CREDIT_NOTE: &str = "CREDIT_NOTE";

/// One credit-note request — the pure inputs the handler resolves from the REST
/// DTO (Group E) before reading any ledger state. Amounts are major-unit `PostedMoney` values;
/// `amount` is **incl-tax** (the design's note amount), `tax_amount` is the
/// reversed tax slice of it, and `requested_deferred` is how much of the
/// ex-tax revenue portion targets the unreleased deferred balance (the rest
/// reduces recognized revenue). The ex-tax revenue amount the splitter divides is
/// `amount − tax_amount`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditNoteRequest {
    /// The seller tenant whose ledger this posts into.
    pub tenant_id: Uuid,
    /// The tenant the original invoice billed (the AR / wallet owner).
    pub payer_tenant_id: Uuid,
    /// The business id of this credit note — the `(tenant, CREDIT_NOTE,
    /// credit_note_id)` idempotency key + the `credit_note` row PK.
    pub credit_note_id: String,
    /// The originating posted invoice (`NOTE_INVOICE_NOT_FOUND` if absent,
    /// enforced by the handler). The credit note never mutates its rows.
    pub origin_invoice_id: String,
    /// The targeted posted invoice-item ref (the line being credited) — anchors
    /// the recognized/deferred split + the `split_basis_ref`. `None` for an
    /// invoice-level (no specific item) credit.
    pub origin_invoice_item_ref: Option<String>,
    /// The PO / allocation group the targeted line books under (the split-basis
    /// dimension, §4.2). `None` for a line with no allocation group.
    pub po_allocation_group: Option<String>,
    /// The revenue stream the credit books against (the `CONTRA_REVENUE` /
    /// `CONTRACT_LIABILITY` legs carry it; per-stream classes need it).
    pub revenue_stream: String,
    /// The note amount **incl-tax**, in major units (`>= 0`).
    pub amount: PostedMoney,
    /// The tax slice of `amount` to reverse onto `TAX_PAYABLE` (`>= 0`,
    /// `<= amount`). The ex-tax revenue amount is `amount − tax_amount`.
    pub tax_amount: PostedMoney,
    /// The **authoritative** tax breakdown (computed by the tax engine for the
    /// *original* invoice's tax-date — the caller's concern; the gear only routes
    /// the dims, never recomputes, §4.5). Each component reverses onto its OWN
    /// `TAX_PAYABLE` leg carrying its `(jurisdiction, filing-period, rate)` dims so
    /// the projector disaggregates `tax_subbalance` per `(jurisdiction, filing)`.
    /// REQUIRED when `tax_amount.amount() > Decimal::ZERO` — `validate_shape` rejects a bare `tax_amount`
    /// (a dimensionless `TAX_PAYABLE` leg has no (jurisdiction, filing) and the
    /// schema rejects it, `chk_journal_line_tax_dims`). `tax_amount` remains the
    /// authoritative split scalar (`amount_ex_tax = amount − tax_amount`); the
    /// breakdown MUST sum to it (`validate_shape`).
    pub tax: Vec<TaxBreakdown>,
    /// How much of the ex-tax revenue amount targets the **unreleased deferred**
    /// balance (`0 <= requested_deferred <= amount − tax_amount`). The
    /// remainder reduces recognized revenue. MUST be 0 when `goodwill` is set.
    pub requested_deferred: PostedMoney,
    /// The mandatory business reason code (AC #14) recorded on the `credit_note`
    /// row.
    pub reason_code: String,
    /// `true` ⇒ an AR-only **goodwill** credit (D3): the ex-tax debit is
    /// `GOODWILL`, no recognized-revenue reduction and no schedule reduction.
    pub goodwill: bool,
}

impl CreditNoteRequest {
    /// Exact ex-tax amount. Reject malformed signs, metadata or tax above gross.
    ///
    /// # Errors
    /// [`DomainError::CurrencyMismatch`] / [`DomainError::InconsistentScale`] when `amount` and
    /// `tax_amount` carry different currency metadata; [`DomainError::AmountOutOfRange`] when
    /// either is negative or the tax exceeds the gross; the exact-arithmetic [`DomainError`]
    /// when the subtraction leaves the money contract.
    pub fn amount_ex_tax(&self) -> Result<PostedMoney, DomainError> {
        matching_spec(&self.amount, &self.tax_amount)?;
        if self.amount.amount() < Decimal::ZERO {
            return Err(DomainError::AmountOutOfRange(format!(
                "credit-note amount must be >= 0, got {}",
                self.amount
            )));
        }
        if self.tax_amount.amount() < Decimal::ZERO {
            return Err(DomainError::AmountOutOfRange(format!(
                "credit-note tax_amount must be >= 0, got {}",
                self.tax_amount
            )));
        }
        if self.tax_amount.amount() > self.amount.amount() {
            return Err(DomainError::AmountOutOfRange(format!(
                "credit-note tax_amount {} exceeds amount {}",
                self.tax_amount, self.amount
            )));
        }
        subtract_posted(&self.amount, &self.tax_amount)
    }
}

/// An invoice's remaining credit-note headroom: `original + Σ debit notes −
/// Σ prior credit notes`, exact, in the exposure row's currency and scale. The
/// one rule shared by the exposure read and the credit-note cap.
///
/// # Errors
/// [`DomainError::CurrencyMismatch`] / [`DomainError::InconsistentScale`] when the three
/// totals disagree on currency metadata; [`DomainError::Internal`] when the headroom is
/// negative (the `invoice_exposure` CHECK guarantees `credit <= original + debit`, so a
/// negative value is a stored-invariant failure, never a figure to report or floor);
/// the exact-arithmetic [`DomainError`] when the result leaves the money contract.
pub fn remaining_headroom(
    original_total: &PostedMoney,
    debit_note_total: &PostedMoney,
    credit_note_total: &PostedMoney,
) -> Result<PostedMoney, DomainError> {
    matching_spec(original_total, debit_note_total)?;
    matching_spec(original_total, credit_note_total)?;
    let remaining = ExactAmount::from_decimal(original_total.amount())
        .checked_add(&ExactAmount::from_decimal(debit_note_total.amount()))
        .and_then(|v| v.checked_sub(&ExactAmount::from_decimal(credit_note_total.amount())))
        .map_err(map_exact_error)?;
    if remaining.is_negative() {
        return Err(DomainError::Internal(format!(
            "invoice exposure headroom is negative: original {original_total} + debit notes \
             {debit_note_total} - credit notes {credit_note_total}"
        )));
    }
    remaining
        .into_posted_exact(original_total.currency().clone())
        .map_err(map_exact_error)
}

/// One planned compensating leg of a credit-note entry — a pure description the
/// handler maps onto a posting line (binding the chart `account_id` + scale).
/// `revenue_stream` is `Some` for the per-stream classes (`CONTRA_REVENUE`,
/// `CONTRACT_LIABILITY`) and `None` for the stream-less classes (`AR`,
/// `TAX_PAYABLE`, `GOODWILL`, `REUSABLE_CREDIT`); `credit_grant_event_type` is
/// `Some` only on the `CR REUSABLE_CREDIT` wallet remainder leg.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedLeg {
    /// The account class this leg posts to.
    pub account_class: AccountClass,
    /// DR / CR.
    pub side: Side,
    /// The leg amount in major units (`> 0`; zero-amount legs are never emitted —
    /// inherited S1 / AC #4 rejects a zero placeholder line).
    pub amount: PostedMoney,
    /// The revenue stream (per-stream classes only); `None` for stream-less.
    pub revenue_stream: Option<String>,
    /// The owning `recognition_schedule` id this leg's deferred reduction targets
    /// — `Some` only on a per-stream `DR CONTRACT_LIABILITY` leg, so the handler
    /// reduces the right schedule. `None` on every other leg.
    pub schedule_id: Option<String>,
    /// The wallet bucket — `Some(CREDIT_NOTE)` only on the `CR REUSABLE_CREDIT`
    /// remainder leg (the projector seeds the wallet sub-grain under it); `None`
    /// elsewhere.
    pub credit_grant_event_type: Option<String>,
    /// Tax dims (per-(jurisdiction, filing-period, rate) disaggregation, design §4.5).
    /// `Some` only on a `TAX_PAYABLE` leg built from a `TaxBreakdown`; `None` on every
    /// other leg and on the legacy single dimensionless tax leg.
    pub tax_jurisdiction: Option<String>,
    pub tax_filing_period: Option<String>,
    pub tax_rate_ref: Option<String>,
}

/// The full balanced leg plan for one credit note: the legs to post plus the
/// `split_basis_ref` to stamp on the `credit_note` row and the wallet remainder
/// amount (the `CR REUSABLE_CREDIT` slice, `0` for a fully-open-AR invoice). Pure
/// data — the handler posts the legs, persists the row, and seeds the headroom /
/// schedule / wallet writes from these fields.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditNoteLegPlan {
    /// The balanced legs (Σ DR == Σ CR), in a deterministic order: the debit legs
    /// (`CONTRA_REVENUE`/`GOODWILL`, then per-stream `CONTRACT_LIABILITY`, then
    /// `TAX_PAYABLE`) followed by the credit legs (`AR`, then `REUSABLE_CREDIT`).
    pub legs: Vec<PlannedLeg>,
    /// The total ex-tax amount reducing recognized revenue (the
    /// `CONTRA_REVENUE`/`GOODWILL` debit) — mirrors
    /// [`SplitResult::recognized_part`] (or the whole ex-tax amount for a
    /// goodwill credit). Recorded on the `credit_note` row.
    pub recognized_part: PostedMoney,
    /// The total ex-tax amount reducing the unreleased deferred balance (Σ
    /// per-stream `CONTRACT_LIABILITY` debit) — mirrors
    /// [`SplitResult::deferred_part`] (`0` for goodwill). Recorded on the
    /// `credit_note` row.
    pub deferred_part: PostedMoney,
    /// The amount credited to `AR` (incl. tax), capped at the invoice's current
    /// open AR.
    pub ar_credit: PostedMoney,
    /// The remainder credited to `REUSABLE_CREDIT` (the paid-invoice wallet seed,
    /// K-2) — `note amount − ar_credit`, `0` when open AR fully absorbs the
    /// note.
    pub wallet_remainder: PostedMoney,
    /// The deterministic split-basis description to stamp on the `credit_note` row
    /// (from the splitter; a synthetic one for a goodwill credit that runs no
    /// split).
    pub split_basis_ref: String,
}

/// Validate a credit-note request's amounts + the goodwill shape (design §4.2).
/// Pure shape checks the splitter does not own (it sees only the ex-tax revenue
/// amount): a negative amount/tax, a tax over the note amount, a deferred request
/// over the ex-tax revenue amount, an empty reason code, or a goodwill credit that
/// carries a deferred part (a goodwill credit reduces no obligation, C4).
///
/// # Errors
/// [`DomainError::AmountOutOfRange`] for a malformed amount/tax/deferred;
/// [`DomainError::InvalidRequest`] for an empty reason code or a goodwill credit
/// with a non-zero deferred part.
pub fn validate_shape(req: &CreditNoteRequest) -> Result<(), DomainError> {
    let ex_tax = req.amount_ex_tax()?;
    matching_spec(&req.amount, &req.requested_deferred)?;
    if req.requested_deferred.amount() < Decimal::ZERO
        || req.requested_deferred.amount() > ex_tax.amount()
    {
        return Err(DomainError::AmountOutOfRange(
            "note deferred amount exceeds ex-tax amount".to_owned(),
        ));
    }
    if req.tax_amount.amount() > Decimal::ZERO && req.tax.is_empty() {
        return Err(DomainError::InvalidRequest(
            "note tax requires a dimensioned breakdown".to_owned(),
        ));
    }
    for component in &req.tax {
        matching_spec(&req.amount, &component.amount)?;
        if component.amount.amount() < Decimal::ZERO {
            return Err(DomainError::AmountOutOfRange(
                "negative tax component".to_owned(),
            ));
        }
    }
    let tax_sum = sum_posted(
        &req.tax.iter().map(|t| t.amount.clone()).collect::<Vec<_>>(),
        req.amount.currency().clone(),
    )
    .map_err(map_exact_error)?;
    if tax_sum != req.tax_amount {
        return Err(DomainError::AmountOutOfRange(
            "tax breakdown sum differs from tax amount".to_owned(),
        ));
    }
    if req.reason_code.trim().is_empty() {
        return Err(DomainError::InvalidRequest(
            "credit note requires a non-empty reason_code (AC #14)".to_owned(),
        ));
    }
    // C4: a goodwill (AR-only) credit reduces no recognized revenue and touches no
    // schedule, so it must carry no deferred part. (The handler also passes an
    // empty schedule-state set for a goodwill credit, so the splitter would block
    // a deferred request anyway — this is the clean up-front 400.)
    if req.goodwill && req.requested_deferred.amount() != Decimal::ZERO {
        return Err(DomainError::InvalidRequest(format!(
            "goodwill credit note must not target a deferred part (got {})",
            req.requested_deferred.amount()
        )));
    }
    Ok(())
}

/// Build the balanced compensating-leg plan for a credit note (design §4.2). Pure
/// — no DB / txn. The caller supplies the [`SplitResult`] (from the splitter, the
/// recognized-vs-deferred ex-tax division across the obligation's per-stream
/// schedule state) and `open_ar` (the invoice's current open AR incl. tax,
/// read by the handler under the lock order). Produces:
///
/// - DR `CONTRA_REVENUE` = `split.recognized_part` (ex-tax) — OR DR
///   `GOODWILL` for the whole ex-tax amount when `req.goodwill` (C4: no revenue
///   reduction, no schedule touch);
/// - one DR `CONTRACT_LIABILITY` per stream with a deferred part (`> 0`), carrying
///   that stream's `schedule_id` so the handler reduces the right schedule;
/// - DR `TAX_PAYABLE` = `req.tax` (when `> 0`);
/// - CR `AR` = `min(note amount, open_ar)` (when `> 0`);
/// - CR `REUSABLE_CREDIT` = the remainder beyond open AR (when `> 0`, K-2),
///   stamped `credit_grant_event_type = CREDIT_NOTE`.
///
/// The plan is balanced by construction (Σ DR == note amount == Σ CR), asserted
/// before returning. Zero-amount legs are omitted (inherited S1 / AC #4).
///
/// # Errors
/// [`DomainError::Internal`] if the supplied split does not net to the request's
/// ex-tax amount, or if `open_ar` is negative — both invariant breaches the
/// handler's reads should never produce (the assertions guard against a silent
/// unbalanced post).
pub fn build_credit_note_legs(
    req: &CreditNoteRequest,
    split: &SplitResult,
    open_ar: PostedMoney,
) -> Result<CreditNoteLegPlan, DomainError> {
    validate_shape(req)?;
    matching_spec(&req.amount, &open_ar)?;
    if open_ar.amount() < Decimal::ZERO {
        return Err(DomainError::Internal(format!(
            "credit-note open AR must be >= 0, got {open_ar:?}"
        )));
    }
    let ex_tax = req.amount_ex_tax()?;
    // The split is over the ex-tax revenue amount: recognized + deferred == ex_tax.
    // A mismatch means the handler fed the splitter a different amount than the
    // request's ex-tax — an invariant breach we refuse rather than post unbalanced.
    for value in [&split.recognized_part, &split.deferred_part] {
        matching_spec(&req.amount, value)?;
        if value.amount() < Decimal::ZERO {
            return Err(DomainError::Internal("negative split part".to_owned()));
        }
    }
    for part in &split.per_stream {
        for value in [&part.recognized_part, &part.deferred_part] {
            matching_spec(&req.amount, value)?;
            if value.amount() < Decimal::ZERO {
                return Err(DomainError::Internal(
                    "negative stream split part".to_owned(),
                ));
            }
        }
    }
    let split_total = sum_posted(
        &[split.recognized_part.clone(), split.deferred_part.clone()],
        req.amount.currency().clone(),
    )
    .map_err(map_exact_error)?;
    let deferred_matches_request = split.deferred_part == req.requested_deferred;
    if split_total != ex_tax || !deferred_matches_request {
        return Err(DomainError::Internal(
            "split does not match requested ex-tax/deferred amounts".to_owned(),
        ));
    }
    if !split.per_stream.is_empty() {
        let stream_recognized = sum_posted(
            &split
                .per_stream
                .iter()
                .map(|p| p.recognized_part.clone())
                .collect::<Vec<_>>(),
            req.amount.currency().clone(),
        )
        .map_err(map_exact_error)?;
        if stream_recognized != split.recognized_part {
            return Err(DomainError::Internal(
                "stream recognized split differs from total".to_owned(),
            ));
        }
    }
    let stream_deferred = sum_posted(
        &split
            .per_stream
            .iter()
            .map(|p| p.deferred_part.clone())
            .collect::<Vec<_>>(),
        req.amount.currency().clone(),
    )
    .map_err(map_exact_error)?;
    if stream_deferred != split.deferred_part {
        return Err(DomainError::Internal(
            "stream deferred split differs from total".to_owned(),
        ));
    }

    let stream = req.revenue_stream.clone();
    let mut legs: Vec<PlannedLeg> = Vec::new();

    // --- Debit side ---
    let recognized_part = split.recognized_part.clone();
    if req.goodwill {
        // C4 — AR-only goodwill: the whole ex-tax amount debits GOODWILL (never
        // CONTRA_REVENUE), no per-stream deferred reduction. `validate_shape`
        // already guaranteed `deferred == 0`, so `ex_tax == recognized_part`.
        if ex_tax.amount() > Decimal::ZERO {
            legs.push(PlannedLeg {
                account_class: AccountClass::Goodwill,
                side: Side::Debit,
                amount: ex_tax.clone(),
                revenue_stream: None,
                schedule_id: None,
                credit_grant_event_type: None,
                tax_jurisdiction: None,
                tax_filing_period: None,
                tax_rate_ref: None,
            });
        }
    } else {
        // Reduce recognized revenue via CONTRA_REVENUE (debit-normal; NOT REVENUE
        // directly, design §4.2). Per-stream class ⇒ carries the stream.
        if recognized_part.amount() > Decimal::ZERO {
            legs.push(PlannedLeg {
                account_class: AccountClass::ContraRevenue,
                side: Side::Debit,
                amount: recognized_part.clone(),
                revenue_stream: Some(stream.clone()),
                schedule_id: None,
                credit_grant_event_type: None,
                tax_jurisdiction: None,
                tax_filing_period: None,
                tax_rate_ref: None,
            });
        }
        // Reduce the unreleased deferred balance per stream — one DR
        // CONTRACT_LIABILITY per stream that took a deferred part, carrying that
        // stream's schedule_id so the handler reduces the right schedule (§4.5).
        for ps in &split.per_stream {
            if ps.deferred_part.amount() > Decimal::ZERO {
                legs.push(PlannedLeg {
                    account_class: AccountClass::ContractLiability,
                    side: Side::Debit,
                    amount: ps.deferred_part.clone(),
                    revenue_stream: Some(ps.revenue_stream.clone()),
                    schedule_id: Some(ps.schedule_id.clone()),
                    credit_grant_event_type: None,
                    tax_jurisdiction: None,
                    tax_filing_period: None,
                    tax_rate_ref: None,
                });
            }
        }
    }

    // Reverse tax onto TAX_PAYABLE (stream-less). Carries posted tax evidence
    // upstream (never recomputed here, §4.5). Emit ONE DR TAX_PAYABLE per breakdown
    // component carrying its (jurisdiction, filing-period, rate) dims so the
    // projector disaggregates `tax_subbalance` per (jurisdiction, filing). A taxed
    // note MUST carry a breakdown (`validate_shape` rejects a bare `tax_amount`): a
    // dimensionless TAX_PAYABLE line carries no (jurisdiction, filing) and the
    // schema rejects it (chk_journal_line_tax_dims). Σ of the per-component legs ==
    // tax (validated), so the plan still balances.
    if !req.tax.is_empty() {
        for t in &req.tax {
            if t.amount.amount() > Decimal::ZERO {
                legs.push(PlannedLeg {
                    account_class: AccountClass::TaxPayable,
                    side: Side::Debit,
                    amount: t.amount.clone(),
                    revenue_stream: None,
                    schedule_id: None,
                    credit_grant_event_type: None,
                    tax_jurisdiction: Some(t.tax_jurisdiction.clone()),
                    tax_filing_period: Some(t.tax_filing_period.clone()),
                    tax_rate_ref: t.tax_rate_ref.clone(),
                });
            }
        }
    }

    // --- Credit side: open-AR cap then wallet remainder (K-2) ---
    let ar_credit = if req.amount.amount() <= open_ar.amount() {
        req.amount.clone()
    } else {
        open_ar.clone()
    };
    let wallet_remainder = subtract_posted(&req.amount, &ar_credit)?;
    // Goodwill is AR-only relief (design D3): it may only reduce the open receivable,
    // never mint spendable wallet credit. An amount beyond open AR — or ANY amount on a
    // fully-paid invoice (open_ar == 0) — has no receivable to relieve, so reject rather
    // than convert a goodwill gesture into a cash-equivalent REUSABLE_CREDIT grant.
    if req.goodwill && wallet_remainder.amount() > Decimal::ZERO {
        return Err(DomainError::InvalidRequest(format!(
            "goodwill credit note {} ({} major units) exceeds the invoice's open AR ({open_ar:?}): \
             goodwill is AR-only and cannot mint reusable credit",
            req.credit_note_id,
            req.amount.amount()
        )));
    }
    if ar_credit.amount() > Decimal::ZERO {
        legs.push(PlannedLeg {
            account_class: AccountClass::Ar,
            side: Side::Credit,
            amount: ar_credit.clone(),
            revenue_stream: None,
            schedule_id: None,
            credit_grant_event_type: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }
    if wallet_remainder.amount() > Decimal::ZERO {
        legs.push(PlannedLeg {
            account_class: AccountClass::ReusableCredit,
            side: Side::Credit,
            amount: wallet_remainder.clone(),
            revenue_stream: None,
            schedule_id: None,
            credit_grant_event_type: Some(CREDIT_GRANT_EVENT_TYPE_CREDIT_NOTE.to_owned()),
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }

    // Balance invariant (Σ DR == Σ CR). The debit side is recognized_ex_tax +
    // deferred_ex_tax + tax == ex_tax + tax == amount; the credit side is
    // ar_credit + wallet_remainder == amount. A zero-amount note emits no
    // legs (both sides 0) — a benign no-op the handler still records, but the post
    // engine rejects an empty entry, so the handler guards zero up-front.
    let side_total = |side| {
        sum_posted(
            &legs
                .iter()
                .filter(|l| l.side == side)
                .map(|l| l.amount.clone())
                .collect::<Vec<_>>(),
            req.amount.currency().clone(),
        )
        .map_err(map_exact_error)
    };
    let dr = side_total(Side::Debit)?;
    let cr = side_total(Side::Credit)?;
    debug_assert_eq!(dr, cr, "credit-note leg plan must balance");
    if dr != cr {
        return Err(DomainError::Internal(format!(
            "credit-note leg plan does not balance (DR {dr} != CR {cr})"
        )));
    }

    Ok(CreditNoteLegPlan {
        legs,
        recognized_part,
        deferred_part: split.deferred_part.clone(),
        ar_credit,
        wallet_remainder,
        split_basis_ref: split.split_basis_ref.clone(),
    })
}

#[cfg(test)]
#[path = "credit_note_tests.rs"]
mod credit_note_tests;

#[cfg(test)]
#[path = "credit_note_guard_tests.rs"]
mod credit_note_guard_tests;
