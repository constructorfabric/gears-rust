//! The **millisecond quantum** every authored instant is expressed at (D-144).
//!
//! The gear's treatment of time fixed the zone and left the resolution open —
//! "all effective dating, window boundaries, `grandfatherUntil`,
//! `availableFrom`/`availableTo` and anchor math are UTC" — and an unquantized
//! axis is not a stylistic gap. `cohort` **is** a cutover instant and is matched
//! for **equality** across a gear boundary against a window bound another gear
//! produced (`design/07-pricewindow-linkage.md` §5, D-126): two instants denoting
//! the same moment at different resolutions are not equal, so the generation
//! becomes unfindable by exactly the subscribers grandfathering exists to
//! protect.
//!
//! A finer instant is therefore **refused, never truncated**. Truncation is what
//! an unstated quantum degenerates into, and it moves the instant a scope-key
//! axis, a window bound and an approval-time floor are all derived from — a
//! truncating producer and a non-truncating consumer agree until the day they do
//! not, with no failure in between.
//!
//! The rule is over instants the gear **authors, carries in a contract field,
//! publishes or compares**. Storage bookkeeping an operator never authors —
//! `created_at`, audit-chain and outbox timestamps — is outside it: none of it
//! enters a contract field and none of it is compared with anything.
//!
//! This is [`crate::domain::money`]'s temporal sibling and is shaped like it on
//! purpose: a predicate for callers that only have to decide, and a checked form
//! that names the code for callers that have to refuse.

use chrono::{DateTime, Timelike, Utc};

use crate::domain::error::DomainError;

/// Nanoseconds in the quantum. One millisecond, as `chrono` counts subsecond
/// precision.
const QUANTUM_NANOS: u32 = 1_000_000;

/// Is `at` expressible at the quantum — i.e. does it carry no precision below
/// one millisecond?
///
/// A leap second lands in the 1s-2s nanosecond range `chrono` reserves for it;
/// the remainder is taken over the same nanosecond field either way, so a leap
/// second authored at whole milliseconds is expressible and one authored with
/// microseconds is not, exactly as any other instant.
#[must_use]
pub fn is_quantized(at: DateTime<Utc>) -> bool {
    at.nanosecond().is_multiple_of(QUANTUM_NANOS)
}

/// The authoring-time form of [`is_quantized`].
///
/// `field` is the authored field the instant arrived on (`grandfatherUntil`,
/// `availableFrom`, `cohort`), so the author corrects one value instead of
/// resubmitting a request and guessing which of its instants was refused.
///
/// # Errors
///
/// [`DomainError::TimestampPrecisionExceeded`] when `at` carries precision finer
/// than one millisecond.
pub fn check_quantum(field: &str, at: DateTime<Utc>) -> Result<(), DomainError> {
    if is_quantized(at) {
        return Ok(());
    }
    Err(DomainError::TimestampPrecisionExceeded(format!(
        "{field} {} is finer than the millisecond quantum",
        at.to_rfc3339()
    )))
}

#[cfg(test)]
#[path = "instant_tests.rs"]
mod instant_tests;
