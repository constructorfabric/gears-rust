//! The two driver-message classifiers, without a database.
//!
//! Both of them answer a **specific, caller-fixable refusal** — "that precedence
//! is taken", "that `line_id` is taken" — and both decide it from text the driver
//! prints. `storage::policy_guard_or_contention`'s doc argues at length why
//! message matching is the narrow case rather than a precedent, and the argument
//! rests on two things these cases pin: the typed unique class is asked **first**,
//! and the literals are DDL these chains own.
//!
//! The conjunct is the half no database suite can testify about. A real collision
//! is both unique *and* named, so the suites in `tests/sqlite_overlay_repo.rs`
//! (`a_second_published_overlay_on_one_precedence_is_refused`,
//! `a_line_id_already_taken_at_that_revision_is_a_typed_refusal`) stay green
//! whether the conjunct is there or not. What needs a case is the pair the
//! database cannot easily be made to produce: a failure that is **named but not
//! unique**, and one that is **unique but not named**.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use sea_orm::DbErr;
use toolkit_db::secure::ScopeError;
use uuid::Uuid;

use super::{is_line_identity_collision, line_of, precedence_held_or_db, sold_currency};
use crate::domain::money::CurrencyCode;
use crate::domain::overlay::{Adjustment, AdjustmentKind, AmountSet, Magnitude, MagnitudeKind};
use crate::infra::storage::RepoError;
use crate::infra::storage::entity::{price_overlay_line, price_overlay_line_amount};

/// A driver error carrying exactly this message and nothing the typed class can
/// be read from.
///
/// `DbErr::Custom` has no SQLSTATE, so `is_unique_violation` falls back to the
/// toolkit's own message test — the shared one, not a substring this crate
/// invented — which is precisely the seam these cases exercise.
fn driver_says(message: &str) -> ScopeError {
    ScopeError::Db(DbErr::Custom(message.to_owned()))
}

// ---------------------------------------------------------------------------
// The precedence slot.
// ---------------------------------------------------------------------------

/// Both renderings of the real refusal, one per backend.
///
/// Captured rather than invented: the `SQLite` form is what the mirror prints for
/// `uq_pricing_price_overlay_precedence` (which names the indexed columns, not the
/// index), the Postgres form is that server's documented `unique_violation`
/// message. A rename of the index that forgot this function reddens here.
#[test]
fn a_real_precedence_collision_is_the_precedence_refusal_on_both_backends() {
    for message in [
        "database error: Execution Error: error returned from database: duplicate key value \
         violates unique constraint \"uq_pricing_price_overlay_precedence\"",
        "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
         constraint failed: pricing_price_overlay.precedence",
    ] {
        assert!(
            matches!(
                precedence_held_or_db(&driver_says(message), "flip pricing_price_overlay"),
                RepoError::OverlayPrecedenceHeld
            ),
            "the real refusal must survive the conjunct: {message}"
        );
    }
}

/// **Named but not unique**: the conjunct's whole purpose.
///
/// A lock report naming the index is a storage fault the caller can do nothing
/// about, and answering it with "that precedence is held" sends them to change a
/// field that was never the problem. Without `is_unique_violation()` this was
/// classified by prose the driver is free to reword.
#[test]
fn a_failure_that_merely_names_the_precedence_index_is_not_the_precedence_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: deadlock detected; \
         process 8123 waits for ShareLock on index \"uq_pricing_price_overlay_precedence\"",
    );
    assert!(
        matches!(
            precedence_held_or_db(&err, "flip pricing_price_overlay"),
            RepoError::Db(ref detail) if detail.starts_with("flip pricing_price_overlay: ")
        ),
        "a non-unique failure is a storage failure, whatever index its text names"
    );
}

/// **Unique but not named**: the other index on this table.
///
/// `uq_pricing_price_overlay_open_draft` is a second partial unique index on
/// `pricing_price_overlay`, so "a unique index refused" cannot on its own mean the
/// precedence slot. Its loser is not told to pick another precedence.
#[test]
fn a_unique_violation_on_another_overlay_index_is_not_the_precedence_refusal() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
         constraint failed: pricing_price_overlay.tenant_id, \
         pricing_price_overlay.price_overlay_id",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        matches!(
            precedence_held_or_db(&err, "flip pricing_price_overlay"),
            RepoError::Db(_)
        ),
        "the open-draft index is not the precedence slot"
    );
}

// ---------------------------------------------------------------------------
// The line table's identity.
// ---------------------------------------------------------------------------

/// The line primary key's two renderings.
///
/// `SQLite` reports extended code `1555` for a primary-key violation rather than
/// `2067`, and `sea_orm` folds both into `UniqueConstraintViolation` — which is
/// what makes the conjunct free here rather than a narrowing.
#[test]
fn a_real_line_identity_collision_is_recognised_on_both_backends() {
    for message in [
        "database error: Execution Error: error returned from database: duplicate key value \
         violates unique constraint \"pricing_price_overlay_line_pkey\"",
        "database error: Execution Error: error returned from database: (code: 1555) UNIQUE \
         constraint failed: pricing_price_overlay_line.line_id",
    ] {
        assert!(
            is_line_identity_collision(&driver_says(message)),
            "the real collision must survive the conjunct: {message}"
        );
    }
}

/// Named but not unique, on the line table this time.
///
/// The caller-facing consequence is the sharper of the two: this refusal is
/// `ValueOutOfRange { field: "line_id" }`, which tells an author to drop a field
/// they supplied. A storage fault reported that way is a wrong instruction rather
/// than a generic one.
#[test]
fn a_failure_that_merely_names_the_line_primary_key_is_not_a_line_identity_collision() {
    assert!(
        !is_line_identity_collision(&driver_says(
            "database error: Execution Error: error returned from database: could not serialize \
             access due to concurrent update on index \"pricing_price_overlay_line_pkey\""
        )),
        "a serialization failure is not a caller-fixable line id"
    );
}

/// Unique but not named: the amounts table's own key.
#[test]
fn a_unique_violation_on_the_amount_table_is_not_a_line_identity_collision() {
    let err = driver_says(
        "database error: Execution Error: error returned from database: (code: 1555) UNIQUE \
         constraint failed: pricing_price_overlay_line_amount.line_id, \
         pricing_price_overlay_line_amount.currency",
    );
    assert!(err.is_unique_violation(), "the premise of this case");
    assert!(
        !is_line_identity_collision(&err),
        "the amount row's key is a different constraint, and the qualified table name is \
         what tells them apart"
    );
}

// ---------------------------------------------------------------------------
// The two stored kind columns, written by the domain and read back by it.
// ---------------------------------------------------------------------------

/// The write half of `write_lines`, isolated: the two columns as the producer
/// renders them.
///
/// **The tokens are never typed here.** `line_of`'s inverse used to be a
/// hand-written literal match, so a fixture that typed `"markup"` on both sides
/// would have proved the two literals equal and nothing about the round trip. The
/// row is rendered from the adjustment through the same producer
/// `overlay_repo::write_lines` uses, so a rename of a token moves this fixture
/// with it and the assertion still has to hold.
fn stored_row(adjustment: &Adjustment) -> price_overlay_line::Model {
    price_overlay_line::Model {
        line_id: Uuid::from_u128(0x_11),
        overlay_revision: 1,
        price_overlay_id: Uuid::from_u128(0x_0f),
        tenant_id: Uuid::from_u128(0x_7e),
        plan_id: None,
        target_sku: None,
        cohort: None,
        adjustment_kind: adjustment.kind().as_str().to_owned(),
        magnitude_kind: adjustment.magnitude_kind().as_str().to_owned(),
        adjustment_value: adjustment.percent_bp(),
    }
}

fn stored_amounts() -> Vec<price_overlay_line_amount::Model> {
    vec![price_overlay_line_amount::Model {
        line_id: Uuid::from_u128(0x_11),
        overlay_revision: 1,
        currency: "EUR".to_owned(),
        tenant_id: Uuid::from_u128(0x_7e),
        value_minor: 1200,
    }]
}

fn eur(minor: i64) -> AmountSet {
    AmountSet::new([(CurrencyCode::new("EUR").expect("three letters"), minor)])
}

/// Every adjustment this domain can express survives write-then-read unchanged.
///
/// The case Z2-4 found missing: `line_of` parsed both kind columns against
/// hand-written literals while `write_lines` rendered them through
/// `Adjustment::kind()`, and **no test drove the pair**. A renamed token would
/// have had the writer storing the new spelling and every read of a row it wrote
/// answering `RepoError::CorruptRow` — a perfectly legal row read back as a
/// corrupt one, which is the failure `read_token`'s own doc names.
///
/// The sample set is checked against `AdjustmentKind::ALL` and
/// `MagnitudeKind::ALL` below rather than left as a list somebody keeps in their
/// head.
#[test]
fn every_adjustment_survives_the_two_stored_kind_columns() {
    for adjustment in samples() {
        let round_tripped = line_of(&stored_row(&adjustment), &stored_amounts())
            .expect("a row this domain produced is a row this domain reads")
            .adjustment;
        assert_eq!(
            round_tripped, adjustment,
            "the read is the write's inverse, on every variant"
        );
    }
}

/// The samples cover every kind the domain declares, in **both** directions.
///
/// This is what makes the round trip above a census rather than four cases. A
/// kind added to `AdjustmentKind::ALL` or `MagnitudeKind::ALL` with no sample
/// producing it reddens here, so the round trip cannot silently stop covering the
/// vocabulary it claims to cover — and a kind added to the enum but *not* to
/// `ALL` cannot hide either, because `as_str` and `line_of`'s own `match` are
/// both exhaustive and stop compiling first.
#[test]
fn the_samples_produce_every_declared_kind() {
    let adjustment_kinds: BTreeSet<&str> = samples().iter().map(|a| a.kind().as_str()).collect();
    let magnitude_kinds: BTreeSet<&str> = samples()
        .iter()
        .map(|a| a.magnitude_kind().as_str())
        .collect();
    assert_eq!(
        adjustment_kinds,
        AdjustmentKind::ALL.iter().map(|k| k.as_str()).collect(),
        "every declared adjustment kind needs a round-tripped sample"
    );
    assert_eq!(
        magnitude_kinds,
        MagnitudeKind::ALL.iter().map(|k| k.as_str()).collect(),
        "every declared magnitude kind needs a round-tripped sample"
    );
}

/// One adjustment per `(adjustment_kind, magnitude_kind)` pairing the domain
/// admits. `fixed` is amount-only by construction (D-138), so there are five and
/// not six.
fn samples() -> Vec<Adjustment> {
    vec![
        Adjustment::Markup(Magnitude::PercentBp(1200)),
        Adjustment::Markup(Magnitude::Amount(eur(1200))),
        Adjustment::Discount(Magnitude::PercentBp(1200)),
        Adjustment::Discount(Magnitude::Amount(eur(1200))),
        Adjustment::Fixed(eur(1200)),
    ]
}

/// Every token parses back to the kind that rendered it, and no two kinds render
/// alike.
///
/// The property `read_token` rests on: it finds a candidate by comparing
/// `render(candidate)` against the stored string, so two kinds sharing a token
/// would make the first one win silently.
#[test]
fn every_kind_token_round_trips_and_no_two_collide() {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for kind in AdjustmentKind::ALL {
        assert_eq!(AdjustmentKind::parse(kind.as_str()), Some(*kind));
        assert!(
            seen.insert(kind.as_str()),
            "two kinds render {}",
            kind.as_str()
        );
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for kind in MagnitudeKind::ALL {
        assert_eq!(MagnitudeKind::parse(kind.as_str()), Some(*kind));
        assert!(
            seen.insert(kind.as_str()),
            "two kinds render {}",
            kind.as_str()
        );
    }
    assert_eq!(AdjustmentKind::parse("mark_up"), None);
    assert_eq!(MagnitudeKind::parse("percentBp"), None);
}

/// **A stored currency the domain cannot read is a corrupt row, not a market that
/// does not exist** (review 2026-08-19).
///
/// `price_facts` swallowed the parse failure with `if let Ok(..)`, and the market
/// it dropped is the only input to `ADJUSTMENT_CURRENCY_NOT_COVERED`: an overlay
/// carrying no value in that currency then passed the coverage requirement, because
/// the requirement no longer knew the plan sold it. Nothing else in the crate reads
/// `OverlayWorld::sold_currencies`, so no second rule catches the omission.
///
/// The positive control is the half that matters here: a refusal that refused
/// everything would also make the case above pass, and would take every overlay
/// publish with it.
#[test]
fn a_stored_currency_the_domain_cannot_read_is_a_corrupt_row() {
    let price_id = uuid::Uuid::from_u128(0x_c0_de);

    // Two characters, which `varchar(3)` permits on Postgres and `SQLite` does not
    // constrain at all, while `CurrencyCode::new` demands exactly three letters.
    let err = sold_currency(price_id, "US").expect_err("two characters is not a code");
    assert!(
        matches!(&err, RepoError::CorruptRow(detail)
            if detail.contains("pricing_price.currency") && detail.contains(&price_id.to_string())),
        "the refusal names the column and the row: {err}"
    );

    // Non-alphabetic, the other shape the column admits.
    assert!(matches!(
        sold_currency(price_id, "U$D"),
        Err(RepoError::CorruptRow(_))
    ));

    // The control: an ordinary market still reads.
    assert_eq!(
        sold_currency(price_id, "USD")
            .expect("a well-formed code")
            .as_str(),
        "USD"
    );
}
