//! The one rule for windows: a window covers from `not_before` to its end,
//! grace included.

use license_enforcement_sdk::models::Window;
use time::OffsetDateTime;

/// The end of `w`, grace included.
#[must_use]
pub fn end(w: &Window) -> OffsetDateTime {
    w.grace_until.unwrap_or(w.expires_at)
}

/// `outer` covers all of `inner`.
#[must_use]
pub fn inside(outer: &Window, inner: &Window) -> bool {
    outer.not_before <= inner.not_before && end(inner) <= end(outer)
}
