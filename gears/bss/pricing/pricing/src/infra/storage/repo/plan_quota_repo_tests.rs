//! The quota rule repository on SQLite (D-543, D-544, D-545): a replace writes the set at the
//! version the caller read and moves the revision to its next version, a stale, locked or
//! published revision writes nothing, a cleared set has no set row, a damaged set reads as a
//! corrupt row, many revisions' sets read in two statements, a copy moves each rule onto its
//! copied item, and the item delete's check sees one item's rules. The Postgres twins are in
//! `tests/postgres_plan_quota_rules.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::infra::storage::entity::{
    plan as plan_e, plan_item, plan_quota_rule, plan_revision, price_book,
};
use crate::infra::storage::repo::{
    approval_repo::PricingApprovalStore, book_repo, plan_item_repo, plan_repo, plan_revision_repo,
    price_repo,
};
use crate::test_support::{TestDsn, at, test_db};
use bss_approval::{Store, Unit, UnitState};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{ConnectOpts, DBProvider, DbConn, DbError};

fn now() -> time::OffsetDateTime {
    at(10)
}

/// One tenant's plans on one database file.
struct Draft {
    db: DBProvider<DbError>,
    scope_: AccessScope,
    tenant: Uuid,
    user: Uuid,
    book: Uuid,
    /// The first plan's draft revision, at version 1 until a test moves it.
    revision: Uuid,
    /// Two items of `revision`.
    items: Vec<Uuid>,
    /// One draft revision per plan, for the reads of many.
    revisions: Vec<Uuid>,
    /// Held for the test's life: its directory holds the database.
    dsn: TestDsn,
}

impl Draft {
    fn conn(&self) -> DbConn<'_> {
        self.db.conn().unwrap()
    }

    fn scope(&self) -> AccessScope {
        self.scope_.clone()
    }

    /// A plan with a draft revision and `items` items without an entry; answers the revision and
    /// its items.
    async fn plan_with_draft(&self, code: &str, items: usize) -> (Uuid, Vec<Uuid>) {
        let conn = self.conn();
        let plan = plan_repo::insert(
            &conn,
            &self.scope_,
            plan_e::Model {
                id: Uuid::new_v4(),
                tenant_id: self.tenant,
                code: code.into(),
                name: code.into(),
                published_rev: None,
                version: 1,
                created_by: self.user,
                created_at: at(9),
                updated_at: at(9),
                work_revision_id: None,
                work_state: None,
                scheduled_revision_id: None,
                scheduled_from: None,
                published_revision_id: None,
                current_book_id: None,
                current_currency: None,
                last_activity_at: at(9),
            },
        )
        .await
        .unwrap();
        let revision = plan_revision_repo::insert(
            &conn,
            &self.scope_,
            plan_revision::Model {
                id: Uuid::new_v4(),
                tenant_id: self.tenant,
                plan_id: plan.id,
                rev_no: 1,
                book_id: self.book,
                state: "draft".into(),
                available_from: None,
                pending_unit_id: None,
                approved_by_unit_id: None,
                published_at: None,
                version: 1,
                created_by: self.user,
                created_at: at(9),
                updated_at: at(9),
            },
        )
        .await
        .unwrap();
        let mut ids = Vec::new();
        for _ in 0..items {
            let item = plan_item_repo::insert(
                &conn,
                &self.scope_,
                plan_item::Model {
                    id: Uuid::new_v4(),
                    tenant_id: self.tenant,
                    revision_id: revision.id,
                    sku_id: Uuid::new_v4(),
                    price_book_entry_id: None,
                    treatment: "included".into(),
                    included_qty: None,
                    qty_min: None,
                    reservation_id: None,
                    reference_state: "unreserved".into(),
                    version: 1,
                    created_by: self.user,
                    created_at: at(9),
                    updated_at: at(9),
                },
            )
            .await
            .unwrap();
            ids.push(item.id);
        }
        (revision.id, ids)
    }

    /// A draft of another plan with two items: a copy's target.
    async fn second_draft_with_items(&self) -> (Uuid, Vec<Uuid>) {
        self.plan_with_draft(&format!("copy-{}", Uuid::new_v4()), 2)
            .await
    }

    /// Lock the first revision for a fresh approval unit, as a submit does: its version moves to 2.
    async fn lock(&self) {
        lock_at(self, 1).await;
    }

    /// One revision's set, read in a transaction of its own (one snapshot).
    async fn set(&self, revision: Uuid) -> Result<StoredSet, RepoError> {
        let (scope, tenant) = (self.scope(), self.tenant);
        price_repo::transaction(&self.db.db(), move |tx| {
            let scope = scope.clone();
            Box::pin(async move { for_revision(tx, &scope, tenant, revision).await })
        })
        .await
    }

    /// The sets of `revisions`, read in a transaction of its own (one snapshot).
    async fn sets(&self, revisions: &[Uuid]) -> Result<BTreeMap<Uuid, StoredSet>, RepoError> {
        let (scope, tenant, revisions) = (self.scope(), self.tenant, revisions.to_vec());
        price_repo::transaction(&self.db.db(), move |tx| {
            let (scope, revisions) = (scope.clone(), revisions.clone());
            Box::pin(async move { for_revisions(tx, &scope, tenant, &revisions).await })
        })
        .await
    }

    /// Raw SQL on the database file, past every repository: the damage a test needs.
    async fn raw(&self, sql: &str) {
        use sea_orm::ConnectionTrait;
        sea_orm::Database::connect(String::from(&self.dsn))
            .await
            .unwrap()
            .execute_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                sql.to_owned(),
            ))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    /// The number of set rows `revision` holds, counted past every repository.
    async fn set_rows(&self, revision: Uuid) -> i64 {
        use sea_orm::ConnectionTrait;
        sea_orm::Database::connect(String::from(&self.dsn))
            .await
            .unwrap()
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                format!(
                    "SELECT count(*) AS n FROM pricing_plan_quota_set WHERE revision_id = {}",
                    x(revision)
                ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap()
    }

    /// The version of `revision` and its plan's last activity.
    async fn version_and_activity(&self, revision: Uuid) -> (i64, time::OffsetDateTime) {
        let row = plan_revision_repo::find(&self.conn(), &self.scope_, self.tenant, revision)
            .await
            .unwrap()
            .unwrap();
        let plan = plan_repo::find(&self.conn(), &self.scope_, self.tenant, row.plan_id)
            .await
            .unwrap()
            .unwrap();
        (row.version, plan.last_activity_at)
    }
}

/// A uuid as SQLite stores it: a blob.
fn x(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

/// The empty set's digest, pinned: sha256 of
/// `{"domain":"pricing.quota-rules.v1","payload":{"rules":[]}}`.
const EMPTY_SET_DIGEST: &str = "599cdcc359ef3e8e6f8a32068b381eb4db89aa0d2fd0e702ed2935a793dd1d5a";

/// A tenant with a book on `db`, and a first plan whose draft has two items.
async fn draft_on(db: DBProvider<DbError>, tenant: Uuid, dsn: TestDsn) -> Draft {
    let scope = AccessScope::for_tenant(tenant);
    let book = book_repo::insert(
        &db.conn().unwrap(),
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
            created_at: at(9),
            updated_at: at(9),
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    let mut f = Draft {
        db,
        scope_: scope,
        tenant,
        user: Uuid::new_v4(),
        book: book.id,
        revision: Uuid::nil(),
        items: Vec::new(),
        revisions: Vec::new(),
        dsn,
    };
    let (revision, items) = f.plan_with_draft("pro", 2).await;
    f.revision = revision;
    f.items = items;
    f.revisions.push(revision);
    f
}

/// One plan whose draft revision (version 1) has two items.
async fn draft() -> Draft {
    let (db, _, tenant, dsn) = test_db().await;
    draft_on(db, tenant, dsn).await
}

/// `count` plans on a recorded database, each a draft with one ruled item.
async fn recorded_drafts(count: usize) -> (Draft, toolkit_db::test_support::QueryRecorder) {
    let dsn = TestDsn::new("pricing-quota-recorded-");
    let (db, recorder) = toolkit_db::test_support::connect_with_recorder(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    let mut f = draft_on(DBProvider::new(db), Uuid::new_v4(), dsn).await;
    f.revisions.clear();
    for n in 0..count {
        let (revision, items) = f.plan_with_draft(&format!("plan-{n}"), 1).await;
        let rules = vec![rule_on(&f, revision, 1, items[0], "10")];
        replace(
            &f.conn(),
            &f.scope(),
            f.tenant,
            revision,
            1,
            rules,
            "c".repeat(64),
            f.user,
            now(),
        )
        .await
        .unwrap();
        f.revisions.push(revision);
    }
    (f, recorder)
}

/// A complete rule row of the first revision.
fn row(f: &Draft, n: u128, item: Uuid, amount: &str) -> plan_quota_rule::Model {
    rule_on(f, f.revision, n, item, amount)
}

/// A complete rule row of `revision`, in the fixture's tenant.
fn rule_on(f: &Draft, revision: Uuid, n: u128, item: Uuid, amount: &str) -> plan_quota_rule::Model {
    plan_quota_rule::Model {
        revision_id: revision,
        rule_id: Uuid::from_u128(n),
        tenant_id: f.tenant,
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

#[tokio::test]
async fn replace_writes_the_set_and_bumps_the_revision_version() {
    let f = draft().await;
    let rules = vec![row(&f, 2, f.items[1], "20"), row(&f, 1, f.items[0], "10")];
    let version = replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        rules.clone(),
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(version, 2);
    let revision = plan_revision_repo::find(&f.conn(), &f.scope(), f.tenant, f.revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((revision.version, revision.updated_at), (2, now()));
    // The bump moved the revision's `updated_at`, so the plan's last activity follows (D-484).
    assert_eq!(f.version_and_activity(f.revision).await, (2, now()));
    let stored = f.set(f.revision).await.unwrap();
    assert_eq!(stored.digest, "a".repeat(64));
    assert_eq!(
        stored.rules.iter().map(|r| r.rule_id).collect::<Vec<_>>(),
        [Uuid::from_u128(1), Uuid::from_u128(2)]
    );
    assert_eq!(stored.rules[0], rules[1], "a rule reads back as written");
    assert_eq!(f.sets(&[f.revision]).await.unwrap()[&f.revision], stored);
}

/// D-546: a replace with no rules deletes the set row, so "no set row" and "no rules" mean the
/// same and both reads answer the empty set's digest.
#[tokio::test]
async fn clearing_a_set_deletes_its_set_row() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(f.set_rows(f.revision).await, 1);
    let cleared = replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        2,
        Vec::new(),
        empty_digest(),
        f.user,
        at(11),
    )
    .await
    .unwrap();
    assert_eq!(cleared, 3);
    assert_eq!(f.set_rows(f.revision).await, 0, "no set row");
    assert_eq!(
        f.set(f.revision).await.unwrap(),
        StoredSet {
            digest: EMPTY_SET_DIGEST.to_owned(),
            rules: Vec::new()
        }
    );
    assert!(f.sets(&[f.revision]).await.unwrap().is_empty());
    assert_eq!(f.version_and_activity(f.revision).await, (3, at(11)));
    // A second clear of a set that has no set row moves the version and writes nothing else.
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        3,
        Vec::new(),
        empty_digest(),
        f.user,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(f.set_rows(f.revision).await, 0);
}

#[tokio::test]
async fn a_revision_without_a_set_reads_the_empty_set() {
    let f = draft().await;
    assert_eq!(empty_digest(), EMPTY_SET_DIGEST);
    assert_eq!(f.set_rows(f.revision).await, 0);
    assert_eq!(
        f.set(f.revision).await.unwrap(),
        StoredSet {
            digest: EMPTY_SET_DIGEST.to_owned(),
            rules: Vec::new()
        }
    );
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    // Another tenant's scope reads none of it.
    let other = AccessScope::for_tenant(Uuid::new_v4());
    let tenant = f.tenant;
    let revision = f.revision;
    let foreign = price_repo::transaction(&f.db.db(), move |tx| {
        let other = other.clone();
        Box::pin(async move { for_revision(tx, &other, tenant, revision).await })
    })
    .await
    .unwrap();
    assert_eq!(foreign.digest, EMPTY_SET_DIGEST);
    assert!(foreign.rules.is_empty());
}

#[tokio::test]
async fn replace_at_a_stale_version_writes_nothing() {
    let f = draft().await;
    let error = replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        7,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            RepoError::Conflict {
                code: "STALE_REVISION"
            }
        ),
        "{error:?}"
    );
    let stored = f.set(f.revision).await.unwrap();
    assert_eq!((stored.rules.len(), stored.digest), (0, empty_digest()));
    assert_eq!(f.set_rows(f.revision).await, 0);
}

#[tokio::test]
async fn replace_on_a_locked_revision_writes_nothing() {
    let f = draft().await;
    f.lock().await;
    let error = replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        2,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            RepoError::Conflict {
                code: "STALE_REVISION"
            }
        ),
        "{error:?}"
    );
    assert!(f.set(f.revision).await.unwrap().rules.is_empty());
}

/// A revision that is not a draft refuses a replace at its own version: `STALE_REVISION`, and
/// nothing is written.
#[tokio::test]
async fn replace_on_a_published_revision_is_stale_revision() {
    for state in ["published", "scheduled", "superseded"] {
        let f = draft().await;
        f.raw(&format!(
            "UPDATE pricing_plan_revision SET state = '{state}', available_from = '2026-09-26' \
             WHERE id = {}",
            x(f.revision)
        ))
        .await;
        let before = f.version_and_activity(f.revision).await;
        let error = replace(
            &f.conn(),
            &f.scope(),
            f.tenant,
            f.revision,
            1,
            vec![row(&f, 1, f.items[0], "10")],
            "a".repeat(64),
            f.user,
            now(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                RepoError::Conflict {
                    code: "STALE_REVISION"
                }
            ),
            "{state}: {error:?}"
        );
        assert_eq!(f.version_and_activity(f.revision).await, before, "{state}");
        assert!(f.set(f.revision).await.unwrap().rules.is_empty(), "{state}");
        assert_eq!(f.set_rows(f.revision).await, 0, "{state}");
    }
}

/// A row of another revision or tenant, or an empty set with a digest that is not the empty set's,
/// is refused before anything is written: the version does not move.
#[tokio::test]
async fn replace_refuses_a_rule_of_another_revision_and_writes_nothing() {
    let f = draft().await;
    let (other_revision, other_items) = f.second_draft_with_items().await;
    let mut foreign_tenant = row(&f, 2, f.items[1], "20");
    foreign_tenant.tenant_id = Uuid::new_v4();
    for (rules, digest, what) in [
        (
            vec![
                row(&f, 1, f.items[0], "10"),
                rule_on(&f, other_revision, 2, other_items[0], "20"),
            ],
            "a".repeat(64),
            "a rule of another revision",
        ),
        (
            vec![row(&f, 1, f.items[0], "10"), foreign_tenant],
            "a".repeat(64),
            "a rule of another tenant",
        ),
        (
            Vec::new(),
            "a".repeat(64),
            "an empty set with another digest",
        ),
    ] {
        let error = replace(
            &f.conn(),
            &f.scope(),
            f.tenant,
            f.revision,
            1,
            rules,
            digest,
            f.user,
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RepoError::Db(_)), "{what}: {error:?}");
        assert_eq!(f.version_and_activity(f.revision).await.0, 1, "{what}");
        assert!(f.set(f.revision).await.unwrap().rules.is_empty(), "{what}");
        assert_eq!(f.set_rows(f.revision).await, 0, "{what}");
    }
}

/// A set that disagrees with its rows is a corrupt row (500), never a wrong digest (D-541, D-546).
#[tokio::test]
async fn a_rule_without_a_set_row_is_a_corrupt_row() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    f.raw(&format!(
        "DELETE FROM pricing_plan_quota_set WHERE revision_id = {}",
        x(f.revision)
    ))
    .await;
    let one = f.set(f.revision).await.unwrap_err();
    assert!(matches!(one, RepoError::CorruptRow(_)), "{one:?}");
    let many = f.sets(&[f.revision]).await.unwrap_err();
    assert!(matches!(many, RepoError::CorruptRow(_)), "{many:?}");
}

#[tokio::test]
async fn a_set_count_that_disagrees_is_a_corrupt_row() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10"), row(&f, 2, f.items[1], "20")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    f.raw(&format!(
        "UPDATE pricing_plan_quota_set SET rule_count = 1 WHERE revision_id = {}",
        x(f.revision)
    ))
    .await;
    let one = f.set(f.revision).await.unwrap_err();
    assert!(matches!(one, RepoError::CorruptRow(_)), "{one:?}");
    let many = f.sets(&[f.revision]).await.unwrap_err();
    assert!(matches!(many, RepoError::CorruptRow(_)), "{many:?}");
}

/// A set row that names no rule (the batch read's left-over row) is a corrupt row too.
#[tokio::test]
async fn a_set_row_without_rules_is_a_corrupt_row() {
    let f = draft().await;
    f.raw(&format!(
        "INSERT INTO pricing_plan_quota_set \
         (revision_id,tenant_id,quota_rules_digest,rule_count,updated_by,updated_at) \
         VALUES ({},{},'{}',1,{},'2026-09-26T10:00:00Z')",
        x(f.revision),
        x(f.tenant),
        "a".repeat(64),
        x(f.user)
    ))
    .await;
    let one = f.set(f.revision).await.unwrap_err();
    assert!(matches!(one, RepoError::CorruptRow(_)), "{one:?}");
    let many = f.sets(&[f.revision]).await.unwrap_err();
    assert!(matches!(many, RepoError::CorruptRow(_)), "{many:?}");
}

#[tokio::test]
async fn the_sets_of_many_revisions_read_in_two_statements() {
    let (f, recorder) = recorded_drafts(3).await;
    let quota = |recorder: &toolkit_db::test_support::QueryRecorder| {
        recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_plan_quota"))
            })
            .collect::<Vec<_>>()
    };
    recorder.clear();
    let sets = f.sets(&f.revisions).await.unwrap();
    assert_eq!(sets.len(), 3);
    let read = quota(&recorder);
    assert_eq!(read.len(), 2, "{read:?}");
    assert!(
        read.iter().all(|q| q.in_tx),
        "both in the caller's transaction"
    );
    for revision in &f.revisions {
        let set = &sets[revision];
        assert_eq!(set.digest, "c".repeat(64));
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].revision_id, *revision);
    }
    // A revision without rules is absent, and the statements do not grow with the list.
    let mut more = f.revisions.clone();
    more.push(f.revision);
    recorder.clear();
    let sets = f.sets(&more).await.unwrap();
    assert_eq!(sets.len(), 3);
    assert!(!sets.contains_key(&f.revision));
    assert_eq!(quota(&recorder).len(), 2);
    recorder.clear();
    assert!(f.sets(&[]).await.unwrap().is_empty());
    assert_eq!(quota(&recorder).len(), 2, "two statements for none too");
}

#[tokio::test]
async fn copy_moves_each_rule_onto_its_copied_item() {
    let f = draft().await;
    let source = row(&f, 1, f.items[0], "10");
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![source.clone()],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    let (target, copied) = f.second_draft_with_items().await;
    let items = BTreeMap::from([(f.items[0], copied[0]), (f.items[1], copied[1])]);
    copy(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        target,
        &items,
        "b".repeat(64),
        f.user,
        at(11),
    )
    .await
    .unwrap();
    let stored = f.set(target).await.unwrap();
    assert_eq!(
        (stored.rules[0].rule_id, stored.rules[0].item_id),
        (Uuid::from_u128(1), copied[0])
    );
    assert_eq!(stored.digest, "b".repeat(64));
    assert_eq!(
        stored.rules[0],
        plan_quota_rule::Model {
            revision_id: target,
            item_id: copied[0],
            created_at: at(11),
            ..source
        },
        "the definition, evidence and observation are copied as stored"
    );
    // The copy does not move the target's version, and the source keeps its set.
    assert_eq!(f.version_and_activity(target).await.0, 1);
    assert_eq!(f.set(f.revision).await.unwrap().rules.len(), 1);
}

#[tokio::test]
async fn copy_refuses_a_rule_whose_item_was_not_copied() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[1], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    let (target, copied) = f.second_draft_with_items().await;
    let error = copy(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        target,
        &BTreeMap::from([(f.items[0], copied[0])]),
        "b".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, RepoError::CorruptRow(_)), "{error:?}");
}

#[tokio::test]
async fn a_quota_free_copy_writes_no_set_row() {
    let f = draft().await;
    let (target, copied) = f.second_draft_with_items().await;
    copy(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        target,
        &BTreeMap::from([(f.items[0], copied[0]), (f.items[1], copied[1])]),
        empty_digest(),
        f.user,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        f.set_rows(target).await,
        0,
        "no set row, not even a zero-count one"
    );
    assert_eq!(f.set(target).await.unwrap().digest, EMPTY_SET_DIGEST);
}

#[tokio::test]
async fn item_has_rules_answers_for_one_item() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    assert!(
        item_has_rules(&f.conn(), &f.scope(), f.tenant, f.items[0])
            .await
            .unwrap()
    );
    assert!(
        !item_has_rules(&f.conn(), &f.scope(), f.tenant, f.items[1])
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn delete_for_draft_removes_the_rules_and_the_set_row() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    delete_for_draft(&f.conn(), &f.scope(), f.tenant, f.revision)
        .await
        .unwrap();
    assert_eq!(f.set_rows(f.revision).await, 0);
    assert!(f.set(f.revision).await.unwrap().rules.is_empty());
    assert!(
        !item_has_rules(&f.conn(), &f.scope(), f.tenant, f.items[0])
            .await
            .unwrap()
    );
    // The items go next, as the draft delete orders them: nothing names them any more.
    plan_item_repo::delete_draft(&f.conn(), &f.scope(), f.tenant, f.items[0], 1)
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_for_draft_leaves_a_locked_revisions_rules() {
    let f = draft().await;
    replace(
        &f.conn(),
        &f.scope(),
        f.tenant,
        f.revision,
        1,
        vec![row(&f, 1, f.items[0], "10")],
        "a".repeat(64),
        f.user,
        now(),
    )
    .await
    .unwrap();
    // The replace moved the revision to version 2; the lock takes it at that version.
    lock_at(&f, 2).await;
    delete_for_draft(&f.conn(), &f.scope(), f.tenant, f.revision)
        .await
        .unwrap();
    assert_eq!(
        f.set(f.revision).await.unwrap().rules.len(),
        1,
        "a locked revision's rules stay"
    );
    assert_eq!(f.set_rows(f.revision).await, 1);
}

/// [`Draft::lock`] at `version`.
async fn lock_at(f: &Draft, version: i64) {
    let unit = Uuid::new_v4();
    let (scope, tenant) = (f.scope(), f.tenant);
    price_repo::transaction(&f.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(
                tx,
                &Unit {
                    id: unit,
                    tenant_id: tenant,
                    kind: "plan_revision".into(),
                    ref_type: "plan_revision".into(),
                    ref_id: Uuid::new_v4(),
                    state: UnitState::Pending,
                    common_effective_date: None,
                    quorum_required: 1,
                    generation: 1,
                    submitted_by: Uuid::new_v4(),
                    submitted_at: at(9),
                    submit_note: None,
                    decided_at: None,
                    decided_note: None,
                    snapshot: serde_json::json!({}),
                    snapshot_hash: "hash".into(),
                    version: 1,
                },
                &[],
            )
            .await
            .map_err(|e| RepoError::Db(e.to_string()))
        })
    })
    .await
    .unwrap();
    assert!(
        plan_revision_repo::try_lock(&f.conn(), &f.scope(), tenant, f.revision, unit, version)
            .await
            .unwrap()
    );
}
