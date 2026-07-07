//! Resolver port traits for recognition derivation (design §4.2 / §4.4) + their
//! v1 config/input-backed default impls.
//!
//! Three ports, each a **sync** trait (mirroring
//! [`crate::domain::ports::metrics`]): the v1 derivation reads only **local**
//! facts (the post request's item attributes + tenant-config defaults), so there
//! is no async I/O to model and a sync trait keeps the
//! [`ScheduleBuilder`](super::builder::ScheduleBuilder) callable from the in-txn
//! Group C sidecar without an executor.
//!
//! - [`DeferralPolicyResolver`] resolves deferral + timing per the **R1/R2**
//!   precedence (Contract → Catalog SKU/Plan → PO type → billing model; same
//!   dimension → Contract wins; unresolvable → block). The v1
//!   [`DefaultDeferralPolicyResolver`] uses the timing already on the input and
//!   fills a `STRAIGHT_LINE` schedule's missing `first_period_id` from the
//!   invoice period — but the trait signature is **precedence-capable**: it
//!   takes the whole [`RecognitionContext`] (input + invoice period + the
//!   dimensions a future impl needs), so a Contract→Catalog→PO-type→billing-model
//!   implementation drops in without a signature change.
//! - [`SspResolver`] validates the SSP-snapshot ref presence for a multi-PO line
//!   (§4.4) — the per-post presence guard, not a fresh SSP pick (the value is
//!   pinned at inception, N-revrec-2).
//! - [`VcResolver`] is minimal — VC posting is OUT of the MVP (N-revrec-4); it
//!   only carries the VC refs through to the schedule.
//!
//! The defaults are **constructed with the tenant-config defaults they need**
//! (here: nothing beyond the invoice period, which arrives per-call on the
//! context) so a future config-backed impl is a drop-in. They perform **no**
//! network call and read **no** snapshot tables — those are deferred (design
//! §13 / I-6).

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::period::period_id_plus;
use crate::domain::recognition::input::{RecognitionInput, RecognitionTiming};

/// The local context the resolvers + builder derive over: the per-item spec plus
/// the invoice facts needed to fill defaults and apply the R4 exemption. Pure
/// data — no DB handle, no snapshot table (those are deferred).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecognitionContext<'a> {
    /// The per-item recognition spec (the v1 interface).
    pub input: &'a RecognitionInput,
    /// The invoice's fiscal `period_id` (`YYYYMM`) — the default first segment
    /// period for a `STRAIGHT_LINE` schedule whose `first_period_id` is `None`.
    pub invoice_period_id: &'a str,
    /// This item's ex-tax amount in minor units (the whole-amount that defers /
    /// recognizes). Must be `>= 0`.
    pub item_amount_minor_ex_tax: i64,
    /// The invoice's gross total in minor units — the denominator of the R4
    /// immaterial-one-shot exemption (`<= 1% of invoice total`).
    pub invoice_total_minor: i64,
    /// The item's ISO currency (stamped on the schedule; the R4 100-USD-equiv leg
    /// is evaluated against it — v1 treats the minor amount as USD-equivalent,
    /// see [`super::builder`]).
    pub currency: &'a str,
    /// The item's revenue stream (one schedule per stream, §4.5).
    pub revenue_stream: &'a str,
}

/// The deferral + timing a [`DeferralPolicyResolver`] resolved for an item: the
/// immutable `policy_ref` to stamp and the concrete [`RecognitionTiming`] with
/// any default (e.g. `first_period_id`) already filled. The builder turns this
/// into the schedule plan.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPolicy {
    /// The immutable deferral+timing policy version stamped on the schedule.
    pub policy_ref: String,
    /// The resolved timing — `first_period_id` filled when it was `None` on the
    /// input.
    pub timing: RecognitionTiming,
}

/// Resolves deferral + timing for one item per the R1/R2 precedence. The
/// signature is precedence-capable (it takes the whole [`RecognitionContext`]),
/// so a future Contract→Catalog→PO-type→billing-model impl needs no signature
/// change — only the body changes.
pub trait DeferralPolicyResolver: Send + Sync + 'static {
    /// Resolve the deferral+timing policy for the item in `ctx`.
    ///
    /// # Errors
    /// [`DomainError::RecognitionPolicyConflict`] when R1/R2 cannot be resolved
    /// unambiguously (a same-dimension conflict that does not reduce to "Contract
    /// wins", or no resolvable policy at all). The v1 default never conflicts (it
    /// trusts the input's timing), but the trait carries the contract for the
    /// precedence-aware impls.
    fn resolve(&self, ctx: &RecognitionContext<'_>) -> Result<ResolvedPolicy, DomainError>;
}

/// Validates the SSP-snapshot ref presence for a multi-PO line (§4.4) — the
/// per-post presence guard. The SSP **value** is pinned at inception and reused
/// (N-revrec-2); this never re-picks a fresh SSP.
pub trait SspResolver: Send + Sync + 'static {
    /// Resolve/validate the SSP snapshot ref for the item in `ctx`, returning the
    /// ref to stamp (`None` for a single-PO line that needs none).
    ///
    /// # Errors
    /// [`DomainError::SspSnapshotRequired`] when a multi-PO line's snapshot ref is
    /// missing or unresolvable.
    fn resolve(&self, ctx: &RecognitionContext<'_>) -> Result<Option<String>, DomainError>;
}

/// Carries the variable-consideration refs through to the schedule. Minimal by
/// design — VC estimate / true-up posting is OUT of the MVP (N-revrec-4); this
/// port only threads the immutable refs, it never computes an estimate.
pub trait VcResolver: Send + Sync + 'static {
    /// Resolve the `(vc_estimate_ref, vc_method_ref)` to stamp on the schedule
    /// for the item in `ctx`.
    ///
    /// # Errors
    /// [`DomainError`] — reserved for a future impl that validates VC evidence;
    /// the v1 default is infallible (it echoes the input refs).
    fn resolve(
        &self,
        ctx: &RecognitionContext<'_>,
    ) -> Result<(Option<String>, Option<String>), DomainError>;
}

/// v1 [`DeferralPolicyResolver`]: trusts the timing already on the input (R1/R2
/// were decided upstream) and only fills a `STRAIGHT_LINE` schedule's missing
/// `first_period_id` from the invoice period. No network, no snapshot table.
/// A future precedence-aware impl replaces the body; the signature is unchanged.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultDeferralPolicyResolver;

impl DeferralPolicyResolver for DefaultDeferralPolicyResolver {
    fn resolve(&self, ctx: &RecognitionContext<'_>) -> Result<ResolvedPolicy, DomainError> {
        let timing = match &ctx.input.timing {
            RecognitionTiming::PointInTime => RecognitionTiming::PointInTime,
            RecognitionTiming::StraightLine {
                periods,
                first_period_id,
            } => {
                // Default the first segment period to the invoice period when the
                // input left it unset (the common case — the upstream knows the
                // term length, the ledger knows the period it is posting into).
                let first = first_period_id
                    .clone()
                    .unwrap_or_else(|| ctx.invoice_period_id.to_owned());
                // Validate it is a usable YYYYMM up front (period_id_plus is the
                // same validator the builder uses): an unparseable period is a
                // policy-config defect, surfaced as a conflict rather than a panic
                // deeper in segment layout.
                if period_id_plus(&first, 0).is_none() {
                    return Err(DomainError::RecognitionPolicyConflict(format!(
                        "first_period_id `{first}` is not a valid YYYYMM period"
                    )));
                }
                RecognitionTiming::StraightLine {
                    periods: *periods,
                    first_period_id: Some(first),
                }
            }
        };
        Ok(ResolvedPolicy {
            policy_ref: ctx.input.policy_ref.clone(),
            timing,
        })
    }
}

/// v1 [`SspResolver`]: the per-post presence guard over the input ref. A
/// `multi_po` line with a missing/blank `ssp_snapshot_ref` blocks; otherwise the
/// (possibly `None`) ref is echoed back to stamp. No network, no snapshot table
/// (the inception-pinned value resolves locally in a later refinement).
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultSspResolver;

impl SspResolver for DefaultSspResolver {
    fn resolve(&self, ctx: &RecognitionContext<'_>) -> Result<Option<String>, DomainError> {
        if ctx.input.ssp_snapshot_missing() {
            return Err(DomainError::SspSnapshotRequired(format!(
                "multi-PO line (stream `{}`) has no resolvable SSP snapshot ref",
                ctx.revenue_stream
            )));
        }
        Ok(ctx.input.ssp_snapshot_ref.clone())
    }
}

/// v1 [`VcResolver`]: echoes the input VC refs (carry-only — VC posting is OUT of
/// the MVP, N-revrec-4). Infallible.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultVcResolver;

impl VcResolver for DefaultVcResolver {
    fn resolve(
        &self,
        ctx: &RecognitionContext<'_>,
    ) -> Result<(Option<String>, Option<String>), DomainError> {
        Ok((
            ctx.input.vc_estimate_ref.clone(),
            ctx.input.vc_method_ref.clone(),
        ))
    }
}

#[cfg(test)]
#[path = "ports_tests.rs"]
mod ports_tests;
