//! Book content and half-open sale validity.
use super::RuleError;
use time::Date;

#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub name: String,
    pub currency: String,
    pub valid_from: Option<Date>,
    pub valid_until: Option<Date>,
}
/// Validate content. Code uniqueness belongs to the repository.
#[must_use]
pub fn validate(book: &Book) -> Vec<RuleError> {
    let mut errors = Vec::new();
    if book.name.trim().is_empty() {
        errors.push(RuleError::new("BOOK_NAME_REQUIRED"));
    }
    if !currency_code(&book.currency) {
        errors.push(RuleError::new("BOOK_CURRENCY_INVALID"));
    }
    if matches!((book.valid_from,book.valid_until),(Some(a),Some(b)) if a>=b) {
        errors.push(RuleError::new("BOOK_VALIDITY_INVALID"));
    }
    errors
}
/// Whether `text` is spelled as a currency code: three uppercase ASCII letters. The shape only —
/// the workspace holds no ISO 4217 list — and the one rule for a book's currency and for the
/// tenant settings' offered currencies (D-438).
#[must_use]
pub fn currency_code(text: &str) -> bool {
    text.len() == 3 && text.bytes().all(|b| b.is_ascii_uppercase())
}
/// The most characters a book's description holds (D-444): a note's cap (D-457).
pub const DESCRIPTION_MAX_CHARS: usize = super::caps::NOTE_MAX_CHARS;
/// A book's description: at most [`DESCRIPTION_MAX_CHARS`] characters (Unicode scalar values).
/// # Errors
/// `BOOK_DESCRIPTION_TOO_LONG` for a longer one.
pub fn validate_description(text: Option<&str>) -> Result<(), RuleError> {
    if text.is_some_and(|t| t.chars().count() > DESCRIPTION_MAX_CHARS) {
        Err(RuleError::new("BOOK_DESCRIPTION_TOO_LONG"))
    } else {
        Ok(())
    }
}
/// Whether sales from this book are allowed on a date.
#[must_use]
pub fn valid_on(book: &Book, date: Date) -> bool {
    book.valid_from.is_none_or(|start| date >= start)
        && book.valid_until.is_none_or(|end| date < end)
}
/// Fractional digits of an ISO 4217 currency; two unless the standard says otherwise.
#[must_use]
pub fn minor_digits(currency: &str) -> u32 {
    match currency {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "CLF" | "UYW" => 4,
        _ => 2,
    }
}
#[cfg(test)]
#[path = "book_tests.rs"]
mod tests;
