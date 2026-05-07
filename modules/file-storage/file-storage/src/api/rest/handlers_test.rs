//! REST handler tests against the unified P1 surface.

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::{delete, get, post, put};
    use axum::{Extension, Router};
    use authz_resolver_sdk::{
        AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
        constraints::{Constraint, InPredicate, Predicate},
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use file_storage_sdk::{
        Backend, BackendCapability, BackendId, BackendKind, BackendTransport, FileMeta,
        PresignedDownload, UrlParams,
    };
    use http_body_util::BodyExt;
    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::{ConnectOpts, DBProvider, connect_db};
    use modkit_security::{SecurityContext, pep_properties};
    use sea_orm_migration::MigratorTrait;
    use serde_json::{Value, json};
    use time::OffsetDateTime;
    use tokio::sync::Mutex as TokioMutex;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::api::rest::handlers;
    use crate::api::rest::routes::ConcreteService;
    use crate::config::FileStorageConfig;
    use crate::domain::error::DomainError;
    use crate::domain::etag::compose;
    use crate::domain::service::Service;
    use crate::infra::backends::registry::BackendRegistry;
    use crate::infra::backends::r#trait::{
        BackendDescriptor, BackendObjectKey, BackendReadResult, HeadResult, PresignedGetItem,
        PresignedGetOutcome, SharedBackend, StorageBackend,
    };
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::sea_orm_repo::SeaOrmFilesRepository;

    // ── Allow-all authz ─────────────────────────────────────────────────────
    struct AllowAuthZ;

    #[async_trait]
    impl AuthZResolverClient for AllowAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            let tenant = request
                .context
                .tenant_context
                .as_ref()
                .and_then(|tc| tc.root_id)
                .or_else(|| {
                    request
                        .subject
                        .properties
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                })
                .ok_or_else(|| AuthZResolverError::Internal("tenant required".to_owned()))?;
            let mut preds = vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [tenant],
            ))];
            if let Some(rid) = request.resource.id {
                preds.push(Predicate::In(InPredicate::new(
                    pep_properties::RESOURCE_ID,
                    [rid],
                )));
            }
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint { predicates: preds }],
                    ..Default::default()
                },
            })
        }
    }

    // ── Fake backend ────────────────────────────────────────────────────────
    struct FakeBackend {
        descriptor: BackendDescriptor,
        head_content_hash: StdMutex<String>,
    }

    impl FakeBackend {
        fn new(id: BackendId) -> Self {
            Self {
                descriptor: BackendDescriptor {
                    sdk: Backend {
                        id,
                        kind: BackendKind::S3Compatible,
                        default_public: false,
                        default_private: true,
                        transport: BackendTransport::Redirect,
                        capabilities: vec![BackendCapability::PresignedUrls],
                        max_file_size_bytes: Some(10 * 1024 * 1024),
                    },
                    max_signed_url_ttl_seconds_value: 3_600,
                    tenant_access: vec![],
                },
                head_content_hash: StdMutex::new("backend-hash".to_owned()),
            }
        }
    }

    #[async_trait]
    impl StorageBackend for FakeBackend {
        fn descriptor(&self) -> &BackendDescriptor {
            &self.descriptor
        }
        async fn open_read(
            &self,
            _key: &BackendObjectKey,
        ) -> Result<BackendReadResult, DomainError> {
            use bytes::Bytes;
            use futures::stream;
            let h = self.head_content_hash.lock().unwrap().clone();
            let data: Vec<Result<Bytes, file_storage_sdk::FileStorageError>> =
                vec![Ok(Bytes::from_static(b"hello"))];
            Ok(BackendReadResult {
                bytes: Box::pin(stream::iter(data)),
                content_hash: h,
            })
        }
        async fn delete_object(&self, _key: &BackendObjectKey) -> Result<(), DomainError> {
            Ok(())
        }
        async fn issue_presigned_put(
            &self,
            _key: &BackendObjectKey,
            _meta: &FileMeta,
            _params: &UrlParams,
            _expected_etag: &str,
            _ttl: u64,
        ) -> Result<String, DomainError> {
            Ok("https://example.test/u".to_owned())
        }
        async fn issue_presigned_gets(
            &self,
            items: Vec<PresignedGetItem>,
        ) -> Result<Vec<PresignedGetOutcome>, DomainError> {
            Ok(items
                .into_iter()
                .map(|i| PresignedGetOutcome {
                    key: i.key,
                    result: Ok(PresignedDownload {
                        url: "https://example.test/d".to_owned(),
                        expires_at: OffsetDateTime::UNIX_EPOCH,
                        is_public: false,
                    }),
                })
                .collect())
        }
        async fn head_object(&self, _key: &BackendObjectKey) -> Result<HeadResult, DomainError> {
            Ok(HeadResult {
                content_hash: self.head_content_hash.lock().unwrap().clone(),
                size_bytes: 5,
            })
        }
    }

    struct Harness {
        service: Arc<ConcreteService>,
        backend_id: BackendId,
    }

    async fn build_service() -> Harness {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts).await.unwrap();
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .unwrap();
        let provider = Arc::new(DBProvider::<modkit_db::DbError>::new(db));
        let repo = Arc::new(SeaOrmFilesRepository::new());

        let backend_id = Uuid::new_v4();
        let backend: SharedBackend = Arc::new(FakeBackend::new(backend_id));
        let mut map: HashMap<BackendId, SharedBackend> = HashMap::new();
        map.insert(backend_id, backend);
        let registry = Arc::new(BackendRegistry::new(map));

        let cfg = Arc::new(FileStorageConfig {
            default_private_storage_id: Some(backend_id),
            default_public_storage_id: None,
            orphan_delete_grace_seconds: 86_400,
            signed_url_clock_skew_margin_seconds: 60,
            backends: vec![],
        });

        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowAuthZ);
        let enforcer = PolicyEnforcer::new(authz);
        let oq: crate::domain::service::OrphanQueue =
            Arc::new(TokioMutex::new(VecDeque::new()));

        Harness {
            service: Arc::new(Service::new(provider, repo, enforcer, cfg, registry, oq)),
            backend_id,
        }
    }

    fn make_router(svc: Arc<ConcreteService>, ctx: SecurityContext) -> Router {
        Router::new()
            .route("/storages", get(handlers::list_backends))
            .route("/files/{file_id}", get(handlers::get_file))
            .route("/files/{file_id}", put(handlers::update_file))
            .route("/files/{file_id}", delete(handlers::delete_file))
            .route("/files", get(handlers::list_files))
            .route("/presign-batch", post(handlers::presign_batch))
            .layer(Extension(svc))
            .layer(Extension(ctx))
    }

    fn ctx_for(tenant: Uuid, user: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(user)
            .subject_tenant_id(tenant)
            .build()
            .unwrap()
    }

    async fn body_to_json(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        if bytes.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    fn presign_upload_batch(tenant: Uuid, user: Uuid, backend_id: BackendId) -> Value {
        json!({
            "items": [{
                "kind": "upload",
                "backend_id": backend_id,
                "owner": {
                    "tenant_id": tenant,
                    "owner_id": user
                },
                "file_path": "p/x.txt",
                "meta": {
                    "name": "x.txt",
                    "mime_type": "text/plain",
                    "gts_file_type": "gts.cf.fstorage.file.type.v1~doc~",
                    "size_bytes": 5,
                    "custom_metadata": {}
                },
                "params": {
                    "expires_in_seconds": 600,
                    "content_disposition": null,
                    "content_type_override": null,
                    "allowed_client_cidrs": [],
                    "refresh_etag": false
                }
            }]
        })
    }

    async fn presign_and_get_id(
        router: &Router,
        tenant: Uuid,
        user: Uuid,
        backend_id: BackendId,
    ) -> Uuid {
        let body = serde_json::to_vec(&presign_upload_batch(tenant, user, backend_id)).unwrap();
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/presign-batch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        let item = &v["items"][0];
        assert_eq!(item["kind"], "upload");
        let id = item["ok_upload"]["file_id"].as_str().expect("file_id");
        Uuid::parse_str(id).unwrap()
    }

    // ── GET /storages ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_backends_ok() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let resp = router
            .oneshot(Request::builder().uri("/storages").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert!(v["items"].is_array());
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
    }

    // ── POST /presign-batch (upload) ────────────────────────────────────────

    #[tokio::test]
    async fn presign_batch_upload_ok() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service.clone(), ctx);

        let body = serde_json::to_vec(&presign_upload_batch(tenant, user, h.backend_id)).unwrap();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/presign-batch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert_eq!(v["items"][0]["kind"], "upload");
        assert!(v["items"][0]["ok_upload"]["file_id"].is_string());
        assert_eq!(v["items"][0]["ok_upload"]["upload_url"], "https://example.test/u");
        assert_eq!(v["items"][0]["ok_upload"]["etag_pinned"], compose("", 0));
    }

    #[tokio::test]
    async fn presign_batch_upload_owner_tenant_mismatch_returns_per_item_error() {
        let h = build_service().await;
        let tenant_caller = Uuid::new_v4();
        let tenant_other = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant_caller, user);
        let router = make_router(h.service, ctx);

        let body = serde_json::to_vec(&presign_upload_batch(tenant_other, user, h.backend_id))
            .unwrap();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/presign-batch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Per-item error (tenant mismatch surfaces as access_denied for the
        // upload item).
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert!(v["items"][0]["error"].is_string());
    }

    // ── PUT /files/{id} (status branch) ─────────────────────────────────────

    #[tokio::test]
    async fn update_file_status_branch_ok_and_returns_etag() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;

        let body = json!({
            "status": "uploaded",
            "new_etag": "fresh-content-hash"
        });
        let etag_pinned = compose("", 0);
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, format!("\"{etag_pinned}\""))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let etag = resp.headers().get(header::ETAG).unwrap().to_str().unwrap();
        assert!(etag.starts_with('"') && etag.ends_with('"'), "got: {etag}");
    }

    #[tokio::test]
    async fn update_file_status_branch_412_on_etag_mismatch() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;

        let body = json!({
            "status": "uploaded",
            "new_etag": "ignored"
        });
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, "\"wrong-etag\"")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
    }

    #[tokio::test]
    async fn update_file_rejects_mixed_branches() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;

        let body = json!({
            "status": "uploaded",
            "new_etag": "x",
            "name": "renamed.txt"
        });
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, format!("\"{}\"", compose("", 0)))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_file_rejects_empty_body() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let body = json!({});
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{}", Uuid::new_v4()))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, "\"x\"")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── GET /files/{id} ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_file_ok() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/files/{file_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert_eq!(v["status"], "pending_upload");
    }

    #[tokio::test]
    async fn get_file_returns_304_when_if_none_match_matches() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;
        let etag_quoted = format!(r#""{}""#, compose("", 0));

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::IF_NONE_MATCH, etag_quoted)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn get_file_returns_404_for_unknown_id() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/files/{}", Uuid::new_v4()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── PUT /files/{id} (metadata branch) ──────────────────────────────────

    #[tokio::test]
    async fn update_file_metadata_branch_requires_if_match_header() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let body = json!({ "name": "x" });
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{}", Uuid::new_v4()))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_file_metadata_branch_ok() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;

        // Promote to Uploaded
        let status_body = json!({
            "status": "uploaded",
            "new_etag": "ch"
        });
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, format!("\"{}\"", compose("", 0)))
                    .body(Body::from(serde_json::to_vec(&status_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag_after = resp
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();

        let put_body = json!({ "name": "renamed.txt" });
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, &etag_after)
                    .body(Body::from(serde_json::to_vec(&put_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert_eq!(v["meta"]["name"], "renamed.txt");
    }

    // ── DELETE /files/{id} ──────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_file_requires_if_match_header() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&format!("/files/{}", Uuid::new_v4()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_file_returns_204_on_happy_path() {
        let h = build_service().await;
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user);
        let router = make_router(h.service, ctx);
        let file_id = presign_and_get_id(&router, tenant, user, h.backend_id).await;

        let body = json!({
            "status": "uploaded",
            "new_etag": "ch"
        });
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, format!("\"{}\"", compose("", 0)))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = resp
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&format!("/files/{file_id}"))
                    .header(header::IF_MATCH, &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    // ── GET /files (list) ───────────────────────────────────────────────────

    #[tokio::test]
    async fn list_files_returns_ok_with_empty_items() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/files")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert!(v["items"].as_array().unwrap().is_empty());
    }

    // ── POST /presign-batch (download) ──────────────────────────────────────

    #[tokio::test]
    async fn presign_batch_download_returns_outcome_array() {
        let h = build_service().await;
        let ctx = ctx_for(Uuid::new_v4(), Uuid::new_v4());
        let router = make_router(h.service, ctx);
        let body = json!({
            "items": [
                {
                    "kind": "download",
                    "file_id": Uuid::new_v4(),
                    "params": {
                        "expires_in_seconds": 600,
                        "content_disposition": null,
                        "content_type_override": null,
                        "allowed_client_cidrs": [],
                        "refresh_etag": false
                    },
                    "etag": null
                }
            ]
        });
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/presign-batch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_json(resp).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        assert_eq!(v["items"][0]["kind"], "download");
        // Per item, error path: file_id is unknown → "not found".
        assert!(v["items"][0]["error"].is_string());
    }
}
