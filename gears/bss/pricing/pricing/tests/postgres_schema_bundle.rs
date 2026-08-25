//! The four Slice-8 bundle tables — `pricing_bundle` and its three
//! revision-scoped composition children — proved by **executing the statement
//! each object must refuse**, on Postgres.
//!
//! # Why this suite exists
//!
//! `pricing_bundle`…`…000027` had never run on the backend they target.
//! `tests/sqlite_bundle_store.rs` proves the **mirror**, which is a different set
//! of objects: three fixed-message `RAISE(ABORT, …)` triggers per child table
//! where Postgres has one PL/pgSQL function with two arms, and a `WHERE NOT
//! EXISTS` subquery in the trigger body where Postgres does a `SELECT … INTO`.
//! `tests/postgres_migrations.rs` pins the CHECK, function, trigger and
//! partial-index rosters by name and issues no DML, so it says the objects
//! reached the server and nothing about what any of them does. This suite is the
//! other half: one executed refusal per object, and the assertion names the
//! object the refusal came from.
//!
//! # The three rules every test here follows
//!
//! **Execute the refusal.** A test that writes valid values catches a constraint
//! that got *narrower* and never one that stopped refusing.
//!
//! **Put the world in the state where the object under test is what answers.**
//! Two hazards live on these tables and both were hit while writing this file:
//!
//! * the child triggers resolve the owning revision **through `pricing_bundle`**,
//!   so an absent bundle makes their `NOT EXISTS` true and the trigger answers
//!   before `fk_pricing_bundle_component_bundle` is ever consulted. The foreign
//!   key is therefore unreachable on INSERT and is tested through the one
//!   statement it alone can refuse — a rev-share **party** whose group is absent,
//!   where the group table is not the trigger's referent.
//! * `uq_pricing_plan_open_draft` refuses a second `draft` revision of one plan,
//!   so every draft parent here is its own plan, and the frozen parents are
//!   reached by seeding a draft and taking a sanctioned flip.
//!
//! **Assert the object, never the table.** Every CHECK and trigger message over
//! these tables carries the table name, so a test that accepted any error naming
//! the table would pass with the guard it means to prove switched off. The
//! `CHECK` cases therefore name the **constraint**, and the trigger cases name
//! the trigger's own sentence — which `no such table` and a foreign-key
//! violation both cannot produce.
//!
//! # The trigger has two arms, and the split follows them
//!
//! All three child tables carry `pricing_plan_addon_rule`'s function shape:
//!
//! ```text
//! IF TG_OP <> 'INSERT' THEN  -- arm 1: the OLD parent must be a draft
//! IF TG_OP =  'DELETE' THEN RETURN OLD;
//!                            -- arm 2: the NEW parent must be a draft
//! ```
//!
//! Arm 1's only unshared statement is the **DELETE**; arm 2's are the **INSERT**
//! and the `UPDATE` that **re-points** a child row from a draft revision onto a
//! frozen one. The tests below are split along that line, so deleting either arm
//! reddens exactly one of them.
//!
//! # Objects this suite deliberately does not test by refusal
//!
//! `idx_pricing_bundle_tenant`, `idx_pricing_bundle_component_revision`,
//! `idx_pricing_bundle_component_plan`,
//! `idx_pricing_bundle_revshare_group_revision` and
//! `idx_pricing_bundle_revshare_revision` are non-unique, non-partial indexes.
//! They refuse nothing; their presence is pinned by name in
//! `tests/postgres_migrations.rs` and that is the whole of what can be said.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_bundle -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";

/// One parent plan per state a test needs: `uq_pricing_plan_open_draft` is keyed
/// on `plan_id` alone, so two draft revisions cannot share a plan.
const PLAN_A: &str = "22222222-0000-0000-0000-00000000000a";
const PLAN_B: &str = "22222222-0000-0000-0000-00000000000b";
const PLAN_C: &str = "22222222-0000-0000-0000-00000000000c";

const BUNDLE_A: &str = "66666666-0000-0000-0000-00000000000a";
const BUNDLE_B: &str = "66666666-0000-0000-0000-00000000000b";
const BUNDLE_C: &str = "66666666-0000-0000-0000-00000000000c";
/// A bundle id no `pricing_bundle` row carries.
const BUNDLE_ABSENT: &str = "66666666-0000-0000-0000-0000000000ff";

const COMPONENT_1: &str = "77777777-0000-0000-0000-000000000001";
const COMPONENT_2: &str = "77777777-0000-0000-0000-000000000002";
const SKU_1: &str = "88888888-0000-0000-0000-000000000001";
const VENDOR_1: &str = "99999999-0000-0000-0000-000000000001";
const VENDOR_2: &str = "99999999-0000-0000-0000-000000000002";
const VENDOR_3: &str = "99999999-0000-0000-0000-000000000003";
const VENDOR_ABSENT: &str = "99999999-0000-0000-0000-0000000000ff";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A fresh database carrying the applied chain, on the one shared server.
///
/// **One** container for the whole binary and a `CREATE DATABASE` per test; the
/// arrangement and the eleven false positives that motivated it are documented
/// in `tests/pg_support/mod.rs`.
///
/// The connection handed back is a **plain** one: every statement this suite
/// issues is raw SQL that deliberately reaches past `bundle_repo`, because the
/// repository is exactly the layer that cannot see a guard stop refusing.
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

/// Reject, **and by the named object**.
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

/// A plan revision in `draft`.
async fn seed_revision(conn: &DatabaseConnection, plan: &str, revision: i64) {
    must_succeed(
        conn,
        &format!(
            "INSERT INTO bss.pricing_plan (
                plan_id, revision, tenant_id, lifecycle_state, created_by, created_at_utc)
             VALUES ('{plan}', {revision}, '{TENANT}', 'draft', '{ACTOR}', now())"
        ),
    )
    .await;
}

/// A sanctioned flip out of `draft`, taken through the plan's own whitelist.
async fn flip(conn: &DatabaseConnection, plan: &str, revision: i64, state: &str) {
    must_succeed(
        conn,
        &format!(
            "UPDATE bss.pricing_plan SET lifecycle_state = '{state}' \
             WHERE plan_id = '{plan}' AND revision = {revision}"
        ),
    )
    .await;
}

async fn seed_bundle(conn: &DatabaseConnection, bundle: &str, plan: &str, basis: &str) {
    must_succeed(
        conn,
        &format!(
            "INSERT INTO bss.pricing_bundle (
                bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
             VALUES ('{bundle}', '{TENANT}', '{plan}', '{basis}', 'aggregate')"
        ),
    )
    .await;
}

fn component_sql(bundle: &str, revision: i64, component: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_bundle_component (
            bundle_id, plan_revision, component_plan_id, tenant_id, included_sku_id)
         VALUES ('{bundle}', {revision}, '{component}', '{TENANT}', '{SKU_1}')"
    )
}

fn group_sql(bundle: &str, revision: i64, vendor: &str, cut: i32, absorber: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_bundle_revshare_group (
            bundle_id, plan_revision, vendor_sku_id, tenant_id,
            platform_cut_bp, residual_absorber_party)
         VALUES ('{bundle}', {revision}, '{vendor}', '{TENANT}', {cut}, '{absorber}')"
    )
}

fn party_sql(bundle: &str, revision: i64, vendor: &str, party: &str, share: i32) -> String {
    format!(
        "INSERT INTO bss.pricing_bundle_revshare (
            bundle_id, plan_revision, vendor_sku_id, party, tenant_id, share_bp)
         VALUES ('{bundle}', {revision}, '{vendor}', '{party}', '{TENANT}', {share})"
    )
}

// ---------------------------------------------------------------------------
// `pricing_bundle` — the CHECKs and the one unique index.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker"]
async fn the_price_basis_check_refuses_a_third_basis() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_bundle (
                bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
             VALUES ('{BUNDLE_A}', '{TENANT}', '{PLAN_A}', 'sum_of_the_parts', 'aggregate')"
        ),
        "chk_pricing_bundle_price_basis",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn the_itemization_check_refuses_a_third_layout() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_bundle (
                bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
             VALUES ('{BUNDLE_A}', '{TENANT}', '{PLAN_A}', 'own_price', 'itemised')"
        ),
        "chk_pricing_bundle_invoice_itemization",
    )
    .await;
}

/// The Postgres half of the rule `sqlite_bundle_store` can only assert against a
/// column name: here the index is named in the message, so the assertion is
/// sharper on this backend than on the mirror.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_plan_carries_at_most_one_bundle() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_bundle (
                bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
             VALUES ('{BUNDLE_B}', '{TENANT}', '{PLAN_A}', 'own_price', 'itemize')"
        ),
        "uq_pricing_bundle_plan",
    )
    .await;
}

/// **`plan_id` has no foreign key**, and this is where that is proved on the
/// backend it matters on. D-105 calls it an FK; `pricing_plan`'s only uniqueness
/// on `plan_id` alone is in two partial indexes, which Postgres refuses as a
/// referent — so the constraint is not declarable and the row lands.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_bundle_may_name_a_plan_that_does_not_exist() {
    let conn = applied().await;

    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_bundle (
            bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
         VALUES ('{BUNDLE_A}', '{TENANT}', '{PLAN_C}', 'sum_of_parts', 'aggregate')"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// The composition CHECKs.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker"]
async fn an_inverted_component_quantity_range_is_refused() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_bundle_component (
                bundle_id, plan_revision, component_plan_id, tenant_id,
                included_sku_id, min_qty, max_qty)
             VALUES ('{BUNDLE_A}', 0, '{COMPONENT_1}', '{TENANT}', '{SKU_1}', 5, 2)"
        ),
        "chk_pricing_bundle_component_qty_range",
    )
    .await;
}

/// **A component's minimum quantity has a floor, and it is zero.**
///
/// The rule fails **open** and its neighbour hides that: `chk_..._qty_range`
/// only orders the pair, so `min_qty = -1` with a larger `max_qty` satisfies it
/// and lands the moment `chk_..._min_qty` stops refusing. A negative minimum is
/// a component the coverage walk reads as *owing* quantity — `inst-bc-coverage`
/// quantifies over a set whose floor it takes from this column — and no typed
/// path can produce one, so nothing above the schema would ever have said so.
///
/// The pair is asked from both sides: zero is the boundary and is legal, because
/// an optional component's minimum **is** zero and a constraint tightened to
/// `> 0` would refuse every one of them.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_negative_component_minimum_quantity_is_refused() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    let with_quantities = |min: i32, max: i32| {
        format!(
            "INSERT INTO bss.pricing_bundle_component (
                bundle_id, plan_revision, component_plan_id, tenant_id,
                included_sku_id, min_qty, max_qty)
             VALUES ('{BUNDLE_A}', 0, '{COMPONENT_1}', '{TENANT}', '{SKU_1}', {min}, {max})"
        )
    };

    must_be_rejected(
        &conn,
        &with_quantities(-1, 5),
        "chk_pricing_bundle_component_min_qty",
    )
    .await;
    must_succeed(&conn, &with_quantities(0, 5)).await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn a_share_outside_the_basis_point_scale_is_refused() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    must_succeed(&conn, &group_sql(BUNDLE_A, 0, VENDOR_1, 1000, "platform")).await;

    // **Both ends, and both boundaries.** The CHECK is `>= 0 AND <= 10000` and
    // only `10_001` was ever submitted, so the lower half was proved by nothing
    // and neither boundary was admitted by anything - a CHECK written `> 0 AND <
    // 10000` would have passed this case while refusing a 0% share and a 100% one,
    // both of which are real: a vendor carrying none of the revenue and a vendor
    // carrying all of it.
    must_be_rejected(
        &conn,
        &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-a", 10_001),
        "chk_pricing_bundle_revshare_share_bp",
    )
    .await;
    must_be_rejected(
        &conn,
        &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-b", -1),
        "chk_pricing_bundle_revshare_share_bp",
    )
    .await;
    must_succeed(&conn, &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-zero", 0)).await;
    must_succeed(
        &conn,
        &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-all", 10_000),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn a_platform_cut_outside_the_basis_point_scale_is_refused() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    // Both ends and both boundaries, for the share CHECK's reason one table over.
    // A platform cut of 0 is a platform taking nothing, and 10000 is a platform
    // taking everything; each is a shape an author can mean.
    must_be_rejected(
        &conn,
        &group_sql(BUNDLE_A, 0, VENDOR_1, 10_001, "platform"),
        "chk_pricing_bundle_revshare_group_platform_cut_bp",
    )
    .await;
    must_be_rejected(
        &conn,
        &group_sql(BUNDLE_A, 0, VENDOR_2, -1, "platform"),
        "chk_pricing_bundle_revshare_group_platform_cut_bp",
    )
    .await;
    must_succeed(&conn, &group_sql(BUNDLE_A, 0, VENDOR_2, 0, "platform")).await;
    must_succeed(&conn, &group_sql(BUNDLE_A, 0, VENDOR_3, 10_000, "platform")).await;
}

/// A party may not spell the group's `platform` sentinel: the absorber column is
/// compared against this one, and D-07's default would otherwise be ambiguous.
///
/// **Nor may it spell it with padding.** `party <> 'platform'` compares the stored
/// text, so a padded copy satisfied the clause while `Party::new` trimmed it back to
/// the sentinel and refused it — the ambiguity this case exists to prevent, reachable
/// through the one clause meant to close it. The predicate trims before both of its
/// comparisons, and the padded forms are asserted here rather than beside the blank
/// ones because the bare `'platform'` case above cannot see them.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_party_may_not_be_named_platform() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    must_succeed(&conn, &group_sql(BUNDLE_A, 0, VENDOR_1, 1000, "platform")).await;

    for forged in ["platform", " platform ", "platform ", "\tplatform"] {
        must_be_rejected(
            &conn,
            &party_sql(BUNDLE_A, 0, VENDOR_1, forged, 9000),
            "chk_pricing_bundle_revshare_party",
        )
        .await;
    }

    // The control: padding alone is not what the clause refuses. A party whose
    // trimmed name is its own is stored, which is what `Party::new` does with it.
    must_succeed(&conn, &party_sql(BUNDLE_A, 0, VENDOR_1, " acme ", 9000)).await;
}

/// An absorber must name something. The blank string would give "unnominated" a
/// spelling, which D-07 says cannot exist.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_blank_absorber_is_refused() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    must_be_rejected(
        &conn,
        &group_sql(BUNDLE_A, 0, VENDOR_1, 1000, ""),
        "chk_pricing_bundle_revshare_group_absorber",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The group foreign key — the one statement it alone can refuse.
// ---------------------------------------------------------------------------

/// `inst-rs-sum` requires *"an explicit per-group platform cut"*, and this is its
/// physical half: a party row cannot exist without the group that carries the
/// cut. It is also the only foreign key of this table set that is reachable — the
/// child triggers resolve their parent through `pricing_bundle`, so they answer
/// first for anything keyed on a missing bundle, while the **group** is not a
/// referent any trigger consults.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_party_row_outside_its_group_is_refused() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    must_be_rejected(
        &conn,
        &party_sql(BUNDLE_A, 0, VENDOR_ABSENT, "vendor-a", 4500),
        "fk_pricing_bundle_revshare_group",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The append-only triggers — arm 2 (INSERT and the re-point).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker"]
async fn a_component_cannot_be_appended_to_a_published_revision() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    flip(&conn, PLAN_A, 0, "published").await;

    must_be_rejected(
        &conn,
        &component_sql(BUNDLE_A, 0, COMPONENT_1),
        "INSERT of a component under a non-draft plan revision is not permitted",
    )
    .await;
}

/// **`abandoned` is not `draft`** — the ordering constraint that forces a
/// repository to drop a discarded draft's composition before it flips.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_component_cannot_be_appended_to_an_abandoned_revision() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    flip(&conn, PLAN_A, 0, "abandoned").await;

    must_be_rejected(
        &conn,
        &component_sql(BUNDLE_A, 0, COMPONENT_1),
        "INSERT of a component under a non-draft plan revision is not permitted",
    )
    .await;
}

/// The re-point: arm 2's other unshared statement, and the way a frozen revision
/// would otherwise acquire a child with no INSERT ever issued.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_component_cannot_be_repointed_onto_a_published_revision() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    flip(&conn, PLAN_A, 0, "published").await;
    seed_revision(&conn, PLAN_A, 1).await;
    must_succeed(&conn, &component_sql(BUNDLE_A, 1, COMPONENT_1)).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_bundle_component SET plan_revision = 0 \
             WHERE bundle_id = '{BUNDLE_A}' AND plan_revision = 1"
        ),
        "UPDATE of a component under a non-draft plan revision is not permitted",
    )
    .await;
}

/// A rev-share re-split on a published revision — vendor payout, which is why
/// D-104 makes it always-material in the first place.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_published_revisions_rev_share_cannot_be_re_split() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_B, 0).await;
    seed_bundle(&conn, BUNDLE_B, PLAN_B, "sum_of_parts").await;
    must_succeed(&conn, &group_sql(BUNDLE_B, 0, VENDOR_1, 1000, "platform")).await;
    must_succeed(&conn, &party_sql(BUNDLE_B, 0, VENDOR_1, "vendor-a", 9000)).await;
    flip(&conn, PLAN_B, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_bundle_revshare SET share_bp = 5000 \
             WHERE bundle_id = '{BUNDLE_B}' AND plan_revision = 0"
        ),
        "UPDATE of a rev-share party under a non-draft plan revision is not permitted",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The append-only triggers — arm 1 (DELETE, the only statement arm 2 never sees).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker"]
async fn a_published_revisions_component_cannot_be_deleted() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    must_succeed(&conn, &component_sql(BUNDLE_A, 0, COMPONENT_1)).await;
    flip(&conn, PLAN_A, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_bundle_component WHERE bundle_id = '{BUNDLE_A}'"),
        "DELETE of a component under a non-draft plan revision is not permitted",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn a_published_revisions_rev_share_group_cannot_be_deleted() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_C, 0).await;
    seed_bundle(&conn, BUNDLE_C, PLAN_C, "sum_of_parts").await;
    must_succeed(&conn, &group_sql(BUNDLE_C, 0, VENDOR_1, 1000, "platform")).await;
    flip(&conn, PLAN_C, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_bundle_revshare_group WHERE bundle_id = '{BUNDLE_C}'"),
        "DELETE of a rev-share group under a non-draft plan revision is not permitted",
    )
    .await;
}

/// The child trigger answers before the foreign key for a composition row under
/// a bundle that does not exist — because its parent lookup goes **through**
/// `pricing_bundle`. Recorded as a test rather than a comment: it is why
/// `fk_pricing_bundle_component_bundle` has no refusal case of its own.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_component_under_an_absent_bundle_is_refused_by_the_trigger() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;

    must_be_rejected(
        &conn,
        &component_sql(BUNDLE_ABSENT, 0, COMPONENT_1),
        "INSERT of a component under a non-draft plan revision is not permitted",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Positives — without them a table nothing can be written to would pass.
// ---------------------------------------------------------------------------

/// D-105's whole point: one revision holds several components, a group per
/// vendor SKU and a party row per party — and all three verbs run freely under a
/// `draft` parent, which is the other half of copy-on-new-revision.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_draft_revision_holds_a_whole_composition_and_stays_mutable() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;

    must_succeed(&conn, &component_sql(BUNDLE_A, 0, COMPONENT_1)).await;
    must_succeed(&conn, &component_sql(BUNDLE_A, 0, COMPONENT_2)).await;
    must_succeed(&conn, &group_sql(BUNDLE_A, 0, VENDOR_1, 1000, "platform")).await;
    must_succeed(&conn, &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-a", 4500)).await;
    must_succeed(&conn, &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-b", 4500)).await;

    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_bundle_revshare SET share_bp = 4000 \
             WHERE bundle_id = '{BUNDLE_A}' AND party = 'vendor-a'"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_bundle_component \
             WHERE bundle_id = '{BUNDLE_A}' AND component_plan_id = '{COMPONENT_2}'"
        ),
    )
    .await;
}

/// `effective_share_bp` is nullable on the way in and bounded once set — the two
/// halves of D-07's audit trail.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_effective_share_is_absent_until_publish_and_bounded_after() {
    let conn = applied().await;
    seed_revision(&conn, PLAN_A, 0).await;
    seed_bundle(&conn, BUNDLE_A, PLAN_A, "sum_of_parts").await;
    must_succeed(&conn, &group_sql(BUNDLE_A, 0, VENDOR_1, 1000, "platform")).await;
    must_succeed(&conn, &party_sql(BUNDLE_A, 0, VENDOR_1, "vendor-a", 9000)).await;

    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_bundle_revshare SET effective_share_bp = 9000 \
             WHERE bundle_id = '{BUNDLE_A}'"
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_bundle_revshare SET effective_share_bp = 10001 \
             WHERE bundle_id = '{BUNDLE_A}'"
        ),
        "chk_pricing_bundle_revshare_effective_share_bp",
    )
    .await;
}
