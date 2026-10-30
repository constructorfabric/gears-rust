//! Book content and half-open sale validity.
use super::{RuleError, currency::CurrencyScales};
use time::Date;

#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub name: String,
    pub currency: String,
    pub valid_from: Option<Date>,
    pub valid_until: Option<Date>,
}
/// Validate a new book's content. Code uniqueness belongs to the repository.
///
/// The currency must be one `scales` resolves: an ISO 4217 code or a configured non-ISO one.
#[must_use]
pub fn validate(book: &Book, scales: &CurrencyScales) -> Vec<RuleError> {
    rules(book, scales.resolve(&book.currency).is_some())
}
/// Validate the content of a book whose currency cannot change (a patch). The currency is judged
/// by its shape only, never against today's table: a book stored before the table changed stays
/// editable. Code uniqueness belongs to the repository.
#[must_use]
pub fn validate_shape(book: &Book) -> Vec<RuleError> {
    rules(book, currency_code(&book.currency))
}
/// The content rules, with the verdict on the currency already made.
fn rules(book: &Book, currency_known: bool) -> Vec<RuleError> {
    let mut errors = Vec::new();
    if book.name.trim().is_empty() {
        errors.push(RuleError::new("BOOK_NAME_REQUIRED"));
    }
    if !currency_known {
        errors.push(RuleError::new("BOOK_CURRENCY_INVALID"));
    }
    if matches!((book.valid_from,book.valid_until),(Some(a),Some(b)) if a>=b) {
        errors.push(RuleError::new("BOOK_VALIDITY_INVALID"));
    }
    errors
}
/// Whether `text` is spelled as a currency code: three uppercase ASCII letters. The shape only;
/// whether pricing knows the code is [`CurrencyScales::resolve`].
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
#[cfg(test)]
#[path = "book_tests.rs"]
mod tests;
