//! Governed manual-adjustment domain (Slice 3, Phase 3 / Group 1) — the **pure**
//! request shape + the deterministic [`govern`] gate a governed manual posting must
//! clear (design §4.6). Backend-agnostic: no DB / txn / async I/O. The infra
//! handler (Group 3) resolves the REST DTO into a [`ManualAdjustmentRequest`], runs
//! [`govern`] out-of-txn (a `Reject` short-circuits the post), then — on `Ok` —
//! binds the [`ManualLeg`]s onto posting lines and posts them atomically.
//!
//! **Why a governor, not a free-form post.** A manual adjustment is the ledger's
//! escape hatch for corrections the typed flows (invoice / settle / allocate /
//! S3 notes / S4 recognition) do not cover — rounding residue, suspense /
//! cash-clearing clean-up. Left ungoverned it is the obvious vector for a
//! disguised write-off (a hidden bad-debt) or a direct revenue restatement that
//! bypasses ASC 606. [`govern`] therefore enforces a **closed** contract: only the
//! per-action code-owned allow-list of account classes (below) may post, `REVENUE`
//! and `CONTRACT_LIABILITY` are globally off-limits, and any `CONTRA_REVENUE` leg
//! must be a *paired* revenue reduction or it is rejected as an attempted
//! write-off.
//!
//! **The code-owned allow-list (design §4.6 / Rev3 S3-minor).** Each
//! [`ManualAdjustmentAction`] owns a fixed `&[AccountClass]` it may touch — see
//! [`ManualAdjustmentAction::allowed_classes`]. This is a deliberate MVP: the
//! allow-list lives in code, so **every** new governed action (or a widening of an
//! existing one) is a reviewed, deliberate deploy rather than a runtime config
//! toggle. A data/config-driven allow-list + its meta-governance (who may edit it,
//! under what dual-control) is explicitly deferred. `CONTRA_REVENUE` is in **no**
//! allowed-set: it is reachable only through the write-off structural guard, which
//! is precisely what rejects it (a bare contra-revenue leg is a write-off).
//!
//! **The governor is sync + pure** (mirroring [`super::credit_note`] /
//! [`super::splitter`]): it is a function of the already-resolved request, so there
//! is no I/O to model and it stays unit-testable without a database. `SoD`
//! (`preparer ≠ approver`) and the dual-control threshold are NOT enforced here —
//! they are the `ApprovalService`'s concern (Group 4); this layer only carries the
//! actor ids so the handler can hand them on.

use bss_ledger_sdk::{AccountClass, Side};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::invoice::builder::TaxBreakdown;

/// Which governed manual-adjustment a request performs — the discriminator that
/// selects the [`Self::allowed_classes`] allow-list and is stamped (as
/// [`Self::as_str`]) onto the adjustment record. The set is deliberately small
/// (design §4.6 / Rev3 S3-minor): each variant is a reviewed governed capability,
/// NOT a free-form posting mode.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualAdjustmentAction {
    /// Correct sub-minor rounding residue stranded in a parking class (e.g. a
    /// 1-minor remainder in `SUSPENSE` / `CASH_CLEARING` after a split).
    RoundingCorrection,
    /// Clear a stale `SUSPENSE` balance to its resolved home (a clean-up move
    /// between parking / clearing classes). An AR write-off to `GOODWILL` is NOT
    /// permitted here: bad-debt / write-off is out of MVP scope (design §1.2), and
    /// any goodwill relief must route through the governed credit-note path (with its
    /// AR-floor cap + dual-control), never a bare suspense-clear.
    SuspenseClear,
}

impl ManualAdjustmentAction {
    /// The stable wire/DB token. Inverse of [`Self::parse`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoundingCorrection => "ROUNDING_CORRECTION",
            Self::SuspenseClear => "SUSPENSE_CLEAR",
        }
    }

    /// Parse a stored token back into an action — the inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ROUNDING_CORRECTION" => Some(Self::RoundingCorrection),
            "SUSPENSE_CLEAR" => Some(Self::SuspenseClear),
            _ => None,
        }
    }

    /// The **code-owned** allow-list: the account classes this action may post to
    /// (design §4.6 / Rev3 S3-minor). Anything outside the returned slice is a
    /// [`ManualAdjustmentReject::NotAllowed`].
    ///
    /// This is an intentional MVP shape: the allow-list is compiled in, so every
    /// new governed action — and every widening of an existing one — is a
    /// deliberate, reviewed deploy rather than a runtime config toggle. A
    /// data/config-driven allow-list and its meta-governance (who may edit it, and
    /// under what dual-control) are deferred.
    ///
    /// `CONTRA_REVENUE` is in **no** allowed-set on purpose: a bare contra-revenue
    /// leg is the disguised-write-off shape the [`govern`] write-off guard rejects,
    /// so it must never be reachable via the allow-list. `REVENUE` /
    /// `CONTRACT_LIABILITY` are likewise absent (and additionally banned globally
    /// by [`govern`]): revenue changes route through S3/S4/S6, never a manual post.
    #[must_use]
    pub fn allowed_classes(self) -> &'static [AccountClass] {
        match self {
            Self::RoundingCorrection => &[
                AccountClass::Suspense,
                AccountClass::CashClearing,
                AccountClass::Ar,
                AccountClass::Unallocated,
            ],
            // `Goodwill` is deliberately ABSENT: an AR→GOODWILL move here is a bad-debt
            // write-off (out of MVP scope, design §1.2) the write-off guard does not
            // catch (it inspects only CONTRA_REVENUE legs). Goodwill relief routes
            // through the governed credit-note path, never a manual suspense-clear.
            Self::SuspenseClear => &[
                AccountClass::Suspense,
                AccountClass::Ar,
                AccountClass::CashClearing,
                AccountClass::Unallocated,
            ],
        }
    }
}

/// One planned leg of a governed manual adjustment — a pure description the handler
/// (Group 3) maps onto a posting line (binding the chart `account_id` + scale).
/// All legs of a request share the request's currency; `revenue_stream` is `Some`
/// only for the per-stream classes (it is otherwise `None`).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualLeg {
    /// The account class this leg posts to (subject to the action's allow-list +
    /// the global `REVENUE`/`CONTRACT_LIABILITY` ban + the write-off guard).
    pub account_class: AccountClass,
    /// DR / CR.
    pub side: Side,
    /// The leg amount in minor units (`> 0`; zero/negative legs are rejected by
    /// [`govern`], inherited S1 / AC #4).
    pub amount_minor: i64,
    /// The revenue stream — `Some` only for a per-stream class
    /// ([`AccountClass::is_per_stream`]); `None` for the stream-less classes.
    pub revenue_stream: Option<String>,
}

/// One governed manual-adjustment request — the pure inputs the handler (Group 3)
/// resolves from the REST DTO before posting. Amounts are `i64` minor units; all
/// legs are in [`Self::currency`]. The request is idempotent on the `(tenant,
/// MANUAL_ADJUSTMENT, adjustment_id)` key.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualAdjustmentRequest {
    /// The tenant whose ledger this posts into.
    pub tenant_id: Uuid,
    /// The payer tenant the legs attribute to, when the adjustment touches a
    /// payer-scoped balance (`AR` / `UNALLOCATED`); `None` for a payer-less
    /// internal clean-up.
    pub payer_tenant_id: Option<Uuid>,
    /// The business id of this adjustment — the `(tenant, MANUAL_ADJUSTMENT,
    /// adjustment_id)` idempotency key.
    pub adjustment_id: String,
    /// Which governed action this is (selects the allow-list, stamped on the row).
    pub action: ManualAdjustmentAction,
    /// ISO-4217 currency of the adjustment (every leg shares it).
    pub currency: String,
    /// The legs to post — must net to zero per [`govern`] (Σ DR == Σ CR).
    pub legs: Vec<ManualLeg>,
    /// The mandatory business reason code (AC #14). A governed manual adjustment
    /// MUST justify itself; an empty/blank reason is rejected by [`govern`].
    pub reason_code: String,
    /// The actor posting the adjustment (the preparer subject). Carried for the
    /// audit record and the dual-control `SoD` check (enforced in Group 4, not here).
    pub preparer_actor_id: Uuid,
    /// The second actor when the adjustment is over the dual-control threshold (the
    /// approver). `None` below threshold. The `preparer ≠ approver` `SoD` check is the
    /// `ApprovalService`'s (Group 4); [`govern`] does NOT enforce it, but carries
    /// the field so the handler can.
    pub approver_actor_id: Option<Uuid>,
    /// The authoritative tax breakdown for a tax-bearing action — never recomputed
    /// here. Usually empty for the MVP actions (rounding / suspense clean-up rarely
    /// move tax), but carried for the Group 2 tax-routing the handler will drive.
    pub tax: Vec<TaxBreakdown>,
}

/// The pure domain signal a governed manual adjustment is rejected (design §4.6) —
/// distinct from [`crate::domain::error::DomainError`]: the handler (Group 3) maps
/// **both** variants onto [`crate::domain::error::DomainError::ManualAdjustmentNotAllowed`]
/// (a 400), but treats them differently for observability.
///
/// - [`Self::NotAllowed`] — a generic governance violation (blank reason,
///   unbalanced / zero / negative legs, a class outside the allow-list, or the
///   global `REVENUE`/`CONTRACT_LIABILITY` ban). A plain rejection, no alarm.
/// - [`Self::AttemptedWriteOff`] — a `CONTRA_REVENUE` leg with no paired same-stream
///   recognized-revenue reduction: the disguised bad-debt write-off shape. The
///   handler additionally fires a `SecuredAuditSink` capture + a page (the
///   `AttemptedWriteOff` alarm), because this is a deliberate-misuse signal, not a
///   benign typo.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualAdjustmentReject {
    /// A generic governance violation (see the type doc). Carries a human-readable
    /// detail the handler forwards into the canonical 400.
    NotAllowed(String),
    /// A `CONTRA_REVENUE` leg without a paired same-stream revenue reduction — an
    /// attempted (disguised) write-off. The handler captures + pages on this.
    AttemptedWriteOff(String),
}

/// Govern a manual-adjustment request: the pure §4.6 gate every governed manual
/// posting must clear BEFORE it is allowed to post. No DB / txn — a function of the
/// already-resolved request.
///
/// The checks run in this **order** (the order matters: the write-off shape is
/// detected as its own signal *before* the generic allow-list check, so a
/// disguised bad-debt pages rather than reading as a plain "class not allowed"):
///
/// 1. **Reason** — a blank `reason_code` is [`ManualAdjustmentReject::NotAllowed`]
///    (AC #14: a governed adjustment must justify itself).
/// 2. **Shape** — no legs, or any `amount_minor <= 0`, is `NotAllowed` (zero /
///    negative legs are forbidden, inherited S1 / AC #4).
/// 3. **Balance** — the legs must net to zero (Σ `Side::Debit` == Σ `Side::Credit`,
///    one currency), else `NotAllowed`. Summed in `i128` so a pathological set
///    cannot overflow `i64`.
/// 4. **Global `REVENUE` / `CONTRACT_LIABILITY` ban** — a governed posting MUST NOT
///    directly DR/CR `REVENUE` or touch `CONTRACT_LIABILITY` (revenue changes route
///    through S3/S4/S6, design §4.6). Either is `NotAllowed`.
/// 5. **Write-off structural guard** — a `CONTRA_REVENUE` leg is legitimate ONLY if
///    the same entry carries a paired *recognized-revenue reduction* for the SAME
///    `revenue_stream` (a `REVENUE` leg on the revenue-reducing side, `Side::Debit`).
///    Since step 4 already banned every `REVENUE` leg the pair can never exist, so a
///    `CONTRA_REVENUE` leg here is always an [`ManualAdjustmentReject::AttemptedWriteOff`].
///    The pairing is nonetheless checked *structurally* (not short-circuited) — it
///    is the contract shape Slice 6 (legitimate write-offs paired with a revenue
///    reduction) will reuse.
/// 6. **Allow-list** — every remaining leg's class must be in
///    [`ManualAdjustmentAction::allowed_classes`], else `NotAllowed`.
///    (`CONTRA_REVENUE` never reaches here — step 5 catches it; `REVENUE` /
///    `CONTRACT_LIABILITY` — step 4.)
///
/// # Errors
/// [`ManualAdjustmentReject::AttemptedWriteOff`] for the unpaired-contra-revenue
/// shape; [`ManualAdjustmentReject::NotAllowed`] for every other governance
/// violation.
pub fn govern(req: &ManualAdjustmentRequest) -> Result<(), ManualAdjustmentReject> {
    use ManualAdjustmentReject as R;

    // 1. Reason code (AC #14).
    if req.reason_code.trim().is_empty() {
        return Err(R::NotAllowed(
            "manual adjustment requires a non-empty reason_code (AC #14)".to_owned(),
        ));
    }

    // 1b. Tax (Z7-1): the MVP governed actions move NO tax — `TAX_PAYABLE` is in no
    //     action's allow-list, and the dual-control snapshot (`ManualAdjustmentIntent`)
    //     deliberately does NOT carry `tax` (rebuilt empty on the approved replay). A
    //     non-empty `tax` here would therefore be SILENTLY DROPPED on an over-D2
    //     replay, so reject it up front — this makes the snapshot-drop provably
    //     faithful and keeps the `tax` field from arming a future divergence.
    if !req.tax.is_empty() {
        return Err(R::NotAllowed(
            "governed manual adjustment must not carry tax (no governed action moves tax)"
                .to_owned(),
        ));
    }

    // 2. Shape: at least one leg, every leg strictly positive (inherited S1 / AC #4
    //    — a governed adjustment is a real money move, never a zero/negative
    //    placeholder).
    if req.legs.is_empty() {
        return Err(R::NotAllowed("manual adjustment has no legs".to_owned()));
    }
    if let Some(bad) = req.legs.iter().find(|l| l.amount_minor <= 0) {
        return Err(R::NotAllowed(format!(
            "manual adjustment leg amount_minor must be > 0, got {} for {}",
            bad.amount_minor,
            bad.account_class.as_str()
        )));
    }

    // 3. Balance: Σ DR == Σ CR (one currency, `req.currency`). i128 accumulation so
    //    a pathological leg set cannot overflow before the comparison.
    let mut dr: i128 = 0;
    let mut cr: i128 = 0;
    for leg in &req.legs {
        match leg.side {
            Side::Debit => dr += i128::from(leg.amount_minor),
            Side::Credit => cr += i128::from(leg.amount_minor),
        }
    }
    if dr != cr {
        return Err(R::NotAllowed(format!(
            "manual adjustment legs do not net to zero (DR {dr} != CR {cr})"
        )));
    }

    // 4. Global REVENUE / CONTRACT_LIABILITY ban (design §4.6): a governed posting
    //    must NOT directly move REVENUE or touch CONTRACT_LIABILITY on EITHER side —
    //    revenue changes route through S3 (notes), S4 (recognition), S6 (write-offs).
    if req
        .legs
        .iter()
        .any(|l| l.account_class == AccountClass::Revenue)
    {
        return Err(R::NotAllowed(
            "manual adjustment must not post REVENUE directly (route revenue changes through \
             S3/S4)"
                .to_owned(),
        ));
    }
    if req
        .legs
        .iter()
        .any(|l| l.account_class == AccountClass::ContractLiability)
    {
        return Err(R::NotAllowed(
            "manual adjustment must not touch CONTRACT_LIABILITY outside S3/S4/S6".to_owned(),
        ));
    }

    // 5. Write-off structural guard (design §4.6 / Rev3 S3-minor): a CONTRA_REVENUE
    //    leg is legitimate ONLY paired — in the SAME entry — with a recognized-
    //    REVENUE reduction (a `REVENUE` leg on the revenue-reducing side, DR) for
    //    the SAME revenue_stream. The pairing is checked structurally for every
    //    contra-revenue leg (this is the contract shape Slice 6 reuses); because
    //    step 4 already banned every REVENUE leg, no pair can exist, so an unpaired
    //    CONTRA_REVENUE leg is an attempted (disguised bad-debt) write-off.
    for contra in req
        .legs
        .iter()
        .filter(|l| l.account_class == AccountClass::ContraRevenue)
    {
        let paired = req.legs.iter().any(|l| {
            l.account_class == AccountClass::Revenue
                && l.side == Side::Debit
                && l.revenue_stream == contra.revenue_stream
        });
        if !paired {
            return Err(R::AttemptedWriteOff(
                "CONTRA_REVENUE leg without a paired same-stream recognized-REVENUE reduction is \
                 an attempted write-off (disguised bad-debt) — out of scope; route via GOODWILL / \
                 S3"
                .to_owned(),
            ));
        }
    }

    // 6. Allow-list: every leg's class must be in the action's allowed-set.
    //    (CONTRA_REVENUE is filtered out by step 5; REVENUE / CONTRACT_LIABILITY by
    //    step 4 — so only the parking / clearing classes reach here.)
    let allowed = req.action.allowed_classes();
    if let Some(bad) = req
        .legs
        .iter()
        .find(|l| !allowed.contains(&l.account_class))
    {
        return Err(R::NotAllowed(format!(
            "account class {} is outside the {} allow-list",
            bad.account_class.as_str(),
            req.action.as_str()
        )));
    }

    Ok(())
}

#[cfg(test)]
#[path = "manual_tests.rs"]
mod manual_tests;
