//! Usage-type binding and meter declaration checks.

use crate::domain::error::DomainError;

pub use bss_products_sdk::usage_types::{UsageTypeAnswer, UsageTypeBinding};

/// What a door resolved a SKU's `usage_type_ref` to before its transaction (P-D-184, P-D-232). A
/// GTS ref is the usage-type catalog's answer. A derived ref (`products.derived/<code>@<n>`) is the
/// tenant's stored version, read from this gear's own store: the catalog is never asked for one.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageRefAnswer {
    /// A GTS ref, as the usage-type catalog answered it.
    Catalog(UsageTypeAnswer),
    /// A derived ref the tenant holds.
    Derived(crate::domain::derived::DerivedPin),
    /// A derived ref the tenant does not hold: no such code or version, or not canonical.
    DerivedUnknown,
}

/// The atomic-pair rule (`inst-mt-atomic-pair`, `dod-meter-atomic`): the
/// resulting row carries `metering_unit` and `usage_type_ref` together or
/// not at all. The paired `CHECK` refuses the same shape at the physical
/// layer; this is the door's half, with the code the taxonomy names.
///
/// # Errors
///
/// [`DomainError::MeterDeclarationIncomplete`].
pub fn meter_pair_complete(
    metering_unit: Option<&str>,
    usage_type_ref: Option<&str>,
) -> Result<(), DomainError> {
    if metering_unit.is_some() == usage_type_ref.is_some() {
        return Ok(());
    }
    let (present, absent) = if metering_unit.is_some() {
        ("metering_unit", "usage_type_ref")
    } else {
        ("usage_type_ref", "metering_unit")
    };
    Err(DomainError::MeterDeclarationIncomplete(format!(
        "a MeterDeclaration is atomic: {present} arrived without {absent}, and the pair travels \
         together or not at all"
    )))
}

#[cfg(test)]
#[path = "recognized_tests.rs"]
mod recognized_tests;
