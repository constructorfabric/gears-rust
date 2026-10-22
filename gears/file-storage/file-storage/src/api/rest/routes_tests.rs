//! `register_routes` is the only place the control-plane routes are declared, while the
//! integration tests build their own routers; this checks the real route table.

use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use sea_orm_migration::MigratorTrait;
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::{OperationSpec, ParamLocation};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

use super::{handlers, register_routes};
use crate::domain::authz::{Authorizer, TenantOnlyAuthorizer};
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::ports::{MultipartStore, PolicyStore};
use crate::domain::service::{FileService, ServiceConfig};
use crate::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use crate::infra::signed_url::Issuer;
use crate::infra::storage::Store;
use crate::infra::storage::migrations::Migrator;

const BASE: &str = "/api/file-storage/v1";

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-routes-{}.db", Uuid::now_v7().simple()));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    Arc::new(DBProvider::new(db))
}

async fn registry_with_routes() -> OpenApiRegistryImpl {
    let db = build_db().await;
    let store = Store::new(db);
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 25,
        max_page_size: 200,
        idempotency_ttl_secs: 86400,
    };

    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let service = Arc::new(FileService::new(
        store,
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let multipart_service = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let policy_service = Arc::new(PolicyService::new(policy_store, authorizer, 25, 200));
    let finalize_auth = Arc::new(handlers::FinalizeAuth::new("test-secret".to_owned()));

    let registry = OpenApiRegistryImpl::new();
    let _router = register_routes(
        Router::new(),
        &registry,
        service,
        multipart_service,
        policy_service,
        finalize_auth,
    );
    registry
}

fn lookup_spec(registry: &OpenApiRegistryImpl, method: &Method, path: &str) -> OperationSpec {
    let key = format!("{}:{BASE}{path}", method.as_str());
    registry
        .operation_specs
        .get(&key)
        .unwrap_or_else(|| panic!("expected route to be registered: {key}"))
        .value()
        .clone()
}

#[tokio::test]
async fn registers_every_control_plane_route() {
    let registry = registry_with_routes().await;

    let mut actual: Vec<String> = registry
        .operation_specs
        .iter()
        .map(|e| e.key().clone())
        .collect();
    actual.sort();

    let mut expected: Vec<String> = [
        ("POST", "/files/{file_id}/versions/{version_id}/finalize"),
        (
            "POST",
            "/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report",
        ),
        ("POST", "/files"),
        ("POST", "/files/{id}/versions"),
        ("POST", "/files/{id}/bind"),
        ("GET", "/files/{id}/download-url"),
        ("GET", "/files/{id}/versions"),
        ("DELETE", "/files/{id}/versions/{version_id}"),
        ("GET", "/files/{id}"),
        ("PATCH", "/files/{id}"),
        ("DELETE", "/files/{id}"),
        ("GET", "/files"),
        ("GET", "/storages"),
        ("GET", "/storages/{id}"),
        ("GET", "/policy"),
        ("PUT", "/policy"),
        ("GET", "/policy/effective"),
        ("GET", "/retention-rules"),
        ("POST", "/retention-rules"),
        ("DELETE", "/retention-rules/{rule_id}"),
        ("POST", "/files/{id}/multipart"),
        ("POST", "/files/{id}/multipart/{upload_id}/complete"),
        ("GET", "/files/{id}/multipart/{upload_id}"),
        ("DELETE", "/files/{id}/multipart/{upload_id}"),
        ("POST", "/files/{id}/migrate"),
        ("POST", "/files/{id}/transfer"),
    ]
    .iter()
    .map(|(method, path)| format!("{method}:{BASE}{path}"))
    .collect();
    expected.sort();

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn listings_declare_page_parameters() {
    let registry = registry_with_routes().await;

    let listings = [
        ("/files", "created_at desc, file_id desc"),
        ("/files/{id}/versions", "created_at desc, version_id desc"),
        ("/retention-rules", "created_at desc, rule_id desc"),
        ("/storages", "id asc"),
    ];
    for (path, order) in listings {
        let spec = lookup_spec(&registry, &Method::GET, path);
        for name in ["limit", "cursor"] {
            assert!(
                spec.params
                    .iter()
                    .any(|p| p.name == name && p.location == ParamLocation::Query),
                "GET {path} must declare the `{name}` query parameter",
            );
        }
        let description = spec.description.as_deref().unwrap_or_default();
        assert!(
            description.contains(order),
            "GET {path} description must state the listing order `{order}`",
        );
    }
}
