//! Generation 1's frozen output types (rating DESIGN 01 §3.1, §4.3, §4.4).
//!
//! An [`EvaluationOutcome`] carries the rated lines of one child, the true-up obligations (always
//! none: they are deferred with price adjustments) and, per price and stored scale, the exact
//! totals roll-up floors. Every amount is a reduced fraction in major units as canonical text
//! ([`MoneyText`]); the outcome carries no digest, because the pipeline hashes the input.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::exact::ExactQuantity;
use crate::input::{ChargeKind, CoreInvoiceInputs, MeterRef, PricePin};

/// An exact amount as it leaves the core: a reduced fraction in major units with the currency and
/// the posting scale stored with its price (Rating's `ExactMoney` wire form, money ADR rule 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneyText {
    /// Canonical integer text; the sign of the amount is here.
    pub numerator: String,
    /// Canonical positive integer text.
    pub denominator: String,
    pub currency: String,
    pub currency_scale: u8,
}

/// The result of one evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationOutcome {
    pub lines: Vec<RatedLine>,
    /// True-up obligations; none exist while price adjustments are deferred.
    pub obligations: Vec<Obligation>,
    /// One entry per price and stored scale the child rated, for the minimum-fee floor at
    /// roll-up (T-D-78).
    pub price_totals: Vec<PriceTotal>,
}

/// A true-up obligation. Price adjustments are deferred (T-D-81), so generation 1 emits none and
/// the type has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Obligation {}

/// What one price rated in the child, per stored scale: roll-up sums these over the fact's
/// children and floors the price at `minimum_fee × covered_seconds / period_seconds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceTotal {
    pub price_id: Uuid,
    pub currency_scale: u8,
    pub rated: MoneyText,
    pub covered_seconds: u64,
    pub period_seconds: u64,
    pub minimum_fee: Option<MoneyText>,
}

/// How a line's amount was formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Flat,
    PerUnit,
    Volume,
    Graduated,
    Package,
}

/// One rated line: one slice of the child, priced by one binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatedLine {
    /// `{fact_id}|{meter or item}|{dimension_key}={dimension_value}|{kind}|{gl_code}/{tax_category}|{currency_scale}`
    /// (T-D-58): no hour, revision or price id, so revisions compare line by line.
    pub line_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub slice_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub slice_end: OffsetDateTime,
    pub item_id: Uuid,
    pub dimension_key: Option<String>,
    pub dimension_value: Option<String>,
    pub sku_id: Uuid,
    pub sku_version: i64,
    pub plan_id: Uuid,
    pub revision_id: Uuid,
    pub price_id: Uuid,
    pub price_book_entry_id: Uuid,
    pub charge_kind: ChargeKind,
    pub model_kind: ModelKind,
    /// The slice quantity the model priced; none for a flat price.
    pub quantity: Option<ExactQuantity>,
    /// The quantity billed after declared business rounding (the package block count, F02b).
    pub billable_quantity: Option<ExactQuantity>,
    /// Where the slice starts on the price's bands; none for an untiered price.
    pub band_offset: Option<ExactQuantity>,
    pub exact_amount: MoneyText,
    pub currency: String,
    pub currency_scale: u8,
    pub unit: Option<String>,
    pub gl_code: String,
    pub tax_category: String,
    pub invoice_line_template: String,
    /// ASC 606 performance obligation: null, no gear supplies it (DESIGN 10 §4.4).
    pub performance_obligation_ref: Option<String>,
    /// ASC 606 SSP snapshot pointer: null, Pricing has no SSP (DESIGN 10 §4.4).
    pub ssp_snapshot_pointer: Option<String>,
    pub snapshot: SnapshotBody,
    pub lineage: Lineage,
}

/// What the line priced with (DESIGN 01 §4.3); the pipeline derives
/// `snapshot_id = "rsnap1:" + hex(sha256(canonical body))`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotBody {
    pub plan_id: Uuid,
    pub revision_id: Uuid,
    #[serde(with = "crate::input::iso_date")]
    pub resolve_date: Date,
    pub bindings: Vec<SnapshotBinding>,
    /// The child's binding of record.
    pub binding_digest: String,
    pub fact_id: Uuid,
    pub fact_version: u64,
    /// The accepted pin the period-start binding was checked against; none for an `ends_on`
    /// successor, which is never compared (R-29, V53).
    pub accepted_binding: Option<PricePin>,
    pub engine_generation: u32,
    pub engine_digest: String,
}

/// One binding a line used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotBinding {
    pub item_id: Uuid,
    pub dimension_value: Option<String>,
    pub price_id: Uuid,
    pub price_book_entry_id: Uuid,
    pub money_digest: String,
    pub sku_id: Uuid,
    pub sku_version: i64,
    pub usage_rating_policy: Option<PolicyRef>,
    pub invoice_inputs: CoreInvoiceInputs,
    pub meter: Option<MeterRef>,
    pub derived_declaration_digest: Option<String>,
}

/// A usage rating policy by identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRef {
    pub policy_id: Uuid,
    pub version: u64,
    pub digest: String,
}

/// The evidence of how a line's amount was formed (DESIGN 01 §4.4, guard 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lineage {
    /// The binding the line used.
    pub price_id: Uuid,
    pub money_digest: String,
    /// The resolve the binding came from.
    #[serde(with = "crate::input::iso_date")]
    pub resolve_date: Date,
    /// The cell was bound through the default chain (pricing D-420).
    pub via_default: bool,
    /// The amount before and after each step that changed or checked it, in step order.
    pub steps: Vec<StepRecord>,
    /// The covered seconds and period seconds of a prorated line.
    pub proration: Option<Proration>,
    /// The pre-clamp amount when the non-negative guard clamped the line to zero; an intermediate,
    /// bounded by the arithmetic budget, not the delivery bound.
    pub clamped_from: Option<MoneyText>,
}

/// A step of the fixed order (PRD §17.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Bind,
    Meter,
    Model,
    Prorate,
    NonNegative,
    Precision,
}

/// The amount around one step. These are intermediates: bounded by the generation's arithmetic
/// budget, not by the delivery bound only the line's `exact_amount` carries (DESIGN §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepRecord {
    pub step: Step,
    pub pre: Option<MoneyText>,
    pub post: Option<MoneyText>,
}

/// The covered fraction a period line was prorated by (T-D-77).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proration {
    pub covered_seconds: u64,
    pub period_seconds: u64,
}
