//! Flat category authoring rules.
use crate::domain::validation::ValidationReport;
use toolkit_macros::domain_model;
/// Input for a new active category.
#[domain_model]
#[derive(Debug, Clone)]
pub struct NewCategory {
    pub code: String,
    pub name: String,
    pub is_default: bool,
    pub sort_order: i32,
}
/// The mutable category fields; its code is stable.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct CategoryPatch {
    pub name: Option<String>,
    pub is_default: Option<bool>,
    pub sort_order: Option<i32>,
}
/// Validate the required category identity and display name.
/// @cpt-cf-bss-products-fr-category-flat
#[must_use]
pub fn validate_new_category(new: &NewCategory) -> ValidationReport {
    let mut report = ValidationReport::new();
    if new.code.trim().is_empty() {
        report.violate("VALIDATION", "code", "code must not be blank");
    }
    if new.name.trim().is_empty() {
        report.violate("VALIDATION", "name", "name must not be blank");
    }
    crate::domain::caps::check(
        &mut report,
        "code",
        Some(&new.code),
        crate::domain::caps::CODE_MAX_CHARS,
    );
    crate::domain::caps::check(
        &mut report,
        "name",
        Some(&new.name),
        crate::domain::caps::NAME_MAX_CHARS,
    );
    report
}
#[cfg(test)]
#[path = "category_tests.rs"]
mod category_tests;
