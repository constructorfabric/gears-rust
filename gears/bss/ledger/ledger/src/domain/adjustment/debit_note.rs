//! Debit-note domain (Slice 3, Phase 1 / Group D1) — the **pure** request shape
//! and the deterministic **direct-split** leg plan a debit note posts (design
//! §4.3). A debit note is an *additional charge* against an already-posted
//! invoice; unlike the credit note (a compensating reduction driven by the
//! [`RecognizedDeferredSplitter`](super::splitter)), it **mirrors the Slice-1
//! invoice-post direct split** — it books fresh AR / Revenue / Contract-liability
//! / Tax exactly as a new invoice line would, and (when it defers) triggers the
//! Slice 4 `ScheduleBuilder` in the same atomic unit (D4). Backend-agnostic: no
//! DB / txn / async I/O. The infra
//! [`DebitNoteHandler`](crate::infra::adjustment::debit_note_service) derives the
//! deferred split + the schedule plan, calls [`build_debit_note_legs`], then posts
//! the legs atomically with the schedule-build + headroom writes.
//!
//! **The leg plan (design §4.3 legs table — a mirror of S1 invoice-post).**
//!
//! | Line | Side | Account class |
//! |------|------|---------------|
//! | Additional AR (incl. tax) | DR | `AR` |
//! | Revenue recognized at post (ex-tax) | CR | `REVENUE` |
//! | Contract liability deferred per PO (ex-tax, if any) | CR | `CONTRACT_LIABILITY` |
//! | Tax | CR | `TAX_PAYABLE` |
//!
//! The plan is **balanced by construction** (`DR AR == CR REVENUE + CR
//! CONTRACT_LIABILITY + CR TAX_PAYABLE`): the single AR debit is the incl-tax
//! amount, and the credit side splits it into recognized revenue
//! (`ex_tax − deferred`), the deferred Contract-liability (`deferred`, the part the
//! schedule will release), and the reversed-evidence tax (`tax_amount`). The ex-tax total
//! is `amount − tax_amount`; `deferred` is how much of THAT ex-tax goes
//! to `CONTRACT_LIABILITY` (the rest recognizes now). [`build_debit_note_legs`]
//! asserts the balance as a domain invariant before returning.
//!
//! **No zero-placeholder lines (inherited S1 / AC #4).** A fully-recognized debit
//! note (`deferred == 0`) emits NO `CONTRACT_LIABILITY` line (byte-identical
//! to the S1 direct split for a non-deferred item); a zero recognized-now part
//! (`deferred == ex_tax`) emits NO `REVENUE` line; a zero tax emits no
//! `TAX_PAYABLE` line.

use crate::domain::exact_money::{map_exact_error, matching_spec, subtract_posted, sum_posted};
use bss_ledger_sdk::money::PostedMoney;
use bss_ledger_sdk::{AccountClass, Side};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::invoice::builder::TaxBreakdown;
use crate::domain::recognition::input::RecognitionInput;

/// One debit-note request — the pure inputs the handler resolves from the REST
/// DTO (Group E) before posting. Amounts are major-unit `PostedMoney` values; `amount` is
/// **incl-tax** (the design's note amount), `tax_amount` is the tax slice of it
/// (carried posted tax evidence, never recomputed, §4.3), and `deferred` is
/// how much of the ex-tax revenue portion (`amount − tax_amount`) is deferred
/// to `CONTRACT_LIABILITY` per the line's PO (the rest recognizes now). When
/// `deferred.amount() > Decimal::ZERO` the [`Self::recognition`] spec drives the schedule build
/// (D4) — the SAME `ScheduleBuilder` path Slice 1's invoice-post uses.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// The `*_ref` / `*_id` fields mirror the storage / SDK column names verbatim;
// renaming to satisfy `struct_field_names` would diverge from `NewDebitNote` /
// the journal-line contract.
#[allow(clippy::struct_field_names)]
pub struct DebitNoteRequest {
    /// The seller tenant whose ledger this posts into.
    pub tenant_id: Uuid,
    /// The tenant the original invoice billed (the AR owner the charge lands on).
    pub payer_tenant_id: Uuid,
    /// The business id of this debit note — the `(tenant, DEBIT_NOTE,
    /// debit_note_id)` idempotency key + the `debit_note` row PK.
    pub debit_note_id: String,
    /// The originating posted invoice (`NOTE_INVOICE_NOT_FOUND` if absent,
    /// enforced by the handler in Group E). The debit note never mutates its rows;
    /// it raises that invoice's headroom (`debit_note_total += amount`).
    pub origin_invoice_id: String,
    /// The targeted posted invoice-item ref — anchors the freshly-built
    /// `recognition_schedule`'s NOT-NULL `source_invoice_item_ref` when the note
    /// defers (§4.7). Required (non-empty) by the handler for a deferred note; a
    /// fully-recognized note may carry it for lineage but does not require it.
    pub origin_invoice_item_ref: Option<String>,
    /// The revenue stream the charge books against (the `REVENUE` /
    /// `CONTRACT_LIABILITY` legs carry it; per-stream classes need it).
    pub revenue_stream: String,
    /// The note amount **incl-tax**, in major units (`>= 0`) — the single DR AR.
    pub amount: PostedMoney,
    /// The tax slice of `amount` posted onto `TAX_PAYABLE` (`>= 0`,
    /// `<= amount`). Posted tax evidence — never recomputed here (§4.3). The
    /// ex-tax revenue amount is `amount − tax_amount`.
    pub tax_amount: PostedMoney,
    /// The **authoritative** tax breakdown (computed by the tax engine for the
    /// *original* invoice's tax-date — the caller's concern; the gear only routes
    /// the dims, never recomputes, §4.5). Each component posts onto its OWN
    /// `TAX_PAYABLE` leg carrying its `(jurisdiction, filing-period, rate)` dims so
    /// the projector disaggregates `tax_subbalance` per `(jurisdiction, filing)`.
    /// REQUIRED when `tax_amount.amount() > Decimal::ZERO` — `validate_shape` rejects a bare `tax_amount`
    /// (a dimensionless `TAX_PAYABLE` leg has no (jurisdiction, filing) and the
    /// schema rejects it, `chk_journal_line_tax_dims`). `tax_amount` remains the
    /// authoritative split scalar (`amount_ex_tax = amount − tax_amount`); the
    /// breakdown MUST sum to it (`validate_shape`).
    pub tax: Vec<TaxBreakdown>,
    /// How much of the ex-tax revenue amount is **deferred** to
    /// `CONTRACT_LIABILITY` per the line's PO (`0 <= deferred <= amount
    /// − tax`). The remainder (`ex_tax − deferred`) recognizes now to
    /// `REVENUE`. `0` ⇒ fully recognized, NO `CONTRACT_LIABILITY` line + no schedule
    /// build (byte-identical to the S1 direct split for a non-deferred line).
    pub deferred: PostedMoney,
    /// The mandatory business reason / context code (AC #14 — "MUST link business
    /// context", §4.3) recorded for audit. Non-empty.
    pub reason_code: String,
    /// The optional per-item ASC 606 recognition spec (Slice 4) — the SAME shape
    /// Slice 1's invoice-post item carries. REQUIRED to be `Some` when
    /// `deferred.amount() > Decimal::ZERO` (the handler runs it through the recognition
    /// [`ScheduleBuilder`](crate::domain::recognition::builder::ScheduleBuilder) to
    /// build the schedule that releases the deferred Contract-liability, D4). `None`
    /// for a fully-recognized note.
    pub recognition: Option<RecognitionInput>,
}

impl DebitNoteRequest {
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
                "debit-note amount must be >= 0, got {}",
                self.amount
            )));
        }
        if self.tax_amount.amount() < Decimal::ZERO {
            return Err(DomainError::AmountOutOfRange(format!(
                "debit-note tax_amount must be >= 0, got {}",
                self.tax_amount
            )));
        }
        if self.tax_amount.amount() > self.amount.amount() {
            return Err(DomainError::AmountOutOfRange(format!(
                "debit-note tax_amount {} exceeds amount {}",
                self.tax_amount, self.amount
            )));
        }
        subtract_posted(&self.amount, &self.tax_amount)
    }
    /// Exact recognized amount; reject deferred amounts outside the ex-tax total.
    ///
    /// # Errors
    /// Whatever [`Self::amount_ex_tax`] raises; [`DomainError::CurrencyMismatch`] /
    /// [`DomainError::InconsistentScale`] when `deferred` carries different currency metadata;
    /// [`DomainError::AmountOutOfRange`] when `deferred` is negative or exceeds the ex-tax
    /// amount; the exact-arithmetic [`DomainError`] when the subtraction leaves the money
    /// contract.
    pub fn recognized(&self) -> Result<PostedMoney, DomainError> {
        let ex_tax = self.amount_ex_tax()?;
        matching_spec(&ex_tax, &self.deferred)?;
        if self.deferred.amount() < Decimal::ZERO {
            return Err(DomainError::AmountOutOfRange(format!(
                "debit-note deferred must be >= 0, got {}",
                self.deferred
            )));
        }
        if self.deferred.amount() > ex_tax.amount() {
            return Err(DomainError::AmountOutOfRange(format!(
                "debit-note deferred {} exceeds the ex-tax amount {ex_tax}",
                self.deferred
            )));
        }
        subtract_posted(&ex_tax, &self.deferred)
    }
}

/// One planned leg of a debit-note direct-split entry — a pure description the
/// handler maps onto a posting line (binding the chart `account_id` + scale).
/// `revenue_stream` is `Some` for the per-stream classes (`REVENUE`,
/// `CONTRACT_LIABILITY`) and `None` for the stream-less classes (`AR`,
/// `TAX_PAYABLE`).
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
    /// Tax dims (per-(jurisdiction, filing-period, rate) disaggregation, design §4.5).
    /// `Some` only on a `TAX_PAYABLE` leg built from a `TaxBreakdown`; `None` on
    /// every other leg.
    pub tax_jurisdiction: Option<String>,
    pub tax_filing_period: Option<String>,
    pub tax_rate_ref: Option<String>,
}

/// The full balanced direct-split leg plan for one debit note: the legs to post
/// plus the recognized / deferred ex-tax parts to record on the `debit_note` row.
/// Pure data — the handler posts the legs and persists the row from these fields;
/// the schedule build + headroom bump ride the handler's sidecar (not described
/// here — they key off `deferred_part` / `amount`).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebitNoteLegPlan {
    /// The balanced legs (Σ DR == Σ CR), in a deterministic order: DR `AR`, then
    /// CR `REVENUE`, then CR `CONTRACT_LIABILITY`, then CR `TAX_PAYABLE`.
    pub legs: Vec<PlannedLeg>,
    /// The ex-tax amount recognized now to `REVENUE` (`= ex_tax − deferred`).
    /// Recorded on the `debit_note` row.
    pub recognized_part: PostedMoney,
    /// The ex-tax amount deferred to `CONTRACT_LIABILITY` (`= deferred`).
    /// Recorded on the `debit_note` row; the handler builds the schedule for it.
    pub deferred_part: PostedMoney,
}

/// Validate a debit-note request's amounts + the deferral/recognition shape
/// (design §4.3). Pure shape checks: a negative amount/tax/deferred, a tax over the
/// note amount, a deferred part over the ex-tax revenue amount, an empty reason
/// code, or a deferred note that carries no recognition spec (a deferred line MUST
/// carry the spec the schedule build derives from — the S1 invoice-item-link rule,
/// §4.7 / D4).
///
/// # Errors
/// [`DomainError::AmountOutOfRange`] for a malformed amount/tax/deferred;
/// [`DomainError::InvalidRequest`] for an empty reason code or a deferred note that
/// is missing its recognition spec.
pub fn validate_shape(req: &DebitNoteRequest) -> Result<(), DomainError> {
    let ex_tax = req.amount_ex_tax()?;
    matching_spec(&req.amount, &req.deferred)?;
    if req.deferred.amount() < Decimal::ZERO || req.deferred.amount() > ex_tax.amount() {
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
            "debit note requires a non-empty reason_code / business context (AC #14)".to_owned(),
        ));
    }
    // D4 / §4.7: a deferring note MUST carry the recognition spec the schedule
    // build derives from (no deferred Contract-liability balance without a schedule
    // — the S1 rule). A fully-recognized note (`deferred == 0`) needs none.
    if req.deferred.amount() > Decimal::ZERO && req.recognition.is_none() {
        return Err(DomainError::InvalidRequest(
            "deferred debit note must carry a recognition spec to build its schedule (D4)"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Build the balanced direct-split leg plan for a debit note (design §4.3) — a
/// mirror of [`build_invoice_entry`](crate::domain::invoice::builder::build_invoice_entry)'s
/// per-item split, for a single charge line. Pure — no DB / txn. Produces:
///
/// - DR `AR` = `req.amount` (incl. tax) — the single additional receivable;
/// - CR `REVENUE` = `ex_tax − deferred` (the recognized-now part), carrying the
///   stream — emitted only when `> 0`;
/// - CR `CONTRACT_LIABILITY` = `deferred` (the per-PO deferred part), carrying the
///   stream — emitted only when `> 0` (NO zero-placeholder line);
/// - CR `TAX_PAYABLE` = `req.tax` (the posted tax evidence) — emitted only
///   when `> 0`.
///
/// The plan is balanced by construction (`DR AR == CR REVENUE + CR
/// CONTRACT_LIABILITY + CR TAX_PAYABLE == amount`), asserted before
/// returning. Zero-amount legs are omitted (inherited S1 / AC #4).
///
/// # Errors
/// [`DomainError::Internal`] if the plan does not balance — an invariant breach
/// that should be impossible once [`validate_shape`] has passed (the assertion
/// guards against a silent unbalanced post).
pub fn build_debit_note_legs(req: &DebitNoteRequest) -> Result<DebitNoteLegPlan, DomainError> {
    validate_shape(req)?;
    let deferred = req.deferred.clone();
    let recognized = req.recognized()?;
    let stream = req.revenue_stream.clone();

    let mut legs: Vec<PlannedLeg> = Vec::with_capacity(4);

    // --- Debit side: the single additional AR (incl. tax) ---
    if req.amount.amount() > Decimal::ZERO {
        legs.push(PlannedLeg {
            account_class: AccountClass::Ar,
            side: Side::Debit,
            amount: req.amount.clone(),
            revenue_stream: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }

    // --- Credit side: recognized REVENUE + deferred CONTRACT_LIABILITY + TAX ---
    // CR REVENUE — the recognized-now ex-tax part (per-stream class ⇒ carries the
    // stream). Omitted when the whole ex-tax amount defers (a lone CL credit then
    // balances the AR/tax debit, the S1 fully-deferred shape).
    if recognized.amount() > Decimal::ZERO {
        legs.push(PlannedLeg {
            account_class: AccountClass::Revenue,
            side: Side::Credit,
            amount: recognized.clone(),
            revenue_stream: Some(stream.clone()),
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }
    // CR CONTRACT_LIABILITY — the deferred per-PO part (per-stream class). NO
    // zero-placeholder line: a fully-recognized note (`deferred == 0`) emits none,
    // byte-identical to the S1 direct split for a non-deferred item.
    if deferred.amount() > Decimal::ZERO {
        legs.push(PlannedLeg {
            account_class: AccountClass::ContractLiability,
            side: Side::Credit,
            amount: deferred.clone(),
            revenue_stream: Some(stream),
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }
    // CR TAX_PAYABLE — the posted tax evidence (stream-less, never recomputed,
    // §4.3). Emit ONE CR TAX_PAYABLE per breakdown component carrying its
    // (jurisdiction, filing-period, rate) dims so the projector disaggregates
    // `tax_subbalance` per (jurisdiction, filing). A taxed note MUST carry a
    // breakdown (`validate_shape` rejects a bare `tax_amount`): a dimensionless
    // TAX_PAYABLE line carries no (jurisdiction, filing) and the schema rejects it
    // (chk_journal_line_tax_dims). Σ of the per-component legs == tax
    // (validated), so the plan still balances.
    if !req.tax.is_empty() {
        for t in &req.tax {
            if t.amount.amount() > Decimal::ZERO {
                legs.push(PlannedLeg {
                    account_class: AccountClass::TaxPayable,
                    side: Side::Credit,
                    amount: t.amount.clone(),
                    revenue_stream: None,
                    tax_jurisdiction: Some(t.tax_jurisdiction.clone()),
                    tax_filing_period: Some(t.tax_filing_period.clone()),
                    tax_rate_ref: t.tax_rate_ref.clone(),
                });
            }
        }
    }

    // Balance invariant (Σ DR == Σ CR). DR side is `amount` (the AR);
    // CR side is `recognized + deferred + tax == ex_tax + tax == amount`. A
    // zero-amount note emits no legs (both sides 0) — a benign no-op the handler
    // still records, but the post engine rejects an empty entry, so the handler
    // guards zero up-front.
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
    debug_assert_eq!(dr, cr, "debit-note leg plan must balance");
    if dr != cr {
        return Err(DomainError::Internal(format!(
            "debit-note leg plan does not balance (DR {dr} != CR {cr})"
        )));
    }

    Ok(DebitNoteLegPlan {
        legs,
        recognized_part: recognized,
        deferred_part: deferred,
    })
}

#[cfg(test)]
#[path = "debit_note_tests.rs"]
mod debit_note_tests;

#[cfg(test)]
#[path = "debit_note_message_tests.rs"]
mod debit_note_message_tests;
