//! The Deletion store on an in-memory `SQLite` database: the mark, the
//! finish that clears one subject's rows and moves its generation on, the
//! list of stopped erasures, and retention's removal by age.

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit_db::DBProvider;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::deletion_repo::DeletionTables;
use super::record_ids_repo::SeaOrmRecordIdRepository;
use super::review_requests_repo::ReviewRequestStore;
use crate::domain::deletion::{DeletionStore, ExpiredRows, ProfileGenerations};
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::record_intake::{IdentityParts, RecordId, RecordIdRepository, RecordKey};
use crate::domain::review_queue::{NewReviewRequest, ReviewRequests};
use crate::domain::subject_settings::{SettingsWrite, SubjectKey, SubjectSettings};
use crate::test_support::{CoveredLookup, inmem_db, settings_service};

struct Tables {
    db: toolkit_db::Db,
    store: DeletionTables,
    reviews: ReviewRequestStore,
}

/// Finish the erasure under way for `key`, as the run that read its mark.
async fn finish_marked(t: &Tables, key: SubjectKey) -> bool {
    let marked = t
        .store
        .marked(&tenant_scope(key), key)
        .await
        .unwrap()
        .map_or(OffsetDateTime::UNIX_EPOCH, |mark| mark.at);
    t.store
        .finish(&tenant_scope(key), key, marked)
        .await
        .unwrap()
}

async fn tables() -> Tables {
    let db = inmem_db().await;
    let provider = Arc::new(DBProvider::new(db.clone()));
    Tables {
        db,
        store: DeletionTables::new(Arc::clone(&provider)),
        reviews: ReviewRequestStore::new(provider),
    }
}

fn subject() -> SubjectKey {
    SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    }
}

fn tenant_scope(key: SubjectKey) -> AccessScope {
    AccessScope::for_tenant(key.tenant_id)
}

impl Tables {
    async fn settings(&self, key: SubjectKey) -> Option<SubjectSettings> {
        use crate::domain::subject_settings::SubjectSettingsRepository;
        use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;
        let conn = self.db.conn().unwrap();
        SeaOrmSubjectSettingsRepository::new()
            .find(&conn, &tenant_scope(key), key)
            .await
            .unwrap()
            .stored()
    }

    async fn add_request(&self, key: SubjectKey, created_at: OffsetDateTime) {
        self.reviews
            .open(
                &tenant_scope(key),
                NewReviewRequest {
                    id: Uuid::new_v4(),
                    key,
                    fact_id: NodeKey(format!("construct:{}:{}", key.tenant_id, Uuid::new_v4())),
                    category: Category::Skill,
                    property: "skills".to_owned(),
                    value: json!({ "name": "Rust" }),
                    comment: None,
                    created_at,
                },
            )
            .await
            .unwrap();
    }

    /// A request of `key`'s created at `created_at`, closed since.
    async fn add_closed_request(&self, key: SubjectKey, created_at: OffsetDateTime) {
        use crate::domain::review_queue::{Closed, ReviewOutcome};
        self.add_request(key, created_at).await;
        let open = self
            .reviews
            .of_subject(&tenant_scope(key), key)
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.is_open() && request.created_at == created_at)
            .expect("the request just opened");
        let closed = Closed {
            outcome: ReviewOutcome::Deleted,
            by: Uuid::new_v4(),
            at: OffsetDateTime::now_utc(),
            reason: None,
        };
        assert!(
            self.reviews
                .close_open(&tenant_scope(key), open.key(), None, &closed)
                .await
                .unwrap()
                .is_some()
        );
    }

    async fn requests(&self, key: SubjectKey) -> usize {
        self.reviews
            .of_subject(&tenant_scope(key), key)
            .await
            .unwrap()
            .len()
    }

    async fn add_identity(&self, key: SubjectKey) {
        let conn = self.db.conn().unwrap();
        SeaOrmRecordIdRepository::new()
            .insert(
                &conn,
                &tenant_scope(key),
                RecordId {
                    tenant_id: key.tenant_id,
                    key: RecordKey::of(IdentityParts {
                        connector: "connector",
                        provenance: &Uuid::new_v4().to_string(),
                        version: "v1",
                    }),
                    connector: "connector".to_owned(),
                    subject_id: Some(key.subject_id),
                },
            )
            .await
            .unwrap();
    }

    async fn identities(&self) -> usize {
        crate::test_support::stored_record_ids(&self.db).await
    }
}

#[tokio::test]
async fn a_mark_turns_personalization_off_and_marks_the_erasure_on_a_new_row() {
    let t = tables().await;
    let key = subject();
    let at = OffsetDateTime::now_utc();

    let written = t.store.mark(&tenant_scope(key), key, at).await.unwrap();

    assert_eq!(written, SettingsWrite::Written);
    assert_eq!(
        t.settings(key).await,
        Some(SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: true,
        })
    );
    let marked = t.store.marked(&tenant_scope(key), key).await.unwrap();
    assert_eq!(
        marked.map(|mark| mark.at.unix_timestamp()),
        Some(at.unix_timestamp())
    );
    assert_eq!(t.store.generation(key).await.unwrap(), 0);
}

#[tokio::test]
async fn a_second_mark_keeps_the_first_time_and_turns_personalization_off_again() {
    let t = tables().await;
    let key = subject();
    let first = OffsetDateTime::now_utc() - Duration::minutes(5);
    t.store.mark(&tenant_scope(key), key, first).await.unwrap();
    // The subject turns personalization on while the erasure is under way.
    settings_service(&t.db, Arc::new(crate::test_support::AllowResolver), true)
        .set_personalization(
            &crate::test_support::context_of(key.subject_id, key.tenant_id),
            key.subject_id,
            true,
        )
        .await
        .unwrap();

    t.store
        .mark(&tenant_scope(key), key, OffsetDateTime::now_utc())
        .await
        .unwrap();

    let marked = t.store.marked(&tenant_scope(key), key).await.unwrap();
    assert_eq!(
        marked.map(|mark| mark.at.unix_timestamp()),
        Some(first.unix_timestamp()),
        "a second request does not put the end off"
    );
    assert_eq!(
        t.settings(key).await.map(|s| s.personalization_enabled),
        Some(false)
    );
}

#[tokio::test]
async fn a_mark_the_scope_leaves_out_is_refused_and_writes_nothing() {
    let t = tables().await;
    let key = subject();
    let other = AccessScope::for_tenant(Uuid::new_v4());

    let written = t
        .store
        .mark(&other, key, OffsetDateTime::now_utc())
        .await
        .unwrap();

    assert!(
        matches!(written, SettingsWrite::OutOfScope(_)),
        "{written:?}"
    );
    assert_eq!(t.settings(key).await, None);
}

#[tokio::test]
async fn the_finish_clears_the_mark_moves_the_generation_on_and_removes_only_the_subjects_rows() {
    let t = tables().await;
    let key = subject();
    let neighbour = SubjectKey {
        tenant_id: key.tenant_id,
        subject_id: Uuid::new_v4(),
    };
    let now = OffsetDateTime::now_utc();
    t.add_request(key, now).await;
    t.add_request(neighbour, now).await;
    t.add_identity(key).await;
    t.add_identity(neighbour).await;
    t.store.mark(&tenant_scope(key), key, now).await.unwrap();

    assert!(finish_marked(&t, key).await);

    assert_eq!(t.requests(key).await, 0);
    assert_eq!(t.requests(neighbour).await, 1, "another subject keeps its");
    assert_eq!(
        t.identities().await,
        1,
        "only the neighbour's identity is left"
    );
    assert_eq!(t.store.marked(&tenant_scope(key), key).await.unwrap(), None);
    assert_eq!(t.store.generation(key).await.unwrap(), 1);
    assert_eq!(
        t.settings(key).await,
        Some(SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: false,
        }),
        "only the setting, off, is kept"
    );
}

#[tokio::test]
async fn a_finish_for_another_mark_leaves_the_erasure_under_way_and_removes_nothing() {
    let t = tables().await;
    let key = subject();
    let now = OffsetDateTime::now_utc();
    t.add_request(key, now).await;
    t.add_identity(key).await;
    t.store.mark(&tenant_scope(key), key, now).await.unwrap();

    // A run of an earlier erasure, which another run already finished.
    let earlier = now - Duration::minutes(10);
    assert!(
        !t.store
            .finish(&tenant_scope(key), key, earlier)
            .await
            .unwrap()
    );

    assert_eq!(t.requests(key).await, 1);
    assert_eq!(t.identities().await, 1);
    assert_eq!(t.store.generation(key).await.unwrap(), 0);
    assert!(
        t.store
            .marked(&tenant_scope(key), key)
            .await
            .unwrap()
            .is_some(),
        "the erasure under way stays marked"
    );
}

#[tokio::test]
async fn a_finish_without_a_mark_removes_nothing() {
    let t = tables().await;
    let key = subject();
    t.add_request(key, OffsetDateTime::now_utc()).await;
    t.add_identity(key).await;

    assert!(!finish_marked(&t, key).await);

    assert_eq!(t.requests(key).await, 1);
    assert_eq!(t.identities().await, 1);
    assert_eq!(t.store.generation(key).await.unwrap(), 0);
}

#[tokio::test]
async fn each_erasure_moves_the_generation_on_once_more() {
    let t = tables().await;
    let key = subject();
    for expected in 1..=2 {
        t.store
            .mark(&tenant_scope(key), key, OffsetDateTime::now_utc())
            .await
            .unwrap();
        assert!(finish_marked(&t, key).await);
        assert_eq!(t.store.generation(key).await.unwrap(), expected);
    }
}

#[tokio::test]
async fn the_stopped_erasures_are_those_marked_before_the_cut_of_any_tenant() {
    let t = tables().await;
    let (old, other_tenant, fresh, done) = (subject(), subject(), subject(), subject());
    let now = OffsetDateTime::now_utc();
    for (key, at) in [
        (old, now - Duration::hours(2)),
        (other_tenant, now - Duration::hours(1)),
        (fresh, now),
        (done, now - Duration::hours(3)),
    ] {
        t.store.mark(&tenant_scope(key), key, at).await.unwrap();
    }
    finish_marked(&t, done).await;

    let stopped = t
        .store
        .under_way(now - Duration::minutes(2), 10)
        .await
        .unwrap();
    assert_eq!(
        stopped,
        [old, other_tenant],
        "oldest first, the fresh one left"
    );

    let one = t
        .store
        .under_way(now - Duration::minutes(2), 1)
        .await
        .unwrap();
    assert_eq!(one, [old]);
}

#[tokio::test]
async fn a_tried_erasure_goes_behind_the_untried_and_a_finish_forgets_the_attempt() {
    let t = tables().await;
    let (tried, untried, never_marked) = (subject(), subject(), subject());
    let now = OffsetDateTime::now_utc();
    t.store
        .mark(&tenant_scope(tried), tried, now - Duration::hours(2))
        .await
        .unwrap();
    t.store
        .mark(&tenant_scope(untried), untried, now - Duration::hours(1))
        .await
        .unwrap();
    let cut = now - Duration::minutes(2);
    assert_eq!(
        t.store.under_way(cut, 10).await.unwrap(),
        [tried, untried],
        "by mark while neither was tried"
    );

    t.store
        .attempted(tried, now - Duration::minutes(30))
        .await
        .unwrap();
    // A subject with no erasure under way is not touched.
    t.store.attempted(never_marked, now).await.unwrap();

    assert_eq!(t.store.under_way(cut, 10).await.unwrap(), [untried, tried]);
    assert_eq!(t.store.under_way(cut, 1).await.unwrap(), [untried]);
    assert_eq!(t.settings(never_marked).await, None);

    // A finish clears the attempt with the mark, so a later erasure starts
    // in order of its own mark.
    assert!(finish_marked(&t, tried).await);
    t.store
        .mark(&tenant_scope(tried), tried, now - Duration::hours(3))
        .await
        .unwrap();
    assert_eq!(t.store.under_way(cut, 10).await.unwrap(), [tried, untried]);
}

#[tokio::test]
async fn retention_removes_the_tenants_rows_from_before_the_cut_and_no_other() {
    let t = tables().await;
    let key = subject();
    let other = subject();
    let now = OffsetDateTime::now_utc();
    t.add_closed_request(key, now - Duration::days(40)).await;
    t.add_closed_request(key, now).await;
    t.add_closed_request(other, now - Duration::days(40)).await;
    t.add_identity(key).await;
    t.add_identity(other).await;

    let kept = t
        .store
        .expire(&tenant_scope(key), key.tenant_id, now - Duration::days(30))
        .await
        .unwrap();
    assert_eq!(
        kept,
        ExpiredRows {
            review_requests: 1,
            record_ids: 0,
        },
        "an identity received now is within the period"
    );
    assert_eq!(t.requests(key).await, 1);

    let removed = t
        .store
        .expire(
            &tenant_scope(key),
            key.tenant_id,
            OffsetDateTime::now_utc() + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(
        removed,
        ExpiredRows {
            review_requests: 1,
            record_ids: 1,
        }
    );
    assert_eq!(t.requests(other).await, 1, "another tenant keeps its rows");
    assert_eq!(t.identities().await, 1);
}

#[tokio::test]
async fn retention_leaves_an_open_request_to_the_removal_of_its_fact() {
    let t = tables().await;
    let key = subject();
    let now = OffsetDateTime::now_utc();
    t.add_request(key, now - Duration::days(40)).await;

    let removed = t
        .store
        .expire(&tenant_scope(key), key.tenant_id, now - Duration::days(30))
        .await
        .unwrap();

    assert_eq!(removed.review_requests, 0);
    assert_eq!(
        t.requests(key).await,
        1,
        "the removal of its fact closes it, with its event; the next run removes it"
    );
}

#[tokio::test]
async fn retention_removes_more_identities_than_one_batch_holds_batch_by_batch() {
    let t = tables().await;
    let key = subject();
    let count = crate::infra::storage::deletion_repo::EXPIRE_BATCH * 2 + 1;
    for _ in 0..count {
        t.add_identity(key).await;
    }

    let removed = t
        .store
        .expire(
            &tenant_scope(key),
            key.tenant_id,
            OffsetDateTime::now_utc() + Duration::seconds(1),
        )
        .await
        .unwrap();

    assert_eq!(
        removed.record_ids, count,
        "every batch, the short last one too"
    );
    assert_eq!(t.identities().await, 0);
}

#[tokio::test]
async fn the_tenants_are_those_with_a_row_in_any_table() {
    let t = tables().await;
    let (settings, identity, request) = (subject(), subject(), subject());
    t.store
        .mark(&tenant_scope(settings), settings, OffsetDateTime::now_utc())
        .await
        .unwrap();
    t.add_identity(identity).await;
    t.add_request(request, OffsetDateTime::now_utc()).await;

    let tenants = t.store.tenants().await.unwrap();

    assert_eq!(
        tenants,
        [settings.tenant_id, identity.tenant_id, request.tenant_id]
            .into_iter()
            .collect()
    );
}

#[tokio::test]
async fn the_generation_is_read_only_in_the_subjects_tenant() {
    let t = tables().await;
    let key = subject();
    t.store
        .mark(&tenant_scope(key), key, OffsetDateTime::now_utc())
        .await
        .unwrap();
    finish_marked(&t, key).await;
    let elsewhere = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: key.subject_id,
    };

    assert_eq!(t.store.generation(key).await.unwrap(), 1);
    assert_eq!(
        t.store.generation(elsewhere).await.unwrap(),
        0,
        "the same subject in another tenant is not erased"
    );
}

#[tokio::test]
async fn an_erasure_marked_before_its_time_was_kept_counts_as_marked_long_ago() {
    let t = tables().await;
    let key = subject();
    crate::test_support::insert_subject_settings(&t.db, key, false, Some(true))
        .await
        .unwrap();

    assert_eq!(
        t.store
            .marked(&tenant_scope(key), key)
            .await
            .unwrap()
            .map(|mark| mark.at),
        Some(OffsetDateTime::UNIX_EPOCH)
    );
    assert_eq!(
        t.store
            .under_way(OffsetDateTime::now_utc() - Duration::minutes(2), 10)
            .await
            .unwrap(),
        [key],
        "the leader's job finds it"
    );
}

#[tokio::test]
async fn a_mark_on_a_subject_with_settings_turns_personalization_off_and_takes_the_time() {
    let t = tables().await;
    let key = subject();
    crate::test_support::insert_subject_settings(&t.db, key, true, None)
        .await
        .unwrap();
    let at = OffsetDateTime::now_utc();

    t.store.mark(&tenant_scope(key), key, at).await.unwrap();

    assert_eq!(
        t.settings(key).await,
        Some(SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: true,
        })
    );
    let marked = t.store.marked(&tenant_scope(key), key).await.unwrap();
    assert_eq!(
        marked.map(|mark| mark.at.unix_timestamp()),
        Some(at.unix_timestamp())
    );
}

#[tokio::test]
async fn a_tenant_stays_known_after_retention_removed_its_rows() {
    let t = tables().await;
    let key = subject();
    t.add_identity(key).await;
    assert!(t.store.tenants().await.unwrap().contains(&key.tenant_id));

    t.store
        .expire(
            &tenant_scope(key),
            key.tenant_id,
            OffsetDateTime::now_utc() + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(t.identities().await, 0);

    assert!(
        t.store.tenants().await.unwrap().contains(&key.tenant_id),
        "its facts in graph storage are still swept"
    );
}

#[tokio::test]
async fn a_noted_tenant_is_among_the_tenants_and_noting_it_again_changes_nothing() {
    use crate::domain::deletion::KnownTenants as _;

    let t = tables().await;
    let tenant = Uuid::new_v4();

    t.store.note(tenant).await.unwrap();
    t.store.note(tenant).await.unwrap();

    assert_eq!(t.store.tenants().await.unwrap(), [tenant].into());
}
