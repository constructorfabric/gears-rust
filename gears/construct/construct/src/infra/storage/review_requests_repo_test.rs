//! The review request store on an in-memory `SQLite` database: one open
//! request per fact, closes only while open, the subject's and the reviewers'
//! lists, and the tenant scope.

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit_odata::{CursorV1, ODataQuery};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::review_requests_repo::SeaOrmReviewRequests;
use crate::domain::error::DomainError;
use crate::domain::pagination::QueryError;
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::review_queue::{
    Closing, NewReviewRequest, Opened, ReviewOutcome, ReviewRequest, ReviewRequests, ReviewState,
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

async fn store() -> SeaOrmReviewRequests {
    SeaOrmReviewRequests::new(Arc::new(toolkit_db::DBProvider::new(inmem_db().await)))
}

fn key() -> SubjectKey {
    SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    }
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
        comment: Some("wrong".to_owned()),
        created_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000 + seconds),
    }
}

async fn opened(store: &SeaOrmReviewRequests, request: NewReviewRequest) -> ReviewRequest {
    match store.open(request).await.expect("open") {
        Opened::Opened(request) => *request,
        Opened::AlreadyOpen => panic!("the fact already had an open request"),
    }
}

fn rejection(by: Uuid) -> Closing {
    Closing {
        outcome: ReviewOutcome::Rejected,
        by,
        reason: Some("it is right".to_owned()),
        corrected_fact_id: None,
        at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(5_000),
    }
}

#[tokio::test]
async fn an_open_request_reads_back_as_stored() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;

    let found = store
        .find(
            &AccessScope::for_tenant(key.tenant_id),
            key.tenant_id,
            request.id,
        )
        .await
        .unwrap();

    assert_eq!(found, Some(request.clone()));
    assert_eq!(request.state, ReviewState::Open);
    assert_eq!(request.value, Some(json!({ "name": "Rust" })));
}

#[tokio::test]
async fn a_second_open_request_on_a_fact_is_refused_and_a_closed_one_is_not() {
    let store = store().await;
    let key = key();
    let first = opened(&store, new(key, "construct:t:a", 0)).await;

    let second = store.open(new(key, "construct:t:a", 1)).await.unwrap();
    store
        .close(
            &AccessScope::for_tenant(key.tenant_id),
            key.tenant_id,
            first.id,
            &rejection(Uuid::new_v4()),
        )
        .await
        .unwrap()
        .expect("closed");
    let after_close = store.open(new(key, "construct:t:a", 2)).await.unwrap();
    let in_another_tenant = store
        .open(new(
            SubjectKey {
                tenant_id: Uuid::new_v4(),
                ..key
            },
            "construct:t:a",
            3,
        ))
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
async fn a_close_sets_the_outcome_clears_the_value_and_happens_once() {
    let store = store().await;
    let key = key();
    let scope = AccessScope::for_tenant(key.tenant_id);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let reviewer = Uuid::new_v4();

    let closed = store
        .close(&scope, key.tenant_id, request.id, &rejection(reviewer))
        .await
        .unwrap()
        .expect("closed");
    let again = store
        .close(&scope, key.tenant_id, request.id, &rejection(reviewer))
        .await
        .unwrap();

    assert_eq!(closed.state, ReviewState::Closed);
    assert_eq!(closed.outcome, Some(ReviewOutcome::Rejected));
    assert_eq!(closed.reason.as_deref(), Some("it is right"));
    assert_eq!(closed.resolved_by, Some(reviewer));
    assert_eq!(
        closed.resolved_at,
        Some(OffsetDateTime::UNIX_EPOCH + Duration::seconds(5_000))
    );
    assert_eq!(closed.value, None);
    assert_eq!(
        closed.comment.as_deref(),
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

    let closed = store
        .close(
            &AccessScope::for_tenant(key.tenant_id),
            key.tenant_id,
            request.id,
            &Closing {
                outcome: ReviewOutcome::Corrected,
                corrected_fact_id: Some(NodeKey("construct:t:b".to_owned())),
                ..rejection(Uuid::new_v4())
            },
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        closed.corrected_fact_id,
        Some(NodeKey("construct:t:b".to_owned()))
    );
}

#[tokio::test]
async fn the_open_request_on_a_fact_closes_by_the_fact_and_only_the_open_facts_are_under_review() {
    let store = store().await;
    let key = key();
    opened(&store, new(key, "construct:t:a", 0)).await;
    opened(&store, new(key, "construct:t:b", 1)).await;
    opened(&store, new(self::key(), "construct:t:c", 2)).await;

    let closed = store
        .close_open_on(
            key,
            &NodeKey("construct:t:a".to_owned()),
            &rejection(key.subject_id),
        )
        .await
        .unwrap();
    let nothing = store
        .close_open_on(
            key,
            &NodeKey("construct:t:zzz".to_owned()),
            &rejection(key.subject_id),
        )
        .await
        .unwrap();
    let under_review = store.under_review(key).await.unwrap();

    assert_eq!(
        closed.map(|request| request.fact_id.0),
        Some("construct:t:a".to_owned())
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
async fn the_subjects_list_is_newest_first_filters_and_pages() {
    let store = store().await;
    let key = key();
    let ids: Vec<Uuid> = {
        let mut ids = Vec::new();
        for (fact, at) in [
            ("construct:t:a", 0),
            ("construct:t:b", 1),
            ("construct:t:c", 2),
        ] {
            ids.push(opened(&store, new(key, fact, at)).await.id);
        }
        ids
    };
    opened(&store, new(self::key(), "construct:t:d", 3)).await;
    store
        .close(
            &AccessScope::for_tenant(key.tenant_id),
            key.tenant_id,
            ids[0],
            &rejection(Uuid::new_v4()),
        )
        .await
        .unwrap();

    let all = store.subject_page(key, &ODataQuery::new()).await.unwrap();
    let first = store
        .subject_page(key, &ODataQuery::new().with_limit(2))
        .await
        .unwrap();
    let rest = store
        .subject_page(
            key,
            &ODataQuery::new().with_limit(2).with_cursor(
                CursorV1::decode(&first.page_info.next_cursor.clone().expect("a next page"))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let closed = store
        .subject_page(key, &filtered("state eq 'closed'"))
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
    let other_subject = SubjectKey {
        subject_id: Uuid::new_v4(),
        ..key
    };
    let older = opened(&store, new(other_subject, "construct:t:a", 0)).await;
    let newer = opened(&store, new(key, "construct:t:b", 1)).await;
    let closed = opened(&store, new(key, "construct:t:c", 2)).await;
    opened(&store, new(self::key(), "construct:t:d", 3)).await;
    let scope = AccessScope::for_tenant(key.tenant_id);
    store
        .close(&scope, key.tenant_id, closed.id, &rejection(Uuid::new_v4()))
        .await
        .unwrap();

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
async fn a_scope_of_another_tenant_finds_and_closes_nothing() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let elsewhere = AccessScope::for_tenant(Uuid::new_v4());

    let found = store
        .find(&elsewhere, key.tenant_id, request.id)
        .await
        .unwrap();
    let closed = store
        .close(
            &elsewhere,
            key.tenant_id,
            request.id,
            &rejection(Uuid::new_v4()),
        )
        .await
        .unwrap();
    let listed = store
        .open_page(&elsewhere, key.tenant_id, &ODataQuery::new())
        .await
        .unwrap();

    assert_eq!(found, None);
    assert_eq!(closed, None);
    assert!(listed.items.is_empty());
}

#[tokio::test]
async fn a_bad_filter_or_cursor_is_the_callers_mistake() {
    let store = store().await;
    let key = key();

    let unknown_field = store
        .subject_page(key, &filtered("subject_id eq 'x'"))
        .await;
    let first = store.subject_page(key, &ODataQuery::new()).await.unwrap();
    let foreign_cursor = CursorV1 {
        k: vec!["x".to_owned()],
        o: toolkit_odata::SortDir::Desc,
        s: "-created_at,-id".to_owned(),
        f: Some("another-filter".to_owned()),
        d: "fwd".to_owned(),
    };
    let bad_cursor = store
        .subject_page(key, &ODataQuery::new().with_cursor(foreign_cursor))
        .await;
    assert!(first.items.is_empty());

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
