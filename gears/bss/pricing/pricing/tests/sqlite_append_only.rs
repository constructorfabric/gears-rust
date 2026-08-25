//! The append-only column whitelist on `pricing_price`, proven against a real
//! database.
//!
//! Postgres carries the whitelist as one PL/pgSQL trigger; `SQLite` mirrors it
//! as five `RAISE(ABORT, ...)` triggers (see the migration's module doc), so the
//! guard is exercisable without Docker and this suite does not need a
//! testcontainers test to know the rule holds.
//!
//! One case per branch of the whitelist: a forbidden price mutation, a
//! forbidden lifecycle transition on the published plane **and on the draft one
//! (D-153)**, a forbidden loosening of `grandfather_until`,
//! a forbidden DELETE, a forbidden bump of `row_version`, and a forbidden
//! re-size of a `package` block — plus the moves that are *supposed* to work,
//! so the test proves a whitelist rather than a blanket ban. Without the guard
//! an ad-hoc UPDATE would silently change a frozen `CatalogVersion`'s content
//! at the next warm re-drive, because the projector reads truth rows.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const PLAN: &str = "22222222-2222-2222-2222-222222222222";
const PHASE: &str = "33333333-3333-3333-3333-333333333333";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";
const PUBLISHED: &str = "55555555-5555-5555-5555-555555555555";
const DRAFT: &str = "66666666-6666-6666-6666-666666666666";

/// Reject, **and** for the stated reason.
///
/// The fragment is not decoration. `pricing_price` carries five whitelist
/// triggers, twenty `CHECK` constraints and two partial `UNIQUE` indexes, and
/// every one of those names contains the string `pricing_price` — as does the
/// column list `SQLite` reports for a unique violation. A test that accepted
/// any error naming the table would therefore pass with the trigger it means to
/// prove switched off, refused instead by a constraint it never intended to
/// trip.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the append-only guard must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("pricing_price"),
        "the rejection must name the guard it came from, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// Insert one published `all_subscriptions` row and one draft row on the same
/// plan (different charge kinds, so the scope-key unique index is satisfied).
async fn seed(conn: &DatabaseConnection) {
    must_succeed(
        conn,
        &format!(
            "INSERT INTO pricing_price (
                price_id, tenant_id, plan_id, currency, region, phase,
                charge_kind, amount_minor, model_kind, lifecycle_state,
                created_by, created_at_utc)
             VALUES ('{PUBLISHED}', '{TENANT}', '{PLAN}', 'USD', 'EU', '{PHASE}',
                'recurring', 1000, 'flat', 'published', '{ACTOR}', '2026-08-02 10:00:00 +00:00')"
        ),
    )
    .await;
    must_succeed(
        conn,
        &format!(
            "INSERT INTO pricing_price (
                price_id, tenant_id, plan_id, currency, region, phase,
                charge_kind, amount_minor, model_kind, lifecycle_state,
                created_by, created_at_utc)
             VALUES ('{DRAFT}', '{TENANT}', '{PLAN}', 'USD', 'EU', '{PHASE}',
                'one_time', 500, 'flat', 'draft', '{ACTOR}', '2026-08-02 10:00:00 +00:00')"
        ),
    )
    .await;
}

/// **The whitelist names every content column the *table* holds.**
///
/// Every other case in this file picks its columns by hand, so each of them
/// proves the column it names is frozen and all of them together are blind by
/// construction to a column the enumeration omits. That is not a hypothetical on
/// this table: five migrations have paid for exactly that omission —
/// `trg_pricing_price_append_only` for the tax columns, `000051` for the proration ones,
/// `000055` for the reservation pair, `000057` for the floors and `000069` for
/// the `per_unit` rate, which **is the price** — and every one was found by a
/// person reading a diff rather than by a test.
///
/// This case reads the column list off `pragma_table_info` and the predicate off
/// `sqlite_master`, so a column added to `pricing_price` and forgotten in
/// `trg_pricing_price_frozen_columns` reddens here. It is the twin of
/// `postgres_schema_price::the_frozen_whitelist_names_every_content_column_the_table_holds`
/// and of the pair `trg_pricing_plan_append_only` wrote for `pricing_plan`; this half needs no
/// Docker, so the census runs on every commit rather than on the ignored tier.
///
/// A text census rather than a behavioural one, deliberately: a per-column UPDATE
/// needs a value that both differs from the seed and satisfies every pairing
/// CHECK, which is why the cases below hand-pick theirs — and hand-picking is the
/// very step that gets skipped when a column is added.
#[tokio::test]
async fn the_frozen_whitelist_names_every_content_column_the_table_holds() {
    // The two columns deliberately movable on a frozen row. `lifecycle_state` is
    // the sanctioned `published -> superseded` flip the whitelist exists to
    // permit, and `grandfather_until` is guarded by monotonicity instead — a
    // different trigger, with its own case in this file. Every other column is
    // owed a line, and this list is the only place an exemption can be claimed.
    const SANCTIONED_MUTABLE: [&str; 2] = ["grandfather_until", "lifecycle_state"];

    let conn = migrated_db().await;
    let columns = scalar(
        &conn,
        "SELECT group_concat(name) AS v FROM pragma_table_info('pricing_price')",
    )
    .await;
    let predicate = scalar(
        &conn,
        "SELECT sql AS v FROM sqlite_master \
         WHERE type = 'trigger' AND name = 'trg_pricing_price_frozen_columns'",
    )
    .await;

    let missing: Vec<&str> = columns
        .split(',')
        .filter(|column| !SANCTIONED_MUTABLE.contains(column))
        // The trailing space is what keeps `min_qty_usage` from matching
        // `min_qty_usage_fallback`'s line.
        .filter(|column| !predicate.contains(&format!("NEW.{column} ")))
        .collect();
    assert!(
        missing.is_empty(),
        "these columns are on `pricing_price` and absent from the frozen-column \
         whitelist, so an ad-hoc UPDATE moves them under a frozen CatalogVersion: \
         {missing:?}"
    );
}

#[tokio::test]
async fn a_published_price_row_is_immutable_in_content() {
    let conn = migrated_db().await;
    seed(&conn).await;

    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET amount_minor = 1 WHERE price_id = '{PUBLISHED}'"),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET currency = 'EUR' WHERE price_id = '{PUBLISHED}'"),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET model_kind = 'volume' WHERE price_id = '{PUBLISHED}'"),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;
    // The two tax columns (`T-18`, `trg_pricing_price_append_only`), and they are here on the
    // **fast** tier deliberately. Postgres has the exhaustive thirty-four-column
    // loop, but it is `#[ignore]`d behind Docker — and D-236 is the record of what
    // that costs: a premise living on one tier only means a run without Docker
    // reports a clean change through a guard that stopped guarding.
    //
    // `tax_category_ref` is the sharper of the two: it is authored draft content,
    // D-48 makes it one of the five descriptor elements Billing countersigns, and
    // since the pin moved to `v5` it is inside the approval content hash — so a
    // published row whose category moved would diverge from the pin that approved
    // it.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET tax_category_ref = 'reduced' WHERE price_id = '{PUBLISHED}'"
        ),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET resolved_tax_category = 'standard' \
             WHERE price_id = '{PUBLISHED}'"
        ),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;

    // Slice 6's four (`pricing_price`), on this tier for the reason the two
    // above are: the exhaustive Postgres loop is behind Docker, and D-236 is the
    // record of what a premise living on one tier only costs.
    //
    // `credit_on_downgrade` is the sharpest of the four -- it is what a
    // downgrade's credit is computed from, and all four are inside the approval
    // content pin at `v6`, so a published row whose proration contract moved
    // would diverge from the pin that approved it, silently and for money.
    for column in [
        "billing_anchor_policy = 'subscription_start'",
        "anchor_day = 9",
        "proration_basis = 'by_second'",
        "credit_on_downgrade = 1",
    ] {
        must_be_rejected(
            &conn,
            &format!("UPDATE pricing_price SET {column} WHERE price_id = '{PUBLISHED}'"),
            "price, scope, model and entity-tag columns are immutable",
        )
        .await;
    }

    let amount = scalar(
        &conn,
        &format!("SELECT CAST(amount_minor AS TEXT) AS v FROM pricing_price WHERE price_id = '{PUBLISHED}'"),
    )
    .await;
    assert_eq!(amount, "1000", "no rejected UPDATE may have landed");
}

#[tokio::test]
async fn only_the_sanctioned_lifecycle_transition_is_permitted() {
    let conn = migrated_db().await;
    seed(&conn).await;

    // `published -> retired` is a plan-revision flip, not a price-row one.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'retired' WHERE price_id = '{PUBLISHED}'"
        ),
        "lifecycle_state transition is not sanctioned",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'draft' WHERE price_id = '{PUBLISHED}'"
        ),
        "lifecycle_state transition is not sanctioned",
    )
    .await;

    // `published -> superseded` is the one the state machine sanctions.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'superseded' WHERE price_id = '{PUBLISHED}'"
        ),
    )
    .await;
    let state = scalar(
        &conn,
        &format!("SELECT lifecycle_state AS v FROM pricing_price WHERE price_id = '{PUBLISHED}'"),
    )
    .await;
    assert_eq!(state, "superseded");
}

#[tokio::test]
async fn a_draft_row_may_only_go_to_published_d153() {
    // The draft plane's own whitelist. A column whitelist is scoped to
    // *published* rows by construction, so before D-153 this trigger returned
    // early for a draft row and `draft -> superseded` was physically possible.
    // Such a ghost lands outside **both** partial `UNIQUE` predicates — its key
    // reads free on the published plane and on the draft plane — undoing the
    // guarantee D-148 bought, and `inst-ps-nodelete` then makes it undeletable
    // on a key no supersession chain reaches.
    let conn = migrated_db().await;
    seed(&conn).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'superseded' WHERE price_id = '{DRAFT}'"
        ),
        "lifecycle_state transition is not sanctioned",
    )
    .await;
    assert_eq!(
        scalar(
            &conn,
            &format!("SELECT lifecycle_state AS v FROM pricing_price WHERE price_id = '{DRAFT}'"),
        )
        .await,
        "draft",
        "the refused transition must not have taken effect"
    );

    // The two edges the machine does sanction from `draft`: staying put while
    // content is edited, and publishing.
    must_succeed(
        &conn,
        &format!("UPDATE pricing_price SET amount_minor = 4242 WHERE price_id = '{DRAFT}'"),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET lifecycle_state = 'published' WHERE price_id = '{DRAFT}'"
        ),
    )
    .await;
    assert_eq!(
        scalar(
            &conn,
            &format!("SELECT lifecycle_state AS v FROM pricing_price WHERE price_id = '{DRAFT}'"),
        )
        .await,
        "published"
    );
}

#[tokio::test]
async fn grandfather_until_may_only_be_tightened() {
    let conn = migrated_db().await;
    let row = "77777777-7777-7777-7777-777777777777";
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price (
                price_id, tenant_id, plan_id, currency, region, phase,
                price_eligibility, charge_kind, cohort, amount_minor, model_kind,
                lifecycle_state, created_by, created_at_utc)
             VALUES ('{row}', '{TENANT}', '{PLAN}', 'USD', 'EU', '{PHASE}',
                'existing_grandfathered', 'recurring', '1780000000000', 900, 'flat',
                'published', '{ACTOR}', '2026-08-02 10:00:00 +00:00')"
        ),
    )
    .await;

    // Setting it when null is a tightening.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET grandfather_until = '2027-01-01 00:00:00 +00:00' \
             WHERE price_id = '{row}'"
        ),
    )
    .await;
    // Moving it earlier is a tightening.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET grandfather_until = '2026-10-01 00:00:00 +00:00' \
             WHERE price_id = '{row}'"
        ),
    )
    .await;
    // Moving it later is a loosening.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET grandfather_until = '2028-01-01 00:00:00 +00:00' \
             WHERE price_id = '{row}'"
        ),
        "grandfather_until may only be tightened, never loosened",
    )
    .await;
    // Clearing it is a loosening too.
    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET grandfather_until = NULL WHERE price_id = '{row}'"),
        "grandfather_until may only be tightened, never loosened",
    )
    .await;

    let horizon = scalar(
        &conn,
        &format!("SELECT grandfather_until AS v FROM pricing_price WHERE price_id = '{row}'"),
    )
    .await;
    assert_eq!(horizon, "2026-10-01 00:00:00 +00:00");
}

#[tokio::test]
async fn published_rows_never_delete_and_drafts_stay_mutable() {
    let conn = migrated_db().await;
    seed(&conn).await;

    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_price WHERE price_id = '{PUBLISHED}'"),
        "DELETE of a non-draft row is not permitted",
    )
    .await;

    // A never-published draft is freely mutable and deletable — the whitelist
    // guards frozen rows, it does not freeze authoring.
    must_succeed(
        &conn,
        &format!("UPDATE pricing_price SET amount_minor = 750 WHERE price_id = '{DRAFT}'"),
    )
    .await;
    must_succeed(
        &conn,
        &format!("DELETE FROM pricing_price WHERE price_id = '{DRAFT}'"),
    )
    .await;

    let remaining = scalar(
        &conn,
        "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_price",
    )
    .await;
    assert_eq!(remaining, "1", "only the published row survives");
}

#[tokio::test]
async fn a_published_rows_entity_tag_is_frozen_with_its_content() {
    let conn = migrated_db().await;
    seed(&conn).await;

    // The tag denotes a representation, and this one cannot change. A tag that
    // moved under frozen content would tell a caller its cached copy is stale
    // when it is not, and fail every correctly-read `If-Match` submit after it.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_price SET row_version = row_version + 1 WHERE price_id = '{PUBLISHED}'"
        ),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;

    let version = scalar(
        &conn,
        &format!(
            "SELECT CAST(row_version AS TEXT) AS v FROM pricing_price WHERE price_id = '{PUBLISHED}'"
        ),
    )
    .await;
    assert_eq!(version, "0", "the rejected bump may not have landed");
}

#[tokio::test]
async fn a_published_rows_package_block_size_is_frozen() {
    let conn = migrated_db().await;
    let row = "88888888-8888-8888-8888-888888888888";
    // A published `package` row, so the kind CHECK that ties the package fields
    // to `model_kind` is already satisfied and the only thing that can reject
    // the UPDATE below is the append-only whitelist.
    must_succeed(
        &conn,
        &format!(
            "INSERT INTO pricing_price (
                price_id, tenant_id, plan_id, currency, region, phase,
                charge_kind, model_kind, package_size, package_price_minor,
                lifecycle_state, created_by, created_at_utc)
             VALUES ('{row}', '{TENANT}', '{PLAN}', 'USD', 'EU', '{PHASE}',
                'usage', 'package', 100, 5000, 'published', '{ACTOR}',
                '2026-08-02 10:00:00 +00:00')"
        ),
    )
    .await;

    // `package_size` is a quantity-determining field, not a price lever: block
    // math is non-linear in the window, so re-sizing a block mid-window
    // re-buckets an already-accumulated counter (D-122). It is frozen for the
    // same reason every other model column is.
    must_be_rejected(
        &conn,
        &format!("UPDATE pricing_price SET package_size = 200 WHERE price_id = '{row}'"),
        "price, scope, model and entity-tag columns are immutable",
    )
    .await;

    let size = scalar(
        &conn,
        &format!(
            "SELECT CAST(package_size AS TEXT) AS v FROM pricing_price WHERE price_id = '{row}'"
        ),
    )
    .await;
    assert_eq!(size, "100", "the rejected re-size may not have landed");
}

#[tokio::test]
async fn a_draft_rows_entity_tag_advances_with_each_edit() {
    let conn = migrated_db().await;
    seed(&conn).await;

    // The draft plane is where content moves, so it is where the tag moves.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_price SET row_version = row_version + 1 WHERE price_id = '{DRAFT}'"
        ),
    )
    .await;

    let version = scalar(
        &conn,
        &format!(
            "SELECT CAST(row_version AS TEXT) AS v FROM pricing_price WHERE price_id = '{DRAFT}'"
        ),
    )
    .await;
    assert_eq!(version, "1", "the authoring edit advanced the draft's tag");
}

// ---------------------------------------------------------------------------
// The census, over the rest of the frozen-column trigger family.
// ---------------------------------------------------------------------------

/// The census above, parameterised on the table — the shape `pg_support::frozen_columns`
/// already had and the `SQLite` half did not.
///
/// The migrations install **six** frozen-column trigger families and the
/// catalog-derived census guarded **two** of them: `pricing_price` and
/// `pricing_plan`. The other four were guarded by hand-enumerated per-column
/// cases, which is precisely the blindness the census exists to close — and the
/// bill for that blindness on `pricing_price` alone is five migrations
/// (`…000040`, `…000051`, `…000055`, `…000057`, `…000069`), each found by a
/// person reading a diff rather than by a test.
///
/// The overlay plane is the one that mattered most and was on the wrong side of
/// the line: it is the only per-tenant discount surface, `pricing_price_overlay_line`
/// carries the money, and a column added there and forgotten becomes mutable
/// under a frozen `CatalogVersion`.
///
/// `sanctioned_mutable` is declared **in the test**, which is where an exemption
/// becomes reviewable: a column added to the table is owed a guard line unless
/// somebody writes it into one of these lists and says why.
///
/// **It reads the trigger off `sqlite_master`, not off a migration file, and
/// that distinction is not academic.** Proving these censuses bite, the first
/// attempt deleted a column from the overlay trigger in
/// `pricing_price_overlay` — where the trigger is
/// created — and the census stayed green, correctly:
/// `chk_pricing_price_overlay_lifecycle_state` drops and recreates
/// it, so the creating migration's text is dead. Deleting the same line at
/// `…000045` reddens it naming `target_ref`. Anything that checks a rule against
/// a DB constraint has to ask the schema as it now stands.
async fn census(
    conn: &DatabaseConnection,
    table: &str,
    trigger: &str,
    sanctioned_mutable: &[&str],
) {
    let columns = scalar(
        conn,
        &format!("SELECT group_concat(name) AS v FROM pragma_table_info('{table}')"),
    )
    .await;
    let predicate = scalar(
        conn,
        &format!(
            "SELECT sql AS v FROM sqlite_master \
             WHERE type = 'trigger' AND name = '{trigger}'"
        ),
    )
    .await;
    // A census that quietly read an empty predicate would report every column
    // unguarded, or be silenced with an exemption. `pg_support::frozen_columns`
    // panics for this reason; the SQLite half asserts it.
    assert!(
        !predicate.is_empty(),
        "the trigger {trigger} must exist, or this census is measuring nothing"
    );
    assert!(
        !columns.is_empty(),
        "the table {table} must have columns, or this census is measuring nothing"
    );

    let missing: Vec<&str> = columns
        .split(',')
        .filter(|column| !sanctioned_mutable.contains(column))
        // The trailing space, for the reason the `pricing_price` census gives:
        // it is what keeps a column from matching a longer sibling's line.
        .filter(|column| !predicate.contains(&format!("NEW.{column} ")))
        .collect();
    assert!(
        missing.is_empty(),
        "these columns are on `{table}` and absent from {trigger}, so an ad-hoc \
         UPDATE moves them on a frozen row: {missing:?}"
    );
}

#[tokio::test]
async fn the_overlay_frozen_whitelist_names_every_content_column_the_table_holds() {
    // The money-bearing plane. One exemption, and it is the same one
    // `pricing_price` claims: the sanctioned lifecycle flip the whitelist exists
    // to permit, which the trigger's own refusal message names.
    const SANCTIONED_MUTABLE: [&str; 1] = ["lifecycle_state"];

    census(
        &migrated_db().await,
        "pricing_price_overlay",
        "trg_pricing_price_overlay_frozen_columns",
        &SANCTIONED_MUTABLE,
    )
    .await;
}

#[tokio::test]
async fn the_window_frozen_whitelist_names_every_content_column_the_table_holds() {
    // Six exemptions, and the trigger's own refusal message declares exactly
    // this set: "only state, effective_to and the flip timestamps may move".
    //
    // Two of them are not unguarded, they are guarded by a **different**
    // trigger, which is the `grandfather_until` shape one table over:
    // `effective_to` by `trg_pricing_price_window_future_end` (movable only
    // while future, and only to a future instant) and `mutation_seq` by
    // `trg_pricing_price_window_act_sequence` (+1 per act, never reused, never
    // backwards). `state` is bounded by `trg_pricing_price_window_flip_whitelist`.
    // So the exemption list is "guarded elsewhere or lifecycle progress", never
    // "free".
    const SANCTIONED_MUTABLE: [&str; 6] = [
        "state",
        "effective_to",
        "activated_at",
        "expired_at",
        "cancelled_at",
        "mutation_seq",
    ];

    census(
        &migrated_db().await,
        "pricing_price_window",
        "trg_pricing_price_window_frozen_columns",
        &SANCTIONED_MUTABLE,
    )
    .await;
}

#[tokio::test]
async fn the_migration_frozen_whitelist_names_every_content_column_the_table_holds() {
    // The scheduled-migration plane. The exemptions are the run's own progress:
    // its state, the two records the run writes as it goes, and the three
    // timestamps that mark it. Everything that describes *what* the migration
    // will do is frozen at schedule time.
    const SANCTIONED_MUTABLE: [&str; 6] = [
        "state",
        "exclusion_snapshot",
        "completion_record",
        "started_at",
        "completed_at",
        "cancelled_at",
    ];

    census(
        &migrated_db().await,
        "pricing_migration",
        "trg_pricing_migration_frozen_columns",
        &SANCTIONED_MUTABLE,
    )
    .await;
}

#[tokio::test]
async fn the_bulk_operation_frozen_whitelist_names_every_content_column_the_table_holds() {
    // Slice 12's bulk-operation state machine. Same shape: the request is frozen
    // once accepted, and only the run's own progress moves. `request_hash` is
    // NOT here — `pricing_bulk_operation`'s trigger is written precisely
    // to freeze it, which is why the census reads the trigger as it now stands
    // rather than as its creating migration wrote it.
    const SANCTIONED_MUTABLE: [&str; 3] = ["state", "report", "completed_at"];

    census(
        &migrated_db().await,
        "pricing_bulk_operation",
        "trg_pricing_bulk_operation_frozen_columns",
        &SANCTIONED_MUTABLE,
    )
    .await;
}
