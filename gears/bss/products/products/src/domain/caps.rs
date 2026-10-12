//! The explicit length caps of the text a request writes (P-D-225, twin of pricing D-457), counted
//! in characters (Unicode scalar values), as a submitter's note always was (P-D-219). Only a write
//! is judged, and only on the fields it carries: a stored row over a cap stays readable.
use crate::domain::validation::ValidationReport;

/// A code: a SKU's and a category's.
pub const CODE_MAX_CHARS: usize = 64;
/// A SKU's and a category's name.
pub const NAME_MAX_CHARS: usize = 200;
/// A description, a note or a reason: a SKU's description, the operator's release reason, and a
/// submitter's or a vote's note (the approval engine's cap).
pub const NOTE_MAX_CHARS: usize = 2000;
/// A GL code, a tax category and a metering unit.
pub const LABEL_MAX_CHARS: usize = 64;
/// An invoice line template.
pub const TEMPLATE_MAX_CHARS: usize = 2000;
/// A usage-type reference, a GTS id.
pub const USAGE_TYPE_REF_MAX_CHARS: usize = 512;

// The gear's note cap is the one the approval engine judges a vote's note by, and the submit
// doors' (P-D-219).
const _: () = assert!(NOTE_MAX_CHARS == bss_approval::NOTE_MAX_CHARS);
const _: () = assert!(NOTE_MAX_CHARS == crate::domain::approvals::NOTE_MAX_CHARS);

/// Whether `text` is longer than `max` characters.
#[must_use]
pub fn over(text: &str, max: usize) -> bool {
    text.chars().count() > max
}

/// Records 400 `FIELD_TOO_LONG` on `field` when the carried `text` is longer than `max`
/// characters; a text the write does not carry is not judged.
pub fn check(report: &mut ValidationReport, field: &str, text: Option<&str>, max: usize) {
    if text.is_some_and(|t| over(t, max)) {
        report.violate(
            "FIELD_TOO_LONG",
            field,
            format!("{field} is at most {max} characters"),
        );
    }
}
