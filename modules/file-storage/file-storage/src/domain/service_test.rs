//! Service-level integration tests.

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use authz_resolver_sdk::{
        AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
        constraints::{Constraint, InPredicate, Predicate},
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use file_storage_sdk::{
        Backend, BackendCapability, BackendId, BackendKind, BackendTransport, FileMeta,
        FileMetaUpdate, FileStatus, ListFilesQuery, OwnerRef, PresignDownloadItem,
        PresignedDownload, UrlParams,
    };
    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::{ConnectOpts, DBProvider, connect_db};
    use modkit_security::{SecurityContext, pep_properties};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use tokio::sync::Mutex as TokioMutex;
    use uuid::Uuid;

    use crate::config::FileStorageConfig;
    use crate::domain::error::DomainError;
    use crate::domain::etag::compose;
    use crate::domain::local_client::LocalClient;
    use crate::domain::service::Service;
    use crate::infra::backends::registry::BackendRegistry;
    use crate::infra::backends::r#trait::{
        BackendDescriptor, BackendObjectKey, BackendReadResult, HeadResult, PresignedGetItem,
        PresignedGetOutcome, SharedBackend, StorageBackend,
    };
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::sea_orm_repo::SeaOrmFilesRepository;

    struct AllowAuthZ;

    #[async_trait]
    impl AuthZResolverClient for AllowAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            let tenant_id = request
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

            let mut predicates = vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [tenant_id],
            ))];
            if let Some(rid) = request.resource.id {
                predicates.push(Predicate::In(InPredicate::new(
                    pep_properties::RESOURCE_ID,
                    [rid],
                )));
            }
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint { predicates }],
                    ..Default::default()
                },
            })
        }
    }

    struct FakeBackend {
        descriptor: BackendDescriptor,
        presigned_put_url: String,
        presigned_get_url: String,
        head_content_hash: StdMutex<String>,
        delete_calls: StdMutex<Vec<BackendObjectKey>>,
        put_calls: StdMutex<u32>,
    }

    impl FakeBackend {
        fn new(id: BackendId, caps: Vec<BackendCapability>, tenant_access: Vec<Uuid>) -> Self {
            Self {
                descriptor: BackendDescriptor {
                    sdk: Backend {
                        id,
                        kind: BackendKind::S3Compatible,
                        default_public: false,
                        default_private: true,
                        transport: BackendTransport::Redirect,
                        capabilities: caps,
                        max_file_size_bytes: Some(10 * 1024 * 1024),
                    },
                    max_signed_url_ttl_seconds_value: 3_600,
                    tenant_access,
                },
                presigned_put_url: "https://example.test/upload?sig=abc".to_owned(),
                presigned_get_url: "https://example.test/download?sig=def".to_owned(),
                head_content_hash: StdMutex::new("backend-hash".to_owned()),
                delete_calls: StdMutex::new(vec![]),
                put_calls: StdMutex::new(0),
            }
        }
        fn delete_count(&self) -> usize {
            self.delete_calls.lock().unwrap().len()
        }
        fn put_count(&self) -> u32 {
            *self.put_calls.lock().unwrap()
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
        async fn delete_object(&self, key: &BackendObjectKey) -> Result<(), DomainError> {
            self.delete_calls.lock().unwrap().push(key.clone());
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
            *self.put_calls.lock().unwrap() += 1;
            Ok(self.presigned_put_url.clone())
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
                        url: self.presigned_get_url.clone(),
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

    type ConcreteService = Service<SeaOrmFilesRepository>;

    struct Harness {
        service: Arc<ConcreteService>,
        backend: Arc<FakeBackend>,
        backend_id: BackendId,
    }

    async fn build_harness() -> Harness {
        build_harness_with_caps(vec![
            BackendCapability::PresignedUrls,
            BackendCapability::PresignedConditionalPut,
        ])
        .await
    }

    async fn build_harness_with_caps(caps: Vec<BackendCapability>) -> Harness {
        let backend_id = Uuid::new_v4();
        build_harness_custom(backend_id, caps, vec![], Some(backend_id)).await
    }

    async fn build_harness_with_tenant_access(
        tenant_access: Vec<Uuid>,
    ) -> Harness {
        let backend_id = Uuid::new_v4();
        build_harness_custom(
            backend_id,
            vec![
                BackendCapability::PresignedUrls,
                BackendCapability::PresignedConditionalPut,
            ],
            tenant_access,
            Some(backend_id),
        )
        .await
    }

    async fn build_harness_custom(
        backend_id: BackendId,
        caps: Vec<BackendCapability>,
        tenant_access: Vec<Uuid>,
        default_private_storage_id: Option<BackendId>,
    ) -> Harness {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("connect sqlite");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");

        let provider = Arc::new(DBProvider::<modkit_db::DbError>::new(db));
        let repo = Arc::new(SeaOrmFilesRepository::new());

        let backend = Arc::new(FakeBackend::new(backend_id, caps, tenant_access));
        let mut map: HashMap<BackendId, SharedBackend> = HashMap::new();
        map.insert(backend_id, backend.clone());
        let registry = Arc::new(BackendRegistry::new(map));

        let cfg = Arc::new(FileStorageConfig {
            default_private_storage_id,
            default_public_storage_id: None,
            orphan_delete_grace_seconds: 86_400,
            signed_url_clock_skew_margin_seconds: 60,
            backends: vec![],
        });

        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowAuthZ);
        let enforcer = PolicyEnforcer::new(authz);

        let orphan_queue: crate::domain::service::OrphanQueue =
            Arc::new(TokioMutex::new(VecDeque::new()));

        let service = Arc::new(Service::new(
            provider,
            repo,
            enforcer,
            cfg,
            registry,
            orphan_queue,
        ));
        Harness {
            service,
            backend,
            backend_id,
        }
    }

    fn ctx_for(tenant_id: Uuid, user_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(user_id)
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap()
    }

    fn fresh_meta() -> FileMeta {
        FileMeta {
            name: "doc.txt".to_owned(),
            mime_type: "text/plain".to_owned(),
            gts_file_type: "gts.cf.fstorage.file.type.v1~doc~".to_owned(),
            size_bytes: Some(5),
            custom_metadata: Default::default(),
        }
    }

    fn user_owner(tenant_id: Uuid, user_id: Uuid) -> OwnerRef {
        OwnerRef {
            tenant_id,
            owner_id: user_id,
        }
    }

    // ── list_backends ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_backends_returns_visible_roster() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());

        let backends = h.service.list_backends(&ctx).await.unwrap();
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].id, h.backend_id);
    }

    #[tokio::test]
    async fn list_backends_excludes_hidden_backend() {
        let allowed = Uuid::new_v4();
        let other = Uuid::new_v4();
        let h = build_harness_with_tenant_access(vec![allowed]).await;
        let ctx = ctx_for(other, Uuid::new_v4());
        let backends = h.service.list_backends(&ctx).await.unwrap();
        assert!(backends.is_empty());
    }

    // ── create_presigned_url ────────────────────────────────────────────────

    #[tokio::test]
    async fn create_presigned_url_rejects_owner_tenant_mismatch() {
        let h = build_harness().await;
        let caller_tenant = Uuid::new_v4();
        let other_tenant = Uuid::new_v4();
        let ctx = ctx_for(caller_tenant, Uuid::new_v4());

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(other_tenant, Uuid::new_v4()),
                "a/b",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::AccessDenied(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_empty_meta_name() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());
        let mut meta = fresh_meta();
        meta.name = String::new();

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, Uuid::new_v4()),
                "a/b",
                meta,
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::BadRequest(_)));
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_invalid_gts_prefix() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());
        let mut meta = fresh_meta();
        meta.gts_file_type = "non-gts-prefix".to_owned();

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, Uuid::new_v4()),
                "a/b",
                meta,
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::BadRequest(_)));
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_empty_file_path() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, Uuid::new_v4()),
                "",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::BadRequest(_)));
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_empty_mime_type() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());
        let mut meta = fresh_meta();
        meta.mime_type = String::new();

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, Uuid::new_v4()),
                "a/b",
                meta,
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BadRequest(s) if s.contains("mime_type")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_overlong_name() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());
        let mut meta = fresh_meta();
        meta.name = "x".repeat(513);

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, Uuid::new_v4()),
                "a/b",
                meta,
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BadRequest(s) if s.contains("512")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn create_presigned_url_happy_path_returns_handle() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let handle = h
            .service
            .create_presigned_url(
                &ctx,
                Some(h.backend_id),
                user_owner(tenant, user_id),
                "a/b.txt",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap();
        assert_eq!(handle.upload_url, "https://example.test/upload?sig=abc");
        assert_eq!(handle.etag_pinned, compose("", 0));
        assert_eq!(h.backend.put_count(), 1);
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_payload_too_large() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let mut meta = fresh_meta();
        meta.size_bytes = Some(20 * 1024 * 1024);

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, user_id),
                "p",
                meta,
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::PayloadTooLarge { .. }));
    }

    #[tokio::test]
    async fn create_presigned_url_falls_back_to_default_backend() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let handle = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, user_id),
                "p",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap();
        assert!(!handle.upload_url.is_empty());
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_when_backend_lacks_presigned_urls_capability() {
        let h = build_harness_with_caps(vec![/* no caps */]).await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                Some(h.backend_id),
                user_owner(tenant, user_id),
                "p",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::CapabilityUnavailable(s) if s.contains("PresignedUrls")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn create_presigned_url_rejects_when_backend_omitted_and_default_unset() {
        let backend_id = Uuid::new_v4();
        let h = build_harness_custom(
            backend_id,
            vec![
                BackendCapability::PresignedUrls,
                BackendCapability::PresignedConditionalPut,
            ],
            vec![],
            None, // default unset
        )
        .await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let err = h
            .service
            .create_presigned_url(
                &ctx,
                None,
                user_owner(tenant, user_id),
                "p",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BadRequest(s)
                if s.contains("default_private_storage_id")),
            "got: {err:?}"
        );
    }

    // ── change_status: lifecycle ────────────────────────────────────────────

    async fn create_pending_then_get_id(
        h: &Harness,
        ctx: &SecurityContext,
        user_id: Uuid,
    ) -> Uuid {
        let handle = h
            .service
            .create_presigned_url(
                ctx,
                Some(h.backend_id),
                user_owner(ctx.subject_tenant_id(), user_id),
                "p/file",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap();
        handle.file_id
    }

    #[tokio::test]
    async fn change_status_rejects_non_uploaded_target() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let err = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::PendingUpload,
                compose("", 0),
                "anything".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidStatusTransition(_)));
    }

    #[tokio::test]
    async fn change_status_not_found_for_unknown_file_id() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());

        let err = h
            .service
            .change_status(
                &ctx,
                Uuid::new_v4(),
                FileStatus::Uploaded,
                "a".to_owned(),
                "b".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }

    #[tokio::test]
    async fn change_status_etag_mismatch_when_old_etag_wrong() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let err = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                "wrong-etag".to_owned(),
                "ignored".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::EtagMismatch));
    }

    #[tokio::test]
    async fn change_status_happy_path_marks_uploaded() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let info = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "fresh-content-hash".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(info.status, FileStatus::Uploaded);
        assert_eq!(info.etag, compose("fresh-content-hash", 1));
    }

    // ── get_file_info ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_file_info_not_found_for_other_tenant() {
        let h = build_harness().await;
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx_a = ctx_for(tenant_a, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx_a, user_id).await;

        let ctx_b = ctx_for(tenant_b, Uuid::new_v4());
        let err = h
            .service
            .get_file_info(&ctx_b, file_id, None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }

    #[tokio::test]
    async fn get_file_info_etag_fail_fast_when_pinned_mismatches() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let err = h
            .service
            .get_file_info(&ctx, file_id, Some(&"stale-etag".to_owned()))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::EtagMismatch));
    }

    #[tokio::test]
    async fn get_file_info_happy_path_returns_row() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let info = h.service.get_file_info(&ctx, file_id, None).await.unwrap();
        assert_eq!(info.file_id, file_id);
        assert_eq!(info.status, FileStatus::PendingUpload);
        assert_eq!(info.backend_id, h.backend_id);
    }

    // ── put_file_info ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn put_file_info_etag_mismatch_when_etag_wrong() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let err = h
            .service
            .put_file_info(
                &ctx,
                file_id,
                FileMetaUpdate {
                    name: Some("new".to_owned()),
                    mime_type: None,
                    custom_metadata: None,
                },
                "stale-etag".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::EtagMismatch));
    }

    #[tokio::test]
    async fn put_file_info_happy_path_updates_only_provided_fields() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let info = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let updated = h
            .service
            .put_file_info(
                &ctx,
                file_id,
                FileMetaUpdate {
                    name: Some("renamed.txt".to_owned()),
                    mime_type: None,
                    custom_metadata: None,
                },
                info.etag.clone(),
            )
            .await
            .unwrap();
        assert_eq!(updated.meta.name, "renamed.txt");
        assert_eq!(updated.meta.mime_type, "text/plain");
    }

    // ── delete_file ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_file_etag_mismatch_when_etag_wrong() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let err = h
            .service
            .delete_file(&ctx, file_id, "wrong".to_owned())
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::EtagMismatch));
    }

    #[tokio::test]
    async fn delete_file_happy_path_enqueues_orphan() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let info = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        h.service
            .delete_file(&ctx, file_id, info.etag.clone())
            .await
            .expect("delete must succeed");

        let queue = h.service.orphan_queue();
        let q = queue.lock().await;
        assert_eq!(q.len(), 1, "delete must enqueue an orphan-delete entry");
        let entry = q.front().unwrap();
        assert_eq!(entry.backend_id, h.backend_id);
        assert_eq!(entry.file_id, file_id);
    }

    // ── list_files ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_files_returns_only_caller_files_by_default() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let _ = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = create_pending_then_get_id(&h, &ctx, other_user).await;

        let list = h
            .service
            .list_files(&ctx, ListFilesQuery::default())
            .await
            .unwrap();
        // Default scope = caller's subject_id.
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].owner.owner_id, user_id);
    }

    #[tokio::test]
    async fn list_files_owner_filter_restricts_to_specified_owner() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let _ = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = create_pending_then_get_id(&h, &ctx, other_user).await;

        let list = h
            .service
            .list_files(
                &ctx,
                ListFilesQuery {
                    owner_id: Some(other_user),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].owner.owner_id, other_user);
    }

    #[tokio::test]
    async fn list_files_caps_limit_at_200() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());
        let list = h
            .service
            .list_files(
                &ctx,
                ListFilesQuery {
                    limit: Some(10_000),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(list.items.is_empty());
    }

    // ── presign_urls (batch) ────────────────────────────────────────────────

    #[tokio::test]
    async fn presign_urls_returns_outcome_per_item() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let info = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let bogus_id = Uuid::new_v4();

        let outcomes = h
            .service
            .presign_urls(
                &ctx,
                vec![
                    PresignDownloadItem {
                        file_id: info.file_id,
                        params: UrlParams::default(),
                        etag: Some(info.etag.clone()),
                    },
                    PresignDownloadItem {
                        file_id: bogus_id,
                        params: UrlParams::default(),
                        etag: None,
                    },
                ],
            )
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].result.is_ok());
        assert!(outcomes[1].result.is_err());
        assert!(matches!(
            outcomes[1].result.as_ref().unwrap_err(),
            file_storage_sdk::FileStorageError::NotFound
        ));
    }

    #[tokio::test]
    async fn presign_urls_etag_mismatch_per_item() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let outcomes = h
            .service
            .presign_urls(
                &ctx,
                vec![PresignDownloadItem {
                    file_id,
                    params: UrlParams::default(),
                    etag: Some("stale-etag".to_owned()),
                }],
            )
            .await
            .unwrap();
        assert!(matches!(
            outcomes[0].result.as_ref().unwrap_err(),
            file_storage_sdk::FileStorageError::EtagMismatch
        ));
    }

    #[tokio::test]
    async fn presign_urls_not_found_for_pending_upload_status() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let outcomes = h
            .service
            .presign_urls(
                &ctx,
                vec![PresignDownloadItem {
                    file_id,
                    params: UrlParams::default(),
                    etag: None,
                }],
            )
            .await
            .unwrap();
        assert!(matches!(
            outcomes[0].result.as_ref().unwrap_err(),
            file_storage_sdk::FileStorageError::NotFound
        ));
    }

    // ── read_file ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_file_not_found_for_pending_status() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;

        let err = h.service.read_file(&ctx, file_id, None).await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }

    #[tokio::test]
    async fn read_file_happy_path_self_heal_no_op() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let handle = h.service.read_file(&ctx, file_id, None).await.unwrap();
        assert_eq!(handle.info.file_id, file_id);
        assert_eq!(handle.info.status, FileStatus::Uploaded);
    }

    #[tokio::test]
    async fn read_file_etag_mismatch_when_pin_stale_after_self_heal_repair() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();

        let stale = "definitely-not-the-derived-etag".to_owned();
        let err = h
            .service
            .read_file(&ctx, file_id, Some(&stale))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::EtagMismatch), "got: {err:?}");
    }

    #[tokio::test]
    async fn read_file_etag_mismatch_when_pin_stale_in_already_consistent_path() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = h
            .service
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "backend-hash".to_owned(),
            )
            .await
            .unwrap();

        let stale = "stale-pin".to_owned();
        let err = h
            .service
            .read_file(&ctx, file_id, Some(&stale))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::EtagMismatch), "got: {err:?}");
    }

    // ── LocalClient bridges through the Service ─────────────────────────────

    #[tokio::test]
    async fn local_client_bridges_list_backends_to_service() {
        use file_storage_sdk::FileStorageClient;
        let h = build_harness().await;
        let lc = LocalClient::new(h.service.clone());
        let tenant = Uuid::new_v4();
        let ctx = ctx_for(tenant, Uuid::new_v4());

        let backends = lc.list_backends(&ctx).await.unwrap();
        assert_eq!(backends.len(), 1);
    }

    #[tokio::test]
    async fn local_client_bridges_create_presigned_url_to_service() {
        use file_storage_sdk::FileStorageClient;
        let h = build_harness().await;
        let lc = LocalClient::new(h.service.clone());
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);

        let handle = lc
            .create_presigned_url(
                &ctx,
                Some(h.backend_id),
                user_owner(tenant, user_id),
                "p",
                fresh_meta(),
                UrlParams::default(),
            )
            .await
            .unwrap();
        assert!(!handle.upload_url.is_empty());
    }

    #[tokio::test]
    async fn local_client_bridges_get_and_delete() {
        use file_storage_sdk::FileStorageClient;
        let h = build_harness().await;
        let lc = LocalClient::new(h.service.clone());
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let info = lc.get_file_info(&ctx, file_id, None).await.unwrap();
        assert_eq!(info.file_id, file_id);
        let info = lc
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();
        lc.delete_file(&ctx, file_id, info.etag).await.unwrap();
    }

    #[tokio::test]
    async fn local_client_list_and_presign_urls() {
        use file_storage_sdk::FileStorageClient;
        let h = build_harness().await;
        let lc = LocalClient::new(h.service.clone());
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let _ = create_pending_then_get_id(&h, &ctx, user_id).await;

        let list = lc.list_files(&ctx, ListFilesQuery::default()).await.unwrap();
        assert_eq!(list.items.len(), 1);

        let outcomes = lc
            .presign_urls(
                &ctx,
                vec![PresignDownloadItem {
                    file_id: list.items[0].file_id,
                    params: UrlParams::default(),
                    etag: None,
                }],
            )
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 1);
    }

    #[tokio::test]
    async fn local_client_put_file_info_bridges() {
        use file_storage_sdk::FileStorageClient;
        let h = build_harness().await;
        let lc = LocalClient::new(h.service.clone());
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let info = lc
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();
        let updated = lc
            .put_file_info(
                &ctx,
                file_id,
                FileMetaUpdate {
                    name: Some("renamed.txt".to_owned()),
                    mime_type: None,
                    custom_metadata: None,
                },
                info.etag,
            )
            .await
            .unwrap();
        assert_eq!(updated.meta.name, "renamed.txt");
    }

    #[tokio::test]
    async fn local_client_read_file_bridges() {
        use file_storage_sdk::FileStorageClient;
        let h = build_harness().await;
        let lc = LocalClient::new(h.service.clone());
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let file_id = create_pending_then_get_id(&h, &ctx, user_id).await;
        let _ = lc
            .change_status(
                &ctx,
                file_id,
                FileStatus::Uploaded,
                compose("", 0),
                "ch".to_owned(),
            )
            .await
            .unwrap();
        let handle = lc.read_file(&ctx, file_id, None).await.unwrap();
        assert_eq!(handle.info.file_id, file_id);
    }

    #[tokio::test]
    async fn delete_count_unaffected_by_pending_or_uploaded_paths_before_orphan_drain() {
        let h = build_harness().await;
        let tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let ctx = ctx_for(tenant, user_id);
        let _ = create_pending_then_get_id(&h, &ctx, user_id).await;
        assert_eq!(h.backend.delete_count(), 0);
    }
}
