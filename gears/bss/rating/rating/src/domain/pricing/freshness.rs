//! The freshness proof (DECISIONS T-D-87; Subscriptions PR #5516 `features/09` §3.2 step 4): does
//! a `resolve` answer fetched on or after the UTC day of a window's last second still name the
//! accepted binding the fact version carries for the window's slice?
//!
//! The binding is fresh when the answer's cell for the same item and dimension value is bound to
//! the same price (`price_id`, `price_book_entry_id`, `money_digest`) and any `ends_on` it reports
//! does not end the price inside the window except where the fact cuts a slice. An `ends_on` is
//! inside `selected_bindings_digest` but outside `money_digest` (pricing D-425), so a later end of
//! the same price changes the answer's digest without changing the money a window before it is
//! priced at: that window stays fresh. Anything else is stale, and the child waits
//! `pending(terms_stale)`: Rating never prices a successor the fact does not carry (SUB-D-41).

use bss_rating_core::engine::current::input::CoreCell;
use time::{Date, OffsetDateTime, Time};
use toolkit_macros::domain_model;

use crate::domain::fact::model::AcceptedTerm;
use crate::domain::pricing::StoredAnswer;

/// Why a binding is not fresh for a window.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stale {
    /// The answer has no binding for the cell: the price no longer covers it on the date.
    Uncovered,
    /// The answer binds the cell to another price or other money.
    BindingChanged,
    /// The price ends inside the window on a day the fact does not cut at.
    EndsInsideWindow {
        /// The price's own end.
        ends_on: Date,
    },
}

/// What the proof found.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The answer still names the fact's binding for the window.
    Fresh,
    /// It does not; the child waits for a new fact version.
    Stale(Stale),
}

/// Whether `answer` still names `term`'s binding for a window ending at `window_end`, given the
/// instants the fact version cuts its slices at.
#[must_use]
pub fn prove(
    term: &AcceptedTerm,
    answer: &StoredAnswer,
    window_end: OffsetDateTime,
    cuts: &[OffsetDateTime],
) -> Freshness {
    let accepted = &term.binding;
    let Some(bound) = answer
        .cells
        .iter()
        .find(|cell| is_cell_of(cell, accepted.item_id, accepted.dimension_value.as_deref()))
        .and_then(|cell| cell.binding.as_ref())
    else {
        return Freshness::Stale(Stale::Uncovered);
    };
    if bound.price.price_id != accepted.price.price_id
        || bound.price.price_book_entry_id != accepted.price.price_book_entry_id
        || bound.price.money_digest != accepted.price.money_digest
    {
        return Freshness::Stale(Stale::BindingChanged);
    }
    if let Some(ends_on) = bound.price.ends_on {
        let end = ends_on.with_time(Time::MIDNIGHT).assume_utc();
        if end < window_end && !cuts.contains(&end) {
            return Freshness::Stale(Stale::EndsInsideWindow { ends_on });
        }
    }
    Freshness::Fresh
}

fn is_cell_of(cell: &CoreCell, item_id: uuid::Uuid, dimension_value: Option<&str>) -> bool {
    cell.selection.item_id == item_id
        && cell.selection.dimension_value.as_deref() == dimension_value
}

#[cfg(test)]
#[path = "freshness_tests.rs"]
mod tests;
