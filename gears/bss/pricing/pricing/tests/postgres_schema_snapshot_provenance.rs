//! Slice 11's frozen `migrated-origin` record **on the engine that runs in
//! production** (`design/11-lifecycle.md` §6, `inst-sy-provenance`,
//! `inst-sy-freeze`, `inst-sy-select`, `inst-sy-payload`, D-76, D-81, D-87,
//! D-102).
//!
//! # The largest hole the rule census found, and why the writer that exists did
//! not close it
//!
//! `synthesis_repo::freeze_or_load` writes this table in production and
//! `rest_migrated_origin_snapshots` drives that path end to end — and neither can
//! see a rule here stop refusing. A repository only ever offers **legal** values:
//! `NewProvenance` carries a typed trigger, a resolved set synthesis built itself
//! and a payload it materialized, so every statement it emits satisfies all four
//! `CHECK`s by construction. Driving the repo catches a constraint that got
//! *narrower*; it is blind to one that was dropped. Until this pair the table
//! appeared in the whole test tree only in the two migration censuses, which pin
//! names and digests and run no statement.
//!
//! `sqlite_snapshot_provenance_store` proves the same roster on the mirror, and
//! that is not the same thing: the frozen guard is **one PL/pgSQL function with
//! two arms** here against two `RAISE(ABORT, …)` triggers there, and only the
//! `SQLite` side carries a trigger-**body** digest census — so a lost arm on the
//! shipping engine was invisible to every gate. That is the standing half of the
//! debt D-260 records, closed for this table.
//!
//! # What only this arm can prove, and what only the mirror can
//!
//! Postgres names the constraint in the error, so every case below asserts by
//! **name** — `uq_pricing_snapshot_provenance_subscription` and
//! `pricing_snapshot_provenance_pkey` are distinguishable here where the mirror
//! can only read back which columns it listed.
//!
//! The mirror's `json_valid` conjunct has no counterpart here and its absence is
//! the point: `resolved` and `payload` are `jsonb`, so the **type** refuses
//! malformed input one layer below any constraint. The case that drives it below
//! asserts the type's own refusal, so the two files read as one roster rather
//! than one of them appearing to be missing a rule.
//!
//! # Every `CHECK` here is reachable only on `INSERT`
//!
//! Stated rather than left to be rediscovered: the frozen function raises on
//! **every** `UPDATE`, before any constraint is evaluated, so no statement can
//! move a row into an illegal shape and be answered by a `CHECK`. The absence of
//! update-into-illegal cases below is structural.
//!
//! Run with:
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_snapshot_provenance -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const OTHER_TENANT: &str = "22222222-2222-2222-2222-222222222222";
const SUBSCRIPTION: &str = "33333333-3333-3333-3333-333333333333";
const OTHER_SUBSCRIPTION: &str = "44444444-4444-4444-4444-444444444444";
const PLAN: &str = "55555555-5555-5555-5555-555555555555";
const ACTOR: &str = "66666666-6666-6666-6666-666666666666";
const PROVENANCE: &str = "77777777-7777-7777-7777-777777777777";
const OTHER_PROVENANCE: &str = "88888888-8888-8888-8888-888888888888";

/// D-81's instant `t`, deliberately **later** than the commit instant below.
///
/// The obvious rule — an instant frozen at execution cannot be in the future —
/// is not written in the schema and this fixture is what pins that: D-81 makes
/// `t` the *migration effective timestamp* for the `migration` trigger, and a
/// migration is synthesized in the run-up to a date that has not arrived. A
/// `CHECK` added between these two columns would refuse the ordinary case of the
/// more common trigger, and this suite would redden the day it appeared.
const SNAPSHOT_INSTANT: &str = "2026-11-05T00:00:00+00:00";
const CREATED_AT: &str = "2026-08-07T10:00:00+00:00";

/// One resolved row in `inst-sy-select`'s shape: the id and the tier it came
/// from (D-76). The tier is what tells an auditor a real published price from a
/// governed backdated reconstruction without re-running the lookup.
const RESOLVED: &str =
    r#"[{"priceId":"99999999-9999-9999-9999-999999999999","source":"live_history"}]"#;

/// `inst-sy-payload`'s materialized content, reduced to the one field that makes
/// it an object: what the shape `CHECK` states is that it **is** one.
const PAYLOAD: &str = r#"{"model_kind":"flat"}"#;

/// The columns a freeze carries, as SQL literals. Every case below is this row
/// with exactly **one** field moved — which is what makes each refusal the
/// constraint it names rather than something else about the statement, and what
/// makes each case armed: the base row lands, so only a constraint mentioning
/// the moved field can be what answers.
#[derive(Clone)]
struct Freeze {
    provenance: &'static str,
    tenant: &'static str,
    subscription: &'static str,
    /// `NULL` or an integer — a bare SQL literal, because tier 2's absent
    /// revision is a value this suite has to be able to write.
    revision: &'static str,
    trigger: &'static str,
    resolved: &'static str,
    payload: &'static str,
}

/// The row `synthesis_repo` would write: tier 1, `migration` trigger, one
/// resolved row, an object payload.
fn freeze() -> Freeze {
    Freeze {
        provenance: PROVENANCE,
        tenant: TENANT,
        subscription: SUBSCRIPTION,
        revision: "3",
        trigger: "migration",
        resolved: RESOLVED,
        payload: PAYLOAD,
    }
}

fn insert(row: &Freeze) -> String {
    let Freeze {
        provenance,
        tenant,
        subscription,
        revision,
        trigger,
        resolved,
        payload,
    } = row;
    format!(
        "INSERT INTO bss.pricing_snapshot_provenance \
         (provenance_id, tenant_id, subscription_ref, source_plan_id, source_revision, \
          snapshot_instant, trigger_kind, acting_principal, resolved, payload, created_at) \
         VALUES ('{provenance}', '{tenant}', '{subscription}', '{PLAN}', {revision}, \
          '{SNAPSHOT_INSTANT}', '{trigger}', '{ACTOR}', '{resolved}', '{payload}', \
          '{CREATED_AT}')"
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

/// How many records the tenant holds for the fixture's subscription.
///
/// Load-bearing rather than decorative: a `BEFORE` trigger returning instead of
/// raising cancels a statement **silently**, and a case asserting only "the
/// statement did not fail" cannot tell that apart from one that took effect.
async fn frozen_records(conn: &DatabaseConnection) -> i64 {
    conn.query_one_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!(
            "SELECT count(*) AS n FROM bss.pricing_snapshot_provenance \
             WHERE tenant_id = '{TENANT}' AND subscription_ref = '{SUBSCRIPTION}'"
        ),
    ))
    .await
    .expect("run the count")
    .expect("the count returns a row")
    .try_get::<i64>("", "n")
    .expect("read the count")
}

/// The whitelist half: the row `inst-sy-freeze` actually writes lands, and lands
/// whole. Every refusal below departs from exactly this statement, so without
/// this case a fixture that had drifted into being illegal for some *other*
/// reason would leave every case here green while proving nothing.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_synthesized_snapshot_freezes_and_reads_back_whole() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&freeze())).await;
    assert_eq!(frozen_records(&conn).await, 1);
}

/// **The stored trigger vocabulary is D-81's two, and the underscored spelling is
/// the stored one.**
///
/// `inst-sy-freeze` writes the pair hyphenated (`first-rating`) because that is
/// prose; the column stores `first_rating`. A writer that passed the design set's
/// spelling straight through is exactly what this refuses. A *third* trigger — a
/// re-synthesis, a correction — is the case the constraint exists for: D-81 gives
/// each trigger a different instant `t`, so a third one would freeze a third
/// price with no rule saying which one rating reads.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_trigger_is_one_of_d81s_two() {
    let conn = applied().await;
    for trigger in ["first-rating", "correction", ""] {
        must_be_rejected(
            &conn,
            &insert(&Freeze {
                trigger,
                ..freeze()
            }),
            "chk_pricing_snapshot_provenance_trigger",
        )
        .await;
    }

    // Both sanctioned values land, on their own subscriptions -- the whitelist
    // half, without which a constraint narrowed to one trigger would pass.
    must_succeed(&conn, &insert(&freeze())).await;
    must_succeed(
        &conn,
        &insert(&Freeze {
            provenance: OTHER_PROVENANCE,
            subscription: OTHER_SUBSCRIPTION,
            trigger: "first_rating",
            ..freeze()
        }),
    )
    .await;
}

/// A revision is an ordinal, and its **absence** is tier 2 rather than a defect.
///
/// D-87 states the case plainly: a fully-legacy key may have no plan revision at
/// all, so the column admits `NULL` and the payload is what makes the row
/// evaluable. A constraint tightened to `NOT NULL` would refuse every tier-2
/// synthesis, which is the half of D-76 that has no other store — so the `NULL`
/// insert below is as load-bearing as the refusal above it.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_revision_is_an_ordinal_or_tier_twos_absence() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            revision: "-1",
            ..freeze()
        }),
        "chk_pricing_snapshot_provenance_revision",
    )
    .await;

    must_succeed(
        &conn,
        &insert(&Freeze {
            revision: "NULL",
            ..freeze()
        }),
    )
    .await;
    // 0 is the first revision a plan mints, so the boundary is admitted and not
    // an off-by-one away from being refused.
    must_succeed(
        &conn,
        &insert(&Freeze {
            provenance: OTHER_PROVENANCE,
            subscription: OTHER_SUBSCRIPTION,
            revision: "0",
            ..freeze()
        }),
    )
    .await;
}

/// **`inst-sy-select` clause (3) is fail-closed, and an empty resolved set is
/// that refusal having been ignored.**
///
/// Synthesis never guesses a price: when neither tier resolves a row it fails
/// into the migration exception list. A row frozen with nothing resolved is a
/// snapshot rating would charge from and find empty.
///
/// **The two conjuncts are not separately reachable, and that is worth stating
/// rather than rediscovering.** Both statements below are armed against the
/// constraint — dropped, they land — but neither isolates a conjunct: with
/// `jsonb_typeof(resolved) = 'array'` removed, the object case does not become
/// legal, it becomes the bare runtime error `cannot get array length of a
/// non-array`. The mirror has the same shape for the opposite reason, where
/// `json_array_length` answers 0 for every non-array.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_frozen_snapshot_resolved_at_least_one_row() {
    let conn = applied().await;
    // Valid JSON of the wrong shape: an object where §6 keeps a list of resolved
    // ids, which is what a writer serializing one row instead of a set produces.
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            resolved: r#"{"priceId":"99999999-9999-9999-9999-999999999999"}"#,
            ..freeze()
        }),
        "chk_pricing_snapshot_provenance_resolved",
    )
    .await;
    // The array, empty.
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            resolved: "[]",
            ..freeze()
        }),
        "chk_pricing_snapshot_provenance_resolved",
    )
    .await;
}

/// The payload is an **object** — `inst-sy-payload`'s materialized row content,
/// which Rating reads field by field. An array or a bare scalar is a payload
/// nothing can evaluate, and this table is the only place that content lives:
/// there is no `CatalogVersion` behind a `migrated-origin` ref to re-resolve it
/// from.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_payload_is_an_object() {
    let conn = applied().await;
    for payload in ["[]", r#"["flat"]"#, "7"] {
        must_be_rejected(
            &conn,
            &insert(&Freeze {
                payload,
                ..freeze()
            }),
            "chk_pricing_snapshot_provenance_payload",
        )
        .await;
    }
}

/// **The `jsonb` column is the first guard, and it is what stands in for the
/// mirror's `json_valid` conjunct.**
///
/// Pinned so the two arms of this pair read as one roster: on the mirror these
/// columns are `text` and `json_valid` is the constraint that refuses text that
/// is not JSON at all; here the type refuses it before any constraint runs, so
/// the shape `CHECK`s never see it. A migration that relaxed either column to
/// `text` to match the mirror would lose the guard entirely — the shape `CHECK`s
/// could not even be declared against it — and this case is what would say so.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_column_type_refuses_what_is_not_json_at_all() {
    let conn = applied().await;
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            resolved: "not json",
            ..freeze()
        }),
        "invalid input syntax for type json",
    )
    .await;
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            payload: "not json",
            ..freeze()
        }),
        "invalid input syntax for type json",
    )
    .await;
}

/// **§9's idempotency, as an index rather than as a convention: one subscription,
/// one frozen snapshot, ever.**
///
/// The second insert carries a *different* trigger, which is the drift the index
/// is aimed at: keyed on `(subscription_ref, trigger)` instead, the `migration`
/// and `first-rating` triggers would each freeze their own — and D-81 gives those
/// two different instants `t`, so the subscription would hold two different
/// frozen prices with no rule saying which one rating reads.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_subscription_holds_one_frozen_snapshot_ever() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&freeze())).await;
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            provenance: OTHER_PROVENANCE,
            trigger: "first_rating",
            ..freeze()
        }),
        "uq_pricing_snapshot_provenance_subscription",
    )
    .await;

    // The index is tenant-scoped, like every read of this table. Stated because
    // the assertion above would also pass against an index over
    // `subscription_ref` alone, and that index would be a different rule.
    must_succeed(
        &conn,
        &insert(&Freeze {
            provenance: OTHER_PROVENANCE,
            tenant: OTHER_TENANT,
            ..freeze()
        }),
    )
    .await;
}

/// The primary key, driven by a **different** subscription so the idempotency
/// index above cannot be what answers — the two are distinguishable here by name,
/// which is the sharper half of this pair. Two records sharing a `provenance_id`
/// would give one audit reference two payloads.
#[tokio::test]
#[ignore = "requires Docker"]
async fn two_records_cannot_share_a_provenance_id() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&freeze())).await;
    must_be_rejected(
        &conn,
        &insert(&Freeze {
            subscription: OTHER_SUBSCRIPTION,
            ..freeze()
        }),
        "pricing_snapshot_provenance_pkey",
    )
    .await;
}

/// **The snapshot is frozen, and `UPDATE` is refused whatever it touches.**
///
/// A `migrated-origin` ref resolves through **no** `CatalogVersion` by
/// construction (D-87, Foundation §4.4 names it the one deliberately
/// non-version-pinned reference), so the immutability every other consumer
/// contract gets from a frozen `CatalogVersion` has to come from this row. If it
/// could be edited, a disputed legacy charge could be re-explained after the fact
/// by the party being disputed with.
///
/// Driven column by column rather than once: the arm is unconditional today, and
/// a frozen-column whitelist narrowing it to the payload — the shape every other
/// guarded table in this chain has — would leave the instant, the trigger and the
/// resolved set editable with a single-statement case still green.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_frozen_snapshot_is_never_edited() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&freeze())).await;
    for assignment in [
        r#"payload = '{"model_kind":"tiered"}'"#,
        r#"resolved = '[{"priceId":"99999999-9999-9999-9999-999999999999","source":"historical_import"}]'"#,
        "snapshot_instant = '2020-01-01T00:00:00+00:00'",
        "trigger_kind = 'first_rating'",
        "source_revision = 4",
        "source_plan_id = '12121212-1212-1212-1212-121212121212'",
        "acting_principal = '13131313-1313-1313-1313-131313131313'",
        "created_at = '2020-01-01T00:00:00+00:00'",
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_snapshot_provenance SET {assignment} \
                 WHERE provenance_id = '{PROVENANCE}'"
            ),
            "is frozen",
        )
        .await;
    }
}

/// The interpolated subject is the **subscription**, not the provenance id, and
/// that is what an operator reading the log needs: the record is looked up by
/// subscription because that is all a consumer of a `migrated-origin` ref holds.
///
/// Its own case because the message is the only place this engine's arm differs
/// observably from the mirror's — `SQLite` has no message interpolation, so the
/// twin cannot assert it at all.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_refusal_names_the_subscription_it_is_about() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&freeze())).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_snapshot_provenance SET source_revision = 4 \
             WHERE provenance_id = '{PROVENANCE}'"
        ),
        SUBSCRIPTION,
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_snapshot_provenance WHERE provenance_id = '{PROVENANCE}'"
        ),
        SUBSCRIPTION,
    )
    .await;
}

/// `DELETE` is refused for the same reason one step further: an auditor
/// reconstructing a legacy charge needs the record to still exist, and a
/// subscription's snapshot outlives the migration that synthesized it.
///
/// The count is read back because the arm is a `BEFORE` trigger: one that
/// returned rather than raising would cancel the statement **silently**, and a
/// case asserting only a refusal cannot tell that from one that swept the row
/// away.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_frozen_snapshot_is_never_deleted() {
    let conn = applied().await;
    must_succeed(&conn, &insert(&freeze())).await;
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_snapshot_provenance WHERE provenance_id = '{PROVENANCE}'"
        ),
        "is not permitted",
    )
    .await;
    assert_eq!(
        frozen_records(&conn).await,
        1,
        "the record must still be there for the auditor who comes looking"
    );
}
