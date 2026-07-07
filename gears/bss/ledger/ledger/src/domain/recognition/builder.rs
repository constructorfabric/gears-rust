//! [`ScheduleBuilder`] — the **pure** recognition-schedule derivation (design
//! §4.2). Inputs → plan, with **no DB / txn / async I/O**: it resolves the
//! policy/timing/SSP/VC via the [`ports`](super::ports), applies the R4
//! immaterial-one-shot exemption + the SSP presence gate, lays out the N
//! straight-line segments via [`crate::domain::allocate`] (residual cent → last,
//! design §4.3), enforces the configured segment ceiling, and returns a
//! [`ScheduleOutcome`]. The Group C `ScheduleBuilderSidecar` reads the plan's
//! public fields to build the `recognition_schedule` / `recognition_segment`
//! insert shapes inside the invoice-post txn; the builder itself never imports
//! the repo (DE0301 — no infra in domain).
//!
//! Outcome (per item, per revenue stream — one schedule per stream, §4.5):
//!
//! - [`ScheduleOutcome::NoDeferral`] — `deferred = 0`, no schedule. The item is a
//!   `POINT_IN_TIME` line, has no spec at all (handled by the caller before it
//!   reaches the builder — absence ⇒ no [`RecognitionContext`]), or qualifies for
//!   the **R4 immaterial-one-shot exemption** (point-in-time treatment even
//!   though a deferring timing was requested). Byte-for-byte today's Variant-A
//!   behaviour.
//! - [`ScheduleOutcome::Schedule`] — a [`BuiltSchedule`] plan: the whole ex-tax
//!   amount deferred to `CONTRACT_LIABILITY`, split into `segments` equal
//!   slices, with the immutable `policy_ref` / `ssp_snapshot_ref` /
//!   `po_allocation_group` / VC refs stamped.
//! - `Err(DomainError)` — a block: [`DomainError::SspSnapshotRequired`] (multi-PO
//!   without a resolvable SSP snapshot), [`DomainError::RecognitionPolicyConflict`]
//!   (R1/R2 ambiguity), [`DomainError::ScheduleTooLong`] (segment count over the
//!   configured ceiling), or [`DomainError::AmountOutOfRange`] (a malformed
//!   amount / period that cannot lay out).

use toolkit_macros::domain_model;

use crate::config::RecognitionConfig;
use crate::domain::allocate::{Residual, allocate};
use crate::domain::error::DomainError;
use crate::domain::period::period_id_plus;
use crate::domain::recognition::input::RecognitionTiming;
use crate::domain::recognition::ports::{
    DeferralPolicyResolver, RecognitionContext, SspResolver, VcResolver,
};

/// One planned recognition segment — a `(period_id, amount_minor)` slice the
/// sidecar will materialize as a `recognition_segment` row. `segment_no` is the
/// 1-based position (1:1 with `period_id`, the storage invariant); the sidecar
/// stamps it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedSegment {
    /// 1-based segment number (immutable, 1:1 with `period_id`).
    pub segment_no: i32,
    /// Fiscal `period_id` (`YYYYMM`) this segment recognizes into.
    pub period_id: String,
    /// Minor-unit amount of this segment (`>= 0`; `Σ == deferred_minor`).
    pub amount_minor: i64,
}

/// The derived schedule plan for one deferred item-stream: the whole ex-tax
/// amount deferred, the equal segments, and the immutable refs to stamp. Pure
/// data — the Group C sidecar reads these public fields to build the
/// `recognition_schedule` + `recognition_segment` insert rows (the builder does
/// not import the repo; see the note below the struct).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// The `*_ref` / `*_group` fields mirror the `recognition_schedule` columns.
#[allow(clippy::struct_field_names)]
pub struct BuiltSchedule {
    /// The whole ex-tax amount deferred to `CONTRACT_LIABILITY` (`= Σ segment
    /// amounts`).
    pub deferred_minor: i64,
    /// The equal recognition segments (residual cent on the last), in period
    /// order.
    pub segments: Vec<PlannedSegment>,
    /// The immutable deferral+timing policy version (stamped).
    pub policy_ref: String,
    /// The SSP snapshot ref to stamp (`None` for a single-PO line).
    pub ssp_snapshot_ref: Option<String>,
    /// The PO / allocation group to stamp.
    pub po_allocation_group: Option<String>,
    /// The subscription/entitlement ref to stamp.
    pub subscription_ref: Option<String>,
    /// VC estimate ref (carry-only, N-revrec-4).
    pub vc_estimate_ref: Option<String>,
    /// VC method ref (carry-only, N-revrec-4).
    pub vc_method_ref: Option<String>,
    /// The item's revenue stream (one schedule per stream).
    pub revenue_stream: String,
    /// The item's ISO currency (stamped on the schedule).
    pub currency: String,
}

// NOTE: the projection of a `BuiltSchedule` into the repo insert shapes
// (`NewSchedule` / `NewSegment`) lives in the Group C `ScheduleBuilderSidecar`
// (infra), NOT here: the domain builder must not import the repo (DE0301 — no
// infra in domain). Every field the sidecar needs is `pub` on `BuiltSchedule`
// (`deferred_minor`, `segments` with their `segment_no` / `period_id` /
// `amount_minor`, and the stamped `policy_ref` / `ssp_snapshot_ref` /
// `po_allocation_group` / `subscription_ref` / `vc_*_ref` / `revenue_stream` /
// `currency`), so the sidecar builds `NewSchedule` + `NewSegment` directly,
// minting the `schedule_id` and supplying the posting-context identity
// (`source_invoice_id` / `source_invoice_item_ref` / `payer_tenant_id` /
// `tenant_id`) it already holds.

/// The result of deriving recognition for one item-stream: either no deferral
/// (recognized now) or a [`BuiltSchedule`] plan.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// `Schedule` dwarfs the unit `NoDeferral`, but the outcome is a transient
// per-item return consumed immediately (never collected in bulk), so boxing
// would add an allocation for no meaningful saving.
#[allow(clippy::large_enum_variant)]
pub enum ScheduleOutcome {
    /// `deferred = 0` — recognized now, no schedule (`POINT_IN_TIME` or the R4
    /// exemption applied).
    NoDeferral,
    /// A materializable schedule plan.
    Schedule(BuiltSchedule),
}

impl ScheduleOutcome {
    /// The amount deferred to `CONTRACT_LIABILITY` by this outcome: `0` for
    /// [`Self::NoDeferral`], the schedule's `deferred_minor` otherwise. The
    /// Group C builder split feeds this into the `CR CONTRACT_LIABILITY` /
    /// `CR REVENUE` line amounts (`CR REVENUE = amount − deferred`).
    #[must_use]
    pub fn deferred_minor(&self) -> i64 {
        match self {
            Self::NoDeferral => 0,
            Self::Schedule(s) => s.deferred_minor,
        }
    }
}

/// The pure recognition-schedule derivation. Holds the three resolver ports +
/// the config (for the segment ceiling + R4 thresholds); [`Self::derive`] is the
/// single entry point.
#[domain_model]
pub struct ScheduleBuilder<'r, P, S, V>
where
    P: DeferralPolicyResolver,
    S: SspResolver,
    V: VcResolver,
{
    policy: &'r P,
    ssp: &'r S,
    vc: &'r V,
    config: &'r RecognitionConfig,
}

impl<'r, P, S, V> ScheduleBuilder<'r, P, S, V>
where
    P: DeferralPolicyResolver,
    S: SspResolver,
    V: VcResolver,
{
    /// Build a derivation over the three resolvers + the recognition config.
    #[must_use]
    pub fn new(policy: &'r P, ssp: &'r S, vc: &'r V, config: &'r RecognitionConfig) -> Self {
        Self {
            policy,
            ssp,
            vc,
            config,
        }
    }

    /// Derive the recognition outcome for one item-stream from `ctx`. **Pure** —
    /// no DB / txn / async. Order of operations (each a documented gate):
    ///
    /// 1. **SSP presence gate** (§4.4): a `multi_po` line whose SSP snapshot ref
    ///    is missing/unresolvable ⇒ [`DomainError::SspSnapshotRequired`]. Checked
    ///    first so a multi-PO config gap blocks regardless of timing.
    /// 2. **Policy resolution** (R1/R2, [`DeferralPolicyResolver`]): yields the
    ///    immutable `policy_ref` + concrete timing (with `first_period_id`
    ///    defaulted from the invoice period). Ambiguity ⇒
    ///    [`DomainError::RecognitionPolicyConflict`].
    /// 3. **`POINT_IN_TIME`** ⇒ [`ScheduleOutcome::NoDeferral`] (recognized now).
    /// 4. **R4 immaterial-one-shot exemption**: a deferring timing that is
    ///    SKU-flagged AND under the materiality threshold is treated as
    ///    point-in-time ⇒ [`ScheduleOutcome::NoDeferral`] (design §1.4 R4 / §13).
    /// 5. **`STRAIGHT_LINE`**: defer the whole ex-tax amount, lay out `periods`
    ///    consecutive segments (residual cent → last), enforce the segment
    ///    ceiling, stamp the refs.
    ///
    /// # Errors
    /// [`DomainError::SspSnapshotRequired`], [`DomainError::RecognitionPolicyConflict`],
    /// [`DomainError::ScheduleTooLong`] (segment count over
    /// `config.max_segments_per_schedule`), or [`DomainError::AmountOutOfRange`]
    /// (negative amount, zero/over-large `periods`, or an unparseable period that
    /// cannot lay out).
    pub fn derive(&self, ctx: &RecognitionContext<'_>) -> Result<ScheduleOutcome, DomainError> {
        if ctx.item_amount_minor_ex_tax < 0 {
            return Err(DomainError::AmountOutOfRange(format!(
                "recognition item amount must be >= 0, got {}",
                ctx.item_amount_minor_ex_tax
            )));
        }

        // 1. SSP presence gate (§4.4) — before policy so a multi-PO config gap
        //    blocks even a malformed/point-in-time timing.
        let ssp_snapshot_ref = self.ssp.resolve(ctx)?;

        // 2. Resolve deferral + timing (R1/R2 precedence).
        let resolved = self.policy.resolve(ctx)?;

        // 3. POINT_IN_TIME ⇒ no schedule.
        let RecognitionTiming::StraightLine {
            periods,
            first_period_id,
        } = resolved.timing
        else {
            return Ok(ScheduleOutcome::NoDeferral);
        };

        // 4. R4 immaterial-one-shot exemption: a deferring timing that is
        //    SKU-flagged AND immaterial recognizes now (point-in-time treatment),
        //    no schedule. (The exemption is specifically for point-in-time
        //    *eligible* one-shots; we apply it to a would-be-deferred line that
        //    carries the flag and clears the threshold.)
        if ctx.input.immaterial_one_shot_sku
            && is_immaterial(
                ctx.item_amount_minor_ex_tax,
                ctx.invoice_total_minor,
                self.config,
            )
        {
            return Ok(ScheduleOutcome::NoDeferral);
        }

        // 5. STRAIGHT_LINE: defer the whole ex-tax amount across `periods`
        //    consecutive segments. The resolver is contracted to fill
        //    `first_period_id` (the default resolver defaults it from the invoice
        //    period); a `None` that slips through is a resolver defect, surfaced
        //    as a policy conflict rather than a panic.
        let first_period_id = first_period_id.ok_or_else(|| {
            DomainError::RecognitionPolicyConflict(
                "straight-line timing resolved without a first_period_id".to_owned(),
            )
        })?;
        let deferred_minor = ctx.item_amount_minor_ex_tax;
        let segments = self.plan_straight_line(deferred_minor, periods, &first_period_id)?;

        // VC refs via the port (carry-only in v1 — N-revrec-4 — so the default
        // echoes the input refs; a future impl validates VC evidence). Routing
        // through `self.vc` keeps the port live + consistent with policy / ssp.
        let (vc_estimate_ref, vc_method_ref) = self.vc.resolve(ctx)?;

        Ok(ScheduleOutcome::Schedule(BuiltSchedule {
            deferred_minor,
            segments,
            policy_ref: resolved.policy_ref,
            ssp_snapshot_ref,
            po_allocation_group: ctx.input.po_allocation_group.clone(),
            subscription_ref: ctx.input.subscription_ref.clone(),
            vc_estimate_ref,
            vc_method_ref,
            revenue_stream: ctx.revenue_stream.to_owned(),
            currency: ctx.currency.to_owned(),
        }))
    }

    /// Lay out `periods` equal segments of `deferred_minor` over consecutive
    /// fiscal periods from `first_period_id`, residual cent on the last
    /// (`allocate(deferred, &[1; N], Residual::Last)`). Enforces the configured
    /// ceiling.
    fn plan_straight_line(
        &self,
        deferred_minor: i64,
        periods: u32,
        first_period_id: &str,
    ) -> Result<Vec<PlannedSegment>, DomainError> {
        if periods == 0 {
            return Err(DomainError::AmountOutOfRange(
                "straight-line schedule must have >= 1 period".to_owned(),
            ));
        }
        // Segment-count guard (design §4.2 / §3.7, decision 3): block over the
        // configured ceiling — no degrade in v1. Compared as usize against the
        // config ceiling.
        let n = periods as usize;
        if n > self.config.max_segments_per_schedule {
            return Err(DomainError::ScheduleTooLong(format!(
                "{n} segments exceeds the configured ceiling of {}",
                self.config.max_segments_per_schedule
            )));
        }

        // Equal weights ⇒ even split with the residual cent on the last segment
        // (the schedule-version's pinned residual rule, §4.3). `allocate` errors
        // only on empty/negative/zero-sum weights — none possible here (n >= 1,
        // unit weights) — so map any surprise to AmountOutOfRange rather than
        // unwrapping.
        let weights = vec![1_i64; n];
        let amounts = allocate(deferred_minor, &weights, Residual::Last)
            .map_err(|e| DomainError::AmountOutOfRange(format!("segment allocation: {e}")))?;

        let mut segments = Vec::with_capacity(n);
        for (i, amount_minor) in amounts.into_iter().enumerate() {
            // `i` fits the segment count (<= ceiling, default 120), well within
            // u32/i32; periods are consecutive from the first.
            let offset = u32::try_from(i)
                .map_err(|_| DomainError::AmountOutOfRange("segment index overflow".to_owned()))?;
            let period_id = period_id_plus(first_period_id, offset).ok_or_else(|| {
                DomainError::AmountOutOfRange(format!(
                    "cannot advance period `{first_period_id}` by {offset} months"
                ))
            })?;
            let segment_no = i32::try_from(i + 1)
                .map_err(|_| DomainError::AmountOutOfRange("segment number overflow".to_owned()))?;
            segments.push(PlannedSegment {
                segment_no,
                period_id,
                amount_minor,
            });
        }
        Ok(segments)
    }
}

/// `true` iff `amount_minor` clears the R4 materiality threshold: the **lower**
/// of 1% of the invoice total or the 100-USD-equiv floor (design §1.4 R4 / §13).
/// v1 treats `amount_minor` as USD-equivalent minor units against a fixed
/// 100-USD floor (`100 * 100 = 10_000` minor) — FX normalization is deferred
/// (Slice 5); the threshold is otherwise tenant-configurable via
/// [`RecognitionConfig`] in a later refinement (today the floor is the ratified
/// constant). The 1% leg uses `i128` to avoid an intermediate overflow.
pub(crate) fn is_immaterial(
    amount_minor: i64,
    invoice_total_minor: i64,
    _config: &RecognitionConfig,
) -> bool {
    // 100 USD-equiv in minor units (cents). v1 fixed floor; FX-normalized in a
    // later slice. Declared first (clippy::items_after_statements).
    const USD_FLOOR_MINOR: i128 = 100 * 100;
    // 1% of the invoice total (floored, i128 to dodge overflow on the multiply).
    let one_percent = (i128::from(invoice_total_minor)) / 100;
    // The exemption ceiling is the *lower* of the two (design §1.4 R4).
    let threshold = one_percent.min(USD_FLOOR_MINOR);
    i128::from(amount_minor) <= threshold
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod builder_tests;
