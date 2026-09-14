//! The four scope-value taxonomies against a real database —
//! `design/04-currency-tax.md` §6, D-120.
//!
//! These tables are **Slice 4's** and are built here because `inst-plv-scope`
//! validates every non-`global` overlay scope value against its declared
//! universe, and before this commit none of the four universes existed anywhere
//! in the crate. A validation rule whose universe is absent is the D-211 defect
//! (`inst-bc-coverage` delegating to a `CurrencyBindingChecker` no crate
//! declares), so the alternative to building them was shipping the rule against
//! thin air. Slice 4 inherits them unchanged.
//!
//! Everything proved here is a property of the **schema**, never of a branch in
//! Rust: the composite primary key, the `state` enumeration, and the two columns
//! §6 declares **region-only**. The suite drives raw SQL through
//! `common::migrated_db` for the reason every schema suite in this crate does —
//! a repository that writes only valid values catches a constraint that got
//! narrower and never one that stopped refusing.
//!
//! Postgres mirrors every rule here; the roster is pinned by name in
//! `tests/postgres_migrations.rs` and the refusals are executed in
//! `tests/postgres_schema_taxonomy.rs`, because a measurement on one engine is
//! not a fact about the other.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const OTHER_TENANT: &str = "99999999-9999-9999-9999-999999999999";

/// The four tables, in the order §6 declares them.
///
/// A slice rather than four copies of every test: the shape is **one** shape,
/// and the whole argument for building the `partner` / `orgTier` pair (D-120)
/// is that it is the region/brand shape with nothing added. A rule proved on one
/// of them and merely assumed on the other three is exactly the drift this
/// arrangement exists to make impossible.
const TAXONOMIES: &[&str] = &[
    "pricing_region_taxonomy",
    "pricing_brand_taxonomy",
    "pricing_partner_taxonomy",
    "pricing_org_tier_taxonomy",
];

/// The two `tax_*` columns §6 puts on the **region** taxonomy only (D-01).
const REGION: &str = "pricing_region_taxonomy";

/// Reject, **and** for the stated reason.
///
/// The fragment is the constraint or index name, never the table name: a
/// fragment naming the table is satisfied by `no such table`, which is what a
/// suite written before its migration would assert against forever.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .expect_err(&format!("this statement must be rejected: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

fn insert(table: &str, tenant: &str, value: &str, state: &str) -> String {
    format!(
        "INSERT INTO {table} (tenant_id, value, display_name, state) \
         VALUES ('{tenant}', '{value}', 'A label', '{state}')"
    )
}

// ---------------------------------------------------------------------------
// The shape all four share.
// ---------------------------------------------------------------------------

/// Each of the four exists and is keyed `(tenant_id, value)`.
#[tokio::test]
async fn each_taxonomy_is_keyed_by_tenant_and_value() {
    let conn = migrated_db().await;
    for table in TAXONOMIES {
        must_succeed(&conn, &insert(table, TENANT, "eu-west", "active")).await;
        // The same value under a different tenant is a different row: the key
        // is tenant-scoped, which is what makes one operator's `gold` tier
        // independent of another's.
        must_succeed(&conn, &insert(table, OTHER_TENANT, "eu-west", "active")).await;

        let state = scalar(
            &conn,
            &format!(
                "SELECT state AS v FROM {table} \
                 WHERE tenant_id = '{TENANT}' AND value = 'eu-west'"
            ),
        )
        .await;
        assert_eq!(state, "active", "{table} must store the authored state");
    }
}

/// One value per tenant, in each of the four.
#[tokio::test]
async fn a_value_is_declared_at_most_once_per_tenant() {
    let conn = migrated_db().await;
    for table in TAXONOMIES {
        must_succeed(&conn, &insert(table, TENANT, "eu-west", "active")).await;
        must_be_rejected(
            &conn,
            &insert(table, TENANT, "eu-west", "retired"),
            &format!("UNIQUE constraint failed: {table}.tenant_id, {table}.value"),
        )
        .await;
    }
}

/// `state` is `active | retired` and nothing else.
///
/// The pair is the whole state machine §6 gives these tables — retirement is
/// guarded rather than cascading, and re-activation is a legal audited move —
/// so a third token would be a state no rule in the design set describes.
#[tokio::test]
async fn a_state_outside_the_declared_pair_is_refused() {
    let conn = migrated_db().await;
    for table in TAXONOMIES {
        must_be_rejected(
            &conn,
            &insert(table, TENANT, "eu-west", "deprecated"),
            &format!("chk_{table}_state"),
        )
        .await;
    }
}

/// A blank value is refused in each of the four.
///
/// Not decoration: `inst-plv-scope` validates an overlay's `scope_value` against
/// these tables, and `pricing_price_overlay` renders the **absence** of a scope
/// value as the empty string (the `global` class). A blank taxonomy value would
/// make that sentinel forgeable — a `brand` overlay carrying `''` would validate
/// against a declared universe *and* read as classless.
#[tokio::test]
async fn a_blank_value_is_refused() {
    let conn = migrated_db().await;
    for table in TAXONOMIES {
        must_be_rejected(
            &conn,
            &insert(table, TENANT, "", "active"),
            &format!("chk_{table}_value_present"),
        )
        .await;
    }
}

/// And so is a value that is only whitespace — D-242 (`pricing_region_taxonomy`).
///
/// A separate case rather than another value in the loop above, because it
/// closes a **different** hole and the two would otherwise read as one. The
/// empty string is the classless sentinel `pricing_price_overlay` renders, and
/// `'   '` is not that sentinel, so nothing here was forgeable. What `'   '`
/// broke is one level over: `ScopeValue::new` trims before deciding, so the
/// domain refused a row the store had admitted, and `TaxonomyRepo::list` mapped
/// it to `RepoError::CorruptRow` — one such row failed `GET` for every value in
/// its class, with `PUT` unable to round-trip a list it could not read.
///
/// This is the engine parity half: the admission was measured on **both**
/// engines, so the tightening is pinned on both.
#[tokio::test]
async fn a_whitespace_only_value_is_refused() {
    let conn = migrated_db().await;
    for table in TAXONOMIES {
        must_be_rejected(
            &conn,
            &insert(table, TENANT, "   ", "active"),
            &format!("chk_{table}_value_present"),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// The two columns that are the region's alone (D-01).
// ---------------------------------------------------------------------------

/// The region taxonomy carries `tax_category` and `tax_rate_present`.
#[tokio::test]
async fn the_region_taxonomy_carries_the_two_tax_columns() {
    let conn = migrated_db().await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO {REGION} (tenant_id, value, display_name, state, \
                                   tax_category, tax_rate_present) \
             VALUES ('{TENANT}', 'eu-west', 'EU West', 'active', 'standard', 1)"
        ),
    )
    .await;

    let category = scalar(
        &conn,
        &format!(
            "SELECT tax_category AS v FROM {REGION} \
             WHERE tenant_id = '{TENANT}' AND value = 'eu-west'"
        ),
    )
    .await;
    assert_eq!(category, "standard");
}

/// `tax_rate_present` defaults to **false**, which is the fail-closed reading of
/// `RegionTaxReadiness`: a region nobody has declared a rate for is a region with
/// no rate, not a region with an unknown one.
#[tokio::test]
async fn tax_rate_present_defaults_to_false() {
    let conn = migrated_db().await;
    must_succeed(&conn, &insert(REGION, TENANT, "eu-west", "active")).await;

    let present = scalar(
        &conn,
        &format!(
            "SELECT CAST(tax_rate_present AS text) AS v FROM {REGION} \
             WHERE tenant_id = '{TENANT}' AND value = 'eu-west'"
        ),
    )
    .await;
    assert_eq!(present, "0", "an undeclared region has no rate");
}

/// The other three do **not** carry them — §6 says "region-only" and this is
/// what that sentence is worth if it is true of the schema.
///
/// Asserted by a statement that must fail to compile as SQL at all, which is the
/// only way to prove a column's absence: a `SELECT` of a column that is not
/// there is refused by the parser rather than by a constraint.
#[tokio::test]
async fn the_other_three_taxonomies_carry_no_tax_columns() {
    let conn = migrated_db().await;
    for table in TAXONOMIES.iter().filter(|t| **t != REGION) {
        must_be_rejected(
            &conn,
            &format!("SELECT tax_category AS v FROM {table}"),
            "no such column: tax_category",
        )
        .await;
        must_be_rejected(
            &conn,
            &format!("SELECT tax_rate_present AS v FROM {table}"),
            "no such column: tax_rate_present",
        )
        .await;
    }
}
