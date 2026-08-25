//! `pricing_price` and `pricing_price_tier_band`, proved by **executing the
//! statement each object must refuse**, on Postgres.
//!
//! # Why this suite exists
//!
//! A Phase-2 review of `pricing_price` found that **fourteen** of this
//! table's CHECK constraints could each be replaced with `CHECK (1 = 1)` with
//! the whole crate green. Nothing was broken: the repository writes only legal
//! values, so every test that reached these columns reached them through a
//! writer that could not produce an illegal one. A suite built that way catches
//! a constraint that got *narrower* — the writer starts failing — and never one
//! that stopped refusing.
//!
//! `tests/postgres_migrations.rs` closed half of the gap by pinning the CHECK,
//! trigger and partial-index rosters **by name**, so a constraint cannot vanish
//! unnoticed. It issues no DML, so it says the objects reached the server and
//! nothing about what any of them does. This suite is the other half for these
//! two tables: one executed refusal per object, and the assertion names the
//! object the refusal came from.
//!
//! # The three rules every test here follows
//!
//! **Execute the refusal.** A test that writes valid values is not evidence
//! about a guard.
//!
//! **Put the world in the state where the object under test is what answers.**
//! A refusal an *earlier* guard produced is not evidence about the guard the
//! test names. This table makes the hazard concrete: its `CHECK`s all share one
//! row, and several of them fire on the same illegal value. `package_size = 0`
//! trips `chk_pricing_price_package_size` only on a row whose `model_kind` is
//! already `package`; on any other kind
//! `chk_pricing_price_package_fields_kind` answers first and the test would be
//! green while saying nothing about the constraint it names. Every refusal
//! below is therefore an otherwise-**valid** row with exactly one column moved.
//!
//! **Assert the object, never the table.** Every CHECK, index and trigger over
//! these two tables has `pricing_price` in its name, as does the column list
//! Postgres prints for a unique violation. A test that accepted any error
//! naming the table would pass with the guard it means to prove switched off.
//!
//! # Positives are load-bearing
//!
//! Every guard here is a whitelist rather than a blanket ban, so the suite
//! carries the accepting cases too: the valid row lands, a draft row is
//! deletable, `published -> superseded` is taken, a `grandfather_until` may be
//! *tightened*, a draft and its published predecessor share a scope key, and a
//! band set with gaps, overlaps and a closed top is stored without complaint.
//! Without those a table nothing can be written to at all would pass.
//!
//! # Two objects this suite deliberately does not test by refusal
//!
//! `idx_pricing_price_plan` and `idx_pricing_price_supersedes` are **non-unique**
//! indexes — the second partial. A non-unique index refuses nothing; its only
//! observable effect is on plan choice, which is not a correctness property and
//! would make a brittle test. Their presence is pinned by name in
//! `tests/postgres_migrations.rs`, and that is the whole of what can be said
//! about them here.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_price -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use std::collections::BTreeSet;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const PLAN: &str = "22222222-2222-2222-2222-222222222222";
const PHASE: &str = "33333333-3333-3333-3333-333333333333";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";

const DRAFT: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const PUBLISHED: &str = "aaaaaaaa-0000-0000-0000-000000000002";
const SUPERSEDED: &str = "aaaaaaaa-0000-0000-0000-000000000003";
const OTHER: &str = "aaaaaaaa-0000-0000-0000-000000000004";

/// A grandfathering cohort token. Any value other than `none` puts the row on
/// the `existing_grandfathered` side of the biconditional.
const COHORT: &str = "2026-01-01T00:00:00Z";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A fresh database carrying the applied chain, on the one shared server.
///
/// **One** container for the whole binary, and a `CREATE DATABASE` per test.
/// This suite is where that idiom was measured — fifty-two simultaneous
/// `docker run`s made the daemon the flakiest thing in the run, with sporadic
/// `PortNotExposed { port: Tcp(5432) }` panics in whichever tests happened to be
/// starting — and it now lives in `tests/pg_support/mod.rs`, shared with the two
/// suites it was propagated to, so the three cannot drift apart.
///
/// The connection handed back is a **plain** one: every statement this suite
/// issues is raw SQL that deliberately reaches past every repository, because
/// the repository is exactly the layer that cannot see a guard stop refusing.
async fn applied() -> DatabaseConnection {
    Pg::applied().await.raw().await
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    conn.execute_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .map(|_| ())
}

/// Run one statement that must land.
async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

/// Reject, **and by the named object**.
///
/// See the module doc: the fragment is the whole assertion, because every guard
/// over these two tables names the table too.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, by: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the guard `{by}` must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains(by),
        "the rejection must be the one under test (`{by}`), got: {message}"
    );
}

// ---------------------------------------------------------------------------
// Row builders
// ---------------------------------------------------------------------------

/// A minimal **valid** draft price row: a `flat`, `recurring`, ungrandfathered
/// row on the base overlay.
///
/// Every refusal below is this row with exactly one column moved, which is what
/// makes each of them a fact about the constraint it names rather than about
/// whichever neighbour happened to answer first.
fn base_row(id: &str) -> Vec<(String, String)> {
    [
        ("price_id", format!("'{id}'")),
        ("tenant_id", format!("'{TENANT}'")),
        ("plan_id", format!("'{PLAN}'")),
        ("currency", "'USD'".to_owned()),
        ("region", "'EU'".to_owned()),
        ("phase", format!("'{PHASE}'")),
        ("charge_kind", "'recurring'".to_owned()),
        ("model_kind", "'flat'".to_owned()),
        ("amount_minor", "1000".to_owned()),
        ("lifecycle_state", "'draft'".to_owned()),
        ("created_by", format!("'{ACTOR}'")),
        ("created_at_utc", "'2026-08-03 09:00:00+00'".to_owned()),
    ]
    .into_iter()
    .map(|(column, value)| (column.to_owned(), value))
    .collect()
}

/// `INSERT` of [`base_row`] with the named columns replaced or added.
fn insert(id: &str, overrides: &[(&str, &str)]) -> String {
    let mut columns = base_row(id);
    for (name, value) in overrides {
        match columns.iter_mut().find(|(column, _)| column == name) {
            Some(slot) => (*value).clone_into(&mut slot.1),
            None => columns.push(((*name).to_owned(), (*value).to_owned())),
        }
    }
    let names = columns
        .iter()
        .map(|(column, _)| column.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO bss.pricing_price ({names}) VALUES ({values})")
}

// ---------------------------------------------------------------------------
// The world: what `pricing_price` accepts
// ---------------------------------------------------------------------------

/// The valid rows, first. Without this every refusal below would pass against a
/// table that refuses everything.
///
/// One per lifecycle state the price-row machine has, because
/// `chk_pricing_price_lifecycle_state` is deliberately **narrower** than the
/// `LifecycleState` enum that renders it (no `retired`, no `abandoned`), and a
/// suite that only inserted drafts would leave two thirds of the admitted set
/// unexercised. Distinct regions keep the three off one scope key.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_state_the_price_row_machine_reaches_is_storable() {
    let conn = applied().await;
    must_succeed(&conn, &insert(DRAFT, &[])).await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[("lifecycle_state", "'published'"), ("region", "'US'")],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            SUPERSEDED,
            &[("lifecycle_state", "'superseded'"), ("region", "'APAC'")],
        ),
    )
    .await;
}

/// The grandfathered shape, which four constraints have to agree about at once:
/// a cohort token, the `existing_grandfathered` class, and a horizon.
///
/// It is here as a world rather than as a refusal because the horizon and the
/// cohort tests below each move one of these three columns, and without a stored
/// legal combination those tests would be consistent with a table that refuses
/// the whole shape.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_grandfathered_row_with_a_cohort_and_a_horizon_is_storable() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            DRAFT,
            &[
                ("price_eligibility", "'existing_grandfathered'"),
                ("cohort", &format!("'{COHORT}'")),
                ("grandfather_until", "'2026-12-01 00:00:00+00'"),
            ],
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// The `pricing_price` CHECK constraints
// ---------------------------------------------------------------------------

/// Neither free-form scope-key axis may carry the scope key's separator character.
///
/// The loader refuses a `|` in either, and until the table refuses it too that is
/// a rule one door holds: any other writer reaching this table stores a `meter` of
/// `p|q`, whose rendered scope key then has twelve segments where every reader
/// counts on ten, and collides with a genuinely different key. `region` is `NOT
/// NULL` and `meter` is nullable, which is why the two constraints are spelled
/// differently and why both are asserted rather than one standing for the pair.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn neither_free_form_key_axis_admits_the_separator() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("region", "'eu|west'")]),
        "chk_pricing_price_region_no_separator",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("meter", "'api|calls'")]),
        "chk_pricing_price_meter_no_separator",
    )
    .await;
}

/// A `NULL` meter is not a separator, and the nullable constraint must say so.
///
/// `meter NOT LIKE '%|%'` alone answers `NULL` for a row with no meter, which a
/// `CHECK` admits — but a reader meeting the constraint cannot tell that from an
/// oversight, and a later tightening to `NOT NULL` would change the rule silently.
/// The `meter IS NULL OR` disjunct is written out, and this is what holds it there.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_row_with_no_meter_passes_the_separator_rule() {
    let conn = applied().await;
    must_succeed(&conn, &insert(DRAFT, &[("meter", "NULL")])).await;
}

/// The price-row machine has three states and the shared enum has five.
///
/// A row in `retired` or `abandoned` would fall outside **both** partial UNIQUE
/// predicates below, so the one-current-row-per-key guarantee would simply stop
/// covering it and the key would take a second published row beside it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_lifecycle_state_outside_the_price_row_machine_is_refused() {
    let conn = applied().await;
    for state in ["'retired'", "'abandoned'", "'archived'"] {
        must_be_rejected(
            &conn,
            &insert(DRAFT, &[("lifecycle_state", state)]),
            "chk_pricing_price_lifecycle_state",
        )
        .await;
    }
}

/// D-42's overlay column exists and admits exactly one value today.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_price_overlay_other_than_base_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("price_overlay", "'promo'")]),
        "chk_pricing_price_overlay",
    )
    .await;
}

/// Three eligibility classes, not two: `new_subscriptions_only` is normative in
/// its own right (D-78 / D-132) and sits between the other two.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_eligibility_class_outside_the_three_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("price_eligibility", "'legacy_only'")]),
        "chk_pricing_price_eligibility",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_charge_kind_outside_the_four_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("charge_kind", "'discount'")]),
        "chk_pricing_price_charge_kind",
    )
    .await;
}

/// The kind set, and the row carries no package fields — so
/// `chk_pricing_price_package_fields_kind`, which also mentions `model_kind`,
/// has nothing to object to and this constraint is the only thing that can
/// answer.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_model_kind_outside_the_five_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("model_kind", "'tiered'")]),
        "chk_pricing_price_model_kind",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_billing_timing_outside_advance_and_arrears_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("billing_timing", "'on_signup'")]),
        "chk_pricing_price_billing_timing",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_amount_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("amount_minor", "-1")]),
        "chk_pricing_price_amount_non_negative",
    )
    .await;
    // Zero is not negative, and a free line is a real one.
    must_succeed(&conn, &insert(DRAFT, &[("amount_minor", "0")])).await;
}

/// `amount_minor`'s case one column over, on the column that after D-311 **is** a
/// `per_unit` row's price.
///
/// It had no executed refusal anywhere: the constraint's name was carried by two
/// migration-roster literals, both schema goldens and three comments, and no test
/// attempted a violating write. Those pin the constraint's *text*, which is the
/// half an author regenerates in the same edit that breaks it; this pins that the
/// column still refuses. A negative rate is the fault that matters — the scale is
/// 10^-9, so a sign error here is a credit on every metered line the row prices.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_unit_rate_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("unit_rate_nano", "-1")]),
        "chk_pricing_price_unit_rate_nano",
    )
    .await;
    // The accepting half, and the reason `-1` is the discriminator rather than a
    // presence check: zero is a rate a `per_unit` row may legitimately carry, and
    // a guard that refused it would be a different, wrong constraint passing this
    // test's refusal arm.
    must_succeed(&conn, &insert(DRAFT, &[("unit_rate_nano", "0")])).await;
}

/// The floor is one, not zero: a hold of zero granules holds nothing.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_max_hold_granules_below_one_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("max_hold_granules", "0")]),
        "chk_pricing_price_max_hold_granules",
    )
    .await;
    must_succeed(&conn, &insert(DRAFT, &[("max_hold_granules", "1")])).await;
}

/// Both rate columns refuse a negative, and both admit zero.
///
/// D-311 gave a `per_unit` row's money a column of its own and the reservation
/// flavour a second. The two are read back through the same `RateMinor`, so a
/// floor on one of them is half a rule — and the half that is missing is the one
/// whose rows go unreadable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn neither_rate_column_admits_a_negative() {
    const RATES: [(&str, &str); 2] = [
        ("reserved_rate_nano", "chk_pricing_price_reserved_rate_nano"),
        ("unit_rate_nano", "chk_pricing_price_unit_rate_nano"),
    ];

    let conn = applied().await;
    for (column, constraint) in RATES {
        must_be_rejected(&conn, &insert(DRAFT, &[(column, "-1")]), constraint).await;
    }
    // Zero is a real rate — a line that prices at nothing — so both bounds are
    // `>= 0`, and a case offering only `-1` would pass against `> 0` as well.
    must_succeed(
        &conn,
        &insert(
            DRAFT,
            &[("reserved_rate_nano", "0"), ("unit_rate_nano", "0")],
        ),
    )
    .await;
}

/// Neither minimum-quantity column admits a negative.
///
/// `manual_quantity` one rule up carries this floor; these two are read back
/// through the same conversion into the count the domain holds.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn neither_minimum_quantity_column_admits_a_negative() {
    const MINIMA: [(&str, &str); 2] = [
        ("min_qty_purchase", "chk_pricing_price_min_qty_purchase"),
        ("min_qty_usage", "chk_pricing_price_min_qty_usage"),
    ];

    let conn = applied().await;
    for (column, constraint) in MINIMA {
        must_be_rejected(&conn, &insert(DRAFT, &[(column, "-1")]), constraint).await;
    }
    // A minimum of zero is no minimum at all, which is a real shape.
    must_succeed(
        &conn,
        &insert(DRAFT, &[("min_qty_purchase", "0"), ("min_qty_usage", "0")]),
    )
    .await;
}

/// The entity tag counts up from zero, and zero is where every row starts.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_entity_tag_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("row_version", "-1")]),
        "chk_pricing_price_row_version",
    )
    .await;
    // `> 0` would refuse every row this gear creates.
    must_succeed(&conn, &insert(DRAFT, &[("row_version", "0")])).await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_quantity_source_outside_the_two_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("quantity_source", "'metered'")]),
        "chk_pricing_price_quantity_source",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_manual_quantity_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            DRAFT,
            &[("quantity_source", "'manual'"), ("manual_quantity", "-1")],
        ),
        "chk_pricing_price_manual_quantity",
    )
    .await;
}

/// A package block of zero units prices nothing.
///
/// The row is `model_kind = 'package'` on purpose: on any other kind
/// `chk_pricing_price_package_fields_kind` answers first, and the test would be
/// green while proving nothing about the constraint it names. This is the
/// concrete instance of the module doc's second rule.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_zero_or_negative_package_size_is_refused() {
    let conn = applied().await;
    for size in ["0", "-10"] {
        must_be_rejected(
            &conn,
            &insert(
                DRAFT,
                &[
                    ("model_kind", "'package'"),
                    ("package_size", size),
                    ("package_price_minor", "100"),
                ],
            ),
            "chk_pricing_price_package_size",
        )
        .await;
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_package_price_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            DRAFT,
            &[
                ("model_kind", "'package'"),
                ("package_size", "10"),
                ("package_price_minor", "-1"),
            ],
        ),
        "chk_pricing_price_package_price",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_billing_granularity_outside_the_five_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("billing_granularity", "'per_week'")]),
        "chk_pricing_price_billing_granularity",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_aggregation_function_outside_the_three_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("aggregation_function", "'average'")]),
        "chk_pricing_price_aggregation_function",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_aggregation_granularity_outside_hour_and_day_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("aggregation_granularity", "'minute'")]),
        "chk_pricing_price_aggregation_granularity",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_tier_aggregation_window_outside_the_four_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("tier_aggregation_window", "'weekly'")]),
        "chk_pricing_price_tier_aggregation_window",
    )
    .await;
}

/// D-40's window, whose *values* the store already constrains even though the
/// Slice-10 rules that read them are unbuilt.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_tier_qualification_window_outside_the_two_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("tier_qualification_window", "'rolling'")]),
        "chk_pricing_price_tier_qualification_window",
    )
    .await;
}

/// The package half of §6's structural-exclusivity rule, **including the
/// kindless arm the migration's own doc argues is the whole constraint**.
///
/// `model_kind` is nullable, so on a kindless row the shorter spelling
/// `model_kind = 'package'` evaluates to NULL, `FALSE OR NULL` is NULL, and both
/// engines count a NULL CHECK result as satisfied — admitting exactly the row
/// the rule exists to refuse. The second case below is what would catch a
/// "simplification" back to the short form; the first alone would not.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn package_fields_on_a_non_package_row_are_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            DRAFT,
            &[
                ("model_kind", "'flat'"),
                ("package_size", "10"),
                ("package_price_minor", "100"),
            ],
        ),
        "chk_pricing_price_package_fields_kind",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            DRAFT,
            &[
                ("model_kind", "NULL"),
                ("package_size", "10"),
                ("package_price_minor", "100"),
            ],
        ),
        "chk_pricing_price_package_fields_kind",
    )
    .await;
    // And the legal shape, so this is an exclusivity rule and not a ban.
    must_succeed(
        &conn,
        &insert(
            DRAFT,
            &[
                ("model_kind", "'package'"),
                ("package_size", "10"),
                ("package_price_minor", "100"),
            ],
        ),
    )
    .await;
}

/// A biconditional, refused in **both** directions.
///
/// One-sided tests are the recurring way this constraint class rots: a
/// `CHECK (cohort = 'none' OR price_eligibility = 'existing_grandfathered')`
/// would pass the first case below and admit the second, and the cohort axis
/// would stop meaning "a retained generation".
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_cohort_eligibility_biconditional_is_refused_in_both_directions() {
    let conn = applied().await;
    // A cohort without the class.
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("cohort", &format!("'{COHORT}'"))]),
        "chk_pricing_price_cohort_eligibility",
    )
    .await;
    // The class without a cohort.
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("price_eligibility", "'existing_grandfathered'")]),
        "chk_pricing_price_cohort_eligibility",
    )
    .await;
    // `new_subscriptions_only` pairs with `cohort = 'none'` like
    // `all_subscriptions` does — it retains nobody — so the biconditional is
    // unaffected by the third class.
    must_succeed(
        &conn,
        &insert(DRAFT, &[("price_eligibility", "'new_subscriptions_only'")]),
    )
    .await;
}

/// Only a grandfathered row may carry a horizon.
///
/// The row below keeps `cohort = 'none'`, which keeps
/// `chk_pricing_price_cohort_eligibility` satisfied — otherwise that neighbour
/// answers and this constraint is never reached.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_grandfathering_horizon_on_an_ungrandfathered_row_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(DRAFT, &[("grandfather_until", "'2026-12-01 00:00:00+00'")]),
        "chk_pricing_price_grandfather_until",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            DRAFT,
            &[
                ("price_eligibility", "'new_subscriptions_only'"),
                ("grandfather_until", "'2026-12-01 00:00:00+00'"),
            ],
        ),
        "chk_pricing_price_grandfather_until",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The three partial UNIQUE indexes
// ---------------------------------------------------------------------------

/// At most one **current** row per canonical scope key.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_published_rows_on_one_scope_key_cannot_coexist() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(OTHER, &[("lifecycle_state", "'published'")]),
        "uq_pricing_price_scope_key_current",
    )
    .await;
}

/// D-148: the same rule on the **draft** plane, which the published index
/// cannot say.
///
/// Two concurrent authoring calls on one key each read the key as free under the
/// published index alone, and both land — landing exactly the second draft
/// `inst-pr-return` puts among the save-time checks to refuse.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_draft_rows_on_one_scope_key_cannot_coexist() {
    let conn = applied().await;
    must_succeed(&conn, &insert(DRAFT, &[])).await;
    must_be_rejected(
        &conn,
        &insert(OTHER, &[]),
        "uq_pricing_price_scope_key_draft",
    )
    .await;
}

/// And the two indexes are **disjoint by construction**, which is the reason
/// there are two of them rather than one widened one.
///
/// A key legitimately holds a draft *and* its published predecessor at once —
/// the state the D-88 supersession unit works in. A single index over both
/// planes would refuse it, and the suite above would be just as green.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_and_its_published_predecessor_share_one_scope_key() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(DRAFT, &[("supersedes_price_id", &format!("'{PUBLISHED}'"))]),
    )
    .await;
    // A superseded predecessor is outside both predicates, so the chain may be
    // arbitrarily long on one key.
    must_succeed(
        &conn,
        &insert(SUPERSEDED, &[("lifecycle_state", "'superseded'")]),
    )
    .await;
}

/// Meter injectivity (D-103): one priced line per `(meter, dimension_key)` per
/// scope-key slice.
///
/// The two rows differ in `charge_kind`, which is **out** of this index and
/// **in** the scope-key one — so the scope-key index cannot refuse them and this
/// index is the only thing that can. A test that left the charge kinds equal
/// would have been refused by `uq_pricing_price_scope_key_current` and would
/// have proved nothing about meter injectivity.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_published_rows_pricing_one_meter_line_cannot_coexist() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'cloudlets'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            OTHER,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'recurring'"),
                ("meter", "'cloudlets'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
        "uq_pricing_price_meter_line_current",
    )
    .await;
}

/// The `meter IS NOT NULL` conjunct and the empty-tuple `dimension_key`
/// sentinel, from the accepting side.
///
/// Two meterless rows differing only in `charge_kind` must both land: the index
/// holds no entry at all for the recurring, one-time and setup rows it can never
/// speak about. And two rows on one meter with **different** dimension keys are
/// two lines, not one — the per-line reading D-103 fixed the prose to.
///
/// The fourth row differs from the third in `dimension_key` **alone**. It used to
/// move `region` as well, and `region` is an axis of this index and of both
/// scope-key indexes — so the row would have landed with `dimension_key` dropped
/// from every one of them, and the case could not fail for the property it names.
/// One column per case is what the rest of this file does.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_meter_line_index_speaks_only_about_metered_lines() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            OTHER,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'one_time'"),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            SUPERSEDED,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'egress'"),
                ("dimension_key", "'eu-west'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            DRAFT,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'egress'"),
                ("dimension_key", "'us-east'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// D-196 clause (2): the usage pair inside the two scope-key indexes
// ---------------------------------------------------------------------------

/// D-103's confirmed example, which the eight-axis key could not store.
///
/// Two usage lines of one plan in one market differ only in `meter`, and under
/// the eight axes they rendered **one** key — the second was refused
/// `uq_pricing_price_scope_key_current` at save, so *"a `PaaS` plan pricing
/// cloudlets, storage and egress is one plan, not three"* was a decision the
/// store contradicted. Both must land.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_usage_lines_of_one_market_are_two_published_keys() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'cloudlets'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            OTHER,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'egress_gb'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
}

/// The tenth axis carries its own weight: one meter, two dimensions, two keys.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_meter_dimensioned_two_ways_is_two_published_keys() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'cloudlets'"),
                ("dimension_key", "'region=eu'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            OTHER,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'cloudlets'"),
                ("dimension_key", "'region=us'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
}

/// The same widening on the **draft** plane, which D-148's index owns.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_usage_lines_of_one_market_are_two_draft_keys() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            DRAFT,
            &[
                ("charge_kind", "'usage'"),
                ("meter", "'cloudlets'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            OTHER,
            &[
                ("charge_kind", "'usage'"),
                ("meter", "'egress_gb'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
}

/// **The hole the naive widening would have opened, proved on Postgres.**
///
/// `meter` is nullable and NULLs are *distinct* inside a `UNIQUE` index, so an
/// index that simply listed the column would stop refusing the duplicate it
/// refuses today on every non-usage key — every one of which carries
/// `meter IS NULL`. Measured on `SQLite` before the migration was written; this is
/// the same fact on the engine that actually runs production, and it is why both
/// indexes key over `COALESCE(meter, '')` rather than over the column.
///
/// Two published usage rows with **no meter at all** are the sharpest form: they
/// are inside the widened axis set and still share one key.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_meterless_usage_rows_on_one_key_still_collide() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            OTHER,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
        "uq_pricing_price_scope_key_current",
    )
    .await;
}

/// And the same on the draft plane, where the NULL would have been just as
/// distinct.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_meterless_usage_drafts_on_one_key_still_collide() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            DRAFT,
            &[("charge_kind", "'usage'"), ("model_kind", "'per_unit'")],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            OTHER,
            &[("charge_kind", "'usage'"), ("model_kind", "'per_unit'")],
        ),
        "uq_pricing_price_scope_key_draft",
    )
    .await;
}

/// A meterless row and a metered one on otherwise-equal axes are two keys, and
/// the empty-string sentinel is what makes that statement safe: `Meter::new`
/// refuses a blank value, so `''` denotes *no meter* and nothing else can render
/// it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_metered_line_and_a_meterless_one_are_two_keys() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            OTHER,
            &[
                ("lifecycle_state", "'published'"),
                ("charge_kind", "'usage'"),
                ("meter", "'cloudlets'"),
                ("model_kind", "'per_unit'"),
            ],
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_price_append_only()` — the five arms
// ---------------------------------------------------------------------------

/// The two columns of `pricing_price` that are deliberately movable on a frozen
/// row, and why each one is.
///
/// `lifecycle_state` is the sanctioned `published -> superseded` flip the
/// whitelist exists to permit — freezing it would forbid supersession itself, and
/// the arm two cases down is what keeps it from walking back to `draft`.
/// `grandfather_until` is guarded by **monotonicity** instead: it may be
/// tightened and never loosened, which is a different arm of the same function
/// and is proved by its own case.
///
/// Everything else on the table is owed a line in the frozen-column arm, and
/// this list is the only place an exemption can be claimed — so an exemption is
/// a visible edit rather than a column quietly missing from an array.
const SANCTIONED_MUTABLE: [&str; 2] = ["grandfather_until", "lifecycle_state"];

/// **The whitelist names every content column the *table* holds.**
///
/// The census `every_frozen_column_of_a_published_row_refuses_to_move`
/// structurally cannot run, and the reason is the defect class this crate keeps
/// producing: that case moves a hand-written list of columns, so it proves the
/// enumerated ones are frozen and is blind by construction to a column the
/// enumeration omits. Five migrations have already paid for exactly that omission
/// on this table — `000040` for the tax columns, `000051` for the proration ones,
/// `000055` for the reservation pair, `000057` for the floors and `000069` for
/// the `per_unit` rate, which **is the price** — and every one of them was found
/// by a person reading a diff.
///
/// `trg_pricing_plan_append_only` brought `pricing_plan` under a census that reads the column
/// list off the table; this is the same census for the table that holds money,
/// through the one helper both now use.
///
/// It is a text census and not a behavioural one deliberately: a per-column
/// UPDATE needs a value that both differs from the seed and satisfies every
/// pairing CHECK, which is why the sibling case hand-picks its list — and
/// hand-picking is the very step that gets skipped when a column is added. The
/// two are not redundant: a guard could name a column in a comment, and a
/// behavioural case cannot see a column nobody thought to add.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_frozen_whitelist_names_every_content_column_the_table_holds() {
    let conn = applied().await;
    let census = pg_support::frozen_columns(
        &conn,
        "pricing_price",
        "pricing_price_append_only",
        "IF NEW.price_id",
        &SANCTIONED_MUTABLE,
    )
    .await;

    assert!(
        !census.owed.is_empty(),
        "the census read no columns at all, which is the shape a mistyped table \
         name leaves -- it would report every guard complete"
    );
    assert!(
        census.missing().is_empty(),
        "these columns are on bss.pricing_price and absent from the frozen-column \
         whitelist, so an ad-hoc UPDATE moves them under a frozen CatalogVersion: \
         {:?}",
        census.missing()
    );
}

/// Every column the whitelist freezes, one UPDATE each.
///
/// The loop is the point: a whitelist maintained by hand rots one forgotten `OR`
/// at a time, and a test that moved only `amount_minor` would stay green while
/// `included_allowance` or `row_version` quietly became mutable on a frozen row.
/// The trigger is `BEFORE`, so it answers ahead of every CHECK — several of the
/// values below would also be illegal, and none of them gets that far.
///
/// **What keeps the list complete is the table, not a count.** Until 2026-08-13
/// the cross-check below was `moves.len() == 45` against a literal — which is the
/// blindness this case exists to close, moved one layer up: a 46th column added
/// to `pricing_price` and forgotten changes neither the array nor the literal, so
/// both stay green while the column becomes mutable under a frozen
/// `CatalogVersion`. The columns exercised here are now compared **by name**
/// against the census the sibling case reads off `information_schema`, in both
/// directions: a column the table gained and nobody moved is named, and so is a
/// move left behind for a column that no longer exists.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_frozen_column_of_a_published_row_refuses_to_move() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;

    let moves = [
        format!("price_id = '{OTHER}'"),
        "tenant_id = '99999999-9999-9999-9999-999999999999'".to_owned(),
        "plan_id = '99999999-9999-9999-9999-999999999999'".to_owned(),
        "currency = 'EUR'".to_owned(),
        "region = 'US'".to_owned(),
        "price_overlay = 'promo'".to_owned(),
        "phase = '99999999-9999-9999-9999-999999999999'".to_owned(),
        "price_eligibility = 'new_subscriptions_only'".to_owned(),
        "charge_kind = 'usage'".to_owned(),
        format!("cohort = '{COHORT}'"),
        "amount_minor = 2000".to_owned(),
        // D-311's `per_unit` rate (`pricing_price.unit_rate_nano`, guarded by
        // `chk_pricing_price_unit_rate_nano`). It sits beside the column it was split out of
        // because it is the same fact: the price. Splitting a column carries
        // every rule the original had, and `000066` shipped without this one for
        // the length of one commit -- so a published metered row's price was
        // editable by any writer outside this crate, away from the pin that
        // approved it. The Slice-6 note below is the same rot found twice.
        "unit_rate_nano = 23_000_000".to_owned(),
        "model_kind = 'per_unit'".to_owned(),
        "tax_inclusive = true".to_owned(),
        "tax_category_ref = 'reduced'".to_owned(),
        "resolved_tax_category = 'standard'".to_owned(),
        // Its twin (`pricing_price`), and it is here because this case's own
        // sibling census made it red the moment the column existed - which is the
        // whole point of the pair. The guard freezes it for the same reason: a
        // charge replayed from a pinned `CatalogVersion` must round the way it
        // rounded when the version was cut.
        "resolved_rounding_policy = 'half_even/2'".to_owned(),
        "billing_timing = 'advance'".to_owned(),
        // Slice 6's proration contract. `pricing_plan` carries all four in the
        // guard and **none of them here**, so for two days the whitelist froze
        // four columns nothing tested -- the exact rot this loop's own comment
        // describes, found while Slice 10 extended the same list.
        "billing_anchor_policy = 'fixed_day'".to_owned(),
        "anchor_day = 15".to_owned(),
        "proration_basis = 'by_second'".to_owned(),
        "credit_on_downgrade = true".to_owned(),
        "quantity_source = 'manual'".to_owned(),
        "manual_quantity = 5".to_owned(),
        "package_size = 10".to_owned(),
        "package_price_minor = 100".to_owned(),
        "meter = 'cloudlets'".to_owned(),
        "dimension_key = 'eu-west'".to_owned(),
        "billing_granularity = 'per_hour'".to_owned(),
        "aggregation_function = 'sum'".to_owned(),
        "aggregation_granularity = 'day'".to_owned(),
        "tier_aggregation_window = 'calendar_month'".to_owned(),
        "tier_qualification_window = 'current'".to_owned(),
        "max_hold_granules = 3".to_owned(),
        "included_allowance = '{\"units\": 100}'::jsonb".to_owned(),
        // Slice 10's reservation pair (`pricing_price`). The rate is the
        // sharpest case on this table: a writer moving it on a published row
        // moves money per covered granule, away from the pin that approved it.
        "reserved_rate_nano = 250".to_owned(),
        "reservation_flavor = 'capacity'".to_owned(),
        // Slice 10's typed floors and discount hook (`pricing_price`).
        // `min_qty_purchase` decides who may buy, `min_qty_usage` what is
        // billable, the fallback what happens beneath it, and `discount_ref`
        // which instrument discounts the line -- four different consequences of
        // one writer moving a published row.
        "min_qty_purchase = 10".to_owned(),
        "min_qty_usage = 20".to_owned(),
        "min_qty_usage_fallback = 'exception'".to_owned(),
        "discount_ref = 'promo/spring'".to_owned(),
        "rounding_policy_ref = 'policy/1'".to_owned(),
        format!("supersedes_price_id = '{OTHER}'"),
        "created_by = '99999999-9999-9999-9999-999999999999'".to_owned(),
        "created_at_utc = '2026-08-02 09:00:00+00'".to_owned(),
        "row_version = 1".to_owned(),
    ];
    // The cross-check, by name and against the table. The history the old
    // literal carried is worth keeping because it is the argument for reading
    // the table instead: this list stood at 34 against 38 in the guard until
    // 2026-08-08, when Slice 10 added its six and the four Slice-6 proration
    // columns turned out never to have been added at all, and at 44 until
    // 2026-08-11, when D-311 split the `per_unit` rate off `amount_minor`. Each
    // of those gaps was open while a literal count agreed with itself.
    let census = pg_support::frozen_columns(
        &conn,
        "pricing_price",
        "pricing_price_append_only",
        "IF NEW.price_id",
        &SANCTIONED_MUTABLE,
    )
    .await;
    let exercised: BTreeSet<&str> = moves
        .iter()
        .map(|change| {
            change
                .split(' ')
                .next()
                .expect("every move is `column = value`")
        })
        .collect();
    let owed: BTreeSet<&str> = census.owed.iter().map(String::as_str).collect();
    let untested: Vec<&&str> = owed.difference(&exercised).collect();
    assert!(
        untested.is_empty(),
        "these columns are on bss.pricing_price and no UPDATE below moves them, \
         so nothing here would notice them becoming mutable on a frozen row: \
         {untested:?}"
    );
    let stale: Vec<&&str> = exercised.difference(&owed).collect();
    assert!(
        stale.is_empty(),
        "these moves name something that is not an owed column of \
         bss.pricing_price -- a dropped column, a typo, or an exemption that \
         belongs in SANCTIONED_MUTABLE: {stale:?}"
    );

    for change in &moves {
        must_be_rejected(
            &conn,
            &format!("UPDATE bss.pricing_price SET {change} WHERE price_id = '{PUBLISHED}'"),
            "price, scope, model and entity-tag columns are immutable",
        )
        .await;
    }
}

/// A published row has exactly one edge, and it is to `superseded`.
///
/// `lifecycle_state` is deliberately **not** in the frozen list above — freezing
/// it would forbid the supersession flip itself — so this arm is the only thing
/// standing between a published row and a walk back to `draft`, which would put
/// it under the draft index and free the key it currently occupies.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_published_row_may_only_move_to_superseded() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            SUPERSEDED,
            &[("lifecycle_state", "'superseded'"), ("region", "'US'")],
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET lifecycle_state = 'draft' \
             WHERE price_id = '{PUBLISHED}'"
        ),
        "is not a sanctioned transition",
    )
    .await;
    // And nothing leaves `superseded` — the terminal state has no edges at all.
    for target in ["'draft'", "'published'"] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_price SET lifecycle_state = {target} \
                 WHERE price_id = '{SUPERSEDED}'"
            ),
            "is not a sanctioned transition",
        )
        .await;
    }
}

/// The flip that *is* sanctioned, so the arm above is a whitelist.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_supersession_flip_is_accepted() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET lifecycle_state = 'superseded' \
             WHERE price_id = '{PUBLISHED}'"
        ),
    )
    .await;
}

/// D-153, executed: a **draft** row may not jump straight to `superseded`.
///
/// A column whitelist is scoped to published rows by construction, so it says
/// nothing about where a draft row may go, and this trigger used to return early
/// for one. A draft moved to `superseded` satisfies every constraint on the
/// table and lands outside **both** partial UNIQUE predicates: its key reads free
/// on the published plane *and* on the draft plane, undoing D-148's guarantee
/// with a single UPDATE, and `inst-ps-nodelete` then makes the ghost
/// undeletable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_row_cannot_jump_to_superseded() {
    let conn = applied().await;
    must_succeed(&conn, &insert(DRAFT, &[])).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET lifecycle_state = 'superseded' \
             WHERE price_id = '{DRAFT}'"
        ),
        "is not a sanctioned transition",
    )
    .await;
}

/// The draft plane stays mutable, which is what makes the arm above a whitelist
/// and not a freeze.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_row_is_editable_and_publishable() {
    let conn = applied().await;
    must_succeed(&conn, &insert(DRAFT, &[])).await;
    // Content moves, and so does the entity tag that denotes it.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET amount_minor = 2000, row_version = 1 \
             WHERE price_id = '{DRAFT}'"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET lifecycle_state = 'published' \
             WHERE price_id = '{DRAFT}'"
        ),
    )
    .await;
}

/// `inst-ps-nodelete`: a row that has ever been published is never deleted.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_row_that_left_draft_cannot_be_deleted() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PUBLISHED, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            SUPERSEDED,
            &[("lifecycle_state", "'superseded'"), ("region", "'US'")],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_price WHERE price_id = '{PUBLISHED}'"),
        "DELETE of a published row is not permitted",
    )
    .await;
    // The superseded case separately: the message interpolates the state, so a
    // test on the published row alone would leave the other half of the branch
    // resting on a reading of the SQL rather than on a run of it.
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_price WHERE price_id = '{SUPERSEDED}'"),
        "DELETE of a superseded row is not permitted",
    )
    .await;
}

/// A never-published draft row **is** deletable — §4.3, and the reason this
/// table's DELETE arm is conditional where `pricing_plan`'s is absolute.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_row_can_be_deleted() {
    let conn = applied().await;
    must_succeed(&conn, &insert(DRAFT, &[])).await;
    must_succeed(
        &conn,
        &format!("DELETE FROM bss.pricing_price WHERE price_id = '{DRAFT}'"),
    )
    .await;
}

/// Monotonic tightening (D-100): a horizon may be brought in, never pushed out.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_grandfathering_horizon_cannot_be_loosened() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("price_eligibility", "'existing_grandfathered'"),
                ("cohort", &format!("'{COHORT}'")),
                ("grandfather_until", "'2026-12-01 00:00:00+00'"),
            ],
        ),
    )
    .await;
    // Pushed out.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET grandfather_until = '2027-06-01 00:00:00+00' \
             WHERE price_id = '{PUBLISHED}'"
        ),
        "may only be tightened, never loosened",
    )
    .await;
    // Cleared, which is the unbounded horizon and therefore the loosest move of
    // all. A test that only pushed the date out would miss it.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET grandfather_until = NULL \
             WHERE price_id = '{PUBLISHED}'"
        ),
        "may only be tightened, never loosened",
    )
    .await;
}

/// The tightening direction, which the arm must let through: setting a horizon
/// where there was none, and then bringing it in.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_grandfathering_horizon_may_be_tightened() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("lifecycle_state", "'published'"),
                ("price_eligibility", "'existing_grandfathered'"),
                ("cohort", &format!("'{COHORT}'")),
            ],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET grandfather_until = '2026-12-01 00:00:00+00' \
             WHERE price_id = '{PUBLISHED}'"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET grandfather_until = '2026-09-01 00:00:00+00' \
             WHERE price_id = '{PUBLISHED}'"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_price_tier_band` — the three CHECKs, the UNIQUE and the FK
// ---------------------------------------------------------------------------

const BAND_A: &str = "bbbbbbbb-0000-0000-0000-000000000001";
const BAND_B: &str = "bbbbbbbb-0000-0000-0000-000000000002";

fn band(band_id: &str, price_id: &str, from_qty: &str, to_qty: &str, unit_price: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_price_tier_band
            (band_id, tenant_id, price_id, from_qty, to_qty, unit_price_nano)
         VALUES ('{band_id}', '{TENANT}', '{price_id}', {from_qty}, {to_qty}, {unit_price})"
    )
}

/// A `graduated` draft row, the only parent a band may legally hang off.
async fn seed_graduated_draft(conn: &DatabaseConnection, id: &str, region: &str) {
    must_succeed(
        conn,
        &insert(
            id,
            &[
                ("model_kind", "'graduated'"),
                ("charge_kind", "'usage'"),
                ("amount_minor", "NULL"),
                ("region", &format!("'{region}'")),
            ],
        ),
    )
    .await;
}

/// The world for every band refusal below: a legal band on a legal parent.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_on_a_graduated_draft_row_is_storable() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "100", "500")).await;
    // The open top is a real band, not a missing bound.
    must_succeed(&conn, &band(BAND_B, DRAFT, "100", "NULL", "400")).await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_band_lower_bound_is_refused() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_be_rejected(
        &conn,
        &band(BAND_A, DRAFT, "-1", "100", "500"),
        "chk_pricing_price_tier_band_from_qty",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_band_unit_price_is_refused() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_be_rejected(
        &conn,
        &band(BAND_A, DRAFT, "0", "100", "-1"),
        "chk_pricing_price_tier_band_unit_price",
    )
    .await;
    // Zero is a legal unit price: the D-45 allowance compile's `$0` first band
    // is exactly that, and a `> 0` here would make it unstorable.
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "100", "0")).await;
}

/// A band must have width. `NULL to_qty` is the open top, not a missing value —
/// which is why the constraint is `to_qty IS NULL OR to_qty > from_qty` and not
/// a bare `>`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_zero_width_or_inverted_band_is_refused() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_be_rejected(
        &conn,
        &band(BAND_A, DRAFT, "100", "100", "500"),
        "chk_pricing_price_tier_band_width",
    )
    .await;
    must_be_rejected(
        &conn,
        &band(BAND_A, DRAFT, "100", "50", "500"),
        "chk_pricing_price_tier_band_width",
    )
    .await;
}

/// A band's identity is where it starts, and there is no ordinal column — so two
/// bands on one lower bound are one band twice.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_bands_sharing_a_lower_bound_cannot_coexist() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "100", "500")).await;
    must_be_rejected(
        &conn,
        &band(BAND_B, DRAFT, "0", "200", "400"),
        "uq_pricing_price_tier_band_lower_bound",
    )
    .await;
}

/// The foreign key, reached from the **parent** end — the only end that reaches
/// it at all.
///
/// A band naming a price row that does not exist is refused by
/// `trg_pricing_price_tier_band_append_only`, which fires `BEFORE INSERT` and
/// finds no parent state, so the FK never gets a statement to judge on that side
/// (the case below the next test). What it does judge is the delete of a
/// still-referenced parent: a draft price row is deletable, and deleting one out
/// from under its bands would leave them orphaned on a table whose own triggers
/// resolve every rule through the parent — after which no arm of any band
/// trigger can find a `lifecycle_state` and every one of them refuses with
/// `missing`, i.e. the band set becomes permanently unwritable and undeletable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_price_row_that_still_carries_bands_cannot_be_deleted() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "NULL", "500")).await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_price WHERE price_id = '{DRAFT}'"),
        "fk_pricing_price_tier_band_price",
    )
    .await;
    // Bands first, then the row — the order `PriceRepo` is obliged to use.
    must_succeed(
        &conn,
        &format!("DELETE FROM bss.pricing_price_tier_band WHERE band_id = '{BAND_A}'"),
    )
    .await;
    must_succeed(
        &conn,
        &format!("DELETE FROM bss.pricing_price WHERE price_id = '{DRAFT}'"),
    )
    .await;
}

/// **Deliberately absent guards**, pinned so that adding one is a decision
/// rather than an accident.
///
/// Ascending order, gaplessness, non-overlap and the always-open top are
/// properties of the band set *as a sequence*: each is a statement about a row
/// and its neighbour, and neither a row CHECK nor a unique index can see a
/// neighbour. `domain::rules::tier_bands` owns them at publish, where the whole
/// set is in hand and every violation can be reported at once. A constraint here
/// could only ever express a weaker rule while looking like the real one — so
/// the table stores the malformed set below without complaint, and this test
/// reddens the day something starts refusing it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_band_table_does_not_judge_the_set_as_a_sequence() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    // A gap between 100 and 200, an overlap between 200-400 and 300-500, a
    // closed top, and the rows inserted out of ascending order.
    must_succeed(&conn, &band(BAND_A, DRAFT, "300", "500", "300")).await;
    must_succeed(&conn, &band(BAND_B, DRAFT, "0", "100", "500")).await;
    must_succeed(
        &conn,
        &band(
            "bbbbbbbb-0000-0000-0000-000000000003",
            DRAFT,
            "200",
            "400",
            "400",
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_price_tier_band_kind()` — structural exclusivity, child end
// ---------------------------------------------------------------------------

/// Band rows are forbidden unless the parent is `graduated` or `volume`.
///
/// The kindless arm is separate because the message distinguishes it, and
/// because a `parent_kind NOT IN (...)` written without the `IS NULL` disjunct
/// evaluates to NULL on a kindless parent — the same NULL-swallowing trap the
/// package CHECK spells out — and would admit bands on a row with no kind at
/// all.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_on_a_price_row_of_the_wrong_kind_is_refused() {
    let conn = applied().await;
    // A `flat` draft parent.
    must_succeed(&conn, &insert(DRAFT, &[])).await;
    must_be_rejected(
        &conn,
        &band(BAND_A, DRAFT, "0", "100", "500"),
        "band rows are forbidden on a flat price row",
    )
    .await;

    // And a kindless one.
    must_succeed(
        &conn,
        &insert(
            OTHER,
            &[
                ("model_kind", "NULL"),
                ("amount_minor", "NULL"),
                ("region", "'US'"),
            ],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &band(BAND_A, OTHER, "0", "100", "500"),
        "band rows are forbidden on a kindless price row",
    )
    .await;

    // `volume` is the other legal kind, so the rule is a pair and not one name.
    must_succeed(
        &conn,
        &insert(
            SUPERSEDED,
            &[
                ("model_kind", "'volume'"),
                ("charge_kind", "'usage'"),
                ("amount_minor", "NULL"),
                ("region", "'APAC'"),
            ],
        ),
    )
    .await;
    must_succeed(&conn, &band(BAND_A, SUPERSEDED, "0", "100", "500")).await;
}

/// The same trigger's UPDATE event: a band re-pointed onto a `flat` parent
/// reaches the forbidden pair without an INSERT ever happening.
///
/// Both parents are draft, so `trg_pricing_price_tier_band_append_only` — which
/// fires first, its name sorting ahead — has nothing to object to and this
/// trigger is what answers.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_repointed_onto_a_price_row_of_the_wrong_kind_is_refused() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &insert(OTHER, &[("region", "'US'")])).await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "100", "500")).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_tier_band SET price_id = '{OTHER}' \
             WHERE band_id = '{BAND_A}'"
        ),
        "band rows are forbidden on a flat price row",
    )
    .await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_price_tier_band_parent_kind()` — the same rule, parent end
// ---------------------------------------------------------------------------

/// The child-side arms judge a band as it arrives; nothing in them stops a draft
/// parent's `model_kind` flipping out from under a band set that is already
/// there.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_banded_price_row_cannot_become_a_kind_that_carries_no_bands() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "NULL", "500")).await;
    must_be_rejected(
        &conn,
        &format!("UPDATE bss.pricing_price SET model_kind = 'flat' WHERE price_id = '{DRAFT}'"),
        "still carries bands and may not become a flat row",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("UPDATE bss.pricing_price SET model_kind = NULL WHERE price_id = '{DRAFT}'"),
        "still carries bands and may not become a kindless row",
    )
    .await;
}

/// The moves the parent-side arm must let through, and the order it obliges.
///
/// `graduated -> volume` keeps the bands meaningful, so it is accepted; and a
/// legitimate edit turning a banded `graduated` row into a bandless `flat` one
/// works only if the band set goes first — which is exactly why
/// `PriceRepo::update_draft` replaces the bands before it moves the row.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_banded_price_row_moves_between_the_two_banded_kinds() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "NULL", "500")).await;
    must_succeed(
        &conn,
        &format!("UPDATE bss.pricing_price SET model_kind = 'volume' WHERE price_id = '{DRAFT}'"),
    )
    .await;
    must_succeed(
        &conn,
        &format!("DELETE FROM bss.pricing_price_tier_band WHERE price_id = '{DRAFT}'"),
    )
    .await;
    must_succeed(
        &conn,
        &format!("UPDATE bss.pricing_price SET model_kind = 'flat' WHERE price_id = '{DRAFT}'"),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_price_tier_band_append_only()` — the two arms
// ---------------------------------------------------------------------------

/// The **`NEW`-side** arm: a band may not land under a parent that has left
/// draft.
///
/// INSERT is guarded and not only UPDATE and DELETE because an INSERT is the one
/// verb that adds money to a frozen row, and the kind trigger — which does fire
/// on INSERT — reads only the parent's `model_kind`, so a `graduated` row that
/// had already published would otherwise have accepted a new band from any
/// caller. The parent below is `graduated` precisely so that the kind trigger has
/// nothing to say and this arm is what answers.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_cannot_be_inserted_under_a_frozen_price_row() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("model_kind", "'graduated'"),
                ("charge_kind", "'usage'"),
                ("amount_minor", "NULL"),
                ("lifecycle_state", "'published'"),
            ],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &band(BAND_A, PUBLISHED, "0", "100", "500"),
        "INSERT of a band under a published price row is not permitted",
    )
    .await;
}

/// The `missing` branch of the same arm — **and the reason the foreign key never
/// answers on this side**.
///
/// `fk_pricing_price_tier_band_price` would refuse a band naming a price row that
/// does not exist, but a `BEFORE INSERT` trigger runs ahead of constraint
/// checking and this one refuses first. The FK is therefore live only on the
/// parent-delete path tested above; on the child-insert path it is unreachable,
/// and this test is what records that rather than leaving it to be rediscovered.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_naming_no_existing_price_row_is_refused_by_the_trigger_not_the_key() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &band(BAND_A, OTHER, "0", "100", "500"),
        "INSERT of a band under a missing price row is not permitted",
    )
    .await;
}

/// The **`OLD`-side** arm: the parent a band is bound to *now* governs whether
/// the band may be mutated or dropped.
///
/// The band is inserted while the parent is still draft and the parent is
/// published afterwards, which is the only way to reach this state — the
/// `NEW`-side arm forbids inserting it directly.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_under_a_frozen_price_row_can_be_neither_updated_nor_deleted() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "NULL", "500")).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price SET lifecycle_state = 'published' \
             WHERE price_id = '{DRAFT}'"
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_tier_band SET unit_price_nano = 1 \
             WHERE band_id = '{BAND_A}'"
        ),
        "UPDATE of a band under a published price row is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_price_tier_band WHERE band_id = '{BAND_A}'"),
        "DELETE of a band under a published price row is not permitted",
    )
    .await;
}

/// The two arms are two arms, and this is the statement only the `NEW`-side one
/// refuses.
///
/// Re-pointing a band from a draft parent onto a frozen one is how you would
/// append to a frozen band set without ever issuing an INSERT. The `OLD`-side arm
/// is satisfied — the band's current parent *is* draft — so a trigger carrying
/// only that arm would let this through.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_cannot_be_repointed_onto_a_frozen_price_row() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "NULL", "500")).await;
    must_succeed(
        &conn,
        &insert(
            PUBLISHED,
            &[
                ("model_kind", "'graduated'"),
                ("charge_kind", "'usage'"),
                ("amount_minor", "NULL"),
                ("lifecycle_state", "'published'"),
                ("region", "'US'"),
            ],
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_tier_band SET price_id = '{PUBLISHED}' \
             WHERE band_id = '{BAND_A}'"
        ),
        "UPDATE of a band under a published price row is not permitted",
    )
    .await;
}

/// Under a draft parent every verb works, so the arms above are a freeze on the
/// published plane and not a ban on the table.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_band_under_a_draft_price_row_can_be_updated_and_deleted() {
    let conn = applied().await;
    seed_graduated_draft(&conn, DRAFT, "EU").await;
    seed_graduated_draft(&conn, OTHER, "US").await;
    must_succeed(&conn, &band(BAND_A, DRAFT, "0", "NULL", "500")).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_tier_band SET unit_price_nano = 400 \
             WHERE band_id = '{BAND_A}'"
        ),
    )
    .await;
    // Re-pointed onto another draft parent: both arms are satisfied.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_tier_band SET price_id = '{OTHER}' \
             WHERE band_id = '{BAND_A}'"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!("DELETE FROM bss.pricing_price_tier_band WHERE band_id = '{BAND_A}'"),
    )
    .await;
}
