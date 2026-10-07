//! The length caps of D-457 on the request bodies, judged by each door right after it parses its
//! body, before it reads or writes anything. A field the body does not carry is not judged, and
//! only new text is: a text that must name a stored row (a dimension key or value an entry, a
//! price or a PATCH of the registry names, a value it removes) is never capped, so a row stored
//! before the caps never locks its registry (the second review of W1a, L1). The two full-replace
//! PUTs are judged against the stored row, after their If-Match: the registry's caps only the
//! keys and values it does not hold ([`new_dimension_text`]), the settings' only the texts that
//! differ from the stored ones ([`changed_settings_text`], the second review of W1b, L2).
use super::dto::{
    PriceBookCreate, PriceBookPatch, PricingDimensionKeyPatch, PricingPlanClone, PricingPlanCreate,
    PricingPlanPatch, PricingPriceBookEntryCreate, PricingPriceBookEntryPatch, PricingPriceCreate,
    PricingPricePatch, PricingPublishChangesRequest, PricingSettingsDto, PricingSettingsPut,
};
use super::support::invalid_because;
use crate::domain::caps::{
    CODE_MAX_CHARS, LABEL_MAX_CHARS, METER_REF_MAX_CHARS, NAME_MAX_CHARS, NOTE_MAX_CHARS,
    TEMPLATE_MAX_CHARS, over,
};
use toolkit_canonical_errors::CanonicalError;

/// 400 `FIELD_TOO_LONG` on `field` for a text longer than `max` characters.
pub(super) fn field(name: &str, text: &str, max: usize) -> Result<(), CanonicalError> {
    if over(text, max) {
        Err(invalid_because(
            name,
            "FIELD_TOO_LONG",
            &format!("{name} is at most {max} characters"),
        ))
    } else {
        Ok(())
    }
}
/// [`field`] for each text of a list.
fn each(name: &str, texts: &[String], max: usize) -> Result<(), CanonicalError> {
    texts.iter().try_for_each(|text| field(name, text, max))
}
/// A note keeps its own code: 400 `NOTE_TOO_LONG` on `note`, as a vote's note (the approval
/// engine) and products' submit notes answer it. A submit's note (D-464) is judged here too, by
/// its door, before any read: the engine's submit caps none.
pub(super) fn note(text: Option<&str>) -> Result<(), CanonicalError> {
    if text.is_some_and(|t| over(t, NOTE_MAX_CHARS)) {
        Err(invalid_because(
            "note",
            "NOTE_TOO_LONG",
            &format!("a note is at most {NOTE_MAX_CHARS} characters"),
        ))
    } else {
        Ok(())
    }
}
/// A request body whose text fields have their caps.
pub(super) trait Capped {
    /// # Errors
    /// 400 `FIELD_TOO_LONG` (or `NOTE_TOO_LONG`) on the first field over its cap.
    fn caps(&self) -> Result<(), CanonicalError>;
}
impl Capped for PriceBookCreate {
    // The description keeps its own code, `BOOK_DESCRIPTION_TOO_LONG`, judged with the book.
    fn caps(&self) -> Result<(), CanonicalError> {
        field("code", &self.code, CODE_MAX_CHARS)?;
        field("name", &self.name, NAME_MAX_CHARS)
    }
}
impl Capped for PriceBookPatch {
    fn caps(&self) -> Result<(), CanonicalError> {
        self.name
            .as_deref()
            .map_or(Ok(()), |name| field("name", name, NAME_MAX_CHARS))
    }
}
impl Capped for PricingPlanCreate {
    fn caps(&self) -> Result<(), CanonicalError> {
        field("code", &self.code, CODE_MAX_CHARS)?;
        field("name", &self.name, NAME_MAX_CHARS)
    }
}
impl Capped for PricingPlanClone {
    fn caps(&self) -> Result<(), CanonicalError> {
        field("code", &self.code, CODE_MAX_CHARS)?;
        field("name", &self.name, NAME_MAX_CHARS)
    }
}
impl Capped for PricingPlanPatch {
    fn caps(&self) -> Result<(), CanonicalError> {
        field("name", &self.name, NAME_MAX_CHARS)
    }
}
// A price's `dim_value` names a value of its entry's key (400 `DIM_VALUE_UNKNOWN` otherwise), so
// it is not new text and has no cap of its own.
impl Capped for PricingPriceCreate {
    fn caps(&self) -> Result<(), CanonicalError> {
        note(self.note.as_deref())
    }
}
impl Capped for PricingPricePatch {
    fn caps(&self) -> Result<(), CanonicalError> {
        note(self.note.as_ref().and_then(Option::as_deref))
    }
}
impl Capped for PricingPublishChangesRequest {
    fn caps(&self) -> Result<(), CanonicalError> {
        note(self.note.as_deref())
    }
}
// An entry's `dimension_key` names a declared key (400 `DIM_NOT_DECLARED` otherwise), so it is not
// new text and has no cap of its own.
impl Capped for PricingPriceBookEntryCreate {
    fn caps(&self) -> Result<(), CanonicalError> {
        self.invoice_line_override
            .as_deref()
            .map_or(Ok(()), |line| {
                field("invoice_line_override", line, TEMPLATE_MAX_CHARS)
            })?;
        let Some(policy) = &self.usage_rating_policy else {
            return Ok(());
        };
        let Some(quantity) = &policy.quantity_semantics else {
            return Ok(());
        };
        field(
            "usage_rating_policy.usage_type_id",
            &quantity.meter.usage_type_id,
            METER_REF_MAX_CHARS,
        )?;
        field(
            "usage_rating_policy.version",
            &quantity.meter.version,
            CODE_MAX_CHARS,
        )?;
        field("usage_rating_policy.unit", &quantity.unit, CODE_MAX_CHARS)?;
        field(
            "usage_rating_policy.accrual_policy_version",
            &quantity.accrual_policy_version,
            METER_REF_MAX_CHARS,
        )
    }
}
/// A list's `q`, before it becomes a pattern. 400 `FIELD_TOO_LONG` on `q`.
pub(super) fn search(text: &str) -> Result<(), CanonicalError> {
    field("q", text, NAME_MAX_CHARS)
}
impl Capped for PricingPriceBookEntryPatch {
    fn caps(&self) -> Result<(), CanonicalError> {
        match &self.invoice_line_override {
            Some(Some(line)) => field("invoice_line_override", line, TEMPLATE_MAX_CHARS),
            _ => Ok(()),
        }
    }
}
/// The settings PUT (judged in `configuration::put_settings` against the stored settings, after
/// If-Match): 400 `FIELD_TOO_LONG` on `default_gl` or `default_tax_category` for a value other than
/// the stored one, and on `invoice_line_templates` for a template other than the one stored under
/// its SKU type, longer than its cap. The PUT replaces the whole row, so every write carries the
/// stored texts back: a text stored before the caps passes unchanged whatever its length, and never
/// locks the settings.
pub(super) fn changed_settings_text(
    body: &PricingSettingsPut,
    stored: &PricingSettingsDto,
) -> Result<(), CanonicalError> {
    for (name, sent, kept) in [
        ("default_gl", &body.default_gl, &stored.default_gl),
        (
            "default_tax_category",
            &body.default_tax_category,
            &stored.default_tax_category,
        ),
    ] {
        if let Some(text) = sent
            .as_deref()
            .filter(|text| Some(*text) != kept.as_deref())
        {
            field(name, text, LABEL_MAX_CHARS)?;
        }
    }
    body.invoice_line_templates
        .iter()
        .filter(|(kind, line)| {
            stored
                .invoice_line_templates
                .get(kind.as_str())
                .and_then(serde_json::Value::as_str)
                != Some(line.as_str())
        })
        .try_for_each(|(_, line)| field("invoice_line_templates", line, TEMPLATE_MAX_CHARS))
}
/// The PUT of the registry (`PricingDimensions`, judged in `configuration::put_dimensions` against
/// the stored registry): 400 `FIELD_TOO_LONG` on `key` for a key the registry does not hold, and on
/// `values` for a value its key does not hold, longer than a code's cap. A key or value already
/// stored passes whatever its length.
pub(super) fn new_dimension_text(
    key: &str,
    values: &[String],
    stored: Option<&[String]>,
) -> Result<(), CanonicalError> {
    let Some(held) = stored else {
        field("key", key, CODE_MAX_CHARS)?;
        return each("values", values, CODE_MAX_CHARS);
    };
    values
        .iter()
        .filter(|value| !held.contains(value))
        .try_for_each(|value| field("values", value, CODE_MAX_CHARS))
}
// The PATCH names a stored key (400 `DIM_NOT_DECLARED` otherwise) and removes stored values (400
// `DIM_VALUE_UNKNOWN` otherwise): only the values it adds are new text.
impl Capped for PricingDimensionKeyPatch {
    fn caps(&self) -> Result<(), CanonicalError> {
        each("add", &self.add, CODE_MAX_CHARS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::usage_policy_wire::{
        AggregationScope, Fold, MeterRef, PartialWindow, QuantitySemanticsRequest, RatingWindow,
        Reset, UsageRatingPolicyRequest,
    };
    use uuid::Uuid;

    fn policy(usage_type_id: &str) -> UsageRatingPolicyRequest {
        UsageRatingPolicyRequest {
            rating_window: RatingWindow::BillingCycle,
            aggregation_scope: AggregationScope::SubscriptionLine,
            reset: Reset::RatingWindowStart,
            quantity_semantics: Some(QuantitySemanticsRequest {
                meter: MeterRef {
                    usage_type_id: usage_type_id.to_owned(),
                    version: "1".into(),
                },
                unit: "h".into(),
                fold: Fold::Sum,
                accrual_policy_version: "v1".into(),
            }),
            partial_window: PartialWindow::ActualQuantityFullThresholds,
            fold: None,
        }
    }
    fn create(usage_type_id: &str) -> PricingPriceBookEntryCreate {
        PricingPriceBookEntryCreate {
            usage_rating_policy: Some(policy(usage_type_id)),
            sku_id: Uuid::nil(),
            model: "per_unit".into(),
            period: None,
            dimension_key: None,
            invoice_line_override: None,
        }
    }

    #[test]
    fn a_meter_id_over_the_meter_ref_cap_is_field_too_long() {
        let error = create(&"m".repeat(METER_REF_MAX_CHARS + 1))
            .caps()
            .unwrap_err();
        let body =
            serde_json::to_string(&toolkit_canonical_errors::Problem::from_error(&error).unwrap())
                .unwrap();
        assert!(body.contains("FIELD_TOO_LONG"), "{body}");
        assert!(body.contains("usage_rating_policy.usage_type_id"), "{body}");
        assert!(create(&"m".repeat(METER_REF_MAX_CHARS)).caps().is_ok());
    }

    /// A derived meter's real strings pass: its accrual version is `derived-v1:` plus 64 hex
    /// digits (75 characters), over a code's 64, and a raw GTS id may pass 64 too.
    #[test]
    fn a_derived_accrual_version_and_a_long_gts_id_fit() {
        let mut entry = create("gts.cf.core.uc.usage_record.v1~cf.bss.usage_type.memorygbhours.v1");
        if let Some(quantity) = entry
            .usage_rating_policy
            .as_mut()
            .and_then(|policy| policy.quantity_semantics.as_mut())
        {
            quantity.accrual_policy_version = format!("derived-v1:{}", "a".repeat(64));
        }
        assert!(entry.caps().is_ok());
    }

    #[test]
    fn a_search_over_the_name_cap_is_field_too_long() {
        let error = search(&"q".repeat(NAME_MAX_CHARS + 1)).unwrap_err();
        let body =
            serde_json::to_string(&toolkit_canonical_errors::Problem::from_error(&error).unwrap())
                .unwrap();
        assert!(body.contains("FIELD_TOO_LONG"), "{body}");
        assert!(search(&"q".repeat(NAME_MAX_CHARS)).is_ok());
    }
}
