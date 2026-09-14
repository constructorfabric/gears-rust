//! `pricing_plan`, proved by **executing the statement each object must
//! refuse**, on Postgres.
//!
//! # Why this suite exists
//!
//! `pricing_plan` is the oldest migration in the chain and until Phase 3 it
//! had never run on the backend it targets: the `SQLite` mirror was what every
//! test reached, and the mirror is a *different* set of objects — three
//! `RAISE(ABORT, …)` triggers with literal messages where Postgres has one
//! PL/pgSQL function with four arms. `tests/sqlite_plan_guards.rs` therefore
//! proves the mirror, not this table.
//!
//! `tests/postgres_migrations.rs` closed half the remaining gap by pinning the
//! CHECK, trigger and partial-index rosters **by name**, so an object cannot
//! vanish unnoticed. It issues no DML, so it says the objects reached the server
//! and nothing about what any of them does. This suite is the other half: one
//! executed refusal per object, and the assertion names the object the refusal
//! came from.
//!
//! # The three rules every test here follows
//!
//! **Execute the refusal.** A test that writes valid values is not evidence
//! about a guard. It catches a constraint that got *narrower* — the writer
//! starts failing — and never one that stopped refusing.
//!
//! **Put the world in the state where the object under test is what answers.**
//! A refusal an *earlier* guard produced is not evidence about the guard the
//! test names. This table makes the hazard concrete twice over. The four value
//! CHECKs on the frequency columns overlap with
//! `chk_pricing_plan_custom_interval_pairing`, which reads all three of them at
//! once, so every refusal below moves exactly one column of an otherwise-valid
//! row and leaves the pairing satisfied. And the trigger's arms are ordered:
//! the frozen-column arm is tested before the published-plane flip arm, so a
//! statement that moves a content column can never be evidence about the flip
//! whitelist and vice versa.
//!
//! **Assert the object, never the table.** Every CHECK, index and trigger over
//! this table has `pricing_plan` in its name, as does the column list Postgres
//! prints for a unique violation, as does every one of the trigger's four
//! messages. A test that accepted any error naming the table would pass with
//! the guard it means to prove switched off.
//!
//! ## The two flip arms share a sentence, and the assertions do not
//!
//! `bss.pricing_plan_append_only()` raises the *same* wording — `lifecycle_state
//! % -> % is not a sanctioned flip` — from the draft-plane arm and from the
//! published-plane arm. What separates them is the interpolated pair, so every
//! flip assertion here carries it: `draft -> superseded` can only have come from
//! the arm that judges drafts, `superseded -> published` only from the arm that
//! judges everything else.
//!
//! # Positives are load-bearing
//!
//! Every guard here is a whitelist rather than a blanket ban, so the suite
//! carries the accepting cases too: all five lifecycle tokens store, the four
//! sanctioned flips are taken, a draft revision's content moves freely, a plan
//! holds one draft beside one current revision beside any number of tombstones,
//! and a well-formed custom interval lands. Without those a table nothing can be
//! written to at all would pass.
//!
//! # One object this suite deliberately does not test by refusal
//!
//! `idx_pricing_plan_tenant` is a **non-unique, non-partial** index. It refuses
//! nothing; its only observable effect is on plan choice, which is not a
//! correctness property and would make a brittle test. Its presence is pinned by
//! name in `tests/postgres_migrations.rs`, and that is the whole of what can be
//! said about it here.
//!
//! Ignored by default; they need Docker. Run with
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_plan -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const ACTOR: &str = "44444444-4444-4444-4444-444444444444";

/// One plan per state a test needs to reach, rather than one plan carrying
/// several revisions.
///
/// Both partial UNIQUE indexes are keyed on `plan_id` alone, so staging two
/// current revisions of one plan — or two drafts — is refused by an index
/// before the trigger arm under test is ever consulted. Separate plans keep the
/// trigger the thing that answers, and keep the index removals below from
/// reddening a trigger test.
const PLAN_A: &str = "22222222-0000-0000-0000-00000000000a";
const PLAN_B: &str = "22222222-0000-0000-0000-00000000000b";
const PLAN_C: &str = "22222222-0000-0000-0000-00000000000c";
const PLAN_D: &str = "22222222-0000-0000-0000-00000000000d";
const PLAN_E: &str = "22222222-0000-0000-0000-00000000000e";

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
/// issues is raw SQL that deliberately reaches past `PlanRepo`, because the
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

/// Run one statement that must land.
async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

/// Reject, **and by the named object**.
///
/// See the module doc: the fragment is the whole assertion, because every guard
/// over this table names the table too.
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
// Row builder
// ---------------------------------------------------------------------------

/// A minimal **valid** draft revision: no shape columns, no availability window,
/// revision zero.
///
/// Every refusal below is this row with exactly one column moved, which is what
/// makes each of them a fact about the object it names rather than about
/// whichever neighbour happened to answer first.
fn base_row(plan: &str, revision: u32) -> Vec<(String, String)> {
    [
        ("plan_id", format!("'{plan}'")),
        ("revision", revision.to_string()),
        ("tenant_id", format!("'{TENANT}'")),
        ("lifecycle_state", "'draft'".to_owned()),
        ("created_by", format!("'{ACTOR}'")),
        ("created_at_utc", "'2026-08-03 09:00:00+00'".to_owned()),
    ]
    .into_iter()
    .map(|(column, value)| (column.to_owned(), value))
    .collect()
}

/// `INSERT` of [`base_row`] with the named columns replaced or added.
fn insert(plan: &str, revision: u32, overrides: &[(&str, &str)]) -> String {
    let mut columns = base_row(plan, revision);
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
    format!("INSERT INTO bss.pricing_plan ({names}) VALUES ({values})")
}

/// `INSERT` of a revision already in a terminal or frozen state.
fn seeded(plan: &str, state: &str) -> String {
    insert(plan, 0, &[("lifecycle_state", &format!("'{state}'"))])
}

fn flip(plan: &str, revision: u32, state: &str) -> String {
    format!(
        "UPDATE bss.pricing_plan SET lifecycle_state = '{state}' \
         WHERE plan_id = '{plan}' AND revision = {revision}"
    )
}

// ---------------------------------------------------------------------------
// The world: what `pricing_plan` accepts
// ---------------------------------------------------------------------------

/// The valid rows, first. Without this every refusal below would pass against a
/// table that refuses everything.
///
/// One per lifecycle token, because `chk_pricing_plan_lifecycle_state` admits
/// five and a suite that only inserted drafts would leave four fifths of the
/// admitted set unexercised. `published` and `retired` go on different plans:
/// both are inside `uq_pricing_plan_current`, so a single plan cannot hold them
/// at once and the index — not the CHECK — would be what answered.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_state_the_revision_machine_reaches_is_storable() {
    let conn = applied().await;
    must_succeed(&conn, &seeded(PLAN_A, "draft")).await;
    must_succeed(&conn, &seeded(PLAN_B, "abandoned")).await;
    must_succeed(&conn, &seeded(PLAN_C, "published")).await;
    must_succeed(&conn, &seeded(PLAN_D, "superseded")).await;
    must_succeed(&conn, &seeded(PLAN_E, "retired")).await;
}

/// D-145's arithmetic, from the accepting side: a plan is a *chain* of
/// revisions, and the two partial predicates are disjoint precisely so the chain
/// can be long.
///
/// One current revision, one open draft and any number of `abandoned`
/// tombstones coexist under one `plan_id`. A single index over both planes — or
/// a predicate that forgot to exclude `abandoned` — would refuse this, and every
/// collision test below would be just as green.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_plan_holds_one_draft_one_current_and_many_tombstones() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PLAN_A, 0, &[("lifecycle_state", "'superseded'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(PLAN_A, 1, &[("lifecycle_state", "'abandoned'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(PLAN_A, 2, &[("lifecycle_state", "'abandoned'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(PLAN_A, 3, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_succeed(&conn, &insert(PLAN_A, 4, &[])).await;
}

/// The four flips the whitelist sanctions, so the two refusal tests below are
/// whitelists and not freezes.
///
/// Each on its own plan: the state a flip lands in is inside a partial index for
/// three of the four, and staging them together would make an index the thing
/// that answered.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_four_sanctioned_flips_are_accepted() {
    let conn = applied().await;
    must_succeed(&conn, &seeded(PLAN_A, "draft")).await;
    must_succeed(&conn, &flip(PLAN_A, 0, "published")).await;

    must_succeed(&conn, &seeded(PLAN_B, "draft")).await;
    must_succeed(&conn, &flip(PLAN_B, 0, "abandoned")).await;

    must_succeed(&conn, &seeded(PLAN_C, "published")).await;
    must_succeed(&conn, &flip(PLAN_C, 0, "superseded")).await;

    must_succeed(&conn, &seeded(PLAN_D, "published")).await;
    must_succeed(&conn, &flip(PLAN_D, 0, "retired")).await;
}

/// The draft plane is where content moves — the whole reason the frozen-column
/// arm is scoped to `OLD.lifecycle_state <> 'draft'`.
///
/// The second statement is the one worth having: the publishing flip may carry
/// content with it, because the draft arm returns before the whitelist is ever
/// consulted. A reader who assumed the whitelist applied to the publishing
/// statement itself would be wrong, and this pins which reading is the schema's.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_revision_is_freely_mutable_in_content() {
    let conn = applied().await;
    must_succeed(&conn, &insert(PLAN_A, 0, &[])).await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_plan SET sku_id = '{ACTOR}', plan_tier = 'gold', \
             billing_cycle = 'recurring', row_version = 1 \
             WHERE plan_id = '{PLAN_A}' AND revision = 0"
        ),
    )
    .await;
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_plan SET plan_tier = 'platinum', \
             lifecycle_state = 'published', row_version = 2 \
             WHERE plan_id = '{PLAN_A}' AND revision = 0"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// The nine CHECK constraints
// ---------------------------------------------------------------------------

/// The five tokens `domain::lifecycle` renders, and nothing else.
///
/// A token outside the set falls outside **both** partial predicates, so the
/// one-current-revision and one-open-draft guarantees would simply stop covering
/// the row.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_lifecycle_state_outside_the_five_is_refused() {
    let conn = applied().await;
    for state in ["'archived'", "'active'", "'DRAFT'"] {
        must_be_rejected(
            &conn,
            &insert(PLAN_A, 0, &[("lifecycle_state", state)]),
            "chk_pricing_plan_lifecycle_state",
        )
        .await;
    }
}

/// Revision numbers count up from zero (D-145), and zero is the first one — so
/// the floor is `>= 0` and not `> 0`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_negative_revision_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(PLAN_A, 0, &[("revision", "-1")]),
        "chk_pricing_plan_revision",
    )
    .await;
    // Zero is the first revision a plan ever has, not a missing one.
    must_succeed(&conn, &insert(PLAN_A, 0, &[])).await;
}

/// Every counter column refuses a negative, by the constraint that names it.
///
/// Each is read back into the unsigned type the domain counts in and answers
/// `CorruptRow` for anything else, so an unguarded column is one a stray UPDATE
/// can poison into a row no typed path reads. `revision` one rule up is the
/// sibling whose floor says what the other three owe.
///
/// Each negative is offered **alone**: `chk_pricing_plan_purchase_qty` is
/// satisfied whenever either bound is NULL, so it cannot be the constraint that
/// answers here.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn no_counter_column_admits_a_negative() {
    const COUNTERS: [(&str, &str); 3] = [
        ("purchase_max_qty", "chk_pricing_plan_purchase_max_qty"),
        ("purchase_min_qty", "chk_pricing_plan_purchase_min_qty"),
        ("row_version", "chk_pricing_plan_row_version"),
    ];

    let conn = applied().await;
    for (column, constraint) in COUNTERS {
        must_be_rejected(&conn, &insert(PLAN_A, 0, &[(column, "-1")]), constraint).await;
    }
    // Zero is admitted by all three — a window of no quantity at all, and the
    // tag every revision is created at — so none of the bounds is `> 0`.
    must_succeed(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[
                ("purchase_max_qty", "0"),
                ("purchase_min_qty", "0"),
                ("row_version", "0"),
            ],
        ),
    )
    .await;
}

/// An availability window that closes before — or exactly when — it opens.
///
/// The bound is strict (`available_to > available_from`), so the equal case is
/// refused too: a window of zero width is a plan that is never available, which
/// is spelled by not publishing it rather than by a degenerate window.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_availability_window_that_does_not_open_before_it_closes_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[
                ("available_from", "'2026-12-01 00:00:00+00'"),
                ("available_to", "'2026-01-01 00:00:00+00'"),
            ],
        ),
        "chk_pricing_plan_availability",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[
                ("available_from", "'2026-12-01 00:00:00+00'"),
                ("available_to", "'2026-12-01 00:00:00+00'"),
            ],
        ),
        "chk_pricing_plan_availability",
    )
    .await;
    // Either bound alone is an open-ended window, and both are legal. Two
    // plans, not two revisions of one: a second open draft is refused by
    // `uq_pricing_plan_open_draft`, which would make an index the thing that
    // answered a test about a CHECK.
    must_succeed(
        &conn,
        &insert(PLAN_A, 0, &[("available_from", "'2026-01-01 00:00:00+00'")]),
    )
    .await;
    must_succeed(
        &conn,
        &insert(PLAN_B, 0, &[("available_to", "'2026-12-01 00:00:00+00'")]),
    )
    .await;
}

/// Slice 2's `billing_cycle` value set.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_billing_cycle_outside_the_four_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(PLAN_A, 0, &[("billing_cycle", "'subscription'")]),
        "chk_pricing_plan_billing_cycle",
    )
    .await;
}

/// Slice 2's `frequency` value set.
///
/// The row leaves both interval columns NULL, so the pairing biconditional reads
/// `false = false` and is satisfied — otherwise it would answer first and this
/// test would be green while proving nothing about the constraint it names.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_frequency_outside_the_five_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(PLAN_A, 0, &[("frequency", "'weekly'")]),
        "chk_pricing_plan_frequency",
    )
    .await;
}

/// The custom interval's unit set.
///
/// `custom_interval_n` stays NULL, which keeps the pairing's right-hand side
/// false against a NULL `frequency`'s false left-hand side. Setting both would
/// have handed the refusal to the pairing constraint.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_custom_interval_unit_outside_days_and_months_is_refused() {
    let conn = applied().await;
    for unit in ["'weeks'", "'years'", "'hours'"] {
        must_be_rejected(
            &conn,
            &insert(PLAN_A, 0, &[("custom_interval_unit", unit)]),
            "chk_pricing_plan_custom_interval_unit",
        )
        .await;
    }
}

/// An interval of zero repeats forever at once; a negative one repeats
/// backwards.
///
/// `custom_interval_unit` stays NULL for the same reason as above.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_non_positive_custom_interval_is_refused() {
    let conn = applied().await;
    for n in ["0", "-3"] {
        must_be_rejected(
            &conn,
            &insert(PLAN_A, 0, &[("custom_interval_n", n)]),
            "chk_pricing_plan_custom_interval_n",
        )
        .await;
    }
}

/// The physical half of `Frequency`'s unrepresentable pairing, refused in
/// **both** directions and in the half-set case between them.
///
/// One-sided tests are how this constraint class rots: a bare
/// `CHECK (frequency <> 'custom_every_n' OR custom_interval_n IS NOT NULL)`
/// would refuse the first case below and admit the second, and an interval would
/// start meaning something on a `monthly` plan.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_custom_interval_pairing_is_refused_in_both_directions() {
    let conn = applied().await;
    // The custom token without an interval.
    must_be_rejected(
        &conn,
        &insert(PLAN_A, 0, &[("frequency", "'custom_every_n'")]),
        "chk_pricing_plan_custom_interval_pairing",
    )
    .await;
    // The custom token with only half of one.
    must_be_rejected(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[
                ("frequency", "'custom_every_n'"),
                ("custom_interval_n", "3"),
            ],
        ),
        "chk_pricing_plan_custom_interval_pairing",
    )
    .await;
    // An interval without the custom token.
    must_be_rejected(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[
                ("frequency", "'monthly'"),
                ("custom_interval_n", "3"),
                ("custom_interval_unit", "'days'"),
            ],
        ),
        "chk_pricing_plan_custom_interval_pairing",
    )
    .await;
    // And the shape the constraint exists to admit.
    must_succeed(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[
                ("frequency", "'custom_every_n'"),
                ("custom_interval_n", "14"),
                ("custom_interval_unit", "'days'"),
            ],
        ),
    )
    .await;
}

/// **A hole the migration's own doc argues for, pinned so that closing it is a
/// decision rather than an accident.**
///
/// A half-set interval pair under a *non-custom* frequency satisfies the
/// biconditional: its right-hand side is already false, so `false = false`
/// holds. The row below is stored, and the migration's module doc says why —
/// `plan_repo::read_frequency` accepts an interval column only on the custom
/// token, so the typed path fails closed on it and the schema stands behind the
/// reading rather than in front of it.
///
/// The test is here because the constraint's *name* promises more than the
/// constraint delivers, and a reader who trusted the name would believe a
/// half-set pair is unstorable. It is not.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_pairing_check_does_not_judge_a_half_set_interval_under_a_plain_frequency() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[("frequency", "'monthly'"), ("custom_interval_n", "3")],
        ),
    )
    .await;
    must_succeed(
        &conn,
        &insert(
            PLAN_B,
            0,
            &[
                ("frequency", "'monthly'"),
                ("custom_interval_unit", "'days'"),
            ],
        ),
    )
    .await;
    // And with no frequency at all, which is the state a shapeless draft is in.
    // One plan each: three open drafts of one plan is what
    // `uq_pricing_plan_open_draft` exists to refuse.
    must_succeed(&conn, &insert(PLAN_C, 0, &[("custom_interval_n", "3")])).await;
}

/// D-96's purchase-quantity window may not close before it opens.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_purchase_window_that_closes_before_it_opens_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[("purchase_min_qty", "10"), ("purchase_max_qty", "5")],
        ),
        "chk_pricing_plan_purchase_qty",
    )
    .await;
    // Equal bounds are a window of exactly one quantity, which is a real rule.
    must_succeed(
        &conn,
        &insert(
            PLAN_A,
            0,
            &[("purchase_min_qty", "5"), ("purchase_max_qty", "5")],
        ),
    )
    .await;
    // And either bound alone is open-ended. One plan each, so that
    // `uq_pricing_plan_open_draft` is not what answers.
    must_succeed(&conn, &insert(PLAN_B, 0, &[("purchase_min_qty", "2")])).await;
    must_succeed(&conn, &insert(PLAN_C, 0, &[("purchase_max_qty", "9")])).await;
}

// ---------------------------------------------------------------------------
// The two partial UNIQUE indexes
// ---------------------------------------------------------------------------

/// At most one **current** revision per plan, over the predicate **D-128
/// widened**.
///
/// All three collisions are here rather than in three tests, because they are
/// three readings of one index: `published` beside `published` would be refused
/// by the pre-D-128 predicate too, and only the mixed and `retired` pairs show
/// that retirement did not fall out of the index when it flipped the row. A test
/// that staged only the first pair would stay green against a predicate narrowed
/// back to `= 'published'`, under which a retired revision stops being anybody's
/// current one and the projector loses its source row.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_current_revisions_of_one_plan_cannot_coexist() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PLAN_A, 0, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(PLAN_A, 1, &[("lifecycle_state", "'published'")]),
        "uq_pricing_plan_current",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(PLAN_A, 2, &[("lifecycle_state", "'retired'")]),
        "uq_pricing_plan_current",
    )
    .await;

    must_succeed(
        &conn,
        &insert(PLAN_B, 0, &[("lifecycle_state", "'retired'")]),
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(PLAN_B, 1, &[("lifecycle_state", "'retired'")]),
        "uq_pricing_plan_current",
    )
    .await;
}

/// At most one **open draft** per plan, which the current-revision index cannot
/// say.
///
/// Two concurrent authoring calls each read the plan as having no open draft
/// under the current-revision index alone, and both land — after which the plan
/// has two concurrently editable shapes and two entity tags that each look
/// authoritative.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_open_drafts_of_one_plan_cannot_coexist() {
    let conn = applied().await;
    must_succeed(&conn, &insert(PLAN_A, 0, &[])).await;
    must_be_rejected(&conn, &insert(PLAN_A, 1, &[]), "uq_pricing_plan_open_draft").await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_plan_append_only()` — arm 1, DELETE
// ---------------------------------------------------------------------------

/// D-145: **no** revision row is ever deleted — not even a draft.
///
/// This is where the table diverges from `pricing_price`, whose DELETE arm is
/// conditional on having left draft. Here the arm is absolute, because the thing
/// being protected is not the content but the *number*: deleting a draft returns
/// `max(revision)` to its previous value, the next opened draft mints the same
/// number, and `(plan_id, revision)` then denotes two different rows over a
/// plan's lifetime — under which a stale entity tag passes its precondition
/// against the wrong one.
///
/// Both states are exercised because the message interpolates the revision and
/// the plan, so a test on one row alone would leave the rest of the branch
/// resting on a reading of the SQL rather than on a run of it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn no_revision_is_ever_deleted_not_even_a_draft() {
    let conn = applied().await;
    must_succeed(&conn, &insert(PLAN_A, 0, &[])).await;
    must_succeed(
        &conn,
        &insert(PLAN_B, 7, &[("lifecycle_state", "'published'")]),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_plan WHERE plan_id = '{PLAN_A}' AND revision = 0"),
        &format!("DELETE of revision 0 of plan {PLAN_A} is not permitted"),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_plan WHERE plan_id = '{PLAN_B}' AND revision = 7"),
        &format!("DELETE of revision 7 of plan {PLAN_B} is not permitted"),
    )
    .await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_plan_append_only()` — arm 2, the draft-plane flip whitelist
// ---------------------------------------------------------------------------

/// D-153 on the draft plane: a draft leaves only by publishing or by being
/// abandoned.
///
/// A column whitelist is scoped to non-draft rows by construction, so it says
/// nothing about where a draft may go, and this arm is the only thing standing
/// between a draft and `retired` — a state inside `uq_pricing_plan_current`,
/// which is to say a revision that becomes the plan's current one **without ever
/// having published**, and the projector sources a plan subject from exactly
/// that row. `draft -> superseded` is the other half: it leaves both partial
/// predicates at once, freeing the draft key while the revision number stays
/// consumed.
///
/// Both cases in one test, and each assertion carries the interpolated state
/// pair: the published-plane arm below raises the same sentence, and only the
/// pair says which arm answered.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_revision_leaves_only_by_publishing_or_by_being_abandoned() {
    let conn = applied().await;
    must_succeed(&conn, &insert(PLAN_A, 0, &[])).await;
    must_succeed(&conn, &insert(PLAN_B, 0, &[])).await;
    must_be_rejected(
        &conn,
        &flip(PLAN_A, 0, "superseded"),
        "lifecycle_state draft -> superseded is not a sanctioned flip",
    )
    .await;
    must_be_rejected(
        &conn,
        &flip(PLAN_B, 0, "retired"),
        "lifecycle_state draft -> retired is not a sanctioned flip",
    )
    .await;
}

// ---------------------------------------------------------------------------
// `bss.pricing_plan_append_only()` — arm 3, the frozen-column whitelist
// ---------------------------------------------------------------------------

/// The one column of `bss.pricing_plan` that may move on a frozen revision:
/// `lifecycle_state`, the sanctioned flip the whitelist exists to permit.
///
/// `row_version` is the tag and **is** frozen, named here only because the move
/// list's own doc calls it out. One spelling for the two censuses below, because
/// two spellings are two answers to what the guard owes.
const SANCTIONED_MUTABLE: [&str; 1] = ["lifecycle_state"];

/// Every column the whitelist freezes, one UPDATE each.
///
/// **Two things are proved, and only one of them by executing a statement.** Each
/// column the list names is refused, one `UPDATE` at a time; and the list names
/// every frozen column the table holds, which the census at the end asserts
/// against `information_schema` rather than against a number. A literal count
/// there proves nothing an edit to the list does not also edit.
///
/// [`the_frozen_whitelist_names_every_content_column_the_table_holds`] is the
/// neighbouring half: it holds the *trigger predicate* to the same census. A
/// column can therefore go missing from either side and be caught on that side.
///
/// The loop is the point: a whitelist maintained by hand rots one forgotten `OR`
/// at a time, and a test that moved only `plan_tier` would stay green while
/// `invoice_grouping_key` or `row_version` quietly became mutable on a frozen
/// revision. That is not an abstract hazard here — the
/// projector reads truth rows on a warm re-drive (§4.4), so a column moved under
/// a frozen `CatalogVersion` is re-materialized as though it had always been
/// that way.
///
/// The trigger is `BEFORE`, so it answers ahead of every CHECK: several of the
/// values below would also be illegal rows, and none of them gets that far.
///
/// `lifecycle_state` is deliberately **absent** from the list — freezing it would
/// forbid the sanctioned flips themselves — and the arm below is what governs it
/// instead.
///
/// The four non-draft states are exercised in the same test rather than in a
/// sibling, and that is a deliberate consequence of the guard-by-removal
/// discipline: they are two facts about **one** arm, and deleting the arm has to
/// redden exactly one test. Splitting them reddened two, which is the same
/// ambiguity a flake produces. The state coverage is not decoration — the arm's
/// condition is "not a draft", which is four states and not one, and the
/// migration's claim that an `abandoned` revision number "can never be attached
/// to a different shape" rests entirely on the tombstone being inside it.
///
/// # Each move is issued twice, and the second pass is the load-bearing one
///
/// A bare `UPDATE … SET plan_tier = 'gold'` on a published revision leaves
/// `lifecycle_state` where it is, so arm 4 below would refuse it too — it
/// refuses every non-draft UPDATE that is not `published -> superseded|retired`,
/// a state-preserving one included. Deleting this arm therefore does not make
/// those statements *land*; it makes them come back with arm 4's sentence, which
/// this suite catches only because the assertion names the message.
///
/// The statement only this arm can refuse is a frozen column moving **inside a
/// sanctioned flip** — `SET lifecycle_state = 'superseded', plan_tier = 'gold'`
/// — which arm 4 waves through by construction. That is also the realistic
/// shape of the defect: supersession is a real write the gear performs, and
/// smuggling content into it is how a frozen `CatalogVersion` would actually
/// change. Without the second pass this test would be an assertion about
/// wording; with it, deleting the arm makes every illegal write in the move list
/// below succeed.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_frozen_column_of_a_frozen_revision_refuses_to_move() {
    let conn = applied().await;
    must_succeed(
        &conn,
        &insert(PLAN_A, 0, &[("lifecycle_state", "'published'")]),
    )
    .await;

    let moves = [
        format!("plan_id = '{PLAN_B}'"),
        "revision = 5".to_owned(),
        "tenant_id = '99999999-9999-9999-9999-999999999999'".to_owned(),
        format!("sku_id = '{ACTOR}'"),
        "plan_tier = 'gold'".to_owned(),
        "plan_name = 'Renamed Under A Frozen Version'".to_owned(),
        "billing_cycle = 'recurring'".to_owned(),
        "frequency = 'monthly'".to_owned(),
        "custom_interval_n = 3".to_owned(),
        "custom_interval_unit = 'days'".to_owned(),
        "plan_tier_override = true".to_owned(),
        "purchase_min_qty = 1".to_owned(),
        "purchase_max_qty = 9".to_owned(),
        "invoice_grouping_key = 'group/1'".to_owned(),
        "available_from = '2026-01-01 00:00:00+00'".to_owned(),
        "available_to = '2026-12-01 00:00:00+00'".to_owned(),
        "created_by = '99999999-9999-9999-9999-999999999999'".to_owned(),
        "created_at_utc = '2026-08-02 09:00:00+00'".to_owned(),
        "row_version = 1".to_owned(),
        // The four `trg_pricing_plan_append_only` restated in, which `000052` and `000053`
        // added and neither guarded. Every one is inside the approval content
        // pin, so before that migration a published revision could publish an
        // entitlement its approval never signed, with the pin's digest unchanged.
        r#"entitlement_grants = '{"quotas":{"seats":9000}}'::jsonb"#.to_owned(),
        r#"allowed_change_targets = '["99999999-9999-9999-9999-999999999999"]'::jsonb"#.to_owned(),
        "comparability_rank = 42".to_owned(),
        "usage_counter_on_plan_change = 'carry'".to_owned(),
        // Slice 12's clone provenance (`pricing_plan`), frozen by
        // `trg_pricing_plan_append_only` in the same wave -- lineage nobody can trust is
        // worse than none.
        "cloned_from = '99999999-9999-9999-9999-999999999999'".to_owned(),
    ];

    // **The list covers every frozen column**, asserted against the census rather
    // than a literal length: the number and the roster are edited in one act, so a
    // count here can only restate what the list already says. Removing an entry
    // reddens this; so does a column added to the table and not authored above.
    let census = pg_support::frozen_columns(
        &conn,
        "pricing_plan",
        "pricing_plan_append_only",
        "IF NEW.plan_id",
        &SANCTIONED_MUTABLE,
    )
    .await;
    let named: std::collections::BTreeSet<&str> = moves
        .iter()
        .map(|change| {
            change
                .split(' ')
                .next()
                .expect("every move opens with the column it moves")
        })
        .collect();
    let owed: std::collections::BTreeSet<&str> = census.owed.iter().map(String::as_str).collect();
    assert_eq!(
        named, owed,
        "the move list and bss.pricing_plan's frozen-column census disagree"
    );

    for change in &moves {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_plan SET {change} \
                 WHERE plan_id = '{PLAN_A}' AND revision = 0"
            ),
            "is frozen; only a sanctioned lifecycle_state flip is permitted",
        )
        .await;
    }

    // The same list, smuggled into the one flip arm 4 sanctions. See the
    // doc comment: this pass is what makes the arm's removal an executed
    // failure rather than a difference of wording.
    for change in &moves {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_plan SET lifecycle_state = 'superseded', {change} \
                 WHERE plan_id = '{PLAN_A}' AND revision = 0"
            ),
            "is frozen; only a sanctioned lifecycle_state flip is permitted",
        )
        .await;
    }

    // And the other three states the arm covers. A tombstone whose `sku_id`
    // could be moved is a consumed revision number quietly re-pointed at
    // another product.
    must_succeed(&conn, &seeded(PLAN_B, "abandoned")).await;
    must_succeed(&conn, &seeded(PLAN_C, "superseded")).await;
    must_succeed(&conn, &seeded(PLAN_D, "retired")).await;
    for plan in [PLAN_B, PLAN_C, PLAN_D] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_plan SET sku_id = '{ACTOR}' \
                 WHERE plan_id = '{plan}' AND revision = 0"
            ),
            "is frozen; only a sanctioned lifecycle_state flip is permitted",
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// `bss.pricing_plan_append_only()` — arm 4, the published-plane flip whitelist
// ---------------------------------------------------------------------------

/// Off the draft plane there is exactly one edge, and it starts at `published`.
///
/// Each case on its own plan so that no partial index can be what answers, and
/// each assertion carries the interpolated state pair so that the draft-plane arm
/// — which raises the same sentence — cannot be mistaken for this one.
///
/// The last case is the arm's least obvious consequence and the one a reader is
/// most likely to get wrong: `published -> published` is refused. The condition
/// is a membership test on `NEW.lifecycle_state`, not a change test, so **any**
/// UPDATE of a frozen revision that leaves the state where it is falls through
/// arm 3 (nothing moved) into this one. There is no such thing as a no-op UPDATE
/// of a frozen revision; it is an error.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_frozen_revision_moves_only_from_published_to_superseded_or_retired() {
    let conn = applied().await;
    must_succeed(&conn, &seeded(PLAN_A, "published")).await;
    must_succeed(&conn, &seeded(PLAN_B, "superseded")).await;
    must_succeed(&conn, &seeded(PLAN_C, "retired")).await;
    must_succeed(&conn, &seeded(PLAN_D, "abandoned")).await;
    must_succeed(&conn, &seeded(PLAN_E, "published")).await;

    // A published revision may not walk back to the draft plane: it would enter
    // `uq_pricing_plan_open_draft` and free the key it currently occupies.
    must_be_rejected(
        &conn,
        &flip(PLAN_A, 0, "draft"),
        "lifecycle_state published -> draft is not a sanctioned flip",
    )
    .await;
    // Nor be abandoned: `abandoned` is the discarded-*draft* tombstone, and a
    // published revision reaching it would leave the plan with no current one.
    must_be_rejected(
        &conn,
        &flip(PLAN_A, 0, "abandoned"),
        "lifecycle_state published -> abandoned is not a sanctioned flip",
    )
    .await;
    // The three terminal states have no outward edges at all.
    must_be_rejected(
        &conn,
        &flip(PLAN_B, 0, "published"),
        "lifecycle_state superseded -> published is not a sanctioned flip",
    )
    .await;
    must_be_rejected(
        &conn,
        &flip(PLAN_C, 0, "published"),
        "lifecycle_state retired -> published is not a sanctioned flip",
    )
    .await;
    must_be_rejected(
        &conn,
        &flip(PLAN_D, 0, "draft"),
        "lifecycle_state abandoned -> draft is not a sanctioned flip",
    )
    .await;
    // And the state-preserving UPDATE, which is refused as well.
    must_be_rejected(
        &conn,
        &flip(PLAN_E, 0, "published"),
        "lifecycle_state published -> published is not a sanctioned flip",
    )
    .await;
}

/// **The whitelist names every content column the table holds** — on the engine
/// that ships, and this is the case the sibling above structurally cannot be.
///
/// `every_frozen_column_of_a_frozen_revision_refuses_to_move` maintains its move
/// list by hand, and its own doc warns that "a whitelist maintained by hand rots
/// one forgotten `OR` at a time". It did: `pricing_plan` gained columns that
/// reached neither engine's guard, and neither that loop nor its `SQLite` twin
/// could see it — both enumerate the
/// trigger, so both are blind to exactly what the trigger omits.
///
/// This case reads the column list off `information_schema` and the predicate out
/// of `pg_proc.prosrc`, so a column added to `pricing_plan` and forgotten in
/// `bss.pricing_plan_append_only()` reddens here rather than in production.
///
/// The reading itself moved into `pg_support::frozen_columns` on 2026-08-13, when
/// `pricing_price` — the table that holds money — was brought under the same
/// census. Two spellings of one census are two things that can disagree about
/// what a guard owes; the arm slicing in particular is subtle enough that it must
/// not be re-derived per table.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_frozen_whitelist_names_every_content_column_the_table_holds() {
    let conn = applied().await;
    let census = pg_support::frozen_columns(
        &conn,
        "pricing_plan",
        "pricing_plan_append_only",
        "IF NEW.plan_id",
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
        "these columns are on bss.pricing_plan and absent from the frozen-column \
         whitelist, so an ad-hoc UPDATE moves them under a frozen CatalogVersion: \
         {:?}",
        census.missing()
    );
}
