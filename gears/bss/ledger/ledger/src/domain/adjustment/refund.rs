//! Refund domain (Slice 3, Phase 2 / Group B1) — the **pure** request shape and
//! the deterministic two-leg plan a refund posts (design §4.4). Backend-agnostic:
//! no DB / txn / async I/O. The infra
//! [`RefundHandler`](crate::infra::adjustment::refund_service) resolves the origin
//! `payment_settlement` (by `payment_id` + `currency`), routes by `phase` to the
//! right two-leg shape, calls [`build_refund_legs`] for the balanced plan, and
//! posts it (persisting the `refund` row + the caps in Group C).
//!
//! **A refund is money-OUT against a settled receipt.** Unlike a credit note (a
//! revenue/AR restatement on an invoice) a refund returns cash to the payer; it
//! NEVER restates revenue and NEVER debits `CONTRACT_LIABILITY` (design §4.4 — an
//! unreleased-deferred restatement on a refunded invoice rides a *paired* S3 credit
//! note, not the refund). There are two patterns, by what the refund unwinds:
//!
//! **Pattern A (`A_UNALLOCATED`) — on-account / unallocated money** (the receipt
//! sits in the payer's unallocated pool, never applied to an invoice):
//!
//! | Stage | Phase | DR | CR |
//! |-------|-------|----|----|
//! | 1 (initiated) | `initiated` | `UNALLOCATED` | `REFUND_CLEARING` |
//! | 2 (confirmed) | `confirmed` | `REFUND_CLEARING` | `CASH_CLEARING` |
//!
//! **Pattern B (`B_RESTORE_AR`) — after allocation, restore the receivable** (the
//! receipt had been applied to an invoice; refunding it re-opens that AR):
//!
//! | Stage | Phase | DR | CR |
//! |-------|-------|----|----|
//! | 1 (initiated) | `initiated` | `AR` | `REFUND_CLEARING` |
//! | 2 (confirmed) | `confirmed` | `REFUND_CLEARING` | `CASH_CLEARING` |
//!
//! Both patterns drain through the two-stage `REFUND_CLEARING` liability (a
//! credit-normal, no-negative-guarded clearing account): stage-1 CREDITS it (the
//! cash is committed-out but not yet disbursed), stage-2 DEBITS it back to zero as
//! the cash leaves (`CR CASH_CLEARING`). Neither stage touches Revenue / Contra /
//! Contract-liability — **No P&L** (design §4.4).
//!
//! **Single-step (D1 switch).** When the PSP / tenant guarantees the disbursement
//! is atomic (no clearing remainder ever exists), [`RefundRequest::two_stage`] is
//! `false` and a single `initiated` entry posts straight to cash: Pattern A `DR
//! UNALLOCATED · CR CASH_CLEARING`; Pattern B `DR AR · CR CASH_CLEARING` — no
//! `REFUND_CLEARING` leg at all. The default is two-stage (the conservative shape
//! with an explicit clearing balance and aging). The single-step path is only the
//! `initiated` phase — a single-step refund never has a separate `confirmed` post.
//!
//! `payment_id` + `currency` are mandatory in BOTH patterns (design §9 D7 / Rev2
//! B-1): the handler resolves the origin `payment_settlement` from them (the
//! receipt the refund unwinds + its currency). Pattern B additionally requires the
//! `invoice_id` whose AR it restores; Pattern A must NOT carry one (its money never
//! reached an invoice).

use bss_ledger_sdk::{AccountClass, Side};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The lifecycle phase of a PSP refund (design §3 data model / §4.4). The wire
/// literals are the `chk_ledger_refund_phase` CHECK set; [`Self::as_str`] is the
/// single mapping used both for the `refund.phase` column and for the
/// `(psp_refund_id:phase)` idempotency business id. `Initiated` is stage-1,
/// `Confirmed` is stage-2; `Rejected`/`Voided` are the PSP-failure terminal phases
/// (the stage-1 line-negation + clearing reversal lands in Group E, not here);
/// `UnknownFinal` is the indeterminate-disposition terminal (Group F).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefundPhase {
    /// Stage-1: the refund was initiated at the PSP (cash committed-out, not yet
    /// disbursed). Posts the `CR REFUND_CLEARING` leg (two-stage) or the whole
    /// single-step entry.
    Initiated,
    /// Stage-2: the PSP confirmed the disbursement (cash left). Drains
    /// `REFUND_CLEARING` to `CASH_CLEARING` (two-stage only).
    Confirmed,
    /// Terminal: the PSP rejected the initiated refund (Group E — line-negate
    /// stage-1 + reverse the clearing leg; out of Group B scope).
    Rejected,
    /// Terminal: the initiated refund was voided before disbursement (Group E).
    Voided,
    /// Terminal: the refund's final disposition is indeterminate (Group F — clear
    /// `REFUND_CLEARING` to a documented loss line + secured audit; out of scope).
    UnknownFinal,
}

impl RefundPhase {
    /// The wire literal for this phase (the `chk_ledger_refund_phase` CHECK set +
    /// the idempotency business-id suffix). Mirrors the SDK `str_enum!` `as_str`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initiated => "initiated",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::Voided => "voided",
            Self::UnknownFinal => "unknown_final",
        }
    }

    /// Parse a stored wire literal back into a phase — the inverse of
    /// [`Self::as_str`]. Used to rebuild a refund from its dual-control intent
    /// snapshot on approve.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "initiated" => Some(Self::Initiated),
            "confirmed" => Some(Self::Confirmed),
            "rejected" => Some(Self::Rejected),
            "voided" => Some(Self::Voided),
            "unknown_final" => Some(Self::UnknownFinal),
            _ => None,
        }
    }
}

/// Which economic position a refund unwinds (design §4.4). The wire literals are
/// the `chk_ledger_refund_pattern` CHECK set; [`Self::as_str`] is the single
/// mapping for the `refund.pattern` column. The pattern fixes the stage-1 debit
/// account (`UNALLOCATED` for A, `AR` for B) and the `invoice_id` requirement
/// (None for A, required for B).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefundPattern {
    /// On-account / unallocated money: the receipt sits in the payer's
    /// `UNALLOCATED` pool (never applied to an invoice). Stage-1 debits
    /// `UNALLOCATED`; carries NO `invoice_id`.
    AUnallocated,
    /// After allocation: the receipt was applied to an invoice; the refund
    /// restores that receivable. Stage-1 debits `AR`; REQUIRES the `invoice_id`.
    BRestoreAr,
}

/// The economic DIRECTION of a refund-of-refund (Rev3 / S3-F1, design §4.4). A
/// refund-of-refund references a PRIOR refund (`relates_to_refund_id`); WHICH
/// money-out effect it carries is fixed by its sign, NOT by a lifecycle template:
///
/// - [`Self::Clawback`] — the PSP returned the disbursed cash to the merchant (a
///   refund was undone). It DECREMENTS the origin payment's money-out counters
///   (`payment_settlement.refunded_minor`, + `refunded_unallocated_minor` for a
///   Pattern-A origin / `payment_allocation_refund.refunded_minor` for Pattern B)
///   so the total money-out cap reflects the NET refunded, and its
///   `REFUND_CLEARING` leg drains in the OPPOSITE direction to an outbound refund.
///   The **canonical default** for a refund-of-refund (design D8): when the PSP
///   event does not say otherwise, it is a claw-back.
/// - [`Self::Outbound`] — cash goes OUT again (a further refund). It INCREMENTS
///   the same counters exactly like a plain stage-1 refund and rides the SAME
///   money-out cap. A first-order refund (no `relates_to_refund_id`) is always
///   outbound.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum RefundDirection {
    /// Cash leaves again — INCREMENT the money-out counters (a plain outbound
    /// refund, the only direction for a first-order refund).
    Outbound,
    /// Cash is returned to the merchant — DECREMENT the money-out counters under
    /// the underflow guard (the canonical default for a refund-of-refund, D8).
    #[default]
    Clawback,
}

impl RefundDirection {
    /// The wire literal for this direction (carried on the dual-control intent
    /// snapshot + a future `refund` column / REST discriminator). Mirrors the
    /// sibling enums' `as_str` convention.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Outbound => "OUTBOUND",
            Self::Clawback => "CLAWBACK",
        }
    }

    /// Parse a stored wire literal back into a direction — the inverse of
    /// [`Self::as_str`]. Used to rebuild a refund-of-refund from its dual-control
    /// intent snapshot on approve / from a queued claw-back payload on drain.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "OUTBOUND" => Some(Self::Outbound),
            "CLAWBACK" => Some(Self::Clawback),
            _ => None,
        }
    }
}

impl RefundPattern {
    /// The wire literal for this pattern (the `chk_ledger_refund_pattern` CHECK
    /// set + the `refund.pattern` column).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AUnallocated => "A_UNALLOCATED",
            Self::BRestoreAr => "B_RESTORE_AR",
        }
    }

    /// Parse a stored wire literal back into a pattern — the inverse of
    /// [`Self::as_str`]. Used to rebuild a refund from its dual-control intent
    /// snapshot on approve.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "A_UNALLOCATED" => Some(Self::AUnallocated),
            "B_RESTORE_AR" => Some(Self::BRestoreAr),
            _ => None,
        }
    }

    /// The stage-1 debit account class for this pattern: `UNALLOCATED` (A) draws
    /// down the on-account pool; `AR` (B) re-opens the receivable. (Stage-2 / the
    /// single-step credit side is always `CASH_CLEARING`; the stage-1 credit /
    /// single-step debit pivot on this.)
    #[must_use]
    pub fn debit_class(self) -> AccountClass {
        match self {
            Self::AUnallocated => AccountClass::Unallocated,
            Self::BRestoreAr => AccountClass::Ar,
        }
    }
}

/// One refund request — the pure inputs the handler resolves from the REST DTO
/// (Group G) before reading any ledger state. Amounts are `i64` minor units;
/// `amount_minor` is the cash to return (`>= 0`). `payment_id` + `currency` resolve
/// the origin `payment_settlement` (both patterns, D7); `invoice_id` is the AR the
/// refund restores (Pattern B only — `None` for A).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// The `*_id` fields mirror the storage / SDK column names verbatim; renaming to
// satisfy `struct_field_names` would diverge from `NewRefund` / the journal-line
// contract (the same `allow` the sibling note requests carry).
#[allow(clippy::struct_field_names)]
pub struct RefundRequest {
    /// The seller tenant whose ledger this posts into.
    pub tenant_id: Uuid,
    /// The tenant the refund returns cash to (the original payer / `UNALLOCATED`
    /// + `AR` owner; the cache grains key on it).
    pub payer_tenant_id: Uuid,
    /// The business id of this refund — the `refund` row's surrogate PK
    /// (`(tenant, refund_id)`) and the REST handle (`GET /refunds/{refundId}`).
    /// NOT the idempotency key (that is `(psp_refund_id:phase)`, see below).
    pub refund_id: String,
    /// The PSP's refund id — the natural idempotency grain together with `phase`
    /// (`UNIQUE (tenant, psp_refund_id, phase)`): one PSP refund advances through
    /// several phase rows. The engine claim is keyed on `(psp_refund_id:phase)`.
    pub psp_refund_id: String,
    /// The lifecycle phase this post records (`initiated` ⇒ stage-1, `confirmed`
    /// ⇒ stage-2). Routes the leg shape ([`build_refund_legs`]).
    pub phase: RefundPhase,
    /// Which economic position the refund unwinds (A on-account / B restore-AR) —
    /// fixes the stage-1 debit account + the `invoice_id` requirement.
    pub pattern: RefundPattern,
    /// The origin payment whose settlement the refund unwinds (NOT NULL both
    /// patterns, D7). The handler resolves `payment_settlement` from it +
    /// `currency`; a missing/mismatched settlement is a domain reject.
    pub payment_id: String,
    /// The invoice whose AR the refund restores — REQUIRED for Pattern B
    /// (`B_RESTORE_AR`), MUST be `None` for Pattern A (its money never reached an
    /// invoice). Enforced by [`validate_shape`].
    pub invoice_id: Option<String>,
    /// ISO-4217 currency of the refund (all legs share it; MUST match the origin
    /// settlement's currency — the handler validates it).
    pub currency: String,
    /// The cash to return, in minor units (`>= 0`).
    pub amount_minor: i64,
    /// `true` ⇒ the conservative two-stage shape (stage-1 `… · CR REFUND_CLEARING`,
    /// stage-2 `DR REFUND_CLEARING · CR CASH_CLEARING`); `false` ⇒ the single-step
    /// shape (D1 — `… · CR CASH_CLEARING` in one `initiated` post, no clearing leg),
    /// used only when the PSP/tenant guarantees atomic disbursement. Default
    /// two-stage.
    pub two_stage: bool,
    /// The PRIOR refund this one references (refund-of-refund, Rev3 / S3-F1, design
    /// §4.4 / §7) — the `refund.relates_to_refund_id` self-link. `None` ⇒ a
    /// first-order refund (the only kind in Groups B–D). `Some` ⇒ a refund-of-refund
    /// whose [`Self::direction`] fixes whether it claws back or sends more cash out;
    /// [`validate_shape`] requires it for a `Clawback`. This is a free reference
    /// link, NOT a strict line-negation — a claw-back is an ordinary entry that
    /// carries the link, with its own `psp_refund_id` + full phase lifecycle.
    pub relates_to_refund_id: Option<String>,
    /// The economic direction (refund-of-refund only — moot for a first-order
    /// refund, which is always [`RefundDirection::Outbound`]). The canonical default
    /// is [`RefundDirection::Clawback`] (design D8): a PSP refund-of-refund event,
    /// absent a discriminator, returns cash to the merchant. A `Clawback`
    /// DECREMENTS the origin money-out counters (under the underflow guard the
    /// handler applies in Group E); an `Outbound` INCREMENTS them like a plain
    /// stage-1 refund.
    pub direction: RefundDirection,
}

impl RefundRequest {
    /// `true` iff this is a refund-of-refund claw-back (a `Clawback` direction with
    /// a `relates_to_refund_id` link) — the path that DECREMENTS the origin
    /// money-out counters under the underflow guard (Group E). An `Outbound`
    /// refund-of-refund (cash out again) and every first-order refund INCREMENT
    /// instead, so they are NOT claw-backs.
    #[must_use]
    pub fn is_clawback(&self) -> bool {
        matches!(self.direction, RefundDirection::Clawback) && self.relates_to_refund_id.is_some()
    }
}

/// One planned leg of a refund entry — a pure description the handler maps onto a
/// posting line (binding the chart `account_id` + scale). All refund classes
/// (`UNALLOCATED`, `AR`, `REFUND_CLEARING`, `CASH_CLEARING`) are stream-less, so
/// `revenue_stream` is always `None`; the field is kept for shape-parity with the
/// sibling note `PlannedLeg`s and the `mk_line` mapping.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedLeg {
    /// The account class this leg posts to.
    pub account_class: AccountClass,
    /// DR / CR.
    pub side: Side,
    /// The leg amount in minor units (`> 0`; zero-amount legs are never emitted —
    /// inherited S1 / AC #4 rejects a zero placeholder line, and the handler
    /// guards a zero-amount refund up-front).
    pub amount_minor: i64,
    /// Always `None` for a refund (every refund class is stream-less); present for
    /// shape-parity with the note leg plans.
    pub revenue_stream: Option<String>,
}

/// The full balanced two-leg plan for one refund stage (design §4.4): the legs to
/// post plus the resulting `clearing_state` to stamp on the `refund` row. Pure data
/// — the handler posts the legs, persists the row, and (Group C) increments the
/// caps from the request.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefundLegPlan {
    /// The balanced legs (Σ DR == Σ CR), debit first then credit. Always exactly
    /// two legs (one DR, one CR) — a refund stage is a single clearing/cash move.
    pub legs: Vec<PlannedLeg>,
    /// The `clearing_state` literal (`chk_ledger_refund_clearing_state`) this stage
    /// leaves the refund in: stage-1 two-stage ⇒ `PENDING` (the `REFUND_CLEARING`
    /// balance is now open); stage-2 ⇒ `SETTLED` (it drained to zero); single-step
    /// `initiated` ⇒ `SETTLED` (cash left in one move, no clearing balance ever).
    pub clearing_state: &'static str,
}

/// `clearing_state` literal: the `REFUND_CLEARING` balance is open (stage-1
/// two-stage).
pub const CLEARING_STATE_PENDING: &str = "PENDING";
/// `clearing_state` literal: the disbursement completed / no clearing balance
/// remains (stage-2, or single-step `initiated`).
pub const CLEARING_STATE_SETTLED: &str = "SETTLED";
/// `clearing_state` literal: a stage-1 leg was reversed (PSP reject/void — Group E).
pub const CLEARING_STATE_REVERSED: &str = "REVERSED";

/// Validate a refund request's amounts + the pattern/phase/invoice shape (design
/// §4.4). Pure shape checks the handler does not own:
///
/// - `amount_minor >= 0` (a zero refund is a benign no-op the handler rejects
///   up-front before the empty-entry engine check — out of this gate);
/// - Pattern B (`B_RESTORE_AR`) REQUIRES a non-empty `invoice_id` (the AR it
///   restores); Pattern A (`A_UNALLOCATED`) MUST NOT carry one (its money never
///   reached an invoice);
/// - the single-step path (`two_stage == false`) is only valid for the `initiated`
///   phase (a single-step refund has no separate `confirmed` post — its one entry
///   already moved the cash);
/// - `confirmed` is a two-stage-only phase (it drains the stage-1
///   `REFUND_CLEARING`; there is none to drain in single-step).
/// - a refund-of-refund [`RefundDirection::Clawback`] REQUIRES a non-empty
///   `relates_to_refund_id` (the prior refund it claws back — Rev3 / S3-F1); a
///   `relates_to_refund_id` set with the (default) `Clawback` direction is the
///   canonical refund-of-refund (D8). An `Outbound` refund-of-refund (cash out
///   again) MAY carry the link but does not require it.
///
/// # Errors
/// [`DomainError::AmountOutOfRange`] for a negative amount;
/// [`DomainError::InvalidRequest`] for an invoice/pattern mismatch, an invalid
/// phase/`two_stage` combination, or a `Clawback` missing its
/// `relates_to_refund_id`.
pub fn validate_shape(req: &RefundRequest) -> Result<(), DomainError> {
    if req.amount_minor < 0 {
        return Err(DomainError::AmountOutOfRange(format!(
            "refund amount_minor must be >= 0, got {}",
            req.amount_minor
        )));
    }
    // A claw-back direction is meaningless without the prior refund it claws back
    // (Rev3 / S3-F1, design §7): require the self-link. (`Outbound` is the default
    // for a first-order refund and needs no link.)
    if matches!(req.direction, RefundDirection::Clawback)
        && req
            .relates_to_refund_id
            .as_deref()
            .is_none_or(|i| i.trim().is_empty())
    {
        return Err(DomainError::InvalidRequest(
            "a claw-back refund-of-refund (direction = CLAWBACK) requires a non-empty \
             relates_to_refund_id (the prior refund it claws back)"
                .to_owned(),
        ));
    }
    match req.pattern {
        RefundPattern::BRestoreAr => {
            if req
                .invoice_id
                .as_deref()
                .is_none_or(|i| i.trim().is_empty())
            {
                return Err(DomainError::InvalidRequest(
                    "Pattern B (B_RESTORE_AR) refund requires a non-empty invoice_id (the AR it \
                     restores)"
                        .to_owned(),
                ));
            }
        }
        RefundPattern::AUnallocated => {
            if req.invoice_id.is_some() {
                return Err(DomainError::InvalidRequest(
                    "Pattern A (A_UNALLOCATED) refund must not carry an invoice_id (its money \
                     never reached an invoice)"
                        .to_owned(),
                ));
            }
        }
    }
    // The single-step shape (D1) posts the whole cash move in one `initiated`
    // entry; it has no `confirmed` stage to drain. Conversely a `confirmed` post
    // only exists in the two-stage flow (it drains the stage-1 REFUND_CLEARING).
    if !req.two_stage && req.phase == RefundPhase::Confirmed {
        return Err(DomainError::InvalidRequest(
            "single-step refund (two_stage = false) has no confirmed stage — its initiated entry \
             already moved the cash"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Build the balanced two-leg plan for one refund stage (design §4.4). Pure — no
/// DB / txn. Routes by `(pattern, phase, two_stage)` AND
/// [`RefundRequest::direction`]:
///
/// **Outbound (cash leaves — a plain refund or an `Outbound` refund-of-refund):**
/// - **stage-1 two-stage** (`phase == Initiated`, `two_stage`): DR `pattern.debit`
///   (`UNALLOCATED` / `AR`) · CR `REFUND_CLEARING` ⇒ `clearing_state = PENDING`;
/// - **stage-2** (`phase == Confirmed`, two-stage only): DR `REFUND_CLEARING` · CR
///   `CASH_CLEARING` ⇒ `clearing_state = SETTLED` (drains clearing to zero);
/// - **single-step** (`phase == Initiated`, `!two_stage`): DR `pattern.debit` · CR
///   `CASH_CLEARING` in one move ⇒ `clearing_state = SETTLED` (no clearing leg).
///
/// **Claw-back (cash returns to the merchant — a `Clawback` refund-of-refund,
/// Rev3 / S3-F1):** the SAME accounts as outbound but every leg's SIDE is inverted
/// (`REFUND_CLEARING` drains in the OPPOSITE direction), so the cash flows back in
/// and the drawn-down `UNALLOCATED`(A) / `AR`(B) is RESTORED:
/// - **stage-1 two-stage**: DR `REFUND_CLEARING` · CR `pattern.debit` ⇒
///   `clearing_state = PENDING` (the negative clearing balance opens);
/// - **stage-2**: DR `CASH_CLEARING` · CR `REFUND_CLEARING` ⇒ `SETTLED` (cash comes
///   back in, draining the clearing);
/// - **single-step**: DR `CASH_CLEARING` · CR `pattern.debit` ⇒ `SETTLED`.
///
/// A claw-back is an ordinary entry with `relates_to_refund_id` plus its own
/// `psp_refund_id` / phase lifecycle — NOT a strict line-negation of the prior
/// refund.
///
/// NEVER emits a `CONTRACT_LIABILITY` leg (design §4.4) and never touches Revenue /
/// Contra. The plan is balanced by construction (the single DR equals the single
/// CR, both `amount_minor`), asserted before returning.
///
/// A terminal phase (`Rejected` / `Voided` / `UnknownFinal`) has no Group-B posting
/// shape — the stage-1 reversal (Group E) and the unknown-final disposition (Group
/// F) own those; this returns [`DomainError::InvalidRequest`] for them (the handler
/// only drives `Initiated` / `Confirmed` in this group).
///
/// # Errors
/// [`DomainError::InvalidRequest`] for a terminal phase (no Group-B shape) or an
/// invalid phase/`two_stage` combination (mirrors [`validate_shape`], so a
/// validated request never hits these); [`DomainError::Internal`] if the
/// constructed plan does not balance (an invariant breach — the assertion guards a
/// silent unbalanced post).
pub fn build_refund_legs(req: &RefundRequest) -> Result<RefundLegPlan, DomainError> {
    let amount = req.amount_minor;
    // The OUTBOUND (cash-out) `(debit_class, credit_class, clearing_state)` by the
    // routing matrix. A claw-back uses the SAME accounts but inverts each leg's
    // side below (cash flows the opposite way). A zero-amount refund still routes
    // here (the handler guards amount == 0 before calling), but we keep the
    // leg-emit guarded on `> 0` so a zero plan is empty rather than carrying zero
    // legs (mirrors the note builders).
    let (out_debit, out_credit, clearing_state) = match (req.phase, req.two_stage) {
        // Stage-1, two-stage: pattern debit → REFUND_CLEARING (open the clearing).
        (RefundPhase::Initiated, true) => (
            req.pattern.debit_class(),
            AccountClass::RefundClearing,
            CLEARING_STATE_PENDING,
        ),
        // Stage-2: drain REFUND_CLEARING → CASH_CLEARING (cash leaves).
        (RefundPhase::Confirmed, true) => (
            AccountClass::RefundClearing,
            AccountClass::CashClearing,
            CLEARING_STATE_SETTLED,
        ),
        // Single-step `initiated`: pattern debit → CASH_CLEARING in one move.
        (RefundPhase::Initiated, false) => (
            req.pattern.debit_class(),
            AccountClass::CashClearing,
            CLEARING_STATE_SETTLED,
        ),
        // `confirmed` with two_stage == false is rejected by validate_shape; defend
        // it here too (no clearing to drain in single-step).
        (RefundPhase::Confirmed, false) => {
            return Err(DomainError::InvalidRequest(
                "single-step refund has no confirmed stage".to_owned(),
            ));
        }
        // Terminal phases have no Group-B posting shape (reversal = Group E,
        // unknown-final = Group F).
        (RefundPhase::Rejected | RefundPhase::Voided | RefundPhase::UnknownFinal, _) => {
            return Err(DomainError::InvalidRequest(format!(
                "refund phase {} has no two-stage posting shape in this group (reversal/disposition \
                 are handled elsewhere)",
                req.phase.as_str()
            )));
        }
    };

    // A claw-back (cash returns to the merchant) posts the SAME two accounts as the
    // matching outbound stage but with each leg's side flipped — `REFUND_CLEARING`
    // drains the other way, restoring the drawn-down UNALLOCATED(A) / AR(B). An
    // outbound refund posts the matrix as-is. The pair-flip preserves the balance
    // (one DR, one CR of equal amount) either way.
    let (debit_class, credit_class) = match req.direction {
        RefundDirection::Outbound => (out_debit, out_credit),
        RefundDirection::Clawback => (out_credit, out_debit),
    };

    let mut legs: Vec<PlannedLeg> = Vec::with_capacity(2);
    if amount > 0 {
        legs.push(PlannedLeg {
            account_class: debit_class,
            side: Side::Debit,
            amount_minor: amount,
            revenue_stream: None,
        });
        legs.push(PlannedLeg {
            account_class: credit_class,
            side: Side::Credit,
            amount_minor: amount,
            revenue_stream: None,
        });
    }

    // Balance invariant (Σ DR == Σ CR). Both legs are `amount`, one DR one CR, so a
    // non-zero refund always balances; a zero refund emits no legs (the handler
    // rejects zero up-front — the engine rejects an empty entry).
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
    debug_assert_eq!(dr, cr, "refund leg plan must balance");
    if dr != cr {
        return Err(DomainError::Internal(format!(
            "refund leg plan does not balance (DR {dr} != CR {cr})"
        )));
    }
    // A refund leg must NEVER debit CONTRACT_LIABILITY (design §4.4 — the
    // unreleased-deferred restatement rides a paired credit note, not the refund).
    // The routing matrix above can structurally never produce one; this debug
    // assertion documents + pins the invariant for the test suite.
    debug_assert!(
        legs.iter()
            .all(|l| l.account_class != AccountClass::ContractLiability),
        "refund must never touch CONTRACT_LIABILITY"
    );

    Ok(RefundLegPlan {
        legs,
        clearing_state,
    })
}

#[cfg(test)]
#[path = "refund_tests.rs"]
mod refund_tests;
