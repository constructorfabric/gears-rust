use std::sync::Arc;

use quota_resolver_sdk::QUOTA_TENANT_SUBJECT_TYPE_ID;
use quota_resolver_sdk::QuotaResolverPluginClient;
use quota_resolver_sdk::models::{
    ChargeOperation, ChargeRef, DenialReason, LeaseOutcome, MetricAmount,
    PlatformAcquireLeaseRequest, PlatformCommitLeaseRequest, PlatformDebitRequest,
    PlatformQuotaStateRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest,
    PrincipalSubject, QuotaStateMetric, QuotaStateRequest, QuotaSubject, SubjectInput,
};
use std::time::Duration;
use toolkit::client_hub::ClientScope;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformIdentity, SecurityContext};

use types_registry_sdk::testing_platform::{Call, Fault};
use uuid::Uuid;

use super::Service;
use crate::domain::test_support::*;
use crate::domain::{Operation, Outcome, Plane};

fn principal(type_id: &str) -> SubjectInput {
    SubjectInput::Principal(PrincipalSubject {
        subject_type: type_id.to_owned(),
        id: principal_id(),
    })
}

fn quota_subject(type_id: &str, id: &str) -> SubjectInput {
    SubjectInput::Quota(QuotaSubject {
        subject_type: type_id.to_owned(),
        id: id.to_owned(),
    })
}

fn debit_request(subjects: Vec<SubjectInput>) -> PlatformDebitRequest {
    PlatformDebitRequest {
        tenant_id: tenant(),
        subjects,
        items: vec![item()],
        idempotency_key: "k".to_owned(),
    }
}

fn lease_request(subjects: Vec<SubjectInput>) -> PlatformAcquireLeaseRequest {
    PlatformAcquireLeaseRequest {
        tenant_id: tenant(),
        subjects,
        items: vec![item()],
        ttl_secs: 30,
        idempotency_key: "k".to_owned(),
    }
}

fn state_request(subject: SubjectInput) -> PlatformQuotaStateRequest {
    PlatformQuotaStateRequest {
        tenant_id: tenant(),
        subject,
        metrics: vec![QuotaStateMetric::of(METRIC_TYPE)],
        limit: Some(7),
        cursor: None,
    }
}

fn tenant_state_request() -> QuotaStateRequest {
    QuotaStateRequest {
        metrics: vec![QuotaStateMetric::of(METRIC_TYPE)],
        limit: Some(7),
        cursor: Some("c".to_owned()),
    }
}

fn user_ctx(type_id: Option<&str>, subject: Uuid, tenant_id: Uuid) -> SecurityContext {
    let mut builder = SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant_id);
    if let Some(type_id) = type_id {
        builder = builder.subject_type(type_id);
    }
    builder.build().unwrap()
}

fn is_unavailable(err: &CanonicalError) -> bool {
    matches!(err, CanonicalError::ServiceUnavailable { .. })
}

fn is_invalid(err: &CanonicalError) -> bool {
    matches!(err, CanonicalError::InvalidArgument { .. })
}

fn user_subject_for(id: Uuid) -> QuotaSubject {
    QuotaSubject {
        subject_type: USER_SUBJECT.to_owned(),
        id: id.to_string(),
    }
}

fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable().create()
}

async fn every_operation(svc: &Service) -> Vec<Result<(), CanonicalError>> {
    let subject = || vec![principal(USER_TYPE)];
    vec![
        svc.debit(debit_request(subject()), None).await.map(drop),
        svc.acquire_lease(lease_request(subject()), None)
            .await
            .map(drop),
        svc.commit_lease(
            PlatformCommitLeaseRequest {
                tenant_id: tenant(),
                token: "t".to_owned(),
                amounts: None,
                idempotency_key: "k".to_owned(),
            },
            None,
        )
        .await,
        svc.release_lease(
            PlatformReleaseLeaseRequest {
                tenant_id: tenant(),
                token: "t".to_owned(),
                idempotency_key: "k".to_owned(),
            },
            None,
        )
        .await,
        svc.return_amount(
            PlatformReturnRequest {
                tenant_id: tenant(),
                charge: ChargeRef {
                    operation: ChargeOperation::Debit,
                    idempotency_key: "d".to_owned(),
                },
                amounts: vec![MetricAmount {
                    metric_type: METRIC_TYPE.to_owned(),
                    amount: 1,
                }],
                idempotency_key: "k".to_owned(),
            },
            None,
        )
        .await,
        svc.quota_state(state_request(principal(USER_TYPE)), None)
            .await
            .map(drop),
        svc.tenant_quota_state(
            &user_ctx(Some(USER_TYPE), principal_id(), tenant()),
            tenant_state_request(),
        )
        .await
        .map(drop),
    ]
}

fn bind_plugin(env: &Env, name: &str, vendor: &str, priority: i16) -> Arc<MockPlugin> {
    let plugin = MockPlugin::new();
    let id = instance_id(name);
    seed_plugin(&env.registry, &id, vendor, priority);
    env.hub.register_scoped::<dyn QuotaResolverPluginClient>(
        ClientScope::gts_id(&id),
        plugin.clone() as Arc<dyn QuotaResolverPluginClient>,
    );
    plugin
}

#[tokio::test]
async fn the_plugin_is_chosen_by_vendor_and_lowest_priority() {
    let env = Env::new();
    let better = bind_plugin(&env, "better", VENDOR, -5);
    let other_vendor = bind_plugin(&env, "other", "someone-else", -50);

    env.service()
        .debit(debit_request(vec![quota_subject(USER_SUBJECT, "u")]), None)
        .await
        .unwrap();

    assert_eq!(better.calls(), 1);
    assert_eq!(env.plugin.calls(), 0);
    assert_eq!(other_vendor.calls(), 0);
}

#[tokio::test]
async fn every_operation_without_a_plugin_for_the_vendor_is_service_unavailable() {
    let env = Env::new();
    seed_type(&env.registry, USER_TYPE);
    let svc = Service::new(
        env.hub.clone(),
        "nobody".to_owned(),
        [(USER_TYPE.to_owned(), USER_SUBJECT.to_owned())].into(),
        env.metrics.clone(),
    );

    for result in every_operation(&svc).await {
        assert!(is_unavailable(&result.unwrap_err()));
    }
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn an_advertised_plugin_without_a_client_is_service_unavailable() {
    let env = Env::new();
    env.hub
        .remove_scoped::<dyn QuotaResolverPluginClient>(&ClientScope::gts_id(&instance_id("mock")));

    let err = env
        .service()
        .debit(debit_request(vec![quota_subject(USER_SUBJECT, "u")]), None)
        .await
        .unwrap_err();

    assert!(is_unavailable(&err));
}

#[tokio::test]
async fn an_unavailable_registry_is_service_unavailable() {
    let env = Env::new();
    env.registry.inject(Fault::any().fail(unavailable()));

    let err = env
        .service()
        .debit(debit_request(vec![quota_subject(USER_SUBJECT, "u")]), None)
        .await
        .unwrap_err();

    assert!(is_unavailable(&err));
    assert_eq!(env.metrics.last().2, Outcome::Error);
}

#[tokio::test]
async fn the_plugin_selection_is_memoized() {
    let env = Env::new();
    let svc = env.service();
    for _ in 0..3 {
        svc.debit(debit_request(vec![quota_subject(USER_SUBJECT, "u")]), None)
            .await
            .unwrap();
    }
    assert_eq!(env.registry.calls(Call::List), 1);
}

#[tokio::test]
async fn a_failed_selection_is_not_memoized() {
    let env = Env::new();
    env.registry
        .inject(Fault::on(Call::List).first(1).fail(unavailable()));
    let svc = env.service();
    let request = || debit_request(vec![quota_subject(USER_SUBJECT, "u")]);

    assert!(svc.debit(request(), None).await.is_err());
    assert!(svc.debit(request(), None).await.unwrap().allowed);
}

#[tokio::test]
async fn an_exact_entry_maps_without_asking_the_registry() {
    let env = Env::new();

    let decision = env
        .service()
        .debit(debit_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap();

    assert!(decision.allowed);
    let seen = env.plugin.debits.lock().unwrap();
    assert_eq!(seen[0].subjects, vec![user_subject_for(principal_id())]);
    assert_eq!(env.registry.calls(Call::BatchGet), 0);
}

#[tokio::test]
async fn the_nearest_ancestor_entry_maps_a_subtype() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);
    let svc = env.service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT), (USER_TYPE, USER_SUBJECT)]);

    svc.debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();

    let seen = env.plugin.debits.lock().unwrap();
    assert_eq!(seen[0].subjects[0].subject_type, USER_SUBJECT);
}

#[tokio::test]
async fn a_farther_ancestor_entry_maps_when_no_nearer_one_exists() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);
    let svc = env.service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT)]);

    svc.debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();

    let seen = env.plugin.debits.lock().unwrap();
    assert_eq!(seen[0].subjects[0].subject_type, ADMIN_SUBJECT);
}

#[tokio::test]
async fn a_registered_type_without_an_entry_on_its_chain_is_not_mapped() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);

    let decision = env
        .service_with(&[])
        .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();

    assert!(!decision.allowed);
    assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn an_empty_mapping_is_not_mapped_without_asking_the_registry() {
    let env = Env::new();
    env.registry
        .inject(Fault::on(Call::BatchGet).fail(unavailable()));

    let decision = env
        .service_with(&[])
        .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();

    assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(env.registry.calls(Call::BatchGet), 0);
}

#[tokio::test]
async fn the_resolved_chain_is_memoized_per_registered_type() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);
    let svc = env.service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT)]);
    for _ in 0..3 {
        svc.debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
            .await
            .unwrap();
    }
    assert_eq!(env.registry.calls(Call::BatchGet), 1);
}

#[tokio::test]
async fn an_unregistered_type_is_not_mapped_and_not_memoized() {
    let env = Env::new();
    let svc = env.service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT)]);

    for _ in 0..2 {
        let decision = svc
            .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
            .await
            .unwrap();
        assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    }
    assert_eq!(env.registry.calls(Call::BatchGet), 2);

    seed_type(&env.registry, ADMIN_TYPE);
    let decision = svc
        .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();
    assert!(decision.allowed);
}

#[tokio::test]
async fn a_deleted_type_is_not_mapped() {
    let env = Env::new();
    seed_deleted_type(&env.registry, ADMIN_TYPE);

    let decision = env
        .service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT)])
        .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();

    assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn a_malformed_type_is_not_mapped_without_asking_the_registry() {
    let env = Env::new();

    let decision = env
        .service()
        .debit(debit_request(vec![principal("not a gts id")]), None)
        .await
        .unwrap();

    assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(env.registry.calls(Call::BatchGet), 0);
}

#[tokio::test]
async fn a_nil_principal_id_is_not_mapped() {
    let env = Env::new();
    let nil = SubjectInput::Principal(PrincipalSubject {
        subject_type: USER_TYPE.to_owned(),
        id: Uuid::nil(),
    });

    let decision = env
        .service()
        .debit(debit_request(vec![nil]), None)
        .await
        .unwrap();

    assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_registry_that_does_not_answer_the_plugin_listing_is_service_unavailable() {
    let env = Env::new();
    env.registry
        .inject(Fault::on(Call::List).delay(Duration::from_secs(3600)));

    let err = env
        .service()
        .debit(debit_request(vec![quota_subject(USER_SUBJECT, "u")]), None)
        .await
        .unwrap_err();

    assert!(is_unavailable(&err));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_registry_that_does_not_answer_while_mapping_is_service_unavailable() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);
    env.registry
        .inject(Fault::on(Call::BatchGet).delay(Duration::from_secs(3600)));

    let err = env
        .service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT)])
        .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap_err();

    assert!(is_unavailable(&err));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn a_registry_fault_that_is_not_an_outage_is_not_reported_as_retryable() {
    let env = Env::new();
    env.registry
        .inject(Fault::on(Call::List).fail(CanonicalError::internal("malformed page").create()));

    let err = env
        .service()
        .debit(debit_request(vec![quota_subject(USER_SUBJECT, "u")]), None)
        .await
        .unwrap_err();

    assert!(matches!(err, CanonicalError::Internal { .. }), "{err:?}");
}

#[tokio::test]
async fn a_registry_failure_while_mapping_is_service_unavailable() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);
    env.registry
        .inject(Fault::on(Call::BatchGet).fail(unavailable()));

    let err = env
        .service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT)])
        .debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap_err();

    assert!(is_unavailable(&err));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn after_warm_up_a_failing_registry_does_not_stop_resolved_types() {
    let env = Env::new();
    seed_type(&env.registry, ADMIN_TYPE);
    let svc = env.service_with(&[(PRINCIPAL_BASE, ADMIN_SUBJECT), (USER_TYPE, USER_SUBJECT)]);
    svc.debit(debit_request(vec![principal(ADMIN_TYPE)]), None)
        .await
        .unwrap();

    env.registry.inject(Fault::any().fail(unavailable()));

    for type_id in [ADMIN_TYPE, USER_TYPE] {
        let decision = svc
            .debit(debit_request(vec![principal(type_id)]), None)
            .await
            .unwrap();
        assert!(decision.allowed);
    }
    let page = svc
        .quota_state(state_request(principal(ADMIN_TYPE)), None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn a_principal_that_is_not_mapped_never_reaches_the_plugin() {
    let env = Env::new();
    let svc = env.service_with(&[]);
    let subjects = || vec![principal(USER_TYPE)];

    let debit = svc.debit(debit_request(subjects()), None).await.unwrap();
    assert!(!debit.allowed);
    assert_eq!(debit.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(
        env.metrics.last(),
        (
            Operation::Debit,
            Plane::Platform,
            Outcome::PrincipalNotMapped
        )
    );

    let lease = svc
        .acquire_lease(lease_request(subjects()), None)
        .await
        .unwrap();
    match lease {
        LeaseOutcome::Denied { decision } => {
            assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
        }
        other => panic!("expected a denial, got {other:?}"),
    }

    let page = svc
        .quota_state(state_request(principal(USER_TYPE)), None)
        .await
        .unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.page_info.limit, 7);
    assert_eq!(
        env.metrics.last(),
        (
            Operation::QuotaState,
            Plane::Platform,
            Outcome::PrincipalNotMapped
        )
    );

    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn one_unmapped_principal_among_several_subjects_denies_the_request() {
    let env = Env::new();

    let decision = env
        .service()
        .debit(
            debit_request(vec![
                quota_subject(USER_SUBJECT, "x"),
                principal(ADMIN_TYPE),
            ]),
            None,
        )
        .await
        .unwrap();

    assert_eq!(decision.reason, Some(DenialReason::PrincipalNotMapped));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn the_platform_plane_passes_exactly_the_request_tenant() {
    let env = Env::new();
    let svc = env.service();
    let other_tenant = Uuid::from_u128(77);
    let mut debit = debit_request(vec![principal(USER_TYPE)]);
    debit.tenant_id = other_tenant;
    svc.debit(debit, None).await.unwrap();
    let mut lease = lease_request(vec![principal(USER_TYPE)]);
    lease.tenant_id = other_tenant;
    svc.acquire_lease(lease, None).await.unwrap();
    let mut state = state_request(principal(USER_TYPE));
    state.tenant_id = other_tenant;
    svc.quota_state(state, None).await.unwrap();
    svc.commit_lease(
        PlatformCommitLeaseRequest {
            tenant_id: other_tenant,
            token: "t".to_owned(),
            amounts: None,
            idempotency_key: "c".to_owned(),
        },
        None,
    )
    .await
    .unwrap();
    svc.release_lease(
        PlatformReleaseLeaseRequest {
            tenant_id: other_tenant,
            token: "t".to_owned(),
            idempotency_key: "r".to_owned(),
        },
        None,
    )
    .await
    .unwrap();
    svc.return_amount(
        PlatformReturnRequest {
            tenant_id: other_tenant,
            charge: ChargeRef {
                operation: ChargeOperation::Debit,
                idempotency_key: "d".to_owned(),
            },
            amounts: vec![MetricAmount {
                metric_type: METRIC_TYPE.to_owned(),
                amount: 1,
            }],
            idempotency_key: "g".to_owned(),
        },
        None,
    )
    .await
    .unwrap();

    assert_eq!(env.plugin.debits.lock().unwrap()[0].tenant_id, other_tenant);
    assert_eq!(env.plugin.leases.lock().unwrap()[0].tenant_id, other_tenant);
    assert_eq!(env.plugin.states.lock().unwrap()[0].tenant_id, other_tenant);
    assert_eq!(
        *env.plugin.settlements.lock().unwrap(),
        vec![(other_tenant, None); 3]
    );
}

#[tokio::test]
async fn the_tenant_plane_reads_the_callers_mapped_subject_in_the_callers_tenant() {
    let env = Env::new();
    let ctx = user_ctx(Some(USER_TYPE), principal_id(), tenant());

    let page = env
        .service()
        .tenant_quota_state(&ctx, tenant_state_request())
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    let seen = env.plugin.states.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tenant_id, tenant());
    assert_eq!(seen[0].subject, user_subject_for(principal_id()));
    assert_eq!(seen[0].metrics, vec![QuotaStateMetric::of(METRIC_TYPE)]);
    assert_eq!(seen[0].cursor.as_deref(), Some("c"));
    assert!(seen[0].caller.is_none());
    assert_eq!(
        env.metrics.last(),
        (Operation::QuotaState, Plane::Tenant, Outcome::Allowed)
    );
}

#[tokio::test]
async fn the_tenant_plane_without_a_mappable_caller_gets_an_empty_page() {
    let env = Env::new();
    let svc = env.service();
    let contexts = [
        user_ctx(None, principal_id(), tenant()),
        user_ctx(Some(USER_TYPE), Uuid::nil(), tenant()),
        user_ctx(Some(USER_TYPE), principal_id(), Uuid::nil()),
        user_ctx(Some(ADMIN_TYPE), principal_id(), tenant()),
    ];

    for ctx in &contexts {
        let page = svc
            .tenant_quota_state(ctx, tenant_state_request())
            .await
            .unwrap();
        assert!(page.items.is_empty());
        assert_eq!(
            env.metrics.last(),
            (
                Operation::QuotaState,
                Plane::Tenant,
                Outcome::PrincipalNotMapped
            )
        );
    }
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn the_tenant_plane_rejects_more_than_100_metrics() {
    let env = Env::new();
    let mut request = tenant_state_request();
    request.metrics = vec![QuotaStateMetric::of(METRIC_TYPE); 101];

    let err = env
        .service()
        .tenant_quota_state(&user_ctx(None, principal_id(), tenant()), request)
        .await
        .unwrap_err();

    assert!(is_invalid(&err));
}

#[tokio::test]
async fn a_quota_state_read_without_a_metric_is_invalid_and_calls_no_backend() {
    let env = Env::new();
    let mut request = tenant_state_request();
    request.metrics = Vec::new();

    let err = env
        .service()
        .tenant_quota_state(&user_ctx(None, principal_id(), tenant()), request)
        .await
        .unwrap_err();

    assert!(is_invalid(&err));
    assert_eq!(env.plugin.calls(), 0);
}

/// The fields an `InvalidArgument` names, as the caller sent them.
fn fields_of(error: &CanonicalError) -> Vec<String> {
    match error {
        CanonicalError::InvalidArgument {
            ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
            ..
        } => field_violations.iter().map(|v| v.field.clone()).collect(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn a_tenant_subject_of_another_tenant_is_invalid() {
    let env = Env::new();
    let svc = env.service();
    let other = quota_subject(
        QUOTA_TENANT_SUBJECT_TYPE_ID,
        &Uuid::from_u128(9).to_string(),
    );

    let err = svc
        .debit(debit_request(vec![other.clone()]), None)
        .await
        .unwrap_err();
    assert!(is_invalid(&err));
    let err = svc
        .acquire_lease(lease_request(vec![other.clone()]), None)
        .await
        .unwrap_err();
    assert!(is_invalid(&err));
    let err = svc
        .quota_state(state_request(other.clone()), None)
        .await
        .unwrap_err();
    assert_eq!(fields_of(&err), ["subject.id"]);
    let err = svc
        .debit(debit_request(vec![principal(ADMIN_TYPE), other]), None)
        .await
        .unwrap_err();
    assert_eq!(fields_of(&err), ["subjects[1].id"]);
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn a_tenant_subject_in_a_non_canonical_form_is_invalid() {
    let env = Env::new();
    let svc = env.service();
    let id = tenant();
    for form in [
        id.to_string().to_uppercase(),
        id.urn().to_string(),
        id.braced().to_string(),
        id.simple().to_string(),
    ] {
        let subject = quota_subject(QUOTA_TENANT_SUBJECT_TYPE_ID, &form);
        let err = svc
            .debit(debit_request(vec![subject.clone()]), None)
            .await
            .unwrap_err();
        assert!(is_invalid(&err), "{form}");
        let err = svc
            .quota_state(state_request(subject), None)
            .await
            .unwrap_err();
        assert!(is_invalid(&err), "{form}");
    }
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn a_quota_subject_with_an_empty_id_is_invalid() {
    let env = Env::new();
    let svc = env.service();
    let empty = quota_subject(USER_SUBJECT, "");

    let err = svc
        .debit(debit_request(vec![empty.clone()]), None)
        .await
        .unwrap_err();
    assert!(is_invalid(&err));
    let err = svc
        .acquire_lease(lease_request(vec![empty.clone()]), None)
        .await
        .unwrap_err();
    assert!(is_invalid(&err));
    let err = svc
        .quota_state(state_request(empty), None)
        .await
        .unwrap_err();
    assert!(is_invalid(&err));
    assert_eq!(env.plugin.calls(), 0);
}

#[tokio::test]
async fn a_tenant_subject_of_the_request_tenant_is_passed_on() {
    let env = Env::new();
    let own = quota_subject(QUOTA_TENANT_SUBJECT_TYPE_ID, &tenant().to_string());

    let decision = env
        .service()
        .debit(debit_request(vec![own]), None)
        .await
        .unwrap();

    assert!(decision.allowed);
    assert_eq!(
        env.plugin.debits.lock().unwrap()[0].subjects[0].subject_type,
        QUOTA_TENANT_SUBJECT_TYPE_ID
    );
}

#[tokio::test]
async fn a_charge_without_a_subject_or_an_item_is_invalid() {
    let env = Env::new();
    let svc = env.service();

    let err = svc.debit(debit_request(vec![]), None).await.unwrap_err();
    assert!(is_invalid(&err));
    let err = svc
        .acquire_lease(lease_request(vec![]), None)
        .await
        .unwrap_err();
    assert!(is_invalid(&err));

    let mut no_items = debit_request(vec![principal(USER_TYPE)]);
    no_items.items.clear();
    assert!(is_invalid(&svc.debit(no_items, None).await.unwrap_err()));
    let mut no_items = lease_request(vec![principal(USER_TYPE)]);
    no_items.items.clear();
    let err = svc.acquire_lease(no_items, None).await.unwrap_err();
    assert!(is_invalid(&err));

    assert_eq!(env.plugin.calls(), 0);
    assert_eq!(env.metrics.last().2, Outcome::Error);
}

#[tokio::test]
async fn more_than_one_metric_item_is_passed_to_the_plugin() {
    let env = Env::new();
    let mut request = debit_request(vec![principal(USER_TYPE)]);
    request.items = vec![item(), item()];

    env.service().debit(request, None).await.unwrap();

    assert_eq!(env.plugin.debits.lock().unwrap()[0].items.len(), 2);
}

#[tokio::test]
async fn quota_state_takes_at_most_100_metrics() {
    let env = Env::new();
    let svc = env.service();
    let mut request = state_request(principal(USER_TYPE));

    request.metrics = vec![QuotaStateMetric::of(METRIC_TYPE); 100];
    assert!(svc.quota_state(request.clone(), None).await.is_ok());

    request.metrics = vec![QuotaStateMetric::of(METRIC_TYPE); 101];
    let err = svc.quota_state(request, None).await.unwrap_err();
    assert!(is_invalid(&err));
}

#[tokio::test]
async fn the_quota_state_page_is_returned_unchanged() {
    let env = Env::new();

    let page = env
        .service()
        .quota_state(state_request(principal(USER_TYPE)), None)
        .await
        .unwrap();

    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        serde_json::to_value(page_of_one()).unwrap()
    );
}

#[tokio::test]
async fn a_plugin_error_passes_through_unchanged() {
    let makers: [fn() -> CanonicalError; 3] = [aborted, already_exists, || {
        CanonicalError::internal("boom").create()
    }];
    for make in makers {
        let expected = format!("{:?}", make());
        let env = Env::with_plugin(MockPlugin::failing(make()));
        let svc = env.service();

        for result in every_operation(&svc).await {
            assert_eq!(format!("{:?}", result.unwrap_err()), expected);
        }
        assert_eq!(env.metrics.last().2, Outcome::Error);
    }
}

#[tokio::test]
async fn an_invalid_argument_from_the_plugin_for_a_mapped_principal_passes_through() {
    let env = Env::with_plugin(MockPlugin::failing(invalid_argument()));

    let err = env
        .service()
        .debit(debit_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap_err();

    assert_eq!(format!("{err:?}"), format!("{:?}", invalid_argument()));
}

#[tokio::test]
async fn the_plugin_receives_the_caller_the_service_is_given() {
    let env = Env::new();
    let svc = env.service();
    let identity = PlatformIdentity::Shared {
        name: "billing".to_owned(),
    };

    svc.debit(
        debit_request(vec![principal(USER_TYPE)]),
        Some(identity.clone()),
    )
    .await
    .unwrap();
    svc.debit(debit_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap();

    let seen = env.plugin.debits.lock().unwrap();
    assert_eq!(seen[0].caller, Some(identity));
    assert_eq!(seen[1].caller, None);
}

#[tokio::test]
async fn outcomes_and_overhead_are_recorded() {
    let env = Env::new();
    let svc = env.service();

    svc.debit(debit_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap();
    assert_eq!(
        env.metrics.last(),
        (Operation::Debit, Plane::Platform, Outcome::Allowed)
    );
    svc.acquire_lease(lease_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap();
    assert_eq!(
        env.metrics.last(),
        (Operation::AcquireLease, Plane::Platform, Outcome::Allowed)
    );
    assert!(svc.debit(debit_request(vec![]), None).await.is_err());
    assert_eq!(env.metrics.last().2, Outcome::Error);

    assert_eq!(*env.metrics.overheads.lock().unwrap(), 2);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_rejected_debit_for_a_mapped_principal_warns_with_both_types() {
    let env = Env::with_plugin(MockPlugin::failing(invalid_argument()));

    env.service()
        .debit(debit_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap_err();

    assert!(logs_contain(&format!("principal_type={USER_TYPE}")));
    assert!(logs_contain(&format!("quota_subject_type={USER_SUBJECT}")));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_rejected_lease_for_a_mapped_principal_warns_with_both_types() {
    let env = Env::with_plugin(MockPlugin::failing(invalid_argument()));

    env.service()
        .acquire_lease(lease_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap_err();

    assert!(logs_contain(&format!("principal_type={USER_TYPE}")));
    assert!(logs_contain(&format!("quota_subject_type={USER_SUBJECT}")));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn no_mapping_warning_without_a_principal_or_for_a_state_read() {
    let env = Env::with_plugin(MockPlugin::failing(invalid_argument()));
    let svc = env.service();
    let subject = || vec![quota_subject(USER_SUBJECT, "u1")];

    svc.debit(debit_request(subject()), None).await.unwrap_err();
    svc.acquire_lease(lease_request(subject()), None)
        .await
        .unwrap_err();
    svc.quota_state(state_request(principal(USER_TYPE)), None)
        .await
        .unwrap_err();
    svc.tenant_quota_state(
        &user_ctx(Some(USER_TYPE), principal_id(), tenant()),
        tenant_state_request(),
    )
    .await
    .unwrap_err();

    assert!(!logs_contain("principal_type"));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_rejection_that_does_not_name_the_subjects_does_not_warn_about_the_mapping() {
    let bad_amount = crate::domain::test_support::PluginError::invalid_argument()
        .with_field_violation("items[0].amount", "must be positive", "INVALID_AMOUNT")
        .create();
    let env = Env::with_plugin(MockPlugin::failing(bad_amount));

    env.service()
        .debit(debit_request(vec![principal(USER_TYPE)]), None)
        .await
        .unwrap_err();

    assert!(!logs_contain("principal_type"));
}
