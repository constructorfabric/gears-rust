//! The domain→row conversion, without a store.
//!
//! `row_of` is the only writer of `pricing_approval_threshold`'s two value columns,
//! and the number it writes has to be the number
//! `domain::approval::content_pin::put_threshold_basis` framed into the digest the
//! approval was opened against — `effective_version_at` re-derives that digest from
//! the version it reads **back out of the store** and will not accept a version whose
//! content hash it cannot find approved. A conversion that quietly changed the value
//! therefore did not store a wrong threshold; it stored an unapprovable one.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::row_of;
use crate::domain::error::DomainError;
use crate::domain::materiality::{ThresholdBasis, ThresholdEntry};
use crate::domain::money::CurrencyCode;

fn entry(basis: ThresholdBasis) -> ThresholdEntry {
    ThresholdEntry {
        currency: CurrencyCode::new("USD").expect("a well-formed code"),
        basis,
    }
}

/// **A percent the column cannot hold is refused, not saturated** (review
/// 2026-08-19).
///
/// `i32::try_from(bp).unwrap_or(i32::MAX)` was justified by `MAX_PERCENT_BP`, which
/// is declared and enforced only in `api::rest::threshold_policy::parse_entries`.
/// `ThresholdVersion::new` carries no such bound and `ThresholdService::propose` is a
/// public `infra` entry point taking `Vec<ThresholdEntry>`, so a caller past the wire
/// had nothing holding it — and a saturated row can never match the digest the
/// approval unit was opened against, wedging the version out of ever being found
/// approved.
#[test]
fn a_percent_the_column_cannot_hold_is_refused_rather_than_saturated() {
    let err = row_of(&entry(ThresholdBasis::Percent { bp: u32::MAX }))
        .expect_err("u32::MAX bp does not fit the percent_bp column");
    assert!(
        matches!(&err, DomainError::ThresholdInvalid(detail)
            if detail.contains(&u32::MAX.to_string()) && detail.contains("percent_bp")),
        "the refusal names the value and the column it does not fit: {err}"
    );

    // The exact edge, so the boundary is pinned rather than merely a large number
    // being refused: `i32::MAX` is the last value the column holds.
    let last = u32::try_from(i32::MAX).expect("i32::MAX is non-negative");
    assert_eq!(
        row_of(&entry(ThresholdBasis::Percent { bp: last }))
            .expect("the column's last value")
            .percent_bp,
        Some(i32::MAX)
    );
    assert!(row_of(&entry(ThresholdBasis::Percent { bp: last + 1 })).is_err());
}

/// The value that reaches the column **is** the value the digest framed.
///
/// The positive control on the refusal above, and the property it protects: for
/// every percent the column can hold, `row_of` is the identity on the number.
#[test]
fn a_percent_the_column_can_hold_reaches_it_unchanged() {
    for bp in [0_u32, 1, 50, 2_500, 10_000, 1_000_000] {
        let row = row_of(&entry(ThresholdBasis::Percent { bp })).expect("within range");
        assert_eq!(
            row.percent_bp,
            Some(i32::try_from(bp).expect("within range"))
        );
        assert_eq!(row.absolute_minor, None, "exactly one column per basis");
    }
}

/// The absolute basis is unaffected — it was never narrowed, and the new `Result`
/// must not have made it fallible in practice.
#[test]
fn an_absolute_basis_still_writes_one_column_and_cannot_be_refused() {
    for minor in [0_i64, 1, 100_000, i64::MAX] {
        let row = row_of(&entry(ThresholdBasis::Absolute { minor })).expect("always representable");
        assert_eq!(row.absolute_minor, Some(minor));
        assert_eq!(row.percent_bp, None, "exactly one column per basis");
        assert_eq!(row.currency, "USD");
    }
}
