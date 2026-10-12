//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-rules:p1
//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-pin:p1
//! Derived usage types: the create and new-version rules (P-D-229, P-D-231).
//!
//! A derived usage type is a tenant's catalog data: a stable `id`, a `code` unique in the tenant, a
//! `name` set at the create, and versions 1, 2, … each holding one immutable declaration. The rules
//! of a declaration are the SDK's ([`derived::validate`]); this module turns its typed refusal into
//! the door's 400 `DERIVED_DECLARATION_INVALID`, which names the rule. A version stores the SHA-256
//! of the SDK's canonical bytes, taken here through `aws-lc-rs` (lint DE0708), and nothing ever
//! recomputes it: the stored digest is what pricing pins (P-D-229 decision 3).
//!
//! Every input names a raw GTS usage type, and each is resolved through the [`UsageTypeCatalog`]
//! port as the caller, as a usage SKU's publish is (P-D-184, P-D-207): an unresolved input is 400
//! `USAGE_TYPE_UNRESOLVED`, an unreachable or unconfigured catalog 503 `USAGE_TYPE_UNAVAILABLE`, and
//! a catalog that refuses the caller 403 `USAGE_TYPE_FORBIDDEN`. A derived input is refused before
//! the catalog is asked: [`derived::validate`] refuses the `products.derived/` prefix.
//!
//! A usage SKU names a version by its meter id and pins it at its first publish (P-D-232):
//! [`judge_binding`] is the binding rule the draft doors and the publish rule share.
//! [`metering_moves`] is the rule a published usage SKU is judged by, at the change door, at submit
//! and at apply (P-D-258): it keeps its ref and its unit. A published raw meter may move onto the
//! identity wrapper of that meter ([`wraps`], P-D-251); every other move stays refused.
use crate::domain::caps;
use crate::domain::error::DomainError;
use crate::domain::recognized::UsageTypeAnswer;
use crate::domain::validation::ValidationReport;
use aws_lc_rs::digest::{SHA256, digest as sha256};
use bss_products_sdk::derived::{self, DeclarationError, DerivedUsageDeclaration, Expr, MeterId};
use bss_products_sdk::usage_types::UsageTypeCatalog;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The audit `subject_kind` of a derived usage type's acts.
pub const SUBJECT_KIND: &str = "derived_usage_type";
/// The audit action of a create (version 1).
pub const ACTION_CREATE: &str = "derived_usage_type.create";
/// The audit action of a new version (2 and later).
pub const ACTION_VERSION: &str = "derived_usage_type.version";
/// A declaration the shape parse or the SDK refused: 400, naming the rule.
pub const DECLARATION_INVALID: &str = "DERIVED_DECLARATION_INVALID";
/// A second type with the tenant's code: 409.
pub const CODE_TAKEN: &str = "DERIVED_CODE_TAKEN";
/// The head of a version's `accrual_policy_version`: `derived-v1:<digest hex>`.
pub const ACCRUAL_POLICY_PREFIX: &str = "derived-v1:";
/// A SKU's derived ref the tenant does not hold (P-D-232): 400 on `usage_type_ref`.
pub const USAGE_TYPE_UNKNOWN: &str = "DERIVED_USAGE_TYPE_UNKNOWN";
/// A SKU's unit other than its derived version's output unit (P-D-232): 400 on `unit`.
pub const UNIT_MISMATCH: &str = "DERIVED_UNIT_MISMATCH";
/// A change that moves a published usage SKU's metering (P-D-258): 400 at submit, 409 at apply.
/// PROBE-9-13-2: the code is `METERING_IMMUTABLE`.
pub const METERING_IMMUTABLE: &str = "METERING_IMMUTABLE";
/// A usage SKU's ref is not a derived usage type (P-D-259): 400 on `usage_type_ref`.
pub const USAGE_TYPE_REQUIRED: &str = "DERIVED_USAGE_TYPE_REQUIRED";
/// The binding's codes: they refuse a SKU's write, so they name the SKU, not the derived type.
pub const SKU_BINDING_CODES: [&str; 4] = [
    USAGE_TYPE_UNKNOWN,
    UNIT_MISMATCH,
    METERING_IMMUTABLE,
    USAGE_TYPE_REQUIRED,
];

// The SDK's caps are the SKU's (Run 1's note): a derived output unit is the selling SKU's unit, and
// an input ref is a usage-type ref.
const _: () = assert!(derived::UNIT_MAX_CHARS == caps::LABEL_MAX_CHARS);
const _: () = assert!(derived::INPUT_REF_MAX_CHARS == caps::USAGE_TYPE_REF_MAX_CHARS);
const _: () = assert!(derived::CODE_MAX_CHARS == caps::CODE_MAX_CHARS);

/// A stored derived usage type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedUsageType {
    pub tenant_id: Uuid,
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// A stored version of a derived usage type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedUsageTypeVersion {
    pub tenant_id: Uuid,
    pub type_id: Uuid,
    pub version: u32,
    /// The declaration as the doors serve it (`dto::DerivedDeclarationDto`).
    pub declaration_json: serde_json::Value,
    /// The stored SHA-256 of the canonical bytes, 64 lowercase hex digits.
    pub digest: String,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

impl DerivedUsageTypeVersion {
    /// `derived-v1:<stored digest>`: what a pricing usage policy names as its accrual (decision 5).
    #[must_use]
    pub fn accrual_policy_version(&self) -> String {
        format!("{ACCRUAL_POLICY_PREFIX}{}", self.digest)
    }
}

/// What a create writes: the type and its version 1.
#[domain_model]
#[derive(Debug, Clone)]
pub struct NewDerivedType {
    pub code: String,
    pub name: String,
}

/// What a version insert writes.
#[domain_model]
#[derive(Debug, Clone)]
pub struct NewDerivedVersion {
    pub type_id: Uuid,
    pub version: u32,
    pub declaration_json: serde_json::Value,
    pub digest: String,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// The identity rules of a create: a code the meter id can carry, and a name.
#[must_use]
pub fn check_identity(new: &NewDerivedType) -> ValidationReport {
    let mut report = ValidationReport::new();
    if caps::over(&new.code, caps::CODE_MAX_CHARS) {
        caps::check(&mut report, "code", Some(&new.code), caps::CODE_MAX_CHARS);
    } else if MeterId::new(new.code.as_str(), 1).is_err() {
        report.violate("VALIDATION", "code", "code is ^[a-z0-9][a-z0-9._-]{0,63}$");
    }
    if new.name.trim().is_empty() {
        report.violate("VALIDATION", "name", "name must not be blank");
    }
    caps::check(&mut report, "name", Some(&new.name), caps::NAME_MAX_CHARS);
    report
}

/// The rule a [`DeclarationError`] names, as the 400 carries it: one token per SDK variant.
#[must_use]
pub const fn rule(error: &DeclarationError) -> &'static str {
    match error {
        DeclarationError::EmptyUnit { .. } => "empty_unit",
        DeclarationError::UnitTooLong { .. } => "unit_too_long",
        DeclarationError::ScaleTooLarge { .. } => "scale_too_large",
        DeclarationError::TooFewInputs { .. } => "too_few_inputs",
        DeclarationError::InvalidInputName { .. } => "invalid_input_name",
        DeclarationError::DuplicateInput { .. } => "duplicate_input",
        DeclarationError::EmptyInputRef { .. } => "empty_input_ref",
        DeclarationError::InputRefTooLong { .. } => "input_ref_too_long",
        DeclarationError::DerivedInput { .. } => "derived_input",
        DeclarationError::HoldMissing { .. } => "hold_missing",
        DeclarationError::HoldNotAllowed { .. } => "hold_not_allowed",
        DeclarationError::HoldOutOfRange { .. } => "hold_out_of_range",
        DeclarationError::UnknownInput { .. } => "unknown_input",
        DeclarationError::UnusedInput { .. } => "unused_input",
        DeclarationError::DivisionByZero => "division_by_zero",
        DeclarationError::TooFewOperands { .. } => "too_few_operands",
        DeclarationError::TooDeep => "too_deep",
        DeclarationError::TooManyNodes => "too_many_nodes",
    }
}

/// 400 `DERIVED_DECLARATION_INVALID` on `declaration`, its detail led by the rule: `<rule>: <why>`.
#[must_use]
pub fn declaration_invalid(rule: &str, detail: impl std::fmt::Display) -> DomainError {
    let mut report = ValidationReport::new();
    report.violate(
        DECLARATION_INVALID,
        "declaration",
        format!("{rule}: {detail}"),
    );
    DomainError::Validation(report)
}

/// The SDK's rules, as the door answers them.
///
/// # Errors
/// [`declaration_invalid`] naming the first rule the SDK refused.
pub fn validate(declaration: &DerivedUsageDeclaration) -> Result<(), DomainError> {
    derived::validate(declaration).map_err(|e| declaration_invalid(rule(&e), &e))
}

/// The SHA-256 of [`derived::canonical_bytes`], as 64 lowercase hex digits: what a version stores.
#[must_use]
pub fn digest_hex(declaration: &DerivedUsageDeclaration) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    sha256(&SHA256, &derived::canonical_bytes(declaration))
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            hex.push(char::from(DIGITS[usize::from(b >> 4)]));
            hex.push(char::from(DIGITS[usize::from(b & 0x0f)]));
            hex
        })
}

/// Resolve every input through the catalog, as the caller, in input order; each input is asked
/// once. A refusal of the caller answers 403 and an outage 503, whichever input met it; otherwise
/// every unresolved input is one 400 `USAGE_TYPE_UNRESOLVED` on its ref (P-D-202).
///
/// # Errors
/// [`DomainError::UsageTypeForbidden`], [`DomainError::UsageTypeUnavailable`], or
/// [`DomainError::Validation`] with one violation per unresolved input.
pub async fn resolve_inputs(
    catalog: &dyn UsageTypeCatalog,
    ctx: &SecurityContext,
    declaration: &DerivedUsageDeclaration,
) -> Result<(), DomainError> {
    let mut unresolved = ValidationReport::new();
    let (mut forbidden, mut unavailable) = (None, None);
    for input in &declaration.inputs {
        match catalog.resolve(ctx, &input.usage_type_ref).await {
            UsageTypeAnswer::Resolved(_) => {}
            UsageTypeAnswer::Unresolved => unresolved.violate(
                "USAGE_TYPE_UNRESOLVED",
                format!("declaration.inputs.{}.usage_type_ref", input.name),
                format!(
                    "the usage type catalog does not know {}",
                    input.usage_type_ref
                ),
            ),
            UsageTypeAnswer::Forbidden => {
                forbidden.get_or_insert_with(|| input.usage_type_ref.clone());
            }
            UsageTypeAnswer::Unavailable => {
                unavailable.get_or_insert_with(|| input.usage_type_ref.clone());
            }
        }
    }
    if let Some(reference) = forbidden {
        return Err(DomainError::UsageTypeForbidden(reference));
    }
    if let Some(reference) = unavailable {
        return Err(DomainError::UsageTypeUnavailable(reference));
    }
    if unresolved.is_empty() {
        Ok(())
    } else {
        Err(DomainError::Validation(unresolved))
    }
}

/// A usage SKU's derived ref as this gear's store answers it (P-D-232): the tenant's version, read
/// by its meter id, and the output unit the SKU sells.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedPin {
    /// `products.derived/<code>@<n>`, canonical.
    pub meter: String,
    /// The version's `output_unit`.
    pub output_unit: String,
}

/// Whether `reference` names a derived usage type. The `products.derived/` prefix is reserved, so a
/// GTS id never starts with it (decision 4): a ref that does is judged here, whatever follows, and
/// is never a catalog's question.
#[must_use]
pub fn is_derived_ref(reference: &str) -> bool {
    reference.starts_with(derived::DERIVED_METER_PREFIX)
}

/// A usage SKU names a derived usage type (P-D-259). A raw ref is 400 [`USAGE_TYPE_REQUIRED`],
/// before any catalog is asked.
///
/// PROBE-9-13-4: a raw ref is refused here.
#[must_use]
pub fn usage_type_required() -> DomainError {
    let mut report = ValidationReport::new();
    report.violate(
        USAGE_TYPE_REQUIRED,
        "usage_type_ref",
        "a usage SKU names a derived usage type",
    );
    DomainError::Validation(report)
}

/// The unit a derived SKU stores (P-D-259): none. A raw SKU keeps the unit it was given.
///
/// PROBE-9-13-5: a derived SKU's unit is not stored.
#[must_use]
pub fn persisted_unit(reference: Option<&str>, unit: Option<String>) -> Option<String> {
    // PROBE-9-13-5: a derived SKU stores no unit.
    if reference.is_some_and(is_derived_ref) {
        None
    } else {
        unit
    }
}

/// The binding rule (P-D-232): a derived `reference` binds when `pin` is the tenant's version of it
/// and the SKU's `unit`, when it names one, is that version's output unit. Otherwise it records 400
/// [`USAGE_TYPE_UNKNOWN`] on `usage_type_ref`, or [`UNIT_MISMATCH`] on `unit`. A blank unit is no
/// unit: a draft may name its unit later, and a publish needs one (`USAGE_NEEDS_METER`).
pub fn judge_binding(
    report: &mut ValidationReport,
    reference: &str,
    unit: Option<&str>,
    pin: Option<&DerivedPin>,
) {
    let Some(pin) = pin.filter(|p| p.meter == reference) else {
        report.violate(
            USAGE_TYPE_UNKNOWN,
            "usage_type_ref",
            format!("the tenant holds no derived usage type version {reference}"),
        );
        return;
    };
    if let Some(unit) = unit.filter(|u| !u.trim().is_empty())
        && unit != pin.output_unit
    {
        report.violate(
            UNIT_MISMATCH,
            "unit",
            format!(
                "{reference} sells {}; a usage SKU on it sells that unit",
                pin.output_unit
            ),
        );
    }
}

/// The pin rule (P-D-232, M1), pure: true when the current or the proposed ref is derived and
/// the two differ. `@1` → `@2`, raw → derived, derived → raw, and a derived ref dropped (a type
/// change included) are all moves. A raw meter moving onto the identity wrapper of that meter is
/// still a move here; the callers allow it only when [`wrap_exception`] holds (P-D-251).
#[must_use]
pub fn pin_moves(current: Option<&str>, proposed: Option<&str>) -> bool {
    current != proposed
        && (current.is_some_and(is_derived_ref) || proposed.is_some_and(is_derived_ref))
}

/// Whether `declaration` wraps the raw meter `current_raw` and sells `sku_unit` (P-D-251).
///
/// All of these hold: exactly one input; that input's `usage_type_ref` equals `current_raw`
/// whole-string; the formula is the identity `{"op":"input","name":<that input>}`; `output_unit`
/// equals the input's `unit` and equals `sku_unit`.
#[must_use]
pub fn wraps(current_raw: &str, declaration: &DerivedUsageDeclaration, sku_unit: &str) -> bool {
    // PROBE-W-1: exactly one input.
    let [input] = declaration.inputs.as_slice() else {
        return false;
    };
    if input.usage_type_ref != current_raw {
        return false;
    }
    // PROBE-W-2: the formula is the identity over that input.
    let identity = matches!(&declaration.formula, Expr::Input(name) if name == &input.name);
    if !identity {
        return false;
    }
    // PROBE-W-3: the output unit is the input's unit and the SKU's unit.
    declaration.output_unit == input.unit && declaration.output_unit == sku_unit
}

/// A blank unit is no unit, as a draft may leave its unit for later.
fn named_unit(unit: Option<&str>) -> Option<&str> {
    unit.filter(|unit| !unit.trim().is_empty())
}

/// The current ref is a raw GTS id and the proposed ref is derived: the only shape that can be a
/// wrap. Every other move is not a candidate.
#[must_use]
pub fn wrap_candidate(current: Option<&str>, proposed: Option<&str>) -> bool {
    match (current, proposed) {
        (Some(current), Some(proposed)) => !is_derived_ref(current) && is_derived_ref(proposed),
        _ => false,
    }
}

/// A usage ref and the unit stored beside it.
#[domain_model]
#[derive(Clone, Copy)]
pub struct RefUnit<'a> {
    pub usage_type_ref: Option<&'a str>,
    pub unit: Option<&'a str>,
}

/// The one move P-D-251 allows: `stored` is the proposed version, the SKU's unit does not change,
/// and [`wraps`] holds. A missing version is not a wrap.
#[must_use]
pub fn wrap_exception(
    current: RefUnit<'_>,
    proposed: RefUnit<'_>,
    stored: Option<&DerivedUsageDeclaration>,
) -> bool {
    if !wrap_candidate(current.usage_type_ref, proposed.usage_type_ref) {
        return false;
    }
    let (Some(current_ref), Some(unit)) = (current.usage_type_ref, named_unit(current.unit)) else {
        return false;
    };
    if named_unit(proposed.unit) != Some(unit) {
        return false;
    }
    stored.is_some_and(|declaration| wraps(current_ref, declaration, unit))
}

/// The pin refuses this change, unless it is the wrap [`wrap_exception`] allows.
#[must_use]
pub fn pin_refuses(current: Option<&str>, proposed: Option<&str>, wrap: bool) -> bool {
    // PROBE-W-4: derived → raw, and every other move, stays refused.
    pin_moves(current, proposed) && !wrap
}

/// The metering a published usage SKU's change is judged on (P-D-258).
#[domain_model]
#[derive(Clone, Copy)]
pub struct Metering<'a> {
    /// The SKU's type is usage.
    pub usage: bool,
    pub usage_type_ref: Option<&'a str>,
    pub unit: Option<&'a str>,
}

/// The field a published usage SKU's change moves, or `None` when the metering stays or the move
/// is the identity wrap (P-D-251).
///
/// `usage_type_ref` when the ref moves or the type leaves usage, and when both the ref and the
/// unit move. `unit` when only the unit moves. A non-usage SKU has no metering. [`pin_moves`] and
/// [`wrap_exception`] stay pure; the change door, submit and apply share this predicate.
#[must_use]
pub fn metering_moves(
    current: Metering<'_>,
    proposed: Metering<'_>,
    stored: Option<&DerivedUsageDeclaration>,
) -> Option<&'static str> {
    if !current.usage {
        return None;
    }
    // A type change away from usage drops the metering, so it is a ref move even when the strings
    // are left in place.
    if !proposed.usage {
        return Some("usage_type_ref");
    }
    let current_pair = RefUnit {
        usage_type_ref: current.usage_type_ref,
        unit: current.unit,
    };
    let proposed_pair = RefUnit {
        usage_type_ref: proposed.usage_type_ref,
        unit: proposed.unit,
    };
    // PROBE-9-13-3: the identity wrap is the one move this rule allows.
    if wrap_exception(current_pair, proposed_pair, stored) {
        return None;
    }
    // PROBE-9-13-1: a raw ref change is a move, as a derived one is.
    let ref_moved = current.usage_type_ref != proposed.usage_type_ref;
    let unit_moved = named_unit(current.unit) != named_unit(proposed.unit);
    if ref_moved {
        Some("usage_type_ref")
    } else if unit_moved {
        Some("unit")
    } else {
        None
    }
}

/// The metering rule's refusal: 400 [`METERING_IMMUTABLE`] on `field`.
#[must_use]
pub fn metering_immutable(field: &'static str) -> DomainError {
    let mut report = ValidationReport::new();
    report.violate(METERING_IMMUTABLE, field, METERING_DETAIL);
    DomainError::Validation(report)
}

/// What a [`METERING_IMMUTABLE`] refusal says, at submit and at apply.
pub const METERING_DETAIL: &str = "a published usage SKU keeps its usage type and its unit; \
     sell another meter through a new usage SKU";

#[cfg(test)]
#[path = "derived_tests.rs"]
mod derived_tests;
