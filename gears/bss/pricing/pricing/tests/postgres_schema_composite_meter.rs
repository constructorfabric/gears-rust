//! Slice 10's composite meter **on the engine that runs in production**
//! (`design/10-advanced-primitives.md` §6, D-256).
//!
//! `sqlite_migrations` proves the objects exist by name and digest on the
//! mirror; `postgres_migrations` proves they exist by name here. Neither runs
//! the guard. The two arms are written separately — one PL/pgSQL function
//! against three `RAISE(ABORT, …)` triggers — and only the `SQLite` side carries
//! a trigger-**body** digest census, so until this file a lost disjunct in the
//! PL/pgSQL append-only function was invisible on the engine that ships.
//!
//! This is the standing half of the debt D-260 records, closed for this table.
//!
//! Run with:
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_composite_meter -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const PLAN: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const TENANT: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "33333333-3333-3333-3333-333333333333";
const COMPOSITE: &str = "cccccccc-0000-0000-0000-000000000001";

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

async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, by: &str) {
    let Err(err) = exec(conn, sql).await else {
        panic!("this statement must be rejected: {sql}");
    };
    let message = err.to_string();
    assert!(
        message.contains(by),
        "the rejection must be the one under test (`{by}`), got: {message}"
    );
}

/// A plan revision in `state`, the parent every composite row hangs off.
async fn seed_plan(conn: &DatabaseConnection, revision: i64, state: &str) {
    must_succeed(
        conn,
        &format!(
            "INSERT INTO bss.pricing_plan \
             (plan_id, revision, tenant_id, lifecycle_state, created_by, created_at_utc) \
             VALUES ('{PLAN}', {revision}, '{TENANT}', '{state}', '{ACTOR}', now())"
        ),
    )
    .await;
}

fn insert_composite(composite: &str, revision: i64, output: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_composite_meter \
         (composite_id, plan_revision, tenant_id, plan_id, output_unit, constituent_units, formula) \
         VALUES ('{composite}', {revision}, '{TENANT}', '{PLAN}', '{output}', \
         '[\"vcpu\",\"ram\"]'::jsonb, '{{\"op\":\"weighted_sum\"}}'::jsonb)"
    )
}

/// A composite is authorable under a **draft** revision and under no other — the
/// parent's `lifecycle_state` is the row's, which is the whole append-only
/// arrangement.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_composite_is_authorable_only_under_a_draft_revision() {
    let conn = applied().await;
    seed_plan(&conn, 0, "draft").await;
    must_succeed(&conn, &insert_composite(COMPOSITE, 0, "vm-hour")).await;

    // The parent publishes; the child is frozen with it.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_plan SET lifecycle_state = 'published' \
             WHERE plan_id = '{PLAN}' AND revision = 0"
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_composite("cccccccc-0000-0000-0000-000000000002", 0, "pod-hour"),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_composite_meter SET output_unit = 'moved' \
             WHERE composite_id = '{COMPOSITE}'"
        ),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_composite_meter WHERE composite_id = '{COMPOSITE}'"),
        "is not permitted",
    )
    .await;
}

/// One output unit per revision — §3 step 3's injectivity as far as the schema
/// carries it. Two composites rating into the same unit would produce two priced
/// lines on one `(meter, dimensionKey)`.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_output_unit_per_revision() {
    let conn = applied().await;
    seed_plan(&conn, 0, "draft").await;
    must_succeed(&conn, &insert_composite(COMPOSITE, 0, "vm-hour")).await;
    must_be_rejected(
        &conn,
        &insert_composite("cccccccc-0000-0000-0000-000000000002", 0, "vm-hour"),
        "uq_pricing_composite_meter_output",
    )
    .await;
}

/// An empty output unit publishes a composite nothing can rate — and so does one
/// made of nothing but blanks, of whatever width.
///
/// The blank cases are what make the constraint a trim rather than
/// `length(output_unit) > 0`: a bare length test admits `"   "`, which renders on an
/// invoice line as a blank and joins no meter to any unit. Six taxonomy tables spell
/// their value-present rule the same way, so this is the chain's shape for the rule
/// rather than an exception.
///
/// **Each width, on the whole set.** `btrim(X, Y)` strips the characters `Y` names
/// and the predicate names ASCII whitespace entire, so a tab-only unit is refused
/// here exactly as it is by `chk_pricing_region_taxonomy_value_present` and its five
/// siblings — one rule with one meaning across the chain. The tab is asserted
/// separately from the spaces because the one-argument `btrim` strips spaces alone
/// and passes a spaces-only case while admitting it. The residue beyond ASCII is
/// stated on `pricing_region_taxonomy`'s migration and belongs to the domain.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_output_unit_is_not_empty() {
    let conn = applied().await;
    seed_plan(&conn, 0, "draft").await;
    for blank in ["", "   ", "\t", "\n", "\u{b}", "\u{c}", "\r"] {
        must_be_rejected(
            &conn,
            &insert_composite(COMPOSITE, 0, blank),
            "chk_pricing_composite_meter_output_unit",
        )
        .await;
    }
    // The control: a unit that merely needs a trim is a unit, and the predicate is
    // about the absence of a non-blank character rather than about padding.
    must_succeed(&conn, &insert_composite(COMPOSITE, 0, " vm-hour ")).await;
}

/// A composite naming a revision that does not exist is refused — **by the
/// trigger, and the foreign key behind it is shadowed.**
///
/// The append-only function reads the parent's `lifecycle_state` and renders a
/// missing parent as `missing`, so it answers `BEFORE INSERT`, ahead of
/// `fk_pricing_composite_meter_revision`. The key is not removed: it still
/// declares the relationship and would answer if the trigger were ever
/// narrowed. What no test can assert is the key *acting* — the same shadowing
/// D-231 recorded for `pricing_price_overlay_line`'s composite key, stated here
/// rather than discovered again.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_composite_names_a_revision_that_exists() {
    let conn = applied().await;
    seed_plan(&conn, 0, "draft").await;
    must_be_rejected(
        &conn,
        &insert_composite(COMPOSITE, 7, "vm-hour"),
        "under a missing plan revision is not permitted",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The parent revision's tenancy.
// ---------------------------------------------------------------------------

/// A composite belongs to the tenant its own revision belongs to.
///
/// `fk_pricing_composite_meter_revision` covers `(plan_id, plan_revision)` alone
/// while the table is scoped by `tenant_id`, so without this arm the key admits a
/// row naming somebody else's tenant — invisible to every scoped reader and frozen
/// with the revision it was written under. On this table the row is also an
/// **oracle**: a composite's output unit is a name a formula elsewhere resolves,
/// and one written under a foreign tenant is a name that tenant can neither see nor
/// remove.
///
/// The arm was present on both engines and **executed nowhere**. It was carried by
/// the trigger-name roster, the trigger-body digest roster and both schema
/// goldens, and all three pin the guard's *text* — the half an author regenerates
/// in the same edit that breaks it. A guard that is present, correct-looking and
/// semantically inert reads as coverage to every one of them.
///
/// Pinned by the guard's **own message fragment** rather than by the table name: a
/// table-name discriminator is shared by every guard on this table — the
/// draft-only arm, the missing-revision arm, the output-unit CHECK — and would
/// pass for whichever one happened to answer.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_composite_may_not_claim_a_tenant_its_revision_does_not_belong_to() {
    const TENANT_TWO: &str = "22222222-2222-2222-2222-2222222222b2";
    let conn = applied().await;
    seed_plan(&conn, 0, "draft").await;

    must_be_rejected(
        &conn,
        &insert_composite(COMPOSITE, 0, "vm-hour").replace(TENANT, TENANT_TWO),
        "belongs to another tenant and may not hold this row",
    )
    .await;

    // The same row under the revision's own tenant lands, so the refusal above is
    // about the tenant and about nothing else the row carries.
    must_succeed(&conn, &insert_composite(COMPOSITE, 0, "vm-hour")).await;

    // And the UPDATE direction, because the arm sits on the function's `NEW` path
    // and a draft revision's composites are mutable.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_composite_meter SET tenant_id = '{TENANT_TWO}' \
             WHERE composite_id = '{COMPOSITE}'"
        ),
        "belongs to another tenant and may not hold this row",
    )
    .await;
}
