//! The three Slice-9 overlay tables — `pricing_price_overlay` and its two
//! revision-scoped children — proved by **executing the statement each object
//! must refuse**, on Postgres.
//!
//! # Why this suite exists
//!
//! `pricing_price_overlay`…`…000034` had never run on the backend they target.
//! `tests/sqlite_overlay_store.rs` proves the **mirror**, which is a different
//! set of objects: four fixed-message `RAISE(ABORT, …)` triggers on the header
//! where Postgres has one PL/pgSQL function with three arms, three per child
//! table where Postgres has one, and a `WHERE NOT EXISTS` subquery in the
//! trigger body where Postgres does a `SELECT … INTO`.
//! `tests/postgres_migrations.rs` pins the rosters by name and issues no DML.
//! This suite is the other half.
//!
//! # Two facts this engine states differently, and both matter here
//!
//! * **The uniqueness message.** `SQLite` names the **index** only for an
//!   *expression* index and the column list otherwise, so its suite asserts the
//!   column list for `uq_pricing_price_overlay_precedence` and the index name
//!   for `uq_pricing_price_overlay_line_key`. Postgres names the index either
//!   way, so this suite asserts the **name** for both — which is the sharper
//!   assertion and the one that could not be made over there.
//! * **The `COALESCE` sentinels differ by design.** `cohort` coalesces to
//!   `'-infinity'::timestamptz` here and to `''` on `SQLite`, because each
//!   sentinel is type-native to its column. That the *rule* is the same on both
//!   is precisely what a measurement on one engine cannot establish.
//!
//! # Objects this suite deliberately does not test by refusal
//!
//! `idx_pricing_price_overlay_scope`, `idx_pricing_price_overlay_line_revision`,
//! `idx_pricing_price_overlay_line_plan` and
//! `idx_pricing_price_overlay_line_amount_tenant` are non-unique, non-partial
//! indexes. They refuse nothing; their presence is pinned by name in
//! `tests/postgres_migrations.rs` and that is the whole of what can be said.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_overlay -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const OVERLAY_A: &str = "aaaaaaaa-0000-0000-0000-00000000000a";
const OVERLAY_B: &str = "aaaaaaaa-0000-0000-0000-00000000000b";
const PLAN_1: &str = "22222222-0000-0000-0000-000000000001";
const LINE_1: &str = "cccccccc-0000-0000-0000-000000000001";
const LINE_2: &str = "cccccccc-0000-0000-0000-000000000002";
const LINE_3: &str = "cccccccc-0000-0000-0000-000000000003";
/// A grandfathered generation's cutover instant. Dated 2099 so "future" stays a
/// fact.
const COHORT: &str = "2099-03-01T00:00:00Z";

/// A second tenant, for the one guard here that is a statement about **whose**
/// rows these are: the append-only trigger's parent-tenant arm.
const TENANT_TWO: &str = "11111111-1111-1111-1111-1111000000b2";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A fresh database carrying the applied chain, on the one shared server.
///
/// The connection is a **plain** one: every statement here is raw SQL that
/// deliberately reaches past `overlay_repo`, because the repository is exactly
/// the layer that cannot see a guard stop refusing.
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

fn insert_overlay(overlay: &str, revision: i64, state: &str, precedence: i32) -> String {
    format!(
        "INSERT INTO bss.pricing_price_overlay (
            price_overlay_id, revision, tenant_id, lifecycle_state,
            scope_class, scope_value, precedence, tax_basis, disclosure, target_ref)
         VALUES ('{overlay}', {revision}, '{TENANT}', '{state}',
                 'brand', 'acme', {precedence}, 'delegated_tariffs', 'restricted', '{{}}'::jsonb)"
    )
}

fn insert_line(line: &str, overlay: &str, revision: i64, plan: &str, cohort: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_price_overlay_line (
            line_id, price_overlay_id, overlay_revision, tenant_id,
            plan_id, cohort, adjustment_kind, magnitude_kind, adjustment_value)
         VALUES ('{line}', '{overlay}', {revision}, '{TENANT}',
                 {plan}, {cohort}, 'discount', 'percent_bp', 1500)"
    )
}

async fn flip(conn: &DatabaseConnection, overlay: &str, revision: i64, state: &str) {
    must_succeed(
        conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET lifecycle_state = '{state}' \
             WHERE price_overlay_id = '{overlay}' AND revision = {revision}"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_price_overlay` — the header.
// ---------------------------------------------------------------------------

/// **D-107's partial predicate, on the engine that runs in production.**
///
/// A draft revision of a published overlay reuses its own `(class, precedence)`
/// and is admitted; a **different** overlay claiming the same slot is refused,
/// and Postgres names the index.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_precedence_index_is_partial_on_published() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    flip(&conn, OVERLAY_A, 0, "published").await;

    // The overlay's own successor, at the same precedence: admitted.
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 1, "draft", 10)).await;

    // A different overlay, claiming the published slot: refused, by name.
    must_succeed(&conn, &insert_overlay(OVERLAY_B, 0, "draft", 10)).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET lifecycle_state = 'published' \
             WHERE price_overlay_id = '{OVERLAY_B}' AND revision = 0"
        ),
        "uq_pricing_price_overlay_precedence",
    )
    .await;
}

/// One open draft revision per overlay (D-92).
#[tokio::test]
#[ignore = "requires Docker"]
async fn an_overlay_holds_at_most_one_open_draft_revision() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;

    must_be_rejected(
        &conn,
        &insert_overlay(OVERLAY_A, 1, "draft", 10),
        "uq_pricing_price_overlay_open_draft",
    )
    .await;
}

/// The classless sentinel's biconditional, both ways.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_global_class_and_the_empty_scope_value_imply_each_other() {
    let conn = applied().await;

    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay (
                price_overlay_id, revision, tenant_id, lifecycle_state, scope_class,
                scope_value, precedence, tax_basis, disclosure, target_ref)
             VALUES ('{OVERLAY_A}', 0, '{TENANT}', 'draft', 'global', '', 10,
                     'delegated_tariffs', 'restricted', '{{}}'::jsonb)"
        ),
    )
    .await;

    for (class, value, precedence) in [("global", "everyone", 11), ("brand", "", 12)] {
        must_be_rejected(
            &conn,
            &format!(
                "INSERT INTO bss.pricing_price_overlay (
                    price_overlay_id, revision, tenant_id, lifecycle_state, scope_class,
                    scope_value, precedence, tax_basis, disclosure, target_ref)
                 VALUES ('{OVERLAY_B}', 0, '{TENANT}', 'draft', '{class}', '{value}',
                         {precedence}, 'delegated_tariffs', 'restricted', '{{}}'::jsonb)"
            ),
            "chk_pricing_price_overlay_scope_value",
        )
        .await;
    }
}

/// The three closed vocabularies, and the interval sanity check.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_headers_check_constraints_refuse_their_own_violations() {
    let conn = applied().await;

    for (column, value, constraint) in [
        (
            "tax_basis",
            "unspecified",
            "chk_pricing_price_overlay_tax_basis",
        ),
        (
            "disclosure",
            "internal",
            "chk_pricing_price_overlay_disclosure",
        ),
        (
            "lifecycle_state",
            "retired",
            "chk_pricing_price_overlay_lifecycle_state",
        ),
        (
            "scope_class",
            "reseller",
            "chk_pricing_price_overlay_scope_class",
        ),
    ] {
        let mut columns = vec![
            ("lifecycle_state", "'draft'".to_owned()),
            ("scope_class", "'brand'".to_owned()),
            ("scope_value", "'acme'".to_owned()),
            ("tax_basis", "'delegated_tariffs'".to_owned()),
            ("disclosure", "'restricted'".to_owned()),
        ];
        for entry in &mut columns {
            if entry.0 == column {
                entry.1 = format!("'{value}'");
            }
        }
        let names: Vec<&str> = columns.iter().map(|c| c.0).collect();
        let values: Vec<String> = columns.iter().map(|c| c.1.clone()).collect();
        must_be_rejected(
            &conn,
            &format!(
                "INSERT INTO bss.pricing_price_overlay (
                    price_overlay_id, revision, tenant_id, precedence, target_ref, {})
                 VALUES ('{OVERLAY_A}', 0, '{TENANT}', 10, '{{}}'::jsonb, {})",
                names.join(", "),
                values.join(", ")
            ),
            constraint,
        )
        .await;
    }

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay (
                price_overlay_id, revision, tenant_id, lifecycle_state, scope_class,
                scope_value, precedence, effective_from, effective_to, tax_basis,
                disclosure, target_ref)
             VALUES ('{OVERLAY_A}', 0, '{TENANT}', 'draft', 'brand', 'acme', 10,
                     '2099-06-01T00:00:00Z', '2099-01-01T00:00:00Z',
                     'delegated_tariffs', 'restricted', '{{}}'::jsonb)"
        ),
        "chk_pricing_price_overlay_interval",
    )
    .await;

    // **A revision is a position in a chain, and the chain starts at zero.**
    // `chk_pricing_price_overlay_revision` fails open: no writer in this gear
    // mints a negative revision, so dropping it breaks nothing and admits a row
    // that sorts **ahead of** the overlay's own origin. `uq_..._open_draft`
    // admits one draft per overlay wherever it sorts, so the row would take the
    // draft slot while every "latest revision" read answered from the real head
    // — and the line table's composite foreign key would then let lines hang off
    // a predecessor of revision 0. Reached here rather than through
    // `overlay_repo`, which numbers revisions itself and can never spell one.
    must_be_rejected(
        &conn,
        &insert_overlay(OVERLAY_A, -1, "draft", 10),
        "chk_pricing_price_overlay_revision",
    )
    .await;
    // Zero is the boundary and is the origin every chain starts at, so the
    // constraint is pinned from below as well: `> 0` would refuse every overlay.
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;

    // The entity tag is the same shape read the same way, and `overlay_repo`
    // calls a value no caller can address `CorruptRow`. The vehicle is an UPDATE
    // of the draft just landed: the frozen-column trigger stands only over rows
    // that have left `draft`, so here the constraint is the one guard that can
    // answer.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET row_version = -1 \
             WHERE price_overlay_id = '{OVERLAY_A}' AND revision = 0"
        ),
        "chk_pricing_price_overlay_row_version",
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET row_version = 0 \
             WHERE price_overlay_id = '{OVERLAY_A}' AND revision = 0"
        ),
    )
    .await;
}

/// The append-only function's three arms: frozen content, the unsanctioned flip,
/// and the delete off the draft plane.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_published_revision_is_frozen_and_leaves_by_one_flip_only() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    flip(&conn, OVERLAY_A, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET precedence = 20 \
             WHERE price_overlay_id = '{OVERLAY_A}' AND revision = 0"
        ),
        "is frozen",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_price_overlay \
             WHERE price_overlay_id = '{OVERLAY_A}' AND revision = 0"
        ),
        "is not permitted",
    )
    .await;

    flip(&conn, OVERLAY_A, 0, "superseded").await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET lifecycle_state = 'published' \
             WHERE price_overlay_id = '{OVERLAY_A}' AND revision = 0"
        ),
        "not a sanctioned flip",
    )
    .await;
}

/// A draft never leaves straight to `superseded` — that would mint a superseded
/// revision that never published, which the projector would then source from.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_draft_cannot_flip_straight_to_superseded() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay SET lifecycle_state = 'superseded' \
             WHERE price_overlay_id = '{OVERLAY_A}' AND revision = 0"
        ),
        "not a sanctioned flip",
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_price_overlay_line` — the null-safe key and the `CHECK`s.
// ---------------------------------------------------------------------------

/// **The null-safe line key, and Postgres names the index.**
///
/// The three cases §6 spells as *"one default line, one line per plan, one per
/// `(plan, sku)`"* — each of which a plain `UNIQUE` over three nullable columns
/// would admit, because NULLs are distinct inside one on this engine too — plus
/// D-78's cohort distinctness, which must **not** collide.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_line_key_is_null_safe_on_all_three_nullable_columns() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;

    // Two list-default lines: plan, sku and cohort all NULL.
    must_succeed(&conn, &insert_line(LINE_1, OVERLAY_A, 0, "NULL", "NULL")).await;
    must_be_rejected(
        &conn,
        &insert_line(LINE_2, OVERLAY_A, 0, "NULL", "NULL"),
        "uq_pricing_price_overlay_line_key",
    )
    .await;

    // Two lines on one plan: sku and cohort NULL.
    must_succeed(
        &conn,
        &insert_line(LINE_2, OVERLAY_A, 0, &format!("'{PLAN_1}'"), "NULL"),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_line(LINE_3, OVERLAY_A, 0, &format!("'{PLAN_1}'"), "NULL"),
        "uq_pricing_price_overlay_line_key",
    )
    .await;

    // Two lines on one `(plan, sku)`: cohort NULL. This is the third of §6's
    // three, and the `COALESCE(target_sku, '')` component is exercised in the
    // **collision** direction only here.
    let sku_line = |line: &str| {
        format!(
            "INSERT INTO bss.pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                plan_id, target_sku, adjustment_kind, magnitude_kind, adjustment_value)
             VALUES ('{line}', '{OVERLAY_A}', 0, '{TENANT}',
                     '{PLAN_1}', 'sku-a', 'discount', 'percent_bp', 1500)"
        )
    };
    must_succeed(&conn, &sku_line(LINE_3)).await;
    must_be_rejected(
        &conn,
        &sku_line("cccccccc-0000-0000-0000-000000000004"),
        "uq_pricing_price_overlay_line_key",
    )
    .await;

    // ...and a cohort-targeted line on the same plan is a **different** key
    // (D-78): disjoint by eligibility, so it never collides.
    must_succeed(
        &conn,
        &insert_line(
            "cccccccc-0000-0000-0000-000000000005",
            OVERLAY_A,
            0,
            &format!("'{PLAN_1}'"),
            &format!("'{COHORT}'"),
        ),
    )
    .await;
}

/// The line's own `CHECK`s, each named.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_lines_check_constraints_refuse_their_own_violations() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;

    let line = |plan: &str, sku: &str, cohort: &str, kind: &str, mk: &str, value: &str| {
        format!(
            "INSERT INTO bss.pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                plan_id, target_sku, cohort, adjustment_kind, magnitude_kind, adjustment_value)
             VALUES ('{LINE_1}', '{OVERLAY_A}', 0, '{TENANT}',
                     {plan}, {sku}, {cohort}, '{kind}', '{mk}', {value})"
        )
    };

    for (sql, constraint) in [
        (
            line(
                "NULL",
                "NULL",
                &format!("'{COHORT}'"),
                "discount",
                "percent_bp",
                "1500",
            ),
            "chk_pricing_price_overlay_line_cohort_needs_plan",
        ),
        (
            line("NULL", "'sku-a'", "NULL", "discount", "percent_bp", "1500"),
            "chk_pricing_price_overlay_line_sku_needs_plan",
        ),
        (
            line("NULL", "NULL", "NULL", "discount", "percent_bp", "NULL"),
            "chk_pricing_price_overlay_line_magnitude_pairing",
        ),
        (
            line("NULL", "NULL", "NULL", "discount", "amount", "1500"),
            "chk_pricing_price_overlay_line_magnitude_pairing",
        ),
        (
            line("NULL", "NULL", "NULL", "fixed", "percent_bp", "5000"),
            "chk_pricing_price_overlay_line_fixed_is_amount",
        ),
        (
            line("NULL", "NULL", "NULL", "discount", "percent_bp", "15000"),
            "chk_pricing_price_overlay_line_discount_ceiling",
        ),
        (
            line("NULL", "NULL", "NULL", "markup", "percent_bp", "0"),
            "chk_pricing_price_overlay_line_magnitude_positive",
        ),
        (
            line("NULL", "NULL", "NULL", "sidegrade", "percent_bp", "1500"),
            "chk_pricing_price_overlay_line_adjustment_kind",
        ),
        // **D-08's other half**: the magnitude *kind* is a closed pair. The
        // pairing constraint above only relates `percent_bp` to the presence of
        // a bp value, so a third token with no value satisfies it — `ratio` here
        // passes every other `CHECK` on the row and lands the moment this one
        // stops refusing, at which point `overlay_rules` reads a magnitude it
        // has no arm for on a line the store said was well formed.
        (
            line("NULL", "NULL", "NULL", "discount", "ratio", "NULL"),
            "chk_pricing_price_overlay_line_magnitude_kind",
        ),
        // **The nil uuid is the line key's `plan_id` sentinel, and it is
        // spellable from the wire.** `uq_..._line_key` coalesces a NULL plan to
        // it, so a line naming it explicitly keys as the list-default line and
        // collides with it — or, worse, takes the slot first and makes the real
        // default line unauthorable. The module doc calls this the constraint
        // that makes the index self-enforcing; `overlay_repo` refuses it too,
        // which is exactly why only a raw statement can reach the `CHECK`.
        (
            line(
                "'00000000-0000-0000-0000-000000000000'",
                "NULL",
                "NULL",
                "discount",
                "percent_bp",
                "1500",
            ),
            "chk_pricing_price_overlay_line_plan_id_not_nil",
        ),
        // The same argument on the second forgeable sentinel: `COALESCE(target_sku,
        // '')` means a blank SKU keys as "no SKU". The plan is named because
        // `chk_..._sku_needs_plan` would otherwise answer first and this case
        // would prove that rule twice instead of this one.
        (
            line(
                &format!("'{PLAN_1}'"),
                "''",
                "NULL",
                "discount",
                "percent_bp",
                "1500",
            ),
            "chk_pricing_price_overlay_line_target_sku_present",
        ),
    ] {
        must_be_rejected(&conn, &sql, constraint).await;
    }
}

/// A line under a published revision is frozen — the D-92 property the
/// acceptance criteria state as *"the published revision keeps serving
/// unchanged"*.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_lines_of_a_published_revision_are_frozen() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    must_succeed(&conn, &insert_line(LINE_1, OVERLAY_A, 0, "NULL", "NULL")).await;
    flip(&conn, OVERLAY_A, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay_line SET adjustment_value = 9000 \
             WHERE line_id = '{LINE_1}' AND overlay_revision = 0"
        ),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_price_overlay_line \
             WHERE line_id = '{LINE_1}' AND overlay_revision = 0"
        ),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_line(LINE_2, OVERLAY_A, 0, &format!("'{PLAN_1}'"), "NULL"),
        "is not permitted",
    )
    .await;
}

/// **The trigger's `NEW` arm**: re-pointing a line from a draft revision onto a
/// frozen one is how a line would otherwise be appended to a published revision
/// without an INSERT ever being issued.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_line_cannot_be_re_pointed_onto_a_frozen_revision() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    flip(&conn, OVERLAY_A, 0, "published").await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 1, "draft", 10)).await;
    must_succeed(&conn, &insert_line(LINE_1, OVERLAY_A, 1, "NULL", "NULL")).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay_line SET overlay_revision = 0 \
             WHERE line_id = '{LINE_1}' AND overlay_revision = 1"
        ),
        "is not permitted",
    )
    .await;
}

/// A line belongs to the tenant its own overlay revision belongs to.
///
/// `fk_pricing_price_overlay_line_overlay` covers
/// `(price_overlay_id, overlay_revision)` alone, while the table is
/// `#[secure(tenant_col = "tenant_id")]` — so without the trigger's
/// parent-tenant arm the key admits a line naming somebody else's tenant, on the
/// one table of this pair whose child *does* carry the tenant in its own foreign
/// key (`pricing_price_overlay_line_amount`) and would then inherit the wrong
/// one. `INSERT` and `UPDATE` both: the arm sits on the function's `NEW` path and
/// a draft revision's line is mutable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_line_may_not_claim_a_tenant_its_overlay_does_not_belong_to() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;

    let foreign = format!(
        "INSERT INTO bss.pricing_price_overlay_line (
            line_id, price_overlay_id, overlay_revision, tenant_id,
            plan_id, cohort, adjustment_kind, magnitude_kind, adjustment_value)
         VALUES ('{LINE_1}', '{OVERLAY_A}', 0, '{TENANT_TWO}',
                 '{PLAN_1}', NULL, 'discount', 'percent_bp', 1500)"
    );
    must_be_rejected(
        &conn,
        &foreign,
        "belongs to another tenant and may not hold this line",
    )
    .await;

    // The same line under the overlay's own tenant lands, so the refusal above is
    // about the tenant and about nothing else the row carries.
    must_succeed(
        &conn,
        &insert_line(LINE_1, OVERLAY_A, 0, &format!("'{PLAN_1}'"), "NULL"),
    )
    .await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay_line SET tenant_id = '{TENANT_TWO}' \
             WHERE line_id = '{LINE_1}'"
        ),
        "belongs to another tenant and may not hold this line",
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_price_overlay_line_amount`.
// ---------------------------------------------------------------------------

/// One value per `(line, revision, currency)`, `>= 0`, and the ISO 4217 shape.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_amount_tables_key_and_checks_hold() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                adjustment_kind, magnitude_kind)
             VALUES ('{LINE_1}', '{OVERLAY_A}', 0, '{TENANT}', 'fixed', 'amount')"
        ),
    )
    .await;

    let amount = |currency: &str, value: i64| {
        format!(
            "INSERT INTO bss.pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE_1}', 0, '{currency}', '{TENANT}', {value})"
        )
    };

    must_succeed(&conn, &amount("EUR", 5000)).await;
    must_succeed(&conn, &amount("USD", 5500)).await;
    // Zero is admitted: a `fixed 0` line prices a market at nothing (D-67).
    must_succeed(&conn, &amount("GBP", 0)).await;

    must_be_rejected(
        &conn,
        &amount("EUR", 6000),
        "pricing_price_overlay_line_amount_pkey",
    )
    .await;
    must_be_rejected(
        &conn,
        &amount("CHF", -1),
        "chk_pricing_price_overlay_line_amount_value_minor",
    )
    .await;
    must_be_rejected(
        &conn,
        &amount("EURO", 100),
        "chk_pricing_price_overlay_line_amount_currency",
    )
    .await;
}

/// The amount table's freeze reaches its parent **through the line**, which is
/// the one join more than the line's own trigger makes.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_amounts_of_a_published_revision_are_frozen() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                adjustment_kind, magnitude_kind)
             VALUES ('{LINE_1}', '{OVERLAY_A}', 0, '{TENANT}', 'fixed', 'amount')"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE_1}', 0, 'EUR', '{TENANT}', 5000)"
        ),
    )
    .await;
    flip(&conn, OVERLAY_A, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay_line_amount SET value_minor = 1 \
             WHERE line_id = '{LINE_1}' AND overlay_revision = 0 AND currency = 'EUR'"
        ),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE_1}', 0, 'USD', '{TENANT}', 5500)"
        ),
        "is not permitted",
    )
    .await;
}

/// **The amount table's `OLD` arm**, which is the only arm this statement can
/// trip.
///
/// The row's destination is a legal draft, so the `NEW` arm passes and the whole
/// guard rests on the arm asking where the row comes **from**. Postgres had this
/// right; the `SQLite` mirror keyed the same arm on `line_id` alone and admitted
/// the statement, moving a published revision's money onto a draft — so this case
/// and its `SQLite` twin exist to keep the two engines saying one thing.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_published_revisions_amounts_cannot_be_re_pointed_onto_a_draft() {
    let conn = applied().await;
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 0, "draft", 10)).await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                adjustment_kind, magnitude_kind)
             VALUES ('{LINE_1}', '{OVERLAY_A}', 0, '{TENANT}', 'fixed', 'amount')"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE_1}', 0, 'EUR', '{TENANT}', 5000)"
        ),
    )
    .await;
    flip(&conn, OVERLAY_A, 0, "published").await;

    // The successor exists and is a draft, so the destination is legal.
    must_succeed(&conn, &insert_overlay(OVERLAY_A, 1, "draft", 10)).await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO bss.pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                adjustment_kind, magnitude_kind)
             VALUES ('{LINE_1}', '{OVERLAY_A}', 1, '{TENANT}', 'fixed', 'amount')"
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_price_overlay_line_amount SET overlay_revision = 1 \
             WHERE line_id = '{LINE_1}' AND overlay_revision = 0"
        ),
        "is not permitted",
    )
    .await;
}

// ---------------------------------------------------------------------------
// The four taxonomies.
// ---------------------------------------------------------------------------

/// Slice 4's four scope-value universes, on the engine they will ship on.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_four_taxonomies_hold_their_key_and_their_state() {
    let conn = applied().await;

    for table in [
        "pricing_region_taxonomy",
        "pricing_brand_taxonomy",
        "pricing_partner_taxonomy",
        "pricing_org_tier_taxonomy",
    ] {
        let insert = |value: &str, state: &str| {
            format!(
                "INSERT INTO bss.{table} (tenant_id, value, display_name, state) \
                 VALUES ('{TENANT}', '{value}', 'A label', '{state}')"
            )
        };
        must_succeed(&conn, &insert("eu-west", "active")).await;
        must_be_rejected(
            &conn,
            &insert("eu-west", "retired"),
            &format!("{table}_pkey"),
        )
        .await;
        must_be_rejected(
            &conn,
            &insert("eu-north", "deprecated"),
            &format!("chk_{table}_state"),
        )
        .await;
        must_be_rejected(
            &conn,
            &insert("", "active"),
            &format!("chk_{table}_value_present"),
        )
        .await;
    }
}
