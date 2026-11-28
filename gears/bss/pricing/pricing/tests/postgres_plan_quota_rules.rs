//! Plan quota rules on Postgres (D-543, D-545, D-546): migration 000024's CHECKs and keys hold on
//! the native engine, its `down` refuses while a rule exists and it re-applies; the rule
//! repository round-trips and refuses a damaged set; a set read in one transaction keeps its
//! snapshot while a replace commits; and the races of a rule set's replace store one set and leave
//! no orphan. Through the endpoints (D-540, D-544): a replace, its replay and its fences, and the
//! checks read of a revision with rules, single and batch.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;
mod plan_support;
mod quota_support;

use bss_pricing::infra::storage::migrations::Migrator;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use uuid::Uuid;

const MIGRATION: &str = "m20261121_000024_plan_quota_rules";
const TENANT: Uuid = Uuid::from_u128(0x24);
const BOOK: Uuid = Uuid::from_u128(0x2401);
const PLAN: Uuid = Uuid::from_u128(0x2402);
const REVISION: Uuid = Uuid::from_u128(0x2403);
const ITEM: Uuid = Uuid::from_u128(0x2404);
const AUTHOR: Uuid = Uuid::from_u128(0x2405);
const SKU: Uuid = Uuid::from_u128(0x2406);
const NOW: &str = "'2026-11-21T00:00:00Z'";

fn q(id: Uuid) -> String {
    format!("'{id}'")
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    conn.execute_raw(Statement::from_string(
        DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .map(|_| ())
}

/// A book, a plan, its draft revision and one item without an entry, by raw SQL.
async fn draft_with_item(conn: &DatabaseConnection) -> (Uuid, Uuid) {
    for sql in [
        format!(
            "INSERT INTO bss.pricing_price_book \
             (id,tenant_id,code,name,currency,version,created_at,updated_at) \
             VALUES ({},{},'eur','eur','EUR',1,{NOW},{NOW})",
            q(BOOK),
            q(TENANT)
        ),
        format!(
            "INSERT INTO bss.pricing_plan \
             (id,tenant_id,code,name,version,created_by,created_at,updated_at) \
             VALUES ({},{},'P','P',1,{},{NOW},{NOW})",
            q(PLAN),
            q(TENANT),
            q(AUTHOR)
        ),
        format!(
            "INSERT INTO bss.pricing_plan_revision \
             (id,tenant_id,plan_id,rev_no,book_id,state,version,created_by,created_at,updated_at) \
             VALUES ({},{},{},1,{},'draft',1,{},{NOW},{NOW})",
            q(REVISION),
            q(TENANT),
            q(PLAN),
            q(BOOK),
            q(AUTHOR)
        ),
        format!(
            "INSERT INTO bss.pricing_plan_item \
             (id,tenant_id,revision_id,sku_id,treatment,reference_state,version,created_by,\
             created_at,updated_at) \
             VALUES ({},{},{},{},'included','unreserved',1,{},{NOW},{NOW})",
            q(ITEM),
            q(TENANT),
            q(REVISION),
            q(SKU),
            q(AUTHOR)
        ),
    ] {
        exec(conn, &sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    (REVISION, ITEM)
}

/// One valid rule row, every column written, with each `(column, SQL literal)` of `overrides` in
/// place of its default.
async fn insert_rule(
    conn: &DatabaseConnection,
    revision: Uuid,
    item: Uuid,
    overrides: &[(&str, &str)],
) -> Result<(), DbErr> {
    let mut columns: Vec<(&str, String)> = vec![
        ("revision_id", q(revision)),
        ("rule_id", q(Uuid::new_v4())),
        ("tenant_id", q(TENANT)),
        ("item_id", q(item)),
        ("definition_id", q(Uuid::from_u128(0xd1))),
        ("definition_version", "'1'".to_owned()),
        ("definition_digest", format!("'{}'", "a".repeat(64))),
        ("dimension_value", "NULL".to_owned()),
        ("logical_binding_key", "'compute'".to_owned()),
        ("subject_selector", "'resource_owner'".to_owned()),
        (
            "scope_selection_json",
            r#"'{"region":"region-1"}'"#.to_owned(),
        ),
        ("amount", "'10'".to_owned()),
        (
            "unit_ref",
            "'gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1'".to_owned(),
        ),
        ("basis", "'per_plan_unit'".to_owned()),
        ("evidence_json", "'{}'".to_owned()),
        ("availability_json", "'{}'".to_owned()),
        ("created_at", NOW.to_owned()),
    ];
    for (column, value) in overrides {
        let slot = columns
            .iter_mut()
            .find(|(name, _)| name == column)
            .unwrap_or_else(|| panic!("no column {column}"));
        (*value).clone_into(&mut slot.1);
    }
    let names = columns
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(",");
    let values = columns
        .iter()
        .map(|(_, v)| v.as_str())
        .collect::<Vec<_>>()
        .join(",");
    exec(
        conn,
        &format!("INSERT INTO bss.pricing_plan_quota_rule ({names}) VALUES ({values})"),
    )
    .await
}

/// The twin of the SQLite `the_rule_checks_hold`, with the set's CHECKs and both foreign keys:
/// each refusal names its own constraint.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_rule_checks_hold_on_postgres() {
    let pg = Pg::applied().await;
    let conn = pg.raw().await;
    let (revision, item) = draft_with_item(&conn).await;
    for (column, value, check) in [
        ("basis", "'PerPlanUnit'", "basis"),
        ("subject_selector", "'tenant'", "subject"),
        ("logical_binding_key", "''", "key"),
        (
            "logical_binding_key",
            &format!("'{}'", "k".repeat(65)),
            "key",
        ),
        ("definition_digest", "'abc'", "digest"),
        ("amount", "''", "amount"),
        ("amount", &format!("'{}'", "9".repeat(40)), "amount"),
        ("definition_version", "''", "version"),
    ] {
        let error = insert_rule(&conn, revision, item, &[(column, value)])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!(
                "violates check constraint \"chk_pricing_plan_quota_rule_{check}\""
            )),
            "{column}: {error}"
        );
    }
    for (revision, item, what) in [
        (revision, Uuid::new_v4(), "an item that does not exist"),
        (Uuid::new_v4(), item, "a revision that does not exist"),
    ] {
        let error = insert_rule(&conn, revision, item, &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("violates foreign key constraint"),
            "{what}: {error}"
        );
    }
    insert_rule(&conn, revision, item, &[]).await.unwrap();
    let error = exec(
        &conn,
        &format!("DELETE FROM bss.pricing_plan_item WHERE id = {}", q(item)),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("pricing_plan_quota_rule_item_id_fkey"),
        "an item that a rule names: {error}"
    );

    let set = |count: &str, digest: &str| {
        format!(
            "INSERT INTO bss.pricing_plan_quota_set \
             (revision_id,tenant_id,quota_rules_digest,rule_count,updated_by,updated_at) \
             VALUES ({},{},'{digest}',{count},{},{NOW})",
            q(revision),
            q(TENANT),
            q(AUTHOR)
        )
    };
    let full = "a".repeat(64);
    for (sql, check) in [
        (set("201", &full), "count"),
        (set("0", &full), "count"),
        (set("1", "abc"), "digest"),
    ] {
        let error = exec(&conn, &sql).await.unwrap_err().to_string();
        assert!(
            error.contains(&format!(
                "violates check constraint \"chk_pricing_plan_quota_set_{check}\""
            )),
            "{sql}: {error}"
        );
    }
    exec(&conn, &set("200", &full)).await.unwrap();
}

/// 000024's `down` on Postgres: refused while a rule exists, naming the table; on an empty rule
/// table it drops both tables, twice without effect, and `up` creates them again, twice without
/// effect.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn down_refuses_while_a_rule_exists_on_postgres() {
    let pg = Pg::applied().await;
    let conn = pg.raw().await;
    let (revision, item) = draft_with_item(&conn).await;
    insert_rule(&conn, revision, item, &[]).await.unwrap();
    let schema = sea_orm::Database::connect(pg.url(true)).await.unwrap();
    let manager = SchemaManager::new(&schema);
    let step = Migrator::migrations()
        .into_iter()
        .find(|m| m.name() == MIGRATION)
        .unwrap();
    let refused = step.down(&manager).await.unwrap_err().to_string();
    assert!(
        refused.contains(MIGRATION) && refused.contains("pricing_plan_quota_rule holds rows"),
        "{refused}"
    );
    exec(&conn, "DELETE FROM bss.pricing_plan_quota_rule")
        .await
        .unwrap();
    step.down(&manager).await.unwrap();
    step.down(&manager).await.unwrap();
    let present = |table: &'static str| {
        let conn = &conn;
        async move {
            conn.query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                format!("SELECT to_regclass('bss.{table}') IS NOT NULL AS present"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<bool>("", "present")
            .unwrap()
        }
    };
    assert!(!present("pricing_plan_quota_rule").await);
    assert!(!present("pricing_plan_quota_set").await);
    step.up(&manager).await.unwrap();
    step.up(&manager).await.unwrap();
    assert!(present("pricing_plan_quota_rule").await);
    assert!(present("pricing_plan_quota_set").await);
    let (revision, item) = (REVISION, ITEM);
    insert_rule(&conn, revision, item, &[]).await.unwrap();
}

// ------------------------------------------------------------------ the races of a replace

use bss_pricing::infra::storage::{
    RepoError,
    entity::{plan as plan_e, plan_item, plan_quota_rule, plan_revision, price_book},
    repo::{book_repo, plan_item_repo, plan_quota_repo, plan_repo, plan_revision_repo, price_repo},
};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError, DbTx};

fn now() -> time::OffsetDateTime {
    // Postgres keeps microseconds; a fixture time with nanoseconds would not read back equal.
    time::OffsetDateTime::from_unix_timestamp(1_795_000_000).unwrap()
}

/// A tenant's draft revision (version 1) with two items, written through the repositories.
struct Seed {
    pg: Pg,
    scope: AccessScope,
    tenant: Uuid,
    user: Uuid,
    book: Uuid,
    revision: Uuid,
    items: [Uuid; 2],
}

async fn seed() -> Seed {
    let pg = Pg::applied().await;
    let provider = DBProvider::<DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let user = Uuid::new_v4();
    let book = book_repo::insert(
        &conn,
        &scope,
        price_book::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            code: "eur".into(),
            name: "Default EUR".into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: now(),
            updated_at: now(),
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    let mut s = Seed {
        pg,
        scope,
        tenant,
        user,
        book: book.id,
        revision: Uuid::nil(),
        items: [Uuid::nil(); 2],
    };
    (s.revision, s.items) = plan_with_draft(&s, "pro").await;
    s
}

/// Another plan of the seed's tenant with a draft revision (version 1) and two items without an
/// entry; answers the revision and its items.
async fn plan_with_draft(s: &Seed, code: &str) -> (Uuid, [Uuid; 2]) {
    let provider = DBProvider::<DbError>::new(s.pg.db().await);
    let conn = provider.conn().unwrap();
    let plan = plan_repo::insert(
        &conn,
        &s.scope,
        plan_e::Model {
            id: Uuid::new_v4(),
            tenant_id: s.tenant,
            code: code.into(),
            name: code.into(),
            published_rev: None,
            version: 1,
            created_by: s.user,
            created_at: now(),
            updated_at: now(),
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: now(),
        },
    )
    .await
    .unwrap();
    let revision = plan_revision_repo::insert(
        &conn,
        &s.scope,
        plan_revision::Model {
            id: Uuid::new_v4(),
            tenant_id: s.tenant,
            plan_id: plan.id,
            rev_no: 1,
            book_id: s.book,
            state: "draft".into(),
            available_from: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: s.user,
            created_at: now(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    let mut items = [Uuid::nil(); 2];
    for slot in &mut items {
        *slot = plan_item_repo::insert(
            &conn,
            &s.scope,
            plan_item::Model {
                id: Uuid::new_v4(),
                tenant_id: s.tenant,
                revision_id: revision.id,
                sku_id: Uuid::new_v4(),
                price_book_entry_id: None,
                treatment: "included".into(),
                included_qty: None,
                qty_min: None,
                reservation_id: None,
                reference_state: "unreserved".into(),
                version: 1,
                created_by: s.user,
                created_at: now(),
                updated_at: now(),
            },
        )
        .await
        .unwrap()
        .id;
    }
    (revision.id, items)
}

/// A complete rule row of the seed's revision.
fn rule(s: &Seed, n: u128, item: Uuid, amount: &str) -> plan_quota_rule::Model {
    rule_on(s, s.revision, n, item, amount)
}

/// A complete rule row of `revision`, in the seed's tenant.
fn rule_on(s: &Seed, revision: Uuid, n: u128, item: Uuid, amount: &str) -> plan_quota_rule::Model {
    plan_quota_rule::Model {
        revision_id: revision,
        rule_id: Uuid::from_u128(n),
        tenant_id: s.tenant,
        item_id: item,
        definition_id: Uuid::from_u128(0xd1),
        definition_version: "1".into(),
        definition_digest: "a".repeat(64),
        dimension_value: None,
        logical_binding_key: "compute".into(),
        subject_selector: "resource_owner".into(),
        scope_selection_json: r#"{"region":"region-1"}"#.into(),
        amount: amount.into(),
        unit_ref: "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1".into(),
        basis: "per_plan_unit".into(),
        evidence_json: "{}".into(),
        availability_json: "{}".into(),
        created_at: now(),
    }
}

type Work = Arc<
    dyn for<'a> Fn(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<(), RepoError>> + Send + 'a>>
        + Send
        + Sync,
>;

/// The seed's replace of its draft at version 1 with `rules` and `digest`.
fn replacing(s: &Seed, rules: Vec<plan_quota_rule::Model>, digest: String) -> Work {
    replacing_at(s, 1, rules, digest)
}

/// The seed's replace of its draft at `version` with `rules` and `digest`.
fn replacing_at(
    s: &Seed,
    version: i64,
    rules: Vec<plan_quota_rule::Model>,
    digest: String,
) -> Work {
    let (scope, tenant, revision, user) = (s.scope.clone(), s.tenant, s.revision, s.user);
    Arc::new(move |tx| {
        let (scope, rules, digest) = (scope.clone(), rules.clone(), digest.clone());
        Box::pin(async move {
            plan_quota_repo::replace(
                tx,
                &scope,
                tenant,
                revision,
                version,
                rules,
                digest,
                user,
                now(),
            )
            .await
            .map(|_| ())
        })
    })
}

/// The seed's delete of its draft item `item` at version 1.
fn deleting(s: &Seed, item: Uuid) -> Work {
    let (scope, tenant) = (s.scope.clone(), s.tenant);
    Arc::new(move |tx| {
        let scope = scope.clone();
        Box::pin(async move { plan_item_repo::delete_draft(tx, &scope, tenant, item, 1).await })
    })
}

/// Run `first` and `second` in two serializable transactions on two pools, in that order on
/// purpose: `first` writes and holds its transaction open until `second` has begun and read the
/// revision's set, so `second`'s snapshot cannot see `first`'s write; `second` writes after that.
/// Each runs in `price_repo::transaction`, which retries a serialization failure; the gate is
/// passed on the first attempt only. Answers both outcomes and the attempts `second` made.
async fn race(
    s: &Seed,
    first: Work,
    second: Work,
) -> (Result<(), RepoError>, Result<(), RepoError>, usize) {
    let written = Arc::new(tokio::sync::Barrier::new(2));
    let read = Arc::new(tokio::sync::Barrier::new(2));
    let attempts = Arc::new(AtomicUsize::new(0));
    let one = {
        let (written, read) = (Arc::clone(&written), Arc::clone(&read));
        let tries = AtomicUsize::new(0);
        let db = s.pg.db().await;
        async move {
            price_repo::transaction(&db, move |tx| {
                let (first, written, read) =
                    (Arc::clone(&first), Arc::clone(&written), Arc::clone(&read));
                let go = tries.fetch_add(1, Ordering::SeqCst) == 0;
                Box::pin(async move {
                    first(tx).await?;
                    if go {
                        written.wait().await;
                        read.wait().await;
                    }
                    Ok(())
                })
            })
            .await
        }
    };
    let two = {
        let (written, read, attempts) = (
            Arc::clone(&written),
            Arc::clone(&read),
            Arc::clone(&attempts),
        );
        let (scope, tenant, revision) = (s.scope.clone(), s.tenant, s.revision);
        let db = s.pg.db().await;
        async move {
            written.wait().await;
            price_repo::transaction(&db, move |tx| {
                let (second, read, attempts) = (
                    Arc::clone(&second),
                    Arc::clone(&read),
                    Arc::clone(&attempts),
                );
                let scope = scope.clone();
                let go = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                Box::pin(async move {
                    plan_quota_repo::for_revision(tx, &scope, tenant, revision).await?;
                    if go {
                        read.wait().await;
                    }
                    second(tx).await
                })
            })
            .await
        }
    };
    // Bounded: a writer that fails before its gate would leave the other waiting at it.
    let (a, b) = tokio::time::timeout(RACE_BUDGET, async { tokio::join!(one, two) })
        .await
        .expect("the race finishes within its budget: a writer failed before its gate");
    (a, b, attempts.load(Ordering::SeqCst))
}

/// How long a race may run before it is reported as stuck.
const RACE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// One revision's set, read in a transaction of its own (one snapshot).
async fn set_of(s: &Seed, revision: Uuid) -> Result<plan_quota_repo::StoredSet, RepoError> {
    let (scope, tenant) = (s.scope.clone(), s.tenant);
    price_repo::transaction(&s.pg.db().await, move |tx| {
        let scope = scope.clone();
        Box::pin(async move { plan_quota_repo::for_revision(tx, &scope, tenant, revision).await })
    })
    .await
}

/// The sets of `revisions`, read in a transaction of their own (one snapshot).
async fn sets_of(
    s: &Seed,
    revisions: &[Uuid],
) -> Result<BTreeMap<Uuid, plan_quota_repo::StoredSet>, RepoError> {
    let (scope, tenant, revisions) = (s.scope.clone(), s.tenant, revisions.to_vec());
    price_repo::transaction(&s.pg.db().await, move |tx| {
        let (scope, revisions) = (scope.clone(), revisions.clone());
        Box::pin(
            async move { plan_quota_repo::for_revisions(tx, &scope, tenant, &revisions).await },
        )
    })
    .await
}

/// Rules that name an item the revision no longer holds; the foreign key keeps this at zero.
async fn orphans(s: &Seed) -> i64 {
    s.pg.raw()
        .await
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT count(*)::bigint AS n FROM bss.pricing_plan_quota_rule r WHERE NOT EXISTS \
             (SELECT 1 FROM bss.pricing_plan_item i WHERE i.id = r.item_id)"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

/// D-543: two replaces that both read version 1. The first commits; the second's first attempt,
/// whose snapshot predates that commit, is a serialization failure, and its retry finds version 2:
/// `STALE_REVISION`. The stored set is the first's, in both orders.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_replaces_on_one_version_store_one_set_on_postgres() {
    for a_first in [true, false] {
        let s = seed().await;
        let a = (vec![rule(&s, 1, s.items[0], "10")], "a".repeat(64));
        let b = (
            vec![rule(&s, 2, s.items[1], "20"), rule(&s, 3, s.items[0], "30")],
            "b".repeat(64),
        );
        let (won, lost) = if a_first { (a, b) } else { (b, a) };
        let (first, second, attempts) = race(
            &s,
            replacing(&s, won.0.clone(), won.1.clone()),
            replacing(&s, lost.0, lost.1),
        )
        .await;
        first.unwrap();
        assert!(
            matches!(
                second,
                Err(RepoError::Conflict {
                    code: "STALE_REVISION"
                })
            ),
            "a_first={a_first}: {second:?}"
        );
        assert_eq!(
            attempts, 2,
            "the second write met the first's uncommitted one"
        );
        let provider = DBProvider::<DbError>::new(s.pg.db().await);
        let stored = set_of(&s, s.revision).await.unwrap();
        let mut expected = won.0;
        expected.sort_by_key(|r| r.rule_id);
        assert_eq!(
            (stored.digest, stored.rules),
            (won.1, expected),
            "a_first={a_first}"
        );
        let revision =
            plan_revision_repo::find(&provider.conn().unwrap(), &s.scope, s.tenant, s.revision)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(revision.version, 2, "one replace moved the version");
    }
}

/// D-543: a replace that names item `a` and the delete of `a`, in two transactions, in both orders:
/// exactly one commits, and no rule names a missing item. The replace first: the delete is refused
/// by the rule's foreign key. The delete first: the replace's rule finds no item.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_item_delete_racing_a_replace_leaves_no_orphan_on_postgres() {
    for replace_first in [true, false] {
        let s = seed().await;
        let a = s.items[0];
        let replace = replacing(&s, vec![rule(&s, 1, a, "10")], "a".repeat(64));
        let delete = deleting(&s, a);
        let (replaced, deleted) = if replace_first {
            let (first, second, _) = race(&s, replace, delete).await;
            (first, second)
        } else {
            let (first, second, _) = race(&s, delete, replace).await;
            (second, first)
        };
        assert_eq!(
            usize::from(replaced.is_ok()) + usize::from(deleted.is_ok()),
            1,
            "replace_first={replace_first}: {replaced:?} / {deleted:?}"
        );
        assert_eq!(
            replaced.is_ok(),
            replace_first,
            "the first writer wins: {replaced:?}"
        );
        let loser = if replace_first { deleted } else { replaced };
        assert!(
            matches!(&loser, Err(RepoError::Driver { source, .. })
                if source.to_string().contains("pricing_plan_quota_rule_item_id_fkey")),
            "replace_first={replace_first}: the foreign key refused the loser: {loser:?}"
        );
        assert_eq!(orphans(&s).await, 0, "replace_first={replace_first}");
        let provider = DBProvider::<DbError>::new(s.pg.db().await);
        let conn = provider.conn().unwrap();
        let item = plan_item_repo::find(&conn, &s.scope, s.tenant, a)
            .await
            .unwrap();
        let rules = set_of(&s, s.revision).await.unwrap().rules;
        if replace_first {
            assert!(item.is_some());
            assert_eq!(rules.len(), 1);
        } else {
            assert!(item.is_none());
            assert!(rules.is_empty());
        }
    }
}

// ------------------------------------------------------------------ the repository on Postgres

/// The empty set's digest, pinned: sha256 of
/// `{"domain":"pricing.quota-rules.v1","payload":{"rules":[]}}`.
const EMPTY_SET_DIGEST: &str = "599cdcc359ef3e8e6f8a32068b381eb4db89aa0d2fd0e702ed2935a793dd1d5a";

/// Raw SQL past every repository: the damage a test needs.
async fn raw(s: &Seed, sql: &str) {
    exec(&s.pg.raw().await, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// The number of set rows `revision` holds, counted past every repository.
async fn set_rows(s: &Seed, revision: Uuid) -> i64 {
    s.pg.raw()
        .await
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT count(*)::bigint AS n FROM bss.pricing_plan_quota_set \
                 WHERE revision_id = {}",
                q(revision)
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

/// The twins of the SQLite repository tests on the native engine: the batch read over an `IN`
/// list (an empty one included) on native uuids, `item_has_rules`, `copy` with its timestamps and
/// its first set row, the cleared set, and `delete_for_draft`'s unlocked-draft filter.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_rule_repository_round_trips_on_postgres() {
    let s = seed().await;
    let provider = DBProvider::<DbError>::new(s.pg.db().await);
    let conn = provider.conn().unwrap();
    let rules = vec![rule(&s, 2, s.items[1], "20"), rule(&s, 1, s.items[0], "10")];
    plan_quota_repo::replace(
        &conn,
        &s.scope,
        s.tenant,
        s.revision,
        1,
        rules.clone(),
        "a".repeat(64),
        s.user,
        now(),
    )
    .await
    .unwrap();
    let (other, _) = plan_with_draft(&s, "other").await;
    let sets = sets_of(&s, &[s.revision, other, Uuid::new_v4()])
        .await
        .unwrap();
    assert_eq!(sets.keys().copied().collect::<Vec<_>>(), [s.revision]);
    let mut ordered = rules.clone();
    ordered.sort_by_key(|r| r.rule_id);
    assert_eq!(sets[&s.revision].rules, ordered, "whole models, by rule id");
    assert_eq!(sets[&s.revision].digest, "a".repeat(64));
    assert_eq!(set_of(&s, s.revision).await.unwrap(), sets[&s.revision]);
    assert!(sets_of(&s, &[]).await.unwrap().is_empty());
    let (empty, _) = plan_with_draft(&s, "empty").await;
    assert_eq!(set_of(&s, empty).await.unwrap().digest, EMPTY_SET_DIGEST);
    for (item, has) in [(s.items[0], true), (Uuid::new_v4(), false)] {
        assert_eq!(
            plan_quota_repo::item_has_rules(&conn, &s.scope, s.tenant, item)
                .await
                .unwrap(),
            has
        );
    }

    // A copy writes each rule onto its copied item and the target's first set row.
    let (target, copied) = plan_with_draft(&s, "copy").await;
    let at = now() + time::Duration::seconds(1);
    plan_quota_repo::copy(
        &conn,
        &s.scope,
        s.tenant,
        s.revision,
        target,
        &BTreeMap::from([(s.items[0], copied[0]), (s.items[1], copied[1])]),
        "b".repeat(64),
        s.user,
        at,
    )
    .await
    .unwrap();
    let copy = set_of(&s, target).await.unwrap();
    let expected: Vec<_> = ordered
        .iter()
        .map(|r| plan_quota_rule::Model {
            revision_id: target,
            item_id: if r.item_id == s.items[0] {
                copied[0]
            } else {
                copied[1]
            },
            created_at: at,
            ..r.clone()
        })
        .collect();
    assert_eq!((copy.digest, copy.rules), ("b".repeat(64), expected));
    let target_row = plan_revision_repo::find(&conn, &s.scope, s.tenant, target)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        target_row.version, 1,
        "a copy does not move the target's version"
    );

    // A cleared set has no set row, and both reads answer the empty set's digest.
    plan_quota_repo::replace(
        &conn,
        &s.scope,
        s.tenant,
        target,
        1,
        Vec::new(),
        plan_quota_repo::empty_digest(),
        s.user,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(set_rows(&s, target).await, 0);
    assert_eq!(set_of(&s, target).await.unwrap().digest, EMPTY_SET_DIGEST);
    assert!(sets_of(&s, &[target]).await.unwrap().is_empty());

    // `delete_for_draft` leaves a revision that is not an unlocked draft as it is...
    raw(
        &s,
        &format!(
            "UPDATE bss.pricing_plan_revision SET state = 'published', published_at = now() \
             WHERE id = {}",
            q(s.revision)
        ),
    )
    .await;
    plan_quota_repo::delete_for_draft(&conn, &s.scope, s.tenant, s.revision)
        .await
        .unwrap();
    assert_eq!(set_of(&s, s.revision).await.unwrap().rules.len(), 2);
    assert_eq!(set_rows(&s, s.revision).await, 1);
    // ...and removes a draft's rules and set row, after which its items can go.
    plan_quota_repo::copy(
        &conn,
        &s.scope,
        s.tenant,
        s.revision,
        target,
        &BTreeMap::from([(s.items[0], copied[0]), (s.items[1], copied[1])]),
        "b".repeat(64),
        s.user,
        at,
    )
    .await
    .unwrap();
    plan_quota_repo::delete_for_draft(&conn, &s.scope, s.tenant, target)
        .await
        .unwrap();
    assert_eq!(set_rows(&s, target).await, 0);
    assert!(set_of(&s, target).await.unwrap().rules.is_empty());
    for item in copied {
        plan_item_repo::delete_draft(&conn, &s.scope, s.tenant, item, 1)
            .await
            .unwrap();
    }
}

/// A set that disagrees with its rows is a corrupt row (500), never a wrong digest (D-541, D-546), on
/// Postgres: a rule without a set row, a count that disagrees and a set row without rules each read
/// as a corrupt row, in both reads.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_damaged_set_is_a_corrupt_row_on_postgres() {
    let damages = [
        "DELETE FROM bss.pricing_plan_quota_set WHERE revision_id = {r}",
        "UPDATE bss.pricing_plan_quota_set SET rule_count = 2 WHERE revision_id = {r}",
        "DELETE FROM bss.pricing_plan_quota_rule WHERE revision_id = {r}",
    ];
    for damage in damages {
        let s = seed().await;
        let provider = DBProvider::<DbError>::new(s.pg.db().await);
        plan_quota_repo::replace(
            &provider.conn().unwrap(),
            &s.scope,
            s.tenant,
            s.revision,
            1,
            vec![rule(&s, 1, s.items[0], "10")],
            "a".repeat(64),
            s.user,
            now(),
        )
        .await
        .unwrap();
        raw(&s, &damage.replace("{r}", &q(s.revision))).await;
        let one = set_of(&s, s.revision).await.unwrap_err();
        assert!(matches!(one, RepoError::CorruptRow(_)), "{damage}: {one:?}");
        let many = sets_of(&s, &[s.revision]).await.unwrap_err();
        assert!(
            matches!(many, RepoError::CorruptRow(_)),
            "{damage}: {many:?}"
        );
    }
}

/// D-546: a set read twice in one serializable transaction answers the same set while a replace
/// commits between the two reads. The reader's first read fixes its snapshot; the
/// replace then commits on another pool; the reader's second reads still answer the first set,
/// never a corrupt row or the other set's digest.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_set_read_in_one_transaction_keeps_its_snapshot_on_postgres() {
    let s = seed().await;
    plan_quota_repo::replace(
        &DBProvider::<DbError>::new(s.pg.db().await).conn().unwrap(),
        &s.scope,
        s.tenant,
        s.revision,
        1,
        vec![rule(&s, 1, s.items[0], "10")],
        "a".repeat(64),
        s.user,
        now(),
    )
    .await
    .unwrap();
    let read = Arc::new(tokio::sync::Barrier::new(2));
    let written = Arc::new(tokio::sync::Barrier::new(2));
    let reader = {
        let (read, written) = (Arc::clone(&read), Arc::clone(&written));
        let (scope, tenant, revision) = (s.scope.clone(), s.tenant, s.revision);
        let tries = AtomicUsize::new(0);
        let db = s.pg.db().await;
        async move {
            price_repo::transaction(&db, move |tx| {
                let (read, written, scope) =
                    (Arc::clone(&read), Arc::clone(&written), scope.clone());
                let go = tries.fetch_add(1, Ordering::SeqCst) == 0;
                Box::pin(async move {
                    let before =
                        plan_quota_repo::for_revision(tx, &scope, tenant, revision).await?;
                    if go {
                        read.wait().await;
                        written.wait().await;
                    }
                    let one = plan_quota_repo::for_revision(tx, &scope, tenant, revision).await?;
                    let many =
                        plan_quota_repo::for_revisions(tx, &scope, tenant, &[revision]).await?;
                    Ok((go, before, one, many))
                })
            })
            .await
        }
    };
    let writer = {
        let (read, written) = (Arc::clone(&read), Arc::clone(&written));
        let replace = replacing_at(
            &s,
            2,
            vec![rule(&s, 2, s.items[1], "20"), rule(&s, 3, s.items[0], "30")],
            "b".repeat(64),
        );
        let db = s.pg.db().await;
        async move {
            read.wait().await;
            let outcome = price_repo::transaction(&db, move |tx| replace(tx)).await;
            written.wait().await;
            outcome
        }
    };
    let (reader, writer) =
        tokio::time::timeout(RACE_BUDGET, async { tokio::join!(reader, writer) })
            .await
            .expect("the reader and the writer finish within the budget");
    writer.unwrap();
    let (first_attempt, before, one, many) = reader.unwrap();
    assert!(
        first_attempt,
        "the reader kept its first snapshot to the end"
    );
    assert_eq!(before.digest, "a".repeat(64));
    assert_eq!(one, before, "the second read is the first read's set");
    assert_eq!(many[&s.revision], before);
    let after = set_of(&s, s.revision).await.unwrap();
    assert_eq!((after.digest, after.rules.len()), ("b".repeat(64), 2));
}

/// A fixture over an applied Postgres database, with a quota catalog double in its hub.
async fn endpoints() -> (
    plan_support::Fixture,
    Arc<plan_support::Catalog>,
    Arc<quota_support::FakeQuotaCatalog>,
    Pg,
) {
    let pg = Pg::applied().await;
    let catalog = Arc::new(plan_support::Catalog::default());
    let f = plan_support::Fixture::on(
        DBProvider::new(pg.db().await),
        Uuid::new_v4(),
        plan_support::entry_support::TestDsn::of(pg.url(true)),
        catalog.clone(),
    )
    .await;
    let fake = quota_support::install(&f);
    (f, catalog, fake, pg)
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_quota_rule_endpoints_replace_replay_and_fence_on_postgres() {
    use quota_support::{draft_with_items, path, put, ram, rule_json, vcpu};
    use serde_json::json;
    let (f, catalog, fake, pg) = endpoints().await;
    let d = draft_with_items(&f, &catalog).await;
    let (cpu, ram) = (fake.add(vcpu(d.sku)), fake.add(ram(d.sku)));
    let rules = [
        rule_json(2, &ram, d.items[1], "20"),
        rule_json(1, &cpu, d.items[0], "10"),
    ];
    let (s, saved, etag) = put(&f, &d, &rules, "k1").await;
    assert_eq!(s, 200, "{saved}");
    let (s, read, read_etag) = f.call("GET", &path(&d), json!(null), None, None).await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(
        (read, read_etag),
        (saved.clone(), etag.clone()),
        "as stored"
    );
    let (_, revision, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(revision["quota_rules"], saved["quota_rules"]);
    assert_eq!(revision["quota_rules_digest"], saved["quota_rules_digest"]);
    let (s, replayed, replayed_etag) = put(&f, &d, &rules, "k1").await;
    assert_eq!(
        (s, replayed, replayed_etag),
        (200, saved.clone(), etag.clone()),
        "the stored answer"
    );
    let (s, stale, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "11")], "k2").await;
    assert_eq!(s, 409, "{stale}");
    assert!(stale.to_string().contains("STALE_REVISION"), "{stale}");
    let (s, refused, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", d.items[0]),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 409, "{refused}");
    assert!(
        refused.to_string().contains("ITEM_HAS_QUOTA_RULES"),
        "{refused}"
    );
    let cleared = quota_support::Draft { etag, ..d.clone() };
    let (s, empty, _) = put(&f, &cleared, &[], "k3").await;
    assert_eq!(s, 200, "{empty}");
    assert_eq!(empty["quota_rules_digest"], plan_quota_repo::empty_digest());
    let set_rows = pg
        .raw()
        .await
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT count(*)::bigint AS n FROM bss.pricing_plan_quota_set \
                 WHERE revision_id = {}",
                q(d.revision)
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(set_rows, 0, "a cleared set has no set row");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_checks_read_with_rules_on_postgres() {
    use quota_support::{disabled, priced_draft_with_items, put, rule_json, vcpu};
    use serde_json::json;
    let (f, catalog, fake, _pg) = endpoints().await;
    let ruled = priced_draft_with_items(&f, &catalog).await;
    let free = priced_draft_with_items(&f, &catalog).await;
    let pending = fake.add(disabled(vcpu(ruled.sku)));
    let (s, saved, etag) = put(
        &f,
        &ruled,
        &[rule_json(1, &pending, ruled.items[0], "10")],
        "k1",
    )
    .await;
    assert_eq!(s, 200, "{saved}");
    let checks = |revision: Uuid| {
        let f = &f;
        async move {
            let (s, body, _) = f
                .call(
                    "GET",
                    &format!("/plan-revisions/{revision}/checks"),
                    json!(null),
                    None,
                    None,
                )
                .await;
            assert_eq!(s, 200, "{body}");
            body
        }
    };
    let one = checks(ruled.revision).await;
    assert_eq!(one["ready"], false);
    assert_eq!(format!("\"{}\"", one["revision_version"]), etag);
    assert_eq!(one["quota_rules_digest"], saved["quota_rules_digest"]);
    let red = one["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "QUOTA_QUALIFICATION_UNAVAILABLE")
        .unwrap();
    assert_eq!(red["ok"], false, "{red}");
    assert_eq!(
        red["subjects"][0]["quota_rule_id"],
        Uuid::from_u128(1).to_string()
    );
    assert_eq!(red["subjects"][0]["field"], "definition_ref");
    let other = checks(free.revision).await;
    assert_eq!(
        other["ready"], true,
        "green but for the quota rows: {other}"
    );
    assert!(
        other["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| !c["code"].as_str().unwrap().starts_with("QUOTA_")),
        "{other}"
    );
    assert_eq!(other["quota_rules_digest"], plan_quota_repo::empty_digest());
    let (s, batch, _) = f
        .call(
            "GET",
            &format!(
                "/plan-revisions/checks?revision_ids={},{}",
                ruled.revision, free.revision
            ),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{batch}");
    assert_eq!(batch["items"][0]["checks"].to_string(), one.to_string());
    assert_eq!(batch["items"][1]["checks"].to_string(), other.to_string());
    assert_eq!(
        fake.requests().len(),
        3,
        "one validation for the PUT, one per read of the revision with rules"
    );
}
