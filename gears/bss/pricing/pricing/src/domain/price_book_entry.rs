//! SKU-derived charge kinds, permitted models and invoice-line placeholders.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-price-models:p1
use super::RuleError;
use bss_products_sdk::models::SkuType;

string_enum!(ChargeKind {Recurring=>"recurring", Usage=>"usage", OneTime=>"one_time"});
string_enum!(Model {Flat=>"flat", PerUnit=>"per_unit", Graduated=>"graduated", Volume=>"volume", Package=>"package"});
// `released`: the reference of an archived book's entry, released in Products (D-522).
string_enum!(ReferenceState {ConfirmationPending=>"confirmation_pending", Confirmed=>"confirmed", Lost=>"lost", Released=>"released"});
string_enum!(OpState {Reserving=>"reserving", Written=>"written", Cancelling=>"cancelling", Releasing=>"releasing", Done=>"done"});

/// Derive charge kind from the current, freshly read SKU type.
/// # Errors
/// Bundle SKUs cannot be priced directly.
pub fn charge_kind_for(sku: SkuType) -> Result<ChargeKind, RuleError> {
    match sku {
        SkuType::Recurring => Ok(ChargeKind::Recurring),
        SkuType::Usage => Ok(ChargeKind::Usage),
        SkuType::OneTime => Ok(ChargeKind::OneTime),
        SkuType::Bundle => Err(RuleError::new("BUNDLE_SKU_NOT_PRICEABLE")),
    }
}
/// A recurring SKU is priced per `month` or `year`; any other SKU takes no period.
#[must_use]
pub fn period_valid(sku: SkuType, period: Option<&str>) -> bool {
    if sku == SkuType::Recurring {
        matches!(period, Some("month" | "year"))
    } else {
        period.is_none()
    }
}
/// Check an existing kind against a SKU; used for plan-item consistency.
/// # Errors
/// Returns the bundle or charge-kind mismatch refusal.
pub fn validate_entry_kind(kind: ChargeKind, sku: SkuType) -> Result<(), RuleError> {
    if charge_kind_for(sku)? == kind {
        Ok(())
    } else {
        Err(RuleError::new("CHARGE_KIND_SKU_TYPE"))
    }
}
/// Whether the charge kind permits this model.
#[must_use]
pub fn model_allowed(kind: ChargeKind, model: Model) -> bool {
    match kind {
        ChargeKind::Usage => model != Model::Flat,
        ChargeKind::Recurring | ChargeKind::OneTime => {
            matches!(model, Model::Flat | Model::PerUnit)
        }
    }
}
/// The model an entry without prices takes (D-427): flat for recurring and one-time, per unit for
/// usage. `m20260926_000013` backfills a priceless entry with it, and an entry create op stored
/// before that migration (no `model` in its input) resolves to it.
#[must_use]
pub const fn default_model(kind: ChargeKind) -> Model {
    match kind {
        ChargeKind::Usage => Model::PerUnit,
        ChargeKind::Recurring | ChargeKind::OneTime => Model::Flat,
    }
}
/// Check braces and the six supported placeholders.
/// # Errors
/// Returns `LINE_TEMPLATE_EMPTY` or `LINE_TEMPLATE_INVALID`.
pub fn validate_template(text: &str) -> Result<(), RuleError> {
    if text.trim().is_empty() {
        return Err(RuleError::new("LINE_TEMPLATE_EMPTY"));
    }
    let mut rest = text;
    while let Some(index) = rest.find(['{', '}']) {
        if rest.as_bytes()[index] == b'}' {
            return Err(RuleError::new("LINE_TEMPLATE_INVALID"));
        }
        rest = &rest[index + 1..];
        let Some(end) = rest.find('}') else {
            return Err(RuleError::new("LINE_TEMPLATE_INVALID"));
        };
        if !["sku", "sku_code", "unit", "plan", "period", "dimension"].contains(&&rest[..end]) {
            return Err(RuleError::new("LINE_TEMPLATE_INVALID"));
        }
        rest = &rest[end + 1..];
    }
    Ok(())
}
#[cfg(test)]
#[path = "price_book_entry_tests.rs"]
mod tests;
