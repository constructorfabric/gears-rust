//! Preview: the nonbinding Pricing assessment of a basket (F03 §2.2/§4.6; `models.json`
//! `PreviewBasket` and `PreviewResult`).
//!
//! The request types refuse unknown fields and validate like the draft line they mirror
//! ([`PreviewLine`] converts into an [`AddLine`]; the rules are not duplicated). The result
//! types are stored/forward-compatible (unknown members and unknown enum kinds tolerated), `#[non_exhaustive]`, and built
//! by the gear through the constructors; consumers decode them. Every money value is text copied
//! verbatim from the producer: nothing here adds, rounds or converts. There is no approval
//! verdict, deadline or quote validity on the result, by design.
//! Subscription-side cardinality is advisory at order time: a gate or activation re-check
//! pass is not an admission guarantee. Subscriptions must enforce it atomically at activation;
//! submit/activation is not production-ready until that upstream contract is delivered (D-180).
use crate::authoring::{
    AddLine, AuthoredTerm, AuthoringInvalid, BillingCycle, CalendarDate, Currency, SelectedItem,
};
use crate::gate_report::GateReport;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Maximum lines in one Preview basket (`models.json` `limits.lines`).
pub const MAX_PREVIEW_LINES: usize = 200;

/// The basket to assess (`models.json` `PreviewBasket`). Nothing is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewBasket {
    /// The tenant that will own the resources.
    pub resource_tenant_id: Uuid,
    /// The paying tenant.
    pub payer_tenant_id: Uuid,
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
    /// The contract the basket would be sold under, when known.
    #[serde(default)]
    pub contract_id: Option<Uuid>,
    /// The lines, at most [`MAX_PREVIEW_LINES`].
    pub lines: Vec<PreviewLine>,
}

impl PreviewBasket {
    /// Boundary validation: the line limit, then every line's [`AddLine`] rules.
    ///
    /// # Errors
    /// `request-invalid` input: `lines` when empty (as submit's `no-lines`) or over the limit,
    /// else the first line's failure.
    pub fn validate(&self) -> Result<(), AuthoringInvalid> {
        if self.lines.is_empty() || self.lines.len() > MAX_PREVIEW_LINES {
            return Err(AuthoringInvalid("lines"));
        }
        self.lines.iter().try_for_each(PreviewLine::validate)
    }
}

/// One line to assess: the shape of an [`AddLine`], with term and cycle optional so a basket
/// can be previewed before they are chosen (their absence withholds the TCV, D-125).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewLine {
    /// The plan.
    pub plan_id: Uuid,
    /// The plan revision.
    pub plan_revision_id: Uuid,
    /// The selected items.
    pub selected_items: Vec<SelectedItem>,
    /// The currency.
    pub currency: Currency,
    /// The contract effective date, when authored.
    #[serde(default)]
    pub contract_effective_date: Option<CalendarDate>,
    /// The service activation date, when authored.
    #[serde(default)]
    pub service_activation_date: Option<CalendarDate>,
    /// The acceptance due date, when authored.
    #[serde(default)]
    pub acceptance_due_date: Option<CalendarDate>,
    /// The term, when authored.
    #[serde(default)]
    pub term_duration: Option<AuthoredTerm>,
    /// The billing cycle, when authored.
    #[serde(default)]
    pub billing_cycle: Option<BillingCycle>,
}

impl From<PreviewLine> for AddLine {
    fn from(line: PreviewLine) -> Self {
        Self {
            plan_id: line.plan_id,
            plan_revision_id: line.plan_revision_id,
            selected_items: line.selected_items,
            currency: line.currency,
            contract_effective_date: line.contract_effective_date,
            service_activation_date: line.service_activation_date,
            acceptance_due_date: line.acceptance_due_date,
            term_duration: line.term_duration,
            billing_cycle: line.billing_cycle,
        }
    }
}

impl PreviewLine {
    /// Validate exactly as the equivalent [`AddLine`].
    ///
    /// # Errors
    /// `request-invalid` input.
    pub fn validate(&self) -> Result<(), AuthoringInvalid> {
        crate::authoring::validate_line_parts(
            &self.selected_items,
            self.term_duration.as_ref(),
            [
                (
                    self.contract_effective_date.as_ref(),
                    "contract_effective_date",
                ),
                (
                    self.service_activation_date.as_ref(),
                    "service_activation_date",
                ),
                (self.acceptance_due_date.as_ref(), "acceptance_due_date"),
            ],
        )
    }
}

/// The assessment (`models.json` `PreviewResult`). `tcv` is absent and `tcv_withheld` present
/// exactly when a line lacks term or cycle (D-125); the wire name of the latter is the camelCase
/// `tcvWithheld`, everything else is `snake_case`.
///
/// Decoding refuses a body whose `assessment_id` differs from `assessment.assessment_id`: the
/// two are one identity, so a disagreement is a corrupt body, not a choice (D-229: consumers
/// read the fields and never build the value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PreviewResultWire")]
#[non_exhaustive]
pub struct PreviewResult {
    /// The stored assessment identity; always equal to `assessment.assessment_id`.
    pub assessment_id: Uuid,
    /// The gate assessment of the basket.
    pub assessment: GateReport,
    /// One result per basket line.
    pub lines: Vec<PreviewLineResult>,
    /// The resolved total, when every line priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_total: Option<ResolvedTotalView>,
    /// The indicative tax, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicative_tax: Option<IndicativeTaxView>,
    /// The expected fulfillment time, when known.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub expected_fulfillment_time: Option<OffsetDateTime>,
    /// The total contract value, verbatim; absent when withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcv: Option<MoneyView>,
    /// Producer-declared valuation horizon; absent when TCV is unavailable or withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcv_basis: Option<String>,
    /// Why and for which lines the TCV is withheld.
    #[serde(
        default,
        rename = "tcvWithheld",
        skip_serializing_if = "Option::is_none"
    )]
    pub tcv_withheld: Option<TcvWithheld>,
}

impl PreviewResult {
    /// A result with the required members; set the optional ones on the value. The
    /// `assessment_id` is taken from `assessment`, so the two cannot differ.
    #[must_use]
    pub fn new(assessment: GateReport, lines: Vec<PreviewLineResult>) -> Self {
        Self {
            assessment_id: assessment.assessment_id,
            assessment,
            lines,
            resolved_total: None,
            indicative_tax: None,
            expected_fulfillment_time: None,
            tcv: None,
            tcv_basis: None,
            tcv_withheld: None,
        }
    }
}

/// The decode shape of [`PreviewResult`]: the same members and serde attributes, checked by
/// `TryFrom` before the value exists. Unknown members stay tolerated (a result, not a request).
#[derive(Deserialize)]
struct PreviewResultWire {
    /// The stored assessment identity; always equal to `assessment.assessment_id`.
    pub assessment_id: Uuid,
    /// The gate assessment of the basket.
    pub assessment: GateReport,
    /// One result per basket line.
    pub lines: Vec<PreviewLineResult>,
    /// The resolved total, when every line priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_total: Option<ResolvedTotalView>,
    /// The indicative tax, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicative_tax: Option<IndicativeTaxView>,
    /// The expected fulfillment time, when known.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub expected_fulfillment_time: Option<OffsetDateTime>,
    /// The total contract value, verbatim; absent when withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcv: Option<MoneyView>,
    /// Producer-declared valuation horizon; absent when TCV is unavailable or withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcv_basis: Option<String>,
    /// Why and for which lines the TCV is withheld.
    #[serde(
        default,
        rename = "tcvWithheld",
        skip_serializing_if = "Option::is_none"
    )]
    pub tcv_withheld: Option<TcvWithheld>,
}

impl TryFrom<PreviewResultWire> for PreviewResult {
    type Error = &'static str;

    fn try_from(wire: PreviewResultWire) -> Result<Self, Self::Error> {
        if wire.assessment_id != wire.assessment.assessment_id {
            return Err("preview result assessment_id differs from its assessment");
        }
        Ok(Self {
            assessment_id: wire.assessment_id,
            assessment: wire.assessment,
            lines: wire.lines,
            resolved_total: wire.resolved_total,
            indicative_tax: wire.indicative_tax,
            expected_fulfillment_time: wire.expected_fulfillment_time,
            tcv: wire.tcv,
            tcv_basis: wire.tcv_basis,
            tcv_withheld: wire.tcv_withheld,
        })
    }
}

/// The pin outcome of one previewed line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PinOutcomeView {
    /// The line composed.
    Composed,
    /// The plan revision cannot be resolved.
    Unresolvable {
        /// The public reason token.
        reason: String,
    },
    /// The line cannot be evaluated now.
    Unevaluable {
        /// The public reason token.
        reason: String,
    },
    /// A kind this SDK version does not know; treat as not composed / unavailable.
    #[serde(other)]
    Unknown,
}

/// The result for one basket line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PreviewLineResult {
    /// Run-local line identity (nothing is stored under it).
    pub line_id: Uuid,
    /// The pin outcome.
    pub pin: PinOutcomeView,
    /// The per-line service activation date when it differs from the authored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferral: Option<CalendarDate>,
}

impl PreviewLineResult {
    /// A line result.
    #[must_use]
    pub fn new(line_id: Uuid, pin: PinOutcomeView, deferral: Option<CalendarDate>) -> Self {
        Self {
            line_id,
            pin,
            deferral,
        }
    }
}

/// Why the TCV is withheld and which lines lack term or cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TcvWithheld {
    /// The reason token, `preview-term-or-cycle-missing`.
    pub reason: String,
    /// The lines lacking term or cycle.
    #[serde(rename = "lineIds")]
    pub line_ids: Vec<Uuid>,
}

impl TcvWithheld {
    /// A withheld marker.
    #[must_use]
    pub fn new(reason: impl Into<String>, line_ids: Vec<Uuid>) -> Self {
        Self {
            reason: reason.into(),
            line_ids,
        }
    }
}

/// An exact fraction of a currency's major unit, as the gear's `ExactMoney` carries it:
/// `numerator` is canonical integer text with an optional leading `-`, `denominator` canonical
/// positive integer text, both copied verbatim. The SDK performs no arithmetic and no
/// validation of its own: the producing gear validates before building a view (D-216).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExactMoneyView {
    /// Canonical integer text, optional leading `-`.
    pub numerator: String,
    /// Canonical positive integer text.
    pub denominator: String,
    /// The currency code.
    pub currency: String,
    /// The currency's minor-unit scale.
    pub currency_scale: u8,
}

impl ExactMoneyView {
    /// A view of the four verbatim parts.
    #[must_use]
    pub fn new(
        numerator: impl Into<String>,
        denominator: impl Into<String>,
        currency: impl Into<String>,
        currency_scale: u8,
    ) -> Self {
        Self {
            numerator: numerator.into(),
            denominator: denominator.into(),
            currency: currency.into(),
            currency_scale,
        }
    }
}

/// A money value: the exact fraction and the producer's display text, both verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MoneyView {
    /// The exact value.
    pub exact: ExactMoneyView,
    /// The display text.
    pub display: String,
}

impl MoneyView {
    /// A money view.
    #[must_use]
    pub fn new(exact: ExactMoneyView, display: impl Into<String>) -> Self {
        Self {
            exact,
            display: display.into(),
        }
    }
}

/// The discount of a resolved total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiscountView {
    /// The discount amount.
    Available(MoneyView),
    /// The producer could not state a discount.
    Unavailable,
    /// A kind this SDK version does not know; treat as not composed / unavailable.
    #[serde(other)]
    Unknown,
}

/// The resolved total of the previewed lines, every amount verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResolvedTotalView {
    /// The currency code.
    pub currency: String,
    /// The currency's minor-unit scale.
    pub currency_scale: u8,
    /// The gross amount.
    pub gross: MoneyView,
    /// The net amount.
    pub net: MoneyView,
    /// The discount.
    pub discount: DiscountView,
    /// Whether usage-priced items make the total partial.
    pub usage_flagged: bool,
    /// Producer-declared exclusions, copied verbatim when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusions: Option<serde_json::Value>,
}

impl ResolvedTotalView {
    /// A resolved total view.
    #[must_use]
    pub fn new(
        currency: impl Into<String>,
        currency_scale: u8,
        gross: MoneyView,
        net: MoneyView,
        discount: DiscountView,
        usage_flagged: bool,
    ) -> Self {
        Self {
            currency: currency.into(),
            currency_scale,
            gross,
            net,
            discount,
            usage_flagged,
            exclusions: None,
        }
    }
}

/// The indicative tax of one line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LineTax {
    /// The line the tax belongs to.
    pub line_id: Uuid,
    /// The tax amount.
    pub tax: MoneyView,
}

impl LineTax {
    /// A line tax.
    #[must_use]
    pub fn new(line_id: Uuid, tax: MoneyView) -> Self {
        Self { line_id, tax }
    }
}

/// The indicative (nonbinding, unstored) tax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IndicativeTaxView {
    /// Tax per line, in basket order.
    pub per_line: Vec<LineTax>,
    /// The tax total as the producer states it.
    pub total: MoneyView,
    /// Whether the producer stated explicitly that no tax applies.
    pub explicit_no_tax: bool,
}

impl IndicativeTaxView {
    /// An indicative tax view.
    #[must_use]
    pub fn new(per_line: Vec<LineTax>, total: MoneyView, explicit_no_tax: bool) -> Self {
        Self {
            per_line,
            total,
            explicit_no_tax,
        }
    }
}
