//! The wire `$select` string, from a query or the `:batchGet` body.

use toolkit::api::odata;
use toolkit_canonical_errors::CanonicalError;

use super::error::select_refused;
use crate::domain::selection::{FieldSelection, SelectionError};

/// `ToolKit`'s parser drops empty comma segments, so `content,,kind` is checked
/// on the raw text.
///
/// # Errors
/// A `400` naming `$select`.
pub fn check_raw(raw: &str) -> Result<(), CanonicalError> {
    let raw = raw.trim();
    if !raw.is_empty() && raw.split(',').any(|segment| segment.trim().is_empty()) {
        return Err(select_refused(&SelectionError::EmptySegment));
    }
    Ok(())
}

/// `None` is the default set.
///
/// # Errors
/// A `400` naming `$select` for any [`SelectionError`].
pub fn from_names(names: Option<&[String]>) -> Result<FieldSelection, CanonicalError> {
    names.map_or_else(
        || Ok(FieldSelection::default()),
        |names| FieldSelection::parse(names).map_err(|e| select_refused(&e)),
    )
}

/// `ToolKit`'s limits first, then the gear's rules.
///
/// # Errors
/// A `400` naming `$select`.
pub fn parse(raw: Option<&str>) -> Result<FieldSelection, CanonicalError> {
    let Some(raw) = raw else {
        return Ok(FieldSelection::default());
    };
    let names = odata::parse_select(raw)?;
    check_raw(raw)?;
    from_names(Some(&names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::selection::EntityField;

    fn select_violation(error: CanonicalError) -> (String, String) {
        let problem = toolkit_canonical_errors::Problem::from(error);
        let violation = &problem.context["field_violations"][0];
        (
            violation["field"].as_str().unwrap_or_default().to_owned(),
            violation["reason"].as_str().unwrap_or_default().to_owned(),
        )
    }

    #[test]
    fn absent_is_the_default_selection() {
        assert_eq!(parse(None).ok(), Some(FieldSelection::default()));
    }

    #[test]
    fn a_valid_selection_is_normalized() {
        let parsed = parse(Some(" Content , gts_id ")).expect("valid");
        assert!(parsed.contains(EntityField::Content));
        assert_eq!(
            parsed.canonical(),
            "content,gts_id,gts_uuid,kind,lifecycle_status"
        );
    }

    #[test]
    fn every_refusal_names_the_select_parameter() {
        let too_long = format!("content,{}", "x".repeat(odata::MAX_SELECT_LEN));
        let too_many = vec!["content"; odata::MAX_SELECT_FIELDS + 1].join(",");
        for raw in [
            "",
            "   ",
            ",",
            "content,,kind",
            ",content",
            "content,",
            "content,CONTENT",
            "contents",
            "availability",
            "content.title",
            too_long.as_str(),
            too_many.as_str(),
        ] {
            let error = parse(Some(raw)).expect_err(raw);
            let (field, reason) = select_violation(error);
            assert_eq!(field, "$select", "{raw}");
            assert_eq!(reason, "INVALID_SELECT", "{raw}");
        }
    }
}
