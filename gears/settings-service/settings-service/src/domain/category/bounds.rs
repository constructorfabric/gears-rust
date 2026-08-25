// Created: 2026-08-25 by Constructor Tech
//! Length bounds on a category's descriptive fields.
//!
//! `name` and `description` are `varchar(256)` and `varchar(4096)` in the schema
//! (DESIGN.md §4.1, §4.7). Without a check here an over-long value reaches the
//! driver, and `map_write_error` classifies anything that is not a uniqueness
//! violation as internal — so a caller who typed too much would receive `500`
//! for a mistake that is entirely theirs to fix. `SQLite`, whose columns are
//! unbounded `text`, would instead store it, giving the dev stack and Postgres
//! two different answers to one request.
//!
//! Separate from [`super::key`] because these are presentation fields: their
//! bounds come from the column, not from the setting-key grammar.

use crate::domain::error::DomainError;
use crate::field;

/// Inclusive bounds on a category name, in characters.
const NAME_MIN: usize = 1;
const NAME_MAX: usize = 256;

/// Upper bound on a category description, in characters.
const DESCRIPTION_MAX: usize = 4096;

/// The request field a name violation is reported against.
pub const NAME_FIELD: &str = "name";

/// The request field a description violation is reported against.
pub const DESCRIPTION_FIELD: &str = "description";

/// Validate the descriptive fields a caller supplied.
///
/// # Errors
///
/// [`DomainError::Validation`] naming whichever field fell outside its bound.
/// `name` is checked first, so a request that breaks both is reported against
/// the field a caller is likelier to have gotten wrong.
pub fn validate(name: &str, description: Option<&str>) -> Result<(), DomainError> {
    // Characters, not bytes — the same rule the key bound follows. A name in a
    // non-Latin script would otherwise be refused for a length its author never
    // sees, and Postgres counts `varchar` in characters too, so counting bytes
    // here would also disagree with the column it is protecting.
    let length = name.chars().count();
    if !(NAME_MIN..=NAME_MAX).contains(&length) {
        return Err(DomainError::Validation {
            field: NAME_FIELD.to_owned(),
            code: field::CATEGORY_NAME_LENGTH,
            message: format!(
                "a category name is {NAME_MIN} to {NAME_MAX} characters; this one is {length}"
            ),
        });
    }

    // An absent description and an empty one are both fine: the column is
    // nullable and the bound is an upper one only.
    if let Some(description) = description {
        let length = description.chars().count();
        if length > DESCRIPTION_MAX {
            return Err(DomainError::Validation {
                field: DESCRIPTION_FIELD.to_owned(),
                code: field::CATEGORY_DESCRIPTION_LENGTH,
                message: format!(
                    "a category description is at most {DESCRIPTION_MAX} characters; \
                     this one is {length}"
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "bounds_tests.rs"]
mod bounds_tests;
