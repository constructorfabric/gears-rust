//! The physical guards on `pricing_plan`, proven against a real database: the
//! append-only whitelist, the two partial `UNIQUE` indexes, and the table's
//! `CHECK` constraints.
//!
//! They are one suite because one rule rests on all three. A `revision` number
//! is an **identity** — minted once for a plan and never again (D-145) — and
//! the mechanism that keeps it true is a discarded draft becoming an
//! `abandoned` tombstone instead of disappearing. That needs the CHECK to admit
//! the token, the trigger to keep the tombstone frozen and undeletable, and
//! **both** partial predicates to exclude it, so a plan can accumulate
//! tombstones while still holding exactly one open draft and one current
//! revision. A repository-level suite cannot see any of it: these are properties
//! of the schema, and the engine is not the only thing that can reach the table.
//!
//! Postgres carries the whitelist as one PL/pgSQL trigger and `SQLite` mirrors
//! it as three `RAISE(ABORT, ...)` triggers (see the migration's module doc), so
//! the guards are exercisable without Docker.
//!
//! The Slice-2 shape columns are held to the same two standards, one case each:
//! their value sets and pairings are `CHECK`-constrained, and **every one of
//! them is enumerated in the frozen-column whitelist**. The enumeration is the
//! point of `a_published_revision_freezes_every_column_the_whitelist_names`: a
//! column added to the table and forgotten in the trigger is a column an ad-hoc
//! UPDATE can move under a frozen `CatalogVersion`, which the projector's warm
//! re-drive then re-materializes (§4.4) — the whole class of defect the
//! whitelist exists to close, and one no smaller test can notice.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const PLAN: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";
const AUTHORED: &str = "2026-08-02 10:00:00 +00:00";

/// Reject, **and** for the stated reason.
///
/// The fragment is not decoration: every whitelist trigger, `CHECK` constraint
/// and partial `UNIQUE` index on `pricing_plan` has a name containing the
/// string `pricing_plan` — as does the
/// column list `SQLite` reports for a unique violation. A test that accepted any
/// error naming the table would pass with the guard it means to prove switched
/// off, refused instead by a constraint it never intended to trip.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the guard must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("pricing_plan"),
        "the rejection must name the guard it came from, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// Rejected by the append-only whitelist — by **either** of its two arms.
///
/// On a tombstone the arms overlap by construction: `abandoned` is not
/// `published`, so the flip arm refuses every UPDATE of the row, and a statement
/// that also moves a content column trips the frozen arm as well. Which one
/// answers is a trigger-ordering detail `SQLite` does not define and Postgres
/// settles the other way — its single function checks the frozen columns first.
/// Pinning one message would pin the mirror's incidental order instead of the
/// rule both backends enforce, which is that nothing about this row moves.
async fn must_be_frozen(conn: &DatabaseConnection, sql: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the guard must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("is frozen") || message.contains("not a sanctioned flip"),
        "the rejection must come from the append-only whitelist, got: {message}"
    );
}

/// One revision row of `PLAN`, in whatever state the case needs.
fn insert(plan: &str, revision: u32, state: &str) -> String {
    insert_with(plan, revision, state, &[])
}

/// The same row, plus whatever Slice-2 columns the case is about.
///
/// The values arrive as raw SQL fragments rather than as typed arguments,
/// because half the cases here exist to store something the typed path would
/// have refused — a `billing_cycle` of `monthly`, an interval with no custom
/// frequency. A helper that took domain values could not express the subjects
/// these CHECKs are for.
fn insert_with(plan: &str, revision: u32, state: &str, extra: &[(&str, &str)]) -> String {
    let mut columns = String::from(
        "plan_id, revision, tenant_id, plan_tier, lifecycle_state, created_by, created_at_utc",
    );
    let mut values =
        format!("'{plan}', {revision}, '{TENANT}', 'gold', '{state}', '{ACTOR}', '{AUTHORED}'");
    for &(column, value) in extra {
        columns.push_str(", ");
        columns.push_str(column);
        values.push_str(", ");
        values.push_str(value);
    }
    format!("INSERT INTO pricing_plan ({columns}) VALUES ({values})")
}

/// A distinct plan id per case, for the cases that need one current revision
/// each and cannot share a plan with the others.
fn plan_of(index: usize) -> String {
    format!("333333{index:02}-3333-3333-3333-333333333333")
}

async fn state_of(conn: &DatabaseConnection, revision: u32) -> String {
    scalar(
        conn,
        &format!(
            "SELECT lifecycle_state AS v FROM pricing_plan \
             WHERE plan_id = '{PLAN}' AND revision = {revision}"
        ),
    )
    .await
}

#[tokio::test]
async fn a_discarded_draft_flips_to_abandoned_and_the_tombstone_freezes() {
    let conn = migrated_db().await;
    must_succeed(&conn, &insert(PLAN, 0, "draft")).await;

    // The discard itself. It rides the draft plane, which is unguarded because
    // that is where content is supposed to move.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_plan SET lifecycle_state = 'abandoned', \
             row_version = row_version + 1 WHERE plan_id = '{PLAN}' AND revision = 0"
        ),
    )
    .await;
    assert_eq!(state_of(&conn, 0).await, "abandoned");

    // From here the row is a tombstone, and the number it holds may never be
    // attached to a different shape: its content is frozen exactly as a
    // published revision's is, and so is the entity tag that names it.
    must_be_frozen(
        &conn,
        &format!("UPDATE pricing_plan SET plan_tier = 'silver' WHERE plan_id = '{PLAN}'"),
    )
    .await;
    must_be_frozen(
        &conn,
        &format!("UPDATE pricing_plan SET row_version = row_version + 1 WHERE plan_id = '{PLAN}'"),
    )
    .await;

    // And it is terminal. An edge out of it would put a live revision back at a
    // number already handed out as a durable name — which is the state the
    // tombstone exists to make unreachable.
    for state in ["draft", "published", "superseded", "retired"] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_plan SET lifecycle_state = '{state}' WHERE plan_id = '{PLAN}'"
            ),
            "not a sanctioned flip",
        )
        .await;
    }
    assert_eq!(
        state_of(&conn, 0).await,
        "abandoned",
        "no rejected move may have landed"
    );
}

#[tokio::test]
async fn a_draft_leaves_only_by_publishing_or_by_being_abandoned() {
    let conn = migrated_db().await;
    must_succeed(&conn, &insert(PLAN, 0, "draft")).await;

    // The draft plane is unguarded in its **columns**, not in its exits. A
    // hand-run flip to `retired` mints a row that satisfies
    // `uq_pricing_plan_current` — the row the projector sources a plan subject
    // from — without that row ever having published, and a flip to `superseded`
    // makes history out of something that was never current. The domain machine
    // refuses both; so must the table, or the two disagree about the edge set
    // and the one that drifted is discovered as a revision nothing can move.
    for state in ["retired", "superseded"] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_plan SET lifecycle_state = '{state}' WHERE plan_id = '{PLAN}'"
            ),
            "not a sanctioned flip",
        )
        .await;
    }
    assert_eq!(state_of(&conn, 0).await, "draft");

    // Editing a draft's content is still free — this is a whitelist on the way
    // out, not a freeze.
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_plan SET plan_tier = 'silver', row_version = row_version + 1 \
             WHERE plan_id = '{PLAN}'"
        ),
    )
    .await;

    // And the two sanctioned exits work: publishing here, abandoning in
    // `a_discarded_draft_flips_to_abandoned_and_the_tombstone_freezes`.
    must_succeed(
        &conn,
        &format!("UPDATE pricing_plan SET lifecycle_state = 'published' WHERE plan_id = '{PLAN}'"),
    )
    .await;
    assert_eq!(state_of(&conn, 0).await, "published");
}

#[tokio::test]
async fn no_revision_row_is_ever_deleted_not_even_a_draft() {
    let conn = migrated_db().await;
    must_succeed(&conn, &insert(PLAN, 0, "published")).await;
    must_succeed(&conn, &insert(PLAN, 1, "draft")).await;

    // The draft is the case that matters. Deleting one was the old discard, and
    // it returned `max(revision)` to its previous value: the next opened draft
    // minted the same number, `(plan_id, revision)` named two rows over the
    // plan's life, and a stale entity tag passed its precondition against the
    // wrong one. There is no verb here that frees a number.
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_plan WHERE plan_id = '{PLAN}' AND revision = 1"),
        "DELETE of a revision is not permitted",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_plan WHERE plan_id = '{PLAN}' AND revision = 0"),
        "DELETE of a revision is not permitted",
    )
    .await;

    let rows = scalar(
        &conn,
        "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_plan",
    )
    .await;
    assert_eq!(rows, "2", "both revisions survive");
}

#[tokio::test]
async fn a_plan_holds_many_tombstones_beside_one_draft_and_one_current() {
    let conn = migrated_db().await;

    // Two discarded revisions, the plan's current one, and the draft that
    // replaced them — the ordinary shape of a plan somebody has edited. It is
    // legal only because `abandoned` falls outside **both** partial predicates:
    // outside `WHERE lifecycle_state = 'draft'`, so the replacement draft opens
    // immediately, and outside `IN ('published','retired')`, so the current
    // revision is untouched.
    must_succeed(&conn, &insert(PLAN, 0, "published")).await;
    must_succeed(&conn, &insert(PLAN, 1, "abandoned")).await;
    must_succeed(&conn, &insert(PLAN, 2, "abandoned")).await;
    must_succeed(&conn, &insert(PLAN, 3, "draft")).await;

    // Both indexes are still live over the states they do cover, so the
    // exclusion above widened nothing else.
    must_be_rejected(
        &conn,
        &insert(PLAN, 4, "draft"),
        "UNIQUE constraint failed: pricing_plan.plan_id",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(PLAN, 4, "published"),
        "UNIQUE constraint failed: pricing_plan.plan_id",
    )
    .await;
    // Including the state D-128 widened the current-revision predicate to hold.
    must_be_rejected(
        &conn,
        &insert(PLAN, 4, "retired"),
        "UNIQUE constraint failed: pricing_plan.plan_id",
    )
    .await;

    // A third tombstone, on the other hand, is always admissible — a plan may
    // discard as many drafts as its author cares to.
    must_succeed(&conn, &insert(PLAN, 4, "abandoned")).await;

    let tombstones = scalar(
        &conn,
        &format!(
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_plan \
             WHERE plan_id = '{PLAN}' AND lifecycle_state = 'abandoned'"
        ),
    )
    .await;
    assert_eq!(tombstones, "3");
}

#[tokio::test]
async fn the_lifecycle_check_admits_the_five_states_and_nothing_else() {
    let conn = migrated_db().await;

    // One plan per token, because two of these five collide on a single plan by
    // design and this case is about the CHECK, not about the indexes.
    for (i, state) in ["draft", "abandoned", "published", "superseded", "retired"]
        .into_iter()
        .enumerate()
    {
        let plan = format!("2222222{i}-2222-2222-2222-222222222222");
        must_succeed(&conn, &insert(&plan, 0, state)).await;
    }

    // The token the state has to be spelled with is the one the domain machine
    // renders. A near-miss stored here would read back as a corrupt row, and the
    // revision would be unreachable through every typed path in the gear.
    //
    // **On a plan id of its own**, so the CHECK is the only constraint the row can
    // trip. It used to name `PLAN`, which is byte-for-byte the `2222222{i}` id the
    // loop above seeds at `i == 2`, so the statement violated the lifecycle CHECK
    // *and* `(plan_id, revision)` at once and passed only because SQLite happens to
    // evaluate CHECKs before uniqueness — the guard-shadowing `must_be_rejected`'s
    // own doc exists to prevent. Measured 2026-08-20: the same insert with a
    // *valid* state answered `UNIQUE constraint failed: pricing_plan.plan_id,
    // pricing_plan.revision`.
    must_be_rejected(
        &conn,
        &insert(&plan_of(9), 0, "discarded"),
        "chk_pricing_plan_lifecycle_state",
    )
    .await;
}

#[tokio::test]
async fn the_shape_checks_admit_the_slice_2_tokens_and_nothing_else() {
    let conn = migrated_db().await;

    // The value sets §6 declares, each stored under its own plan so the two
    // partial UNIQUE indexes stay out of the way.
    for (i, cycle) in ["one_time", "recurring", "usage", "hybrid"]
        .into_iter()
        .enumerate()
    {
        must_succeed(
            &conn,
            &insert_with(
                &plan_of(i),
                0,
                "draft",
                &[("billing_cycle", &format!("'{cycle}'"))],
            ),
        )
        .await;
    }
    for (i, frequency) in ["monthly", "quarterly", "semiannual", "annual"]
        .into_iter()
        .enumerate()
    {
        must_succeed(
            &conn,
            &insert_with(
                &plan_of(i + 10),
                0,
                "draft",
                &[("frequency", &format!("'{frequency}'"))],
            ),
        )
        .await;
    }
    for (i, unit) in ["days", "months"].into_iter().enumerate() {
        must_succeed(
            &conn,
            &insert_with(
                &plan_of(i + 20),
                0,
                "draft",
                &[
                    ("frequency", "'custom_every_n'"),
                    ("custom_interval_n", "3"),
                    ("custom_interval_unit", &format!("'{unit}'")),
                ],
            ),
        )
        .await;
    }

    // `monthly` is the near-miss that matters, because it is a real token of the
    // **other** column: a `frequency` value in the `billing_cycle` slot is the
    // transposition an authoring surface makes, not a typo.
    must_be_rejected(
        &conn,
        &insert_with(PLAN, 0, "draft", &[("billing_cycle", "'monthly'")]),
        "chk_pricing_plan_billing_cycle",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_with(PLAN, 0, "draft", &[("frequency", "'biweekly'")]),
        "chk_pricing_plan_frequency",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_with(
            PLAN,
            0,
            "draft",
            &[
                ("frequency", "'custom_every_n'"),
                ("custom_interval_n", "2"),
                ("custom_interval_unit", "'weeks'"),
            ],
        ),
        "chk_pricing_plan_custom_interval_unit",
    )
    .await;
}

#[tokio::test]
async fn a_custom_interval_exists_exactly_when_the_frequency_is_the_custom_one() {
    let conn = migrated_db().await;

    // `Frequency` makes both of these unrepresentable by carrying the interval
    // inside the variant. Three columns can express what one enum cannot, so
    // the table has to refuse them itself — otherwise the illegal pairing is
    // storable and only discovered on the read that cannot map it back.
    must_be_rejected(
        &conn,
        &insert_with(PLAN, 0, "draft", &[("frequency", "'custom_every_n'")]),
        "chk_pricing_plan_custom_interval_pairing",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert_with(
            PLAN,
            0,
            "draft",
            &[
                ("frequency", "'monthly'"),
                ("custom_interval_n", "3"),
                ("custom_interval_unit", "'days'"),
            ],
        ),
        "chk_pricing_plan_custom_interval_pairing",
    )
    .await;

    // The NULL case is the one the CHECK is written to survive. A bare
    // `frequency = 'custom_every_n'` is NULL when the column is NULL, and both
    // engines count a NULL CHECK as satisfied — so without the explicit
    // `frequency IS NOT NULL` conjunct this row would be storable, and a plan
    // with no frequency at all would carry an interval nothing could read.
    must_be_rejected(
        &conn,
        &insert_with(
            PLAN,
            0,
            "draft",
            &[
                ("custom_interval_n", "3"),
                ("custom_interval_unit", "'days'"),
            ],
        ),
        "chk_pricing_plan_custom_interval_pairing",
    )
    .await;

    // And `n` counts something, so zero is not an interval. §6 names this
    // constraint explicitly.
    must_be_rejected(
        &conn,
        &insert_with(
            PLAN,
            0,
            "draft",
            &[
                ("frequency", "'custom_every_n'"),
                ("custom_interval_n", "0"),
                ("custom_interval_unit", "'days'"),
            ],
        ),
        "chk_pricing_plan_custom_interval_n",
    )
    .await;
}

#[tokio::test]
async fn a_purchase_window_may_not_close_before_it_opens() {
    let conn = migrated_db().await;

    // §6 names this constraint explicitly. An open-ended window on either side
    // is legal — a one-time plan with a floor and no ceiling is an ordinary
    // shape — so the CHECK has to be the three-way one and not `min <= max`.
    must_succeed(
        &conn,
        &insert_with(&plan_of(0), 0, "draft", &[("purchase_min_qty", "2")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert_with(&plan_of(1), 0, "draft", &[("purchase_max_qty", "9")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert_with(
            &plan_of(2),
            0,
            "draft",
            &[("purchase_min_qty", "4"), ("purchase_max_qty", "4")],
        ),
    )
    .await;

    must_be_rejected(
        &conn,
        &insert_with(
            PLAN,
            0,
            "draft",
            &[("purchase_min_qty", "10"), ("purchase_max_qty", "2")],
        ),
        "chk_pricing_plan_purchase_qty",
    )
    .await;
}

/// Every counter and quantity column refuses a negative, by the constraint that
/// names it.
///
/// The repository reads each of these back through a conversion into the
/// unsigned type the domain counts in and answers `CorruptRow` — a 500 no
/// operator can act on — for anything else. That reading is sound only while the
/// column cannot hold such a value, which is a statement about the schema that
/// only a case reaching past the repository can make.
///
/// **The pair is the case.** `>= 0` and `> 0` agree on every input but one, so a
/// case that only offered `-1` would pass against either and a bound copied from
/// the wrong neighbour would look correct.
///
/// `revision` and `row_version` belong together here: same shape, same reading,
/// and `revision` is the sibling whose guard says what `row_version` owes.
#[tokio::test]
async fn every_counter_column_refuses_a_negative() {
    const COUNTERS: [(&str, &str); 3] = [
        ("purchase_max_qty", "chk_pricing_plan_purchase_max_qty"),
        ("purchase_min_qty", "chk_pricing_plan_purchase_min_qty"),
        ("row_version", "chk_pricing_plan_row_version"),
    ];

    let conn = migrated_db().await;

    for (index, (column, constraint)) in COUNTERS.into_iter().enumerate() {
        // A plan of its own per case: `revision` is keyed and a second row under
        // one plan would be refused by the key rather than by the constraint.
        must_succeed(
            &conn,
            &insert_with(&plan_of(index), 0, "draft", &[(column, "0")]),
        )
        .await;
        must_be_rejected(
            &conn,
            &insert_with(
                &plan_of(index + COUNTERS.len()),
                0,
                "draft",
                &[(column, "-1")],
            ),
            constraint,
        )
        .await;
    }

    // `revision` cannot ride the loop — it is one of the columns every row
    // carries, so it is written in place rather than appended.
    must_be_rejected(
        &conn,
        &format!(
            "INSERT INTO pricing_plan \
             (plan_id, revision, tenant_id, plan_tier, lifecycle_state, created_by, created_at_utc) \
             VALUES ('{}', -1, '{TENANT}', 'gold', 'draft', '{ACTOR}', '{AUTHORED}')",
            plan_of(9)
        ),
        "chk_pricing_plan_revision",
    )
    .await;
}

/// Every column of the append-only whitelist, one assertion each.
///
/// The vehicle is a **sanctioned `published -> retired` flip carrying the
/// column change**, and it has to be: an UPDATE that leaves `lifecycle_state`
/// alone is already refused by the flip whitelist, whatever columns it moves, so
/// a test built on one would pass with the frozen-column list empty. Under the
/// flip only the frozen-column arm can answer, which is why the assertion pins
/// `is frozen` rather than accepting either arm.
///
/// A column added to the table and forgotten in the trigger fails **here**, and
/// nowhere else: it would otherwise be a column an ad-hoc UPDATE moves under a
/// `CatalogVersion` that is already frozen, which the projector re-materializes
/// on its next warm re-drive as though the plan had always said so.
#[tokio::test]
async fn a_published_revision_freezes_every_column_the_whitelist_names() {
    // Column, and a value the seed row does not hold. The seed carries a fixed
    // frequency and no interval, so moving one interval column on its own
    // leaves the pairing CHECK satisfied and the trigger is the only guard that
    // can refuse — which is what this case has to observe.
    const FROZEN: [(&str, &str); 20] = [
        ("plan_id", "'99999999-9999-9999-9999-999999999999'"),
        ("revision", "7"),
        ("tenant_id", "'88888888-8888-8888-8888-888888888888'"),
        ("sku_id", "'77777777-7777-7777-7777-777777777777'"),
        ("plan_tier", "'silver'"),
        ("plan_name", "'Renamed Under A Frozen Version'"),
        ("billing_cycle", "'usage'"),
        ("frequency", "'quarterly'"),
        ("custom_interval_n", "3"),
        ("custom_interval_unit", "'months'"),
        ("plan_tier_override", "1"),
        ("purchase_min_qty", "5"),
        ("purchase_max_qty", "9"),
        ("invoice_grouping_key", "'q3-bundle'"),
        ("available_from", "'2027-01-01 00:00:00 +00:00'"),
        ("available_to", "'2028-01-01 00:00:00 +00:00'"),
        ("created_by", "'66666666-6666-6666-6666-666666666666'"),
        ("created_at_utc", "'2027-06-01 00:00:00 +00:00'"),
        ("row_version", "row_version + 1"),
        // D-264's provenance column (`trg_pricing_plan_append_only` whitelists it).
        // It had no behavioural probe in either this case or the `LATER_COLUMNS`
        // one until 2026-08-20 — 19 + 4 against the trigger's 24 — so a
        // `cloned_from` conjunct that was present but not refusing was the one arm
        // nothing in this file measured. The text census two cases down can only
        // see that the trigger's SQL *mentions* it.
        ("cloned_from", "'99999999-9999-9999-9999-999999999999'"),
    ];

    let conn = migrated_db().await;
    for (i, (column, value)) in FROZEN.into_iter().enumerate() {
        // One plan per column: the vehicle consumes the plan's single current
        // revision slot, and a rejected statement leaves the row published for
        // the next case to trip over.
        let plan = plan_of(i);
        must_succeed(
            &conn,
            &insert_with(
                &plan,
                0,
                "published",
                &[("billing_cycle", "'recurring'"), ("frequency", "'monthly'")],
            ),
        )
        .await;
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_plan SET lifecycle_state = 'retired', {column} = {value} \
                 WHERE plan_id = '{plan}' AND revision = 0"
            ),
            "is frozen",
        )
        .await;
    }

    // The vehicle itself must work, or every assertion above proves only that
    // `published -> retired` is banned — which it is not, and which would make
    // this whole case vacuous.
    let plan = plan_of(FROZEN.len());
    must_succeed(&conn, &insert(&plan, 0, "published")).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE pricing_plan SET lifecycle_state = 'retired' \
             WHERE plan_id = '{plan}' AND revision = 0"
        ),
    )
    .await;
    let state = scalar(
        &conn,
        &format!(
            "SELECT lifecycle_state AS v FROM pricing_plan \
             WHERE plan_id = '{plan}' AND revision = 0"
        ),
    )
    .await;
    assert_eq!(state, "retired", "the sanctioned flip is what it rides on");
}

/// **The whitelist names every content column the *table* holds** — the census
/// `a_published_revision_freezes_every_column_the_whitelist_names` structurally
/// cannot run.
///
/// That case pins a hand-copied array from the trigger, so it proves the
/// enumerated columns are frozen and is blind by construction to a column the
/// enumeration omits. Four were: `entitlement_grants` (`pricing_plan`) and
/// the three plan-change contract columns (`pricing_plan`), all added to
/// `pricing_plan` after `pricing_plan` wrote the trigger and none restated
/// into it — where every later column wave on `pricing_price` got its guard
/// restatement, `trg_pricing_price_append_only` covering two of them and `000051`, `000055`
/// and `000057` one each, for exactly this reason.
///
/// This case reads the column list off the table and the predicate off the
/// trigger, so a column added and forgotten reddens **here** rather than in
/// production. It is a text census and not a behavioural one deliberately: a
/// per-column UPDATE needs a value that both differs from the seed and satisfies
/// every pairing `CHECK`, which is why the sibling case hand-picks its list — and
/// hand-picking is the very step that gets skipped when a column is added.
#[tokio::test]
async fn the_frozen_whitelist_names_every_content_column_the_table_holds() {
    // `lifecycle_state` is the sanctioned flip the whitelist exists to permit,
    // and it is the only exemption: `row_version` is frozen too, and exempting
    // it here would have made this census blind to a column that is in the
    // list. (The Postgres twin's doc is the one that calls `row_version` out; the
    // sibling in this file does not.)
    const SANCTIONED_MUTABLE: [&str; 1] = ["lifecycle_state"];

    let conn = migrated_db().await;
    let columns = scalar(
        &conn,
        "SELECT group_concat(name) AS v FROM pragma_table_info('pricing_plan')",
    )
    .await;
    let predicate = scalar(
        &conn,
        "SELECT sql AS v FROM sqlite_master \
         WHERE type = 'trigger' AND name = 'trg_pricing_plan_frozen_columns'",
    )
    .await;

    let missing: Vec<&str> = columns
        .split(',')
        .filter(|column| !SANCTIONED_MUTABLE.contains(column))
        // The trailing space is what keeps `plan_tier` from matching
        // `plan_tier_override`'s line.
        .filter(|column| !predicate.contains(&format!("NEW.{column} ")))
        .collect();
    assert!(
        missing.is_empty(),
        "these columns are on `pricing_plan` and absent from the frozen-column \
         whitelist, so an ad-hoc UPDATE moves them under a frozen CatalogVersion: \
         {missing:?}"
    );
}

/// The four columns that were missing, refused behaviourally.
///
/// The census above is a text assertion; this is the refusal itself, and the two
/// are not redundant — a trigger could name a column in a comment, and a
/// behavioural case cannot see a column nobody thought to add.
#[tokio::test]
async fn a_published_revision_freezes_its_grant_set_and_change_contract() {
    const LATER_COLUMNS: [(&str, &str, &str); 4] = [
        (
            "entitlement_grants",
            "'{\"quotas\":{\"seats\":1}}'",
            "'{\"quotas\":{\"seats\":9000}}'",
        ),
        (
            "allowed_change_targets",
            "'[]'",
            "'[\"99999999-9999-9999-9999-999999999999\"]'",
        ),
        ("comparability_rank", "10", "99"),
        ("usage_counter_on_plan_change", "'reset'", "'carry'"),
    ];

    let conn = migrated_db().await;
    for (i, (column, seeded, moved)) in LATER_COLUMNS.into_iter().enumerate() {
        let plan = plan_of(70 + i);
        must_succeed(
            &conn,
            &insert_with(
                &plan,
                0,
                "published",
                &[
                    ("billing_cycle", "'recurring'"),
                    ("frequency", "'monthly'"),
                    (column, seeded),
                ],
            ),
        )
        .await;
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_plan SET lifecycle_state = 'retired', {column} = {moved} \
                 WHERE plan_id = '{plan}' AND revision = 0"
            ),
            "is frozen",
        )
        .await;
    }
}
