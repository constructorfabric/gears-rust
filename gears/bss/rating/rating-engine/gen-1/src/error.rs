//! The closed error taxonomy of generation 1 (rating DESIGN 01 §4.4).
//!
//! Every variant is raised only by `rating-core` and becomes an exception `reason_code`; a child
//! that fails gets no result. The set is closed for this generation: a new error, or a condition
//! moved to another error, is a new generation. Each variant carries a `detail` for the exception
//! register; the `reason_code` alone decides what the pipeline does.

use bss_money::exact::ExactError;
use serde::{Deserialize, Serialize};

/// Why an evaluation failed, with the detail an operator reads in the exception register.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason_code", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvaluationError {
    /// A cell the child prices has no binding: Pricing answered uncovered, never free.
    #[error("price_uncovered: {detail}")]
    PriceUncovered { detail: String },
    /// The period-start binding differs from the accepted binding the fact carries.
    #[error("binding_mismatch: {detail}")]
    BindingMismatch { detail: String },
    /// The bindings of one child carry different currencies, or two amounts of different
    /// currencies or stored scales were combined; never converted.
    #[error("binding_currency_mismatch: {detail}")]
    BindingCurrencyMismatch { detail: String },
    /// Two cells of one line resolve to the same SKU or derived meter and dimension value for one
    /// charge kind (`rating-val-02`).
    #[error("non_injective_mapping: {detail}")]
    NonInjectiveMapping { detail: String },
    /// Usage dimension values are missing or partial and no default chain covers them.
    #[error("unroutable_dimension_tuple: {detail}")]
    UnroutableDimensionTuple { detail: String },
    /// A model, charge kind, rating window or other enum value this generation does not know:
    /// raised when `rating-core` decodes the pipeline's text into this generation's types
    /// (`canonical::decode_input`, DESIGN 01 §3.1), since a typed input cannot hold such a value.
    #[error("unknown_enum_value: {detail}")]
    UnknownEnumValue { detail: String },
    /// A derived usage type has a `Peak` or `TimeWeighted` input (R-06).
    #[error("unsupported_input_fold: {detail}")]
    UnsupportedInputFold { detail: String },
    /// A derived usage type takes a raw type whose collector fold is not `SUM` as a `Sum` input.
    #[error("usage_type_not_chargeable: {detail}")]
    UsageTypeNotChargeable { detail: String },
    /// The Products derived evaluator refused the granule.
    #[error("derived_evaluation_failed: {detail}")]
    DerivedEvaluationFailed { detail: String },
    /// A derived formula step would leave `rust_decimal`'s exact range, so Products would round.
    #[error("derived_formula_inexact: {detail}")]
    DerivedFormulaInexact { detail: String },
    /// A primitive this generation does not implement.
    #[error("unsupported_primitive: {detail}")]
    UnsupportedPrimitive { detail: String },
    /// A slice or window quantity is below zero.
    #[error("negative_window_quantity: {detail}")]
    NegativeWindowQuantity { detail: String },
    /// The input names a contract override, commitment pool, reservation, coupon or promotion;
    /// price adjustments are deferred and nothing is applied by default.
    #[error("adjustment_unsupported: {detail}")]
    AdjustmentUnsupported { detail: String },
    /// The billing currency differs from the binding's price currency; FX is deferred.
    #[error("fx_not_supported: {detail}")]
    FxNotSupported { detail: String },
    /// A binding's usage rating policy changes inside a `CalendarHour` window.
    #[error("intra_window_policy_change: {detail}")]
    IntraWindowPolicyChange { detail: String },
    /// A reduced amount leaves the wire and storage bound, or an operand the arithmetic budget.
    #[error("precision_overflow: {detail}")]
    PrecisionOverflow { detail: String },
    /// A quantity computed inside evaluation exceeds 60 integer or 28 fractional digits.
    #[error("quantity_out_of_range: {detail}")]
    QuantityOutOfRange { detail: String },
    /// The requested generation is not this one, or its digest or budget differs.
    #[error("engine_unavailable: {detail}")]
    EngineUnavailable { detail: String },
    /// The child's layout disagrees with what the evaluation computes from its inputs: slices cut
    /// at other split points, a slice quantity that is not its committed quantity or its granules'
    /// derived sum, a meter or scope other than the child's, or a window-group context that does
    /// not add up.
    #[error("layout_mismatch: {detail}")]
    LayoutMismatch { detail: String },
    /// The fact's period bounds disagree with its billing terms, or a slice leaves the period.
    #[error("period_geometry_mismatch: {detail}")]
    PeriodGeometryMismatch { detail: String },
    /// The money crate returned an error the call sites exclude: an engine defect.
    #[error("money_contract_violation: {detail}")]
    MoneyContractViolation { detail: String },
}

impl EvaluationError {
    /// The exception `reason_code` of this error.
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::PriceUncovered { .. } => "price_uncovered",
            Self::BindingMismatch { .. } => "binding_mismatch",
            Self::BindingCurrencyMismatch { .. } => "binding_currency_mismatch",
            Self::NonInjectiveMapping { .. } => "non_injective_mapping",
            Self::UnroutableDimensionTuple { .. } => "unroutable_dimension_tuple",
            Self::UnknownEnumValue { .. } => "unknown_enum_value",
            Self::UnsupportedInputFold { .. } => "unsupported_input_fold",
            Self::UsageTypeNotChargeable { .. } => "usage_type_not_chargeable",
            Self::DerivedEvaluationFailed { .. } => "derived_evaluation_failed",
            Self::DerivedFormulaInexact { .. } => "derived_formula_inexact",
            Self::UnsupportedPrimitive { .. } => "unsupported_primitive",
            Self::NegativeWindowQuantity { .. } => "negative_window_quantity",
            Self::AdjustmentUnsupported { .. } => "adjustment_unsupported",
            Self::FxNotSupported { .. } => "fx_not_supported",
            Self::IntraWindowPolicyChange { .. } => "intra_window_policy_change",
            Self::PrecisionOverflow { .. } => "precision_overflow",
            Self::QuantityOutOfRange { .. } => "quantity_out_of_range",
            Self::EngineUnavailable { .. } => "engine_unavailable",
            Self::LayoutMismatch { .. } => "layout_mismatch",
            Self::PeriodGeometryMismatch { .. } => "period_geometry_mismatch",
            Self::MoneyContractViolation { .. } => "money_contract_violation",
        }
    }

    /// The operator-facing detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::PriceUncovered { detail }
            | Self::BindingMismatch { detail }
            | Self::BindingCurrencyMismatch { detail }
            | Self::NonInjectiveMapping { detail }
            | Self::UnroutableDimensionTuple { detail }
            | Self::UnknownEnumValue { detail }
            | Self::UnsupportedInputFold { detail }
            | Self::UsageTypeNotChargeable { detail }
            | Self::DerivedEvaluationFailed { detail }
            | Self::DerivedFormulaInexact { detail }
            | Self::UnsupportedPrimitive { detail }
            | Self::NegativeWindowQuantity { detail }
            | Self::AdjustmentUnsupported { detail }
            | Self::FxNotSupported { detail }
            | Self::IntraWindowPolicyChange { detail }
            | Self::PrecisionOverflow { detail }
            | Self::QuantityOutOfRange { detail }
            | Self::EngineUnavailable { detail }
            | Self::LayoutMismatch { detail }
            | Self::PeriodGeometryMismatch { detail }
            | Self::MoneyContractViolation { detail } => detail,
        }
    }

    /// Maps a money-crate error raised at `site` (DESIGN 01 §4.4, "Money-crate errors"): the
    /// arithmetic limit is `precision_overflow`; every other error is one the core's call sites
    /// exclude, so it is `money_contract_violation`.
    pub(crate) fn from_money(site: &str, error: &ExactError) -> Self {
        match error {
            ExactError::ArithmeticLimit => Self::PrecisionOverflow {
                detail: format!("{site}: the exact arithmetic limit was exceeded"),
            },
            ExactError::Money(_)
            | ExactError::DivisionByZero
            | ExactError::EmptyWeights
            | ExactError::NegativeWeight
            | ExactError::TooManyTerms => Self::MoneyContractViolation {
                detail: format!("{site}: {error}"),
            },
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
