//! The three Slice-9 overlay tables against a real database —
//! `design/09-price-overlays.md` §6, D-42, D-67, D-78, D-92, D-107, D-138.
//!
//! Everything proved here is a property of the **schema** or of a **statement**,
//! never of a branch in Rust: the two-column primary key that makes an overlay a
//! revision chain, the **partial** precedence index D-107 requires, the
//! null-safe line key D-42 and D-78 built, the `CHECK`s the engine evaluates,
//! and the append-only triggers that freeze a published revision's lines. The
//! suite drives raw SQL through `common::migrated_db` because the states it needs
//! — a line under a published revision, a second draft of one overlay — are
//! states no typed path will ever produce, and because a repository that writes
//! only valid values catches a constraint that got *narrower* and never one that
//! stopped refusing.
//!
//! Postgres mirrors every rule here; the rosters are pinned by name in
//! `tests/postgres_migrations.rs` and the refusals are executed in
//! `tests/postgres_schema_overlay.rs`, because a measurement on one engine is not
//! a fact about the other — and here that is sharper than usual: the line key is
//! an **expression** index, which changes `SQLite`'s violation message from the
//! column list to `index '<name>'` while Postgres names the index either way.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const OVERLAY: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const OTHER_OVERLAY: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const PLAN: &str = "22222222-2222-2222-2222-222222222222";
const OTHER_PLAN: &str = "33333333-3333-3333-3333-333333333333";
const LINE: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";
const OTHER_LINE: &str = "dddddddd-dddd-dddd-dddd-dddddddddddd";
/// A grandfathered generation's cutover instant (D-78). Dated 2099 so "future"
/// stays a fact.
const COHORT: &str = "2099-03-01 00:00:00 +00:00";

/// Reject, **and** for the stated reason.
///
/// The fragment is a constraint or index name, never a table name: a fragment
/// naming the table is satisfied by `no such table`, which is what a suite
/// written before its migration would assert against forever.
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

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// One overlay revision row, spelled out so every case can vary one field.
#[allow(clippy::too_many_arguments, reason = "one argument per column varied")]
fn insert_overlay(
    overlay: &str,
    revision: i64,
    state: &str,
    scope_class: &str,
    scope_value: &str,
    precedence: i32,
    tax_basis: &str,
    disclosure: &str,
) -> String {
    format!(
        "INSERT INTO pricing_price_overlay (
            price_overlay_id, revision, tenant_id, lifecycle_state,
            scope_class, scope_value, precedence, tax_basis, disclosure, target_ref)
         VALUES ('{overlay}', {revision}, '{TENANT}', '{state}',
                 '{scope_class}', '{scope_value}', {precedence}, '{tax_basis}',
                 '{disclosure}', '{{}}')"
    )
}

/// The ordinary overlay: a `brand`-scoped draft at precedence 10.
fn draft_overlay(overlay: &str, revision: i64) -> String {
    insert_overlay(
        overlay,
        revision,
        "draft",
        "brand",
        "acme",
        10,
        "delegated_tariffs",
        "restricted",
    )
}

/// One line of one overlay revision. `plan`, `sku` and `cohort` are SQL
/// fragments so a case can pass `NULL`.
///
/// Nine arguments because the line **has** nine authored columns and every one
/// of them is varied by some case below — the `CHECK`s under test are precisely
/// the pairings between them, so a builder struct with defaults would let a case
/// prove a rule while leaving the field it is about unset.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per authored column; the pairings between them are what \
              this suite tests, so no default may hide one"
)]
fn insert_line(
    line: &str,
    overlay: &str,
    revision: i64,
    plan: &str,
    sku: &str,
    cohort: &str,
    kind: &str,
    magnitude_kind: &str,
    value: &str,
) -> String {
    format!(
        "INSERT INTO pricing_price_overlay_line (
            line_id, price_overlay_id, overlay_revision, tenant_id,
            plan_id, target_sku, cohort, adjustment_kind, magnitude_kind, adjustment_value)
         VALUES ('{line}', '{overlay}', {revision}, '{TENANT}',
                 {plan}, {sku}, {cohort}, '{kind}', '{magnitude_kind}', {value})"
    )
}

/// The list-default percent line — no plan, no sku, no cohort.
fn default_line(line: &str, overlay: &str, revision: i64) -> String {
    insert_line(
        line,
        overlay,
        revision,
        "NULL",
        "NULL",
        "NULL",
        "discount",
        "percent_bp",
        "1500",
    )
}

/// Move a revision out of `draft`.
async fn flip(conn: &DatabaseConnection, overlay: &str, revision: i64, state: &str) {
    must_succeed(
        conn,
        &format!(
            "UPDATE pricing_price_overlay SET lifecycle_state = '{state}' \
             WHERE price_overlay_id = '{overlay}' AND revision = {revision}"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_price_overlay` — the header, and its revision chain (D-92).
// ---------------------------------------------------------------------------

/// The overlay is keyed `(price_overlay_id, revision)`, so one overlay holds a
/// chain of revision rows (D-92).
#[tokio::test]
async fn an_overlay_is_a_chain_of_revision_rows() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;

    let count = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS text) AS v FROM pricing_price_overlay \
             WHERE price_overlay_id = '{OVERLAY}'"
        ),
    )
    .await;
    assert_eq!(count, "2", "the overlay must hold both revisions");
}

/// **D-107, the whole reason the precedence index is partial.**
///
/// D-92 gave overlays coexisting `draft` / `published` / `superseded` revision
/// rows. An unqualified `UNIQUE (tenant_id, scope_class, precedence)` makes a
/// draft revision of a published overlay collide **with itself**, so every edit
/// of a live overlay would fail `PRECEDENCE_DUPLICATE` — the overlay would be
/// authored once and then frozen forever.
///
/// This is the case that fails the moment the `WHERE` is dropped, and it is why
/// the index carries one.
#[tokio::test]
async fn a_draft_revision_may_reuse_its_own_published_revisions_precedence() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    // The same `(tenant, scope_class, precedence)` triple, on the same overlay.
    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;
}

/// Two **published** overlays may not share a precedence within one scope class
/// (L2, `inst-plv-precedence`).
///
/// The other half of the rule above: partial does not mean absent.
///
/// # The fragment is the column list, and the line below it is the index name
///
/// `SQLite` names the **index** in a uniqueness violation only when the index is
/// an **expression** index; a plain one — even a partial one, as this is —
/// still reports the column list. So this suite asserts the column list here and
/// `uq_pricing_price_overlay_line_key` one section down, and the difference is a
/// property of the index shape rather than of the rule. Postgres names the index
/// either way, which is why `postgres_schema_overlay.rs` can assert the name on
/// both and this file cannot.
#[tokio::test]
async fn two_published_overlays_of_one_class_may_not_share_a_precedence() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    must_succeed(&conn, &draft_overlay(OTHER_OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay SET lifecycle_state = 'published' \
             WHERE price_overlay_id = '{OTHER_OVERLAY}' AND revision = 0"
        ),
        "UNIQUE constraint failed: pricing_price_overlay.tenant_id, \
         pricing_price_overlay.scope_class, pricing_price_overlay.precedence",
    )
    .await;
}

/// Precedence is unique **within** a class, so two classes may tie — which is
/// exactly why `inst-plv-class-tiebreak` exists at all.
#[tokio::test]
async fn two_classes_may_share_a_precedence() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    must_succeed(
        &conn,
        &insert_overlay(
            OTHER_OVERLAY,
            0,
            "published",
            "region",
            "eu-west",
            10,
            "delegated_tariffs",
            "restricted",
        ),
    )
    .await;
}

/// An overlay holds at most one open draft revision (D-92).
#[tokio::test]
async fn an_overlay_holds_at_most_one_open_draft_revision() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &draft_overlay(OVERLAY, 1),
        "UNIQUE constraint failed: pricing_price_overlay.price_overlay_id",
    )
    .await;
}

/// `lifecycle_state` is the three D-92 names and nothing else.
#[tokio::test]
async fn a_lifecycle_state_outside_the_three_is_refused() {
    let conn = migrated_db().await;
    must_be_rejected(
        &conn,
        &insert_overlay(
            OVERLAY,
            0,
            "retired",
            "brand",
            "acme",
            10,
            "delegated_tariffs",
            "restricted",
        ),
        "chk_pricing_price_overlay_lifecycle_state",
    )
    .await;
}

/// `scope_class` is §1.1's six and nothing else.
#[tokio::test]
async fn a_scope_class_outside_the_six_is_refused() {
    let conn = migrated_db().await;
    must_be_rejected(
        &conn,
        &insert_overlay(
            OVERLAY,
            0,
            "draft",
            "reseller",
            "acme",
            10,
            "delegated_tariffs",
            "restricted",
        ),
        "chk_pricing_price_overlay_scope_class",
    )
    .await;
}

/// The **classless sentinel**: `global` carries an empty scope value, every
/// other class carries a non-empty one, and the biconditional holds both ways.
///
/// Worth a `CHECK` rather than a convention because `inst-plv-scope` validates
/// every non-`global` value against a declared universe: a `brand` overlay with
/// no value would have nothing to validate, and a `global` overlay with one
/// would claim a universe membership the class has no universe for.
#[tokio::test]
async fn the_global_class_and_the_empty_scope_value_imply_each_other() {
    let conn = migrated_db().await;
    must_succeed(
        &conn,
        &insert_overlay(
            OVERLAY,
            0,
            "draft",
            "global",
            "",
            10,
            "delegated_tariffs",
            "restricted",
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &insert_overlay(
            OTHER_OVERLAY,
            0,
            "draft",
            "global",
            "everyone",
            11,
            "delegated_tariffs",
            "restricted",
        ),
        "chk_pricing_price_overlay_scope_value",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_overlay(
            OTHER_OVERLAY,
            0,
            "draft",
            "brand",
            "",
            12,
            "delegated_tariffs",
            "restricted",
        ),
        "chk_pricing_price_overlay_scope_value",
    )
    .await;
}

/// `tax_basis` is L5's three, and it is `NOT NULL` — *silence fails*
/// (`inst-plv-taxbasis`).
#[tokio::test]
async fn a_tax_basis_outside_the_three_is_refused() {
    let conn = migrated_db().await;
    must_be_rejected(
        &conn,
        &insert_overlay(
            OVERLAY,
            0,
            "draft",
            "brand",
            "acme",
            10,
            "unspecified",
            "restricted",
        ),
        "chk_pricing_price_overlay_tax_basis",
    )
    .await;
}

/// `disclosure` is L6's pair and defaults to `restricted` — the fail-closed
/// default, so an overlay nobody classified is one nobody may see.
#[tokio::test]
async fn disclosure_is_the_declared_pair_and_defaults_to_restricted() {
    let conn = migrated_db().await;
    must_be_rejected(
        &conn,
        &insert_overlay(
            OVERLAY,
            0,
            "draft",
            "brand",
            "acme",
            10,
            "delegated_tariffs",
            "internal",
        ),
        "chk_pricing_price_overlay_disclosure",
    )
    .await;

    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay (
                price_overlay_id, revision, tenant_id, lifecycle_state,
                scope_class, scope_value, precedence, tax_basis, target_ref)
             VALUES ('{OVERLAY}', 0, '{TENANT}', 'draft', 'brand', 'acme', 10,
                     'delegated_tariffs', '{{}}')"
        ),
    )
    .await;
    let disclosure = scalar(
        &conn,
        &format!(
            "SELECT disclosure AS v FROM pricing_price_overlay \
             WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
        ),
    )
    .await;
    assert_eq!(disclosure, "restricted", "the default must fail closed");
}

/// An overlay's own interval must be non-empty when both ends are authored.
#[tokio::test]
async fn an_inverted_effective_interval_is_refused() {
    let conn = migrated_db().await;
    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay (
                price_overlay_id, revision, tenant_id, lifecycle_state, scope_class,
                scope_value, precedence, effective_from, effective_to, tax_basis,
                disclosure, target_ref)
             VALUES ('{OVERLAY}', 0, '{TENANT}', 'draft', 'brand', 'acme', 10,
                     '2099-06-01 00:00:00 +00:00', '2099-01-01 00:00:00 +00:00',
                     'delegated_tariffs', 'restricted', '{{}}')"
        ),
        "chk_pricing_price_overlay_interval",
    )
    .await;
}

/// **A revision is a position in a chain, and the chain starts at zero.**
///
/// `chk_pricing_price_overlay_revision` fails **open**: `overlay_repo` numbers
/// revisions itself and can never spell a negative one, so dropping the
/// constraint breaks no path and admits a row that sorts **ahead of** the
/// overlay's own origin. `uq_pricing_price_overlay_open_draft` admits one draft
/// per overlay wherever it sorts, so such a row takes the draft slot while every
/// latest-revision read answers from the real head — and the line table's
/// composite foreign key would then let lines hang off a predecessor of
/// revision 0.
#[tokio::test]
async fn a_negative_revision_is_refused() {
    let conn = migrated_db().await;
    must_be_rejected(
        &conn,
        &draft_overlay(OVERLAY, -1),
        "chk_pricing_price_overlay_revision",
    )
    .await;
    // Zero is the boundary and is the origin every chain starts at, so the rule
    // is pinned from below too: `> 0` would refuse every overlay ever authored.
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
}

/// **The entity tag is a counter too, and starts where the revision chain does.**
///
/// `overlay_repo` reads it back through the same conversion it reads `revision`
/// with, and calls a value no caller can address `CorruptRow`. `revision`
/// carries the guard that says so; without its sibling, the tag a caller submits
/// its compare-and-swap against is the half of the pair the schema does not
/// hold, and a row poisoned there answers 500 to every read of the overlay.
///
/// The vehicle is an UPDATE of a draft: the frozen-column trigger stands only
/// over rows that have left `draft`, so on this row the constraint is the one
/// guard that can answer.
#[tokio::test]
async fn a_negative_entity_tag_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay SET row_version = -1 \
             WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
        ),
        "chk_pricing_price_overlay_row_version",
    )
    .await;
    // Zero is the boundary and every overlay is created at it, so `> 0` would
    // refuse every row this gear writes.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay SET row_version = 0 \
             WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
        ),
    )
    .await;
}

/// A published revision is frozen in content; only the sanctioned flip moves it
/// (D-92).
#[tokio::test]
async fn a_published_revisions_content_is_frozen() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay SET precedence = 20 \
             WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
        ),
        "frozen",
    )
    .await;
}

/// `published -> superseded` is the flip the submit performs; `superseded ->
/// published` is not a flip at all.
#[tokio::test]
async fn only_the_sanctioned_lifecycle_flips_are_permitted() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    flip(&conn, OVERLAY, 0, "superseded").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay SET lifecycle_state = 'published' \
             WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
        ),
        "not a sanctioned flip",
    )
    .await;
}

/// **No revision is deletable, in any state** — D-231, `chk_pricing_price_overlay_lifecycle_state`.
///
/// This case asserted the opposite until the tombstone landed: the overlay's
/// three states carried no `abandoned` the way `pricing_plan` does, so a
/// discarded draft left by `DELETE` and this suite pinned that as the rule. It
/// was the mechanism of the re-mint — `max(revision) + 1` cannot see a deleted
/// row — so the exit is gone rather than narrowed, and a discarded draft now
/// leaves by the `draft -> abandoned` flip below.
///
/// Both halves are still asserted, because the *reason* differs by state: a
/// published revision must not be deletable because some `CatalogVersion` froze
/// it, and a draft must not be deletable because its **number** must stay
/// consumed.
#[tokio::test]
async fn no_revision_is_deletable_in_any_state() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;

    for revision in [0, 1] {
        must_be_rejected(
            &conn,
            &format!(
                "DELETE FROM pricing_price_overlay \
                 WHERE price_overlay_id = '{OVERLAY}' AND revision = {revision}"
            ),
            "is not permitted",
        )
        .await;
    }
}

/// A draft leaves by `draft -> abandoned`, and `abandoned` is terminal.
///
/// The tombstone's whole job is to hold the revision number, so what the schema
/// has to guarantee is that nothing takes the row back out of that state — not
/// to `draft`, not to `published`, not to `superseded`.
#[tokio::test]
async fn a_draft_may_be_abandoned_and_an_abandoned_revision_never_leaves() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "abandoned").await;

    for target in ["draft", "published", "superseded"] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_price_overlay SET lifecycle_state = '{target}' \
                 WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
            ),
            "not a sanctioned flip",
        )
        .await;
    }
}

/// The tombstone keeps its **number** without keeping the **open-draft slot**.
///
/// `uq_pricing_price_overlay_open_draft` is partial on `lifecycle_state =
/// 'draft'`, so an abandoned row leaves it and a fresh draft opens against the
/// same overlay. Asserted at the schema because it is the index's predicate that
/// decides it: an unconditional index would have let one discard block every
/// future revision of the overlay.
#[tokio::test]
async fn an_abandoned_revision_does_not_hold_the_open_draft_index() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "abandoned").await;

    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;
}

// ---------------------------------------------------------------------------
// `pricing_price_overlay_line` — the D-42 line container.
// ---------------------------------------------------------------------------

/// A line hangs off **one revision** of one overlay.
///
/// Without the revision half a line would belong to the overlay rather than to a
/// revision, and D-92's copy-on-new-revision would have nothing to copy onto.
///
/// **Two guards in series, and this case cannot separate them.** A line naming
/// an absent `(price_overlay_id, overlay_revision)` is refused by the
/// append-only trigger's `NOT EXISTS` — which fires `BEFORE INSERT`, ahead of
/// the composite foreign key — so the sentence asserted here is the trigger's
/// and the foreign key is invisible behind it. **Since D-231 the key is invisible
/// on every path**, the delete side included — see
/// [`a_lines_parent_revision_cannot_be_deleted_out_from_under_it`], which used to
/// reach it through the one state the header's trigger permitted and no longer
/// can.
#[tokio::test]
async fn a_line_names_the_overlay_revision_it_belongs_to() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &default_line(OTHER_LINE, OVERLAY, 7),
        "is not permitted",
    )
    .await;
}

/// A line's parent revision cannot be deleted out from under it — **and since
/// D-231 the refusal is the trigger's, not the foreign key's.**
///
/// This case existed to prove the composite foreign key *on its own*: a draft
/// revision used to be deletable, which made it the one state where the key
/// answered with no trigger standing in front of it. `chk_pricing_price_overlay_lifecycle_state` refuses
/// `DELETE` in every state, so that window is closed and the key is now shadowed
/// on this path — the trigger fires `BEFORE DELETE` and the key never gets the
/// statement.
///
/// **The key is therefore no longer independently observable anywhere in this
/// suite**, and that is stated rather than quietly lost. It is not removed: it
/// still declares the parent-child relationship the schema depends on, and a
/// migration that later narrows the trigger would find the key still behind it.
/// What no test can assert any more is the key *acting*, because nothing can
/// reach it — which is the honest consequence of putting a stricter guard in
/// front of a weaker one.
#[tokio::test]
async fn a_lines_parent_revision_cannot_be_deleted_out_from_under_it() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM pricing_price_overlay \
             WHERE price_overlay_id = '{OVERLAY}' AND revision = 0"
        ),
        "a discarded draft revision is abandoned",
    )
    .await;
}

/// **The null-safe line key, and the trap it closes.**
///
/// §6's `UNIQUE (price_overlay_id, overlay_revision, plan_id, target_sku,
/// cohort)` is *"null-safe"*, and a naive `UNIQUE` over those five columns is
/// not: inside a `UNIQUE` index NULLs are **distinct**, so two list-default
/// lines — the case §6's own words call *"one default line"* — would both land.
/// The index therefore keys over `COALESCE`d sentinels, and this is the case
/// that fails the moment it does not.
#[tokio::test]
async fn one_revision_carries_at_most_one_list_default_line() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &default_line(OTHER_LINE, OVERLAY, 0),
        "uq_pricing_price_overlay_line_key",
    )
    .await;
}

/// The same rule at the `(plan)` level — `target_sku` and `cohort` both NULL.
#[tokio::test]
async fn one_revision_carries_at_most_one_line_per_plan() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "NULL",
            "NULL",
            "markup",
            "percent_bp",
            "2000",
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &insert_line(
            OTHER_LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "500",
        ),
        "uq_pricing_price_overlay_line_key",
    )
    .await;
}

/// A `(plan)` line and a `(plan, sku)` line are **two** keys — the most-specific
/// rule's whole subject (`inst-plv-lines`).
#[tokio::test]
async fn a_plan_line_and_a_plan_sku_line_are_distinct_keys() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "1000",
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert_line(
            OTHER_LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "'sku-a'",
            "NULL",
            "discount",
            "percent_bp",
            "1500",
        ),
    )
    .await;
}

/// **D-78's cohort is part of the key.** A cohort-targeted line and a
/// cohort-less line on one `(plan, sku)` are disjoint by eligibility, so they
/// never collide — and the null-safe index has to agree with that.
#[tokio::test]
async fn a_cohort_line_and_a_cohort_less_line_on_one_plan_coexist() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "1000",
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert_line(
            OTHER_LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "NULL",
            &format!("'{COHORT}'"),
            "discount",
            "percent_bp",
            "1000",
        ),
    )
    .await;

    // ...and two lines on the SAME cohort still collide.
    must_be_rejected(
        &conn,
        &insert_line(
            "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "NULL",
            &format!("'{COHORT}'"),
            "markup",
            "percent_bp",
            "100",
        ),
        "uq_pricing_price_overlay_line_key",
    )
    .await;
}

/// The key is **per revision**: the same line key exists once in each revision,
/// which is what copy-on-new-revision produces (D-92).
#[tokio::test]
async fn the_line_key_is_scoped_to_one_revision() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;

    must_succeed(&conn, &default_line(OTHER_LINE, OVERLAY, 1)).await;
}

/// **D-92's stable line identity, and the reason §6's `PK line_id` is not
/// buildable.**
///
/// A copy-on-new-revision writes a second row for **one** line. Under §6's
/// literal `PK line_id` that row would need a new id, and then the identity D-92
/// calls stable is not — which is the half of the sentence a consumer diffing
/// two revisions actually uses. The key is `(line_id, overlay_revision)`, so the
/// **same** `line_id` appears under both revisions and the two rows are
/// distinguishable by the revision alone.
#[tokio::test]
async fn one_line_id_survives_the_copy_onto_a_new_revision() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;

    // The copy: the same line id, one revision on.
    must_succeed(&conn, &default_line(LINE, OVERLAY, 1)).await;

    let revisions = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS text) AS v FROM pricing_price_overlay_line \
             WHERE line_id = '{LINE}'"
        ),
    )
    .await;
    assert_eq!(
        revisions, "2",
        "one line must exist under both revisions under ONE id"
    );

    // ...and the same key is still refused twice within one revision.
    must_be_rejected(
        &conn,
        &default_line(OTHER_LINE, OVERLAY, 1),
        "uq_pricing_price_overlay_line_key",
    )
    .await;
}

/// **A line id belongs to its tenant** — `pricing_price_overlay_line`, review A1-3.
///
/// The primary key was `(line_id, overlay_revision)` and `line_id` is
/// **client-supplied** — the route description documents supplying it as the
/// intended usage, because that is what makes a copy-on-new-revision keep D-92's
/// stable identity. So a line id belonged to one overlay per revision number
/// **across the entire table, every tenant included**, and the refusal on it —
/// typed as `ValueOutOfRange { field: "line_id" }`, which is the mitigation that
/// held this at Medium — was still an oracle over another tenant's line ids, with
/// the same permanent-lockout consequence D-340 measured on the stand.
///
/// Two tenants, two overlays, one line id, the same revision number. The amount
/// row goes with it: the child's key widened in the same migration, because a
/// `line_id` that two tenants can hold at one revision would otherwise collide in
/// `pricing_price_overlay_line_amount` instead — the finding filed as A1-4,
/// recorded there so this fix would not create it.
#[tokio::test]
async fn two_tenants_may_hold_one_line_id_at_one_revision_number() {
    const OTHER_TENANT: &str = "99999999-9999-9999-9999-999999999999";
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE}', 0, 'EUR', '{TENANT}', 1000)"
        ),
    )
    .await;

    // The other tenant's own overlay, and the same line id under it.
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay (
                price_overlay_id, revision, tenant_id, lifecycle_state,
                scope_class, scope_value, precedence, tax_basis, disclosure, target_ref)
             VALUES ('{OTHER_OVERLAY}', 0, '{OTHER_TENANT}', 'draft',
                     'brand', 'acme', 10, 'delegated_tariffs', 'restricted', '{{}}')"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                plan_id, target_sku, cohort, adjustment_kind, magnitude_kind, adjustment_value)
             VALUES ('{LINE}', '{OTHER_OVERLAY}', 0, '{OTHER_TENANT}',
                     NULL, NULL, NULL, 'discount', 'percent_bp', 1500)"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE}', 0, 'EUR', '{OTHER_TENANT}', 2000)"
        ),
    )
    .await;

    let lines = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS text) AS v FROM pricing_price_overlay_line \
             WHERE line_id = '{LINE}' AND overlay_revision = 0"
        ),
    )
    .await;
    assert_eq!(lines, "2", "one line id, one row per tenant");
    let amounts = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS text) AS v FROM pricing_price_overlay_line_amount \
             WHERE line_id = '{LINE}' AND overlay_revision = 0 AND currency = 'EUR'"
        ),
    )
    .await;
    assert_eq!(amounts, "2", "and the money follows its own line");
}

/// **A neighbour's draft does not thaw this tenant's published line** — the half
/// of `pricing_price_overlay_line_amount` that is a trigger change and not a key change.
///
/// The amount table's append-only guard resolves its parent by
/// `(line_id, overlay_revision)` and nothing else. That was unambiguous while the
/// line key was globally unique, and stopped being so the moment two tenants could
/// hold one `(line_id, overlay_revision)`: an amount under a **published** line
/// would find the *other* tenant's draft line, read `draft`, and be admitted —
/// D-92's freeze defeated across a tenant boundary by a widening meant to close a
/// leak. All three `SQLite` bodies and the Postgres function gained
/// `l.tenant_id = NEW.tenant_id` (`OLD` on the delete arm) for that, which is the
/// one trigger-body change in this migration and the one moved digest set.
#[tokio::test]
async fn a_foreign_draft_line_does_not_unfreeze_a_published_lines_money() {
    const OTHER_TENANT: &str = "99999999-9999-9999-9999-999999999999";
    let conn = migrated_db().await;
    // This tenant: a line under a **published** revision.
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;
    // The neighbour: the same line id at the same revision, under a **draft**.
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay (
                price_overlay_id, revision, tenant_id, lifecycle_state,
                scope_class, scope_value, precedence, tax_basis, disclosure, target_ref)
             VALUES ('{OTHER_OVERLAY}', 0, '{OTHER_TENANT}', 'draft',
                     'brand', 'acme', 10, 'delegated_tariffs', 'restricted', '{{}}')"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay_line (
                line_id, price_overlay_id, overlay_revision, tenant_id,
                plan_id, target_sku, cohort, adjustment_kind, magnitude_kind, adjustment_value)
             VALUES ('{LINE}', '{OTHER_OVERLAY}', 0, '{OTHER_TENANT}',
                     NULL, NULL, NULL, 'discount', 'percent_bp', 1500)"
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO pricing_price_overlay_line_amount (
                line_id, overlay_revision, currency, tenant_id, value_minor)
             VALUES ('{LINE}', 0, 'EUR', '{TENANT}', 1000)"
        ),
        "non-draft overlay revision is not permitted",
    )
    .await;
}

/// Each revision's value set is its own, so a published revision's money is not
/// shared with the draft that succeeds it.
#[tokio::test]
async fn the_amount_set_rides_the_revision_and_not_the_line_id() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;
    must_succeed(&conn, &insert_amount_at(LINE, 0, "EUR", 5000)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 1, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;
    // The successor prices the same line differently, and the published
    // revision's value is untouched.
    must_succeed(&conn, &insert_amount_at(LINE, 1, "EUR", 9000)).await;

    let published = scalar(
        &conn,
        &format!(
            "SELECT CAST(value_minor AS text) AS v FROM pricing_price_overlay_line_amount \
             WHERE line_id = '{LINE}' AND overlay_revision = 0 AND currency = 'EUR'"
        ),
    )
    .await;
    assert_eq!(
        published, "5000",
        "the published revision keeps serving unchanged"
    );
}

/// `CHECK (cohort IS NULL OR plan_id IS NOT NULL)` — §6.
///
/// A `cohort` is validated against *"the line's target plan"*
/// (`inst-plv-eligibility`), which the list-default line does not have.
#[tokio::test]
async fn a_cohort_on_the_list_default_line_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            &format!("'{COHORT}'"),
            "discount",
            "percent_bp",
            "1000",
        ),
        "chk_pricing_price_overlay_line_cohort_needs_plan",
    )
    .await;
}

/// A `targetSku` line MUST also name its `planId` (§3 step 2a, §6).
///
/// A bare SKU is ambiguous per `(currency, region)` — the B1 argument one slice
/// over — so a sku-only line names no resolvable target.
#[tokio::test]
async fn a_target_sku_without_a_plan_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "'sku-a'",
            "NULL",
            "discount",
            "percent_bp",
            "1000",
        ),
        "chk_pricing_price_overlay_line_sku_needs_plan",
    )
    .await;
}

/// **D-08: the value type is declared, never inferred.**
///
/// `(magnitude_kind = 'percent_bp') = (adjustment_value IS NOT NULL)`, both
/// ways: a percent line without a bp value carries no magnitude at all, and an
/// amount line with one carries two.
#[tokio::test]
async fn the_magnitude_kind_and_the_bp_value_imply_each_other() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "NULL",
        ),
        "chk_pricing_price_overlay_line_magnitude_pairing",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "discount", "amount", "1000",
        ),
        "chk_pricing_price_overlay_line_magnitude_pairing",
    )
    .await;
}

/// **D-08's other half: the magnitude kind is a closed pair.**
///
/// `chk_..._magnitude_pairing` only relates `percent_bp` to the presence of a bp
/// value, so a third token carrying no value satisfies it — which is why this
/// rule fails **open** with the pairing case green beside it. `ratio` below
/// passes every other `CHECK` on the row and lands the moment
/// `chk_..._magnitude_kind` stops refusing, at which point `overlay_rules` reads
/// a magnitude it has no arm for on a line the store called well formed.
#[tokio::test]
async fn a_magnitude_kind_outside_the_declared_pair_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "discount", "ratio", "NULL",
        ),
        "chk_pricing_price_overlay_line_magnitude_kind",
    )
    .await;
}

/// **The nil uuid is the line key's `plan_id` sentinel, and it is spellable from
/// the wire.**
///
/// `uq_pricing_price_overlay_line_key` coalesces a NULL plan to
/// `00000000-…-0000`, so a line naming it explicitly keys as the **list-default
/// line**: it collides with the real default line, or takes the slot first and
/// makes the default line unauthorable. That is the module doc's *"two of those
/// three sentinels are spellable from the wire, so the table refuses them
/// itself"*, and the reason the rule cannot be left to `overlay_repo` — which
/// refuses a nil plan id too, and is therefore exactly the layer that would keep
/// every test green while the `CHECK` stopped refusing.
#[tokio::test]
async fn the_nil_uuid_cannot_be_spelled_as_a_plan_id() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "'00000000-0000-0000-0000-000000000000'",
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "1500",
        ),
        "chk_pricing_price_overlay_line_plan_id_not_nil",
    )
    .await;
}

/// The same argument on the second forgeable sentinel: `COALESCE(target_sku,
/// '')` means a **blank** SKU keys as "no SKU", so a blank-SKU line collides
/// with its plan's own per-plan line rather than naming a SKU.
///
/// The plan is named on the row because `chk_..._sku_needs_plan` would otherwise
/// answer first — a mis-arranged fixture here would prove that neighbouring rule
/// a second time and leave this one untouched.
#[tokio::test]
async fn a_blank_target_sku_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            &format!("'{PLAN}'"),
            "''",
            "NULL",
            "discount",
            "percent_bp",
            "1500",
        ),
        "chk_pricing_price_overlay_line_target_sku_present",
    )
    .await;
}

/// **D-138: `fixed` requires `amount`.**
///
/// `fixed` replaces the running line amount with an absolute price, and a
/// percentage of an amount it is replacing is not a thing that can be evaluated.
#[tokio::test]
async fn a_fixed_line_must_be_amount_based() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            "NULL",
            "fixed",
            "percent_bp",
            "5000",
        ),
        "chk_pricing_price_overlay_line_fixed_is_amount",
    )
    .await;
}

/// **D-67's ceiling: a discount above 100% is not authorable.**
///
/// `discount / percent_bp = 15000` — the "150% of list" data-entry inversion —
/// passed every other stated validation before D-67.
#[tokio::test]
async fn a_discount_above_one_hundred_percent_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "15000",
        ),
        "chk_pricing_price_overlay_line_discount_ceiling",
    )
    .await;
    // Exactly 100% is the boundary and is authorable: `0 < v <= 10000`.
    must_succeed(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "10000",
        ),
    )
    .await;
}

/// **D-67's floor**: a non-positive bp magnitude is refused on both kinds.
///
/// A `markup` of 0 bp and a `discount` of 0 bp are lines that adjust nothing,
/// which is a line that should not have been authored rather than a no-op the
/// stack should carry.
#[tokio::test]
async fn a_non_positive_bp_magnitude_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;

    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            "NULL",
            "markup",
            "percent_bp",
            "0",
        ),
        "chk_pricing_price_overlay_line_magnitude_positive",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_line(
            LINE,
            OVERLAY,
            0,
            "NULL",
            "NULL",
            "NULL",
            "discount",
            "percent_bp",
            "-500",
        ),
        "chk_pricing_price_overlay_line_magnitude_positive",
    )
    .await;
}

/// A line under a **published** revision is frozen (D-92).
///
/// This is the property the acceptance criteria state as *"the published
/// revision keeps serving unchanged"*: without it, a draft edit rewrites the
/// lines a frozen `CatalogVersion` resolves.
#[tokio::test]
async fn the_lines_of_a_published_revision_are_frozen() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(&conn, &default_line(LINE, OVERLAY, 0)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay_line SET adjustment_value = 9000 \
             WHERE line_id = '{LINE}'"
        ),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_price_overlay_line WHERE line_id = '{LINE}'"),
        "is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_line(
            OTHER_LINE,
            OVERLAY,
            0,
            &format!("'{OTHER_PLAN}'"),
            "NULL",
            "NULL",
            "markup",
            "percent_bp",
            "100",
        ),
        "is not permitted",
    )
    .await;
}

// ---------------------------------------------------------------------------
// `pricing_price_overlay_line_amount` — the per-currency values (D-08).
// ---------------------------------------------------------------------------

/// One value per `(line, currency)` — §6's `UNIQUE (line_id, currency)`.
///
/// The fragment gained `tenant_id` with `pricing_price_overlay_line`, which widened this
/// key alongside its parent line's, and the move is the assertion rather than a
/// loosening: `SQLite` names the offending **columns**, so a fragment still naming
/// only the three would pass against a key that had lost the tenant it just
/// gained.
#[tokio::test]
async fn a_line_carries_at_most_one_value_per_currency() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;
    must_succeed(&conn, &insert_amount(LINE, "EUR", 5000)).await;
    must_succeed(&conn, &insert_amount(LINE, "USD", 5500)).await;

    must_be_rejected(
        &conn,
        &insert_amount(LINE, "EUR", 6000),
        "UNIQUE constraint failed: pricing_price_overlay_line_amount.tenant_id, \
         pricing_price_overlay_line_amount.overlay_revision, \
         pricing_price_overlay_line_amount.line_id, \
         pricing_price_overlay_line_amount.currency",
    )
    .await;
}

/// **D-67: an amount magnitude is `>= 0`.**
#[tokio::test]
async fn a_negative_amount_magnitude_is_refused() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &insert_amount(LINE, "EUR", -1),
        "chk_pricing_price_overlay_line_amount_value_minor",
    )
    .await;
    // **Zero is admitted, and deliberately**: a `fixed 0` line is how a market is
    // priced at nothing, which is a real authoring act. Without this the CHECK
    // could be tightened to `> 0`, a free market would become unauthorable, and
    // this case would not notice -- the boundary discipline the two bp cases in
    // this file already apply.
    must_succeed(&conn, &insert_amount(LINE, "USD", 0)).await;
}

/// An amount row hangs off a line that exists.
///
/// The same two-guards-in-series note as
/// [`a_line_names_the_overlay_revision_it_belongs_to`]: the append-only
/// trigger's parent lookup finds nothing and refuses ahead of the foreign key,
/// so the sentence here is the trigger's. The foreign key is proved on its own
/// below.
#[tokio::test]
async fn an_amount_names_a_line_that_exists() {
    let conn = migrated_db().await;
    must_be_rejected(&conn, &insert_amount(LINE, "EUR", 5000), "is not permitted").await;
}

/// The amount table's foreign key, on its own — a draft line carrying values may
/// not be deleted out from under them.
#[tokio::test]
async fn the_amount_foreign_key_refuses_a_delete_that_would_orphan_values() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;
    must_succeed(&conn, &insert_amount(LINE, "EUR", 5000)).await;

    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_price_overlay_line WHERE line_id = '{LINE}'"),
        "FOREIGN KEY constraint failed",
    )
    .await;
}

/// The amounts of a published revision's line are frozen too — the line's freeze
/// would be worth nothing if its values were still editable.
#[tokio::test]
async fn the_amounts_of_a_published_revision_are_frozen() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;
    must_succeed(&conn, &insert_amount(LINE, "EUR", 5000)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay_line_amount SET value_minor = 1 \
             WHERE line_id = '{LINE}' AND currency = 'EUR'"
        ),
        "is not permitted",
    )
    .await;
    must_be_rejected(&conn, &insert_amount(LINE, "USD", 5500), "is not permitted").await;
}

/// **A published revision's money cannot be moved off it by re-pointing.**
///
/// The `UPDATE` trigger has two arms, and this is the only statement the `OLD`
/// arm alone can refuse: the row's *new* coordinates are a legal draft, so the
/// `NEW` arm passes and the whole guard rests on the arm that asks where the row
/// is coming **from**.
///
/// It is the amount table's version of
/// `postgres_schema_overlay::a_line_cannot_be_re_pointed_onto_a_frozen_revision`,
/// and it exists because D-92's copy-on-new-revision deliberately reuses one
/// `line_id` across revisions — so "some revision of this line is a draft" and
/// "this row's revision is a draft" are different questions, and only the second
/// one is the rule.
#[tokio::test]
async fn a_published_revisions_amounts_cannot_be_re_pointed_onto_a_draft() {
    let conn = migrated_db().await;
    must_succeed(&conn, &draft_overlay(OVERLAY, 0)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 0, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;
    must_succeed(&conn, &insert_amount_at(LINE, 0, "EUR", 5000)).await;
    flip(&conn, OVERLAY, 0, "published").await;

    // The successor exists and is a draft, so the row's destination is legal.
    must_succeed(&conn, &draft_overlay(OVERLAY, 1)).await;
    must_succeed(
        &conn,
        &insert_line(
            LINE, OVERLAY, 1, "NULL", "NULL", "NULL", "fixed", "amount", "NULL",
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price_overlay_line_amount SET overlay_revision = 1 \
             WHERE line_id = '{LINE}' AND overlay_revision = 0"
        ),
        "is not permitted",
    )
    .await;

    // ...and the published revision still carries its value.
    let value = scalar(
        &conn,
        &format!(
            "SELECT CAST(value_minor AS text) AS v FROM pricing_price_overlay_line_amount \
             WHERE line_id = '{LINE}' AND overlay_revision = 0 AND currency = 'EUR'"
        ),
    )
    .await;
    assert_eq!(
        value, "5000",
        "a frozen CatalogVersion must not resolve a line whose money has moved away"
    );
}

fn insert_amount(line: &str, currency: &str, value_minor: i64) -> String {
    insert_amount_at(line, 0, currency, value_minor)
}

/// The same, naming the revision the value rides — the second half of the
/// amount table's foreign key since the line's own key widened.
fn insert_amount_at(line: &str, revision: i64, currency: &str, value_minor: i64) -> String {
    format!(
        "INSERT INTO pricing_price_overlay_line_amount (
            line_id, overlay_revision, currency, tenant_id, value_minor) \
         VALUES ('{line}', {revision}, '{currency}', '{TENANT}', {value_minor})"
    )
}
