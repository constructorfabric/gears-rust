#![cfg(feature = "integration")]
// Created: 2026-07-26 by Constructor Tech
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
//! PostgreSQL concurrency harness for the resource-group DB-behavior audit.
//! Only real Postgres gives genuine SSI conflicts and FK `RESTRICT` under
//! concurrent writers; SQLite's own "SERIALIZABLE" is a whole-database lock.
//!
//! `cargo nextest` runs each `#[tokio::test]` in its own process, so the ten
//! scenarios below are grouped into two tests, [`membership_and_type_races`]
//! and [`hierarchy_move_races`], each sharing one `testcontainers` Postgres.
//!
//! [`PgFixture`] is owned by the test, not a `static`, so its `Drop` reliably
//! removes the container. A grouped test skips itself (stderr message) when
//! Docker isn't reachable; CI sets `RG_PG_REQUIRE_DOCKER=1` to fail instead.
//!
//! Requires the `integration` feature. Run via `make test-rg-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-resource-group --features integration --test pg_concurrency_test
//! ```
//!
//! ## Scenarios
//!
//! `membership_and_type_races` covers the RG-01/RG-02/RG-03 fixes plus two
//! negative controls proving `transaction_with_retry` + SERIALIZABLE
//! protects an invariant, not merely fails to break one.
//!
//! `hierarchy_move_races` exercises `move_group`'s closure-rebuild machinery
//! under concurrent, *overlapping* hierarchy writes -- moves, creates, and
//! force-deletes that all touch the same ancestor chain at once.
//!
//! Full rationale for each finding: `../../docs/db-behavior-audit.md`.
//!
//! Every scenario also runs [`assert_hierarchy_invariants`] (and, for
//! memberships, [`assert_membership_tenant_invariant`]) to confirm the data,
//! not just the call outcome, stayed consistent.
//!
//! Each race is timed against [`max_winning_tx_duration`], a coarse guard
//! against an N+1 regression turning a transaction pathologically slow.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use resource_group::domain::error::DomainError;
use resource_group::domain::group_service::QueryProfile;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::entity::resource_group::{
    Column as RgColumn, Entity as RgEntity,
};
use resource_group::infra::storage::entity::resource_group_closure::Entity as ClosureEntity;
use resource_group::infra::storage::entity::resource_group_membership::Entity as MbrEntity;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::CreateGroupRequest;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;
use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Barrier;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

/// Ceiling on a winning transaction's duration. Deliberately far above any
/// healthy run (the whole suite takes seconds): it exists to catch an N+1
/// regression turning a handful of statements into thousands, and a tighter
/// budget would just flake on a loaded CI runner or a cold container.
/// Override with `RG_PG_TX_BUDGET_SECS` when characterizing something.
fn max_winning_tx_duration() -> Duration {
    std::env::var("RG_PG_TX_BUDGET_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(Duration::from_mins(1), Duration::from_secs)
}

/// Recomputes the transitive closure of `resource_group.parent_id` (also
/// checking for cycles) and compares it against `resource_group_closure`: a
/// concurrent move can diverge the two even when every call looks correct.
async fn assert_hierarchy_invariants(db: &Arc<DBProvider<DbError>>) {
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();

    let groups = RgEntity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query resource_group table");
    let parent_of: HashMap<Uuid, Option<Uuid>> =
        groups.iter().map(|g| (g.id, g.parent_id)).collect();

    // Cycle check: walking up from any node must terminate within
    // parent_of.len() hops.
    for &id in parent_of.keys() {
        let mut current = id;
        let mut hops = 0usize;
        while let Some(parent) = parent_of.get(&current).copied().flatten() {
            hops += 1;
            assert!(
                hops <= parent_of.len(),
                "cycle detected in resource_group.parent_id starting at {id}"
            );
            current = parent;
        }
    }

    // Expected closure: self-row (depth 0) plus one row per ancestor at the
    // correct depth, derived purely from parent_id.
    let mut expected: HashSet<(Uuid, Uuid, i32)> = HashSet::new();
    for &id in parent_of.keys() {
        expected.insert((id, id, 0));
        let mut current = id;
        let mut depth = 0i32;
        while let Some(parent) = parent_of.get(&current).copied().flatten() {
            depth += 1;
            expected.insert((parent, id, depth));
            current = parent;
        }
    }

    let closure_rows = ClosureEntity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query resource_group_closure table");
    let actual: HashSet<(Uuid, Uuid, i32)> = closure_rows
        .iter()
        .map(|r| (r.ancestor_id, r.descendant_id, r.depth))
        .collect();

    let missing: Vec<_> = expected.difference(&actual).collect();
    let extra: Vec<_> = actual.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "closure table diverged from the transitive closure of parent_id: \
         missing={missing:?} extra={extra:?}"
    );
}

/// Checks that no `(resource_type, resource_id)` is linked from groups in
/// more than one tenant, across every membership row in the database (not
/// just the resource a given test created).
async fn assert_membership_tenant_invariant(db: &Arc<DBProvider<DbError>>) {
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();

    let memberships = MbrEntity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query resource_group_membership table");
    let groups = RgEntity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query resource_group table");
    let tenant_of: HashMap<Uuid, Uuid> = groups.iter().map(|g| (g.id, g.tenant_id)).collect();

    let mut by_resource: HashMap<(i16, String), HashSet<Uuid>> = HashMap::new();
    for m in &memberships {
        if let Some(&tenant) = tenant_of.get(&m.group_id) {
            by_resource
                .entry((m.gts_type_id, m.resource_id.clone()))
                .or_default()
                .insert(tenant);
        }
    }
    let violations: Vec<_> = by_resource
        .iter()
        .filter(|(_, tenants)| tenants.len() > 1)
        .collect();
    assert!(
        violations.is_empty(),
        "resource(s) linked from groups in more than one tenant: {violations:?}"
    );
}

/// Times `f`, asserting the result stays under [`max_winning_tx_duration`] --
/// a pathology guard, not a precision benchmark.
async fn timed<T>(label: &str, f: impl std::future::Future<Output = T>) -> T {
    let start = Instant::now();
    let result = f.await;
    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let budget = max_winning_tx_duration();
    let budget_secs = budget.as_secs_f64();
    assert!(
        elapsed <= budget,
        "{label} took {elapsed_secs:.3}s, exceeding the {budget_secs:.3}s transaction-window \
         budget -- likely an N+1 regression, not normal variance"
    );
    eprintln!("{label}: {elapsed_secs:.3}s");
    result
}

/// A PostgreSQL container plus a `DBProvider` connected to it, both owned by
/// whichever test created them.
///
/// Owned rather than parked in a `static`, which never runs its destructor
/// at process exit and would leak the container every run. `Drop` here
/// removes it on scope exit and on panic (`panic = "unwind"` in this crate).
///
/// `ContainerAsync`'s `Drop` reaches the daemon via `block_in_place`, which
/// needs a multi-threaded runtime -- hence `flavor = "multi_thread"` on both
/// `#[tokio::test]`s using this fixture.
struct PgFixture {
    _container: testcontainers::ContainerAsync<Postgres>,
    db: Arc<DBProvider<DbError>>,
}

impl PgFixture {
    fn db(&self) -> &Arc<DBProvider<DbError>> {
        &self.db
    }
}

/// Whether an error is a bounded-retry budget exhaustion rather than a defect.
///
/// `transaction_with_retry` gives three immediate attempts with no backoff, so
/// two writers contending on overlapping closure rows can legitimately spend it
/// and surface a serialization failure. That is a fact about the retry budget,
/// not a broken invariant -- the data-level guarantee is
/// `assert_hierarchy_invariants`, which stays hard either way.
fn is_contention_error(err: &DomainError) -> bool {
    let text = format!("{err:?}").to_ascii_lowercase();
    text.contains("could not serialize") || text.contains("40001") || text.contains("deadlock")
}

/// Whether a missing or broken Docker must fail the run rather than skip it.
/// CI sets `RG_PG_REQUIRE_DOCKER=1`; locally it is unset and skipping is fine.
fn require_docker() -> bool {
    std::env::var_os("RG_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

/// Bring up a `testcontainers` PostgreSQL and run RG's migrations against it,
/// mirroring account-management's `tests/common/mod.rs::pg::bring_up_postgres()`.
///
/// Returns `None` if Docker isn't reachable; callers treat that as a
/// graceful skip. Skipping is right locally, but wrong in CI, where a broken
/// daemon would otherwise pass vacuously -- `RG_PG_REQUIRE_DOCKER=1` panics.
async fn pg_fixture() -> Option<PgFixture> {
    // testcontainers-modules' Postgres image defaults to "11-alpine", which
    // predates gen_random_uuid() becoming a built-in (PG13+) -- RG's migrations
    // use it, so pin a modern tag.
    let request = ContainerRequest::from(Postgres::default())
        .with_tag("16-alpine")
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");

    let container = match request.start().await {
        Ok(container) => container,
        Err(e) => {
            let msg = format!(
                "PostgreSQL concurrency tests: could not start a PostgreSQL container via \
                 testcontainers ({e}). Install/start Docker to run these for real -- see \
                 this file's module docs."
            );
            assert!(!require_docker(), "{msg}");
            eprintln!("skipping -- {msg}");
            return None;
        }
    };
    let port = match container.get_host_port_ipv4(5432).await {
        Ok(port) => port,
        Err(e) => {
            let msg = format!(
                "PostgreSQL concurrency tests: container started but its port could not be \
                 resolved ({e}). Is Docker healthy?"
            );
            assert!(!require_docker(), "{msg}");
            eprintln!("skipping -- {msg}");
            return None;
        }
    };

    let opts = ConnectOpts {
        max_conns: Some(10),
        min_conns: Some(2),
        ..Default::default()
    };
    let db = connect_db(&format!("postgres://user:pass@127.0.0.1:{port}/app"), opts)
        .await
        .expect("connect to the testcontainers PostgreSQL");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run resource-group migrations against PostgreSQL");

    Some(PgFixture {
        _container: container,
        db: Arc::new(DBProvider::new(db)),
    })
}

/// Bring up the fixture, or return from the test if Docker isn't available.
///
/// Binds an owned [`PgFixture`]; keep it alive for the whole test body,
/// since dropping it removes the container. No cross-test lock is needed:
/// each grouped test runs its five scenarios sequentially, alone in its container.
macro_rules! pg_fixture_or_skip {
    () => {
        match pg_fixture().await {
            Some(fixture) => fixture,
            None => return,
        }
    };
}

/// Distinct `tenant_id`s of the groups that *actually hold a membership row*
/// for `(resource_type, resource_id)`, resolved via the membership rows
/// themselves rather than any candidate groups' own tenant.
async fn distinct_tenant_ids_for_resource(
    db: &Arc<DBProvider<DbError>>,
    resource_type: &str,
    resource_id: &str,
) -> Vec<Uuid> {
    use resource_group::infra::storage::entity::resource_group_membership::{
        Column as MbrColumn, Entity as MbrEntity,
    };

    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();

    let gts_type_id = TypeRepository::resolve_id(&conn, resource_type)
        .await
        .expect("resolve resource_type")
        .expect("resource_type must be a registered GTS type");

    let membership_rows = MbrEntity::find()
        .filter(MbrColumn::GtsTypeId.eq(gts_type_id))
        .filter(MbrColumn::ResourceId.eq(resource_id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query resource_group_membership table");
    let holder_group_ids: Vec<Uuid> = membership_rows.iter().map(|m| m.group_id).collect();
    if holder_group_ids.is_empty() {
        return Vec::new();
    }

    let rows = RgEntity::find()
        .filter(RgColumn::Id.is_in(holder_group_ids))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query resource_group table");
    let mut ids: Vec<Uuid> = rows.into_iter().map(|r| r.tenant_id).collect();
    ids.sort();
    ids.dedup();
    ids
}

/// `Display`-based diagnostic summary of an `add_membership` outcome, for
/// `eprintln!` (avoids `clippy::use_debug`).
fn describe_membership_result(
    r: &Result<resource_group_sdk::models::ResourceGroupMembership, DomainError>,
) -> String {
    describe_result(r)
}

/// `Display`-based diagnostic summary of any `Result<_, DomainError>`, for
/// `eprintln!` (avoids `clippy::use_debug`); only names the `Ok` case since
/// the `Ok` payload types here don't all implement `Debug`/`Display`.
fn describe_result<T>(r: &Result<T, DomainError>) -> String {
    match r {
        Ok(_) => "Ok".to_owned(),
        Err(e) => format!("Err({e})"),
    }
}

// RG-01: membership first-write race

/// Two concurrent `add_membership` calls for the same resource in different
/// tenants: exactly one must succeed, the loser getting a clean
/// `TenantIncompatibility`, not a raw serialization failure (RG-01).
async fn membership_first_write_race_exactly_one_tenant_wins(db: &Arc<DBProvider<DbError>>) {
    // Repeated trials: the invariant must hold under real concurrent load,
    // not just one sample.
    const TRIALS: usize = 8;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());

    let member_type = common::create_root_type(&type_svc, "pgmbrres").await;
    let grp_type = {
        // Local copy of membership_service_test.rs's helper -- creates a
        // root type whose allowed_membership_types includes member_type.
        let code = format!(
            "{}x.test.pgmbrgrp.i{}.v1~",
            toolkit_gts::gts_id!("cf.core.rg.type.v1~"),
            Uuid::now_v7().as_simple()
        );
        type_svc
            .create_type(resource_group_sdk::CreateTypeRequest {
                code,
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![member_type.code.clone()],
                metadata_schema: None,
            })
            .await
            .expect("create membership-holding type")
    };

    let mut both_succeeded = 0usize;
    let mut correctly_rejected = 0usize; // one Ok + one Err(TenantIncompatibility)
    let mut unexpected = 0usize;

    for _ in 0..TRIALS {
        let tenant_a = Uuid::now_v7();
        let tenant_b = Uuid::now_v7();
        let ctx_a = common::make_ctx(tenant_a);
        let ctx_b = common::make_ctx(tenant_b);
        let group_a =
            common::create_root_group(&group_svc, &ctx_a, &grp_type.code, "A", tenant_a).await;
        let group_b =
            common::create_root_group(&group_svc, &ctx_b, &grp_type.code, "B", tenant_b).await;

        let resource_id = format!("shared-{}", Uuid::now_v7().as_simple());
        let barrier = Arc::new(Barrier::new(2));

        // Services can't derive Clone (repo type params like TypeRepository
        // aren't Clone), so build a fresh instance per task instead of cloning.
        let (svc1, svc2) = (
            common::make_membership_service(db.clone()),
            common::make_membership_service(db.clone()),
        );
        let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
        let (rt1, rt2) = (member_type.code.clone(), member_type.code.clone());
        let (rid1, rid2) = (resource_id.clone(), resource_id.clone());

        let t1 = tokio::spawn(async move {
            b1.wait().await;
            svc1.add_membership(&ctx_a, group_a.id, &rt1, &rid1).await
        });
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            svc2.add_membership(&ctx_b, group_b.id, &rt2, &rid2).await
        });

        let (r1, r2) = timed("membership_first_write_race trial", async {
            tokio::join!(t1, t2)
        })
        .await;
        let r1 = r1.expect("task 1 join");
        let r2 = r2.expect("task 2 join");

        let tenant_ids =
            distinct_tenant_ids_for_resource(db, &member_type.code, &resource_id).await;
        match (&r1, &r2, tenant_ids.len()) {
            (Ok(_), Ok(_), _) => both_succeeded += 1,
            (Ok(_), Err(e), 1) | (Err(e), Ok(_), 1) => {
                correctly_rejected += 1;
                assert!(
                    matches!(e, DomainError::TenantIncompatibility { .. }),
                    "the losing add_membership must get a clean TenantIncompatibility \
                     (proving transaction_with_retry + the RG-15 fix absorbed the SSI \
                     conflict), got: {e}"
                );
            }
            _ => {
                unexpected += 1;
                let tenants = tenant_ids
                    .iter()
                    .map(Uuid::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                eprintln!(
                    "membership_first_write_race: unexpected outcome r1={} r2={} \
                     distinct_tenants=[{tenants}]",
                    describe_membership_result(&r1),
                    describe_membership_result(&r2),
                );
            }
        }
    }

    eprintln!(
        "membership_first_write_race: both_succeeded={both_succeeded} \
         correctly_rejected={correctly_rejected} unexpected={unexpected} (out of {TRIALS} trials)"
    );
    assert_eq!(
        unexpected, 0,
        "every trial must be either both-succeed (RG-01 regression) or the correct outcome \
         (one rejected with a clean TenantIncompatibility) -- got {unexpected} trials with a \
         genuinely unexpected shape"
    );
    assert_eq!(
        both_succeeded, 0,
        "RG-01 regression: {both_succeeded}/{TRIALS} trials let both tenants' first-membership \
         add succeed for the same resource"
    );
    assert_eq!(
        correctly_rejected, TRIALS,
        "expected every trial to resolve to exactly one tenant winning, got {correctly_rejected}/{TRIALS}"
    );

    // Confirm the data, not just the call outcomes, stayed consistent: every
    // resource still linked from exactly one tenant, and the closure table
    // still matches parent_id.
    assert_membership_tenant_invariant(db).await;
    assert_hierarchy_invariants(db).await;
}

// RG-02: delete_type races create_group of that type

/// `delete_type` and `create_group_unscoped` share a `SERIALIZABLE`
/// predicate, so the write-skew retries cleanly instead of surfacing a raw
/// error; FK `ON DELETE RESTRICT` backstops actual corruption regardless (RG-02).
async fn delete_type_races_create_group_resolves_cleanly(db: &Arc<DBProvider<DbError>>) {
    const TRIALS: usize = 15;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let tenant_id = Uuid::now_v7();

    let mut corruption = 0usize; // both succeeded -- would be a real invariant violation
    let mut delete_won = 0usize; // delete succeeded, create cleanly rejected (type gone)
    let mut create_won = 0usize; // create succeeded, delete cleanly rejected (conflict)
    let mut unexpected = 0usize; // anything else -- a raw error or an unrecognized shape

    for i in 0..TRIALS {
        let t = common::create_root_type(&type_svc, &format!("pgdelrace{i}")).await;
        let barrier = Arc::new(Barrier::new(2));
        let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
        // Fresh instances per task -- see the comment in
        // membership_first_write_race_exactly_one_tenant_wins about why
        // these services can't just be `.clone()`d.
        let type_svc1 = TypeService::new(db.clone(), Arc::new(TypeRepository));
        let group_svc1 = common::make_group_service(db.clone());
        let (code1, code2) = (t.code.clone(), t.code.clone());

        let delete_task = tokio::spawn(async move {
            b1.wait().await;
            type_svc1.delete_type(&code1).await
        });
        let create_task = tokio::spawn(async move {
            b2.wait().await;
            group_svc1
                .create_group_unscoped(
                    CreateGroupRequest {
                        id: None,
                        code: code2,
                        name: "race".to_owned(),
                        parent_id: None,
                        metadata: None,
                    },
                    tenant_id,
                )
                .await
        });

        let (delete_res, create_res) = timed("delete_type_races_create_group trial", async {
            tokio::join!(delete_task, create_task)
        })
        .await;
        let delete_res = delete_res.expect("delete task join");
        let create_res = create_res.expect("create task join");

        // Each trial uses a fresh, uniquely-coded type (no global invariant
        // like tenant-root is at stake), so there's nothing to clean up
        // between trials regardless of outcome.
        match (&delete_res, &create_res) {
            (Ok(()), Ok(_)) => corruption += 1,
            (Ok(()), Err(DomainError::TypeNotFound { .. })) => delete_won += 1,
            (Err(DomainError::ConflictActiveReferences { .. }), Ok(_)) => create_won += 1,
            _ => {
                unexpected += 1;
                eprintln!(
                    "delete_type_races_create_group: unexpected outcome delete={} create={}",
                    describe_result(&delete_res),
                    describe_result(&create_res),
                );
            }
        }

        // Whichever side won, the data must stay consistent (FK RESTRICT
        // backstops corruption regardless, but confirm the closure table
        // agrees with parent_id too, since create_group_unscoped writes it).
        assert_hierarchy_invariants(db).await;
    }

    assert_eq!(
        corruption, 0,
        "invariant violation: delete_type and create_group_unscoped both succeeded for the \
         same type in {corruption}/{TRIALS} trials -- FK RESTRICT should make this impossible"
    );
    assert_eq!(
        unexpected, 0,
        "RG-02 regression: {unexpected}/{TRIALS} trials produced a raw error or unrecognized \
         outcome shape instead of a clean domain error on the losing side"
    );
    assert_eq!(
        delete_won + create_won,
        TRIALS,
        "accounting mismatch: delete_won={delete_won} create_won={create_won} \
         unexpected={unexpected}"
    );
    eprintln!(
        "delete_type_races_create_group: delete_won={delete_won} create_won={create_won} \
         (out of {TRIALS} trials)"
    );
}

// RG-03: create_type conflict, now retried

/// Two concurrent `create_type` calls for the same code race inside their
/// own `SERIALIZABLE` transaction; `transaction_with_retry` turns the
/// loser's abort into a clean `TypeAlreadyExists`, not a raw failure.
async fn create_type_conflict_retried_yields_clean_already_exists_for_loser(
    db: &Arc<DBProvider<DbError>>,
) {
    let code = format!(
        "{}x.test.pgtyperace.i{}.v1~",
        toolkit_gts::gts_id!("cf.core.rg.type.v1~"),
        Uuid::now_v7().as_simple()
    );
    let barrier = Arc::new(Barrier::new(2));
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let svc1 = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let svc2 = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let (code1, code2) = (code.clone(), code.clone());

    let req = |code: String| resource_group_sdk::CreateTypeRequest {
        code,
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    };

    let t1 = tokio::spawn(async move {
        b1.wait().await;
        svc1.create_type(req(code1)).await
    });
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        svc2.create_type(req(code2)).await
    });

    let (r1, r2) = timed("create_type_conflict race", async { tokio::join!(t1, t2) }).await;
    let r1 = r1.expect("task 1 join");
    let r2 = r2.expect("task 2 join");

    let ok_count = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok_count, 1,
        "exactly one concurrent create_type for the same code must succeed: r1={r1:?} r2={r2:?}"
    );

    let loser = if r1.is_err() { &r1 } else { &r2 };
    let loser_err = loser.as_ref().unwrap_err();
    // Also exercises RG-15: is_retryable_contention must recognize
    // DbErr::Custom, or retry never fires here at all.
    eprintln!("create_type_conflict_retried: loser error = {loser_err}");
    assert!(
        matches!(loser_err, DomainError::TypeAlreadyExists { .. }),
        "the losing create_type must get a clean TypeAlreadyExists (proving \
         transaction_with_retry absorbed the SSI conflict), got: {loser_err:?}"
    );

    assert_hierarchy_invariants(db).await;
}

// Negative controls: SSI + retry works for hierarchy mutations

fn unique_tenant_type_code() -> String {
    format!(
        "{}pgrace{}.v1~",
        resource_group_sdk::TENANT_RG_TYPE_PATH,
        Uuid::now_v7().as_simple()
    )
}

/// Two concurrent tenant-root creates: exactly one succeeds, the other gets
/// a clean `TenantRootAlreadyExists` -- proves `transaction_with_retry` +
/// SERIALIZABLE protects the invariant under a real SSI conflict.
async fn negative_control_tenant_root_race_exactly_one_succeeds(db: &Arc<DBProvider<DbError>>) {
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());

    let tenant_type = type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: unique_tenant_type_code(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create tenant type");

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = common::make_ctx(tenant_a);
    let ctx_b = common::make_ctx(tenant_b);
    let barrier = Arc::new(Barrier::new(2));

    let svc1 = common::make_group_service(db.clone());
    let svc2 = common::make_group_service(db.clone());
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let (code1, code2) = (tenant_type.code.clone(), tenant_type.code.clone());

    let t1 = tokio::spawn(async move {
        b1.wait().await;
        svc1.create_group(
            &ctx_a,
            CreateGroupRequest {
                id: None,
                code: code1,
                name: "Tenant A".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
    });
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        svc2.create_group(
            &ctx_b,
            CreateGroupRequest {
                id: None,
                code: code2,
                name: "Tenant B".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_b,
        )
        .await
    });

    let (r1, r2) = timed("negative_control_tenant_root_race", async {
        tokio::join!(t1, t2)
    })
    .await;
    let r1 = r1.expect("task 1 join");
    let r2 = r2.expect("task 2 join");

    let ok_count = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok_count, 1,
        "exactly one concurrent tenant-root create must succeed: r1={r1:?} r2={r2:?}"
    );
    let loser = if r1.is_err() { &r1 } else { &r2 };
    let loser_err = loser.as_ref().unwrap_err();
    // RG-15: is_retryable_contention must recognize DbErr::Custom, since the
    // SSI abort here surfaces from an interior repo call, not just at COMMIT.
    assert!(
        matches!(loser_err, DomainError::TenantRootAlreadyExists { .. }),
        "the losing create must get a clean TenantRootAlreadyExists (proving \
         transaction_with_retry + the RG-15 fix absorbed the SSI conflict), got: {loser_err:?}"
    );

    assert_hierarchy_invariants(db).await;

    // Clean up so the harness can be re-run without a stale tenant-root
    // (the uniqueness check is scoped by TENANT_RG_TYPE_PATH prefix, global
    // across all tenants -- not by our per-test tenant ids).
    let winner_group = if let Ok(g) = &r1 {
        g
    } else {
        r2.as_ref().unwrap()
    };
    // A tenant-type root's effective tenant_id is its own group id, not
    // tenant_a/tenant_b -- scope the cleanup ctx accordingly or delete_group's
    // AuthZ scope won't see the row.
    let cleanup_ctx = common::make_ctx(winner_group.id);
    group_svc
        .delete_group(&cleanup_ctx, winner_group.id, true)
        .await
        .expect("cleanup: force delete tenant root");
}

/// Two concurrent creates under `max_width = 1`: exactly one must succeed,
/// the other must fail with the clean `LimitViolation` error -- never both.
async fn negative_control_width_limited_race_exactly_one_succeeds(db: &Arc<DBProvider<DbError>>) {
    fn width_limited_group_service(
        db: Arc<DBProvider<DbError>>,
    ) -> resource_group::domain::group_service::GroupService<
        resource_group::infra::storage::group_repo::GroupRepository,
        TypeRepository,
    > {
        resource_group::domain::group_service::GroupService::new(
            db,
            QueryProfile {
                max_depth: None,
                max_width: Some(1),
            },
            common::make_enforcer(),
            Arc::new(resource_group::infra::storage::group_repo::GroupRepository),
            Arc::new(TypeRepository),
            common::make_types_registry(),
        )
    }

    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = width_limited_group_service(db.clone());

    let t = {
        // `resolve_ids` rejects a parent path that doesn't exist yet, so a
        // type can't reference itself as an allowed parent at create time;
        // create it plain, then update to add the self-reference below.
        let code = format!(
            "{}x.test.pgwidth.i{}.v1~",
            toolkit_gts::gts_id!("cf.core.rg.type.v1~"),
            Uuid::now_v7().as_simple()
        );
        type_svc
            .create_type(resource_group_sdk::CreateTypeRequest {
                code,
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: None,
            })
            .await
            .expect("create type (initial)")
    };
    let t = type_svc
        .update_type(
            &t.code,
            resource_group_sdk::UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![t.code.clone()],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update type to self-reference");

    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;

    let barrier = Arc::new(Barrier::new(2));
    let svc1 = width_limited_group_service(db.clone());
    let svc2 = width_limited_group_service(db.clone());
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let (ctx1, ctx2) = (ctx.clone(), ctx.clone());
    let (code1, code2) = (t.code.clone(), t.code.clone());

    let t1 = tokio::spawn(async move {
        b1.wait().await;
        svc1.create_group(
            &ctx1,
            CreateGroupRequest {
                id: None,
                code: code1,
                name: "child-1".to_owned(),
                parent_id: Some(root.id),
                metadata: None,
            },
            tenant_id,
        )
        .await
    });
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        svc2.create_group(
            &ctx2,
            CreateGroupRequest {
                id: None,
                code: code2,
                name: "child-2".to_owned(),
                parent_id: Some(root.id),
                metadata: None,
            },
            tenant_id,
        )
        .await
    });

    let (r1, r2) = timed("negative_control_width_limited_race", async {
        tokio::join!(t1, t2)
    })
    .await;
    let r1 = r1.expect("task 1 join");
    let r2 = r2.expect("task 2 join");

    let ok_count = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok_count, 1,
        "exactly one concurrent create under max_width=1 must succeed: r1={r1:?} r2={r2:?}"
    );
    let loser = if r1.is_err() { &r1 } else { &r2 };
    let loser_err = loser.as_ref().unwrap_err();
    // RG-15 fixed (same as negative_control_tenant_root_race_exactly_one_succeeds):
    // the loser must now always get the clean LimitViolation.
    assert!(
        matches!(loser_err, DomainError::LimitViolation { .. }),
        "the losing create must get a clean LimitViolation (proving transaction_with_retry \
         + the RG-15 fix absorbed the SSI conflict), got: {loser_err:?}"
    );

    assert_hierarchy_invariants(db).await;

    group_svc
        .delete_group(&ctx, root.id, true)
        .await
        .expect("cleanup: force delete root + surviving child");
}

/// Runs the five scenarios above against one shared PostgreSQL database. The
/// only grouped test touching the global `TENANT_RG_TYPE_PATH` invariant, so
/// it runs safely concurrent with [`hierarchy_move_races`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn membership_and_type_races() {
    let pg = pg_fixture_or_skip!();
    let db = pg.db().clone();

    eprintln!(
        "=== membership_and_type_races: membership_first_write_race_exactly_one_tenant_wins ==="
    );
    membership_first_write_race_exactly_one_tenant_wins(&db).await;

    eprintln!("=== membership_and_type_races: delete_type_races_create_group_resolves_cleanly ===");
    delete_type_races_create_group_resolves_cleanly(&db).await;

    eprintln!(
        "=== membership_and_type_races: create_type_conflict_retried_yields_clean_already_exists_for_loser ==="
    );
    create_type_conflict_retried_yields_clean_already_exists_for_loser(&db).await;

    eprintln!(
        "=== membership_and_type_races: negative_control_tenant_root_race_exactly_one_succeeds ==="
    );
    negative_control_tenant_root_race_exactly_one_succeeds(&db).await;

    eprintln!(
        "=== membership_and_type_races: negative_control_width_limited_race_exactly_one_succeeds ==="
    );
    negative_control_width_limited_race_exactly_one_succeeds(&db).await;
}

// Move-scenario races: none of the five scenarios above races two hierarchy
// writes that touch *overlapping* closure rows (as opposed to independent
// inserts). These five close that gap.

/// A type that can be root and lists itself as an allowed parent, so its
/// groups can be freely reparented under one another (`resolve_ids` rejects
/// a parent path that doesn't exist yet, hence the create-then-update).
async fn self_referencing_root_type(
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
) -> resource_group_sdk::ResourceGroupType {
    let t = common::create_root_type(type_svc, suffix).await;
    type_svc
        .update_type(
            &t.code,
            resource_group_sdk::UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![t.code.clone()],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update type to self-reference")
}

/// `A` moves under `B` while `B` moves under `A`: each side's cycle check
/// reads exactly the row the other rebuild writes, textbook write-skew. The
/// retried loser must report `CycleDetected`, never an actual cycle.
async fn move_a_to_b_races_move_b_to_a(db: &Arc<DBProvider<DbError>>) {
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let t = self_referencing_root_type(&type_svc, "pgmoveaabba").await;

    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let a = common::create_root_group(&group_svc, &ctx, &t.code, "A", tenant_id).await;
    let b = common::create_root_group(&group_svc, &ctx, &t.code, "B", tenant_id).await;

    let barrier = Arc::new(Barrier::new(2));
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let svc1 = common::make_group_service(db.clone());
    let svc2 = common::make_group_service(db.clone());
    let (a_id, b_id) = (a.id, b.id);

    let t1 = tokio::spawn(async move {
        b1.wait().await;
        svc1.move_group(a_id, Some(b_id)).await
    });
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        svc2.move_group(b_id, Some(a_id)).await
    });

    let (r1, r2) = timed("move_a_to_b_races_move_b_to_a", async {
        tokio::join!(t1, t2)
    })
    .await;
    let r1 = r1.expect("task 1 join");
    let r2 = r2.expect("task 2 join");

    eprintln!(
        "move_a_to_b_races_move_b_to_a: move_A_under_B={} move_B_under_A={}",
        describe_result(&r1),
        describe_result(&r2),
    );

    let ok_count = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok_count, 1,
        "exactly one of A->B / B->A must succeed (the other must be rejected as a cycle) -- \
         both succeeding would mean an actual cycle was written; both failing would mean neither \
         side ever got a clean shot: move_A_under_B={r1:?} move_B_under_A={r2:?}"
    );
    let loser_err = if let Err(e) = &r1 {
        e
    } else {
        r2.as_ref().unwrap_err()
    };
    assert!(
        matches!(loser_err, DomainError::CycleDetected { .. }),
        "the losing move must get a clean CycleDetected (the retried attempt should see the \
         winner's already-committed move and correctly detect the would-be cycle), got: \
         {loser_err:?}"
    );

    // The interesting part: does the *data* actually stay a tree? Recompute
    // the whole-table transitive closure and check for cycles independently
    // of which side "won" the call.
    assert_hierarchy_invariants(db).await;

    // Force-deleting A cascades into B too if B ended up under A; either
    // way, clean up both roots (ignore NotFound on whichever one the first
    // delete already swept up).
    group_svc.delete_group(&ctx, a.id, true).await.ok();
    group_svc.delete_group(&ctx, b.id, true).await.ok();
}

/// `M` moves from `R0` to `R2` while its own child `L` moves to `R3` at the
/// same instant, both touching overlapping closure rows for `L`. The moves
/// are independent, so both must succeed once retry absorbs any conflict.
async fn move_ancestor_races_move_descendant(db: &Arc<DBProvider<DbError>>) {
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let t = self_referencing_root_type(&type_svc, "pgmoveanc").await;

    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let r0 = common::create_root_group(&group_svc, &ctx, &t.code, "R0", tenant_id).await;
    let m = common::create_child_group(&group_svc, &ctx, &t.code, r0.id, "M", tenant_id).await;
    let l = common::create_child_group(&group_svc, &ctx, &t.code, m.id, "L", tenant_id).await;
    let r2 = common::create_root_group(&group_svc, &ctx, &t.code, "R2", tenant_id).await;
    let r3 = common::create_root_group(&group_svc, &ctx, &t.code, "R3", tenant_id).await;

    let barrier = Arc::new(Barrier::new(2));
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let svc1 = common::make_group_service(db.clone());
    let svc2 = common::make_group_service(db.clone());
    let (m_id, l_id, r2_id, r3_id) = (m.id, l.id, r2.id, r3.id);

    let move_m_task = tokio::spawn(async move {
        b1.wait().await;
        svc1.move_group(m_id, Some(r2_id)).await
    });
    let move_leaf_task = tokio::spawn(async move {
        b2.wait().await;
        svc2.move_group(l_id, Some(r3_id)).await
    });

    let (move_m_res, move_leaf_res) = timed("move_ancestor_races_move_descendant", async {
        tokio::join!(move_m_task, move_leaf_task)
    })
    .await;
    let move_m_res = move_m_res.expect("move M task join");
    let move_leaf_res = move_leaf_res.expect("move L task join");

    eprintln!(
        "move_ancestor_races_move_descendant: move_M={} move_L={}",
        describe_result(&move_m_res),
        describe_result(&move_leaf_res),
    );

    // Both should normally succeed: the moves are independent and retry absorbs
    // the transient SSI conflict over the shared closure rows. Exhausting the
    // three-attempt budget is a legal outcome, so tolerate a recognized
    // contention error on one side and let the invariant check below carry the
    // guarantee.
    for (label, res) in [("move_M", &move_m_res), ("move_L", &move_leaf_res)] {
        if let Err(e) = res {
            assert!(
                is_contention_error(e),
                "{label} failed with something other than retry-budget exhaustion: {e:?}"
            );
            eprintln!("{label}: retry budget exhausted under contention ({e})");
        }
    }
    assert!(
        move_m_res.is_ok() || move_leaf_res.is_ok(),
        "both independent moves lost to contention, which no retry budget explains: \
         move_M={move_m_res:?} move_L={move_leaf_res:?}"
    );

    assert_hierarchy_invariants(db).await;

    // M is now under R2 (without L); L is now under R3. Force-deleting the
    // three original roots (R0 is now empty, R2 carries M, R3 carries L)
    // resets the harness.
    for root_id in [r0.id, r2.id, r3.id] {
        group_svc.delete_group(&ctx, root_id, true).await.ok();
    }
}

/// A new child `C` is created under `P` while `P` itself moves from `R0`
/// to `Q`, so `insert_ancestor_closure_rows` and `rebuild_subtree_closure`
/// touch the same ancestor chain; `C` must end up with `P`'s final chain.
async fn create_child_races_move_parent(db: &Arc<DBProvider<DbError>>) {
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let t = self_referencing_root_type(&type_svc, "pgcreatemove").await;

    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let r0 = common::create_root_group(&group_svc, &ctx, &t.code, "R0", tenant_id).await;
    let p = common::create_child_group(&group_svc, &ctx, &t.code, r0.id, "P", tenant_id).await;
    let q = common::create_root_group(&group_svc, &ctx, &t.code, "Q", tenant_id).await;

    let barrier = Arc::new(Barrier::new(2));
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let svc1 = common::make_group_service(db.clone());
    let svc2 = common::make_group_service(db.clone());
    let ctx1 = ctx.clone();
    let (code1, p_id, q_id) = (t.code.clone(), p.id, q.id);

    let create_task = tokio::spawn(async move {
        b1.wait().await;
        svc1.create_group(
            &ctx1,
            CreateGroupRequest {
                id: None,
                code: code1,
                name: "C".to_owned(),
                parent_id: Some(p_id),
                metadata: None,
            },
            tenant_id,
        )
        .await
    });
    let move_task = tokio::spawn(async move {
        b2.wait().await;
        svc2.move_group(p_id, Some(q_id)).await
    });

    let (create_res, move_res) = timed("create_child_races_move_parent", async {
        tokio::join!(create_task, move_task)
    })
    .await;
    let create_res = create_res.expect("create task join");
    let move_res = move_res.expect("move task join");

    eprintln!(
        "create_child_races_move_parent: create_C={} move_P={}",
        describe_result(&create_res),
        describe_result(&move_res),
    );

    // Same tolerance as the ancestor/descendant race above: independent
    // operations, but a spent retry budget is a legal outcome.
    for (label, res) in [("create_C", &create_res), ("move_P", &move_res)] {
        if let Err(e) = res {
            assert!(
                is_contention_error(e),
                "{label} failed with something other than retry-budget exhaustion: {e:?}"
            );
            eprintln!("{label}: retry budget exhausted under contention ({e})");
        }
    }
    assert!(
        create_res.is_ok() || move_res.is_ok(),
        "both independent operations lost to contention, which no retry budget explains: \
         create_C={create_res:?} move_P={move_res:?}"
    );

    assert_hierarchy_invariants(db).await;

    // P (now under Q, with C under it) plus the now-empty R0 and Q.
    for root_id in [r0.id, q.id] {
        group_svc.delete_group(&ctx, root_id, true).await.ok();
    }
}

/// `G` is force-deleted while a caller concurrently attaches a new child to
/// it. `parent_id` is `ON DELETE RESTRICT`, so this asks whether retry
/// resolves the race cleanly (`GroupNotFound`, or child cascaded too).
async fn force_delete_races_create_child(db: &Arc<DBProvider<DbError>>) {
    const TRIALS: usize = 10;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let t = self_referencing_root_type(&type_svc, "pgfdelcreate").await;
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let mut clean_rejection = 0usize; // create failed cleanly (GroupNotFound)
    let mut cascaded_child = 0usize; // create succeeded, but child is gone afterwards (delete's cascade caught it)
    let mut orphan_survivor = 0usize; // create succeeded AND child still exists post-delete -- would violate FK RESTRICT, should be unreachable
    let mut unexpected = 0usize;

    for i in 0..TRIALS {
        let g =
            common::create_root_group(&group_svc, &ctx, &t.code, &format!("G{i}"), tenant_id).await;
        let barrier = Arc::new(Barrier::new(2));
        let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
        let group_svc1 = common::make_group_service(db.clone());
        let group_svc2 = common::make_group_service(db.clone());
        let ctx1 = ctx.clone();
        let ctx2 = ctx.clone();
        let (code1, g_id) = (t.code.clone(), g.id);

        let delete_task = tokio::spawn(async move {
            b1.wait().await;
            group_svc1.delete_group(&ctx1, g_id, true).await
        });
        let create_task = tokio::spawn(async move {
            b2.wait().await;
            group_svc2
                .create_group(
                    &ctx2,
                    CreateGroupRequest {
                        id: None,
                        code: code1,
                        name: "C".to_owned(),
                        parent_id: Some(g_id),
                        metadata: None,
                    },
                    tenant_id,
                )
                .await
        });

        let (delete_res, create_res) = timed("force_delete_races_create_child trial", async {
            tokio::join!(delete_task, create_task)
        })
        .await;
        let delete_res = delete_res.expect("delete task join");
        let create_res = create_res.expect("create task join");

        assert!(
            delete_res.is_ok(),
            "force_delete_group on an existing group must never itself fail: {delete_res:?}"
        );

        match &create_res {
            Err(DomainError::GroupNotFound { .. }) => clean_rejection += 1,
            Ok(child) => {
                let still_exists = group_svc.get_group_unscoped(child.id).await.is_ok();
                if still_exists {
                    orphan_survivor += 1;
                } else {
                    cascaded_child += 1;
                }
            }
            Err(_) => {
                unexpected += 1;
                eprintln!(
                    "force_delete_races_create_child: unexpected create outcome: {}",
                    describe_result(&create_res)
                );
            }
        }

        assert_hierarchy_invariants(db).await;
    }

    eprintln!(
        "force_delete_races_create_child: clean_rejection={clean_rejection} \
         cascaded_child={cascaded_child} orphan_survivor={orphan_survivor} \
         unexpected={unexpected} (out of {TRIALS} trials)"
    );
    assert_eq!(
        orphan_survivor, 0,
        "RG-16 candidate: {orphan_survivor}/{TRIALS} trials left a child alive whose parent was \
         concurrently force-deleted -- this would mean the FK RESTRICT was somehow bypassed"
    );
    assert_eq!(
        unexpected, 0,
        "{unexpected}/{TRIALS} trials produced a raw/unexpected error instead of a clean \
         GroupNotFound or a successful (later cascaded) create -- possible RG-16 candidate"
    );
}

/// Same question as `force_delete_races_create_child`, but for a
/// membership row instead of a child group: `G` is force-deleted while a
/// caller concurrently tries to `add_membership` to it.
async fn force_delete_races_add_membership(db: &Arc<DBProvider<DbError>>) {
    const TRIALS: usize = 10;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let member_type = common::create_root_type(&type_svc, "pgfdelmbrres").await;
    let grp_type = type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: format!(
                "{}x.test.pgfdelmbrgrp.i{}.v1~",
                toolkit_gts::gts_id!("cf.core.rg.type.v1~"),
                Uuid::now_v7().as_simple()
            ),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create membership-holding type");

    let mut clean_rejection = 0usize; // add_membership failed cleanly (GroupNotFound)
    let mut cascaded_membership = 0usize; // add succeeded, but membership is gone afterwards
    let mut orphan_survivor = 0usize; // add succeeded AND membership still exists post-delete -- should be unreachable
    let mut unexpected = 0usize;

    for i in 0..TRIALS {
        let g = common::create_root_group(
            &group_svc,
            &ctx,
            &grp_type.code,
            &format!("G{i}"),
            tenant_id,
        )
        .await;
        let resource_id = format!("res-{}", Uuid::now_v7().as_simple());
        let barrier = Arc::new(Barrier::new(2));
        let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
        let group_svc1 = common::make_group_service(db.clone());
        let membership_svc = common::make_membership_service(db.clone());
        let ctx1 = ctx.clone();
        let ctx2 = ctx.clone();
        let g_id = g.id;
        let (rt, rid) = (member_type.code.clone(), resource_id.clone());

        let delete_task = tokio::spawn(async move {
            b1.wait().await;
            group_svc1.delete_group(&ctx1, g_id, true).await
        });
        let add_task = tokio::spawn(async move {
            b2.wait().await;
            membership_svc.add_membership(&ctx2, g_id, &rt, &rid).await
        });

        let (delete_res, add_res) = timed("force_delete_races_add_membership trial", async {
            tokio::join!(delete_task, add_task)
        })
        .await;
        let delete_res = delete_res.expect("delete task join");
        let add_res = add_res.expect("add task join");

        assert!(
            delete_res.is_ok(),
            "force_delete_group on an existing group must never itself fail: {delete_res:?}"
        );

        match &add_res {
            Err(DomainError::GroupNotFound { .. }) => clean_rejection += 1,
            Ok(_) => {
                let survivors =
                    distinct_tenant_ids_for_resource(db, &member_type.code, &resource_id).await;
                if survivors.is_empty() {
                    cascaded_membership += 1;
                } else {
                    orphan_survivor += 1;
                }
            }
            Err(_) => {
                unexpected += 1;
                eprintln!(
                    "force_delete_races_add_membership: unexpected add outcome: {}",
                    describe_result(&add_res)
                );
            }
        }

        assert_membership_tenant_invariant(db).await;
        assert_hierarchy_invariants(db).await;
    }

    eprintln!(
        "force_delete_races_add_membership: clean_rejection={clean_rejection} \
         cascaded_membership={cascaded_membership} orphan_survivor={orphan_survivor} \
         unexpected={unexpected} (out of {TRIALS} trials)"
    );
    assert_eq!(
        orphan_survivor, 0,
        "RG-16 candidate: {orphan_survivor}/{TRIALS} trials left a membership alive whose group \
         was concurrently force-deleted"
    );
    assert_eq!(
        unexpected, 0,
        "{unexpected}/{TRIALS} trials produced a raw/unexpected error instead of a clean \
         GroupNotFound or a successful (later cascaded) add_membership -- possible RG-16 \
         candidate"
    );
}

/// Runs the five `move_group` scenarios above against one shared PostgreSQL
/// database. None creates a type under `TENANT_RG_TYPE_PATH`, so this test
/// runs safely concurrent with [`membership_and_type_races`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hierarchy_move_races() {
    let pg = pg_fixture_or_skip!();
    let db = pg.db().clone();

    eprintln!("=== hierarchy_move_races: move_a_to_b_races_move_b_to_a ===");
    move_a_to_b_races_move_b_to_a(&db).await;

    eprintln!("=== hierarchy_move_races: move_ancestor_races_move_descendant ===");
    move_ancestor_races_move_descendant(&db).await;

    eprintln!("=== hierarchy_move_races: create_child_races_move_parent ===");
    create_child_races_move_parent(&db).await;

    eprintln!("=== hierarchy_move_races: force_delete_races_create_child ===");
    force_delete_races_create_child(&db).await;

    eprintln!("=== hierarchy_move_races: force_delete_races_add_membership ===");
    force_delete_races_add_membership(&db).await;
}
