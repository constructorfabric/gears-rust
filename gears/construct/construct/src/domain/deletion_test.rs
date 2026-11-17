//! Deletion over an in-memory profile, writer and store: erasure and
//! retention.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::deletion::{
    Deletion, DeletionEvent, DeletionPorts, ERASURE_WAIT, Erasure, ExpiredRows, MAX_PAGES,
    ProfileDirectory, RootRemoval, SubjectsPage,
};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, Origin, Plan};
use crate::domain::planner::processing::{PlanWriter, Written};
use crate::domain::profile::Category;
use crate::domain::profile_writes::ProfileWrites;
use crate::domain::review_queue::{ReviewOutcome, ReviewQueueEvent, ReviewStatus};
use crate::domain::subject_settings::SubjectKey;
use crate::test_support::{
    FixedDirectory, MemoryDeletionStore, MemoryProfile, MemoryReviews, MemoryWriter,
    RecordingDeletionEvents, RecordingReviewEvents, ScriptedRoots, profile_writes, stored_fact,
};

/// How long an erasure waits after its mark.
const WINDOW: Duration = ERASURE_WAIT;

struct Fixture {
    key: SubjectKey,
    identity: Uuid,
    profile: Arc<MemoryProfile>,
    writer: Arc<MemoryWriter>,
    roots: Arc<ScriptedRoots>,
    store: Arc<MemoryDeletionStore>,
    reviews: Arc<MemoryReviews>,
    review_events: Arc<RecordingReviewEvents>,
    events: Arc<RecordingDeletionEvents>,
}

impl Fixture {
    /// A subject with a skill stored at second 10 and a role stored now.
    fn new() -> Self {
        let key = SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        };
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let profile = MemoryProfile::with(vec![
            stored_fact(Category::Skill, "skills", json!({ "name": "Rust" }), 10),
            stored_fact(Category::Role, "roles", json!({ "title": "Engineer" }), now),
        ]);
        let writer = MemoryWriter::new(Arc::clone(&profile), 0);
        Self {
            key,
            identity: Uuid::new_v4(),
            profile,
            writer,
            roots: Arc::default(),
            store: Arc::default(),
            reviews: Arc::default(),
            review_events: Arc::default(),
            events: Arc::default(),
        }
    }

    /// The same subject with a writer that conflicts `conflicts` times.
    fn conflicting(mut self, conflicts: u32) -> Self {
        self.writer = MemoryWriter::new(Arc::clone(&self.profile), conflicts);
        self
    }

    fn deletion(&self, directory: FixedDirectory) -> Deletion {
        self.deletion_writing(
            profile_writes(
                Arc::clone(&self.profile),
                Arc::clone(&self.writer),
                Some(self.identity),
            ),
            directory,
        )
    }

    fn deletion_writing(&self, writes: ProfileWrites, directory: FixedDirectory) -> Deletion {
        Deletion::new(DeletionPorts {
            writes: Arc::new(writes),
            roots: Arc::clone(&self.roots) as _,
            directory: Arc::new(directory),
            store: Arc::clone(&self.store) as _,
            reviews: Arc::clone(&self.reviews) as _,
            review_events: Arc::clone(&self.review_events) as _,
            events: Arc::clone(&self.events) as _,
        })
    }

    /// The erasure, marked long enough ago that it does not wait.
    fn marked_long_ago(&self) {
        self.store
            .mark_at(self.key, OffsetDateTime::now_utc() - WINDOW * 2);
    }
}

#[tokio::test]
async fn an_erasure_removes_every_fact_and_the_root_then_finishes_and_says_so() {
    let fx = Fixture::new();
    fx.marked_long_ago();

    let done = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap();

    assert_eq!(done, Erasure::Done);
    assert!(fx.profile.live().is_empty(), "every fact is removed");
    let [plan] = fx.writer.plans().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Subject {
            subject: fx.key.subject_id
        }
    );
    assert_eq!(plan.steps.len(), 2, "one removal per fact, in one plan");
    assert!(
        plan.steps
            .iter()
            .all(|step| matches!(step.change, Change::RemoveEntity { .. }))
    );
    assert_eq!(
        fx.roots.calls.load(Ordering::SeqCst),
        1,
        "the root is removed once, after the wait"
    );
    assert_eq!(fx.store.finished(), [fx.key], "the tables are cleared last");
    assert!(!fx.store.is_marked(fx.key), "the mark is cleared");
    assert_eq!(
        fx.events.taken(),
        [DeletionEvent::Erased {
            tenant_id: fx.key.tenant_id,
            subject_id: fx.key.subject_id,
        }]
    );
}

#[tokio::test]
async fn no_erasure_under_way_changes_nothing() {
    let fx = Fixture::new();

    let done = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap();

    assert_eq!(done, Erasure::NotUnderWay);
    assert_eq!(fx.profile.live().len(), 2);
    assert!(fx.writer.plans().is_empty());
    assert_eq!(fx.roots.calls.load(Ordering::SeqCst), 0);
    assert!(fx.store.finished().is_empty());
    assert!(fx.events.taken().is_empty());
}

#[tokio::test]
async fn a_fact_written_after_the_removal_leaves_the_erasure_marked_for_the_next_run() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    fx.roots
        .answers
        .lock()
        .unwrap()
        .push_back(Ok(RootRemoval::FactsLeft(1)));

    let err = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::Unavailable(_)), "{err:?}");
    assert!(fx.store.is_marked(fx.key), "the erasure stays marked");
    assert!(
        fx.store.finished().is_empty(),
        "nothing in the tables is removed"
    );
    assert!(fx.events.taken().is_empty());
}

#[tokio::test]
async fn an_interrupted_erasure_completes_when_it_runs_again() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    fx.roots
        .answers
        .lock()
        .unwrap()
        .push_back(Err(DomainError::Unavailable(
            "graph storage is down".to_owned(),
        )));
    let deletion = fx.deletion(FixedDirectory::default());

    deletion.erase(fx.key).await.unwrap_err();
    assert!(
        fx.profile.live().is_empty(),
        "the facts went before the stop"
    );
    assert!(fx.store.is_marked(fx.key));

    assert_eq!(deletion.erase(fx.key).await.unwrap(), Erasure::Done);
    assert_eq!(
        fx.writer.plans().len(),
        1,
        "no fact was left to remove again"
    );
    assert_eq!(
        fx.roots.calls.load(Ordering::SeqCst),
        2,
        "once in the stopped run, once in the next"
    );
    assert_eq!(fx.store.finished(), [fx.key]);
}

#[tokio::test]
async fn an_erasure_without_a_processing_identity_is_not_configured_and_stays_marked() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    let deletion = Deletion::new(DeletionPorts {
        writes: Arc::new(profile_writes(
            Arc::clone(&fx.profile),
            Arc::clone(&fx.writer),
            None,
        )),
        roots: Arc::clone(&fx.roots) as _,
        directory: Arc::new(FixedDirectory::default()),
        store: Arc::clone(&fx.store) as _,
        reviews: Arc::clone(&fx.reviews) as _,
        review_events: Arc::clone(&fx.review_events) as _,
        events: Arc::clone(&fx.events) as _,
    });

    let err = deletion.erase(fx.key).await.unwrap_err();

    assert!(matches!(err, DomainError::NotConfigured(_)), "{err:?}");
    assert!(fx.store.is_marked(fx.key));
    assert_eq!(fx.profile.live().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_erasure_removes_the_tables_rows_only_once_the_receive_window_after_its_mark_is_over() {
    let fx = Fixture::new();
    fx.store.mark_at(fx.key, OffsetDateTime::now_utc());
    let deletion = Arc::new(fx.deletion(FixedDirectory::default()));

    let running = tokio::spawn({
        let deletion = Arc::clone(&deletion);
        let key = fx.key;
        async move { deletion.erase(key).await }
    });
    tokio::time::sleep(WINDOW.saturating_sub(Duration::from_secs(5))).await;
    assert!(fx.profile.live().is_empty(), "graph storage goes at once");
    assert!(
        fx.store.finished().is_empty(),
        "a record checked before the mark may still keep its identity"
    );

    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(running.await.unwrap().unwrap(), Erasure::Done);
    assert_eq!(fx.store.finished(), [fx.key]);
}

#[tokio::test]
async fn retention_removes_only_the_facts_past_the_period_and_closes_their_open_requests() {
    let fx = Fixture::new();
    let [old, new] = fx.profile.live().try_into().unwrap();
    fx.reviews.open_on(fx.key, &old);
    fx.reviews.open_on(fx.key, &new);
    let directory = FixedDirectory {
        pages: vec![SubjectsPage {
            subjects: vec![fx.key.subject_id],
            next: None,
        }],
        ..FixedDirectory::default()
    };

    let started = OffsetDateTime::now_utc();
    fx.deletion(directory)
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    assert_eq!(
        fx.profile.live(),
        std::slice::from_ref(&new),
        "the fact within the period stays"
    );
    let [plan] = fx.writer.plans().try_into().unwrap();
    assert_eq!(plan.origin, Origin::Retention);
    let [(tenant, before)] = fx.store.expired.lock().unwrap().clone().try_into().unwrap();
    assert_eq!(tenant, fx.key.tenant_id);
    let day = time::Duration::days(1);
    assert!(
        before >= started - day && before <= OffsetDateTime::now_utc() - day,
        "the rows are cut a period before now: {before}"
    );
    let requests = fx.reviews.all();
    let on_old = requests
        .iter()
        .find(|request| request.fact_id == old.node_key)
        .unwrap();
    let ReviewStatus::Closed(closed) = &on_old.status else {
        panic!("the removed fact's request closes: {on_old:?}");
    };
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    assert_eq!(closed.by, fx.identity, "Construct closes it");
    let [ReviewQueueEvent::Resolved { .. }] = fx.review_events.taken().as_slice() else {
        panic!("one resolved event, got {:?}", fx.review_events.taken());
    };
    let on_new = requests
        .iter()
        .find(|request| request.fact_id == new.node_key)
        .unwrap();
    assert!(
        matches!(on_new.status, ReviewStatus::Open { .. }),
        "{on_new:?}"
    );
    assert_eq!(
        fx.events.taken(),
        [DeletionEvent::FactsExpired {
            tenant_id: fx.key.tenant_id,
            subject_id: fx.key.subject_id,
            facts: 1,
        }]
    );
}

#[tokio::test]
async fn retention_with_nothing_past_the_period_writes_nothing() {
    let fx = Fixture::new();
    let directory = FixedDirectory {
        pages: vec![SubjectsPage {
            subjects: vec![fx.key.subject_id],
            next: None,
        }],
        ..FixedDirectory::default()
    };

    fx.deletion(directory)
        .expire(fx.key.tenant_id, Duration::from_hours(24 * 365 * 100))
        .await
        .unwrap();

    assert_eq!(fx.profile.live().len(), 2);
    assert!(fx.writer.plans().is_empty());
    assert!(fx.events.taken().is_empty(), "no event for nothing removed");
}

#[tokio::test]
async fn retention_walks_every_page_of_subjects() {
    let fx = Fixture::new();
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let directory = FixedDirectory {
        pages: vec![
            SubjectsPage {
                subjects: vec![first],
                next: Some("1".to_owned()),
            },
            SubjectsPage {
                subjects: vec![second],
                next: None,
            },
        ],
        ..FixedDirectory::default()
    };
    let deletion = fx.deletion(directory);

    deletion
        .expire(fx.key.tenant_id, Duration::from_secs(60))
        .await
        .unwrap();

    let read: Vec<Uuid> = fx
        .profile
        .reads
        .lock()
        .unwrap()
        .iter()
        .map(|(_, key)| key.subject_id)
        .collect();
    assert_eq!(
        read,
        [first, second],
        "each subject read once, page by page"
    );
}

/// One page listing `subjects`.
fn listing(subjects: Vec<Uuid>) -> FixedDirectory {
    FixedDirectory {
        pages: vec![SubjectsPage {
            subjects,
            next: None,
        }],
        ..FixedDirectory::default()
    }
}

#[tokio::test]
async fn a_listing_that_hands_back_the_same_page_stops_retention_of_the_tenant() {
    let fx = Fixture::new();
    let page = |subject| SubjectsPage {
        subjects: vec![subject],
        next: Some("1".to_owned()),
    };
    let directory = FixedDirectory {
        pages: vec![page(Uuid::new_v4()), page(Uuid::new_v4())],
        ..FixedDirectory::default()
    };

    let err = fx
        .deletion(directory)
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, DomainError::Internal(message) if message.contains("same page")),
        "{err:?}"
    );
    assert_eq!(
        fx.profile.reads.lock().unwrap().len(),
        2,
        "both pages swept once"
    );
}

#[tokio::test]
async fn a_listing_that_comes_back_to_an_earlier_page_stops_retention_of_the_tenant() {
    let fx = Fixture::new();
    let page = |next: &str| SubjectsPage {
        subjects: vec![Uuid::new_v4()],
        next: Some(next.to_owned()),
    };
    let directory = FixedDirectory {
        pages: vec![page("1"), page("2"), page("1"), page("2")],
        ..FixedDirectory::default()
    };

    let err = fx
        .deletion(directory)
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, DomainError::Internal(message) if message.contains("same page")),
        "{err:?}"
    );
    assert_eq!(
        fx.profile.reads.lock().unwrap().len(),
        3,
        "stopped where the listing came back, not at the page limit"
    );
}

/// A listing whose every page names a next one.
#[derive(Default)]
struct EndlessDirectory {
    asked: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl ProfileDirectory for EndlessDirectory {
    async fn subjects(
        &self,
        _: &toolkit_security::SecurityContext,
        _: Option<String>,
    ) -> Result<SubjectsPage, DomainError> {
        let page = self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(SubjectsPage {
            subjects: Vec::new(),
            next: Some((page + 1).to_string()),
        })
    }
}

#[tokio::test]
async fn a_listing_that_never_ends_stops_at_the_page_limit_and_says_so() {
    let fx = Fixture::new();
    let directory = Arc::new(EndlessDirectory::default());
    let deletion = Deletion::new(DeletionPorts {
        writes: Arc::new(profile_writes(
            Arc::clone(&fx.profile),
            Arc::clone(&fx.writer),
            Some(fx.identity),
        )),
        roots: Arc::clone(&fx.roots) as _,
        directory: Arc::clone(&directory) as _,
        store: Arc::clone(&fx.store) as _,
        reviews: Arc::clone(&fx.reviews) as _,
        review_events: Arc::clone(&fx.review_events) as _,
        events: Arc::clone(&fx.events) as _,
    });

    let err = deletion
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap_err();

    assert_eq!(directory.asked.load(Ordering::SeqCst), MAX_PAGES);
    let DomainError::Internal(message) = err else {
        panic!("a listing that does not end is a defect, got {err:?}");
    };
    assert!(message.contains("at its page limit"), "{message}");
}

/// A writer that fails every write after it may have landed.
struct FailingWriter;

#[async_trait]
impl PlanWriter for FailingWriter {
    async fn write_by(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &Plan,
        _: tokio::time::Instant,
    ) -> Result<Written, DomainError> {
        Err(DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

#[tokio::test]
async fn an_erasure_whose_write_conflicts_reads_the_profile_again_and_removes_it_all() {
    let fx = Fixture::new().conflicting(1);
    fx.marked_long_ago();

    let done = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap();

    assert_eq!(done, Erasure::Done);
    assert!(fx.profile.live().is_empty());
    let [first, second] = fx.writer.plans().try_into().unwrap();
    assert_eq!(first.profile_version, Some(1));
    assert_eq!(
        second.profile_version,
        Some(2),
        "the second plan is built on the profile as the conflict left it"
    );
}

#[tokio::test]
async fn an_erasure_whose_write_may_have_landed_is_unavailable_and_stays_marked() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    let writes = ProfileWrites::new(
        Arc::clone(&fx.profile) as _,
        Arc::new(FailingWriter),
        Arc::new(crate::infra::processing_contexts::ServiceIdentity::new(
            fx.identity,
        )),
        2,
    );

    let err = fx
        .deletion_writing(writes, FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap_err();

    let DomainError::Unavailable(message) = &err else {
        panic!("unavailable, got {err:?}");
    };
    assert!(message.contains("may have landed"), "{message}");
    assert!(fx.store.is_marked(fx.key));
    assert_eq!(fx.roots.calls.load(Ordering::SeqCst), 0, "the root stays");
}

/// A writer graph storage refuses.
struct RefusedWriter;

#[async_trait]
impl PlanWriter for RefusedWriter {
    async fn write_by(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &Plan,
        _: tokio::time::Instant,
    ) -> Result<Written, DomainError> {
        Err(DomainError::internal("graph storage refused the write"))
    }
}

#[tokio::test]
async fn an_erasure_whose_write_is_refused_fails_as_refused_not_as_unavailable() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    let writes = ProfileWrites::new(
        Arc::clone(&fx.profile) as _,
        Arc::new(RefusedWriter),
        Arc::new(crate::infra::processing_contexts::ServiceIdentity::new(
            fx.identity,
        )),
        2,
    );

    let err = fx
        .deletion_writing(writes, FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap_err();

    assert!(
        matches!(&err, DomainError::Internal(message) if message.contains("refused")),
        "the refusal is passed on, so the erasure is logged as stuck: {err:?}"
    );
    assert!(fx.store.is_marked(fx.key));
}

#[tokio::test]
async fn an_erasure_another_run_finished_meanwhile_is_not_under_way_and_says_nothing() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    fx.store.finish_lost.store(true, Ordering::SeqCst);

    let done = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap();

    assert_eq!(done, Erasure::NotUnderWay);
    assert!(fx.events.taken().is_empty(), "the other run recorded it");
}

#[tokio::test]
async fn retention_whose_write_conflicts_decides_again_on_the_new_profile() {
    let fx = Fixture::new().conflicting(1);

    fx.deletion(listing(vec![fx.key.subject_id]))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    assert_eq!(fx.profile.live().len(), 1, "only the old fact is removed");
    let [first, second] = fx.writer.plans().try_into().unwrap();
    assert_eq!(first.origin, Origin::Retention);
    assert_eq!(
        (first.steps.len(), second.profile_version),
        (1, Some(2)),
        "the same choice, made again on the new version"
    );
}

#[tokio::test]
async fn retention_records_the_rows_it_removed_and_nothing_when_there_were_none() {
    let fx = Fixture::new();
    *fx.store.expired_rows.lock().unwrap() = ExpiredRows {
        review_requests: 2,
        record_ids: 3,
    };

    fx.deletion(listing(Vec::new()))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();
    assert_eq!(
        fx.events.taken(),
        [DeletionEvent::RowsExpired {
            tenant_id: fx.key.tenant_id,
            review_requests: 2,
            record_ids: 3,
        }]
    );

    let quiet = Fixture::new();
    quiet
        .deletion(listing(Vec::new()))
        .expire(quiet.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();
    assert!(quiet.events.taken().is_empty());
}

#[tokio::test]
async fn a_request_left_open_on_a_fact_an_earlier_run_removed_is_closed_by_the_next() {
    let fx = Fixture::new();
    let gone = stored_fact(Category::Skill, "skills", json!({ "name": "Go" }), 10);
    fx.reviews.open_on(fx.key, &gone);

    fx.deletion(listing(vec![fx.key.subject_id]))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    let [request] = fx.reviews.all().try_into().unwrap();
    let ReviewStatus::Closed(closed) = &request.status else {
        panic!("closed, got {request:?}");
    };
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    let [ReviewQueueEvent::Resolved { .. }] = fx.review_events.taken().as_slice() else {
        panic!("one resolved event, got {:?}", fx.review_events.taken());
    };
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_subject_whose_requests_cannot_be_closed_does_not_stop_the_others() {
    let fx = Fixture::new();
    fx.reviews.down.store(true, Ordering::SeqCst);
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());

    fx.deletion(listing(vec![first, second]))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    let read: Vec<Uuid> = fx
        .profile
        .reads
        .lock()
        .unwrap()
        .iter()
        .map(|(_, key)| key.subject_id)
        .collect();
    assert_eq!(read, [first, second], "the second subject is swept too");
    assert_eq!(fx.profile.live().len(), 1, "the old fact is removed");
    assert!(
        logs_contain("past its period"),
        "the tenant's line is an error"
    );
    assert!(logs_contain("failed=2"));
}

/// A reviewer's resolution deleting `fact`, recorded on its open request.
fn deleted_by_reviewer(fx: &Fixture, fact: &crate::domain::profile_read::StoredFact) -> Uuid {
    let reviewer = Uuid::new_v4();
    let request = fx.reviews.open_on_at(fx.key, fact, 1_000);
    fx.reviews.pending.lock().unwrap().insert(
        request.id,
        crate::domain::review_queue::Closed {
            outcome: ReviewOutcome::Deleted,
            by: reviewer,
            at: OffsetDateTime::now_utc(),
            reason: None,
        },
    );
    reviewer
}

/// The one request's close.
fn closed_once(fx: &Fixture) -> crate::domain::review_queue::Closed {
    let [request] = fx.reviews.all().try_into().unwrap();
    let ReviewStatus::Closed(closed) = request.status else {
        panic!("closed, got {request:?}");
    };
    closed
}

#[tokio::test]
async fn a_fact_retention_removes_closes_its_request_as_construct_though_a_resolution_was_recorded()
{
    let fx = Fixture::new();
    let [old, _new] = fx.profile.live().try_into().unwrap();
    deleted_by_reviewer(&fx, &old);

    fx.deletion(listing(vec![fx.key.subject_id]))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    let closed = closed_once(&fx);
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    assert_eq!(
        closed.by, fx.identity,
        "retention's own write removed the fact, so no reviewer's write did"
    );
}

#[tokio::test]
async fn a_fact_an_earlier_run_removed_closes_its_request_as_the_resolution_recorded() {
    let fx = Fixture::new();
    let gone = stored_fact(Category::Skill, "skills", json!({ "name": "Go" }), 10);
    let reviewer = deleted_by_reviewer(&fx, &gone);

    fx.deletion(listing(vec![fx.key.subject_id]))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    let closed = closed_once(&fx);
    assert_eq!(closed.outcome, ReviewOutcome::Deleted);
    assert_eq!(
        closed.by, reviewer,
        "the reviewer's write may have removed it, so the request closes as theirs"
    );
}

#[tracing_test::traced_test]
#[tokio::test]
async fn every_close_is_tried_when_one_fails_and_the_tenant_reports_the_request_left_open() {
    let fx = Fixture::new();
    for name in ["Go", "C"] {
        let gone = stored_fact(Category::Skill, "skills", json!({ "name": name }), 10);
        fx.reviews.open_on(fx.key, &gone);
    }
    fx.reviews.close_open_fails.store(true, Ordering::SeqCst);

    fx.deletion(listing(vec![fx.key.subject_id]))
        .expire(fx.key.tenant_id, Duration::from_hours(24))
        .await
        .unwrap();

    assert_eq!(
        fx.reviews.close_open_calls.load(Ordering::SeqCst),
        2,
        "the second close is tried after the first failed"
    );
    assert!(
        fx.reviews
            .all()
            .iter()
            .all(crate::domain::review_queue::ReviewRequest::is_open)
    );
    assert!(logs_contain("left review requests open"));
    assert!(logs_contain("left_open=1"));
    assert!(
        !logs_contain("past its period"),
        "the facts are gone; only a request is left open"
    );
    assert!(
        !logs_contain("retention of a subject stopped"),
        "each failed close is logged once, with its fact"
    );
}

#[tokio::test]
async fn a_period_reaching_before_the_earliest_time_removes_nothing() {
    let fx = Fixture::new();

    fx.deletion(listing(vec![fx.key.subject_id]))
        .expire(fx.key.tenant_id, Duration::MAX)
        .await
        .unwrap();

    assert_eq!(fx.profile.live().len(), 2);
    assert!(fx.store.expired.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_write_that_lands_during_the_wait_is_removed_before_the_erasure_finishes() {
    let fx = Fixture::new();
    fx.store.mark_at(fx.key, OffsetDateTime::now_utc());
    let deletion = Arc::new(fx.deletion(FixedDirectory::default()));
    let running = tokio::spawn({
        let deletion = Arc::clone(&deletion);
        let key = fx.key;
        async move { deletion.erase(key).await }
    });
    tokio::time::sleep(WINDOW / 3).await;
    assert!(fx.profile.live().is_empty());
    assert_eq!(
        fx.roots.calls.load(Ordering::SeqCst),
        0,
        "the root stays through the wait, so a fact a late write attaches to it is found"
    );

    // A plan that passed Admission before the mark lands now.
    let late = stored_fact(Category::Skill, "skills", json!({ "name": "Go" }), 10);
    fx.profile.facts.lock().unwrap().push(late);
    *fx.profile.version.lock().unwrap() = Some(9);

    assert_eq!(running.await.unwrap().unwrap(), Erasure::Done);
    assert!(fx.profile.live().is_empty(), "the late fact goes too");
    assert_eq!(fx.writer.plans().len(), 2);
    assert_eq!(fx.store.finished(), [fx.key]);
}

#[tokio::test(start_paused = true)]
async fn a_run_whose_erasure_another_run_finished_leaves_the_new_profile() {
    use crate::domain::deletion::DeletionStore as _;

    let fx = Fixture::new();
    let marked = OffsetDateTime::now_utc();
    fx.store.mark_at(fx.key, marked);
    let deletion = Arc::new(fx.deletion(FixedDirectory::default()));
    let running = tokio::spawn({
        let deletion = Arc::clone(&deletion);
        let key = fx.key;
        async move { deletion.erase(key).await }
    });
    tokio::time::sleep(WINDOW / 3).await;
    assert!(fx.profile.live().is_empty(), "the first removal ran");

    // Another run of the same erasure finishes it during this run's wait,
    // and the subject, with personalization on again, starts a new profile.
    assert!(
        fx.store
            .finish(&toolkit_security::AccessScope::allow_all(), fx.key, marked)
            .await
            .unwrap()
    );
    let new = stored_fact(Category::Skill, "skills", json!({ "name": "Go" }), 10);
    fx.profile.facts.lock().unwrap().push(new.clone());

    assert_eq!(running.await.unwrap().unwrap(), Erasure::NotUnderWay);
    assert_eq!(fx.profile.live(), [new], "the new profile stays");
    assert_eq!(fx.writer.plans().len(), 1, "only the first removal wrote");
}

#[tokio::test]
async fn an_erasure_removes_the_root_of_the_generation_it_erases() {
    let fx = Fixture::new();
    fx.store
        .generations
        .lock()
        .unwrap()
        .insert((fx.key.tenant_id, fx.key.subject_id), 2);
    fx.profile.generation.store(2, Ordering::SeqCst);
    fx.marked_long_ago();

    let done = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap();

    assert_eq!(done, Erasure::Done);
    assert_eq!(*fx.roots.generations.lock().unwrap(), [2]);
}

/// Another run finishes the erasure and the subject starts a new profile
/// before this run's first removal reads the profile: the run reads the next
/// generation's profile and leaves it.
#[tokio::test]
async fn a_run_that_reads_a_new_generations_profile_first_leaves_it() {
    let fx = Fixture::new();
    fx.marked_long_ago();
    fx.profile.generation.store(1, Ordering::SeqCst);
    let before = fx.profile.live();

    let done = fx
        .deletion(FixedDirectory::default())
        .erase(fx.key)
        .await
        .unwrap();

    assert_eq!(done, Erasure::NotUnderWay);
    assert_eq!(fx.profile.live(), before, "the new profile stays");
    assert!(fx.writer.plans().is_empty(), "nothing was written");
    assert_eq!(fx.roots.calls.load(Ordering::SeqCst), 0);
    assert!(fx.store.finished().is_empty());
}

/// The run passes its check of the mark after the wait, then pauses; another
/// run finishes the erasure and the subject starts a new profile before this
/// one removes anything more. The mark this store still shows stands for the
/// check that passed just before the finish.
#[tokio::test(start_paused = true)]
async fn a_run_paused_after_its_check_leaves_the_new_profile_another_run_started() {
    let fx = Fixture::new();
    fx.store.mark_at(fx.key, OffsetDateTime::now_utc());
    let deletion = Arc::new(fx.deletion(FixedDirectory::default()));
    let running = tokio::spawn({
        let deletion = Arc::clone(&deletion);
        let key = fx.key;
        async move { deletion.erase(key).await }
    });
    tokio::time::sleep(WINDOW / 3).await;
    assert!(fx.profile.live().is_empty(), "the first removal ran");

    // The subject's new profile, in the next generation.
    let new = stored_fact(Category::Skill, "skills", json!({ "name": "Go" }), 10);
    fx.profile.facts.lock().unwrap().push(new.clone());
    fx.profile.generation.store(1, Ordering::SeqCst);
    *fx.profile.version.lock().unwrap() = Some((1 << 32) | 1);

    assert_eq!(running.await.unwrap().unwrap(), Erasure::NotUnderWay);
    assert_eq!(fx.profile.live(), [new], "the new profile stays");
    assert_eq!(fx.writer.plans().len(), 1, "only the first removal wrote");
    assert_eq!(
        fx.roots.calls.load(Ordering::SeqCst),
        0,
        "no root is removed"
    );
}
