//! [`RecognitionInput`] — the per-invoice-item recognition spec (the approved v1
//! interface). A **domain** value type, not a REST DTO: the invoice-post request
//! carries an optional spec per item, the glue maps it into this shape, and the
//! [`ScheduleBuilder`](super::builder::ScheduleBuilder) derives the schedule plan
//! from it. An item with **no** [`RecognitionInput`] is recognized now
//! (`deferred = 0`, today's Variant-A behaviour) — absence is the default, not an
//! error.
//!
//! v1 semantics (pinned): an item is **all** point-in-time **or** **all**
//! straight-line. There is no partial-defer-per-item and no milestone timing;
//! the whole ex-tax amount either recognizes now or defers in full to
//! `CONTRACT_LIABILITY` over N equal segments. Partial defer + milestone timing
//! are a later refinement.

use toolkit_macros::domain_model;

/// Recognition timing for one invoice item — **all** point-in-time or **all**
/// straight-line (the v1 pin). The `POINT_IN_TIME` / `STRAIGHT_LINE` wire forms
/// are the two timing patterns R2 may resolve to.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecognitionTiming {
    /// No deferral — the whole ex-tax amount recognizes at invoice
    /// (`deferred = 0`). Equivalent to the absence of a spec, but explicit (a
    /// policy may resolve `POINT_IN_TIME` deliberately, e.g. a one-time setup fee).
    PointInTime,
    /// Straight-line deferral — the whole ex-tax amount defers to
    /// `CONTRACT_LIABILITY` and recognizes in `periods` equal segments
    /// (residual cent on the last), the first segment landing in
    /// `first_period_id`. `periods` must be `>= 1`; `first_period_id` is a
    /// `YYYYMM` fiscal period. When `first_period_id` is `None` the
    /// [`DeferralPolicyResolver`](super::ports::DeferralPolicyResolver) fills it
    /// from the invoice period (the default resolver does so).
    StraightLine {
        /// Number of equal recognition segments (`>= 1`).
        periods: u32,
        /// First fiscal period (`YYYYMM`) the schedule recognizes into; `None` ⇒
        /// the resolver defaults it to the invoice period.
        first_period_id: Option<String>,
    },
}

impl RecognitionTiming {
    /// `true` iff this timing defers any amount (i.e. is straight-line). A
    /// `POINT_IN_TIME` item never produces a schedule.
    #[must_use]
    pub const fn is_deferred(&self) -> bool {
        matches!(self, Self::StraightLine { .. })
    }
}

/// The optional per-item recognition spec (the v1 interface). `policy_ref` is
/// stamped immutably on the materialized schedule (historical immutability,
/// design §4.2); `timing` decides `POINT_IN_TIME` vs `STRAIGHT_LINE`. The optional
/// refs/flags carry PO/SSP/VC and subscription context through to the schedule.
///
/// "Multi-PO" — the condition that makes a missing SSP snapshot a block (§4.4) —
/// is modeled by the explicit [`Self::multi_po`] flag rather than inferred from
/// `po_allocation_group`: an ordinary point-in-time line carries a (default)
/// allocation group but is **not** multi-PO, so inferring multi-PO from the
/// group's presence would over-block routine billing. The upstream caller sets
/// `multi_po = true` only for a genuine multi-performance-obligation line; v1
/// keeps the condition this simple and documented (the SSP gate then fires only
/// when `multi_po && ssp_snapshot_ref` is absent).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// The `*_ref` / `*_group` fields mirror the `recognition_schedule` column names
// verbatim (the storage contract); renaming to satisfy `struct_field_names`
// would diverge from `NewSchedule`.
#[allow(clippy::struct_field_names)]
pub struct RecognitionInput {
    /// The deferral+timing policy version stamped immutably on the schedule
    /// (design §4.2). Resolved/validated by the
    /// [`DeferralPolicyResolver`](super::ports::DeferralPolicyResolver).
    pub policy_ref: String,
    /// The resolved recognition timing (`POINT_IN_TIME` or `STRAIGHT_LINE`).
    pub timing: RecognitionTiming,
    /// The PO / allocation group this line books under (audit, §4.7). A Catalog
    /// **default** group auto-tags ordinary lines; `None` is allowed for a
    /// non-multi-PO point-in-time line.
    pub po_allocation_group: Option<String>,
    /// `true` for a genuine multi-performance-obligation line — the only case
    /// where a missing/unresolvable SSP snapshot blocks the post (§4.4). A
    /// single-PO line leaves this `false`.
    pub multi_po: bool,
    /// The SSP snapshot ref pinned at contract inception and reused per invoice
    /// (R3 / N-revrec-2). Required to be present + resolvable for a `multi_po`
    /// line; `None`/empty on such a line ⇒ `SspSnapshotRequired`.
    pub ssp_snapshot_ref: Option<String>,
    /// The subscription/entitlement this obligation belongs to, threaded onto the
    /// schedule for audit (§4.7); `None` when not subscription-scoped.
    pub subscription_ref: Option<String>,
    /// Variable-consideration estimate ref (carried only — VC posting is OUT of
    /// the MVP, N-revrec-4).
    pub vc_estimate_ref: Option<String>,
    /// Variable-consideration method ref (carried only — VC posting is OUT of
    /// the MVP, N-revrec-4).
    pub vc_method_ref: Option<String>,
    /// `true` iff the Catalog SKU is flagged immaterial-one-shot-eligible — a
    /// precondition of the R4 exemption (point-in-time, under the threshold, AND
    /// SKU-flagged). v1 carries the flag on the input; absence ⇒ not eligible.
    pub immaterial_one_shot_sku: bool,
}

impl RecognitionInput {
    /// `true` iff a missing/unresolvable SSP snapshot must block this line: a
    /// `multi_po` line whose `ssp_snapshot_ref` is absent or blank (§4.4). A
    /// single-PO line never trips this (documented pricing suffices, R3).
    #[must_use]
    pub fn ssp_snapshot_missing(&self) -> bool {
        self.multi_po && self.ssp_snapshot_ref.as_deref().is_none_or(str::is_empty)
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;
