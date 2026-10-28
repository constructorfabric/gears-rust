//! Plugin selection over the platform catalogue.
//!
//! Each fake plugin fails every evaluation with its own name, so the error says which
//! plugin the service selected.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::models::{Action, EvaluationRequestContext, Resource, Subject};
use authz_resolver_sdk::{
    AuthZResolverError, AuthZResolverPluginClient, AuthZResolverPluginSpecV1, EvaluationRequest,
    EvaluationResponse,
};
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use super::{DISCOVERY_BUDGET, Service};
use crate::domain::DomainError;

/// An error scope for the failures the fake registry answers with.
#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.core.types_registry.entity.v1~"))]
struct Catalogue;

/// Every availability category the selection treats as a retryable registry outage.
fn outages() -> [CanonicalError; 4] {
    [
        CanonicalError::service_unavailable().create(),
        Catalogue::deadline_exceeded("listing timed out").create(),
        Catalogue::resource_exhausted("rate limited")
            .with_quota_violation("listing", "rate limit")
            .create(),
        Catalogue::aborted("listing aborted")
            .with_reason("LISTING_ABORTED")
            .create(),
    ]
}

struct Named(&'static str);

#[async_trait]
impl AuthZResolverPluginClient for Named {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Err(AuthZResolverError::Internal(self.0.to_owned()))
    }
}

/// Publishes `segment` under `vendor`/`priority` in `registry`, and returns its id.
fn publish(registry: &MockTypesRegistry, segment: &str, vendor: &str, priority: i16) -> String {
    let (id, content) =
        PluginV1::<AuthZResolverPluginSpecV1>::build_registration(segment, vendor, priority)
            .expect("a valid registration");
    registry.seed(id.as_ref(), content);
    id.as_ref().to_owned()
}

/// Binds a running plugin named `name` under the scope of `id`.
fn run(hub: &ClientHub, id: &str, name: &'static str) {
    hub.register_scoped::<dyn AuthZResolverPluginClient>(
        ClientScope::gts_id(id),
        Arc::new(Named(name)),
    );
}

fn hub_with(registry: &Arc<MockTypesRegistry>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    hub
}

fn request() -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: uuid::Uuid::nil(),
            subject_type: None,
            properties: HashMap::new(),
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: toolkit_gts::gts_id!("cf.core.users.user.v1~").to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: Vec::new(),
            require_constraints: false,
            capabilities: Vec::new(),
            supported_properties: Vec::new(),
            bearer_token: None,
        },
    }
}

/// The plugin `service` selected, by the name it failed the evaluation with.
async fn selected(service: &Service) -> Result<String, DomainError> {
    match service.evaluate(request()).await {
        Err(DomainError::Internal(name)) => Ok(name),
        Err(other) => Err(other),
        Ok(_) => panic!("a fake plugin never evaluates"),
    }
}

#[tokio::test]
async fn the_lowest_priority_running_plugin_of_the_vendor_is_selected() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let low = publish(&registry, "cf.builtin.low.plugin.v1", "cf", 10);
    let high = publish(&registry, "cf.builtin.high.plugin.v1", "cf", 50);
    let other = publish(&registry, "cf.builtin.other.plugin.v1", "acme", 0);
    run(&hub, &low, "low");
    run(&hub, &high, "high");
    run(&hub, &other, "other");

    let service = Service::new(hub, "cf".to_owned());
    assert_eq!(selected(&service).await.unwrap(), "low");
}

/// The catalogue is durable: an instance a previous boot published stays listed after
/// its plugin is no longer linked. A better-ranked stale instance must not win.
#[tokio::test]
async fn an_instance_without_a_running_plugin_is_never_selected() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    publish(&registry, "cf.builtin.stale.plugin.v1", "cf", 0);
    let tr = publish(&registry, "cf.builtin.tr.plugin.v1", "cf", 100);
    run(&hub, &tr, "tr");

    let service = Service::new(hub, "cf".to_owned());
    assert_eq!(selected(&service).await.unwrap(), "tr");
}

#[tokio::test]
async fn no_running_plugin_is_not_found_and_is_retried_once_one_runs() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let id = publish(&registry, "cf.builtin.late.plugin.v1", "cf", 0);

    let service = Service::new(Arc::clone(&hub), "cf".to_owned());
    assert!(matches!(
        selected(&service).await,
        Err(DomainError::PluginNotFound { .. })
    ));

    run(&hub, &id, "late");
    assert_eq!(selected(&service).await.unwrap(), "late");
}

/// Every page is read: a selection over the first page alone would miss the winner.
#[tokio::test]
async fn a_failed_page_fails_the_selection_and_is_not_cached() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    for n in 0..60 {
        let id = publish(
            &registry,
            &format!("cf.builtin.a{n:02}.plugin.v1"),
            "cf",
            100,
        );
        run(&hub, &id, "loser");
    }
    let winner = publish(&registry, "cf.builtin.zz.plugin.v1", "cf", 1);
    run(&hub, &winner, "winner");
    registry.inject(
        Fault::on(Call::List)
            .nth(2)
            .fail(CanonicalError::service_unavailable().create()),
    );

    let service = Service::new(hub, "cf".to_owned());
    assert!(matches!(
        selected(&service).await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));
    assert_eq!(selected(&service).await.unwrap(), "winner");
    let lists = registry.calls(Call::List);
    assert_eq!(selected(&service).await.unwrap(), "winner");
    assert_eq!(registry.calls(Call::List), lists, "a selection is cached");
}

#[tokio::test]
async fn without_the_platform_api_the_selection_fails_closed() {
    let hub = Arc::new(ClientHub::new());
    let id = PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
        "cf.builtin.orphan.plugin.v1",
        "cf",
        0,
    )
    .unwrap()
    .0;
    run(&hub, id.as_ref(), "orphan");

    let service = Service::new(hub, "cf".to_owned());
    assert!(matches!(
        selected(&service).await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));
}

/// Discovery timeout bounds queued calls, caches no failure, and permits a later retry.
#[tokio::test(start_paused = true)]
async fn a_stalled_registry_fails_the_selection_at_the_discovery_budget() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let id = publish(&registry, "cf.builtin.late.plugin.v1", "cf", 0);
    run(&hub, &id, "late");
    registry.inject(
        Fault::on(Call::List)
            .first(2)
            .delay(Duration::from_secs(3600)),
    );
    let service = Arc::new(Service::new(hub, "cf".to_owned()));

    let first = tokio::spawn({
        let service = Arc::clone(&service);
        async move { selected(&service).await }
    });
    // The first call now holds the selector, stalled in the listing.
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    let queued_at = tokio::time::Instant::now();
    let queued = selected(&service).await;
    assert_eq!(queued_at.elapsed(), DISCOVERY_BUDGET, "the wait counts");
    for result in [first.await.unwrap(), queued] {
        assert!(
            matches!(result, Err(DomainError::TypesRegistryUnavailable(_))),
            "{result:?}"
        );
    }
    assert_eq!(registry.calls(Call::List), 2);
    assert_eq!(selected(&service).await.unwrap(), "late");
}

/// Only an availability failure of the registry is an outage; anything else it answers
/// stays internal.
#[tokio::test]
async fn registry_failures_are_outages_only_when_retryable() {
    let refused = Catalogue::permission_denied()
        .with_reason("PLATFORM_CREDENTIAL_REFUSED")
        .create();
    for (error, outage) in outages().into_iter().map(|error| (error, true)).chain([
        (refused, false),
        (CanonicalError::internal("corrupt").create(), false),
    ]) {
        let registry = Arc::new(MockTypesRegistry::new());
        let hub = hub_with(&registry);
        let id = publish(&registry, "cf.builtin.any.plugin.v1", "cf", 0);
        run(&hub, &id, "any");
        registry.inject(Fault::on(Call::List).fail(error.clone()));

        let result = selected(&Service::new(hub, "cf".to_owned())).await;
        if outage {
            assert!(
                matches!(result, Err(DomainError::TypesRegistryUnavailable(_))),
                "{error:?} -> {result:?}"
            );
        } else {
            // Internal, and not the plugin's own failure: no plugin was selected.
            assert!(
                matches!(&result, Ok(name) if name != "any"),
                "{error:?} -> {result:?}"
            );
        }
    }
}

// ── Policy semantics through the PEP ─────────────────────────────────────────
//
// The selection tests above identify the selected plugin by a failure. These drive a
// `PolicyEnforcer` over the resolver's own local client, so what a plugin decides — and
// what an infrastructure failure becomes — is observed where a gear sees it.

mod pep {
    use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
    use authz_resolver_sdk::models::{DenyReason, EvaluationResponseContext};
    use authz_resolver_sdk::pep::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};
    use parking_lot::Mutex;
    use toolkit_security::{SecurityContext, pep_properties};
    use uuid::Uuid;

    use super::*;
    use crate::domain::AuthZResolverLocalClient;

    const TENANT: Uuid = Uuid::from_u128(0x1111);
    const SUBJECT: Uuid = Uuid::from_u128(0x2222);
    const ROW: Uuid = Uuid::from_u128(0x3333);
    const CONTEXT_TENANT: Uuid = Uuid::from_u128(0x4444);
    const RESOURCE: ResourceType = ResourceType::from_static(
        toolkit_gts::gts_id!("cf.core.users.user.v1~"),
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// Answers with `response` and keeps the request it was asked.
    struct Recording {
        response: EvaluationResponse,
        seen: Mutex<Option<EvaluationRequest>>,
    }

    #[async_trait]
    impl AuthZResolverPluginClient for Recording {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            *self.seen.lock() = Some(request);
            Ok(self.response.clone())
        }
    }

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(SUBJECT)
            .subject_tenant_id(TENANT)
            .build()
            .unwrap()
    }

    /// A PEP over the resolver, whose only running plugin answers `response`.
    fn enforcer_with(response: EvaluationResponse) -> (PolicyEnforcer, Arc<Recording>) {
        let registry = Arc::new(MockTypesRegistry::new());
        let hub = hub_with(&registry);
        let id = publish(&registry, "cf.builtin.recording.plugin.v1", "cf", 0);
        let plugin = Arc::new(Recording {
            response,
            seen: Mutex::new(None),
        });
        hub.register_scoped::<dyn AuthZResolverPluginClient>(
            ClientScope::gts_id(&id),
            Arc::clone(&plugin) as _,
        );
        let service = Arc::new(Service::new(hub, "cf".to_owned()));
        let enforcer = PolicyEnforcer::new(Arc::new(AuthZResolverLocalClient::new(service)));
        (enforcer, plugin)
    }

    #[tokio::test]
    async fn a_plugin_denial_reaches_the_pep_with_its_reason() {
        let (enforcer, plugin) = enforcer_with(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: Some(DenyReason {
                    error_code: "tenant_barrier".to_owned(),
                    details: Some("crossed".to_owned()),
                }),
            },
        });

        let denied = enforcer
            .access_scope_with(
                &ctx(),
                &RESOURCE,
                "get",
                Some(ROW),
                &AccessRequest::new().context_tenant_id(CONTEXT_TENANT),
            )
            .await;

        match denied {
            Err(EnforcerError::Denied {
                deny_reason: Some(reason),
            }) => {
                assert_eq!(reason.error_code, "tenant_barrier");
                assert_eq!(reason.details.as_deref(), Some("crossed"));
            }
            other => panic!("expected the plugin's denial, got {other:?}"),
        }

        // The plugin was asked the PEP's own request, unchanged by the resolver.
        let seen = plugin.seen.lock().clone().expect("plugin evaluated");
        assert_eq!(seen.subject.id, SUBJECT);
        assert_eq!(seen.action.name, "get");
        assert_eq!(seen.resource.id, Some(ROW));
        assert!(seen.context.require_constraints);
        assert_eq!(
            seen.context.tenant_context.and_then(|tc| tc.root_id),
            Some(CONTEXT_TENANT)
        );
    }

    #[tokio::test]
    async fn a_plugin_grant_narrows_the_scope_to_its_constraints() {
        let (enforcer, _plugin) = enforcer_with(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [TENANT],
                    ))],
                }],
                deny_reason: None,
            },
        });

        let scope = enforcer
            .access_scope(&ctx(), &RESOURCE, "list", None)
            .await
            .expect("the plugin granted access");

        assert_eq!(
            scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
            &[TENANT]
        );
    }

    /// What the PEP gets when listing the catalogue fails with `error`, while an allowing
    /// plugin is bound.
    async fn evaluated_over_failing_registry(
        error: CanonicalError,
    ) -> Result<toolkit_security::AccessScope, EnforcerError> {
        let registry = Arc::new(MockTypesRegistry::new());
        let hub = hub_with(&registry);
        let id = publish(&registry, "cf.builtin.allow.plugin.v1", "cf", 0);
        hub.register_scoped::<dyn AuthZResolverPluginClient>(
            ClientScope::gts_id(&id),
            Arc::new(Recording {
                response: EvaluationResponse {
                    decision: true,
                    context: EvaluationResponseContext::default(),
                },
                seen: Mutex::new(None),
            }),
        );
        registry.inject(Fault::on(Call::List).fail(error));
        let service = Arc::new(Service::new(hub, "cf".to_owned()));
        let enforcer = PolicyEnforcer::new(Arc::new(AuthZResolverLocalClient::new(service)));
        enforcer.access_scope(&ctx(), &RESOURCE, "list", None).await
    }

    /// A registry outage, of every retryable category, is a retryable evaluation failure at
    /// the PEP — never a scope, and never confused with what a plugin returned.
    #[tokio::test]
    async fn a_registry_outage_is_never_access() {
        for error in outages() {
            let result = evaluated_over_failing_registry(error.clone()).await;
            assert!(
                matches!(
                    result,
                    Err(EnforcerError::EvaluationFailed(
                        CanonicalError::ServiceUnavailable { .. }
                    ))
                ),
                "{error:?} -> {result:?}"
            );
        }
    }

    /// A registry fault that is not an outage stays internal: retrying would not help.
    #[tokio::test]
    async fn an_internal_registry_failure_is_internal_and_never_access() {
        let result =
            evaluated_over_failing_registry(CanonicalError::internal("corrupt page").create())
                .await;
        assert!(
            matches!(
                result,
                Err(EnforcerError::EvaluationFailed(
                    CanonicalError::Internal { .. }
                ))
            ),
            "{result:?}"
        );
    }
}
