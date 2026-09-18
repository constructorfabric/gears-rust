// Created: 2026-08-26 by Constructor Tech
//! Query options this gear declines across every listing.

use crate::domain::error::DomainError;

/// Refuse the `OData` options no listing here implements.
///
/// `$select` is parsed by the platform but not honoured: supporting it means a
/// response whose shape varies per request, and no caller has asked for one.
/// Refusing is deliberate rather than ignoring — a caller whose projection was
/// silently dropped receives every field believing it asked for two, which is
/// the same failure the declared filter surface exists to prevent.
///
/// Shared rather than restated per resource: two listings that answered
/// differently would be a difference no caller could predict, and the message
/// names the resource so the refusal is still specific.
///
/// # Errors
/// [`DomainError::Validation`] naming the unsupported option.
pub fn reject_unsupported_options(
    query: &toolkit_odata::ODataQuery,
    resource: &str,
) -> Result<(), DomainError> {
    if query.select.is_some() {
        return Err(DomainError::Validation {
            field: "$select".to_owned(),
            code: crate::field::ODATA_UNSUPPORTED_OPTION,
            message: format!(
                "$select is not supported on {resource}; omit it to receive the full \
                 representation"
            ),
        });
    }
    Ok(())
}
