//! Slice 12's bulk-operation state machine **on the engine that runs in
//! production** (`design/12-operator-efficiency.md` §4, §6).
//!
//! `sqlite_bulk_operation_store` proves the same rules against the mirror. This
//! suite exists because that is not the same thing: the two arms are written
//! separately — one PL/pgSQL function against four `RAISE(ABORT, …)` triggers —
//! and the `SQLite` side is additionally covered by a trigger-**body** digest
//! census that Postgres has no equivalent of. Until this file, dropping a
//! disjunct from the PL/pgSQL edge list kept every gate green while production
//! refused every conflicted run.
//!
//! Run with:
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_bulk_operation -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const OP: &str = "11111111-1111-1111-1111-111111111111";
const TENANT: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "33333333-3333-3333-3333-333333333333";
/// A second run, for the cases that need two rows to collide.
const OTHER_OP: &str = "44444444-4444-4444-4444-444444444444";
/// A second tenant, so O4's key can be shown to be scoped to one.
const OTHER_TENANT: &str = "55555555-5555-5555-5555-555555555555";
/// A third run, for D-307's cross-kind case: it needs one row per kind under one
/// key *and* a second row of the same kind to collide with.
const THIRD_OP: &str = "66666666-6666-6666-6666-666666666666";

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

fn seed(kind: &str, state: &str) -> String {
    seed_as(OP, TENANT, kind, state, "ck-1")
}

/// The same row with the three columns O4's uniqueness is built from — the
/// operation's own id, its tenant and its client key — chosen by the caller, so
/// a case can put two rows in the collision the index is about and vary one axis
/// of it at a time.
fn seed_as(op: &str, tenant: &str, kind: &str, state: &str, client_key: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_bulk_operation \
         (operation_id, tenant_id, kind, state, client_key, report, submitted_by, submitted_at) \
         VALUES ('{op}', '{tenant}', '{kind}', '{state}', '{client_key}', '{{}}'::jsonb, \
         '{ACTOR}', now())"
    )
}

fn move_to(state: &str) -> String {
    move_to_with(state, terminal(state).then_some("now()"))
}

/// The states that end a run, which is also the set
/// `chk_pricing_bulk_operation_completed_at` names. `rejected` joined it with
/// D-267: a refused batch approval is an outcome, not a pause.
fn terminal(state: &str) -> bool {
    matches!(
        state,
        "validation_failed" | "completed" | "completed_with_conflicts" | "rejected"
    )
}

/// The same move with the end instant chosen by the caller, so a case can ask
/// what the `completed_at` agreement refuses rather than only what it admits.
fn move_to_with(state: &str, completed: Option<&str>) -> String {
    let completed = completed.unwrap_or("NULL");
    format!(
        "UPDATE bss.pricing_bulk_operation SET state = '{state}', completed_at = {completed} \
         WHERE operation_id = '{OP}'"
    )
}

/// Every edge §4 draws is walkable on Postgres too — including
/// `committing → completed_with_conflicts`, which carries both `inst-bs-done`'s
/// conflict outcome and `inst-bs-abort`. A guard written too tight here would
/// leave every conflicted run unclosable in production and nothing else would
/// say so.
///
/// **Each move is read back.** A statement that did not error is not a
/// transition that happened: an `UPDATE` matching no row succeeds silently, and
/// a `BEFORE` trigger that returned `NULL` would cancel the statement with no
/// error at all (`pg_support`'s module doc and
/// `postgres_schema_bulk_row_lock.rs`). Without the read-back the single-step
/// path `("import", ["validation_failed"])` has no later move to trip over the
/// cancellation either, exactly as
/// `postgres_schema_migration.rs` and `postgres_schema_bulk_row_lock.rs`
/// already read their own moves back.
#[tokio::test]
#[ignore = "requires Docker"]
async fn every_sanctioned_edge_is_walkable() {
    for (kind, path) in [
        ("import", vec!["committing", "completed"]),
        (
            "repricing",
            vec![
                "awaiting_approval",
                "committing",
                "completed_with_conflicts",
            ],
        ),
        ("import", vec!["validation_failed"]),
        // D-267's edge: the approval was refused, and the run ends there.
        ("repricing", vec!["awaiting_approval", "rejected"]),
    ] {
        // A fresh database per path: the run is undeletable by design, so the
        // four paths cannot share one.
        let conn = applied().await;
        must_succeed(&conn, &seed(kind, "validating")).await;
        assert_eq!(state_of(&conn).await, "validating", "the run is born there");
        for state in path {
            must_succeed(&conn, &move_to(state)).await;
            assert_eq!(
                state_of(&conn).await,
                state,
                "the move to `{state}` must have taken effect, not merely not errored"
            );
        }
    }
}

/// The run's stored `state`.
///
/// The one thing a `must_succeed` on an `UPDATE` cannot tell you, and therefore
/// the whole point of reading it: `pricing_bulk_operation` takes no `DELETE`, so
/// the row is always there and a `None` here would itself be a defect.
async fn state_of(conn: &DatabaseConnection) -> String {
    conn.query_one_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!("SELECT state AS v FROM bss.pricing_bulk_operation WHERE operation_id = '{OP}'"),
    ))
    .await
    .expect("read the run's state")
    .expect("the run must still be there")
    .try_get::<String>("", "v")
    .expect("read the state column")
}

/// **A rejected run is over, so it carries an end instant** (D-267) — the half
/// of the migration that lives in `chk_pricing_bulk_operation_completed_at`
/// rather than in the edge list, and the one Postgres names in the refusal.
/// Asked through the update path because a `BEFORE` trigger answers ahead of a
/// `CHECK`: at `INSERT` the born-validating arm would reply instead.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_rejected_run_carries_the_instant_it_ended() {
    let conn = applied().await;
    must_succeed(&conn, &seed("repricing", "validating")).await;
    must_succeed(&conn, &move_to("awaiting_approval")).await;
    must_be_rejected(
        &conn,
        &move_to_with("rejected", None),
        "chk_pricing_bulk_operation_completed_at",
    )
    .await;
    must_succeed(&conn, &move_to("rejected")).await;
}

/// **`rejected` is terminal and only `awaiting_approval` reaches it** — the two
/// properties that make D-267 one state rather than an escape hatch, on the
/// engine that ships. A run that left `rejected` would commit rows an approver
/// declined; a run that entered it from anywhere else was never refused
/// anything.
#[tokio::test]
#[ignore = "requires Docker"]
async fn rejected_is_terminal_and_reachable_only_from_a_pending_approval() {
    let conn = applied().await;
    must_succeed(&conn, &seed("repricing", "validating")).await;
    // Not from the initial state: no approval is outstanding.
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;
    // Not from `committing`: the decision has already been taken.
    must_succeed(&conn, &move_to("committing")).await;
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;
    // Nor from a terminal state.
    must_succeed(&conn, &move_to("completed")).await;
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;

    // And nothing leaves `rejected`.
    let conn = applied().await;
    must_succeed(&conn, &seed("repricing", "validating")).await;
    must_succeed(&conn, &move_to("awaiting_approval")).await;
    must_succeed(&conn, &move_to("rejected")).await;
    for onward in ["committing", "completed", "awaiting_approval", "validating"] {
        must_be_rejected(&conn, &move_to(onward), "not an edge").await;
    }
}

/// **An import can never be rejected, and D-267 adds no clause saying so.**
/// `rejected` is reachable only from `awaiting_approval`, which
/// `chk_pricing_bulk_operation_import_never_awaits` already forbids an import,
/// so the new edge inherits D-137. Verified rather than argued — a constraint
/// repeating a rule that already holds could never fail, and nothing here would
/// tell it from one that does.
#[tokio::test]
#[ignore = "requires Docker"]
async fn an_import_can_never_be_rejected() {
    let conn = applied().await;
    // Birth is asked first, on an empty table: with the row already seeded, the
    // primary key would be a second reason to refuse and the case could not say
    // which arm answered.
    must_be_rejected(&conn, &seed("import", "rejected"), "born validating").await;

    must_succeed(&conn, &seed("import", "validating")).await;
    // Refused by the edge list, not by the import `CHECK`: an import never gets
    // to the state that `CHECK` is about.
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;
    must_be_rejected(
        &conn,
        &move_to("awaiting_approval"),
        "chk_pricing_bulk_operation_import_never_awaits",
    )
    .await;
}

/// A run is born `validating` and in no other state — the arm that was missing
/// for one commit, here on the engine where its absence would have shipped.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_run_cannot_be_born_past_the_state_machine() {
    let conn = applied().await;
    for state in [
        "committing",
        "awaiting_approval",
        "completed",
        "completed_with_conflicts",
        "validation_failed",
        "rejected",
    ] {
        must_be_rejected(&conn, &seed("repricing", state), "born validating").await;
    }
}

/// D-137: an import can never park awaiting an approval nothing can grant.
#[tokio::test]
#[ignore = "requires Docker"]
async fn an_import_cannot_await_an_approval_that_can_never_come() {
    let conn = applied().await;
    must_succeed(&conn, &seed("import", "validating")).await;
    must_be_rejected(
        &conn,
        &move_to("awaiting_approval"),
        "chk_pricing_bulk_operation_import_never_awaits",
    )
    .await;
}

/// The three terminal states are terminal, and a state may not be skipped.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_machine_admits_no_edge_section_four_does_not_draw() {
    let conn = applied().await;
    must_succeed(&conn, &seed("repricing", "validating")).await;
    // Skipping straight to a terminal state.
    must_be_rejected(&conn, &move_to("completed"), "not an edge").await;
    // And a terminal record never moves again.
    must_succeed(&conn, &move_to("committing")).await;
    must_succeed(&conn, &move_to("completed")).await;
    must_be_rejected(&conn, &move_to("committing"), "not an edge").await;
}

/// **`kind` is §4's two flows and nothing else**, and Postgres names the
/// constraint that says so.
///
/// This one fails **open**: `chk_pricing_bulk_operation_kind` refuses a value,
/// so a rewrite that dropped it or widened its list would not break a single
/// path — it would let a third token land, and `bulk_repo` maps a row whose
/// `kind` it cannot parse to `CorruptRow`. The run then reads as a 500 rather
/// than as a run, in the table whose report is the operator-facing record of
/// money that moved, and no other case in either suite would notice.
///
/// Asked at `INSERT` with the state left at `validating`, because
/// `trg_pricing_bulk_operation_transitions`'s born-validating arm answers ahead
/// of every `CHECK` on this table (D-261's shadowing); the `UPDATE` path is
/// closed too, `kind` being one of the frozen columns.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_kind_outside_the_two_flows_is_refused() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &seed("migration", "validating"),
        "chk_pricing_bulk_operation_kind",
    )
    .await;
    // The near miss, so the constraint is shown to be about the vocabulary and
    // not about the column being non-empty.
    must_be_rejected(
        &conn,
        &seed("Import", "validating"),
        "chk_pricing_bulk_operation_kind",
    )
    .await;
    // The same statement with only `kind` changed lands, which is what makes the
    // two refusals above facts about this constraint rather than about the
    // fixture: with it dropped, both would have landed too.
    must_succeed(&conn, &seed("import", "validating")).await;
}

/// **O4's idempotency, on the engine that runs in production**: one operation
/// per client key per tenant, and Postgres names the index.
///
/// The mirror's case can only observe that *something* unique refused —
/// `SQLite` names the column list for a plain unique index and this suite's twin
/// asserts the bare word `UNIQUE` — so until this case the index that carries O4
/// had no assertion naming it anywhere in the schema tier. Dropped, a retried
/// submit opens a **second** run over the same rows: the first run's locks and
/// journal are keyed by its own `operation_id`, so the second re-applies every
/// repricing the first already applied, which is exactly the double application
/// a client key exists to make impossible.
///
/// The third statement is what makes the case about `(tenant_id, client_key)`
/// rather than about `client_key`: one tenant's key must not exhaust another's.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_client_key_opens_one_run_per_tenant() {
    let conn = applied().await;
    must_succeed(&conn, &seed("import", "validating")).await;

    must_be_rejected(
        &conn,
        &seed_as(OTHER_OP, TENANT, "import", "validating", "ck-1"),
        "uq_pricing_bulk_operation_client_key",
    )
    .await;

    must_succeed(
        &conn,
        &seed_as(OTHER_OP, OTHER_TENANT, "import", "validating", "ck-1"),
    )
    .await;
}

/// **D-307's cross-kind admission, on the engine that ships** — Z6-5.
///
/// The case above seeds `import`/`import` and varies only the tenant, so it would
/// pass identically against the **pre-D-307** `(tenant_id, client_key)` index: it
/// proves the key is per tenant and says nothing about the kind. D-307's decision
/// is the other axis — one `run_id` opens one repricing run **and** one bulk import
/// alike, because the two flows were sharing one namespace and a caller who had
/// spent a key on an import could not open a repricing run under it. That was
/// proved on `SQLite` (`sqlite_repricing_journal_repo`) and over HTTP
/// (`rest_repricing_runs`) and on Postgres by nothing, which is the gap
/// `uq_pricing_price_scope_key_current`'s own principle names: "a measurement on one engine is not a
/// fact about the other".
///
/// Both directions, because the index has to admit one and refuse the other:
/// varying the kind under one key must land, and repeating the kind must not.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_client_key_opens_one_run_per_kind_and_not_across_kinds() {
    let conn = applied().await;
    must_succeed(&conn, &seed("import", "validating")).await;

    // The admission: the same tenant, the same client key, the other kind.
    must_succeed(
        &conn,
        &seed_as(OTHER_OP, TENANT, "repricing", "validating", "ck-1"),
    )
    .await;

    // And the refusal is still there per kind, which is what stops the admission
    // above from being "the index was dropped".
    must_be_rejected(
        &conn,
        &seed_as(THIRD_OP, TENANT, "repricing", "validating", "ck-1"),
        "uq_pricing_bulk_operation_client_key",
    )
    .await;
}

/// Identity and provenance are frozen; `DELETE` is refused in every state.
///
/// `request_hash` is asserted here as well as on the `SQLite` twin because the two
/// engines carry the arm in **different objects** — Postgres inside
/// `bss.pricing_bulk_operation_transitions()`, `SQLite` as a trigger of its own —
/// so `pricing_bulk_operation`'s two texts are restated per engine and a slip in either is invisible to the
/// other's suite.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_run_is_frozen_and_undeletable() {
    let conn = applied().await;
    must_succeed(&conn, &seed("import", "validating")).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_bulk_operation SET kind = 'repricing' WHERE operation_id = '{OP}'"
        ),
        "is frozen",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_bulk_operation SET request_hash = '\\x00'::bytea \
             WHERE operation_id = '{OP}'"
        ),
        "is frozen",
    )
    .await;
    // The positive control: `report` is the column of this group a caller may still
    // write, and the same shape of statement lands on it. Without it a function
    // refusing every UPDATE would satisfy both refusals above.
    must_succeed(
        &conn,
        &format!(
            "UPDATE bss.pricing_bulk_operation SET report = '{{\"rows\":[]}}'::jsonb \
             WHERE operation_id = '{OP}'"
        ),
    )
    .await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM bss.pricing_bulk_operation WHERE operation_id = '{OP}'"),
        "not permitted",
    )
    .await;
}

/// **The frozen-column arm, derived from the engine rather than hand-picked.**
///
/// The case above executes two of the seven columns the arm names, and this
/// file's own module doc says it exists because dropping a disjunct from a
/// PL/pgSQL edge list kept every gate green. The frozen arm has the identical
/// exposure: `sqlite_append_only` censuses the mirror's trigger body and nothing
/// censused the engine that ships, while `frozen_columns`'s own doc names five
/// prior waves of this defect. A column added to the table without a guard line
/// is now owed one by derivation.
///
/// `state`, `report` and `completed_at` are the sanctioned mutable three, and the
/// exception's own sentence names exactly those.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_column_of_a_run_is_named_by_the_frozen_arm_or_sanctioned_mutable() {
    let conn = applied().await;
    let census = pg_support::frozen_columns(
        &conn,
        "pricing_bulk_operation",
        "pricing_bulk_operation_transitions",
        "IF NEW.operation_id",
        &["state", "report", "completed_at"],
    )
    .await;

    assert!(
        !census.owed.is_empty(),
        "the census read no columns at all, which is the shape a mistyped table \
         name leaves -- it would report every guard complete"
    );
    assert!(
        census.missing().is_empty(),
        "these columns are on bss.pricing_bulk_operation and named by neither the \
         frozen arm nor the sanctioned-mutable list, so an ad-hoc UPDATE moves a \
         submitted run's identity or provenance: {:?}",
        census.missing()
    );
}
