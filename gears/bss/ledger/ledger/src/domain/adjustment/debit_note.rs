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
//! schedule will release), and the reversed-evidence tax (`tax`). The ex-tax total
//! is `amount_minor − tax_minor`; `deferred_minor` is how much of THAT ex-tax goes
//! to `CONTRACT_LIABILITY` (the rest recognizes now). [`build_debit_note_legs`]
//! asserts the balance as a domain invariant before returning.
//!
//! **No zero-placeholder lines (inherited S1 / AC #4).** A fully-recognized debit
//! note (`deferred_minor == 0`) emits NO `CONTRACT_LIABILITY` line (byte-identical
//! to the S1 direct split for a non-deferred item); a zero recognized-now part
//! (`deferred_minor == ex_tax`) emits NO `REVENUE` line; a zero tax emits no
//! `TAX_PAYABLE` line.

use bss_ledger_sdk::{AccountClass, Side};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::invoice::builder::TaxBreakdown;
use crate::domain::recognition::input::RecognitionInput;

/// One debit-note request — the pure inputs the handler resolves from the REST
/// DTO (Group E) before posting. Amounts are `i64` minor units; `amount_minor` is
/// **incl-tax** (the design's note amount), `tax_minor` is the tax slice of it
/// (carried posted tax evidence, never recomputed, §4.3), and `deferred_minor` is
/// how much of the ex-tax revenue portion (`amount_minor − tax_minor`) is deferred
/// to `CONTRACT_LIABILITY` per the line's PO (the rest recognizes now). When
/// `deferred_minor > 0` the [`Self::recognition`] spec drives the schedule build
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
    /// it raises that invoice's headroom (`debit_note_total_minor += amount`).
    pub origin_invoice_id: String,
    /// The targeted posted invoice-item ref — anchors the freshly-built
    /// `recognition_schedule`'s NOT-NULL `source_invoice_item_ref` when the note
    /// defers (§4.7). Required (non-empty) by the handler for a deferred note; a
    /// fully-recognized note may carry it for lineage but does not require it.
    pub origin_invoice_item_ref: Option<String>,
    /// The revenue stream the charge books against (the `REVENUE` /
    /// `CONTRACT_LIABILITY` legs carry it; per-stream classes need it).
    pub revenue_stream: String,
    /// ISO-4217 currency of the note (all legs share it).
    pub currency: String,
    /// The note amount **incl-tax**, in minor units (`>= 0`) — the single DR AR.
    pub amount_minor: i64,
    /// The tax slice of `amount_minor` posted onto `TAX_PAYABLE` (`>= 0`,
    /// `<= amount_minor`). Posted tax evidence — never recomputed here (§4.3). The
    /// ex-tax revenue amount is `amount_minor − tax_minor`.
    pub tax_minor: i64,
    /// The **authoritative** tax breakdown (computed by the tax engine for the
    /// *original* invoice's tax-date — the caller's concern; the gear only routes
    /// the dims, never recomputes, §4.5). Each component posts onto its OWN
    /// `TAX_PAYABLE` leg carrying its `(jurisdiction, filing-period, rate)` dims so
    /// the projector disaggregates `tax_subbalance` per `(jurisdiction, filing)`.
    /// REQUIRED when `tax_minor > 0` — `validate_shape` rejects a bare `tax_minor`
    /// (a dimensionless `TAX_PAYABLE` leg has no (jurisdiction, filing) and the
    /// schema rejects it, `chk_journal_line_tax_dims`). `tax_minor` remains the
    /// authoritative split scalar (`amount_minor_ex_tax = amount − tax_minor`); the
    /// breakdown MUST sum to it (`validate_shape`).
    pub tax: Vec<TaxBreakdown>,
    /// How much of the ex-tax revenue amount is **deferred** to
    /// `CONTRACT_LIABILITY` per the line's PO (`0 <= deferred_minor <= amount_minor
    /// − tax_minor`). The remainder (`ex_tax − deferred_minor`) recognizes now to
    /// `REVENUE`. `0` ⇒ fully recognized, NO `CONTRACT_LIABILITY` line + no schedule
    /// build (byte-identical to the S1 direct split for a non-deferred line).
    pub deferred_minor: i64,
    /// The mandatory business reason / context code (AC #14 — "MUST link business
    /// context", §4.3) recorded for audit. Non-empty.
    pub reason_code: String,
    /// The optional per-item ASC 606 recognition spec (Slice 4) — the SAME shape
    /// Slice 1's invoice-post item carries. REQUIRED to be `Some` when
    /// `deferred_minor > 0` (the handler runs it through the recognition
    /// [`ScheduleBuilder`](crate::domain::recognition::builder::ScheduleBuilder) to
    /// build the schedule that releases the deferred Contract-liability, D4). `None`
    /// for a fully-recognized note.
    pub recognition: Option<RecognitionInput>,
}

impl DebitNoteRequest {
    /// The ex-tax revenue amount — `amount_minor − tax_minor`, the note amount net
    /// of the posted tax. Saturating at 0 (a malformed `tax > amount` is rejected
    /// up-front by [`validate_shape`]; this never underflows once validated).
    #[must_use]
    pub fn amount_minor_ex_tax(&self) -> i64 {
        self.amount_minor.saturating_sub(self.tax_minor).max(0)
    }

    /// The recognized-now ex-tax amount — `ex_tax − deferred_minor` (the part the
    /// `CR REVENUE` leg books). Saturating at 0 (validated so `deferred <= ex_tax`).
    #[must_use]
    pub fn recognized_minor(&self) -> i64 {
        self.amount_minor_ex_tax()
            .saturating_sub(self.deferred_minor)
            .max(0)
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
    /// The leg amount in minor units (`> 0`; zero-amount legs are never emitted —
    /// inherited S1 / AC #4 rejects a zero placeholder line).
    pub amount_minor: i64,
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
/// here — they key off `deferred_part_minor` / `amount_minor`).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebitNoteLegPlan {
    /// The balanced legs (Σ DR == Σ CR), in a deterministic order: DR `AR`, then
    /// CR `REVENUE`, then CR `CONTRACT_LIABILITY`, then CR `TAX_PAYABLE`.
    pub legs: Vec<PlannedLeg>,
    /// The ex-tax amount recognized now to `REVENUE` (`= ex_tax − deferred`).
    /// Recorded on the `debit_note` row.
    pub recognized_part_minor: i64,
    /// The ex-tax amount deferred to `CONTRACT_LIABILITY` (`= deferred_minor`).
    /// Recorded on the `debit_note` row; the handler builds the schedule for it.
    pub deferred_part_minor: i64,
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
    if req.amount_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "debit-note amount_minor must be >= 0, got {}",
            req.amount_minor
        )));
    }
    if req.tax_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "debit-note tax_minor must be >= 0, got {}",
            req.tax_minor
        )));
    }
    if req.tax_minor > req.amount_minor {
        return Err(DomainError::AmountOutOfRange(format!(
            "debit-note tax_minor {} exceeds amount_minor {}",
            req.tax_minor, req.amount_minor
        )));
    }
    // Tax must carry a dimensioned breakdown: a TAX_PAYABLE journal line requires
    // (jurisdiction, filing_period) (chk_journal_line_tax_dims), so a bare
    // `tax_minor` with no breakdown can only build a dimensionless leg the schema
    // rejects at insert — reject it up front as a clean 400, not a late DB fault.
    if req.tax_minor > 0 && req.tax.is_empty() {
        return Err(DomainError::InvalidRequest(format!(
            "debit-note tax_minor {} requires a tax breakdown carrying \
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
                "debit-note tax breakdown sum {breakdown_sum} != tax_minor {}",
                req.tax_minor
            )));
        }
        if let Some(bad) = req.tax.iter().find(|t| t.currency != req.currency) {
            return Err(DomainError::AmountOutOfRange(format!(
                "debit-note tax breakdown currency {} != note currency {}",
                bad.currency, req.currency
            )));
        }
    }
    if req.deferred_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "debit-note deferred_minor must be >= 0, got {}",
            req.deferred_minor
        )));
    }
    if req.deferred_minor > req.amount_minor_ex_tax() {
        return Err(DomainError::AmountOutOfRange(format!(
            "debit-note deferred_minor {} exceeds the ex-tax amount {}",
            req.deferred_minor,
            req.amount_minor_ex_tax()
        )));
    }
    if req.reason_code.trim().is_empty() {
        return Err(DomainError::InvalidRequest(
            "debit note requires a non-empty reason_code / business context (AC #14)".to_owned(),
        ));
    }
    // D4 / §4.7: a deferring note MUST carry the recognition spec the schedule
    // build derives from (no deferred Contract-liability balance without a schedule
    // — the S1 rule). A fully-recognized note (`deferred == 0`) needs none.
    if req.deferred_minor > 0 && req.recognition.is_none() {
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
/// - DR `AR` = `req.amount_minor` (incl. tax) — the single additional receivable;
/// - CR `REVENUE` = `ex_tax − deferred` (the recognized-now part), carrying the
///   stream — emitted only when `> 0`;
/// - CR `CONTRACT_LIABILITY` = `deferred` (the per-PO deferred part), carrying the
///   stream — emitted only when `> 0` (NO zero-placeholder line);
/// - CR `TAX_PAYABLE` = `req.tax_minor` (the posted tax evidence) — emitted only
///   when `> 0`.
///
/// The plan is balanced by construction (`DR AR == CR REVENUE + CR
/// CONTRACT_LIABILITY + CR TAX_PAYABLE == amount_minor`), asserted before
/// returning. Zero-amount legs are omitted (inherited S1 / AC #4).
///
/// # Errors
/// [`DomainError::Internal`] if the plan does not balance — an invariant breach
/// that should be impossible once [`validate_shape`] has passed (the assertion
/// guards against a silent unbalanced post).
pub fn build_debit_note_legs(req: &DebitNoteRequest) -> Result<DebitNoteLegPlan, DomainError> {
    let ex_tax = req.amount_minor_ex_tax();
    let deferred = req.deferred_minor.clamp(0, ex_tax);
    let recognized = ex_tax - deferred;
    let stream = req.revenue_stream.clone();

    let mut legs: Vec<PlannedLeg> = Vec::with_capacity(4);

    // --- Debit side: the single additional AR (incl. tax) ---
    if req.amount_minor > 0 {
        legs.push(PlannedLeg {
            account_class: AccountClass::Ar,
            side: Side::Debit,
            amount_minor: req.amount_minor,
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
    if recognized > 0 {
        legs.push(PlannedLeg {
            account_class: AccountClass::Revenue,
            side: Side::Credit,
            amount_minor: recognized,
            revenue_stream: Some(stream.clone()),
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
        });
    }
    // CR CONTRACT_LIABILITY — the deferred per-PO part (per-stream class). NO
    // zero-placeholder line: a fully-recognized note (`deferred == 0`) emits none,
    // byte-identical to the S1 direct split for a non-deferred item.
    if deferred > 0 {
        legs.push(PlannedLeg {
            account_class: AccountClass::ContractLiability,
            side: Side::Credit,
            amount_minor: deferred,
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
    // breakdown (`validate_shape` rejects a bare `tax_minor`): a dimensionless
    // TAX_PAYABLE line carries no (jurisdiction, filing) and the schema rejects it
    // (chk_journal_line_tax_dims). Σ of the per-component legs == tax_minor
    // (validated), so the plan still balances.
    if !req.tax.is_empty() {
        for t in &req.tax {
            if t.amount_minor > 0 {
                legs.push(PlannedLeg {
                    account_class: AccountClass::TaxPayable,
                    side: Side::Credit,
                    amount_minor: t.amount_minor,
                    revenue_stream: None,
                    tax_jurisdiction: Some(t.tax_jurisdiction.clone()),
                    tax_filing_period: Some(t.tax_filing_period.clone()),
                    tax_rate_ref: t.tax_rate_ref.clone(),
                });
            }
        }
    }

    // Balance invariant (Σ DR == Σ CR). DR side is `amount_minor` (the AR);
    // CR side is `recognized + deferred + tax == ex_tax + tax == amount_minor`. A
    // zero-amount note emits no legs (both sides 0) — a benign no-op the handler
    // still records, but the post engine rejects an empty entry, so the handler
    // guards zero up-front.
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
    debug_assert_eq!(dr, cr, "debit-note leg plan must balance");
    if dr != cr {
        return Err(DomainError::Internal(format!(
            "debit-note leg plan does not balance (DR {dr} != CR {cr})"
        )));
    }

    Ok(DebitNoteLegPlan {
        legs,
        recognized_part_minor: recognized,
        deferred_part_minor: deferred,
    })
}

#[cfg(test)]
#[path = "debit_note_tests.rs"]
mod debit_note_tests;
