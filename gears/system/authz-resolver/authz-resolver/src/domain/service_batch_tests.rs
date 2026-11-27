//! Batch evaluation through the resolver: defaults, limits, semantics, the
//! shape check, the deadline and the process-wide in-flight bound.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    Action, DenyReason, EvaluationRequestContext, EvaluationResponseContext, Resource, Subject,
};
use authz_resolver_sdk::{
    AuthZResolverApi, AuthZResolverError, AuthZResolverPluginClient, AuthZResolverPluginSpecV1,
    BatchRequestError, EvaluationBatchItem, EvaluationBatchOptions, EvaluationBatchRequest,
    EvaluationRequest, EvaluationResponse, EvaluationsSemantic, evaluate_batch_concurrently,
};
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::Service;
use crate::config::BatchConfig;
use crate::domain::{AuthZResolverLocalClient, DomainError};

const SUBJECT: Uuid = Uuid::from_u128(0x2222);

/// Decides by action: `deny` is denied with an error code, `uncoded` is denied
/// without one, `fail` fails, `hang` never answers, anything else is allowed.
/// Counts evaluations and the peak number in flight, across all batches.
#[derive(Default)]
struct ByAction {
    evaluated: AtomicUsize,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    seen: std::sync::Mutex<Vec<EvaluationRequest>>,
}

impl ByAction {
    async fn decide(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.evaluated.fetch_add(1, Ordering::SeqCst);
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        // Let other items and batches start before this one answers.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        let action = request.action.name.clone();
        self.seen.lock().unwrap().push(request);
        let answer = match action.as_str() {
            "deny" => Ok(denial(Some(DenyReason::new(
                DenyReason::INSUFFICIENT_PERMISSIONS,
            )))),
            "uncoded" => Ok(denial(None)),
            "fail" => Err(AuthZResolverError::Internal("plugin failed".to_owned())),
            "hang" => std::future::pending().await,
            _ => Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext::default(),
            }),
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        answer
    }
}

fn denial(reason: Option<DenyReason>) -> EvaluationResponse {
    EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext {
            constraints: Vec::new(),
            deny_reason: reason,
        },
    }
}

#[async_trait]
impl AuthZResolverPluginClient for ByAction {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.decide(request).await
    }

    async fn evaluate_batch(
        &self,
        requests: Vec<EvaluationRequest>,
        semantic: EvaluationsSemantic,
        max_concurrency: NonZeroUsize,
        in_flight: &authz_resolver_sdk::Semaphore,
    ) -> Result<Vec<EvaluationResponse>, AuthZResolverError> {
        evaluate_batch_concurrently(requests, semantic, max_concurrency, in_flight, |r| {
            self.decide(r)
        })
        .await
    }
}

/// Answers every batch with one allow, whatever its size.
struct OneResult;

#[async_trait]
impl AuthZResolverPluginClient for OneResult {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        unreachable!("the resolver calls the plugin's batch")
    }

    async fn evaluate_batch(
        &self,
        _requests: Vec<EvaluationRequest>,
        _semantic: EvaluationsSemantic,
        _max_concurrency: NonZeroUsize,
        _in_flight: &authz_resolver_sdk::Semaphore,
    ) -> Result<Vec<EvaluationResponse>, AuthZResolverError> {
        Ok(vec![EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        }])
    }
}

/// A resolver whose only running plugin is `plugin`, with `batch` bounds.
fn service_with(plugin: Arc<dyn AuthZResolverPluginClient>, batch: &BatchConfig) -> Service {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let (id, content) = PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
        "cf.builtin.batch.plugin.v1",
        "cf",
        0,
    )
    .expect("a valid registration");
    registry.seed(id.as_ref(), content);
    hub.register_scoped::<dyn AuthZResolverPluginClient>(ClientScope::gts_id(id.as_ref()), plugin);
    Service::new(hub, "cf".to_owned())
        .with_batch_config(batch)
        .unwrap()
}

fn by_action(batch: &BatchConfig) -> (Service, Arc<ByAction>) {
    let plugin = Arc::new(ByAction::default());
    (service_with(Arc::clone(&plugin) as _, batch), plugin)
}

fn context() -> EvaluationRequestContext {
    EvaluationRequestContext {
        tenant_context: None,
        token_scopes: vec!["*".to_owned()],
        require_constraints: false,
        capabilities: Vec::new(),
        supported_properties: vec!["owner_tenant_id".to_owned()],
        bearer_token: None,
    }
}

fn resource(n: u128) -> Resource {
    Resource {
        resource_type: toolkit_gts::gts_id!("cf.core.users.user.v1~").to_owned(),
        id: Some(Uuid::from_u128(n)),
        properties: HashMap::new(),
    }
}

/// A batch with the subject and context as defaults and one item per action.
fn batch(actions: &[&str], semantic: EvaluationsSemantic) -> EvaluationBatchRequest {
    EvaluationBatchRequest {
        subject: Some(Subject {
            id: SUBJECT,
            subject_type: None,
            properties: HashMap::new(),
        }),
        action: None,
        resource: None,
        context: Some(context()),
        evaluations: actions
            .iter()
            .zip(1..)
            .map(|(name, n)| EvaluationBatchItem {
                action: Some(Action {
                    name: (*name).to_owned(),
                }),
                resource: Some(resource(n)),
                ..Default::default()
            })
            .collect(),
        options: EvaluationBatchOptions::default().with_semantic(semantic),
    }
}

fn decisions(response: &authz_resolver_sdk::EvaluationBatchResponse) -> Vec<bool> {
    response.evaluations.iter().map(|r| r.decision).collect()
}

#[tokio::test]
async fn execute_all_answers_every_item_in_order_with_defaults_applied() {
    let (service, plugin) = by_action(&BatchConfig::default());
    let response = service
        .evaluate_batch(batch(
            &["read", "deny", "read"],
            EvaluationsSemantic::ExecuteAll,
        ))
        .await
        .unwrap();

    assert_eq!(decisions(&response), [true, false, true]);
    let seen = plugin.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for request in seen.iter() {
        assert_eq!(
            request.subject.id, SUBJECT,
            "the default subject reaches every item"
        );
        assert_eq!(request.context.token_scopes, ["*"]);
    }
}

#[tokio::test]
async fn deny_on_first_deny_returns_the_evaluated_prefix() {
    let (service, plugin) = by_action(&BatchConfig {
        max_concurrency: 1,
        ..BatchConfig::default()
    });
    let response = service
        .evaluate_batch(batch(
            &["read", "deny", "read", "read"],
            EvaluationsSemantic::DenyOnFirstDeny,
        ))
        .await
        .unwrap();

    assert_eq!(decisions(&response), [true, false]);
    assert_eq!(plugin.evaluated.load(Ordering::SeqCst), 2);
}

/// An invalid batch is rejected before plugin lookup: the resolver has no
/// registry at all, so a lookup would fail differently.
#[tokio::test]
async fn an_invalid_batch_is_rejected_before_plugin_lookup() {
    let service = Service::new(Arc::new(ClientHub::new()), "cf".to_owned())
        .with_batch_config(&BatchConfig {
            max_items: 2,
            ..BatchConfig::default()
        })
        .unwrap();

    for (request, expected) in [
        (
            batch(&[], EvaluationsSemantic::ExecuteAll),
            BatchRequestError::Empty,
        ),
        (
            batch(&["a", "b", "c"], EvaluationsSemantic::ExecuteAll),
            BatchRequestError::TooManyItems { max: 2, got: 3 },
        ),
        (
            EvaluationBatchRequest {
                context: None,
                ..batch(&["a"], EvaluationsSemantic::ExecuteAll)
            },
            BatchRequestError::MissingKey {
                index: 0,
                key: "context",
            },
        ),
    ] {
        let err = service.evaluate_batch(request).await.unwrap_err();
        let DomainError::InvalidBatch(invalid) = &err else {
            panic!("expected InvalidBatch, got {err:?}");
        };
        assert_eq!(invalid, &expected);
    }
}

#[test]
fn an_out_of_range_batch_config_is_rejected_not_clamped() {
    let result =
        Service::new(Arc::new(ClientHub::new()), "cf".to_owned()).with_batch_config(&BatchConfig {
            max_concurrency: 0,
            ..BatchConfig::default()
        });
    assert!(result.is_err());
}

/// Decides from the subject, its tenant and the token scopes, and permits
/// with a constraint naming the subject's tenant: what a batch entry carries
/// depends on every key an item can override.
struct BySubject;

#[async_trait]
impl AuthZResolverPluginClient for BySubject {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|v| Uuid::parse_str(v).ok());
        let scoped = request.context.token_scopes.iter().any(|s| s == "*");
        Ok(match tenant {
            Some(tenant) if scoped => EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            "owner_tenant_id",
                            [tenant],
                        ))],
                    }],
                    deny_reason: None,
                },
            },
            _ => EvaluationResponse::deny(DenyReason::new(DenyReason::INSUFFICIENT_PERMISSIONS)),
        })
    }

    async fn evaluate_batch(
        &self,
        requests: Vec<EvaluationRequest>,
        semantic: EvaluationsSemantic,
        max_concurrency: NonZeroUsize,
        in_flight: &authz_resolver_sdk::Semaphore,
    ) -> Result<Vec<EvaluationResponse>, AuthZResolverError> {
        evaluate_batch_concurrently(requests, semantic, max_concurrency, in_flight, |r| {
            self.evaluate(r)
        })
        .await
    }
}

fn tenant_subject(id: u128, tenant: u128) -> Subject {
    Subject {
        id: Uuid::from_u128(id),
        subject_type: None,
        properties: HashMap::from([(
            "tenant_id".to_owned(),
            serde_json::json!(Uuid::from_u128(tenant)),
        )]),
    }
}

#[tokio::test]
async fn each_batch_entry_equals_an_independent_evaluation_of_its_resolved_item() {
    let service = service_with(Arc::new(BySubject), &BatchConfig::default());
    let read = || Action {
        name: "read".to_owned(),
    };
    let request = EvaluationBatchRequest {
        subject: Some(tenant_subject(1, 0xA)),
        action: Some(read()),
        resource: None,
        context: Some(context()),
        evaluations: vec![
            // Defaults only.
            EvaluationBatchItem {
                resource: Some(resource(1)),
                ..Default::default()
            },
            // Another subject and tenant.
            EvaluationBatchItem {
                subject: Some(tenant_subject(2, 0xB)),
                resource: Some(resource(2)),
                ..Default::default()
            },
            // Own context without the token scope: replaces the default whole.
            EvaluationBatchItem {
                resource: Some(resource(3)),
                context: Some(EvaluationRequestContext {
                    token_scopes: Vec::new(),
                    ..context()
                }),
                ..Default::default()
            },
            // A duplicate of the first item.
            EvaluationBatchItem {
                resource: Some(resource(1)),
                ..Default::default()
            },
        ],
        options: EvaluationBatchOptions::default(),
    };
    let (resolved, _) = request.clone().resolve(None).unwrap();

    let response = service.evaluate_batch(request).await.unwrap();

    assert_eq!(response.evaluations.len(), resolved.len());
    for (entry, item) in response.evaluations.iter().zip(resolved) {
        let single = service.evaluate(item).await.unwrap();
        assert_eq!(
            serde_json::to_value(entry).unwrap(),
            serde_json::to_value(&single).unwrap()
        );
    }
    let decisions: Vec<bool> = response.evaluations.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, true, false, true]);
    assert_ne!(
        serde_json::to_value(&response.evaluations[0]).unwrap(),
        serde_json::to_value(&response.evaluations[1]).unwrap(),
        "the overriding subject's tenant reaches its constraint"
    );
}

/// Fails every batch as a whole.
struct UnavailableBatch;

#[async_trait]
impl AuthZResolverPluginClient for UnavailableBatch {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        unreachable!("the resolver calls the plugin's batch")
    }

    async fn evaluate_batch(
        &self,
        _requests: Vec<EvaluationRequest>,
        _semantic: EvaluationsSemantic,
        _max_concurrency: NonZeroUsize,
        _in_flight: &authz_resolver_sdk::Semaphore,
    ) -> Result<Vec<EvaluationResponse>, AuthZResolverError> {
        Err(AuthZResolverError::ServiceUnavailable(
            "PDP down".to_owned(),
        ))
    }
}

#[tokio::test]
async fn a_whole_batch_plugin_failure_is_503_with_no_results() {
    let err = local_batch_error(
        service_with(Arc::new(UnavailableBatch), &BatchConfig::default()),
        batch(&["read", "read"], EvaluationsSemantic::ExecuteAll),
    )
    .await;
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_plugin_error_for_one_item_fails_the_whole_batch() {
    let (service, _) = by_action(&BatchConfig::default());
    let err = service
        .evaluate_batch(batch(
            &["read", "fail", "read"],
            EvaluationsSemantic::ExecuteAll,
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Internal(_)), "{err:?}");
}

#[tokio::test]
async fn results_of_the_wrong_shape_are_malformed() {
    let service = service_with(Arc::new(OneResult), &BatchConfig::default());
    let err = service
        .evaluate_batch(batch(&["read", "read"], EvaluationsSemantic::ExecuteAll))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MalformedBatchResults(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_denial_without_an_error_code_is_malformed() {
    let (service, _) = by_action(&BatchConfig::default());
    let err = service
        .evaluate_batch(batch(&["uncoded"], EvaluationsSemantic::ExecuteAll))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MalformedBatchResults(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_deadline_fails_the_whole_batch() {
    let (service, _) = by_action(&BatchConfig {
        deadline: Duration::from_millis(50),
        ..BatchConfig::default()
    });
    let err = service
        .evaluate_batch(batch(&["read", "hang"], EvaluationsSemantic::ExecuteAll))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::BatchDeadlineExceeded(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn without_a_running_plugin_the_batch_is_unavailable() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let service = Service::new(hub, "cf".to_owned());
    let err = service
        .evaluate_batch(batch(&["read"], EvaluationsSemantic::ExecuteAll))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::PluginNotFound { .. } | DomainError::PluginUnavailable { .. }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn max_concurrency_bounds_one_batch() {
    let (service, plugin) = by_action(&BatchConfig {
        max_concurrency: 3,
        ..BatchConfig::default()
    });
    service
        .evaluate_batch(batch(&["read"; 10], EvaluationsSemantic::ExecuteAll))
        .await
        .unwrap();
    assert_eq!(plugin.peak.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn max_in_flight_bounds_items_across_concurrent_batches() {
    let (service, plugin) = by_action(&BatchConfig {
        max_concurrency: 2,
        max_in_flight: 3,
        ..BatchConfig::default()
    });
    let service = Arc::new(service);
    // Spawn every batch before awaiting any, so they compete for permits.
    let mut batches = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let service = Arc::clone(&service);
        batches.spawn(async move {
            service
                .evaluate_batch(batch(&["read"; 4], EvaluationsSemantic::ExecuteAll))
                .await
        });
    }
    while let Some(result) = batches.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(plugin.evaluated.load(Ordering::SeqCst), 16);
    assert!(
        plugin.peak.load(Ordering::SeqCst) <= 3,
        "peak {}",
        plugin.peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn waiting_for_an_in_flight_permit_counts_against_the_deadline() {
    let (service, plugin) = by_action(&BatchConfig {
        max_concurrency: 1,
        max_in_flight: 1,
        deadline: Duration::from_millis(50),
        ..BatchConfig::default()
    });
    // Another batch's item holds the only permit for longer than the deadline.
    let _held = service.batch.in_flight.try_acquire().unwrap();

    let err = service
        .evaluate_batch(batch(&["read"], EvaluationsSemantic::ExecuteAll))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::BatchDeadlineExceeded(_)),
        "{err:?}"
    );
    assert_eq!(plugin.evaluated.load(Ordering::SeqCst), 0);
}

// ── Through the local client: the canonical errors a caller sees ─────────────

async fn local_batch_error(service: Service, request: EvaluationBatchRequest) -> CanonicalError {
    AuthZResolverLocalClient::new(Arc::new(service))
        .evaluate_batch(PlatformSecurityContext::outbound_marker(), request)
        .await
        .unwrap_err()
}

#[tokio::test]
async fn the_local_client_maps_batch_failures_onto_canonical_errors() {
    let invalid = local_batch_error(
        Service::new(Arc::new(ClientHub::new()), "cf".to_owned()),
        batch(&[], EvaluationsSemantic::ExecuteAll),
    )
    .await;
    assert!(
        matches!(invalid, CanonicalError::InvalidArgument { .. }),
        "an invalid batch passes through as 400: {invalid:?}"
    );

    let (slow, _) = by_action(&BatchConfig {
        deadline: Duration::from_millis(50),
        ..BatchConfig::default()
    });
    let deadline = local_batch_error(slow, batch(&["hang"], EvaluationsSemantic::ExecuteAll)).await;
    assert!(
        matches!(deadline, CanonicalError::ServiceUnavailable { .. }),
        "the deadline is 503: {deadline:?}"
    );

    let malformed = local_batch_error(
        service_with(Arc::new(OneResult), &BatchConfig::default()),
        batch(&["read", "read"], EvaluationsSemantic::ExecuteAll),
    )
    .await;
    assert!(
        matches!(malformed, CanonicalError::Internal { .. }),
        "malformed plugin results are 500: {malformed:?}"
    );
}

#[tokio::test]
async fn the_local_client_returns_decisions() {
    let (service, _) = by_action(&BatchConfig::default());
    let response = AuthZResolverLocalClient::new(Arc::new(service))
        .evaluate_batch(
            PlatformSecurityContext::outbound_marker(),
            batch(&["read", "deny"], EvaluationsSemantic::ExecuteAll),
        )
        .await
        .unwrap();
    let results = response
        .into_results(2, EvaluationsSemantic::ExecuteAll)
        .unwrap();
    assert!(results[0].decision);
    assert_eq!(
        results[1].context.deny_reason.as_ref().unwrap().error_code,
        DenyReason::INSUFFICIENT_PERMISSIONS
    );
}
