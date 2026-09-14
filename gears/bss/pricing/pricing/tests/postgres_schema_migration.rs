//! Slice 11's migration schedule **on the engine that runs in production**
//! (`design/11-lifecycle.md` §4/§6, `inst-ms-api`, `inst-mg-idem`,
//! `inst-mg-cancel`, `inst-mst-start`, `inst-mst-complete`, `inst-mst-cancel`,
//! `inst-mst-cancel-inflight`, D-34, D-49, D-65).
//!
//! # The hole this closes
//!
//! `bss.pricing_migration` carries twelve `CHECK` constraints and a five-arm
//! append-only function, and until this file **no Postgres suite drove a single
//! statement at it**. `sqlite_migration_repo` proves four of the five arms on the
//! mirror; `postgres_migrations` proves the objects exist here by name and runs
//! nothing. The two arms are written separately — one PL/pgSQL function against
//! four `RAISE(ABORT, …)` triggers — and only the `SQLite` side carries a
//! trigger-**body** digest census, so a lost disjunct on the shipping engine was
//! invisible to every gate. That is the standing half of the debt D-260 records.
//!
//! Not one of the twelve `CHECK`s had a behavioural case on **either** engine:
//! `sqlite_migration_repo` drives the repository, and a repository only ever
//! offers legal values — every flip it emits carries its own instant and its own
//! compare-and-swap. Driving it catches a constraint that got *narrower* and is
//! blind to one that was dropped.
//!
//! # Which layer is reachable from which statement
//!
//! Stated once here rather than rediscovered per case. The append-only function
//! is `BEFORE UPDATE OR DELETE`, so on an `UPDATE` it answers **before** any
//! constraint is evaluated:
//!
//! * a `CHECK` is reachable from an `INSERT` always, and from an `UPDATE` only
//!   when no arm raises first — which is why every constraint case below is
//!   driven by an `INSERT`;
//! * `chk_pricing_migration_state` is therefore `INSERT`-only in practice: a flip
//!   to a state outside §4's four is answered by the whitelist arm;
//! * and a statement refused by two layers proves neither, so each case below
//!   moves exactly **one** thing away from a row that lands.
//!
//! Run with:
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_migration -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const MIGRATION: &str = "11111111-1111-1111-1111-111111111111";
const OTHER_MIGRATION: &str = "'22222222-2222-2222-2222-222222222222'";
const TENANT: &str = "'33333333-3333-3333-3333-333333333333'";
const OTHER_TENANT: &str = "'44444444-4444-4444-4444-444444444444'";
const SOURCE: &str = "'55555555-5555-5555-5555-555555555555'";
const TARGET: &str = "'66666666-6666-6666-6666-666666666666'";
const THIRD_PLAN: &str = "'77777777-7777-7777-7777-777777777777'";
const ACTOR: &str = "'88888888-8888-8888-8888-888888888888'";
const OTHER_ACTOR: &str = "'99999999-9999-9999-9999-999999999999'";

// The instants, as SQL literals, on `sqlite_migration_repo`'s own scale so the
// two suites describe one fixture. A migration is created, started an hour on,
// and ends the same afternoon; its effect is three months out because D-49's
// notice period is measured in months.
const CREATED: &str = "'2026-08-07T10:00:00+00:00'";
const STARTED: &str = "'2026-08-07T11:00:00+00:00'";
const CANCELLED: &str = "'2026-08-07T12:00:00+00:00'";
const COMPLETED: &str = "'2026-08-07T13:00:00+00:00'";
const ANNOUNCED: &str = "'2026-08-07T10:00:00+00:00'";
const EFFECTIVE: &str = "'2026-11-05T10:00:00+00:00'";
/// Before the row exists at all — what the three ordering constraints refuse.
const BEFORE_CREATED: &str = "'2026-08-06T10:00:00+00:00'";
/// After the row is created but **before** it started, for the completion order.
const BEFORE_STARTED: &str = "'2026-08-07T10:30:00+00:00'";

/// D-65's set: what the executor was handed at `POST .../start` and what every
/// repeat call must be answered with.
const EXCLUSION: &str = r#"'{"locked":["sub-1"]}'"#;
/// The record `inst-mst-complete` closes a run with.
const RECORD: &str = r#"'{"processed":12,"excluded":[],"failed":[]}'"#;
/// §6's scope and the schedule-time deltas, constant across every case here:
/// nothing below is about their content, only about their being frozen.
const SCOPE: &str = r#"'{"kind":"all"}'"#;
const DELTA_REPORT: &str = r#"'{"locked":[],"entitlement":[],"addon":[],"boundary":[]}'"#;

/// One schedule, as the SQL literals a statement carries. Every case is this row
/// with exactly **one** field moved, which is what arms it: the base lands, so
/// only a rule mentioning the moved field can be what refuses.
#[derive(Clone)]
struct Schedule {
    id: &'static str,
    tenant: &'static str,
    source_plan: &'static str,
    revision: &'static str,
    target_plan: &'static str,
    effective: &'static str,
    announced: &'static str,
    state: &'static str,
    exclusion: &'static str,
    record: &'static str,
    created: &'static str,
    started: &'static str,
    completed: &'static str,
    cancelled: &'static str,
}

/// A legal `scheduled` row: announced today, in force in November, nothing
/// executed. `revision` is `0` because that is the first revision a plan mints,
/// so the base fixture sits **on** the ordinal boundary rather than safely above
/// it.
fn scheduled() -> Schedule {
    Schedule {
        id: MIGRATION,
        tenant: TENANT,
        source_plan: SOURCE,
        revision: "0",
        target_plan: TARGET,
        effective: EFFECTIVE,
        announced: ANNOUNCED,
        state: "scheduled",
        exclusion: "NULL",
        record: "NULL",
        created: CREATED,
        started: "NULL",
        completed: "NULL",
        cancelled: "NULL",
    }
}

/// A legal row in each of §4's four states, walked to by the columns that state
/// actually implies.
///
/// The `cancelled` form here is the **never-started** one (`inst-mst-cancel`,
/// M3): `cancelled` is reachable from both `scheduled` and `in_progress`, which
/// is exactly why `chk_pricing_migration_started_required` is written as two
/// implications rather than as the biconditional the other two flip instants get.
fn in_state(state: &'static str) -> Schedule {
    match state {
        "scheduled" => scheduled(),
        "in_progress" => Schedule {
            state,
            exclusion: EXCLUSION,
            started: STARTED,
            ..scheduled()
        },
        "completed" => Schedule {
            state,
            exclusion: EXCLUSION,
            record: RECORD,
            started: STARTED,
            completed: COMPLETED,
            ..scheduled()
        },
        "cancelled" => Schedule {
            state,
            cancelled: CANCELLED,
            ..scheduled()
        },
        other => panic!("{other} is not one of section 4's states"),
    }
}

fn insert(row: &Schedule) -> String {
    let Schedule {
        id,
        tenant,
        source_plan,
        revision,
        target_plan,
        effective,
        announced,
        state,
        exclusion,
        record,
        created,
        started,
        completed,
        cancelled,
    } = row;
    format!(
        "INSERT INTO bss.pricing_migration \
         (migration_id, tenant_id, source_plan_id, source_revision, target_plan_id, \
          effective_at, announced_at, scope, state, delta_report, \
          exclusion_snapshot, completion_record, created_by, created_at, \
          started_at, completed_at, cancelled_at) \
         VALUES ('{id}', {tenant}, {source_plan}, {revision}, {target_plan}, \
          {effective}, {announced}, {SCOPE}, '{state}', {DELTA_REPORT}, \
          {exclusion}, {record}, {ACTOR}, {created}, \
          {started}, {completed}, {cancelled})"
    )
}

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

/// A fresh database holding one legal row in `state`.
async fn seeded(state: &'static str) -> DatabaseConnection {
    let conn = applied().await;
    must_succeed(&conn, &insert(&in_state(state))).await;
    conn
}

/// What the fixture row says it is now.
async fn state_of(conn: &DatabaseConnection) -> String {
    conn.query_one_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!("SELECT state AS s FROM bss.pricing_migration WHERE migration_id = '{MIGRATION}'"),
    ))
    .await
    .expect("read the state")
    .expect("the fixture row is there")
    .try_get::<String>("", "s")
    .expect("state is text")
}

/// How many rows carry the fixture's id.
async fn rows(conn: &DatabaseConnection) -> i64 {
    conn.query_one_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!(
            "SELECT count(*) AS n FROM bss.pricing_migration WHERE migration_id = '{MIGRATION}'"
        ),
    ))
    .await
    .expect("run the count")
    .expect("the count returns a row")
    .try_get::<i64>("", "n")
    .expect("read the count")
}

// ---------------------------------------------------------------------------
// The twelve CHECKs, each driven by an INSERT that isolates it.
// ---------------------------------------------------------------------------

/// §4's four states and no fifth — and all four of them, which is the half a
/// refusal-only case cannot prove. A vocabulary narrowed to three would leave one
/// legal state unwritable, and the schedule that could not be recorded is the one
/// an executor is mid-way through.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_state_vocabulary_is_section_4s_four() {
    let conn = applied().await;
    for state in ["paused", "", "Scheduled", "in-progress"] {
        must_be_rejected(
            &conn,
            &insert(&Schedule {
                state,
                ..scheduled()
            }),
            "chk_pricing_migration_state",
        )
        .await;
    }

    for state in ["scheduled", "in_progress", "completed", "cancelled"] {
        let conn = applied().await;
        must_succeed(&conn, &insert(&in_state(state))).await;
        assert_eq!(state_of(&conn).await, state);
    }
}

/// A revision is an ordinal. The base fixture already sits on `0` — the first
/// revision a plan mints — so this constraint is pinned at its boundary as well
/// as past it, and a `> 0` written where `>= 0` belongs would redden every case
/// in this file rather than none of them.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_source_revision_is_an_ordinal() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            revision: "-1",
            ..scheduled()
        }),
        "chk_pricing_migration_source_revision",
    )
    .await;
}

/// **An addition to §6 rather than a transcription of it**, and this case is why
/// it survives review: a migration from a plan to itself would emit
/// `PlanMigrationScheduled` asking Subscriptions to create `PlanLink`s onto the
/// plan every subscriber is already on.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_migration_never_targets_its_own_plan() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            target_plan: SOURCE,
            ..scheduled()
        }),
        "chk_pricing_migration_distinct_plans",
    )
    .await;
}

/// **D-49's row-local half, and the boundary that shows it is deliberately the
/// weak one.**
///
/// The rule is `effective_at >= announcement + the tenant's configured notice
/// period`, floor 60 days — and the notice value lives in
/// `pricing_policy_object`, another table, so no `CHECK` here can state it. What
/// is row-local is `announced_at <= effective_at`, and the equal case below must
/// **land**: writing `<` here would be a schema quietly claiming a notice period
/// the domain is what enforces.
#[tokio::test]
#[ignore = "requires Docker"]
async fn an_effect_never_precedes_its_announcement() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            announced: "'2027-01-01T00:00:00+00:00'",
            ..scheduled()
        }),
        "chk_pricing_migration_announced_before_effective",
    )
    .await;

    must_succeed(
        &conn,
        &insert(&Schedule {
            announced: EFFECTIVE,
            ..scheduled()
        }),
    )
    .await;
}

/// Both states that imply a start require one, driven **per disjunct**: a
/// constraint narrowed to `in_progress` would leave a `completed` run claiming it
/// finished a run it never began, and D-36's re-validation and D-65's exclusion
/// set both hang off that declaration.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_started_state_requires_its_start_instant() {
    let conn = applied().await;
    for state in ["in_progress", "completed"] {
        let completed = if state == "completed" {
            COMPLETED
        } else {
            "NULL"
        };
        must_be_rejected(
            &conn,
            &insert(&Schedule {
                state,
                completed,
                ..scheduled()
            }),
            "chk_pricing_migration_started_required",
        )
        .await;
    }
}

/// The other half of §4's reachable set: a schedule that has not been started has
/// no start instant. A row in `scheduled` carrying one is a run whose executor
/// declared itself and then was told nothing had happened.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_scheduled_run_has_not_started() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            exclusion: EXCLUSION,
            started: STARTED,
            ..scheduled()
        }),
        "chk_pricing_migration_scheduled_unstarted",
    )
    .await;
}

/// The completion instant is **exactly** the completed state, both ways.
///
/// Two statements because the constraint is a biconditional and half of it is one
/// edit from being an implication: without the forward half a run reports
/// completion with no instant an auditor can place it at; without the reverse
/// half a `scheduled` row carries a completion instant for a run that has not
/// started.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_completion_instant_is_exactly_the_completed_state() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            completed: "NULL",
            ..in_state("completed")
        }),
        "chk_pricing_migration_completed_at",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            completed: COMPLETED,
            ..scheduled()
        }),
        "chk_pricing_migration_completed_at",
    )
    .await;
}

/// The cancellation instant is **exactly** the cancelled state, both ways — the
/// sibling of the case above, and the one D-34's in-flight cancel writes.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_cancellation_instant_is_exactly_the_cancelled_state() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            cancelled: "NULL",
            ..in_state("cancelled")
        }),
        "chk_pricing_migration_cancelled_at",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            cancelled: CANCELLED,
            ..scheduled()
        }),
        "chk_pricing_migration_cancelled_at",
    )
    .await;
}

/// **D-65's persist half, made physical.**
///
/// `POST .../start` computes the exclusion set once, persists it, and answers
/// every repeat call with the stored one. The biconditional is what leaves no
/// state in which a replay has nothing to replay: a started run without its set,
/// or a set on a run that never started, are both refused.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_started_run_carries_the_exclusion_set_it_was_handed() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            exclusion: "NULL",
            ..in_state("in_progress")
        }),
        "chk_pricing_migration_exclusion_snapshot",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            exclusion: EXCLUSION,
            ..scheduled()
        }),
        "chk_pricing_migration_exclusion_snapshot",
    )
    .await;
}

/// The three ordering constraints, each driven by the one instant it orders.
///
/// The completion case is the sharp one: its instant is **after** `created_at`
/// and before `started_at`, so `chk_pricing_migration_started_order` is satisfied
/// and only `chk_pricing_migration_completed_order` can be what answers. A
/// fixture that had simply dated it in the past would have been refused by the
/// wrong rule and proved nothing.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_flip_instants_cannot_precede_what_they_follow() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            started: BEFORE_CREATED,
            ..in_state("in_progress")
        }),
        "chk_pricing_migration_started_order",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            completed: BEFORE_STARTED,
            ..in_state("completed")
        }),
        "chk_pricing_migration_completed_order",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            cancelled: BEFORE_CREATED,
            ..in_state("cancelled")
        }),
        "chk_pricing_migration_cancelled_order",
    )
    .await;
}

/// **M2 is the primary key `(tenant_id, migration_id)`, and that is the whole of
/// `inst-mg-idem`.**
///
/// `migration_id` is client-supplied, so a timed-out retry carries the id the
/// first call did; `insert_or_load` is an `ON CONFLICT DO NOTHING` plus a load,
/// and this is the conflict it relies on. Without the key a retry would schedule
/// a second migration of the same plan and Subscriptions would be asked to
/// re-bind every subscriber twice.
///
/// **The retry is the *same tenant* sending the id again.** This case demonstrated
/// the rejection with `tenant: OTHER_TENANT` until 2026-08-11 while calling it
/// "a client that timed out and rebuilt its request" — a different tenant is not a
/// retry, and what the assertion actually pinned was the deployment-wide namespace
/// `pricing_migration` removed. The neighbour's insert is now its own case below,
/// asserting the opposite outcome.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_migration_id_holds_one_row_per_tenant() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&scheduled())).await;
    // The retry: one tenant, one id, a body rebuilt after a timeout.
    must_be_rejected(
        &conn,
        &insert(&Schedule {
            effective: "'2027-01-01T00:00:00+00:00'",
            ..scheduled()
        }),
        "pricing_migration_pkey",
    )
    .await;
    assert_eq!(rows(&conn).await, 1);
}

/// **A neighbour holding the same client-chosen id is not this tenant's conflict.**
///
/// The other half of the key, and the reason it was widened. While
/// `migration_id` alone was the primary key, this insert was refused — which made
/// the route an existence oracle (409 for a taken id, 202 for a free one) and,
/// worse, a permanent denial: the refusal says *retry*, a retry collides
/// identically forever, and `trg_pricing_migration_no_delete` refuses the DELETE
/// that would free it. Any tenant could reserve arbitrary ids against every other.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_neighbours_identical_migration_id_is_not_a_conflict() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&scheduled())).await;
    must_succeed(
        &conn,
        &insert(&Schedule {
            tenant: OTHER_TENANT,
            effective: "'2027-01-01T00:00:00+00:00'",
            ..scheduled()
        }),
    )
    .await;
    assert_eq!(rows(&conn).await, 2, "both tenants hold their own schedule");
}

// ---------------------------------------------------------------------------
// The five arms of `pricing_migration_append_only`.
// ---------------------------------------------------------------------------

/// **Cancel is a state, not a deletion**, and absence must not read the same as
/// cancellation: `inst-mg-cancel` has the executor re-read the schedule before
/// each batch, and a deleted row would answer "no such migration" to a party
/// whose correct behaviour is to **stop**.
///
/// Armed against the trigger's **event binding** rather than against the arm's
/// body, which is the honest description on this engine: with the `DELETE`
/// branch gone the function would fall through to comparisons against an
/// unassigned `NEW` and still refuse. What would let the statement land is the
/// trigger being narrowed to `BEFORE UPDATE` — the exact drift that left
/// `pricing_bulk_row_lock` releasing nothing while every case stayed green — and
/// that is what the row count below detects.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_schedule_is_cancelled_and_never_deleted() {
    for state in ["scheduled", "in_progress", "cancelled"] {
        let conn = seeded(state).await;
        must_be_rejected(
            &conn,
            &format!("DELETE FROM bss.pricing_migration WHERE migration_id = '{MIGRATION}'"),
            "is not permitted",
        )
        .await;
        assert_eq!(
            rows(&conn).await,
            1,
            "the schedule must still be there for the executor that re-reads it"
        );
    }
}

/// A completed or cancelled run is immutable history: the record an auditor and
/// an executor both read afterwards, and nothing may edit it — not even the
/// completion record, which is the one column a late-arriving executor would most
/// plausibly want to append to.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_terminal_run_is_immutable_history() {
    for state in ["completed", "cancelled"] {
        let conn = seeded(state).await;
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_migration SET completion_record = '{{\"processed\":99}}' \
                 WHERE migration_id = '{MIGRATION}'"
            ),
            "immutable history",
        )
        .await;
    }
}

/// **Every bound column, one statement each — this is the shape a lost disjunct
/// hides in.**
///
/// The arm is eleven `IS DISTINCT FROM` comparisons in one `IF`, and a single
/// case would leave ten of them provable by inspection only. Each statement below
/// is legal in every other respect — no `CHECK` refuses it, no other arm fires —
/// so exactly one disjunct stands between it and landing.
///
/// `delta_report` is the one with a design sentence behind it: §6 calls it the
/// deltas "at schedule time", the evidence of what the operator confirmed
/// against, which is why D-36's execution-time re-resolution lands in
/// `exclusion_snapshot` instead of overwriting it.
#[tokio::test]
#[ignore = "requires Docker"]
async fn every_bound_column_is_bound() {
    let conn = seeded("scheduled").await;
    for (column, value) in [
        ("migration_id", OTHER_MIGRATION),
        ("tenant_id", OTHER_TENANT),
        ("source_plan_id", THIRD_PLAN),
        ("source_revision", "1"),
        ("target_plan_id", THIRD_PLAN),
        ("effective_at", "'2027-01-01T00:00:00+00:00'"),
        ("announced_at", "'2026-08-01T00:00:00+00:00'"),
        ("scope", r#"'{"kind":"segment"}'"#),
        ("delta_report", r#"'{"locked":["sub-9"]}'"#),
        ("created_by", OTHER_ACTOR),
        ("created_at", "'2026-08-01T00:00:00+00:00'"),
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_migration SET {column} = {value} \
                 WHERE migration_id = '{MIGRATION}'"
            ),
            "bound to its source, target",
        )
        .await;
    }
}

/// **§4's four edges, and the fifth that is not one.**
///
/// The four that land are as load-bearing as the refusal: an arm narrowed by one
/// disjunct would leave a real transition unwritable, and `in_progress ->
/// cancelled` is the one to watch — it is D-34's stop-the-bleeding control, added
/// on 2026-08-07, and the arm predates the sentence.
///
/// The refusal carries every column the edge would need, so no `CHECK` can be
/// what answers: `scheduled -> completed` arrives with its start instant, its
/// exclusion set and its completion instant all present, and only the whitelist
/// stands between it and landing. That is the edge §4 leaves out on purpose —
/// a run cannot be reported processed by an executor that never declared it
/// started.
#[tokio::test]
#[ignore = "requires Docker"]
async fn only_section_4s_four_edges_are_sanctioned() {
    for (from, to, assignment) in [
        (
            "scheduled",
            "in_progress",
            format!(
                "state = 'in_progress', started_at = {STARTED}, exclusion_snapshot = {EXCLUSION}"
            ),
        ),
        (
            "scheduled",
            "cancelled",
            format!("state = 'cancelled', cancelled_at = {CANCELLED}"),
        ),
        (
            "in_progress",
            "completed",
            format!(
                "state = 'completed', completed_at = {COMPLETED}, completion_record = {RECORD}"
            ),
        ),
        (
            "in_progress",
            "cancelled",
            format!(
                "state = 'cancelled', cancelled_at = {CANCELLED}, completion_record = {RECORD}"
            ),
        ),
    ] {
        let conn = seeded(from).await;
        must_succeed(
            &conn,
            &format!(
                "UPDATE bss.pricing_migration SET {assignment} \
                 WHERE migration_id = '{MIGRATION}'"
            ),
        )
        .await;
        assert_eq!(
            state_of(&conn).await,
            to,
            "the sanctioned edge must have taken effect, not merely not errored"
        );
    }

    let conn = seeded("scheduled").await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_migration SET state = 'completed', started_at = {STARTED}, \
             exclusion_snapshot = {EXCLUSION}, completed_at = {COMPLETED} \
             WHERE migration_id = '{MIGRATION}'"
        ),
        "is not a sanctioned transition",
    )
    .await;
    assert_eq!(state_of(&conn).await, "scheduled");
}

/// **D-34's own sentence, guarded at two layers rather than one — and the case
/// says which one answers.**
///
/// `completed` is terminal and uncancellable, so the 409 the route returns
/// (`MIGRATION_COMPLETED`) is not the only thing standing behind it. This
/// statement is refused **twice**: the immutable-history arm answers first and
/// the whitelist would answer if it did not, so the case is armed against the
/// rule rather than against either arm alone. It is here because the rule is a
/// design sentence with an executor's behaviour hanging off it, not because it
/// isolates a guard — a distinction worth writing down where the next reader
/// will look for it.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_completed_run_is_uncancellable() {
    let conn = seeded("completed").await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_migration SET state = 'cancelled', cancelled_at = {CANCELLED} \
             WHERE migration_id = '{MIGRATION}'"
        ),
        "immutable history",
    )
    .await;
    assert_eq!(state_of(&conn).await, "completed");
}

/// **D-65's replay half: the exclusion set is computed once and replayed
/// verbatim.**
///
/// A recompute could differ from the set the executor already honoured, and the
/// executor is mid-run against the first one. Nothing else refuses this
/// statement — `exclusion_snapshot` is not a bound column, the state does not
/// move, the run is not terminal — so the arm is the only thing between it and
/// landing.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_exclusion_set_is_replayed_and_never_recomputed() {
    let conn = seeded("in_progress").await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_migration \
             SET exclusion_snapshot = '{{\"locked\":[\"sub-1\",\"sub-2\"]}}' \
             WHERE migration_id = '{MIGRATION}'"
        ),
        "computed once and replayed verbatim",
    )
    .await;

    // Re-persisting the **same** set is not a recompute, and the arm keys on the
    // value rather than on the column having been written: a start that replays
    // must not be refused by the guard that exists to make replay possible.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_migration SET exclusion_snapshot = {EXCLUSION} \
             WHERE migration_id = '{MIGRATION}'"
        ),
    )
    .await;
}
