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

use bss_ledger_sdk::{AccountClass, Side};
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
/// DTO (Group E) before reading any ledger state. Amounts are `i64` minor units;
/// `amount_minor` is **incl-tax** (the design's note amount), `tax_minor` is the
/// reversed tax slice of it, and `requested_deferred_minor` is how much of the
/// ex-tax revenue portion targets the unreleased deferred balance (the rest
/// reduces recognized revenue). The ex-tax revenue amount the splitter divides is
/// `amount_minor − tax_minor`.
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
    /// ISO-4217 currency of the note (all legs share it).
    pub currency: String,
    /// The note amount **incl-tax**, in minor units (`>= 0`).
    pub amount_minor: i64,
    /// The tax slice of `amount_minor` to reverse onto `TAX_PAYABLE` (`>= 0`,
    /// `<= amount_minor`). The ex-tax revenue amount is `amount_minor − tax_minor`.
    pub tax_minor: i64,
    /// The **authoritative** tax breakdown (computed by the tax engine for the
    /// *original* invoice's tax-date — the caller's concern; the gear only routes
    /// the dims, never recomputes, §4.5). Each component reverses onto its OWN
    /// `TAX_PAYABLE` leg carrying its `(jurisdiction, filing-period, rate)` dims so
    /// the projector disaggregates `tax_subbalance` per `(jurisdiction, filing)`.
    /// REQUIRED when `tax_minor > 0` — `validate_shape` rejects a bare `tax_minor`
    /// (a dimensionless `TAX_PAYABLE` leg has no (jurisdiction, filing) and the
    /// schema rejects it, `chk_journal_line_tax_dims`). `tax_minor` remains the
    /// authoritative split scalar (`amount_minor_ex_tax = amount − tax_minor`); the
    /// breakdown MUST sum to it (`validate_shape`).
    pub tax: Vec<TaxBreakdown>,
    /// How much of the ex-tax revenue amount targets the **unreleased deferred**
    /// balance (`0 <= requested_deferred_minor <= amount_minor − tax_minor`). The
    /// remainder reduces recognized revenue. MUST be 0 when `goodwill` is set.
    pub requested_deferred_minor: i64,
    /// The mandatory business reason code (AC #14) recorded on the `credit_note`
    /// row.
    pub reason_code: String,
    /// `true` ⇒ an AR-only **goodwill** credit (D3): the ex-tax debit is
    /// `GOODWILL`, no recognized-revenue reduction and no schedule reduction.
    pub goodwill: bool,
}

impl CreditNoteRequest {
    /// The ex-tax revenue amount the splitter divides into recognized vs deferred
    /// parts — `amount_minor − tax_minor`, the note amount net of the reversed
    /// tax. Saturating at 0 (a malformed `tax > amount` is rejected up-front by
    /// [`validate_shape`]; this never underflows once validated).
    #[must_use]
    pub fn amount_minor_ex_tax(&self) -> i64 {
        self.amount_minor.saturating_sub(self.tax_minor).max(0)
    }
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
    /// The leg amount in minor units (`> 0`; zero-amount legs are never emitted —
    /// inherited S1 / AC #4 rejects a zero placeholder line).
    pub amount_minor: i64,
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
    /// [`SplitResult::recognized_part_minor`] (or the whole ex-tax amount for a
    /// goodwill credit). Recorded on the `credit_note` row.
    pub recognized_part_minor: i64,
    /// The total ex-tax amount reducing the unreleased deferred balance (Σ
    /// per-stream `CONTRACT_LIABILITY` debit) — mirrors
    /// [`SplitResult::deferred_part_minor`] (`0` for goodwill). Recorded on the
    /// `credit_note` row.
    pub deferred_part_minor: i64,
    /// The amount credited to `AR` (incl. tax), capped at the invoice's current
    /// open AR.
    pub ar_credit_minor: i64,
    /// The remainder credited to `REUSABLE_CREDIT` (the paid-invoice wallet seed,
    /// K-2) — `note amount − ar_credit_minor`, `0` when open AR fully absorbs the
    /// note.
    pub wallet_remainder_minor: i64,
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
    if req.amount_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "credit-note amount_minor must be >= 0, got {}",
            req.amount_minor
        )));
    }
    if req.tax_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "credit-note tax_minor must be >= 0, got {}",
            req.tax_minor
        )));
    }
    if req.tax_minor > req.amount_minor {
        return Err(DomainError::AmountOutOfRange(format!(
            "credit-note tax_minor {} exceeds amount_minor {}",
            req.tax_minor, req.amount_minor
        )));
    }
    // Tax must carry a dimensioned breakdown: a TAX_PAYABLE journal line requires
    // (jurisdiction, filing_period) (chk_journal_line_tax_dims), so a bare
    // `tax_minor` with no breakdown can only build a dimensionless leg the schema
    // rejects at insert — reject it up front as a clean 400, not a late DB fault.
    if req.tax_minor > 0 && req.tax.is_empty() {
        return Err(DomainError::InvalidRequest(format!(
            "credit-note tax_minor {} requires a tax breakdown carrying \
             (jurisdiction, filing_period); a bare tax amount is not bookable",
            req.tax_minor
        )));
    }
    // A non-empty tax breakdown carries the per-component dims; `tax_minor` stays the
    // authoritative split scalar, so the components MUST sum to it (Σ tax-legs ==
    // tax_minor keeps the plan balanced) and each MUST share the note currency
    // (every leg posts in `req.currency`).
    if !req.tax.is_empty() {
        let breakdown_sum: i64 = req.tax.iter().map(|t| t.amount_minor).sum();
        if breakdown_sum != req.tax_minor {
            return Err(DomainError::AmountOutOfRange(format!(
                "credit-note tax breakdown sum {breakdown_sum} != tax_minor {}",
                req.tax_minor
            )));
        }
        if let Some(bad) = req.tax.iter().find(|t| t.currency != req.currency) {
            return Err(DomainError::AmountOutOfRange(format!(
                "credit-note tax breakdown currency {} != note currency {}",
                bad.currency, req.currency
            )));
        }
    }
    if req.requested_deferred_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "credit-note requested_deferred_minor must be >= 0, got {}",
            req.requested_deferred_minor
        )));
    }
    if req.requested_deferred_minor > req.amount_minor_ex_tax() {
        return Err(DomainError::AmountOutOfRange(format!(
            "credit-note requested_deferred_minor {} exceeds the ex-tax amount {}",
            req.requested_deferred_minor,
            req.amount_minor_ex_tax()
        )));
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
    if req.goodwill && req.requested_deferred_minor != 0 {
        return Err(DomainError::InvalidRequest(format!(
            "goodwill credit note must not target a deferred part (got {})",
            req.requested_deferred_minor
        )));
    }
    Ok(())
}

/// Build the balanced compensating-leg plan for a credit note (design §4.2). Pure
/// — no DB / txn. The caller supplies the [`SplitResult`] (from the splitter, the
/// recognized-vs-deferred ex-tax division across the obligation's per-stream
/// schedule state) and `open_ar_minor` (the invoice's current open AR incl. tax,
/// read by the handler under the lock order). Produces:
///
/// - DR `CONTRA_REVENUE` = `split.recognized_part_minor` (ex-tax) — OR DR
///   `GOODWILL` for the whole ex-tax amount when `req.goodwill` (C4: no revenue
///   reduction, no schedule touch);
/// - one DR `CONTRACT_LIABILITY` per stream with a deferred part (`> 0`), carrying
///   that stream's `schedule_id` so the handler reduces the right schedule;
/// - DR `TAX_PAYABLE` = `req.tax_minor` (when `> 0`);
/// - CR `AR` = `min(note amount, open_ar_minor)` (when `> 0`);
/// - CR `REUSABLE_CREDIT` = the remainder beyond open AR (when `> 0`, K-2),
///   stamped `credit_grant_event_type = CREDIT_NOTE`.
///
/// The plan is balanced by construction (Σ DR == note amount == Σ CR), asserted
/// before returning. Zero-amount legs are omitted (inherited S1 / AC #4).
///
/// # Errors
/// [`DomainError::Internal`] if the supplied split does not net to the request's
/// ex-tax amount, or if `open_ar_minor` is negative — both invariant breaches the
/// handler's reads should never produce (the assertions guard against a silent
/// unbalanced post).
pub fn build_credit_note_legs(
    req: &CreditNoteRequest,
    split: &SplitResult,
    open_ar_minor: i64,
) -> Result<CreditNoteLegPlan, DomainError> {
    if open_ar_minor < 0 {
        return Err(DomainError::Internal(format!(
            "credit-note open AR must be >= 0, got {open_ar_minor}"
        )));
    }
    let ex_tax = req.amount_minor_ex_tax();
    // The split is over the ex-tax revenue amount: recognized + deferred == ex_tax.
    // A mismatch means the handler fed the splitter a different amount than the
    // request's ex-tax — an invariant breach we refuse rather than post unbalanced.
    let split_total = split
        .recognized_part_minor
        .saturating_add(split.deferred_part_minor);
    if split_total != ex_tax {
        return Err(DomainError::Internal(format!(
            "credit-note split parts ({} recognized + {} deferred) do not net to the ex-tax \
             amount {ex_tax}",
            split.recognized_part_minor, split.deferred_part_minor
        )));
    }

    let stream = req.revenue_stream.clone();
    let mut legs: Vec<PlannedLeg> = Vec::new();

    // --- Debit side ---
    let recognized_part = split.recognized_part_minor;
    if req.goodwill {
        // C4 — AR-only goodwill: the whole ex-tax amount debits GOODWILL (never
        // CONTRA_REVENUE), no per-stream deferred reduction. `validate_shape`
        // already guaranteed `deferred == 0`, so `ex_tax == recognized_part`.
        if ex_tax > 0 {
            legs.push(PlannedLeg {
                account_class: AccountClass::Goodwill,
                side: Side::Debit,
                amount_minor: ex_tax,
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
        if recognized_part > 0 {
            legs.push(PlannedLeg {
                account_class: AccountClass::ContraRevenue,
                side: Side::Debit,
                amount_minor: recognized_part,
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
            if ps.deferred_part_minor > 0 {
                legs.push(PlannedLeg {
                    account_class: AccountClass::ContractLiability,
                    side: Side::Debit,
                    amount_minor: ps.deferred_part_minor,
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
    // note MUST carry a breakdown (`validate_shape` rejects a bare `tax_minor`): a
    // dimensionless TAX_PAYABLE line carries no (jurisdiction, filing) and the
    // schema rejects it (chk_journal_line_tax_dims). Σ of the per-component legs ==
    // tax_minor (validated), so the plan still balances.
    if !req.tax.is_empty() {
        for t in &req.tax {
            if t.amount_minor > 0 {
                legs.push(PlannedLeg {
                    account_class: AccountClass::TaxPayable,
                    side: Side::Debit,
                    amount_minor: t.amount_minor,
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
    let ar_credit = req.amount_minor.min(open_ar_minor).max(0);
    let wallet_remainder = req.amount_minor.saturating_sub(ar_credit).max(0);
    // Goodwill is AR-only relief (design D3): it may only reduce the open receivable,
    // never mint spendable wallet credit. An amount beyond open AR — or ANY amount on a
    // fully-paid invoice (open_ar == 0) — has no receivable to relieve, so reject rather
    // than convert a goodwill gesture into a cash-equivalent REUSABLE_CREDIT grant.
    if req.goodwill && wallet_remainder > 0 {
        return Err(DomainError::InvalidRequest(format!(
            "goodwill credit note {} ({} minor) exceeds the invoice's open AR ({open_ar_minor} minor): \
             goodwill is AR-only and cannot mint reusable credit",
            req.credit_note_id, req.amount_minor
        )));
    }
    if ar_credit > 0 {
        legs.push(PlannedLeg {
            account_class: AccountClass::Ar,
            side: Side::Credit,
            amount_minor: ar_credit,
            revenue_stream: None,
            schedule_id: None,
            credit_grant_event_type: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }
    if wallet_remainder > 0 {
        legs.push(PlannedLeg {
            account_class: AccountClass::ReusableCredit,
            side: Side::Credit,
            amount_minor: wallet_remainder,
            revenue_stream: None,
            schedule_id: None,
            credit_grant_event_type: Some(CREDIT_GRANT_EVENT_TYPE_CREDIT_NOTE.to_owned()),
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }

    // Balance invariant (Σ DR == Σ CR). The debit side is recognized_ex_tax +
    // deferred_ex_tax + tax == ex_tax + tax == amount_minor; the credit side is
    // ar_credit + wallet_remainder == amount_minor. A zero-amount note emits no
    // legs (both sides 0) — a benign no-op the handler still records, but the post
    // engine rejects an empty entry, so the handler guards zero up-front.
    let dr: i64 = legs
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| l.amount_minor)
        .sum();
    let cr: i64 = legs
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| l.amount_minor)
        .sum();
    debug_assert_eq!(dr, cr, "credit-note leg plan must balance");
    if dr != cr {
        return Err(DomainError::Internal(format!(
            "credit-note leg plan does not balance (DR {dr} != CR {cr})"
        )));
    }

    Ok(CreditNoteLegPlan {
        legs,
        recognized_part_minor: recognized_part,
        deferred_part_minor: split.deferred_part_minor,
        ar_credit_minor: ar_credit,
        wallet_remainder_minor: wallet_remainder,
        split_basis_ref: split.split_basis_ref.clone(),
    })
}

#[cfg(test)]
#[path = "credit_note_tests.rs"]
mod credit_note_tests;
