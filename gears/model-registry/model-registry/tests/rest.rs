//! REST surface pins (DESIGN §3.3): the twelve operations with their paths,
//! methods, operation ids, auth, and `OData` documentation, plus the Problem
//! JSON location of every reason code and field reason.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cognitive_complexity)]

use std::sync::{Arc, Mutex};

use authz_resolver_sdk::PolicyEnforcer;
use axum::Router;
use model_registry::api::rest::routes::register_routes;
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::access_guard::AccessGuard;
use model_registry::domain::clock::ManualClock;
use model_registry::domain::error::{Dependency, DomainError, Resource, Violation};
use model_registry::domain::service::Service;
use model_registry::infra::chain_cache::ChainCache;
use model_registry::testing::{
    StaticAuthz, StaticSchemaSource, StaticSecretRefSource, StaticTenantResolver,
};
use model_registry_sdk::gts::{MODEL_RESOURCE_TYPE, PROVIDER_RESOURCE_TYPE};
use model_registry_sdk::{
    ApprovalState, field,
    reason::{aborted, permission},
};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationSpec, RouteAuth};
use toolkit_canonical_errors::{CanonicalError, Problem};
use uuid::Uuid;

#[derive(Default)]
struct RecordingRegistry {
    ops: Mutex<Vec<OperationSpec>>,
}

impl OpenApiRegistry for RecordingRegistry {
    fn register_operation(&self, spec: &OperationSpec) {
        self.ops.lock().unwrap().push(spec.clone());
    }

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn service() -> Arc<Service> {
    let raw = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .expect("in-memory SQLite");
    let db = Arc::new(toolkit_db::DBProvider::<DomainError>::new(raw));
    let cfg = ModelRegistryConfig::default();
    let clock = Arc::new(ManualClock::fixed());
    let chain_cache = Arc::new(ChainCache::new(
        Arc::new(StaticTenantResolver::new()),
        clock.clone(),
        cfg.chain_cache_ttl(),
        cfg.chain_cache_max_entries,
        cfg.tenant_resolver_deadline(),
    ));
    let guard = AccessGuard::new(
        PolicyEnforcer::new(Arc::new(StaticAuthz::deny_all())),
        cfg.max_pdp_constraint_values,
    );
    Arc::new(Service::new(
        db,
        chain_cache,
        guard,
        Arc::new(StaticSchemaSource::new()),
        Arc::new(StaticSecretRefSource::new()),
        clock,
        &cfg,
    ))
}

#[tokio::test]
async fn the_twelve_operations_are_registered_as_designed() {
    let registry = RecordingRegistry::default();
    let _router: Router = register_routes(Router::new(), &registry, service().await);
    let ops = registry.ops.lock().unwrap().clone();

    let mut actual: Vec<(String, String, String)> = ops
        .iter()
        .map(|s| {
            (
                s.method.to_string(),
                s.path.clone(),
                s.operation_id.clone().unwrap_or_default(),
            )
        })
        .collect();
    actual.sort();
    let mut expected: Vec<(String, String, String)> = [
        (
            "GET",
            "/model-registry/v1/available-models/{canonical_id}",
            "resolve_model",
        ),
        (
            "GET",
            "/model-registry/v1/available-models",
            "list_available_models",
        ),
        ("POST", "/model-registry/v1/providers", "create_provider"),
        ("GET", "/model-registry/v1/providers/{id}", "get_provider"),
        ("GET", "/model-registry/v1/providers", "list_providers"),
        (
            "PATCH",
            "/model-registry/v1/providers/{id}",
            "update_provider",
        ),
        (
            "DELETE",
            "/model-registry/v1/providers/{id}",
            "delete_provider",
        ),
        ("POST", "/model-registry/v1/models", "create_model"),
        ("GET", "/model-registry/v1/models/{id}", "get_model"),
        ("GET", "/model-registry/v1/models", "list_models"),
        ("PATCH", "/model-registry/v1/models/{id}", "update_model"),
        ("DELETE", "/model-registry/v1/models/{id}", "delete_model"),
    ]
    .into_iter()
    .map(|(m, p, op)| (m.to_owned(), p.to_owned(), format!("model_registry.{op}")))
    .collect();
    expected.sort();
    assert_eq!(actual, expected);

    for spec in &ops {
        assert!(
            matches!(spec.auth, RouteAuth::Authenticated),
            "{} must be authenticated",
            spec.path
        );
        let statuses: Vec<u16> = spec.responses.iter().map(|r| r.status).collect();
        for status in [400, 401, 403, 404, 409, 500, 503] {
            assert!(statuses.contains(&status), "{} lacks {status}", spec.path);
        }
        let is_list = spec.path.ends_with("models") || spec.path.ends_with("providers");
        let has_odata = spec.params.iter().any(|p| p.name == "$filter");
        assert_eq!(
            has_odata,
            is_list && spec.method == axum::http::Method::GET,
            "{} odata params",
            spec.path
        );
        if has_odata {
            for name in ["$orderby", "$select"] {
                assert!(
                    spec.params.iter().any(|p| p.name == name),
                    "{} lacks {name}",
                    spec.path
                );
            }
        }
    }
    // `If-Match` is required on PATCH and optional on DELETE (DESIGN §3.3).
    for spec in &ops {
        let if_match = spec.params.iter().find(|p| p.name == "If-Match");
        match spec.method {
            axum::http::Method::PATCH => {
                assert!(
                    if_match.is_some_and(|p| p.required),
                    "{} requires If-Match",
                    spec.path
                );
            }
            axum::http::Method::DELETE => {
                assert!(
                    if_match.is_some_and(|p| !p.required),
                    "{} takes an optional If-Match",
                    spec.path
                );
            }
            _ => assert!(if_match.is_none(), "{} has no If-Match", spec.path),
        }
    }
    let created: Vec<&OperationSpec> = ops
        .iter()
        .filter(|s| s.method == axum::http::Method::POST)
        .collect();
    assert_eq!(created.len(), 2);
    for spec in created {
        assert!(
            spec.responses.iter().any(|r| r.status == 201),
            "{}",
            spec.path
        );
    }
}

fn problem(err: DomainError) -> serde_json::Value {
    serde_json::to_value(Problem::from(CanonicalError::from(err))).unwrap()
}

#[test]
fn reason_codes_land_in_their_problem_json_locations() {
    let json = problem(DomainError::model_not_found("openai::gpt-4o"));
    assert_eq!(json["status"], 404);
    assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);
    assert_eq!(json["context"]["resource_name"], "openai::gpt-4o");

    let id = Uuid::new_v4();
    let json = problem(DomainError::ProviderNotFound { id });
    assert_eq!(json["status"], 404);
    assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);

    for (err, reason) in [
        (
            DomainError::ProviderDisabled {
                canonical_id: "a::b".into(),
            },
            permission::PROVIDER_DISABLED,
        ),
        (
            DomainError::ModelNotApproved {
                canonical_id: "a::b".into(),
                state: ApprovalState::Pending,
            },
            permission::MODEL_NOT_APPROVED,
        ),
        (
            DomainError::not_permitted("resolve_model"),
            permission::NOT_PERMITTED,
        ),
        (
            DomainError::InheritedReadOnly {
                resource: Resource::Model,
                operation: "update_model",
                id,
            },
            permission::INHERITED_READ_ONLY,
        ),
    ] {
        let json = problem(err);
        assert_eq!(json["status"], 403);
        assert_eq!(json["context"]["reason"], reason);
    }

    let json = problem(DomainError::ProviderAlreadyExists {
        slug: "openai".into(),
    });
    assert_eq!(json["status"], 409);
    assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);
    assert_eq!(json["context"]["resource_name"], "openai");

    let json = problem(DomainError::ModelAlreadyExists {
        canonical_id: "openai::gpt-4o".into(),
    });
    assert_eq!(json["status"], 409);
    assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);

    for reason in field::Reason::ALL {
        let json = problem(DomainError::invalid(Violation::new(
            "provider_settings/cost",
            "bad",
            *reason,
        )));
        assert_eq!(json["status"], 400);
        assert_eq!(
            json["context"]["field_violations"][0]["field"],
            "provider_settings/cost"
        );
        assert_eq!(
            json["context"]["field_violations"][0]["reason"],
            reason.as_wire()
        );
    }

    for dependency in [
        Dependency::TenantResolver,
        Dependency::Pdp,
        Dependency::TypesRegistry,
        Dependency::CredStore,
        Dependency::Database,
    ] {
        let json = problem(DomainError::unavailable(dependency, "secret diagnostic"));
        assert_eq!(json["status"], 503);
        assert!(!json.to_string().contains("secret diagnostic"));
    }

    let json = problem(DomainError::internal("SELECT 1"));
    assert_eq!(json["status"], 500);
    assert!(!json.to_string().contains("SELECT"));

    // Stale preconditions and raced type changes are `aborted` (409).
    let json = problem(DomainError::OptimisticLockFailure {
        resource: Resource::Model,
        id,
        detail: "stale expected_version",
    });
    assert_eq!(json["status"], 409);
    assert_eq!(json["context"]["reason"], aborted::OPTIMISTIC_LOCK_FAILURE);
    assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);
    let json = problem(DomainError::ProviderTypeChanged { provider_id: id });
    assert_eq!(json["status"], 409);
    assert_eq!(json["context"]["reason"], aborted::PROVIDER_TYPE_CHANGED);
}
