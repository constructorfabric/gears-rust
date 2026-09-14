//! The two cross-table guards on `pricing_price_tier_band`, plus its per-row
//! constraints, proven against a real database.
//!
//! Neither guard is a row CHECK: **structural exclusivity** reads the parent's
//! `model_kind` and **append-only-with-the-parent** reads the parent's
//! `lifecycle_state`, and a row CHECK may not read another table. Both are
//! therefore triggers, and a trigger that is silently wrong is a trigger that
//! never fires — so each branch gets a case here, alongside the moves that are
//! *supposed* to work, so this proves a rule rather than a blanket ban.
//!
//! Structural exclusivity has **two ends**, and the last case in this file is
//! the second one: the child-side arms judge a band as it arrives, and the
//! parent-side arm — a trigger on `pricing_price` this table's migration
//! installs — refuses a row that still carries bands becoming a kind that
//! carries none. Without it the forbidden pair is reachable by moving the
//! parent instead of the band, and the row is left in it.
//!
//! Postgres carries both as PL/pgSQL functions; `SQLite` mirrors them as
//! fixed-message `RAISE(ABORT, ...)` triggers whose parent lookup is a
//! `WHERE NOT EXISTS` subquery in the body, so the guards are exercisable
//! without Docker.
//!
//! Ordering and contiguity are deliberately absent from this file: they are
//! sequence properties the `TierBandValidator` owns at publish, not database
//! constraints, and a test here would be testing something that does not exist.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const PLAN: &str = "22222222-2222-2222-2222-222222222222";
const PHASE: &str = "33333333-3333-3333-3333-333333333333";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";
/// A `graduated` parent — the kind that may carry bands.
const TIERED: &str = "55555555-5555-5555-5555-555555555555";
/// A `flat` parent — the kind that may not.
const UNTIERED: &str = "66666666-6666-6666-6666-666666666666";
/// A `volume` parent — the *other* kind that may. Present because the rule is
/// `model_kind IN ('graduated','volume')` and a suite that only ever exercised
/// `graduated` would pass with the second literal dropped or misspelled on
/// either backend.
const TIERED_VOLUME: &str = "77777777-7777-7777-7777-777777777777";

/// Reject, **and** for the stated reason.
///
/// The fragment is not decoration. This table carries twelve rejectors — five
/// `SQLite` triggers of its own, three CHECKs, a UNIQUE, a `band_id` PRIMARY
/// KEY, a foreign key, and the parent-side kind trigger its migration puts on
/// `pricing_price` — and several of them can fire on the same statement. An
/// INSERT alone passes under the kind trigger, the append-only trigger, the PK
/// and the FK. A test that accepted any error would pass with the guard it
/// means to prove switched off; a band insert refused by the FK says nothing at
/// all about structural exclusivity, and one refused by the kind trigger says
/// nothing about the freeze.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the band guard must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("pricing_price_tier_band"),
        "the rejection must name the guard it came from, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// One parent per model kind under test — `graduated`, `volume` and `flat` —
/// all draft, on distinct charge kinds so nothing depends on the scope-key
/// index.
async fn seed_parents(conn: &DatabaseConnection) {
    for (price_id, charge_kind, model_kind, amount) in [
        (TIERED, "usage", "graduated", "NULL"),
        (TIERED_VOLUME, "one_time", "volume", "NULL"),
        (UNTIERED, "recurring", "flat", "1000"),
    ] {
        must_succeed(
            conn,
            &format!(
                "INSERT INTO pricing_price (
                    price_id, tenant_id, plan_id, currency, region, phase,
                    charge_kind, amount_minor, model_kind, lifecycle_state,
                    created_by, created_at_utc)
                 VALUES ('{price_id}', '{TENANT}', '{PLAN}', 'USD', 'EU', '{PHASE}',
                    '{charge_kind}', {amount}, '{model_kind}', 'draft', '{ACTOR}',
                    '2026-08-02 10:00:00 +00:00')"
            ),
        )
        .await;
    }
}

/// `from_qty` and `unit_price_nano` are **signed**, and deliberately.
///
/// Typed `u32`, no statement this file can build reaches
/// `chk_pricing_price_tier_band_from_qty` or
/// `chk_pricing_price_tier_band_unit_price` - both `>= 0` - so the module doc's
/// claim that each branch gets a case costs two of them their case by construction
/// of the helper rather than by omission.
fn insert_band(band_id: &str, price_id: &str, from_qty: i64, to_qty: &str, unit: i64) -> String {
    format!(
        "INSERT INTO pricing_price_tier_band (
            band_id, tenant_id, price_id, from_qty, to_qty, unit_price_nano)
         VALUES ('{band_id}', '{TENANT}', '{price_id}', {from_qty}, {to_qty}, {unit})"
    )
}

/// The two `>= 0` CHECKs, each refused below its floor and accepted at it.
///
/// Zero is the boundary both admit: a band starting at 0 is the first band of
/// every ladder, and a `unit_price_nano` of 0 is a free tier - real authoring
/// shapes, so the floor has to be inclusive and a case has to say so.
#[tokio::test]
async fn the_two_quantity_floors_refuse_below_zero_and_admit_it() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    must_be_rejected(
        &conn,
        &insert_band(
            "aaaaaaa9-0000-0000-0000-000000000001",
            TIERED,
            -1,
            "100",
            50,
        ),
        "chk_pricing_price_tier_band_from_qty",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_band("aaaaaaa9-0000-0000-0000-000000000002", TIERED, 0, "100", -1),
        "chk_pricing_price_tier_band_unit_price",
    )
    .await;

    must_succeed(
        &conn,
        &insert_band("aaaaaaa9-0000-0000-0000-000000000003", TIERED, 0, "100", 0),
    )
    .await;
}

#[tokio::test]
async fn bands_are_permitted_only_on_a_tiered_parent() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    // Both halves of `model_kind IN ('graduated','volume')`, so neither literal
    // can be dropped without a failure here.
    must_succeed(
        &conn,
        &insert_band("aaaaaaa1-0000-0000-0000-000000000001", TIERED, 0, "100", 50),
    )
    .await;
    must_succeed(
        &conn,
        &insert_band(
            "aaaaaaa1-0000-0000-0000-000000000002",
            TIERED_VOLUME,
            0,
            "100",
            50,
        ),
    )
    .await;

    // A `flat` row's bands would be priced by nothing and read by nothing — a
    // silent mispricing, indistinguishable from a correct row until an invoice
    // is wrong. The kind flip that would otherwise be the way to reach this
    // state is already impossible on a published row, so the case is driven
    // through a second parent rather than an UPDATE of the first.
    must_be_rejected(
        &conn,
        &insert_band(
            "aaaaaaa1-0000-0000-0000-000000000003",
            UNTIERED,
            0,
            "100",
            50,
        ),
        "permitted only on a graduated or volume price row",
    )
    .await;

    let bands = scalar(
        &conn,
        "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_price_tier_band",
    )
    .await;
    assert_eq!(bands, "2", "only the two tiered parents' bands landed");
}

#[tokio::test]
async fn a_lower_bound_identifies_a_band_and_cannot_repeat() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    must_succeed(
        &conn,
        &insert_band("aaaaaaa2-0000-0000-0000-000000000001", TIERED, 0, "100", 50),
    )
    .await;
    // The band set carries no ordinal, so `from_qty` is the identity. Two bands
    // starting at the same quantity would make "the band at 0" ambiguous, and
    // the geometry rules read the set sorted by exactly this column.
    must_be_rejected(
        &conn,
        &insert_band("aaaaaaa2-0000-0000-0000-000000000002", TIERED, 0, "200", 40),
        "UNIQUE constraint failed",
    )
    .await;
}

#[tokio::test]
async fn a_band_covers_a_quantity_or_is_open_at_the_top() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    // A band that covers nothing is always an authoring mistake and it makes
    // the set's contiguity ambiguous.
    must_be_rejected(
        &conn,
        &insert_band(
            "aaaaaaa3-0000-0000-0000-000000000001",
            TIERED,
            100,
            "100",
            50,
        ),
        "chk_pricing_price_tier_band_width",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_band(
            "aaaaaaa3-0000-0000-0000-000000000002",
            TIERED,
            100,
            "40",
            50,
        ),
        "chk_pricing_price_tier_band_width",
    )
    .await;

    // NULL is the open top — a state of the band, not an absent value.
    must_succeed(
        &conn,
        &insert_band(
            "aaaaaaa3-0000-0000-0000-000000000003",
            TIERED,
            100,
            "NULL",
            50,
        ),
    )
    .await;

    let top = scalar(
        &conn,
        "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_price_tier_band WHERE to_qty IS NULL",
    )
    .await;
    assert_eq!(top, "1", "the open-topped band is the one that landed");
}

#[tokio::test]
async fn a_band_freezes_when_its_parent_does() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    let mutable = "aaaaaaa4-0000-0000-0000-000000000001";
    let doomed = "aaaaaaa4-0000-0000-0000-000000000002";
    must_succeed(&conn, &insert_band(mutable, TIERED, 0, "100", 50)).await;
    must_succeed(&conn, &insert_band(doomed, TIERED, 100, "NULL", 40)).await;

    // While the parent is draft the band set is authoring material.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price_tier_band SET unit_price_nano = 45 WHERE band_id = '{mutable}'"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!("DELETE FROM pricing_price_tier_band WHERE band_id = '{doomed}'"),
    )
    .await;

    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'published' WHERE price_id = '{TIERED}'"
        ),
    )
    .await;

    // Bands carry no `lifecycle_state` of their own; the parent's is the
    // referent. Without this the band set of a frozen row could be rewritten
    // under an unchanged `pricing_price`, and the projector's warm re-drive —
    // which reads truth rows — would re-materialize the same `CatalogVersion`
    // at different money.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_tier_band SET unit_price_nano = 1 WHERE band_id = '{mutable}'"
        ),
        "UPDATE of a band under a non-draft price row is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_price_tier_band WHERE band_id = '{mutable}'"),
        "DELETE of a band under a non-draft price row is not permitted",
    )
    .await;

    // And the verb that ADDS money to a frozen row. The kind trigger fires on
    // INSERT but reads only `model_kind`, which is still `graduated` here — so
    // without the INSERT arm this statement would land a new priced band under
    // a published row. The fragment is what distinguishes the arm under test
    // from the kind trigger and the foreign key, either of which could also
    // refuse an INSERT into this table.
    must_be_rejected(
        &conn,
        &insert_band(
            "aaaaaaa4-0000-0000-0000-000000000003",
            TIERED,
            200,
            "NULL",
            30,
        ),
        "INSERT of a band under a non-draft price row is not permitted",
    )
    .await;

    let price = scalar(
        &conn,
        &format!(
            "SELECT CAST(unit_price_nano AS TEXT) AS v FROM pricing_price_tier_band \
             WHERE band_id = '{mutable}'"
        ),
    )
    .await;
    assert_eq!(price, "45", "no rejected statement may have landed");
    let bands = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_price_tier_band \
             WHERE price_id = '{TIERED}'"
        ),
    )
    .await;
    assert_eq!(bands, "1", "the frozen row's band set did not grow");
}

#[tokio::test]
async fn a_re_pointed_band_is_judged_against_its_new_parents_kind() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    let band = "aaaaaaa5-0000-0000-0000-000000000001";
    must_succeed(&conn, &insert_band(band, TIERED, 0, "100", 50)).await;

    // The kind rule cares only about where a band ends up, so it reads
    // `NEW.price_id`. Moving this band onto the `flat` parent is the only way
    // to observe that choice: nothing else in the suite distinguishes it from
    // `OLD.price_id`, and both parents are draft, so the append-only arms stay
    // out of the way.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_tier_band SET price_id = '{UNTIERED}' WHERE band_id = '{band}'"
        ),
        "permitted only on a graduated or volume price row",
    )
    .await;

    // A move onto the other tiered kind is a legitimate authoring edit.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price_tier_band SET price_id = '{TIERED_VOLUME}' \
             WHERE band_id = '{band}'"
        ),
    )
    .await;

    let parent = scalar(
        &conn,
        &format!("SELECT price_id AS v FROM pricing_price_tier_band WHERE band_id = '{band}'"),
    )
    .await;
    assert_eq!(parent, TIERED_VOLUME, "only the legal move landed");
}

#[tokio::test]
async fn a_parent_that_still_carries_bands_may_not_leave_the_tiered_kinds() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    let band = "aaaaaaa7-0000-0000-0000-000000000001";
    must_succeed(&conn, &insert_band(band, TIERED, 0, "100", 50)).await;

    // The other end of structural exclusivity, and the one the child-side arms
    // cannot see: they judge a band as it arrives, and this row's bands arrived
    // legally. Flipping the parent reaches the forbidden pair — a `flat` row
    // with priced bands nothing reads and nothing applies — from the side
    // nothing was watching, and leaves it there.
    //
    // The statement is raw SQL against a **draft** parent on purpose: a draft is
    // where `model_kind` is still movable at all, so this is the only reachable
    // shape of the defect, and `PriceRepo` is not the only thing that can issue
    // an UPDATE.
    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET model_kind = 'flat' WHERE price_id = '{TIERED}'"),
        "may not leave the graduated or volume kinds",
    )
    .await;
    // Kindless is the same fault: `model_kind` is nullable because a draft may
    // be authored before its kind is, and a NULL kind reads bands nowhere.
    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET model_kind = NULL WHERE price_id = '{TIERED}'"),
        "may not leave the graduated or volume kinds",
    )
    .await;

    // The moves that must still work, without which the guard would be a ban.
    // Between the two tiered kinds the bands stay meaningful.
    must_succeed(
        &conn,
        &format!("UPDATE pricing_price SET model_kind = 'volume' WHERE price_id = '{TIERED}'"),
    )
    .await;
    // A non-kind edit of the same banded row is untouched by the rule.
    must_succeed(
        &conn,
        &format!("UPDATE pricing_price SET row_version = 1 WHERE price_id = '{TIERED}'"),
    )
    .await;
    // And a row with no bands may become anything its own CHECK admits — which
    // is also the state an authoring edit reaches by replacing the band set
    // before it moves the row.
    must_succeed(
        &conn,
        &format!("DELETE FROM pricing_price_tier_band WHERE band_id = '{band}'"),
    )
    .await;
    must_succeed(
        &conn,
        &format!("UPDATE pricing_price SET model_kind = 'flat' WHERE price_id = '{TIERED}'"),
    )
    .await;

    let kind = scalar(
        &conn,
        &format!("SELECT model_kind AS v FROM pricing_price WHERE price_id = '{TIERED}'"),
    )
    .await;
    assert_eq!(kind, "flat", "only the legal moves landed");
}

#[tokio::test]
async fn a_band_may_not_be_re_pointed_onto_a_frozen_parent() {
    let conn = migrated_db().await;
    seed_parents(&conn).await;

    let band = "aaaaaaa6-0000-0000-0000-000000000001";
    must_succeed(&conn, &insert_band(band, TIERED, 0, "100", 50)).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'published' \
             WHERE price_id = '{TIERED_VOLUME}'"
        ),
    )
    .await;

    // Re-pointing is how you would append a band to a frozen set without ever
    // issuing an INSERT: the band's own parent is draft, so the OLD-side arm
    // permits the edit, and the target is `volume`, so the kind rule permits it
    // too. Only reading the prospective parent's `lifecycle_state` refuses it.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_tier_band SET price_id = '{TIERED_VOLUME}' \
             WHERE band_id = '{band}'"
        ),
        "UPDATE of a band under a non-draft price row is not permitted",
    )
    .await;

    let parent = scalar(
        &conn,
        &format!("SELECT price_id AS v FROM pricing_price_tier_band WHERE band_id = '{band}'"),
    )
    .await;
    assert_eq!(parent, TIERED, "the band stayed on its draft parent");
}
