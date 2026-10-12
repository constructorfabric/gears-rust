//! [`ScheduleBuilder`] — the **pure** recognition-schedule derivation (design
//! §4.2). Inputs → plan, with **no DB / txn / async I/O**: it resolves the
//! policy/timing/SSP/VC via the [`ports`](super::ports), applies the R4
//! immaterial-one-shot exemption + the SSP presence gate, lays out the N
//! straight-line segments via [`crate::domain::allocate`] (residual posting increment → last,
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

use bss_ledger_sdk::money::PostedMoney;
// `builder_tests` reaches the money error vocabulary through `super::*`.
#[cfg(test)]
use crate::domain::exact_money::ExactError;
#[cfg(test)]
use bss_ledger_sdk::money::MoneyError;
use rust_decimal::Decimal;
use toolkit_macros::domain_model;

use crate::config::RecognitionConfig;
use crate::domain::allocate::{Residual, allocate};
use crate::domain::error::DomainError;
use crate::domain::exact_money::{ExactAmount, map_exact_error, map_money_error, matching_spec};
use crate::domain::period::period_id_plus;
use crate::domain::recognition::input::RecognitionTiming;
use crate::domain::recognition::ports::{
    DeferralPolicyResolver, RecognitionContext, SspResolver, VcResolver,
};

/// One planned recognition segment — a `(period_id, amount)` slice the
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
    /// Posted major-unit amount of this segment (`>= 0`; `Σ == deferred`).
    pub amount: PostedMoney,
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
    pub deferred: PostedMoney,
    /// The equal recognition segments (residual posting increment on the last), in period
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
}

impl BuiltSchedule {
    /// Check the plan invariants every persisted schedule relies on: `1..=max`
    /// segments, a non-negative deferred amount, sequential `segment_no` from 1,
    /// strictly ascending well-formed `YYYYMM` periods, non-negative segment
    /// amounts in the deferred amount's currency and scale, and segments summing
    /// exactly to `deferred`. Each failure names the item (`item_ref`), the
    /// segment and the rule it broke.
    ///
    /// # Errors
    /// [`DomainError::ScheduleTooLong`] for a segment count outside `1..=max_segments`;
    /// [`DomainError::CurrencyMismatch`] / [`DomainError::InconsistentScale`] for a segment
    /// in another currency or scale; [`DomainError::RecognitionPolicyConflict`] for every
    /// other layout defect.
    pub fn validate_plan(&self, item_ref: &str, max_segments: usize) -> Result<(), DomainError> {
        let conflict = |detail: String| {
            DomainError::RecognitionPolicyConflict(format!("item {item_ref}: {detail}"))
        };
        if self.segments.is_empty() || self.segments.len() > max_segments {
            return Err(DomainError::ScheduleTooLong(format!(
                "item {item_ref}: {} recognition segments, allowed 1..={max_segments}",
                self.segments.len()
            )));
        }
        if self.deferred.amount().is_sign_negative() {
            return Err(conflict(format!(
                "deferred amount {} is negative",
                self.deferred
            )));
        }
        let mut sum = ExactAmount::from_decimal(Decimal::ZERO);
        let mut prior: Option<&str> = None;
        for (index, segment) in self.segments.iter().enumerate() {
            let expected = i32::try_from(index)
                .ok()
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| {
                    DomainError::ScheduleTooLong(format!(
                        "item {item_ref}: segment number exhausted"
                    ))
                })?;
            let (no, period) = (segment.segment_no, segment.period_id.as_str());
            matching_spec(&self.deferred, &segment.amount)?;
            if no != expected {
                return Err(conflict(format!(
                    "segment {no} is out of sequence, expected segment {expected}"
                )));
            }
            if segment.amount.amount().is_sign_negative() {
                return Err(conflict(format!(
                    "segment {no} (period {period}) amount {} is negative",
                    segment.amount
                )));
            }
            if period.len() != 6
                || !period.bytes().all(|byte| byte.is_ascii_digit())
                || period_id_plus(period, 0).is_none()
            {
                return Err(conflict(format!(
                    "segment {no} period {period:?} is not a valid YYYYMM period"
                )));
            }
            if let Some(previous) = prior.filter(|p| *p >= period) {
                return Err(conflict(format!(
                    "segment {no} period {period} does not follow period {previous}"
                )));
            }
            prior = Some(period);
            sum = sum
                .checked_add(&ExactAmount::from_decimal(segment.amount.amount()))
                .map_err(map_exact_error)?;
        }
        if sum != ExactAmount::from_decimal(self.deferred.amount()) {
            return Err(conflict(format!(
                "segments sum to {sum} but the deferred amount is {}",
                self.deferred
            )));
        }
        Ok(())
    }
}

// NOTE: the projection of a `BuiltSchedule` into the repo insert shapes
// (`NewSchedule` / `NewSegment`) lives in the Group C `ScheduleBuilderSidecar`
// (infra), NOT here: the domain builder must not import the repo (DE0301 — no
// infra in domain). Every field the sidecar needs is `pub` on `BuiltSchedule`
// (`deferred`, `segments` with their `segment_no` / `period_id` /
// `amount`, and the stamped `policy_ref` / `ssp_snapshot_ref` /
// `po_allocation_group` / `subscription_ref` / `vc_*_ref` / `revenue_stream`), so the sidecar builds `NewSchedule` + `NewSegment` directly,
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
    /// Return the actual deferred posting, or `None` when recognized now.
    /// Callers needing zero must construct it from the validated item currency spec.
    #[must_use]
    pub fn deferred(&self) -> Option<&PostedMoney> {
        match self {
            Self::NoDeferral => None,
            Self::Schedule(schedule) => Some(&schedule.deferred),
        }
    }
}

/// The pure recognition-schedule derivation. Holds the three resolver ports +
/// the config (for the segment ceiling); [`Self::derive`] is the
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
    ///    consecutive segments (residual posting increment → last), enforce the segment
    ///    ceiling, stamp the refs.
    ///
    /// # Errors
    /// [`DomainError::SspSnapshotRequired`], [`DomainError::RecognitionPolicyConflict`],
    /// [`DomainError::ScheduleTooLong`] (segment count over
    /// `config.max_segments_per_schedule`), or [`DomainError::AmountOutOfRange`]
    /// (negative amount, zero/over-large `periods`, or an unparseable period that
    /// cannot lay out).
    pub fn derive(&self, ctx: &RecognitionContext<'_>) -> Result<ScheduleOutcome, DomainError> {
        validate_context(ctx)?;

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
            && is_immaterial(ctx.item_amount_ex_tax, ctx.invoice_total)?
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
        let deferred = ctx.item_amount_ex_tax.clone();
        let segments = self.plan_straight_line(&deferred, periods, &first_period_id)?;

        // VC refs via the port (carry-only in v1 — N-revrec-4 — so the default
        // echoes the input refs; a future impl validates VC evidence). Routing
        // through `self.vc` keeps the port live + consistent with policy / ssp.
        let (vc_estimate_ref, vc_method_ref) = self.vc.resolve(ctx)?;

        Ok(ScheduleOutcome::Schedule(BuiltSchedule {
            deferred,
            segments,
            policy_ref: resolved.policy_ref,
            ssp_snapshot_ref,
            po_allocation_group: ctx.input.po_allocation_group.clone(),
            subscription_ref: ctx.input.subscription_ref.clone(),
            vc_estimate_ref,
            vc_method_ref,
            revenue_stream: ctx.revenue_stream.to_owned(),
        }))
    }

    /// Lay out `periods` equal segments of `deferred` over consecutive
    /// fiscal periods from `first_period_id`, residual posting increment on the last
    /// (`allocate(deferred, &[1; N], Residual::Last)`). Enforces the configured
    /// ceiling.
    fn plan_straight_line(
        &self,
        deferred: &PostedMoney,
        periods: u32,
        first_period_id: &str,
    ) -> Result<Vec<PlannedSegment>, DomainError> {
        // Both guards run before constructing weights or allocation output.
        let n = segment_count(periods, self.config.max_segments_per_schedule)?;
        let weights = vec![Decimal::ONE; n];
        let amounts = allocate(deferred, &weights, Residual::Last).map_err(map_exact_error)?;
        // Last residual placement can produce a negative final share even for a
        // positive total. Preserve allocation policy and enforce existing storage admission.
        if let Some(negative) = amounts
            .iter()
            .find(|amount| amount.amount().is_sign_negative())
        {
            return Err(DomainError::AmountOutOfRange(format!(
                "deferred amount {deferred} (scale {}) cannot be laid out over {n} \
                 straight-line periods: the last-period residual would be negative ({negative})",
                deferred.currency().scale()
            )));
        }

        let mut segments = Vec::with_capacity(n);
        for (i, amount) in amounts.into_iter().enumerate() {
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
                amount,
            });
        }
        Ok(segments)
    }
}

/// Validate metadata even when amounts are zero or timing is point-in-time.
fn validate_context(ctx: &RecognitionContext<'_>) -> Result<(), DomainError> {
    matching_spec(ctx.item_amount_ex_tax, ctx.invoice_total)?;
    if ctx.item_amount_ex_tax.amount().is_sign_negative() {
        return Err(DomainError::AmountOutOfRange(
            "recognition item amount must be >= 0".to_owned(),
        ));
    }
    Ok(())
}

/// Check the configured limit and existing persisted segment-number range before allocation.
fn segment_count(periods: u32, ceiling: usize) -> Result<usize, DomainError> {
    if periods == 0 {
        return Err(DomainError::AmountOutOfRange(
            "straight-line schedule must have >= 1 period".to_owned(),
        ));
    }
    i32::try_from(periods).map_err(|_| {
        DomainError::ScheduleTooLong("segment count exceeds the segment-number range".to_owned())
    })?;
    let count = usize::try_from(periods).map_err(|_| {
        DomainError::ScheduleTooLong("segment count exceeds the platform range".to_owned())
    })?;
    if count > ceiling {
        return Err(DomainError::ScheduleTooLong(format!(
            "{count} segments exceeds the configured ceiling of {ceiling}",
        )));
    }
    Ok(count)
}

/// Existing absolute policy resolved in the compared posting's actual stored spec.
struct RecognitionMateriality {
    absolute_ceiling: PostedMoney,
}

/// Inclusive R4 predicate: item <= 10000 currency quanta AND 100 * item <= invoice total.
/// Products and comparisons remain exact even when the product exceeds posted-money bounds.
pub(crate) fn is_immaterial(
    item: &PostedMoney,
    invoice: &PostedMoney,
) -> Result<bool, DomainError> {
    matching_spec(item, invoice)?;
    let policy = RecognitionMateriality {
        absolute_ceiling: PostedMoney::try_new(
            Decimal::new(10_000, u32::from(item.currency().scale())),
            item.currency().clone(),
        )
        .map_err(map_money_error)?,
    };
    let item_exact = ExactAmount::from_decimal(item.amount());
    let above_absolute = ExactAmount::from_decimal(policy.absolute_ceiling.amount())
        .checked_sub(&item_exact)
        .map_err(map_exact_error)?
        .is_negative();
    let hundred_item = item_exact
        .checked_mul(&ExactAmount::from_decimal(Decimal::from(100)))
        .map_err(map_exact_error)?;
    let above_relative = ExactAmount::from_decimal(invoice.amount())
        .checked_sub(&hundred_item)
        .map_err(map_exact_error)?
        .is_negative();
    Ok(!above_absolute && !above_relative)
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod builder_tests;

#[cfg(test)]
#[path = "builder_plan_tests.rs"]
mod builder_plan_tests;
