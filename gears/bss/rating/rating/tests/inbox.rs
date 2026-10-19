//! Feature 01 §2.1 and §6: accepting an upstream fact into the inbox.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bss_rating::domain::error::DomainError;
use bss_rating::domain::exception::SubjectKind;
use bss_rating::domain::inbox::{
    AcceptOutcome, InboxEntry, InboxKey, InboxRepo, InboxSource, InboxState, InboxTransition,
    ProducerOrigin, ProducerPolicy, Quarantine, ReceivedVia, SchemaSupport,
};
use bss_rating::domain::ports::{Dependency, FailureCode};
use bss_rating::infra::storage::repo::{exception_repo, inbox_repo};
use bss_rating::test_providers::MetricSample;
use common::{exception_key, fact, fact_of, fixture, fixture_over, fixture_with};
use serde_json::json;
use toolkit_db::secure::DbTx;
use toolkit_security::AccessScope;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0xA);

#[tokio::test]
async fn a_fact_accepted_twice_with_the_same_digest_has_one_effect() {
    let f = fixture().await;
    let first = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1", "unit": "h"})))
        .await
        .unwrap();
    let AcceptOutcome::Applied(entry) = first else {
        panic!("a new key is applied: {first:?}");
    };
    assert_eq!(entry.state(), InboxState::Applied);
    assert!(entry.applied_at().is_some());
    assert_eq!(entry.received_via(), ReceivedVia::Pull);

    // Member order does not change the digest: the same fact again is a no-op.
    let again = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"unit": "h", "q": "1"})))
        .await
        .unwrap();
    assert!(matches!(again, AcceptOutcome::Duplicate(ref e) if e.entry_id() == entry.entry_id()));
    assert_eq!(f.applier.applied(), vec![entry.entry_id()]);

    let conn = f.db().conn().unwrap();
    let stored = inbox_repo::find_by_key(&conn, &AccessScope::for_tenant(TENANT), entry.key())
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
}

#[tokio::test]
async fn a_fact_with_another_digest_is_quarantined_and_paged_and_nothing_is_applied() {
    let f = fixture().await;
    let original = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let conflict = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "2"})))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(entry) = conflict else {
        panic!("a digest conflict is quarantined: {conflict:?}");
    };
    assert_eq!(entry.state(), InboxState::Quarantined);
    assert_eq!(
        entry.quarantine(),
        Some(Quarantine::DigestConflict {
            conflict_of: original.entry().entry_id()
        })
    );
    // Only the original was applied, and it is untouched.
    assert_eq!(f.applier.applied(), vec![original.entry().entry_id()]);
    let conn = f.db().conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let stored = inbox_repo::find_by_id(&conn, &scope, original.entry().entry_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state(), InboxState::Applied);

    // Paged: the quarantine metric and an open exception on the entry.
    assert_eq!(
        f.metrics.samples(),
        vec![MetricSample::InboxQuarantined(InboxSource::Fact)]
    );
    let key = exception_key(
        TENANT,
        SubjectKind::Inbox,
        entry.entry_id().to_string(),
        "digest_conflict",
    );
    let open = exception_repo::find_open(&conn, &scope, &key)
        .await
        .unwrap();
    assert!(open.is_some());

    // The same conflicting fact again is absorbed by its own entry.
    let repeat = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "2"})))
        .await
        .unwrap();
    assert!(matches!(repeat, AcceptOutcome::Duplicate(ref e) if e.entry_id() == entry.entry_id()));
}

#[tokio::test]
async fn another_payload_under_the_producers_digest_is_a_conflict_not_a_duplicate() {
    // The producer reuses its own digest for a changed payload. Rating hashes the payload itself,
    // so the change is caught.
    let f = fixture().await;
    let digest = "ab".repeat(32);
    let original = f
        .inbox
        .accept_one(fact(
            TENANT,
            "fact-1",
            1,
            json!({"q": "1", "payload_digest": digest}),
        ))
        .await
        .unwrap();
    let changed = f
        .inbox
        .accept_one(fact(
            TENANT,
            "fact-1",
            1,
            json!({"q": "2", "payload_digest": digest}),
        ))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(entry) = changed else {
        panic!("a changed payload is quarantined: {changed:?}");
    };
    assert_eq!(
        entry.quarantine(),
        Some(Quarantine::DigestConflict {
            conflict_of: original.entry().entry_id()
        })
    );
    assert_eq!(f.applier.applied(), vec![original.entry().entry_id()]);
    // Member order is not a change: the canonical text is the same.
    let reordered = f
        .inbox
        .accept_one(fact(
            TENANT,
            "fact-1",
            1,
            json!({"payload_digest": digest, "q": "1"}),
        ))
        .await
        .unwrap();
    assert!(matches!(reordered, AcceptOutcome::Duplicate(ref e)
        if e.entry_id() == original.entry().entry_id()));
}

#[tokio::test]
async fn an_unsupported_schema_version_is_quarantined_never_dropped() {
    let f = fixture().await;
    let mut unsupported = fact(TENANT, "fact-9", 1, json!({"schema": "v99"}));
    unsupported.schema = SchemaSupport::Unsupported;
    let outcome = f.inbox.accept_one(unsupported).await.unwrap();
    let AcceptOutcome::Quarantined(entry) = outcome else {
        panic!("an unsupported schema is quarantined: {outcome:?}");
    };
    assert_eq!(
        entry.quarantine(),
        Some(Quarantine::UnsupportedSchemaVersion)
    );
    assert!(f.applier.applied().is_empty());
}

#[tokio::test]
async fn a_conflict_names_the_keys_first_arrival_even_when_it_was_never_applied() {
    let f = fixture().await;
    let mut unsupported = fact(TENANT, "fact-9", 1, json!({"schema": "v99"}));
    unsupported.schema = SchemaSupport::Unsupported;
    let primary = f.inbox.accept_one(unsupported).await.unwrap();
    let conflict = f
        .inbox
        .accept_one(fact(TENANT, "fact-9", 1, json!({"schema": "v1"})))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(entry) = conflict else {
        panic!("another digest under the key is a conflict: {conflict:?}");
    };
    assert_eq!(
        entry.quarantine(),
        Some(Quarantine::DigestConflict {
            conflict_of: primary.entry().entry_id()
        })
    );
    // The primary is itself quarantined: nothing of the key was applied.
    assert_eq!(primary.entry().state(), InboxState::Quarantined);
    assert!(f.applier.applied().is_empty());
}

#[tokio::test]
async fn a_quarantine_pages_only_once_it_is_committed() {
    let f = fixture().await;
    f.inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let conflict = fact(TENANT, "fact-1", 1, json!({"q": "2"}));
    let scope = f.inbox.admit(&conflict).await.unwrap();
    let inbox = f.inbox.clone();
    // The caller's transaction quarantines the fact, then fails and rolls back.
    let rolled_back: Result<(), DomainError> = f
        .db()
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let outcome = inbox.accept(tx, &scope, &conflict).await?;
                assert!(matches!(outcome, AcceptOutcome::Quarantined(_)));
                Err(DomainError::Concurrent("the caller gave up".to_owned()))
            })
        })
        .await;
    assert!(rolled_back.is_err());
    assert!(
        f.metrics.samples().is_empty(),
        "a rolled-back quarantine never pages"
    );
    let conn = f.db().conn().unwrap();
    let key = InboxKey::new(TENANT, InboxSource::Fact, "fact-1", 1).unwrap();
    let stored = inbox_repo::find_by_key(&conn, &AccessScope::for_tenant(TENANT), &key)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "only the applied primary is stored");
}

#[tokio::test]
async fn a_tenant_outside_ratings_grant_is_refused_and_nothing_is_written() {
    let f = fixture().await;
    let outside = Uuid::from_u128(0xB);
    f.pdp.put_outside_grant(outside);
    let result = f
        .inbox
        .accept_one(fact(outside, "fact-1", 1, json!({"q": "1"})))
        .await;
    assert_eq!(
        result,
        Err(DomainError::OutsideGrant { tenant_id: outside })
    );
    assert_nothing_stored(&f, outside, "fact-1").await;
}

#[tokio::test]
async fn an_unverified_producer_is_refused_and_nothing_is_written() {
    // Event intake is disabled by default, so no producer verifies on the event path.
    let f = fixture().await;
    let mut from_event = fact(TENANT, "fact-1", 1, json!({"q": "1"}));
    from_event.origin = ProducerOrigin::Event {
        producer: "bss-subscriptions".to_owned(),
    };
    let result = f.inbox.accept_one(from_event).await;
    assert_eq!(
        result,
        Err(DomainError::ProducerNotVerified {
            inbox_source: InboxSource::Fact
        })
    );
    assert_nothing_stored(&f, TENANT, "fact-1").await;
    assert_eq!(
        f.pdp.evaluations(),
        0,
        "a refused producer never reaches the PDP"
    );
}

#[tokio::test]
async fn an_event_from_the_configured_producer_is_accepted() {
    let f = fixture_with(
        ProducerPolicy::default().with_event_producer(InboxSource::Fact, "bss-subscriptions"),
    )
    .await;
    let mut from_event = fact(TENANT, "fact-1", 1, json!({"q": "1"}));
    from_event.origin = ProducerOrigin::Event {
        producer: "bss-subscriptions".to_owned(),
    };
    let outcome = f.inbox.accept_one(from_event).await.unwrap();
    assert_eq!(outcome.entry().received_via(), ReceivedVia::Event);
}

#[tokio::test]
async fn a_source_no_feature_applies_writes_nothing() {
    let f = fixture().await;
    let segment = fact_of(InboxSource::Segment, TENANT, "seg-1", 1, json!({}));
    let result = f.inbox.accept_one(segment).await;
    assert_eq!(
        result,
        Err(DomainError::SourceNotApplicable {
            inbox_source: InboxSource::Segment
        })
    );
    let conn = f.db().conn().unwrap();
    let key =
        bss_rating::domain::inbox::InboxKey::new(TENANT, InboxSource::Segment, "seg-1", 1).unwrap();
    assert!(
        inbox_repo::find_by_key(&conn, &AccessScope::for_tenant(TENANT), &key)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_refused_apply_rolls_the_entry_back() {
    let f = fixture().await;
    f.applier.fail();
    let result = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await;
    assert!(matches!(result, Err(DomainError::InvalidArgument { .. })));
    assert_nothing_stored(&f, TENANT, "fact-1").await;
}

#[tokio::test]
async fn versions_of_one_fact_are_separate_keys() {
    let f = fixture().await;
    let v1 = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let v2 = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 2, json!({"q": "2"})))
        .await
        .unwrap();
    assert!(matches!(v1, AcceptOutcome::Applied(_)));
    assert!(matches!(v2, AcceptOutcome::Applied(_)));
    assert_eq!(f.applier.applied().len(), 2);
}

#[tokio::test]
async fn one_key_in_two_tenants_is_two_entries() {
    let f = fixture().await;
    let other = Uuid::from_u128(0xB);
    let a = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let b = f
        .inbox
        .accept_one(fact(other, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    assert!(matches!(a, AcceptOutcome::Applied(_)));
    assert!(matches!(b, AcceptOutcome::Applied(_)));
    assert_ne!(a.entry().entry_id(), b.entry().entry_id());
}

#[tokio::test]
async fn a_third_payload_under_one_key_conflicts_with_the_primary_not_the_earlier_conflict() {
    let f = fixture().await;
    let primary = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let mut conflicts = Vec::new();
    for q in ["2", "3"] {
        let outcome = f
            .inbox
            .accept_one(fact(TENANT, "fact-1", 1, json!({"q": q})))
            .await
            .unwrap();
        let AcceptOutcome::Quarantined(entry) = outcome else {
            panic!("another digest under the key is a conflict: {outcome:?}");
        };
        conflicts.push(entry);
    }
    for conflict in &conflicts {
        assert_eq!(
            conflict.quarantine(),
            Some(Quarantine::DigestConflict {
                conflict_of: primary.entry().entry_id()
            }),
            "every conflict names the key's first arrival"
        );
    }
    assert_ne!(conflicts[0].entry_id(), conflicts[1].entry_id());
    assert_eq!(f.applier.applied(), vec![primary.entry().entry_id()]);
}

#[tokio::test]
async fn a_pdp_outage_at_intake_refuses_the_fact_and_is_counted() {
    let f = fixture().await;
    f.pdp.set_unavailable(true);
    let result = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await;
    assert!(
        matches!(result, Err(DomainError::AuthzUnavailable(..))),
        "{result:?}"
    );
    assert_eq!(
        f.metrics.samples(),
        vec![MetricSample::DependencyError(
            Dependency::Pdp,
            FailureCode::EvaluationFailed
        )]
    );
    f.pdp.set_unavailable(false);
    assert_nothing_stored(&f, TENANT, "fact-1").await;
}

/// An inbox table where every insert loses its race, as when a concurrent writer of the key keeps
/// committing first; it counts the inserts tried.
#[derive(Default)]
struct EveryInsertLoses {
    inserts: AtomicUsize,
}

#[async_trait]
impl InboxRepo for EveryInsertLoses {
    async fn find_primary_id(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _key: &InboxKey,
    ) -> Result<Option<Uuid>, DomainError> {
        Ok(None)
    }

    async fn find_by_id(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entry_id: Uuid,
    ) -> Result<Option<InboxEntry>, DomainError> {
        Ok(None)
    }

    async fn insert(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entry: &InboxEntry,
    ) -> Result<(), DomainError> {
        self.inserts.fetch_add(1, Ordering::SeqCst);
        Err(DomainError::Concurrent(
            "a concurrent writer inserted first".to_owned(),
        ))
    }

    async fn transition(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entry_id: Uuid,
        _transition: InboxTransition,
    ) -> Result<bool, DomainError> {
        Ok(false)
    }
}

#[tokio::test]
async fn a_retry_that_loses_the_race_again_gives_up_with_concurrent() {
    let repo = Arc::new(EveryInsertLoses::default());
    let f = fixture_over(
        ProducerPolicy::default(),
        Arc::clone(&repo) as Arc<dyn InboxRepo>,
    )
    .await;
    let result = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await;
    assert!(
        matches!(result, Err(DomainError::Concurrent(_))),
        "{result:?}"
    );
    assert_eq!(
        repo.inserts.load(Ordering::SeqCst),
        2,
        "one retry, then the caller hears of the race"
    );
    assert!(f.applier.applied().is_empty());
}

async fn assert_nothing_stored(f: &common::Fixture, tenant: Uuid, business_id: &str) {
    let conn = f.db().conn().unwrap();
    let key = bss_rating::domain::inbox::InboxKey::new(tenant, InboxSource::Fact, business_id, 1)
        .unwrap();
    // Read with the tenant's own scope: the refusal must have written nothing at all.
    let rows = inbox_repo::find_by_key(&conn, &AccessScope::for_tenant(tenant), &key)
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
    assert!(f.applier.applied().is_empty());
}
