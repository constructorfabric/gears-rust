//! The review request store on an in-memory `SQLite` database: one open
//! request per fact, the claim, closes only while open, the subject's and the
//! reviewers' lists, and the scope the caller gives.

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use super::review_requests_entity as entity;
use super::review_requests_repo::ReviewRequestStore;
use crate::domain::error::DomainError;
use crate::domain::pagination::QueryError;
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::review_queue::{
    ClaimId, Claimed, Closed, FactsUnderReview, NewReviewRequest, Opened, RequestKey,
    ReviewOutcome, ReviewRequest, ReviewRequests, ReviewStatus, ReviewText,
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

const HOLD: std::time::Duration = std::time::Duration::from_secs(60);

/// Claim `request` as `claim` at `seconds`.
async fn claim_at(
    store: &ReviewRequestStore,
    request: &ReviewRequest,
    claim: ClaimId,
    seconds: i64,
) -> Claimed {
    store
        .claim(
            &tenant(request.subject()),
            request.key(),
            claim,
            at(seconds),
            HOLD,
        )
        .await
        .unwrap()
}

/// Claim `request` and close it as `closed`.
async fn claimed_and_closed(
    store: &ReviewRequestStore,
    request: &ReviewRequest,
    closed: &Closed,
) -> Option<ReviewRequest> {
    let claim = ClaimId(Uuid::new_v4());
    assert!(matches!(
        claim_at(store, request, claim, 4_000).await,
        Claimed::Taken { .. }
    ));
    store
        .close(&tenant(request.subject()), request.key(), claim, closed)
        .await
        .unwrap()
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
async fn one_claim_holds_a_request_until_it_is_given_back_or_goes_stale() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let (first, second) = (ClaimId(Uuid::new_v4()), ClaimId(Uuid::new_v4()));

    let taken = claim_at(&store, &request, first, 100).await;
    let refused = claim_at(&store, &request, second, 110).await;
    let not_again = claim_at(&store, &request, first, 120).await;
    store.release(&scope, request.key(), first).await.unwrap();
    let after_release = claim_at(&store, &request, second, 130).await;
    let stale_taken = claim_at(&store, &request, first, 200).await;

    assert!(matches!(taken, Claimed::Taken { .. }));
    assert_eq!(refused, Claimed::Held, "another claim holds it");
    assert_eq!(
        not_again,
        Claimed::Held,
        "a held request is not claimed twice, by the same reviewer either"
    );
    assert!(matches!(after_release, Claimed::Taken { .. }));
    assert!(
        matches!(stale_taken, Claimed::Taken { .. }),
        "a claim older than the hold frees itself"
    );
}

#[tokio::test]
async fn a_stale_claim_gives_back_nothing_once_another_took_the_request() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let (stale, newer) = (ClaimId(Uuid::new_v4()), ClaimId(Uuid::new_v4()));
    claim_at(&store, &request, stale, 100).await;
    let took = claim_at(&store, &request, newer, 200).await;

    store.release(&scope, request.key(), stale).await.unwrap();
    let late_pend = store
        .pend(&scope, request.key(), stale, &rejection(Uuid::new_v4()))
        .await
        .unwrap();
    let third = claim_at(&store, &request, ClaimId(Uuid::new_v4()), 210).await;

    assert!(matches!(took, Claimed::Taken { .. }));
    assert!(!late_pend, "a stale claim records nothing");
    assert_eq!(third, Claimed::Held, "the newer claim still holds it");
}

#[tokio::test]
async fn what_a_claim_recorded_reaches_the_next_claim_until_given_back_or_closed() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let pending = Closed {
        outcome: ReviewOutcome::Corrected(NodeKey("construct:t:new".to_owned())),
        by: Uuid::new_v4(),
        at: at(100),
        reason: Some(ReviewText::new("typo").unwrap()),
    };
    let first = ClaimId(Uuid::new_v4());
    claim_at(&store, &request, first, 100).await;
    let recorded = store
        .pend(&scope, request.key(), first, &pending)
        .await
        .unwrap();

    let after_lapse = claim_at(&store, &request, ClaimId(Uuid::new_v4()), 200).await;
    let again = ClaimId(Uuid::new_v4());
    claim_at(&store, &request, again, 300).await;
    store.release(&scope, request.key(), again).await.unwrap();
    let after_release = claim_at(&store, &request, ClaimId(Uuid::new_v4()), 310).await;

    assert!(recorded);
    let Claimed::Taken {
        request: taken,
        pending: carried,
    } = after_lapse
    else {
        panic!("taken after the hold: {after_lapse:?}");
    };
    assert_eq!(*taken, request);
    assert_eq!(carried, Some(pending), "read back as recorded");
    assert!(
        matches!(after_release, Claimed::Taken { pending: None, .. }),
        "a release forgets it: {after_release:?}"
    );
}

#[tokio::test]
async fn a_closed_or_unknown_request_cannot_be_claimed() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    claimed_and_closed(&store, &request, &rejection(Uuid::new_v4()))
        .await
        .expect("closed");
    let unknown = ReviewRequest {
        id: Uuid::new_v4(),
        ..request.clone()
    };

    let closed = claim_at(&store, &request, ClaimId(Uuid::new_v4()), 9_000).await;
    let missing = claim_at(&store, &unknown, ClaimId(Uuid::new_v4()), 9_000).await;

    assert_eq!(closed, Claimed::Closed);
    assert_eq!(missing, Claimed::Closed);
    let still = store
        .find(&tenant(key), request.key())
        .await
        .unwrap()
        .unwrap();
    assert!(!still.is_open(), "still closed");
}

#[tokio::test]
async fn a_close_needs_the_claim_sets_the_outcome_clears_the_value_and_happens_once() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let reviewer = Uuid::new_v4();
    let claim = ClaimId(Uuid::new_v4());

    let unclaimed = store
        .close(&scope, request.key(), claim, &rejection(reviewer))
        .await
        .unwrap();
    claim_at(&store, &request, claim, 4_000).await;
    let by_another_claim = store
        .close(
            &scope,
            request.key(),
            ClaimId(Uuid::new_v4()),
            &rejection(reviewer),
        )
        .await
        .unwrap();
    let closed = store
        .close(&scope, request.key(), claim, &rejection(reviewer))
        .await
        .unwrap()
        .expect("closed");
    let again = store
        .close(&scope, request.key(), claim, &rejection(reviewer))
        .await
        .unwrap();

    assert_eq!(unclaimed, None, "a resolution closes only what it claimed");
    assert_eq!(by_another_claim, None);
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
    claim_at(&store, &claimed, ClaimId(Uuid::new_v4()), 10).await;
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
async fn the_reviewers_list_pages_oldest_first() {
    let store = store().await;
    let key = key();
    let scope = tenant(key);
    let mut ids = Vec::new();
    for (fact, seconds) in [
        ("construct:t:a", 0),
        ("construct:t:b", 1),
        ("construct:t:c", 2),
    ] {
        ids.push(opened(&store, new(key, fact, seconds)).await.id);
    }

    let first = store
        .open_page(&scope, key.tenant_id, &ODataQuery::new().with_limit(2))
        .await
        .unwrap();
    let rest = store
        .open_page(
            &scope,
            key.tenant_id,
            &ODataQuery::new().with_limit(2).with_cursor(
                CursorV1::decode(&first.page_info.next_cursor.clone().expect("a next page"))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();

    let ids_of = |page: &toolkit_odata::Page<ReviewRequest>| {
        page.items
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids_of(&first), [ids[0], ids[1]]);
    assert_eq!(ids_of(&rest), [ids[2]]);
    assert_eq!(rest.page_info.next_cursor, None);
}

#[tokio::test]
async fn a_scope_of_another_tenant_finds_claims_and_closes_nothing() {
    let store = store().await;
    let key = key();
    let request = opened(&store, new(key, "construct:t:a", 0)).await;
    let elsewhere = AccessScope::for_tenant(Uuid::new_v4());
    let reviewer = Uuid::new_v4();

    let found = store.find(&elsewhere, request.key()).await.unwrap();
    let claim = ClaimId(Uuid::new_v4());
    let claimed = store
        .claim(&elsewhere, request.key(), claim, at(10), HOLD)
        .await
        .unwrap();
    let closed = store
        .close(&elsewhere, request.key(), claim, &rejection(reviewer))
        .await
        .unwrap();
    let listed = store
        .open_page(&elsewhere, key.tenant_id, &ODataQuery::new())
        .await
        .unwrap();

    assert_eq!(found, None);
    assert_eq!(claimed, Claimed::Closed);
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
    let bad_order = store
        .subject_page(
            &scope,
            key,
            &ODataQuery::new().with_order(ODataOrderBy(vec![OrderKey {
                field: "value".to_owned(),
                dir: SortDir::Asc,
            }])),
        )
        .await;
    let bad_open_filter = store
        .open_page(&scope, key.tenant_id, &filtered("state eq 'open'"))
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
    assert!(
        matches!(
            bad_order,
            Err(DomainError::InvalidQuery(QueryError::Order(_)))
        ),
        "{bad_order:?}"
    );
    assert!(
        matches!(
            bad_open_filter,
            Err(DomainError::InvalidQuery(QueryError::Filter(_)))
        ),
        "the reviewers' list has no state field: {bad_open_filter:?}"
    );
}

/// A row as an operator's hand or an old version could leave it, inserted
/// past the store.
async fn stored_row(db: &toolkit_db::Db, row: entity::ActiveModel) {
    use sea_orm::EntityTrait as _;
    use toolkit_db::secure::SecureInsertExt as _;

    let conn = db.conn().expect("connection");
    entity::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::allow_all(), &row)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();
}

/// A change that breaks one rule of a stored row.
type Tweak = fn(&mut entity::ActiveModel);

#[tokio::test]
async fn a_row_construct_cannot_read_is_an_internal_error() {
    use sea_orm::ActiveValue::Set;

    let db = inmem_db().await;
    let store = ReviewRequestStore::new(Arc::new(toolkit_db::DBProvider::new(db.clone())));
    let key = key();
    let row = |tweak: fn(&mut entity::ActiveModel)| {
        let mut row = entity::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(key.tenant_id),
            subject_id: Set(key.subject_id),
            fact_id: Set(format!("construct:t:{}", Uuid::new_v4())),
            entity_kind: Set("skill".to_owned()),
            property: Set("skills".to_owned()),
            value: Set(Some("{}".to_owned())),
            comment: Set(None),
            state: Set("open".to_owned()),
            open_fact_id: Set(None),
            outcome: Set(None),
            reason: Set(None),
            claim_id: Set(None),
            claimed_at: Set(None),
            pending: Set(None),
            resolved_by: Set(None),
            corrected_fact_id: Set(None),
            created_at: Set(at(0)),
            resolved_at: Set(None),
        };
        tweak(&mut row);
        row
    };
    let closed = |row: &mut entity::ActiveModel| {
        row.state = Set("closed".to_owned());
        row.value = Set(None);
        row.resolved_by = Set(Some(Uuid::new_v4()));
        row.resolved_at = Set(Some(at(10)));
        row.outcome = Set(Some("rejected".to_owned()));
    };
    let defects: [(&str, Tweak); 6] = [
        ("an unknown state", |row| {
            row.state = Set("waiting".to_owned());
        }),
        ("an open row without a value", |row| row.value = Set(None)),
        ("a value that is not JSON", |row| {
            row.value = Set(Some("{".to_owned()));
        }),
        ("an unknown entity kind", |row| {
            row.entity_kind = Set("hobby".to_owned());
        }),
        ("a correction without its new fact", |row| {
            row.state = Set("closed".to_owned());
            row.value = Set(None);
            row.resolved_by = Set(Some(Uuid::new_v4()));
            row.resolved_at = Set(Some(at(10)));
            row.outcome = Set(Some("corrected".to_owned()));
        }),
        ("a closed row without a resolver", |row| {
            row.state = Set("closed".to_owned());
            row.value = Set(None);
            row.outcome = Set(Some("deleted".to_owned()));
        }),
    ];
    let sound = row(closed);
    let sound_id = sound.id.clone().unwrap();
    stored_row(&db, sound).await;

    for (defect, tweak) in defects {
        let bad = row(tweak);
        let id = bad.id.clone().unwrap();
        stored_row(&db, bad).await;

        let found = store
            .find(
                &tenant(key),
                RequestKey {
                    tenant_id: key.tenant_id,
                    id,
                },
            )
            .await;

        assert!(
            matches!(found, Err(DomainError::Internal(_))),
            "{defect}: {found:?}"
        );
    }
    let fine = store
        .find(
            &tenant(key),
            RequestKey {
                tenant_id: key.tenant_id,
                id: sound_id,
            },
        )
        .await;
    assert!(matches!(fine, Ok(Some(_))), "{fine:?}");
}
