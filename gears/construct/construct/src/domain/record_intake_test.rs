//! Record intake service tests on an in-memory `SQLite` database, with the
//! SDK's record types in a mock types registry.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_canonical_errors::{Problem, resource_error};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::record_intake::{IntakeOutcome, Place, RefusalReason};
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    DenyResolver, ErroringResolver, IntakeFixture, OwnTenantOnlyResolver, PendingResolver,
    RecordingResolver, chat_record, inmem_db, seed_subject_settings, stored_record_ids,
};

/// Builds resource errors for the policy service stubs.
#[resource_error(gts_id!("cf.construct.test.policy.v1~"))]
struct PolicyTestError;

/// A connector's login: its own home tenant, which differs from the tenants
/// it sends records for.
fn connector() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn refusal_reason(result: &Result<IntakeOutcome, DomainError>) -> Option<RefusalReason> {
    match result {
        Err(DomainError::Refused(refusal)) => Some(refusal.reason),
        _ => None,
    }
}

#[tokio::test]
async fn a_valid_record_is_received_handed_off_and_its_identity_kept() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let (ctx, tenant, subject) = (connector(), Uuid::new_v4(), Uuid::new_v4());

    let outcome = intake
        .submit(&ctx, tenant, chat_record(subject, "chat/t/1", "v1"))
        .await
        .expect("submit");

    assert_eq!(outcome, IntakeOutcome::Received);
    let received = fixture.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].tenant_id, tenant);
    assert_eq!(received[0].connector, ctx.subject_id().to_string());
    assert_eq!(received[0].envelope.subject_id, Some(subject));
    assert_eq!(received[0].envelope.provenance, "chat/t/1");
    assert_eq!(stored_record_ids(&db).await, 1);
}

#[tokio::test]
async fn the_same_identity_again_is_a_repeat_and_changes_nothing() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let (ctx, tenant, subject) = (connector(), Uuid::new_v4(), Uuid::new_v4());
    let record = chat_record(subject, "chat/t/1", "v1");
    intake
        .submit(&ctx, tenant, record.clone())
        .await
        .expect("first");

    let mut changed = record;
    changed["payload"]["text"] = json!("different content, same identity");
    let again = intake.submit(&ctx, tenant, changed).await.expect("again");

    assert_eq!(again, IntakeOutcome::Repeat);
    assert_eq!(fixture.received().len(), 1, "a repeat is not processed");
    assert_eq!(stored_record_ids(&db).await, 1);
}

#[tokio::test]
async fn the_identity_is_tenant_connector_provenance_and_version() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let (ctx, tenant, subject) = (connector(), Uuid::new_v4(), Uuid::new_v4());
    intake
        .submit(&ctx, tenant, chat_record(subject, "chat/t/1", "v1"))
        .await
        .expect("first");

    // Each differs from the first in exactly one part of the identity.
    let new_provenance = intake
        .submit(&ctx, tenant, chat_record(subject, "chat/t/2", "v1"))
        .await;
    let new_version = intake
        .submit(&ctx, tenant, chat_record(subject, "chat/t/1", "v2"))
        .await;
    let other_tenant = intake
        .submit(&ctx, Uuid::new_v4(), chat_record(subject, "chat/t/1", "v1"))
        .await;
    let other_connector = intake
        .submit(&connector(), tenant, chat_record(subject, "chat/t/1", "v1"))
        .await;

    for outcome in [new_provenance, new_version, other_tenant, other_connector] {
        assert_eq!(outcome.expect("submit"), IntakeOutcome::Received);
    }
    assert_eq!(stored_record_ids(&db).await, 5);
}

#[tokio::test]
async fn a_record_with_an_id_is_refused_without_naming_its_type() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let mut record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");
    record["id"] = json!(
        "gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~cf.construct.examples.chat_message.v1"
    );
    record["type"] = json!("not checked yet, so never echoed");

    let result = intake.submit(&connector(), Uuid::new_v4(), record).await;

    let Err(DomainError::Refused(refusal)) = result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert_eq!(refusal.reason, RefusalReason::IdInPush);
    assert_eq!(refusal.place, Place::pointer("/id"));
    assert_eq!(refusal.type_id, None);
    assert!(fixture.received().is_empty());
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn a_record_that_breaks_its_type_is_refused_with_the_place_and_nothing_is_stored() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let mut record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");
    record["payload"]["role"] = json!("system");

    let result = intake.submit(&connector(), Uuid::new_v4(), record).await;

    let Err(DomainError::Refused(refusal)) = result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert_eq!(refusal.reason, RefusalReason::SchemaViolation);
    assert_eq!(
        refusal.type_id.as_deref(),
        Some(construct_sdk::gts::CHAT_MESSAGE_TYPE)
    );
    assert_eq!(refusal.place, Place::pointer("/payload/role"));
    assert_eq!(refusal.rule, "enum");
    assert!(fixture.received().is_empty());
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn a_connector_that_is_off_is_refused() {
    let db = inmem_db().await;
    let ctx = connector();
    let fixture = IntakeFixture {
        connectors_off: vec![ctx.subject_id()],
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);

    let result = intake
        .submit(
            &ctx,
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await;

    assert_eq!(refusal_reason(&result), Some(RefusalReason::ConnectorOff));
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn personalization_off_for_the_subject_is_refused() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(&db, tenant, subject, SubjectSettings::new_subject(false)).await;

    let result = intake
        .submit(&connector(), tenant, chat_record(subject, "chat/t/1", "v1"))
        .await;

    assert_eq!(
        refusal_reason(&result),
        Some(RefusalReason::PersonalizationOff)
    );
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn a_new_subject_takes_the_tenant_default() {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        personalization_default: false,
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);

    let result = intake
        .submit(
            &connector(),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await;

    assert_eq!(
        refusal_reason(&result),
        Some(RefusalReason::PersonalizationOff)
    );
}

#[tokio::test]
async fn an_erasure_under_way_is_refused_even_with_personalization_on() {
    let db = inmem_db().await;
    let intake = IntakeFixture::default().build(&db);
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let erasing = SubjectSettings {
        personalization_enabled: true,
        erasure_in_progress: true,
    };
    seed_subject_settings(&db, tenant, subject, erasing).await;

    let result = intake
        .submit(&connector(), tenant, chat_record(subject, "chat/t/1", "v1"))
        .await;

    assert_eq!(
        refusal_reason(&result),
        Some(RefusalReason::ErasureInProgress)
    );
}

#[tokio::test]
async fn the_subject_settings_of_another_tenant_do_not_apply() {
    let db = inmem_db().await;
    let intake = IntakeFixture::default().build(&db);
    let (tenant, other, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(&db, other, subject, SubjectSettings::new_subject(false)).await;

    let outcome = intake
        .submit(&connector(), tenant, chat_record(subject, "chat/t/1", "v1"))
        .await;

    assert_eq!(outcome.expect("submit"), IntakeOutcome::Received);
}

#[tokio::test]
async fn a_record_without_a_subject_skips_the_subject_checks() {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        personalization_default: false,
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);
    // The rolos-cyber example of a source-scoped record, without its store id.
    let catalog = json!({
        "type": construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE,
        "provenance": "lms-eu/course-101",
        "version": "2026-09-01T12:00:00Z",
        "observed_at": "2026-09-10T08:05:00Z",
        "payload": {
            "course_id": "course-101",
            "title": "Introduction to Computational Biology",
            "sections": [{ "section_index": 0, "title": "Sequence alignment" }],
            "concepts": [
                { "concept_index": 0, "name": "Dynamic programming", "section_index": 0 },
                { "concept_index": 1, "name": "Substitution matrices", "section_index": 0 }
            ]
        }
    });

    let outcome = intake.submit(&connector(), Uuid::new_v4(), catalog).await;

    assert_eq!(outcome.expect("submit"), IntakeOutcome::Received);
}

#[tokio::test]
async fn a_connector_without_permission_is_refused_before_the_record_is_checked() {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        resolver: Arc::new(DenyResolver),
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);
    // Every later check would refuse this record: it carries an id, breaks
    // its type, and its subject has personalization off.
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(&db, tenant, subject, SubjectSettings::new_subject(false)).await;
    let mut record = chat_record(subject, "chat/t/1", "v1");
    record["payload"]["role"] = json!("system");
    record["id"] = json!("x");

    let result = intake.submit(&connector(), tenant, record).await;

    assert!(
        matches!(result, Err(DomainError::Forbidden(_))),
        "got {result:?}"
    );
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn a_connector_authorized_only_for_its_own_tenant_cannot_write_into_another() {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        resolver: Arc::new(OwnTenantOnlyResolver),
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);

    let result = intake
        .submit(
            &connector(),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::Forbidden(_))),
        "got {result:?}"
    );
    assert!(fixture.received().is_empty());
    assert_eq!(stored_record_ids(&db).await, 0);
}

async fn submit_with_policy(
    resolver: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
) -> Result<IntakeOutcome, DomainError> {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        resolver,
        policy_deadline: Some(Duration::from_millis(50)),
        ..IntakeFixture::default()
    };
    fixture
        .build(&db)
        .submit(
            &connector(),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
}

#[tokio::test]
async fn a_retryable_policy_failure_is_unavailable() {
    let unavailable: fn() -> CanonicalError = || CanonicalError::service_unavailable().create();
    let slow: fn() -> CanonicalError = || PolicyTestError::deadline_exceeded("too slow").create();
    let throttled: fn() -> CanonicalError = || {
        PolicyTestError::resource_exhausted("too many requests")
            .with_quota_violation("policy", "rate limit")
            .create()
    };

    for error in [unavailable, slow, throttled] {
        let result = submit_with_policy(Arc::new(ErroringResolver(error))).await;
        assert!(
            matches!(result, Err(DomainError::Unavailable(_))),
            "got {result:?}"
        );
    }
}

#[tokio::test]
async fn an_unanswered_policy_request_is_unavailable() {
    let result = submit_with_policy(Arc::new(PendingResolver)).await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "got {result:?}"
    );
}

#[tokio::test]
async fn any_other_policy_failure_is_internal_and_hides_its_cause() {
    let result = submit_with_policy(Arc::new(ErroringResolver(|| {
        CanonicalError::internal("policy store at 10.0.0.7 is corrupt".to_owned()).create()
    })))
    .await;

    let Err(DomainError::Internal(message)) = result else {
        panic!("expected an internal error, got {result:?}");
    };
    let problem = Problem::from(CanonicalError::from(DomainError::Internal(message)));
    assert!(!problem.detail.contains("10.0.0.7"), "{}", problem.detail);
}

#[tokio::test]
async fn without_processing_a_record_is_unavailable_and_leaves_no_identity() {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        no_processing: true,
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);
    let (ctx, tenant) = (connector(), Uuid::new_v4());
    let record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");

    let first = intake.submit(&ctx, tenant, record.clone()).await;
    let again = intake.submit(&ctx, tenant, record).await;

    for result in [first, again] {
        assert!(
            matches!(result, Err(DomainError::Unavailable(_))),
            "a resend is not a repeat: got {result:?}"
        );
    }
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn intake_asks_for_the_record_resource_with_the_named_tenant() {
    let db = inmem_db().await;
    let resolver = Arc::new(RecordingResolver::default());
    let fixture = IntakeFixture {
        resolver: resolver.clone(),
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);
    let tenant = Uuid::new_v4();

    intake
        .submit(
            &connector(),
            tenant,
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
        .expect("submit");

    let asked = resolver.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec![(
            "construct.record".to_owned(),
            "send".to_owned(),
            Some(tenant),
            Some(tenant.to_string()),
        )],
        "one permission check, for the named tenant"
    );
}

#[test]
fn every_refusal_reason_has_its_wire_code() {
    let expected = [
        (RefusalReason::SchemaViolation, "SCHEMA_VIOLATION"),
        (RefusalReason::UnknownType, "UNKNOWN_TYPE"),
        (RefusalReason::NotARecordType, "NOT_A_RECORD_TYPE"),
        (RefusalReason::AbstractType, "ABSTRACT_TYPE"),
        (RefusalReason::IdInPush, "ID_IN_PUSH"),
        (RefusalReason::ConnectorOff, "CONNECTOR_OFF"),
        (RefusalReason::PersonalizationOff, "PERSONALIZATION_OFF"),
        (RefusalReason::ErasureInProgress, "ERASURE_IN_PROGRESS"),
    ];
    for (reason, code) in expected {
        assert_eq!(reason.code(), code, "{reason:?}");
    }
}
