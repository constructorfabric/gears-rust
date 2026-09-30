// Created: 2026-09-24 by Virtuozzo International GmbH
//! The clock the repositories stamp rows with.
//!
//! Every state tag this service hands out is a row timestamp — `updated_at`,
//! `last_change_at` — rendered as nanoseconds. Two things follow. The stamp is
//! minted **once per mutation**, so the columns a mutation writes agree. And
//! it is minted at **microsecond** precision, the precision every backend
//! keeps: a `timestamptz` drops what is finer, so a row in memory and its
//! reload would otherwise carry different tags, and a tag built from a
//! nanosecond the store never kept would never match a comparison against the
//! stored row. `SQLite` keeps what it is given, which is why tests never see the
//! difference.
//!
//! A conditional write stamps **strictly after** the version it replaces: two
//! writes inside one microsecond would otherwise yield one tag, and a third
//! writer holding it would pass the comparison against a row that had moved.

use time::{Duration, OffsetDateTime};

/// Now, UTC, at whole microseconds.
#[must_use]
pub fn now() -> OffsetDateTime {
    align(OffsetDateTime::now_utc())
}

/// The stamp of a write replacing the row version `expected`: now, or one
/// microsecond past `expected` when the clock has not moved past it — so the
/// tag changes with the row, always. Without a version, plain [`now`].
#[must_use]
pub fn stamp_after(expected: Option<OffsetDateTime>) -> OffsetDateTime {
    let at = now();
    match expected {
        Some(previous) if at <= previous => previous + Duration::microseconds(1),
        _ => at,
    }
}

fn align(at: OffsetDateTime) -> OffsetDateTime {
    let micros = at.nanosecond() - at.nanosecond() % 1_000;
    at.replace_nanosecond(micros).unwrap_or(at)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod clock_tests;
