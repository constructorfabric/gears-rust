//! Record intake service tests on an in-memory `SQLite` database, with the
//! SDK's record types in a mock types registry.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use time::format_description::well_known::Rfc3339;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_canonical_errors::{Problem, resource_error};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::record_intake::{IntakeOutcome, Place, RefusalReason};
use crate::domain::subject_settings::{ScopeRefusal, SubjectKey, SubjectSettings};
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
    assert_eq!(
        received[0].envelope.observed_at,
        time::OffsetDateTime::parse("2026-09-10T09:13:05Z", &Rfc3339).unwrap()
    );
    assert_eq!(stored_record_ids(&db).await, 1);
}

/// A taken record carries its subject's profile generation, so processing
/// can drop it once an erasure moved the generation on.
#[tokio::test]
async fn a_taken_record_carries_its_subjects_generation() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    fixture.generations.erase();
    fixture.generations.erase();
    let intake = fixture.build(&db);

    intake
        .submit(
            &connector(),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
        .expect("submit");

    let received = fixture.received();
    assert_eq!(
        received
            .iter()
            .map(|record| record.generation)
            .collect::<Vec<_>>(),
        [2]
    );
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
async fn a_record_of_a_type_about_a_person_without_a_subject_is_refused() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let mut record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");
    record.as_object_mut().unwrap().remove("subject_id");

    let result = intake.submit(&connector(), Uuid::new_v4(), record).await;

    let Err(DomainError::Refused(refusal)) = result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert_eq!(refusal.reason, RefusalReason::SchemaViolation);
    assert_eq!(
        refusal.type_id.as_deref(),
        Some(construct_sdk::gts::CHAT_MESSAGE_TYPE)
    );
    assert_eq!(refusal.place, Place::pointer("/subject_id"));
    assert_eq!(refusal.rule, "required");
    assert!(fixture.received().is_empty());
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn the_type_schema_is_checked_before_the_subject() {
    let db = inmem_db().await;
    let intake = IntakeFixture::default().build(&db);
    let mut record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");
    record.as_object_mut().unwrap().remove("subject_id");
    record["payload"].as_object_mut().unwrap().remove("text");

    let result = intake.submit(&connector(), Uuid::new_v4(), record).await;

    let Err(DomainError::Refused(refusal)) = result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert_eq!(refusal.place, Place::pointer("/payload/text"));
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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        SubjectSettings::new_subject(false),
    )
    .await;

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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        erasing,
    )
    .await;

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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: other,
            subject_id: subject,
        },
        SubjectSettings::new_subject(false),
    )
    .await;

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
    // A source-scoped record: a course catalog has no subject.
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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        SubjectSettings::new_subject(false),
    )
    .await;
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
    // With or without a stored opt-out in the named tenant: the subject's
    // settings there are outside the connector's scope, so the record is
    // refused before anything is read or written.
    for stored in [false, true] {
        let db = inmem_db().await;
        let fixture = IntakeFixture {
            resolver: Arc::new(OwnTenantOnlyResolver),
            ..IntakeFixture::default()
        };
        let intake = fixture.build(&db);
        let (ctx, tenant, subject) = (connector(), Uuid::new_v4(), Uuid::new_v4());
        let key = SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        };
        if stored {
            seed_subject_settings(&db, key, SubjectSettings::new_subject(false)).await;
        }

        let result = intake
            .submit(&ctx, tenant, chat_record(subject, "chat/t/1", "v1"))
            .await;

        match &result {
            Err(DomainError::OutOfScope(refused)) => {
                assert_eq!(
                    (refused.tenant_id, refused.subject_id),
                    (tenant, subject),
                    "stored {stored}: {refused}"
                );
                assert_eq!(refused.refusal, ScopeRefusal::NotCovered, "{refused}");
                assert_eq!(refused.caller_tenant, ctx.subject_tenant_id(), "{refused}");
            }
            other => panic!("stored {stored}: expected the scope refusal, got {other:?}"),
        }
        assert!(fixture.received().is_empty());
        assert_eq!(stored_record_ids(&db).await, 0);
    }
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
async fn until_the_planner_exists_a_record_is_received_and_dropped_with_an_event() {
    let db = inmem_db().await;
    let fixture = IntakeFixture {
        dropping: true,
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db);
    let (ctx, tenant, subject) = (connector(), Uuid::new_v4(), Uuid::new_v4());
    let record = chat_record(subject, "chat/t/1", "v1");

    let first = intake.submit(&ctx, tenant, record.clone()).await;
    let again = intake.submit(&ctx, tenant, record).await;

    assert_eq!(first.expect("submit"), IntakeOutcome::Received);
    assert_eq!(
        again.expect("resend"),
        IntakeOutcome::Repeat,
        "the identity is kept, so a resend is a repeat"
    );
    assert_eq!(stored_record_ids(&db).await, 1);
    let dropped = fixture.events.dropped.lock().unwrap().clone();
    assert_eq!(dropped.len(), 1, "one drop event, for the received record");
    assert_eq!(dropped[0].tenant_id, tenant);
    assert_eq!(dropped[0].subject_id, Some(subject));
    assert_eq!(dropped[0].connector, ctx.subject_id().to_string());
    assert_eq!(
        dropped[0].record_key,
        crate::domain::record_intake::RecordKey::of(crate::domain::record_intake::IdentityParts {
            connector: &ctx.subject_id().to_string(),
            provenance: "chat/t/1",
            version: "v1",
        })
    );
}

/// A hand-off that refuses every record, as a full queue would.
struct RefusingHandOff;

impl crate::domain::record_intake::RecordHandOff for RefusingHandOff {
    fn hand_off(
        &self,
        _record: crate::domain::record_intake::ReceivedRecord,
    ) -> Result<(), DomainError> {
        Err(DomainError::Unavailable("queue full".to_owned()))
    }
}

#[tokio::test]
async fn a_hand_off_that_refuses_leaves_no_identity_so_the_record_can_be_sent_again() {
    let db = inmem_db().await;
    let types = IntakeFixture::default().types;
    let settings = Arc::new(crate::test_support::settings_service(
        &db,
        Arc::new(crate::test_support::AllowResolver),
        true,
    ));
    let intake = crate::domain::record_intake::RecordIntakeService::new(
        Arc::new(toolkit_db::DBProvider::new(db.clone())),
        Arc::new(crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository::new()),
        settings,
        crate::domain::record_intake::IntakePorts {
            types,
            switch: Arc::new(crate::infra::connector_switch::ConfigConnectorSwitch::new(
                [],
            )),
            hand_off: Arc::new(RefusingHandOff),
            retention: crate::test_support::no_retention(),
            generations: crate::test_support::no_erasure(),
        },
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(crate::test_support::AllowResolver)),
    );
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
            "gts.cf.connectors.core.record.v1~".to_owned(),
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
        (RefusalReason::PastRetention, "PAST_RETENTION"),
    ];
    for (reason, code) in expected {
        assert_eq!(reason.code(), code, "{reason:?}");
    }
}

#[tokio::test]
async fn an_observed_at_in_lower_case_form_is_read() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let mut record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");
    record["observed_at"] = json!("2026-09-10t09:13:05.5+02:00");

    let outcome = intake
        .submit(&connector(), Uuid::new_v4(), record)
        .await
        .expect("submit");

    assert_eq!(outcome, IntakeOutcome::Received);
    assert_eq!(
        fixture.received()[0].envelope.observed_at,
        time::OffsetDateTime::parse("2026-09-10T07:13:05.5Z", &Rfc3339).unwrap()
    );
}

#[tokio::test]
async fn an_observed_at_that_is_no_real_date_is_refused_with_its_place() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let mut record = chat_record(Uuid::new_v4(), "chat/t/1", "v1");
    record["observed_at"] = json!("2026-02-30T10:00:00Z");

    let result = intake.submit(&connector(), Uuid::new_v4(), record).await;

    let Err(DomainError::Refused(refusal)) = result else {
        panic!("refused, got {result:?}");
    };
    assert_eq!(refusal.reason, RefusalReason::SchemaViolation);
    assert_eq!(refusal.place, Place::pointer("/observed_at"));
    assert!(fixture.received().is_empty());
    assert_eq!(stored_record_ids(&db).await, 0);
}

/// The subject rule across a type's chain, the record's type first and the
/// base last.
#[test]
fn the_subject_rule_resolves_across_the_type_chain() {
    use crate::domain::record_intake::{SubjectTrait as S, TypeTraits};

    let required = |chain: &[S]| {
        TypeTraits {
            type_chain: Vec::new(),
            subject_chain: chain.to_vec(),
        }
        .subject_required()
    };

    // The base alone decides when nothing below it declares the trait.
    assert!(required(&[S::Unset, S::On]));
    assert!(!required(&[S::Unset, S::Off]));
    // The closest declared value wins.
    assert!(!required(&[S::Off, S::On]));
    assert!(!required(&[S::Unset, S::Off, S::On]));
    // A type derived from a person type stays about a person.
    assert!(required(&[S::Off, S::On, S::On]));
    assert!(required(&[S::Off, S::Unset, S::On, S::Off]));
    // A malformed value requires a subject.
    assert!(required(&[S::Malformed, S::Off, S::Off]));
    // With nothing declared, the SDK's default: a subject is required.
    assert!(required(&[S::Unset, S::Unset]));
    assert!(required(&[]));
}

/// The rules a generated schema cannot carry are the domain's: a fact
/// candidate whose provenance is no digest is refused and nothing is stored.
#[tokio::test]
async fn a_record_that_breaks_its_types_rules_beyond_the_schema_is_refused() {
    let db = inmem_db().await;
    let fixture = IntakeFixture::default();
    let intake = fixture.build(&db);
    let mut record = crate::test_support::example_records()
        .into_iter()
        .find(|record| record["type"] == construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE)
        .expect("a fact candidate example");
    record["provenance"] = json!("board/whatever");

    let result = intake.submit(&connector(), Uuid::new_v4(), record).await;

    let Err(DomainError::Refused(refusal)) = result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert_eq!(refusal.reason, RefusalReason::SchemaViolation);
    assert_eq!(refusal.place, Place::pointer("/provenance"));
    assert_eq!(refusal.rule, "pattern");
    assert_eq!(
        refusal.type_id.as_deref(),
        Some(construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE)
    );
    assert!(fixture.received().is_empty());
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[tokio::test]
async fn a_record_not_taken_within_the_receive_window_is_unavailable_and_keeps_nothing() {
    let db = inmem_db().await;
    // On the real clock: a paused one can jump past the pool's timeouts while
    // the database works on its own thread, and the pool's next in-memory
    // connection is an empty database.
    let fixture = IntakeFixture {
        tenant_default_delay: WINDOW + Duration::from_secs(1),
        ..IntakeFixture::default()
    };
    let intake = fixture.build(&db).with_receive_window(WINDOW);

    let err = intake
        .submit(
            &crate::test_support::context_in(Uuid::new_v4()),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::Unavailable(_)), "{err:?}");
    assert_eq!(stored_record_ids(&db).await, 0, "no identity is kept");
    assert!(fixture.received().is_empty(), "nothing is handed off");
}

/// The identity store, with each insert taking `takes` after its row is
/// written, so only a rollback leaves nothing. `inserts` counts the inserts
/// that started.
struct SlowIds {
    takes: Duration,
    inserts: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::domain::record_intake::RecordIdRepository for SlowIds {
    async fn insert<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        id: crate::domain::record_intake::RecordId,
    ) -> Result<crate::domain::record_intake::IdInsert, DomainError> {
        self.inserts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let inserted = crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository::new()
            .insert(conn, scope, id)
            .await;
        tokio::time::sleep(self.takes).await;
        inserted
    }
}

type SlowIntake = crate::domain::record_intake::RecordIntakeService<
    SlowIds,
    crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository,
>;

/// Intake whose tenant default takes `check` to read and whose identity
/// insert takes `insert`, handing records to `hand_off` and counting the
/// inserts that started in `inserts`.
fn slow_intake(
    db: &toolkit_db::Db,
    hand_off: &Arc<crate::test_support::RecordingHandOff>,
    inserts: &Arc<std::sync::atomic::AtomicUsize>,
    check: Duration,
    insert: Duration,
) -> SlowIntake {
    use crate::domain::record_intake::{IntakePorts, RecordIntakeService};
    use crate::domain::subject_settings::SubjectSettingsService;
    use crate::infra::connector_switch::ConfigConnectorSwitch;
    use crate::infra::record_types::RegistryRecordTypes;
    use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;

    let provider = Arc::new(toolkit_db::DBProvider::new(db.clone()));
    let enforcer =
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(crate::test_support::AllowResolver));
    RecordIntakeService::new(
        Arc::clone(&provider),
        Arc::new(SlowIds {
            takes: insert,
            inserts: Arc::clone(inserts),
        }),
        Arc::new(SubjectSettingsService::new(
            provider,
            Arc::new(SeaOrmSubjectSettingsRepository::new()),
            Arc::new(crate::test_support::DelayedDefaults(true, check)),
            enforcer.clone(),
        )),
        IntakePorts {
            types: Arc::new(
                RegistryRecordTypes::new(crate::test_support::hub_with_record_types()).unwrap(),
            ),
            switch: Arc::new(ConfigConnectorSwitch::new(Vec::new())),
            hand_off: Arc::clone(hand_off) as _,
            retention: crate::test_support::no_retention(),
            generations: crate::test_support::no_erasure(),
        },
        enforcer,
    )
    .with_receive_window(WINDOW)
}

/// The receive window of [`slow_intake`]: short enough to run on the real
/// clock. Each test's phases either overrun it whatever the database takes
/// or leave it at least four fifths of it, so the database's own time does
/// not decide a test.
const WINDOW: Duration = Duration::from_secs(3);

/// Submit one record on the real clock. A paused clock would jump to the
/// receive window's deadline while the database works on its own thread,
/// before the phase under test began.
async fn submit_once(intake: &SlowIntake) -> Result<IntakeOutcome, DomainError> {
    intake
        .submit(
            &crate::test_support::context_in(Uuid::new_v4()),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
}

#[tokio::test]
async fn an_identity_insert_past_the_receive_window_is_rolled_back_before_the_hand_off() {
    let db = inmem_db().await;
    let hand_off = Arc::new(crate::test_support::RecordingHandOff::default());
    let inserts = Arc::default();
    let intake = slow_intake(
        &db,
        &hand_off,
        &inserts,
        Duration::ZERO,
        WINDOW + Duration::from_secs(1),
    );

    let err = submit_once(&intake).await.unwrap_err();

    assert!(matches!(err, DomainError::Unavailable(_)), "{err:?}");
    assert_eq!(
        inserts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the identity was written before the window ran out"
    );
    assert_eq!(
        stored_record_ids(&db).await,
        0,
        "the written identity is rolled back"
    );
    assert!(
        hand_off.received.lock().unwrap().is_empty(),
        "nothing is handed off"
    );
}

#[tokio::test]
async fn the_check_and_the_insert_share_one_receive_window() {
    // The insert alone fits in the window, but not after the check, whatever
    // the database takes, as its time only adds. The check leaves four
    // fifths of the window to the database to start the insert in time.
    let (check, insert) = (WINDOW / 5, WINDOW * 9 / 10);
    let db = inmem_db().await;
    let hand_off = Arc::new(crate::test_support::RecordingHandOff::default());
    let inserts = Arc::default();
    let intake = slow_intake(&db, &hand_off, &inserts, check, insert);

    let err = submit_once(&intake).await.unwrap_err();

    assert!(matches!(err, DomainError::Unavailable(_)), "{err:?}");
    assert_eq!(
        inserts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the check ended in time and the insert ran out of what it left"
    );
    assert_eq!(stored_record_ids(&db).await, 0, "nothing is kept");
    assert!(hand_off.received.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_check_and_an_insert_that_fit_in_the_receive_window_together_take_the_record() {
    // Slow phases that leave four fifths of the window to the database.
    let half = WINDOW / 10;
    let db = inmem_db().await;
    let hand_off = Arc::new(crate::test_support::RecordingHandOff::default());
    let inserts = Arc::default();
    let intake = slow_intake(&db, &hand_off, &inserts, half, half);

    let taken = submit_once(&intake).await.unwrap();

    assert_eq!(taken, IntakeOutcome::Received);
    assert_eq!(stored_record_ids(&db).await, 1);
    assert_eq!(hand_off.received.lock().unwrap().len(), 1);
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_record_taken_after_its_receive_window_is_named_in_a_warning_and_a_late_repeat_is_not() {
    use crate::domain::record_intake::{IdentityParts, RecordKey, warn_if_committed_late};

    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let key = RecordKey::of(IdentityParts {
        connector: "connector",
        provenance: "chat/t/1",
        version: "v1",
    });
    let now = tokio::time::Instant::now();
    let (passed, ahead) = (now - Duration::from_secs(1), now + Duration::from_secs(60));

    warn_if_committed_late(
        passed,
        &Ok(IntakeOutcome::Repeat),
        tenant,
        Some(subject),
        &key,
    );
    warn_if_committed_late(
        ahead,
        &Ok(IntakeOutcome::Received),
        tenant,
        Some(subject),
        &key,
    );
    assert!(
        !logs_contain("committed after its receive window"),
        "a repeat stored nothing, and a commit in time is not late"
    );

    warn_if_committed_late(
        passed,
        &Ok(IntakeOutcome::Received),
        tenant,
        Some(subject),
        &key,
    );
    assert!(logs_contain("committed after its receive window"));
    assert!(logs_contain(&subject.to_string()));
    assert!(logs_contain(key.as_str()));
    assert!(logs_contain("late_ms="));
}

#[tokio::test]
async fn a_record_observed_before_the_tenants_retention_period_is_refused_and_one_within_it_taken()
{
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let intake = IntakeFixture {
        retention: crate::config::RetentionConfig {
            default_days: None,
            tenants: [(tenant, std::num::NonZeroU32::new(30).unwrap())].into(),
        },
        ..IntakeFixture::default()
    }
    .build(&db);
    let subject = Uuid::new_v4();
    let observed = |days: i64| {
        json!(
            (time::OffsetDateTime::now_utc() - time::Duration::days(days))
                .format(&Rfc3339)
                .unwrap()
        )
    };

    let mut old = chat_record(subject, "chat/t/1", "v1");
    old["observed_at"] = observed(31);
    let result = intake.submit(&connector(), tenant, old.clone()).await;
    assert_eq!(refusal_reason(&result), Some(RefusalReason::PastRetention));
    let Err(DomainError::Refused(refusal)) = result else {
        unreachable!("refused above");
    };
    assert_eq!(refusal.place, Place::pointer("/observed_at"));
    assert_eq!(stored_record_ids(&db).await, 0, "nothing is kept");

    let mut recent = chat_record(subject, "chat/t/2", "v1");
    recent["observed_at"] = observed(29);
    assert_eq!(
        intake.submit(&connector(), tenant, recent).await.unwrap(),
        IntakeOutcome::Received
    );
    assert_eq!(
        intake
            .submit(&connector(), Uuid::new_v4(), old)
            .await
            .unwrap(),
        IntakeOutcome::Received,
        "a tenant without a period keeps everything"
    );
}
