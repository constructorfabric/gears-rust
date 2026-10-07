//! @cpt-dod:cpt-cf-bss-products-dod-lifecycle-edges:p1
//! Pure SKU content validation, patching and lifecycle rules.
//! @cpt-dod:cpt-cf-bss-products-dod-bundle-unpriced:p1
use crate::domain::caps;
use crate::domain::derived;
use crate::domain::error::DomainError;
use crate::domain::recognized::UsageRefAnswer;
use crate::domain::validation::ValidationReport;
use bss_products_sdk::models::{BillingTiming, Lifecycle, LifecycleNext, SkuContent, SkuType};
use uuid::Uuid;

/// Input for a new draft SKU.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone)]
pub struct NewSku {
    pub code: String,
    pub name: String,
    pub r#type: SkuType,
    /// Optional (P-D-196): `None` stays null, with no fallback to the tenant's default category.
    pub category_id: Option<Uuid>,
    pub description: String,
    pub sellable: bool,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<BillingTiming>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
}

/// Omitted nullable fields are unchanged; explicit null clears them. The PATCH wire contract is
/// `dto::SkuPatchRequest` alone; this domain type carries no serde of its own (RS-51).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::option_option,
    reason = "None = omitted; Some(None) = clear; Some(Some(_)) = set"
)]
pub struct SkuPatch {
    pub name: Option<String>,
    /// `Some(None)` clears the category (P-D-196).
    pub category_id: Option<Option<Uuid>>,
    pub description: Option<String>,
    pub sellable: Option<bool>,
    pub gl_code: Option<Option<String>>,
    pub tax_category: Option<Option<String>>,
    pub invoice_line_template: Option<Option<String>>,
    pub billing_timing: Option<Option<BillingTiming>>,
    pub usage_type_ref: Option<Option<String>>,
    pub unit: Option<Option<String>>,
    pub lifecycle: Option<Lifecycle>,
    pub r#type: Option<SkuType>,
}

/// Apply business fields without changing lifecycle or concurrency metadata. Every field of
/// the patch is named, so a field added to `SkuPatch` is a compile error here (RS-49).
#[must_use]
pub fn apply_patch(c: &SkuContent, p: &SkuPatch) -> SkuContent {
    let SkuPatch {
        name,
        category_id,
        description,
        sellable,
        gl_code,
        tax_category,
        invoice_line_template,
        billing_timing,
        usage_type_ref,
        unit,
        lifecycle: _,
        r#type,
    } = p;
    let mut out = c.clone();
    if let Some(v) = name {
        out.name.clone_from(v);
    }
    if let Some(v) = category_id {
        out.category_id = *v;
    }
    if let Some(v) = description {
        out.description.clone_from(v);
    }
    if let Some(v) = sellable {
        out.sellable = *v;
    }
    if let Some(v) = gl_code {
        out.gl_code.clone_from(v);
    }
    if let Some(v) = tax_category {
        out.tax_category.clone_from(v);
    }
    if let Some(v) = invoice_line_template {
        out.invoice_line_template.clone_from(v);
    }
    if let Some(v) = billing_timing {
        out.billing_timing = *v;
    }
    if let Some(v) = usage_type_ref {
        out.usage_type_ref.clone_from(v);
    }
    if let Some(v) = unit {
        out.unit.clone_from(v);
    }
    if let Some(v) = r#type {
        out.r#type = *v;
    }
    out
}

/// @cpt-cf-bss-products-fr-sku-define
#[must_use]
pub fn validate_new(new: &NewSku) -> ValidationReport {
    let mut r = ValidationReport::new();
    if new.code.trim().is_empty() {
        r.violate("VALIDATION", "code", "code must not be blank");
    }
    if new.name.trim().is_empty() {
        r.violate("VALIDATION", "name", "name must not be blank");
    }
    caps::check(&mut r, "code", Some(&new.code), caps::CODE_MAX_CHARS);
    caps::check(&mut r, "name", Some(&new.name), caps::NAME_MAX_CHARS);
    check_texts(
        &mut r,
        Some(&new.description),
        new.gl_code.as_deref(),
        new.tax_category.as_deref(),
        new.invoice_line_template.as_deref(),
        new.usage_type_ref.as_deref(),
        new.unit.as_deref(),
    );
    r
}

/// The caps of the texts a draft PATCH or a change carries (P-D-225): only a carried text is
/// judged, and a cleared one (`null`) carries none.
pub fn check_patch(r: &mut ValidationReport, p: &SkuPatch) {
    caps::check(r, "name", p.name.as_deref(), caps::NAME_MAX_CHARS);
    check_texts(
        r,
        p.description.as_deref(),
        p.gl_code.as_ref().and_then(Option::as_deref),
        p.tax_category.as_ref().and_then(Option::as_deref),
        p.invoice_line_template.as_ref().and_then(Option::as_deref),
        p.usage_type_ref.as_ref().and_then(Option::as_deref),
        p.unit.as_ref().and_then(Option::as_deref),
    );
}

/// The caps of a SKU's texts past its code and name.
fn check_texts(
    r: &mut ValidationReport,
    description: Option<&str>,
    gl_code: Option<&str>,
    tax_category: Option<&str>,
    invoice_line_template: Option<&str>,
    usage_type_ref: Option<&str>,
    unit: Option<&str>,
) {
    caps::check(r, "description", description, caps::NOTE_MAX_CHARS);
    caps::check(r, "gl_code", gl_code, caps::LABEL_MAX_CHARS);
    caps::check(r, "tax_category", tax_category, caps::LABEL_MAX_CHARS);
    caps::check(
        r,
        "invoice_line_template",
        invoice_line_template,
        caps::TEMPLATE_MAX_CHARS,
    );
    caps::check(
        r,
        "usage_type_ref",
        usage_type_ref,
        caps::USAGE_TYPE_REF_MAX_CHARS,
    );
    caps::check(r, "unit", unit, caps::LABEL_MAX_CHARS);
}

/// @cpt-cf-bss-products-fr-sku-metering · @cpt-cf-bss-products-fr-sku-bundle
#[must_use]
pub fn validate_publish(c: &SkuContent, usage_type: Option<&UsageRefAnswer>) -> ValidationReport {
    let mut r = ValidationReport::new();
    match c.r#type {
        SkuType::Usage => {
            if c.usage_type_ref.as_deref().unwrap_or("").trim().is_empty() {
                r.violate(
                    "USAGE_NEEDS_METER",
                    "usage_type_ref",
                    "a usage SKU names its usage type before publish",
                );
            }
            // P-D-259: the unit lives on the derived type. A raw ref cannot be published.
            // P-D-232: a derived ref is judged first, by the tenant's stored version the door
            // read. No catalog answer binds it, and a derived answer binds no GTS ref.
            if let Some(reference) = c
                .usage_type_ref
                .as_deref()
                .filter(|reference| derived::is_derived_ref(reference))
            {
                let pin = match usage_type {
                    Some(UsageRefAnswer::Derived(pin)) => Some(pin),
                    _ => None,
                };
                derived::judge_binding(&mut r, reference, c.unit.as_deref(), pin);
            } else if c
                .usage_type_ref
                .as_deref()
                .is_some_and(|reference| !reference.trim().is_empty())
            {
                r.violate(
                    derived::USAGE_TYPE_REQUIRED,
                    "usage_type_ref",
                    "a usage SKU names a derived usage type",
                );
            }
        }
        SkuType::Bundle => {
            if c.usage_type_ref.is_some() {
                r.violate(
                    "BUNDLE_HAS_NO_METER",
                    "usage_type_ref",
                    "a bundle SKU is never metered",
                );
            }
            if c.unit.is_some() {
                r.violate(
                    "BUNDLE_HAS_NO_METER",
                    "unit",
                    "a bundle SKU is never metered",
                );
            }
        }
        SkuType::Recurring | SkuType::OneTime => {}
    }
    r
}

/// @cpt-cf-bss-products-fr-sku-type-frozen
/// Refuse a type change while any reference is live.
///
/// # Errors
/// Returns `SKU_TYPE_FROZEN` for a nonzero live count.
pub fn validate_type_change(references: u32) -> Result<(), DomainError> {
    if references > 0 {
        return Err(DomainError::Conflict {
            code: "SKU_TYPE_FROZEN",
            detail: format!(
                "{references} live reference(s) point to this SKU; its type cannot change"
            ),
        });
    }
    Ok(())
}

/// The lifecycle a head stores, and a dated change that may already be due (P-D-249).
///
/// `next` is absent, or it carries both the lifecycle and the date. A stored pair that sets only
/// one of them is a corrupt row, refused before this value is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleHead {
    pub lifecycle: Lifecycle,
    pub next: Option<LifecycleNext>,
}

/// The lifecycle in force on `today`: `next` when its date has arrived, otherwise the stored
/// lifecycle. Reads use this; they do not wait for a write to fold the row (P-D-249).
#[must_use]
pub fn effective_lifecycle(head: LifecycleHead, today: time::Date) -> Lifecycle {
    match head.next {
        Some(next) if next.from <= today => next.lifecycle,
        _ => head.lifecycle,
    }
}

/// @cpt-cf-bss-products-fr-sku-lifecycle
/// A retire is not an edge (P-D-248): apply moves `published` or `deprecated` straight to
/// `retired`. There is no `retiring` lifecycle.
#[must_use]
pub const fn lifecycle_edge(from: Lifecycle, to: Lifecycle) -> bool {
    use Lifecycle::{Deprecated, Draft, Published, Retired};
    matches!(
        (from, to),
        (Draft | Deprecated, Published)
            | (Published, Deprecated)
            | (Published | Deprecated, Retired)
    )
}

/// Return sorted wire field names whose business values changed. Every field of `SkuContent` is
/// named, so a field added to it is a compile error here (RS-49).
#[must_use]
pub fn changed_fields(a: &SkuContent, b: &SkuContent) -> Vec<String> {
    let SkuContent {
        code,
        name,
        r#type,
        category_id,
        description,
        sellable,
        gl_code,
        tax_category,
        invoice_line_template,
        billing_timing,
        usage_type_ref,
        unit,
    } = a;
    let mut v = Vec::new();
    let mut diff = |field: &str, changed: bool| {
        if changed {
            v.push(field.to_owned());
        }
    };
    diff("code", *code != b.code);
    diff("name", *name != b.name);
    diff("type", *r#type != b.r#type);
    diff("category_id", *category_id != b.category_id);
    diff("description", *description != b.description);
    diff("sellable", *sellable != b.sellable);
    diff("gl_code", *gl_code != b.gl_code);
    diff("tax_category", *tax_category != b.tax_category);
    diff(
        "invoice_line_template",
        *invoice_line_template != b.invoice_line_template,
    );
    diff("billing_timing", *billing_timing != b.billing_timing);
    diff("usage_type_ref", *usage_type_ref != b.usage_type_ref);
    diff("unit", *unit != b.unit);
    v.sort();
    v
}

#[cfg(test)]
#[path = "sku_tests.rs"]
mod sku_tests;
