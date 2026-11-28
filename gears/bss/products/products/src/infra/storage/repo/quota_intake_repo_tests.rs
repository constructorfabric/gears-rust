//! The quota intake store (P-D-268, P-D-269, P-D-274): targets per tenant, snapshots once per
//! revision and compared as numbers, candidates as written, one review write per seen version, and
//! a stored row that does not decode as a corrupt row.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::infra::storage::stored_now;
use crate::test_support::test_db;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use toolkit_db::secure::AccessScope;
use toolkit_odata::CursorV1;

const PLUGIN: &str = "plugin-instance";

pub struct RepoFixture {
    pub db: toolkit_db::DBProvider<toolkit_db::DbError>,
    pub scope: AccessScope,
    pub tenant: Uuid,
    pub actor: Uuid,
    pub now: OffsetDateTime,
    pub sku: Uuid,
    pub dsn: crate::test_support::TestDsn,
}

impl RepoFixture {
    pub async fn new() -> Self {
        let (db, scope, tenant, dsn) = test_db().await;
        Self::on(db, scope, tenant, dsn)
    }

    /// A fixture over a database whose statements `recorder` records.
    pub async fn recorded() -> (Self, toolkit_db::test_support::QueryRecorder) {
        let (db, scope, tenant, dsn, recorder) = crate::test_support::recorded_test_db().await;
        (Self::on(db, scope, tenant, dsn), recorder)
    }

    fn on(
        db: toolkit_db::DBProvider<toolkit_db::DbError>,
        scope: AccessScope,
        tenant: Uuid,
        dsn: crate::test_support::TestDsn,
    ) -> Self {
        Self {
            db,
            scope,
            tenant,
            actor: Uuid::new_v4(),
            now: stored_now(),
            sku: Uuid::new_v4(),
            dsn,
        }
    }

    pub fn conn(&self) -> toolkit_db::DbConn<'_> {
        self.db.conn().unwrap()
    }

    pub async fn target(&self, key: &str) -> QuotaTarget {
        insert_target(
            &self.conn(),
            &self.scope,
            self.tenant,
            NewQuotaTarget {
                plugin_instance_id: PLUGIN.to_owned(),
                target_key: key.to_owned(),
                name: format!("Pool {key}"),
            },
            self.actor,
            self.now,
        )
        .await
        .unwrap()
    }

    pub fn snapshot(&self, target: Uuid, revision: &str) -> StoredSnapshot {
        StoredSnapshot {
            id: Uuid::new_v4(),
            target_id: target,
            snapshot_id: Uuid::new_v4(),
            source_revision: revision.to_owned(),
            observed_at: self.now,
            item_count: 2,
            content_digest: "a".repeat(64),
            snapshot_json: json!({"source_revision": revision, "items": []}),
            evidence_json: json!({"registry": {}}),
            review_json: json!({"rejected": [], "drift": [], "retired": []}),
            created_by: self.actor,
            created_at: self.now,
        }
    }

    pub fn new_candidate(&self, target: Uuid, snapshot: Option<Uuid>, key: &str) -> NewCandidate {
        let record_id = Uuid::new_v4();
        NewCandidate {
            target_id: target,
            origin: if snapshot.is_some() {
                CandidateOrigin::Discovery
            } else {
                CandidateOrigin::Manual
            },
            snapshot_row_id: snapshot,
            record_id,
            capability_key: key.to_owned(),
            record_json: json!({"id": record_id.to_string(), "capability_key": key}),
            record_digest: "b".repeat(64),
            evidence_json: json!({"metric": {"digest": "c".repeat(64)}}),
            created_by: self.actor,
            created_at: self.now,
        }
    }

    /// A target and one manual candidate, `staged` at version 1.
    pub async fn staged_candidate(&self) -> QuotaCandidate {
        let target = self.target(&Uuid::new_v4().to_string()).await;
        let new = self.new_candidate(target.id, None, "tenant-vcpu");
        insert_candidates(&self.conn(), &self.scope, self.tenant, &[new])
            .await
            .unwrap()
            .remove(0)
    }

    pub async fn raw(&self, sql: &str) {
        let conn = sea_orm::Database::connect(&*self.dsn).await.unwrap();
        conn.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .unwrap();
        conn.close().await.unwrap();
    }
}

/// A UUID as the `SQLite` driver stores it: 16 bytes.
fn blob(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

fn submit(f: &RepoFixture) -> ReviewWrite {
    ReviewWrite::Submit {
        proposal: json!({"sku_id": f.sku}),
        evidence: json!({}),
        by: f.actor,
        at: f.now,
        note: None,
    }
}

#[tokio::test]
async fn a_target_reads_back_and_the_same_binding_is_taken() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    assert_eq!(
        (
            target.tenant_id,
            target.plugin_instance_id.as_str(),
            target.target_key.as_str()
        ),
        (f.tenant, PLUGIN, "pool-a")
    );
    assert_eq!(
        find_target(&f.conn(), &f.scope, f.tenant, target.id)
            .await
            .unwrap(),
        Some(target.clone())
    );
    let again = insert_target(
        &f.conn(),
        &f.scope,
        f.tenant,
        NewQuotaTarget {
            plugin_instance_id: PLUGIN.to_owned(),
            target_key: "pool-a".to_owned(),
            name: "Another name".to_owned(),
        },
        f.actor,
        f.now,
    )
    .await;
    assert!(
        matches!(
            again,
            Err(RepoError::Refused(RepoRefusal::QuotaTargetTaken))
        ),
        "{again:?}"
    );
    assert_eq!(RepoRefusal::QuotaTargetTaken.code(), "QUOTA_TARGET_TAKEN");
    f.target("pool-b").await;
}

#[tokio::test]
async fn another_tenant_reads_no_target() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    let other = Uuid::new_v4();
    let theirs = AccessScope::for_tenant(other);
    assert_eq!(
        find_target(&f.conn(), &theirs, other, target.id)
            .await
            .unwrap(),
        None
    );
    // The caller's tenant filter holds even under a scope that would admit the row.
    assert_eq!(
        find_target(&f.conn(), &f.scope, other, target.id)
            .await
            .unwrap(),
        None
    );
    let page = list_targets(&f.conn(), &theirs, other, &ODataQuery::default())
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn the_target_list_pages_by_name_then_id() {
    let f = RepoFixture::new().await;
    let mut names = Vec::new();
    for key in ["c", "a", "b"] {
        names.push(f.target(key).await.name);
    }
    names.sort();
    let first = list_targets(
        &f.conn(),
        &f.scope,
        f.tenant,
        &ODataQuery::default().with_limit(2),
    )
    .await
    .unwrap();
    let next = first.page_info.next_cursor.clone().unwrap();
    let second = list_targets(
        &f.conn(),
        &f.scope,
        f.tenant,
        &ODataQuery::default()
            .with_limit(2)
            .with_cursor(CursorV1::decode(&next).unwrap()),
    )
    .await
    .unwrap();
    let read: Vec<String> = first
        .items
        .into_iter()
        .chain(second.items)
        .map(|t| t.name)
        .collect();
    assert_eq!(read, names);
    assert!(second.page_info.next_cursor.is_none());
}

#[tokio::test]
async fn the_highest_revision_compares_as_a_number() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    assert_eq!(
        highest_revision(&f.conn(), &f.scope, f.tenant, target.id)
            .await
            .unwrap(),
        None
    );
    for revision in ["9", "10", "2"] {
        insert_snapshot(
            &f.conn(),
            &f.scope,
            f.tenant,
            &f.snapshot(target.id, revision),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        highest_revision(&f.conn(), &f.scope, f.tenant, target.id)
            .await
            .unwrap()
            .as_deref(),
        Some("10")
    );
    let wide = "1".repeat(39);
    insert_snapshot(&f.conn(), &f.scope, f.tenant, &f.snapshot(target.id, &wide))
        .await
        .unwrap();
    assert_eq!(
        highest_revision(&f.conn(), &f.scope, f.tenant, target.id)
            .await
            .unwrap(),
        Some(wide)
    );
    let other = f.target("pool-b").await;
    assert_eq!(
        highest_revision(&f.conn(), &f.scope, f.tenant, other.id)
            .await
            .unwrap(),
        None,
        "revisions are per target"
    );
}

/// The stale check reads the revision column only: a snapshot row whose JSON text another writer
/// broke does not stop it.
#[tokio::test]
async fn the_highest_revision_reads_only_the_revision() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    insert_snapshot(&f.conn(), &f.scope, f.tenant, &f.snapshot(target.id, "9"))
        .await
        .unwrap();
    f.raw(&format!(
        "INSERT INTO products_quota_snapshot (tenant_id, id, target_id, snapshot_id, \
         source_revision, observed_at, item_count, content_digest, snapshot_json, evidence_json, \
         review_json, created_by, created_at) VALUES ({}, {}, {}, {}, '12', \
         '2026-11-19T12:00:00Z', 1, '{}', '{{not json', '{{}}', '{{}}', {}, \
         '2026-11-19T12:00:00Z')",
        blob(f.tenant),
        blob(Uuid::new_v4()),
        blob(target.id),
        blob(Uuid::new_v4()),
        "a".repeat(64),
        blob(f.actor),
    ))
    .await;
    assert_eq!(
        highest_revision(&f.conn(), &f.scope, f.tenant, target.id)
            .await
            .unwrap()
            .as_deref(),
        Some("12")
    );
}

#[tokio::test]
async fn a_snapshot_and_its_candidates_read_back_as_written() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    let snapshot = f.snapshot(target.id, "7");
    insert_snapshot(&f.conn(), &f.scope, f.tenant, &snapshot)
        .await
        .unwrap();
    assert_eq!(
        snapshot_at(&f.conn(), &f.scope, f.tenant, target.id, "7")
            .await
            .unwrap(),
        Some(snapshot.clone())
    );
    assert_eq!(
        snapshot_at(&f.conn(), &f.scope, f.tenant, target.id, "8")
            .await
            .unwrap(),
        None
    );
    let new = [
        f.new_candidate(target.id, Some(snapshot.id), "tenant-vcpu"),
        f.new_candidate(target.id, Some(snapshot.id), "tenant-ram"),
    ];
    let written = insert_candidates(&f.conn(), &f.scope, f.tenant, &new)
        .await
        .unwrap();
    assert_eq!(written.len(), 2);
    for (candidate, given) in written.iter().zip(&new) {
        assert_eq!(
            (
                candidate.state,
                candidate.version,
                candidate.origin,
                &candidate.record_json,
                &candidate.evidence_json
            ),
            (
                CandidateState::Staged,
                1,
                CandidateOrigin::Discovery,
                &given.record_json,
                &given.evidence_json
            )
        );
    }
    let mut by_key = written.clone();
    by_key.sort_by(|a, b| a.capability_key.cmp(&b.capability_key));
    assert_eq!(
        candidates_of_snapshot(&f.conn(), &f.scope, f.tenant, snapshot.id)
            .await
            .unwrap(),
        by_key,
        "the stored candidates read back exactly as the insert answered them"
    );
    for candidate in &written {
        assert_eq!(
            find_candidate(&f.conn(), &f.scope, f.tenant, candidate.id)
                .await
                .unwrap()
                .as_ref(),
            Some(candidate)
        );
    }
}

#[tokio::test]
async fn a_second_snapshot_at_a_revision_is_refused_as_a_conflict() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    insert_snapshot(&f.conn(), &f.scope, f.tenant, &f.snapshot(target.id, "7"))
        .await
        .unwrap();
    let mut again = f.snapshot(target.id, "7");
    again.content_digest = "e".repeat(64);
    let refused = insert_snapshot(&f.conn(), &f.scope, f.tenant, &again).await;
    assert!(
        matches!(
            refused,
            Err(RepoError::Refused(RepoRefusal::QuotaSnapshotTaken))
        ),
        "{refused:?}"
    );
    assert_eq!(
        RepoRefusal::QuotaSnapshotTaken.code(),
        "QUOTA_SOURCE_CONFLICT"
    );
    let other = f.target("pool-b").await;
    insert_snapshot(&f.conn(), &f.scope, f.tenant, &f.snapshot(other.id, "7"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_manual_record_is_one_candidate_per_target() {
    let f = RepoFixture::new().await;
    let candidate = f.staged_candidate().await;
    assert_eq!(
        manual_candidate(
            &f.conn(),
            &f.scope,
            f.tenant,
            candidate.target_id,
            candidate.record_id
        )
        .await
        .unwrap()
        .as_ref(),
        Some(&candidate)
    );
    let mut again = f.new_candidate(candidate.target_id, None, "tenant-vcpu");
    again.record_id = candidate.record_id;
    let refused = insert_candidates(&f.conn(), &f.scope, f.tenant, &[again]).await;
    assert!(
        matches!(
            refused,
            Err(RepoError::Refused(RepoRefusal::QuotaManualRecordTaken))
        ),
        "{refused:?}"
    );
    assert_eq!(
        RepoRefusal::QuotaManualRecordTaken.code(),
        "QUOTA_SOURCE_CONFLICT"
    );
}

#[tokio::test]
async fn a_review_write_moves_once_per_seen_version() {
    let f = RepoFixture::new().await;
    let c = f.staged_candidate().await;
    assert!(
        review_candidate(&f.conn(), &f.scope, f.tenant, c.id, c.version, submit(&f))
            .await
            .unwrap()
    );
    assert!(
        !review_candidate(&f.conn(), &f.scope, f.tenant, c.id, c.version, submit(&f))
            .await
            .unwrap()
    );
    let read = find_candidate(&f.conn(), &f.scope, f.tenant, c.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (read.state, read.version),
        (CandidateState::Submitted, c.version + 1)
    );
    assert_eq!(read.proposal_json, Some(json!({"sku_id": f.sku})));
    assert_eq!(
        (read.submitted_by, read.submitted_at),
        (Some(f.actor), Some(f.now))
    );
    // A decision on the version read before the submit moves nothing, though the state allows it.
    let reject = || ReviewWrite::Reject {
        by: Uuid::new_v4(),
        at: f.now,
        note: "no".to_owned(),
    };
    assert!(
        !review_candidate(&f.conn(), &f.scope, f.tenant, c.id, c.version, reject())
            .await
            .unwrap()
    );
    assert!(
        review_candidate(&f.conn(), &f.scope, f.tenant, c.id, read.version, reject())
            .await
            .unwrap()
    );
}

/// Each review write moves only from its own state: a submit of a submitted candidate, or an
/// approval of a staged one, moves nothing even at the current version.
#[tokio::test]
async fn a_review_write_moves_only_from_its_own_state() {
    let f = RepoFixture::new().await;
    let c = f.staged_candidate().await;
    let definition = Uuid::new_v4();
    let approve = |note: Option<&str>| ReviewWrite::Approve {
        by: Uuid::new_v4(),
        at: f.now,
        note: note.map(str::to_owned),
        definition_id: definition,
        definition_version: QuotaVersion::new(1).unwrap(),
    };
    assert!(
        !review_candidate(&f.conn(), &f.scope, f.tenant, c.id, 1, approve(None))
            .await
            .unwrap(),
        "a staged candidate is not approved"
    );
    assert!(
        review_candidate(&f.conn(), &f.scope, f.tenant, c.id, 1, submit(&f))
            .await
            .unwrap()
    );
    assert!(
        !review_candidate(&f.conn(), &f.scope, f.tenant, c.id, 2, submit(&f))
            .await
            .unwrap(),
        "a submitted candidate is not submitted again"
    );
    assert!(
        review_candidate(&f.conn(), &f.scope, f.tenant, c.id, 2, approve(Some("ok")))
            .await
            .unwrap()
    );
    let reject = ReviewWrite::Reject {
        by: Uuid::new_v4(),
        at: f.now,
        note: "late".to_owned(),
    };
    assert!(
        !review_candidate(&f.conn(), &f.scope, f.tenant, c.id, 3, reject)
            .await
            .unwrap(),
        "an approved candidate is not rejected"
    );
    let read = find_candidate(&f.conn(), &f.scope, f.tenant, c.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            read.state,
            read.version,
            read.definition_id,
            read.definition_version,
            read.decided_note.as_deref()
        ),
        (
            CandidateState::Approved,
            3,
            Some(definition),
            Some(QuotaVersion::new(1).unwrap()),
            Some("ok")
        )
    );
    let other = Uuid::new_v4();
    assert!(
        !review_candidate(
            &f.conn(),
            &AccessScope::for_tenant(other),
            other,
            c.id,
            3,
            submit(&f)
        )
        .await
        .unwrap(),
        "another tenant moves nothing"
    );
}

#[tokio::test]
async fn a_stored_candidate_that_does_not_decode_is_corrupt() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    let id = Uuid::new_v4();
    f.raw(&format!(
        "INSERT INTO products_quota_candidate (tenant_id, id, target_id, origin, record_id, \
         capability_key, record_json, record_digest, evidence_json, state, version, created_by, \
         created_at) VALUES ({}, {}, {}, 'manual', {}, 'k', '[\"not\", \"a record\"]', \
         '{}', '{{}}', 'staged', 1, {}, '2026-11-19T12:00:00Z')",
        blob(f.tenant),
        blob(id),
        blob(target.id),
        blob(Uuid::new_v4()),
        "d".repeat(64),
        blob(f.actor),
    ))
    .await;
    let named = |e: RepoError| {
        let text = e.to_string();
        assert!(matches!(e, RepoError::CorruptRow(_)), "{text}");
        for part in [
            "products_quota_candidate".to_owned(),
            f.tenant.to_string(),
            id.to_string(),
            "record_json".to_owned(),
        ] {
            assert!(text.contains(&part), "{part} in {text}");
        }
    };
    named(
        find_candidate(&f.conn(), &f.scope, f.tenant, id)
            .await
            .unwrap_err(),
    );
    match list_candidates(
        &f.conn(),
        &f.scope,
        f.tenant,
        &CandidateFilter::default(),
        &ODataQuery::default(),
    )
    .await
    {
        Err(SkuListError::Repo(e)) => named(e),
        other => panic!("the list must not skip the row: {other:?}"),
    }
}

#[tokio::test]
async fn the_candidate_list_is_newest_first_and_narrows_by_target_and_state() {
    let f = RepoFixture::new().await;
    let a = f.target("pool-a").await;
    let b = f.target("pool-b").await;
    let mut written = Vec::new();
    for (n, target) in [a.id, b.id, a.id].into_iter().enumerate() {
        let mut new = f.new_candidate(target, None, "k");
        new.created_at = f.now + time::Duration::seconds(i64::try_from(n).unwrap());
        written.extend(
            insert_candidates(&f.conn(), &f.scope, f.tenant, &[new])
                .await
                .unwrap(),
        );
    }
    let ids = |page: Page<QuotaCandidate>| page.items.into_iter().map(|c| c.id).collect::<Vec<_>>();
    let list = |filter: CandidateFilter| {
        let f = &f;
        async move {
            list_candidates(
                &f.conn(),
                &f.scope,
                f.tenant,
                &filter,
                &ODataQuery::default(),
            )
            .await
            .unwrap()
        }
    };
    assert_eq!(
        ids(list(CandidateFilter::default()).await),
        vec![written[2].id, written[1].id, written[0].id]
    );
    assert_eq!(
        ids(list(CandidateFilter {
            target_id: Some(a.id),
            state: None
        })
        .await),
        vec![written[2].id, written[0].id]
    );
    assert!(
        review_candidate(&f.conn(), &f.scope, f.tenant, written[0].id, 1, submit(&f))
            .await
            .unwrap()
    );
    assert_eq!(
        ids(list(CandidateFilter {
            target_id: None,
            state: Some(CandidateState::Submitted)
        })
        .await),
        vec![written[0].id]
    );
}

/// A cursor walks the candidate list page by page, newest first, the tie-break by id included,
/// alone and narrowed by target.
#[tokio::test]
async fn the_candidate_list_walks_its_cursor_across_pages() {
    let f = RepoFixture::new().await;
    let a = f.target("pool-a").await;
    let b = f.target("pool-b").await;
    let mut written = Vec::new();
    for (n, (target, second)) in [
        (a.id, 0),
        (b.id, 1),
        (a.id, 2),
        (a.id, 2),
        (b.id, 3),
        (a.id, 4),
    ]
    .into_iter()
    .enumerate()
    {
        let mut new = f.new_candidate(target, None, &format!("k{n}"));
        new.created_at = f.now + time::Duration::seconds(second);
        written.extend(
            insert_candidates(&f.conn(), &f.scope, f.tenant, &[new])
                .await
                .unwrap(),
        );
    }
    let walk = |filter: CandidateFilter| {
        let f = &f;
        async move {
            let mut read = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let mut query = ODataQuery::default().with_limit(2);
                if let Some(next) = cursor {
                    query = query.with_cursor(CursorV1::decode(&next).unwrap());
                }
                let page = list_candidates(&f.conn(), &f.scope, f.tenant, &filter, &query)
                    .await
                    .unwrap();
                assert!(page.items.len() <= 2);
                read.extend(page.items.into_iter().map(|c| c.id));
                match page.page_info.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break read,
                }
            }
        }
    };
    let mut newest_first = written.clone();
    newest_first.sort_by_key(|c| std::cmp::Reverse((c.created_at, c.id)));
    assert_eq!(
        walk(CandidateFilter::default()).await,
        newest_first.iter().map(|c| c.id).collect::<Vec<_>>()
    );
    assert_eq!(
        walk(CandidateFilter {
            target_id: Some(a.id),
            state: None
        })
        .await,
        newest_first
            .iter()
            .filter(|c| c.target_id == a.id)
            .map(|c| c.id)
            .collect::<Vec<_>>()
    );
}

/// A JSON column the store would not read back is refused at the write, never stored.
#[tokio::test]
async fn a_json_column_that_is_not_an_object_is_never_written() {
    let f = RepoFixture::new().await;
    let target = f.target("pool-a").await;
    let mut snapshot = f.snapshot(target.id, "7");
    snapshot.review_json = json!([]);
    assert!(matches!(
        insert_snapshot(&f.conn(), &f.scope, f.tenant, &snapshot).await,
        Err(RepoError::Db(_))
    ));
    let mut candidate = f.new_candidate(target.id, None, "k");
    candidate.record_json = json!("text");
    assert!(matches!(
        insert_candidates(&f.conn(), &f.scope, f.tenant, &[candidate]).await,
        Err(RepoError::Db(_))
    ));
    assert_eq!(
        highest_revision(&f.conn(), &f.scope, f.tenant, target.id)
            .await
            .unwrap(),
        None
    );
}
