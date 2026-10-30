//! The review request store on an in-memory `SQLite` database: one open
//! request per fact, the claim, closes only while open, the subject's and the
//! reviewers' lists, and the scope the caller gives.

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit_odata::{CursorV1, ODataQuery, SortDir};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use super::review_requests_repo::ReviewRequestStore;
use crate::domain::error::DomainError;
use crate::domain::pagination::QueryError;
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::review_queue::{
    Closed, FactsUnderReview, NewReviewRequest, Opened, RequestKey, ReviewOutcome, ReviewRequest,
    ReviewRequests, ReviewStatus, ReviewText,
};
use crate::domain::subject_settings::SubjectKey;
use crate::test_support::inmem_db;

/// The query the `OData` extractor builds for `raw`: the filter and its
/// hash, which a cursor carries.
fn filtered(raw: &str) -> ODataQuery {
    let expr = toolkit_odata::parse_filter_string(raw)
        .expect("a filter")
        .into_expr();
    let hash = toolkit_odata::pagination::short_filter_hash(Some(&expr));
    let query = ODataQuery::new().with_filter(expr);
    match hash {
        Some(hash) => query.with_filter_hash(hash),
        None => query,
    }
}

async fn store() -> ReviewRequestStore {
    ReviewRequestStore::new(Arc::new(toolkit_db::DBProvider::new(inmem_db().await)))
}

fn key() -> SubjectKey {
    SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    }
}

fn tenant(key: SubjectKey) -> AccessScope {
    AccessScope::for_tenant(key.tenant_id)
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::seconds(seconds)
}

/// A request on `fact`, created `seconds` after a fixed time.
fn new(key: SubjectKey, fact: &str, seconds: i64) -> NewReviewRequest {
    NewReviewRequest {
        id: Uuid::new_v4(),
        key,
        fact_id: NodeKey(fact.to_owned()),
        category: Category::Skill,
        property: "skills".to_owned(),
        value: json!({ "name": "Rust" }),
        comment: Some(ReviewText::new("wrong").unwrap()),
        created_at: at(1_000 + seconds),
    }
}

async fn opened(store: &ReviewRequestStore, request: NewReviewRequest) -> ReviewRequest {
    let scope = tenant(request.key);
    match store.open(&scope, request).await.expect("open") {
        Opened::Opened(request) => *request,
        Opened::AlreadyOpen => panic!("the fact already had an open request"),
    }
}

fn rejection(by: Uuid) -> Closed {
    Closed {
        outcome: ReviewOutcome::Rejected,
        by,
        at: at(5_000),
        reason: Some(ReviewText::new("it is right").unwrap()),
    }
}

/// Claim `request` for `closed.by` and close it as `closed`.
async fn claimed_and_closed(
    store: &ReviewRequestStore,
    request: &ReviewRequest,
    closed: &Closed,
) -> Option<ReviewRequest> {
    let scope = tenant(request.subject());
    store
        .claim(&scope, request.key(), closed.by, at(4_000), at(3_000))
        .await
        .unwrap()
        .expect("claimed");
    store.close(&scope, request.key(), closed).await.unwrap()
}

#[tokio::test]
async fn an_open_request_reads_back_as_stored() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;

    let found = store.find(&tenant(key), request.key()).await.unwrap();

    assert_eq!(found, Some(request.clone()));
    assert_eq!(
        request.status,
        ReviewStatus::Open {
            value: json!({ "name": "Rust" })
        }
    );
}

#[tokio::test]
async fn a_scope_naming_the_subject_reaches_its_requests_and_another_subjects_does_not() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let of = |subject: Uuid| {
        AccessScope::single(
            ScopeConstraint::try_new(vec![
                ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, key.tenant_id),
                ScopeFilter::eq(pep_properties::RESOURCE_ID, subject),
            ])
            .unwrap(),
        )
    };

    let own = store
        .find(&of(key.subject_id), request.key())
        .await
        .unwrap();
    let other = store
        .find(&of(Uuid::new_v4()), request.key())
        .await
        .unwrap();

    assert_eq!(own, Some(request));
    assert_eq!(other, None);
}

#[tokio::test]
async fn a_second_open_request_on_a_fact_is_refused_and_a_closed_one_is_not() {
    let store = store().await;
    let key = key();
    let first = opened(&store, new(key, "construct:t:a", 0)).await;

    let second = store
        .open(&tenant(key), new(key, "construct:t:a", 1))
        .await
        .unwrap();
    claimed_and_closed(&store, &first, &rejection(Uuid::new_v4()))
        .await
        .expect("closed");
    let after_close = store
        .open(&tenant(key), new(key, "construct:t:a", 2))
        .await
        .unwrap();
    let elsewhere = SubjectKey {
        tenant_id: Uuid::new_v4(),
        ..key
    };
    let in_another_tenant = store
        .open(&tenant(elsewhere), new(elsewhere, "construct:t:a", 3))
        .await
        .unwrap();

    assert_eq!(second, Opened::AlreadyOpen);
    assert!(matches!(after_close, Opened::Opened(_)), "{after_close:?}");
    assert!(
        matches!(in_another_tenant, Opened::Opened(_)),
        "{in_another_tenant:?}"
    );
}

#[tokio::test]
async fn one_reviewer_holds_a_claim_until_it_gives_it_back_or_it_goes_stale() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let claim = |by, now: i64| store.claim(&scope, request.key(), by, at(now), at(now - 60));

    let taken = claim(first, 100).await.unwrap();
    let refused = claim(second, 110).await.unwrap();
    let again_by_first = claim(first, 120).await.unwrap();
    store.release(&scope, request.key(), first).await.unwrap();
    let after_release = claim(second, 130).await.unwrap();
    let stale_taken = claim(first, 200).await.unwrap();

    assert!(taken.is_some());
    assert_eq!(refused, None, "another reviewer holds it");
    assert!(again_by_first.is_some(), "its own claim is renewed");
    assert!(after_release.is_some());
    assert!(
        stale_taken.is_some(),
        "a claim older than the hold frees itself"
    );
}

#[tokio::test]
async fn a_close_needs_the_claim_sets_the_outcome_clears_the_value_and_happens_once() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let reviewer = Uuid::new_v4();

    let unclaimed = store
        .close(&scope, request.key(), &rejection(reviewer))
        .await
        .unwrap();
    let closed = claimed_and_closed(&store, &request, &rejection(reviewer))
        .await
        .expect("closed");
    let again = store
        .close(&scope, request.key(), &rejection(reviewer))
        .await
        .unwrap();

    assert_eq!(unclaimed, None, "a reviewer closes only what it claimed");
    assert_eq!(closed.status, ReviewStatus::Closed(rejection(reviewer)));
    assert_eq!(
        closed.comment.as_ref().map(ReviewText::as_str),
        Some("wrong"),
        "the comment stays"
    );
    assert_eq!(again, None, "a closed request is not closed again");
}

#[tokio::test]
async fn a_correction_keeps_the_new_facts_id() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let corrected = Closed {
        outcome: ReviewOutcome::Corrected(NodeKey("construct:t:b".to_owned())),
        ..rejection(Uuid::new_v4())
    };

    let closed = claimed_and_closed(&store, &request, &corrected)
        .await
        .unwrap();

    assert_eq!(closed.status, ReviewStatus::Closed(corrected));
}

#[tokio::test]
async fn the_open_request_on_a_fact_closes_by_the_fact_claimed_or_not() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let claimed = opened(&store, new(key, "construct:t:a", 0)).await;
    opened(&store, new(key, "construct:t:b", 1)).await;
    let other = self::key();
    opened(&store, new(other, "construct:t:c", 2)).await;
    store
        .claim(&scope, claimed.key(), Uuid::new_v4(), at(10), at(0))
        .await
        .unwrap();
    let deleted = Closed {
        outcome: ReviewOutcome::Deleted,
        by: key.subject_id,
        at: at(20),
        reason: None,
    };

    let closed = store
        .close_open_on(&scope, key, &NodeKey("construct:t:a".to_owned()), &deleted)
        .await
        .unwrap();
    let nothing = store
        .close_open_on(
            &scope,
            key,
            &NodeKey("construct:t:zzz".to_owned()),
            &deleted,
        )
        .await
        .unwrap();
    let under_review = store.under_review(&scope, key).await.unwrap();

    assert_eq!(
        closed.map(|request| request.status),
        Some(ReviewStatus::Closed(deleted)),
        "the subject's delete closes a request a reviewer holds"
    );
    assert_eq!(nothing, None);
    assert_eq!(
        under_review
            .into_iter()
            .map(|fact| fact.0)
            .collect::<Vec<_>>(),
        ["construct:t:b"],
        "the closed one and another subject's are not"
    );
}

#[tokio::test]
async fn the_subjects_requests_are_newest_first_and_the_latest_per_fact_is_the_newest() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let older = opened(&store, new(key, "construct:t:a", 0)).await;
    claimed_and_closed(&store, &older, &rejection(Uuid::new_v4())).await;
    let newer = opened(&store, new(key, "construct:t:a", 5)).await;
    let other_fact = opened(&store, new(key, "construct:t:b", 3)).await;
    opened(&store, new(self::key(), "construct:t:a", 9)).await;

    let all = store.of_subject(&scope, key).await.unwrap();
    let latest = store
        .latest_on(
            &scope,
            key,
            &[
                NodeKey("construct:t:a".to_owned()),
                NodeKey("construct:t:zzz".to_owned()),
            ],
        )
        .await
        .unwrap();

    let ids: Vec<Uuid> = all.iter().map(|request| request.id).collect();
    assert_eq!(
        ids,
        [newer.id, other_fact.id, older.id],
        "the subject's only"
    );
    let latest: Vec<Uuid> = latest.iter().map(|request| request.id).collect();
    assert_eq!(latest, [newer.id]);
}

#[tokio::test]
async fn the_subjects_list_is_newest_first_filters_and_pages() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let mut requests = Vec::new();
    for (fact, seconds) in [
        ("construct:t:a", 0),
        ("construct:t:b", 1),
        ("construct:t:c", 2),
    ] {
        requests.push(opened(&store, new(key, fact, seconds)).await);
    }
    opened(&store, new(self::key(), "construct:t:d", 3)).await;
    claimed_and_closed(&store, &requests[0], &rejection(Uuid::new_v4())).await;
    let ids: Vec<Uuid> = requests.iter().map(|request| request.id).collect();

    let all = store
        .subject_page(&scope, key, &ODataQuery::new())
        .await
        .unwrap();
    let first = store
        .subject_page(&scope, key, &ODataQuery::new().with_limit(2))
        .await
        .unwrap();
    let rest = store
        .subject_page(
            &scope,
            key,
            &ODataQuery::new().with_limit(2).with_cursor(
                CursorV1::decode(&first.page_info.next_cursor.clone().expect("a next page"))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let closed = store
        .subject_page(&scope, key, &filtered("state eq 'closed'"))
        .await
        .unwrap();

    let ids_of = |page: &toolkit_odata::Page<ReviewRequest>| {
        page.items
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids_of(&all),
        [ids[2], ids[1], ids[0]],
        "newest first, the subject's only"
    );
    assert_eq!(ids_of(&first), [ids[2], ids[1]]);
    assert_eq!(ids_of(&rest), [ids[0]]);
    assert_eq!(ids_of(&closed), [ids[0]]);
}

#[tokio::test]
async fn the_reviewers_list_holds_the_tenants_open_requests_oldest_first() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let other_subject = SubjectKey {
        subject_id: Uuid::new_v4(),
        ..key
    };
    let older = opened(&store, new(other_subject, "construct:t:a", 0)).await;
    let newer = opened(&store, new(key, "construct:t:b", 1)).await;
    let closed = opened(&store, new(key, "construct:t:c", 2)).await;
    opened(&store, new(self::key(), "construct:t:d", 3)).await;
    claimed_and_closed(&store, &closed, &rejection(Uuid::new_v4())).await;

    let open = store
        .open_page(&scope, key.tenant_id, &ODataQuery::new())
        .await
        .unwrap();
    let of_one_subject = store
        .open_page(
            &scope,
            key.tenant_id,
            &filtered(&format!("subject_id eq {}", key.subject_id)),
        )
        .await
        .unwrap();

    let ids: Vec<Uuid> = open.items.iter().map(|request| request.id).collect();
    assert_eq!(ids, [older.id, newer.id]);
    assert_eq!(of_one_subject.items, [newer]);
}

#[tokio::test]
async fn a_scope_of_another_tenant_finds_claims_and_closes_nothing() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let elsewhere = AccessScope::for_tenant(Uuid::new_v4());
    let reviewer = Uuid::new_v4();

    let found = store.find(&elsewhere, request.key()).await.unwrap();
    let claimed = store
        .claim(&elsewhere, request.key(), reviewer, at(10), at(0))
        .await
        .unwrap();
    let closed = store
        .close(&elsewhere, request.key(), &rejection(reviewer))
        .await
        .unwrap();
    let listed = store
        .open_page(&elsewhere, key.tenant_id, &ODataQuery::new())
        .await
        .unwrap();

    assert_eq!(found, None);
    assert_eq!(claimed, None);
    assert_eq!(closed, None);
    assert!(listed.items.is_empty());
    let still = store.find(&tenant(key), request.key()).await.unwrap();
    assert_eq!(still, Some(request), "untouched and still open");
}

#[tokio::test]
async fn an_unknown_request_is_none() {
    let store = store().await;
    let key = key();

    let found = store
        .find(
            &tenant(key),
            RequestKey {
                tenant_id: key.tenant_id,
                id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();

    assert_eq!(found, None);
}

#[tokio::test]
async fn a_bad_filter_or_cursor_is_the_callers_mistake() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);

    let unknown_field = store
        .subject_page(&scope, key, &filtered("subject_id eq 'x'"))
        .await;
    let foreign_cursor = CursorV1 {
        k: vec!["x".to_owned()],
        o: SortDir::Desc,
        s: "-created_at,-id".to_owned(),
        f: Some("another-filter".to_owned()),
        d: "fwd".to_owned(),
    };
    let bad_cursor = store
        .subject_page(&scope, key, &ODataQuery::new().with_cursor(foreign_cursor))
        .await;

    assert!(
        matches!(
            unknown_field,
            Err(DomainError::InvalidQuery(QueryError::Filter(_)))
        ),
        "{unknown_field:?}"
    );
    assert!(
        matches!(
            bad_cursor,
            Err(DomainError::InvalidQuery(QueryError::Cursor(_)))
        ),
        "{bad_cursor:?}"
    );
}
