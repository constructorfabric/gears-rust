#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use gts::{GtsInstanceId, GtsTypeId};
use quota_enforcement_sdk::{METRIC_BASE_TYPE, REQUEST_BASE, SCOPE_TYPE, SCOPE_USER, SUBJECT_BASE};
use serde_json::json;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::CompilePayloadValidatorError;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use super::TypesRegistryContracts;
use crate::domain::error::{CallerRefusal, DomainError};
use crate::domain::ports::contracts::{ContractRegistry, RegistryCaller};
use crate::test_support::{
    LLM_TOKEN_REQUEST, LLM_USER_PROJECTION, METRIC_OTHER, METRIC_TOKENS, MIXIN_REQUEST, MIXIN_TYPE,
    ManagedRegistry, ctx, hub_over, llm_gateway_documents, metric_base_documents,
    mixin_request_document, mixin_type_document, mock_registry,
};

const PLATFORM: RegistryCaller<'static> = RegistryCaller::Platform;

fn adapter(hub: Arc<ClientHub>) -> TypesRegistryContracts {
    TypesRegistryContracts::new(hub).with_deadline(Duration::from_secs(2))
}

fn over(registry: &Arc<MockTypesRegistry>) -> TypesRegistryContracts {
    adapter(hub_over(registry))
}

fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

#[tokio::test]
async fn a_derived_contract_resolves_with_its_traits_dialect_and_inlined_parent() {
    let registry = over(&mock_registry(&llm_gateway_documents(), &[]));
    let user = registry
        .type_schema(PLATFORM, &type_id(LLM_USER_PROJECTION))
        .await
        .expect("registry answers")
        .expect("the projection is registered");
    assert!(!user.is_abstract);
    assert_eq!(user.ancestors, vec![type_id(SUBJECT_BASE)]);
    assert!(user.derives_from(SUBJECT_BASE));
    assert_eq!(user.effective_traits["scope"], SCOPE_USER);
    let payload = user.payload.as_ref().expect("the projection compiles");
    assert_eq!(payload.type_id(), &type_id(LLM_USER_PROJECTION));

    let base = registry
        .type_schema(PLATFORM, &type_id(SUBJECT_BASE))
        .await
        .expect("registry answers")
        .expect("the base is registered");
    assert!(base.is_abstract);
    assert!(base.ancestors.is_empty());
    assert!(
        matches!(
            base.payload,
            Err(CompilePayloadValidatorError::Upstream(
                gts::CompilePayloadValidatorError::Abstract(_)
            ))
        ),
        "an abstract type has no payload validator"
    );
}

/// A request reads under the caller's context through the tenant contract;
/// the gear's own reads go through the platform contract.
#[tokio::test]
async fn a_request_resolves_under_the_callers_context() {
    let mock = mock_registry(&llm_gateway_documents(), &[]);
    let registry = over(&mock);
    let caller = ctx();

    registry
        .type_schema(
            RegistryCaller::Request(&caller),
            &type_id(LLM_USER_PROJECTION),
        )
        .await
        .expect("answers")
        .expect("registered");
    let contexts = mock.caller_contexts(Call::BatchGet);
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].subject_id(), caller.subject_id());
    assert_eq!(mock.platform_calls(Call::BatchGet), 0);

    registry
        .type_schema(PLATFORM, &type_id(LLM_USER_PROJECTION))
        .await
        .expect("answers");
    assert_eq!(mock.platform_calls(Call::BatchGet), 1);
}

#[tokio::test]
async fn local_and_mixin_references_are_resolved_transitively() {
    let documents = vec![mixin_type_document(), mixin_request_document(METRIC_TOKENS)];
    let registry = over(&mock_registry(&documents, &[]));
    let contract = registry
        .type_schema(PLATFORM, &type_id(MIXIN_REQUEST))
        .await
        .expect("registry answers")
        .expect("registered");
    assert_eq!(contract.effective_traits["metric"], METRIC_TOKENS);

    let validator = contract
        .payload
        .as_ref()
        .expect("the resolved contract compiles");
    let good = json!({ "type": MIXIN_REQUEST, "metadata": { "region": "eu", "when": "2026-09-08T10:00:00Z" } });
    assert!(validator.is_valid(&good));
    let bad_region = json!({ "type": MIXIN_REQUEST, "metadata": { "region": "mars" } });
    assert!(!validator.is_valid(&bad_region), "the mixin's enum applies");
    let bad_time =
        json!({ "type": MIXIN_REQUEST, "metadata": { "region": "eu", "when": "yesterday" } });
    assert!(
        !validator.is_valid(&bad_time),
        "the local definition's date-time format is asserted under Draft-07"
    );
}

/// The registry's verdict on `subject` in a refused reconciliation: its kind
/// and the registry's reason.
fn verdict_on(refusal: &CanonicalError, subject: &str) -> (String, String) {
    let CanonicalError::FailedPrecondition { ctx, .. } = refusal else {
        panic!("a refused reconciliation fails its precondition: {refusal:?}");
    };
    let violation = ctx
        .violations
        .iter()
        .find(|v| v.subject == subject)
        .unwrap_or_else(|| panic!("no verdict on {subject}: {refusal:?}"));
    (violation.type_.clone(), violation.description.clone())
}

/// The registry, not QE, validates traits and references at admission: a
/// projection whose scope breaks its trait schema, or a contract whose mixin
/// is not registered, is never admitted, so QE never sees it.
#[tokio::test]
async fn admission_refuses_contracts_that_do_not_resolve() {
    let managed = ManagedRegistry::start(metric_base_documents()).await;
    let hub = Arc::new(ClientHub::new());
    managed.install(&hub);
    let registry = adapter(hub);

    let mut documents = llm_gateway_documents();
    let projection = documents
        .iter_mut()
        .find(|doc| doc["$id"] == format!("gts://{LLM_USER_PROJECTION}"))
        .expect("user projection");
    projection["x-gts-traits"]["scope"] = json!("not-an-id");
    let refusal = managed
        .try_admit("llm-gateway", documents)
        .await
        .expect_err("a scope outside its x-gts-instance-ref is refused");
    let (verdict, why) = verdict_on(&refusal, LLM_USER_PROJECTION);
    assert_eq!(verdict, "reconciliation.rejected");
    assert!(why.contains("trait validation failed"), "{why}");
    assert!(
        registry
            .type_schema(PLATFORM, &type_id(LLM_USER_PROJECTION))
            .await
            .expect("answers")
            .is_none()
    );

    let refusal = managed
        .try_admit("mixins", vec![mixin_request_document(METRIC_TOKENS)])
        .await
        .expect_err("a contract whose mixin is not registered is refused");
    let (verdict, why) = verdict_on(&refusal, MIXIN_REQUEST);
    assert_eq!(verdict, "reconciliation.pending.dependency");
    assert!(
        why.contains(&format!("'{MIXIN_TYPE}' is not registered")),
        "{why}"
    );
    for absent in [MIXIN_REQUEST, MIXIN_TYPE] {
        assert!(
            registry
                .type_schema(PLATFORM, &type_id(absent))
                .await
                .expect("answers")
                .is_none(),
            "{absent}"
        );
    }
}

/// Discovery reads the listing (content and traits) and resolves no schema.
/// A contract with a broken reference graph cannot be listed: admission
/// refuses it ([`admission_refuses_contracts_that_do_not_resolve`]).
#[tokio::test]
async fn discovery_lists_derived_types_from_the_listing() {
    let mut documents = llm_gateway_documents();
    documents.push(mixin_type_document());
    documents.push(mixin_request_document(METRIC_OTHER));
    let registry = over(&mock_registry(&documents, &[]));
    let discovered = registry
        .derived_types(&type_id(REQUEST_BASE))
        .await
        .expect("listing succeeds");
    let ids: Vec<&str> = discovered.iter().map(|d| d.id.as_ref()).collect();
    assert!(ids.contains(&LLM_TOKEN_REQUEST), "{ids:?}");
    assert!(ids.contains(&MIXIN_REQUEST), "{ids:?}");
    assert!(
        !ids.contains(&REQUEST_BASE),
        "the base is not derived from itself"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with(SUBJECT_BASE)),
        "subjects are not request contracts: {ids:?}"
    );
    let mixin = discovered
        .iter()
        .find(|d| d.id.as_ref() == MIXIN_REQUEST)
        .expect("listed");
    assert!(!mixin.is_abstract);
    assert_eq!(mixin.declared_traits["metric"], METRIC_OTHER);
}

#[tokio::test]
async fn missing_and_deleted_types_and_instances_are_none_and_failures_lift() {
    let mock = mock_registry(&llm_gateway_documents(), &metric_base_documents()[1..]);
    let registry = over(&mock);
    assert!(
        registry
            .type_schema(
                PLATFORM,
                &type_id("gts.cf.core.qe.subj.v1~cf.nobody.nothing.absent.v1~")
            )
            .await
            .expect("answers")
            .is_none()
    );
    let metric = GtsInstanceId::try_new(METRIC_TOKENS).expect("instance id");
    assert_eq!(
        registry.instance_type(&metric).await.expect("answers"),
        Some(type_id(METRIC_BASE_TYPE))
    );
    let unknown = GtsInstanceId::try_new(METRIC_OTHER).expect("instance id");
    assert_eq!(
        registry.instance_type(&unknown).await.expect("answers"),
        None
    );

    // A deleted entity is absent.
    let user = mock.content(LLM_USER_PROJECTION).expect("seeded");
    mock.seed_entity(Seed::new(LLM_USER_PROJECTION, user).deleted());
    assert!(
        registry
            .type_schema(PLATFORM, &type_id(LLM_USER_PROJECTION))
            .await
            .expect("answers")
            .is_none()
    );
    let scope = GtsInstanceId::try_new(SCOPE_USER).expect("instance id");
    let scope_doc = mock.content(SCOPE_USER).expect("seeded");
    mock.seed_entity(Seed::new(SCOPE_USER, scope_doc).deleted());
    assert_eq!(registry.instance_type(&scope).await.expect("answers"), None);

    mock.inject(Fault::on(Call::List).fail(CanonicalError::internal("registry down").create()));
    let err = registry
        .derived_types(&type_id(REQUEST_BASE))
        .await
        .expect_err("listing fails");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_real_registry_resolves_the_qe_bases_it_admitted() {
    let managed = ManagedRegistry::start(Vec::new()).await;
    let hub = Arc::new(ClientHub::new());
    managed.install(&hub);
    let registry = adapter(hub);
    let scope = registry
        .type_schema(PLATFORM, &type_id(SCOPE_TYPE))
        .await
        .expect("answers")
        .expect("admitted");
    assert!(!scope.is_abstract);
}

/// The registry's own `NotFound` answer, for fault injection.
async fn not_found(mock: &Arc<MockTypesRegistry>) -> CanonicalError {
    use types_registry_sdk::{PlatformTypesRegistryApiExt, ReadOptions};
    let err = mock
        .get_type_schema(
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            &type_id("gts.cf.test.qe.absent.v1~"),
            ReadOptions::default(),
        )
        .await
        .expect_err("absent");
    assert!(matches!(err, CanonicalError::NotFound { .. }), "{err:?}");
    err
}

/// A failed listing is a registry failure, never an empty catalogue — a
/// `NotFound` answer on the first or a later page included.
#[tokio::test]
async fn a_failed_listing_is_never_an_empty_discovery() {
    for (first, nth) in [(true, 1), (false, 2)] {
        let mut documents = llm_gateway_documents();
        if !first {
            // Enough request contracts to need a second page.
            for n in 0..60 {
                let mut extra = mixin_request_document(METRIC_OTHER);
                extra["$id"] = json!(format!(
                    "gts://gts.cf.core.qe.request.v1~cf.test.qe.extra_{n:02}.v1~"
                ));
                documents.push(extra);
            }
            documents.push(mixin_type_document());
        }
        let mock = mock_registry(&documents, &[]);
        let not_found = not_found(&mock).await;
        mock.inject(Fault::on(Call::List).nth(nth).fail(not_found));
        let err = over(&mock)
            .derived_types(&type_id(REQUEST_BASE))
            .await
            .expect_err("a failed page is not an empty listing");
        assert!(
            matches!(err, DomainError::TypesRegistryUnavailable(_)),
            "page {nth}: {err:?}"
        );
    }
}

/// A selected artifact the registry did not return is drift, never a
/// default; a selected JSON null stays null.
#[test]
fn a_missing_selected_artifact_is_drift() {
    use types_registry_sdk::{LifecycleStatus, TypeSchema};

    let snapshot = || TypeSchema {
        type_id: type_id(LLM_USER_PROJECTION),
        type_uuid: uuid::Uuid::nil(),
        lifecycle_status: LifecycleStatus::Active,
        ownership: types_registry_sdk::Ownership::Global,
        origin: None,
        content: Some(json!({ "type": "object", "x-gts-abstract": true })),
        resolved_schema: Some(json!({ "type": "object" })),
        effective_traits: Some(json!({})),
        effective_traits_schema: None,
        provenance: None,
    };
    let complete = super::resolve(snapshot()).expect("complete");
    assert!(complete.is_abstract, "read from the authored content");
    for field in ["content", "resolved schema", "effective traits"] {
        let mut drifted = snapshot();
        match field {
            "content" => drifted.content = None,
            "resolved schema" => drifted.resolved_schema = None,
            _ => drifted.effective_traits = None,
        }
        let err = super::resolve(drifted).expect_err(field);
        assert!(
            matches!(&err, DomainError::TypesRegistryUnavailable(d) if d.contains(field)),
            "{field}: {err:?}"
        );
    }
    let mut null = snapshot();
    null.effective_traits = Some(serde_json::Value::Null);
    let null = super::resolve(null).expect("a selected null");
    assert_eq!(null.effective_traits, serde_json::Value::Null);
}

/// A registry that refuses a request's caller answered: the refusal is the
/// caller's, not an outage. The gear's own read of the same type keeps the
/// outage classification.
#[tokio::test]
async fn a_refused_request_is_the_callers_refusal_not_an_outage() {
    let mock = mock_registry(&llm_gateway_documents(), &[]);
    let registry = over(&mock);
    let caller = ctx();
    let id = type_id(LLM_USER_PROJECTION);
    let denied = crate::infra::canonical_mapping::QuotaResource::permission_denied()
        .with_reason("FORBIDDEN")
        .create();
    let unauthenticated = CanonicalError::unauthenticated()
        .with_reason("TOKEN_EXPIRED")
        .create();
    for (refusal, expected) in [
        (denied.clone(), CallerRefusal::PermissionDenied),
        (unauthenticated, CallerRefusal::Unauthenticated),
    ] {
        mock.inject(Fault::on(Call::BatchGet).first(1).fail(refusal));
        let err = registry
            .type_schema(RegistryCaller::Request(&caller), &id)
            .await
            .expect_err("refused");
        assert_eq!(err, DomainError::RegistryRefusedCaller(expected));
    }

    mock.inject(Fault::on(Call::BatchGet).first(1).fail(denied));
    let err = registry
        .type_schema(PLATFORM, &id)
        .await
        .expect_err("refused");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}
