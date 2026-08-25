//! The four Slice-8 bundle tables against a real database —
//! `design/08-bundles.md` §6, D-92, D-105 — their keys, their `CHECK`s, and the
//! append-only triggers that freeze a composition with the revision it published
//! under.
//!
//! Everything proved here is a property of the **schema** or of a **statement**,
//! never of a branch in Rust. The three-column and four-column primary keys are
//! what let one revision hold several components, several parties and several
//! groups at all (D-105); the `CHECK`s are evaluated by the engine; the triggers
//! exist precisely for what reaches these tables outside this gear's own
//! repository. The suite therefore drives raw SQL through `common::migrated_db`,
//! because the states it needs — a component row under a published revision, a
//! rev-share party under an abandoned one — are states no typed path will ever
//! produce.
//!
//! Postgres mirrors every rule here as one PL/pgSQL trigger function per table
//! plus the same `CHECK`s, indexes, FKs and PKs; the rosters are pinned by name
//! in `tests/postgres_migrations.rs` and the refusals are executed in
//! `tests/postgres_schema_bundle.rs`, because a measurement on one engine is not
//! a fact about the other.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const PLAN: &str = "22222222-2222-2222-2222-222222222222";
const BUNDLE: &str = "55555555-5555-5555-5555-555555555555";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";
const AUTHORED: &str = "2026-08-06 10:00:00 +00:00";

/// Reject, **and** for the stated reason.
///
/// The fragment is not decoration. These four tables carry several `CHECK`s
/// apiece, composite primary keys, composite foreign keys and an append-only
/// trigger each, and a test that accepted any error naming the table would pass
/// with the guard it means to prove switched off — refused instead by a
/// constraint it never intended to trip.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, table: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .expect_err(&format!("this statement must be rejected: {sql}"));
    let message = err.to_string();
    // The table as well as the fragment, which the siblings one file over
    // (`sqlite_approval_append_only`, `sqlite_tier_band_guard`,
    // `postgres_approval_threshold`) all carry: without it a caller may pass a
    // fragment naming nothing - `UNIQUE constraint failed`, `FOREIGN KEY` - and
    // the case then admits that refusal from any of the four tables this file
    // seeds.
    //
    // `pricing_bundle` is a prefix of the other three, so the three callers passing
    // it get the separation only from their `because`, which each of them supplies
    // as a constraint name. Matching on a suffixed form instead would cost the
    // eight callers whose refusal message carries the bare table with no delimiter
    // after it.
    assert!(
        message.contains(table),
        "the rejection must be about `{table}`, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// Insert one plan revision in `draft`.
async fn insert_revision(conn: &DatabaseConnection, revision: i64) {
    must_succeed(
        conn,
        &format!(
            "INSERT INTO pricing_plan (
                plan_id, revision, tenant_id, lifecycle_state, created_by, created_at_utc)
             VALUES ('{PLAN}', {revision}, '{TENANT}', 'draft', '{ACTOR}', '{AUTHORED}')"
        ),
    )
    .await;
}

/// Move a revision out of `draft`. Both flips this suite uses —
/// `draft -> published` and `draft -> abandoned` — are whitelisted by
/// `pricing_plan`'s own append-only trigger.
async fn flip(conn: &DatabaseConnection, revision: i64, state: &str) {
    must_succeed(
        conn,
        &format!(
            "UPDATE pricing_plan SET lifecycle_state = '{state}' \
             WHERE plan_id = '{PLAN}' AND revision = {revision}"
        ),
    )
    .await;
}

fn insert_bundle(basis: &str, itemization: &str) -> String {
    format!(
        "INSERT INTO pricing_bundle (
            bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
         VALUES ('{BUNDLE}', '{TENANT}', '{PLAN}', '{basis}', '{itemization}')"
    )
}

// ---------------------------------------------------------------------------
// `pricing_bundle` — the bundle's identity, its basis and its itemization.
// ---------------------------------------------------------------------------

/// The table exists, is keyed by `bundle_id`, and carries the `plan_id` FK
/// D-105 added — without which D-92's *"a bundle rides its plan's revisions"*
/// has no join path at all.
#[tokio::test]
async fn a_bundle_row_names_the_plan_whose_revisions_it_rides() {
    let conn = migrated_db().await;
    insert_revision(&conn, 0).await;

    must_succeed(&conn, &insert_bundle("sum_of_parts", "aggregate")).await;

    let plan = scalar(
        &conn,
        &format!("SELECT plan_id AS v FROM pricing_bundle WHERE bundle_id = '{BUNDLE}'"),
    )
    .await;
    assert_eq!(plan, PLAN, "the bundle must name its own plan");
}

/// `price_basis` is `sum_of_parts | own_price` and nothing else
/// (`inst-bb-declared`: *"basis MUST be declared"*).
#[tokio::test]
async fn a_basis_outside_the_declared_pair_is_refused() {
    let conn = migrated_db().await;
    insert_revision(&conn, 0).await;

    must_be_rejected(
        &conn,
        &insert_bundle("sum_of_the_parts", "aggregate"),
        "pricing_bundle",
        "chk_pricing_bundle_price_basis",
    )
    .await;
}

/// `invoice_itemization` is `aggregate | itemize` and nothing else (B3).
#[tokio::test]
async fn an_itemization_outside_the_declared_pair_is_refused() {
    let conn = migrated_db().await;
    insert_revision(&conn, 0).await;

    must_be_rejected(
        &conn,
        &insert_bundle("own_price", "itemised"),
        "pricing_bundle",
        "chk_pricing_bundle_invoice_itemization",
    )
    .await;
}

/// One bundle per plan.
///
/// §6 gives `pricing_bundle` a `plan_id` FK and D-92 has the bundle riding
/// **that** plan's revisions; two bundle rows on one plan would make "the
/// bundle's revisions" ambiguous and give the composition tables — which key on
/// `bundle_id` — two parents inside one revision chain.
///
/// The fragment is the **columns**, not `uq_pricing_bundle_plan`: `SQLite` names
/// the offending columns in a uniqueness violation and never the index. They are
/// still the guard under test and only that one, `(tenant_id, plan_id)` carrying
/// no other uniqueness on this table. Postgres names the index instead, which is
/// why the Postgres proof of this rule lives in `postgres_schema_bundle.rs`
/// rather than being asserted here against a message this engine does not
/// produce.
///
/// **The fragment gained `tenant_id` with `pricing_bundle`** and the move is
/// the assertion, not a loosening: the pair is what the index now spans, and a
/// test still naming the bare column would pass unchanged against an index that
/// had lost its second column entirely.
#[tokio::test]
async fn one_plan_carries_at_most_one_bundle() {
    let conn = migrated_db().await;
    insert_revision(&conn, 0).await;
    must_succeed(&conn, &insert_bundle("sum_of_parts", "aggregate")).await;

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO pricing_bundle (
                bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
             VALUES ('66666666-6666-6666-6666-666666666666', '{TENANT}', '{PLAN}',
                     'own_price', 'itemize')"
        ),
        "pricing_bundle",
        "UNIQUE constraint failed: pricing_bundle.tenant_id, pricing_bundle.plan_id",
    )
    .await;
}

/// The plan slot a tenant can occupy is **its own** — `pricing_bundle`.
///
/// `uq_pricing_bundle_plan` was `UNIQUE (plan_id)` with no tenant, on the one
/// column of this table that is a **client-supplied reference to another table**.
/// So the first tenant to insert a bundle naming a `plan_id` locked every other
/// tenant out of it: the second was refused `BUNDLE_EXISTS_ON_PLAN` pointing at a
/// row invisible to it, in a tenant it cannot see, and `pricing_bundle` has no
/// `DELETE` path anywhere in the API. The index was also narrower than its only
/// reader assumed — `bundle_repo::bundle_of_plan` filters `tenant_id` **and**
/// `plan_id`, so the read and the constraint behind it disagreed about what
/// "taken" meant.
///
/// Raw SQL past the repository, which is this suite's whole charter: with the
/// caller-scope plan read `create_on` now makes, no typed path produces this
/// state, and the index is nevertheless what decides a race and what binds a
/// writer that never went through the repository. Postgres carries the same index
/// and `postgres_schema_bundle.rs` executes the pair there.
#[tokio::test]
async fn two_tenants_may_each_bundle_the_same_plan_id() {
    const OTHER_TENANT: &str = "99999999-9999-9999-9999-999999999999";
    let conn = migrated_db().await;
    insert_revision(&conn, 0).await;
    must_succeed(&conn, &insert_bundle("sum_of_parts", "aggregate")).await;

    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_bundle (
                bundle_id, tenant_id, plan_id, price_basis, invoice_itemization)
             VALUES ('77777777-7777-7777-7777-777777777777', '{OTHER_TENANT}', '{PLAN}',
                     'own_price', 'itemize')"
        ),
    )
    .await;

    let held = scalar(
        &conn,
        &format!("SELECT CAST(count(*) AS TEXT) AS v FROM pricing_bundle WHERE plan_id = '{PLAN}'"),
    )
    .await;
    assert_eq!(held, "2", "one plan id, one bundle slot per tenant");
}

/// **`plan_id` carries no foreign key, and the test says so rather than
/// asserting one.**
///
/// D-105 calls it *"the `plan_id` FK the revision keying presupposes"*, and a
/// foreign key is exactly what cannot be declared on it: `pricing_plan`'s
/// primary key is `(plan_id, revision)`, and the only uniqueness it has on
/// `plan_id` alone lives in two **partial** indexes (`uq_pricing_plan_current`,
/// `uq_pricing_plan_open_draft`), which Postgres will not accept as a foreign
/// key's referent. The reference is real and its enforcement lands one table
/// down, where it **is** expressible — see
/// [`a_component_under_a_bundle_that_does_not_exist_is_refused`].
///
/// Asserting the absence keeps a later reader from "fixing" it and discovering
/// on Postgres what `SQLite` would have accepted.
#[tokio::test]
async fn a_bundle_may_name_a_plan_that_does_not_exist() {
    let conn = migrated_db().await;
    must_succeed(&conn, "PRAGMA foreign_keys = ON").await;

    must_succeed(&conn, &insert_bundle("sum_of_parts", "aggregate")).await;
}

// ---------------------------------------------------------------------------
// The three composition tables — D-92's revision discipline and D-105's keys.
// ---------------------------------------------------------------------------

const COMPONENT_A: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const COMPONENT_B: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const SKU_A: &str = "a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1";
const SKU_B: &str = "b1b1b1b1-b1b1-b1b1-b1b1-b1b1b1b1b1b1";
const VENDOR_X: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";
const PARTY_1: &str = "dddddddd-dddd-dddd-dddd-dddddddddddd";
const PARTY_2: &str = "dddddddd-dddd-dddd-dddd-ddddddddddd2";
const PARTY_3: &str = "dddddddd-dddd-dddd-dddd-ddddddddddd3";
const VENDOR_Y: &str = "cccccccc-cccc-cccc-cccc-ccccccccccc2";
const VENDOR_Z: &str = "cccccccc-cccc-cccc-cccc-ccccccccccc3";

/// A bundle on a draft revision 0, ready for composition rows.
async fn seed_bundle(conn: &DatabaseConnection) -> DatabaseConnection {
    insert_revision(conn, 0).await;
    must_succeed(conn, &insert_bundle("sum_of_parts", "aggregate")).await;
    conn.clone()
}

fn insert_component(revision: i64, component: &str) -> String {
    format!(
        "INSERT INTO pricing_bundle_component (
            bundle_id, plan_revision, component_plan_id, tenant_id, included_sku_id)
         VALUES ('{BUNDLE}', {revision}, '{component}', '{TENANT}', '{SKU_A}')"
    )
}

fn insert_group(revision: i64, vendor: &str, cut: i32, absorber: &str) -> String {
    format!(
        "INSERT INTO pricing_bundle_revshare_group (
            bundle_id, plan_revision, vendor_sku_id, tenant_id,
            platform_cut_bp, residual_absorber_party)
         VALUES ('{BUNDLE}', {revision}, '{vendor}', '{TENANT}', {cut}, '{absorber}')"
    )
}

fn insert_party(revision: i64, vendor: &str, party: &str, share: i32) -> String {
    format!(
        "INSERT INTO pricing_bundle_revshare (
            bundle_id, plan_revision, vendor_sku_id, party, tenant_id, share_bp)
         VALUES ('{BUNDLE}', {revision}, '{vendor}', '{party}', '{TENANT}', {share})"
    )
}

/// D-105's whole point: a revision holds **several** components.
///
/// Under D-92's `(bundle_id, plan_revision)` phrasing a bundle held one, which
/// makes "every referenced component" and the per-market coverage walk rules
/// over data the key cannot represent.
#[tokio::test]
async fn one_revision_holds_several_components() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;

    must_succeed(&conn, &insert_component(0, COMPONENT_A)).await;
    must_succeed(&conn, &insert_component(0, COMPONENT_B)).await;

    let n = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_bundle_component \
             WHERE bundle_id = '{BUNDLE}' AND plan_revision = 0"
        ),
    )
    .await;
    assert_eq!(n, "2", "a revision must hold both components");
}

/// The same component twice under one revision is the duplicate the key exists
/// to refuse.
#[tokio::test]
async fn one_component_appears_once_per_revision() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    must_succeed(&conn, &insert_component(0, COMPONENT_A)).await;

    must_be_rejected(
        &conn,
        &insert_component(0, COMPONENT_A),
        "pricing_bundle_component",
        "pricing_bundle_component.bundle_id, pricing_bundle_component.plan_revision, \
         pricing_bundle_component.component_plan_id",
    )
    .await;
}

/// **A component's minimum quantity has a floor, and it is zero.**
///
/// Neither of this table's two quantity `CHECK`s had a case on either engine,
/// and this is the one that fails **open**: `chk_..._qty_range` only *orders* the
/// pair, so `min_qty = -1` beside a larger `max_qty` satisfies it and lands the
/// moment `chk_..._min_qty` stops refusing. A negative minimum is a component
/// the coverage walk reads as owing quantity — `inst-bc-coverage` takes the
/// floor of the set it quantifies over from this column — and no typed path can
/// spell one, so nothing above the schema would have noticed.
///
/// Asked from both sides: zero is the boundary and is legal, an optional
/// component's minimum **being** zero, so a rule tightened to `> 0` would refuse
/// every one of them.
#[tokio::test]
async fn a_negative_component_minimum_quantity_is_refused() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;

    let with_quantities = |min: i32, max: i32| {
        format!(
            "INSERT INTO pricing_bundle_component (
                bundle_id, plan_revision, component_plan_id, tenant_id,
                included_sku_id, min_qty, max_qty)
             VALUES ('{BUNDLE}', 0, '{COMPONENT_A}', '{TENANT}', '{SKU_A}', {min}, {max})"
        )
    };

    must_be_rejected(
        &conn,
        &with_quantities(-1, 5),
        "pricing_bundle_component",
        "chk_pricing_bundle_component_min_qty",
    )
    .await;
    must_succeed(&conn, &with_quantities(0, 5)).await;
}

/// D-105 again, on the rev-share plane: *"sum to 100% **per** included vendor
/// SKU"* needs a group per vendor SKU and a party row per party.
#[tokio::test]
async fn one_revision_holds_a_group_per_vendor_sku_and_a_party_row_per_party() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;

    must_succeed(&conn, &insert_group(0, VENDOR_X, 1000, "platform")).await;
    must_succeed(&conn, &insert_party(0, VENDOR_X, PARTY_1, 4500)).await;
    must_succeed(
        &conn,
        &insert_party(0, VENDOR_X, "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee", 4500),
    )
    .await;

    let n = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_bundle_revshare \
             WHERE bundle_id = '{BUNDLE}' AND plan_revision = 0 AND vendor_sku_id = '{VENDOR_X}'"
        ),
    )
    .await;
    assert_eq!(n, "2", "one group must hold both parties");
}

/// A party row with no group is refused: the group carries the platform cut, so
/// a party outside one is a share of a total that has no explicit platform cut —
/// which is the structural malformation `REVSHARE_UNBALANCED` names.
#[tokio::test]
async fn a_party_row_outside_its_group_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, "PRAGMA foreign_keys = ON").await;
    seed_bundle(&conn).await;

    // **SQLite names nothing on a foreign key**, so this one refusal cannot go
    // through `must_be_rejected`: the engine prints `FOREIGN KEY constraint
    // failed` and neither the table, the constraint nor the columns. The
    // discriminator has to be built instead of read, and the control below is it -
    // the same insert lands the moment its group exists, so the composite FK to
    // `pricing_bundle_revshare_group` is what refused above and not one of the
    // three CHECKs or the append-only trigger on the same table.
    let orphan = exec(&conn, &insert_party(0, VENDOR_X, PARTY_1, 4500))
        .await
        .expect_err("a party outside its group is refused");
    assert!(
        orphan.to_string().contains("FOREIGN KEY constraint failed"),
        "got: {orphan}"
    );

    must_succeed(&conn, &insert_group(0, VENDOR_X, 1_000, "platform")).await;
    must_succeed(&conn, &insert_party(0, VENDOR_X, PARTY_1, 4500)).await;
}

/// A composition row under a bundle that does not exist is refused — the
/// referential integrity `pricing_bundle.plan_id` could not carry (see
/// [`a_bundle_may_name_a_plan_that_does_not_exist`]) lands here, where the
/// referent **is** a primary key.
///
/// **The append-only trigger is what answers, not the foreign key**, and the
/// test says so rather than asserting the guard it expected. The trigger's
/// predicate resolves the owning revision *through* `pricing_bundle`, so an
/// absent bundle makes its `NOT EXISTS` true and it aborts before the foreign
/// key is ever evaluated. The guarantee — no composition row under a bundle that
/// does not exist — holds either way; which guard delivers it is what changed.
/// The foreign key is therefore unreachable on INSERT and is kept for the verbs
/// and backends where it is not, and because dropping a declared reference
/// because one path happens to shadow it is how the reference stops being true.
#[tokio::test]
async fn a_component_under_a_bundle_that_does_not_exist_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, "PRAGMA foreign_keys = ON").await;
    insert_revision(&conn, 0).await;

    must_be_rejected(
        &conn,
        &insert_component(0, COMPONENT_A),
        "pricing_bundle_component",
        "under a non-draft plan revision is not permitted",
    )
    .await;
}

/// Basis points are a bounded scale on every column that carries them.
#[tokio::test]
async fn a_share_outside_the_basis_point_scale_is_refused() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    must_succeed(&conn, &insert_group(0, VENDOR_X, 1000, "platform")).await;

    // Both ends and both boundaries, mirroring the Postgres twin. This is the
    // engine the dev boot and the gear e2e run against, so a CHECK tightened here
    // to `> 0 AND < 10000` would refuse a vendor carrying none of the revenue and
    // one carrying all of it, and one-ended cases would not see it.
    must_be_rejected(
        &conn,
        &insert_party(0, VENDOR_X, PARTY_1, 10_001),
        "pricing_bundle_revshare",
        "chk_pricing_bundle_revshare_share_bp",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_party(0, VENDOR_X, PARTY_2, -1),
        "pricing_bundle_revshare",
        "chk_pricing_bundle_revshare_share_bp",
    )
    .await;
    must_succeed(&conn, &insert_party(0, VENDOR_X, PARTY_2, 0)).await;
    must_succeed(&conn, &insert_party(0, VENDOR_X, PARTY_3, 10_000)).await;
}

/// The platform cut is bounded on the same scale.
#[tokio::test]
async fn a_platform_cut_outside_the_basis_point_scale_is_refused() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;

    // Both ends and both boundaries, for the share CHECK's reason one table over.
    must_be_rejected(
        &conn,
        &insert_group(0, VENDOR_X, -1, "platform"),
        "pricing_bundle_revshare_group",
        "chk_pricing_bundle_revshare_group_platform_cut_bp",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_group(0, VENDOR_Y, 10_001, "platform"),
        "pricing_bundle_revshare_group",
        "chk_pricing_bundle_revshare_group_platform_cut_bp",
    )
    .await;
    must_succeed(&conn, &insert_group(0, VENDOR_Y, 0, "platform")).await;
    must_succeed(&conn, &insert_group(0, VENDOR_Z, 10_000, "platform")).await;
}

// ---------------------------------------------------------------------------
// D-92 — a published revision's composition is immutable with it.
// ---------------------------------------------------------------------------

/// The defect D-92 was written to close: a draft recomposition must not reach a
/// published revision's rows.
#[tokio::test]
async fn a_component_cannot_be_appended_to_a_published_revision() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    flip(&conn, 0, "published").await;

    must_be_rejected(
        &conn,
        &insert_component(0, COMPONENT_A),
        "pricing_bundle_component",
        "under a non-draft plan revision is not permitted",
    )
    .await;
}

/// UPDATE is guarded at **both** ends — the revision the row is bound to now and
/// the revision it would land under. Re-pointing `plan_revision` is precisely
/// how one appends to a frozen revision without issuing an INSERT.
///
/// The revision sequence here is forced rather than chosen:
/// `uq_pricing_plan_open_draft` admits **one** open draft per plan, so revision
/// 0 has to publish before revision 1 can be opened. That is exactly the
/// copy-on-new-revision sequence D-92 describes, which is why the fixture reads
/// like one.
#[tokio::test]
async fn a_component_cannot_be_repointed_onto_a_published_revision() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    flip(&conn, 0, "published").await;
    insert_revision(&conn, 1).await;
    must_succeed(&conn, &insert_component(1, COMPONENT_A)).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_bundle_component SET plan_revision = 0 \
             WHERE bundle_id = '{BUNDLE}' AND plan_revision = 1"
        ),
        "pricing_bundle_component",
        "under a non-draft plan revision is not permitted",
    )
    .await;
}

/// A published revision's rev-share is frozen with it too — a re-split is vendor
/// payout, which is why D-104 makes it always-material in the first place.
#[tokio::test]
async fn a_published_revisions_rev_share_cannot_be_re_split() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    must_succeed(&conn, &insert_group(0, VENDOR_X, 1000, "platform")).await;
    must_succeed(&conn, &insert_party(0, VENDOR_X, PARTY_1, 9000)).await;
    flip(&conn, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_bundle_revshare SET share_bp = 5000 \
             WHERE bundle_id = '{BUNDLE}' AND plan_revision = 0 AND party = '{PARTY_1}'"
        ),
        "pricing_bundle_revshare",
        "under a non-draft plan revision is not permitted",
    )
    .await;
}

/// A draft revision's own copies stay freely mutable and deletable — the other
/// half of copy-on-new-revision, without which the discipline would freeze
/// authoring itself.
///
/// **The edit moves the column and is read back.** It used to `SET
/// included_sku_id = '{SKU_A}'` on a row [`insert_component`] had already written
/// with `SKU_A` and never read the column at all, so it proved only that the
/// append-only trigger did not abort a statement that changes nothing — not that
/// an edit to a draft composition lands, which is the half it is named for.
/// Measured 2026-08-20: the read-back below is red against the old no-op
/// statement.
#[tokio::test]
async fn a_draft_revisions_composition_stays_mutable() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    must_succeed(&conn, &insert_component(0, COMPONENT_A)).await;

    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_bundle_component SET included_sku_id = '{SKU_B}' \
             WHERE bundle_id = '{BUNDLE}' AND plan_revision = 0"
        ),
    )
    .await;
    assert_eq!(
        scalar(
            &conn,
            &format!(
                "SELECT CAST(included_sku_id AS TEXT) AS v FROM pricing_bundle_component \
                 WHERE bundle_id = '{BUNDLE}' AND plan_revision = 0"
            ),
        )
        .await,
        SKU_B,
        "the draft revision's composition really moved, not merely went unrefused"
    );
    must_succeed(
        &conn,
        &format!(
            "DELETE FROM pricing_bundle_component \
             WHERE bundle_id = '{BUNDLE}' AND plan_revision = 0"
        ),
    )
    .await;
}

/// **`abandoned` is not `draft`** — the same ordering constraint
/// `pricing_plan_addon_rule` records, which forces a repository to drop child
/// rows **before** it flips the revision.
#[tokio::test]
async fn an_abandoned_revisions_composition_is_frozen_too() {
    let conn = migrated_db().await;
    seed_bundle(&conn).await;
    flip(&conn, 0, "abandoned").await;

    must_be_rejected(
        &conn,
        &insert_component(0, COMPONENT_A),
        "pricing_bundle_component",
        "under a non-draft plan revision is not permitted",
    )
    .await;
}
