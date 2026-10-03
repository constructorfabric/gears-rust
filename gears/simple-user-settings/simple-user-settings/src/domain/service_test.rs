//! Integration tests for the Settings service.
//!
//! These tests use an in-memory `SQLite` database since `DBRunner` is a sealed trait
//! and cannot be mocked. All tests use real database operations.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use authz_resolver_sdk::{
        AuthZResolverApi, PolicyEnforcer,
        constraints::{Constraint, InPredicate, Predicate},
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use simple_user_settings_sdk::models::{
        NamedSetting, SimpleUserSettings, SimpleUserSettingsPatch, SimpleUserSettingsUpdate,
    };
    use toolkit::api::canonical_prelude::CanonicalError;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::secure::{
        DBRunner, TxIsolationLevel, transaction_id_for_testing, transaction_isolation_for_testing,
    };
    use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
    use uuid::Uuid;

    use crate::domain::error::DomainError;
    use crate::domain::repo::SettingsRepository;
    use crate::domain::service::{Service, ServiceConfig};
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::sea_orm_repo::SeaOrmSettingsRepository;

    type ConcreteService = Service<SeaOrmSettingsRepository>;

    /// Mock `AuthZ` resolver for personal user settings.
    ///
    /// Derives tenant from `context.tenant_context.root_id` if present,
    /// otherwise falls back to `subject.properties.tenant_id` (like a real PDP).
    /// Always returns:
    /// - `OWNER_TENANT_ID` constraint from the resolved tenant
    /// - `RESOURCE_ID` constraint from `resource.id` (the user whose settings are accessed)
    struct MockAuthZResolver;

    #[async_trait]
    impl AuthZResolverApi for MockAuthZResolver {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            // Resolve tenant: explicit context > subject property (like a real PDP)
            let root_id = request
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
                .ok_or_else(|| {
                    CanonicalError::internal("tenant context is required".to_owned()).create()
                })?;

            let mut predicates = vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [root_id],
            ))];

            // Use resource.id for RESOURCE_ID constraint
            if let Some(resource_id) = request.resource.id {
                predicates.push(Predicate::In(InPredicate::new(
                    pep_properties::RESOURCE_ID,
                    [resource_id],
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

    /// Create an in-memory database with migrations applied.
    async fn inmem_db() -> Db {
        use sea_orm_migration::MigratorTrait;

        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("Failed to connect to in-memory database");

        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("Failed to run migrations");

        db
    }

    fn create_test_context() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .unwrap()
    }

    fn build_service(db: Db, config: ServiceConfig) -> ConcreteService {
        build_service_with(db, config, Arc::new(MockAuthZResolver))
    }

    fn build_service_with(
        db: Db,
        config: ServiceConfig,
        authz: Arc<dyn AuthZResolverApi>,
    ) -> ConcreteService {
        let repo = Arc::new(SeaOrmSettingsRepository::new());
        let db: Arc<DBProvider<toolkit_db::DbError>> = Arc::new(DBProvider::new(db));
        let policy_enforcer = PolicyEnforcer::new(authz);
        Service::new(db, repo, policy_enforcer, config)
    }

    /// A PDP that clamps to the caller's tenant and nothing more.
    ///
    /// The shape the platform's static-authz plugin returns, and a legitimate
    /// one for any PDP: "this subject may use settings in its tenant" says
    /// nothing about *which user's* row. Choosing the row is the gear's job.
    struct TenantOnlyAuthZ;

    #[async_trait]
    impl AuthZResolverApi for TenantOnlyAuthZ {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            let tenant = request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| CanonicalError::internal("no tenant".to_owned()).create())?;
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [tenant],
                        ))],
                    }],
                    ..Default::default()
                },
            })
        }
    }

    fn in_tenant(subject_id: Uuid, tenant_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(subject_id)
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap()
    }

    // =========================================================================
    // a tenant-wide grant still reads and writes only the caller's own row
    // =========================================================================

    #[tokio::test]
    async fn a_colleague_in_the_same_tenant_does_not_read_my_settings() {
        let service = build_service_with(
            inmem_db().await,
            ServiceConfig::default(),
            Arc::new(TenantOnlyAuthZ),
        );
        let org = Uuid::new_v4();
        let me = in_tenant(Uuid::from_u128(1), org);
        let colleague = in_tenant(Uuid::from_u128(2), org);

        service
            .update_settings(
                &me,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .expect("stored");

        let seen = service.get_settings(&colleague).await.expect("read");
        assert_eq!(seen.user_id, colleague.subject_id());
        assert_eq!(seen.theme, None, "my theme is not the colleague's");
        assert_eq!(seen.language, None);
    }

    #[tokio::test]
    async fn a_colleague_patch_does_not_pick_up_my_fields() {
        let service = build_service_with(
            inmem_db().await,
            ServiceConfig::default(),
            Arc::new(TenantOnlyAuthZ),
        );
        let org = Uuid::new_v4();
        let me = in_tenant(Uuid::from_u128(1), org);
        let colleague = in_tenant(Uuid::from_u128(2), org);

        service
            .update_settings(
                &me,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .expect("stored");

        let patched = service
            .patch_settings(
                &colleague,
                SimpleUserSettingsPatch {
                    theme: Some("light".to_owned()),
                    language: None,
                },
            )
            .await
            .expect("patched");
        assert_eq!(patched.language, None, "my language did not leak in");

        let mine = service.get_settings(&me).await.expect("read");
        assert_eq!(mine.theme.as_deref(), Some("dark"), "and mine is intact");
    }

    // =========================================================================
    // get_settings tests
    // =========================================================================

    #[tokio::test]
    async fn test_get_settings_returns_defaults_when_not_found() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        let result = service.get_settings(&ctx).await.unwrap();

        assert_eq!(result.user_id, ctx.subject_id());
        assert_eq!(result.tenant_id, ctx.subject_tenant_id());
        assert_eq!(result.theme, None);
        assert_eq!(result.language, None);
    }

    #[tokio::test]
    async fn test_get_settings_returns_existing() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        // First, create settings
        let _ = service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .unwrap();

        // Then retrieve them
        let result = service.get_settings(&ctx).await.unwrap();

        assert_eq!(result.theme, Some("dark".to_owned()));
        assert_eq!(result.language, Some("en".to_owned()));
    }

    // =========================================================================
    // update_settings tests
    // =========================================================================

    #[tokio::test]
    async fn test_update_settings_success() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        let result = service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: "light".to_owned(),
                    language: "es".to_owned(),
                },
            )
            .await
            .unwrap();

        assert_eq!(result.theme, Some("light".to_owned()));
        assert_eq!(result.language, Some("es".to_owned()));
        assert_eq!(result.user_id, ctx.subject_id());
        assert_eq!(result.tenant_id, ctx.subject_tenant_id());
    }

    #[tokio::test]
    async fn test_update_settings_validates_max_length_for_theme() {
        let db = inmem_db().await;
        let service = build_service(
            db,
            ServiceConfig {
                max_field_length: 10,
                ..ServiceConfig::default()
            },
        );
        let ctx = create_test_context();

        let too_long = "a".repeat(11);
        let result = service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: too_long,
                    language: "en".to_owned(),
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DomainError::Validation { field, .. } if field == "theme"));
    }

    #[tokio::test]
    async fn test_update_settings_validates_max_length_for_language() {
        let db = inmem_db().await;
        let service = build_service(
            db,
            ServiceConfig {
                max_field_length: 10,
                ..ServiceConfig::default()
            },
        );
        let ctx = create_test_context();

        let too_long = "a".repeat(11);
        let result = service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: too_long,
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DomainError::Validation { field, .. } if field == "language"));
    }

    // =========================================================================
    // patch_settings tests
    // =========================================================================

    #[tokio::test]
    async fn test_patch_settings_updates_only_provided_fields() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        // First create initial settings
        let _ = service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .unwrap();

        // Patch only theme
        let result = service
            .patch_settings(
                &ctx,
                SimpleUserSettingsPatch {
                    theme: Some("light".to_owned()),
                    language: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(result.theme, Some("light".to_owned()));
        assert_eq!(result.language, Some("en".to_owned())); // Should remain unchanged
    }

    #[tokio::test]
    async fn test_patch_settings_validates_max_length() {
        let db = inmem_db().await;
        let service = build_service(
            db,
            ServiceConfig {
                max_field_length: 10,
                ..ServiceConfig::default()
            },
        );
        let ctx = create_test_context();

        let too_long = "a".repeat(11);
        let result = service
            .patch_settings(
                &ctx,
                SimpleUserSettingsPatch {
                    theme: None,
                    language: Some(too_long),
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DomainError::Validation { field, .. } if field == "language"));
    }

    #[tokio::test]
    async fn test_patch_settings_empty_patch_returns_existing() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        // First create settings
        let _ = service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .unwrap();

        // Empty patch - no fields to update
        let result = service
            .patch_settings(
                &ctx,
                SimpleUserSettingsPatch {
                    theme: None,
                    language: None,
                },
            )
            .await
            .unwrap();

        // Should return existing values unchanged
        assert_eq!(result.theme, Some("dark".to_owned()));
        assert_eq!(result.language, Some("en".to_owned()));
    }

    #[tokio::test]
    async fn test_patch_settings_creates_if_not_exists() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        // Patch without existing settings
        let result = service
            .patch_settings(
                &ctx,
                SimpleUserSettingsPatch {
                    theme: Some("dark".to_owned()),
                    language: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(result.theme, Some("dark".to_owned()));
        assert_eq!(result.language, None);
    }

    // =========================================================================
    // Tenant isolation tests
    // =========================================================================

    #[tokio::test]
    async fn test_settings_isolated_by_user() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());

        let tenant_id = Uuid::new_v4();
        let user1 = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap();
        let user2 = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap();

        // User 1 creates settings
        let _ = service
            .update_settings(
                &user1,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .unwrap();

        // User 2 should get default settings
        let result = service.get_settings(&user2).await.unwrap();
        assert_eq!(result.theme, None);
        assert_eq!(result.language, None);
        assert_eq!(result.user_id, user2.subject_id());
    }

    #[tokio::test]
    async fn test_settings_isolated_by_tenant() {
        let db = inmem_db().await;
        let service = build_service(db, ServiceConfig::default());

        let user_id = Uuid::new_v4();
        let tenant1 = SecurityContext::builder()
            .subject_id(user_id)
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .unwrap();
        let tenant2 = SecurityContext::builder()
            .subject_id(user_id)
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .unwrap();

        // Same user in tenant 1 creates settings
        let _ = service
            .update_settings(
                &tenant1,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .unwrap();

        // Same user in tenant 2 should get default settings
        let result = service.get_settings(&tenant2).await.unwrap();
        assert_eq!(result.theme, None);
        assert_eq!(result.language, None);
        assert_eq!(result.tenant_id, tenant2.subject_tenant_id());
    }

    // =========================================================================
    // named settings
    // =========================================================================

    async fn named_service(config: ServiceConfig) -> ConcreteService {
        build_service(inmem_db().await, config)
    }

    /// Any JSON value goes in and comes back out unchanged.
    #[tokio::test]
    async fn a_named_setting_round_trips_any_json() {
        let service = named_service(ServiceConfig::default()).await;
        let ctx = create_test_context();

        let values = [
            ("portal.projects.view", serde_json::json!("table")),
            ("portal.sidebar.width", serde_json::json!(280)),
            (
                "portal.hints.dismissed",
                serde_json::json!(["welcome", "tour"]),
            ),
            ("portal.editor", serde_json::json!({"wrap": true, "tab": 4})),
            ("portal.flag", serde_json::json!(null)),
        ];
        for (key, value) in &values {
            let stored = service
                .put_named_setting(&ctx, key, value.clone())
                .await
                .expect("stored");
            assert_eq!(&stored.value, value);
        }
        for (key, value) in &values {
            let seen = service
                .get_named_setting(&ctx, key)
                .await
                .expect("read")
                .expect("present");
            assert_eq!(&seen.value, value, "{key}");
        }
    }

    #[tokio::test]
    async fn a_named_setting_that_was_never_set_is_absent() {
        let service = named_service(ServiceConfig::default()).await;
        let seen = service
            .get_named_setting(&create_test_context(), "portal.projects.view")
            .await
            .expect("read");
        assert_eq!(seen, None);
    }

    #[tokio::test]
    async fn putting_a_named_setting_again_replaces_it() {
        let service = named_service(ServiceConfig::default()).await;
        let ctx = create_test_context();

        service
            .put_named_setting(&ctx, "portal.projects.view", serde_json::json!("table"))
            .await
            .expect("first");
        service
            .put_named_setting(&ctx, "portal.projects.view", serde_json::json!("tiles"))
            .await
            .expect("second");

        let all = service.list_named_settings(&ctx).await.expect("list");
        assert_eq!(all.len(), 1, "replaced, not duplicated");
        assert_eq!(all[0].value, serde_json::json!("tiles"));
    }

    #[tokio::test]
    async fn named_settings_list_in_key_order() {
        let service = named_service(ServiceConfig::default()).await;
        let ctx = create_test_context();
        for key in ["b.second", "c.third", "a.first"] {
            service
                .put_named_setting(&ctx, key, serde_json::json!(key))
                .await
                .expect("stored");
        }

        let keys: Vec<String> = service
            .list_named_settings(&ctx)
            .await
            .expect("list")
            .into_iter()
            .map(|s| s.key)
            .collect();
        assert_eq!(keys, ["a.first", "b.second", "c.third"]);
    }

    /// Deleting says whether there was anything to delete, and deleting twice
    /// is not an error.
    #[tokio::test]
    async fn deleting_a_named_setting_forgets_it() {
        let service = named_service(ServiceConfig::default()).await;
        let ctx = create_test_context();
        service
            .put_named_setting(&ctx, "portal.projects.view", serde_json::json!("table"))
            .await
            .expect("stored");

        let existed = service
            .delete_named_setting(&ctx, "portal.projects.view")
            .await
            .expect("deleted");
        assert!(existed);
        let again = service
            .delete_named_setting(&ctx, "portal.projects.view")
            .await
            .expect("deleting again is fine");
        assert!(!again);
        assert_eq!(
            service
                .get_named_setting(&ctx, "portal.projects.view")
                .await
                .expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn malformed_keys_are_refused() {
        let service = named_service(ServiceConfig::default()).await;
        let ctx = create_test_context();
        let too_long = "k".repeat(129);

        for key in [
            "",
            "has space",
            "a/b",
            "caf\u{e9}",
            "a?b",
            too_long.as_str(),
        ] {
            let err = service
                .put_named_setting(&ctx, key, serde_json::json!(1))
                .await
                .expect_err("refused");
            assert!(
                matches!(&err, DomainError::Validation { field, .. } if field == "key"),
                "{key:?}: {err:?}"
            );
            assert!(
                service.get_named_setting(&ctx, key).await.is_err(),
                "reads validate too: {key:?}"
            );
        }

        let longest = "k".repeat(128);
        service
            .put_named_setting(&ctx, &longest, serde_json::json!(1))
            .await
            .expect("128 characters is allowed");
        service
            .put_named_setting(&ctx, "Portal_2.view-mode:v1", serde_json::json!(1))
            .await
            .expect("every allowed punctuation mark");
    }

    #[tokio::test]
    async fn a_named_value_over_the_size_bound_is_refused() {
        let service = named_service(ServiceConfig {
            named_value_max_bytes: 10,
            ..ServiceConfig::default()
        })
        .await;
        let ctx = create_test_context();

        // `"12345678"` is exactly 10 bytes as JSON, quotes included.
        service
            .put_named_setting(&ctx, "fits", serde_json::json!("12345678"))
            .await
            .expect("at the bound");
        let err = service
            .put_named_setting(&ctx, "too.big", serde_json::json!("123456789"))
            .await
            .expect_err("over the bound");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "value"),
            "{err:?}"
        );
    }

    /// The count bound stops new keys, not replacements, and frees up again
    /// once a key is deleted.
    #[tokio::test]
    async fn the_named_setting_count_bound_applies_to_new_keys() {
        let service = named_service(ServiceConfig {
            named_settings_per_user: 2,
            ..ServiceConfig::default()
        })
        .await;
        let ctx = create_test_context();
        let one = serde_json::json!(1);

        service
            .put_named_setting(&ctx, "a", one.clone())
            .await
            .expect("first");
        service
            .put_named_setting(&ctx, "b", one.clone())
            .await
            .expect("second");

        let err = service
            .put_named_setting(&ctx, "c", one.clone())
            .await
            .expect_err("third new key");
        assert!(matches!(&err, DomainError::LimitReached(_)), "{err:?}");
        assert_eq!(
            service.get_named_setting(&ctx, "c").await.expect("read"),
            None,
            "the refused key was never written"
        );

        service
            .put_named_setting(&ctx, "a", serde_json::json!(2))
            .await
            .expect("replacing at the bound");

        service
            .delete_named_setting(&ctx, "b")
            .await
            .expect("delete");
        service
            .put_named_setting(&ctx, "c", one)
            .await
            .expect("room again after a delete");
    }

    type RecordedCall = (&'static str, Option<u64>, Option<Option<TxIsolationLevel>>);

    /// The real repository, recording for each quota-relevant call the
    /// transaction it ran in. On `SQLite` every transaction is serializable
    /// whatever it is asked for, and the races the quota must survive need
    /// interleavings a test cannot schedule, so what is pinned is the service's
    /// own choice: one transaction, opened `SERIALIZABLE`, around the check
    /// and the write. Downgrading or splitting it changes no result here.
    struct RecordingRepo {
        inner: SeaOrmSettingsRepository,
        calls: std::sync::Mutex<Vec<RecordedCall>>,
    }

    impl RecordingRepo {
        fn new() -> Self {
            Self {
                inner: SeaOrmSettingsRepository::new(),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().expect("calls").push((
                call,
                transaction_id_for_testing(),
                transaction_isolation_for_testing(),
            ));
        }

        fn take(&self) -> Vec<RecordedCall> {
            std::mem::take(&mut *self.calls.lock().expect("calls"))
        }
    }

    #[async_trait]
    impl SettingsRepository for RecordingRepo {
        async fn find_by_user<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            user_id: Uuid,
        ) -> Result<Option<SimpleUserSettings>, DomainError> {
            self.inner
                .find_by_user(conn, scope, tenant_id, user_id)
                .await
        }

        async fn upsert_full<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            user_id: Uuid,
            tenant_id: Uuid,
            theme: Option<String>,
            language: Option<String>,
        ) -> Result<SimpleUserSettings, DomainError> {
            self.inner
                .upsert_full(conn, scope, user_id, tenant_id, theme, language)
                .await
        }

        async fn upsert_patch<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            user_id: Uuid,
            tenant_id: Uuid,
            patch: SimpleUserSettingsPatch,
        ) -> Result<SimpleUserSettings, DomainError> {
            self.inner
                .upsert_patch(conn, scope, user_id, tenant_id, patch)
                .await
        }

        async fn list_named<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            user_id: Uuid,
        ) -> Result<Vec<NamedSetting>, DomainError> {
            self.inner.list_named(conn, scope, tenant_id, user_id).await
        }

        async fn find_named<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            user_id: Uuid,
            key: &str,
        ) -> Result<Option<NamedSetting>, DomainError> {
            self.record("find");
            self.inner
                .find_named(conn, scope, tenant_id, user_id, key)
                .await
        }

        async fn count_named<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            user_id: Uuid,
        ) -> Result<u64, DomainError> {
            self.record("count");
            self.inner
                .count_named(conn, scope, tenant_id, user_id)
                .await
        }

        async fn upsert_named<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            user_id: Uuid,
            tenant_id: Uuid,
            setting: NamedSetting,
        ) -> Result<NamedSetting, DomainError> {
            self.record("upsert");
            self.inner
                .upsert_named(conn, scope, user_id, tenant_id, setting)
                .await
        }

        async fn delete_named<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            user_id: Uuid,
            key: &str,
        ) -> Result<bool, DomainError> {
            self.record("delete");
            self.inner
                .delete_named(conn, scope, tenant_id, user_id, key)
                .await
        }

        async fn delete_all_named<C: DBRunner>(
            &self,
            conn: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            user_id: Uuid,
        ) -> Result<u64, DomainError> {
            self.inner
                .delete_all_named(conn, scope, tenant_id, user_id)
                .await
        }
    }

    /// The names of `calls`, after asserting they all ran in one transaction
    /// and that it was opened `SERIALIZABLE`.
    fn in_one_serializable_transaction(calls: &[RecordedCall]) -> Vec<&'static str> {
        let first_tx = calls.first().and_then(|(_, tx, _)| *tx);
        assert!(first_tx.is_some(), "outside a transaction: {calls:?}");
        for (call, tx, isolation) in calls {
            assert_eq!(
                *tx, first_tx,
                "{call} ran in another transaction: {calls:?}"
            );
            assert_eq!(
                *isolation,
                Some(Some(TxIsolationLevel::Serializable)),
                "{call} was not in a SERIALIZABLE transaction"
            );
        }
        calls.iter().map(|(call, _, _)| *call).collect()
    }

    /// The quota check and the write are one serializable transaction, so a
    /// concurrent delete cannot turn a replacement into an unchecked insert,
    /// and a refused key is never written: there is no compensating delete
    /// that could remove another request's write.
    #[tokio::test]
    async fn the_named_quota_check_and_the_write_are_one_serializable_transaction() {
        let repo = Arc::new(RecordingRepo::new());
        let db: Arc<DBProvider<toolkit_db::DbError>> = Arc::new(DBProvider::new(inmem_db().await));
        let authz: Arc<dyn AuthZResolverApi> = Arc::new(MockAuthZResolver);
        let service = Service::new(
            db,
            Arc::clone(&repo),
            PolicyEnforcer::new(authz),
            ServiceConfig {
                named_settings_per_user: 1,
                ..ServiceConfig::default()
            },
        );
        let ctx = create_test_context();

        service
            .put_named_setting(&ctx, "a", serde_json::json!(1))
            .await
            .expect("a new key below the bound");
        assert_eq!(
            in_one_serializable_transaction(&repo.take()),
            ["find", "count", "upsert"]
        );

        service
            .put_named_setting(&ctx, "a", serde_json::json!(2))
            .await
            .expect("a replacement at the bound");
        assert_eq!(
            in_one_serializable_transaction(&repo.take()),
            ["find", "upsert"],
            "a replacement is not counted"
        );

        let err = service
            .put_named_setting(&ctx, "b", serde_json::json!(1))
            .await
            .expect_err("a new key over the bound");
        assert!(matches!(&err, DomainError::LimitReached(_)), "{err:?}");
        assert_eq!(
            in_one_serializable_transaction(&repo.take()),
            ["find", "count"],
            "refused before writing, and nothing deleted"
        );
    }

    #[tokio::test]
    async fn named_settings_are_isolated_by_user_and_by_tenant() {
        let service = named_service(ServiceConfig::default()).await;
        let org = Uuid::new_v4();
        let person = Uuid::new_v4();
        let caller = |subject, tenant| {
            SecurityContext::builder()
                .subject_id(subject)
                .subject_tenant_id(tenant)
                .build()
                .unwrap()
        };
        let owner = caller(person, org);
        let colleague = caller(Uuid::new_v4(), org);
        let owner_elsewhere = caller(person, Uuid::new_v4());

        service
            .put_named_setting(&owner, "portal.projects.view", serde_json::json!("table"))
            .await
            .expect("stored");

        for other in [&colleague, &owner_elsewhere] {
            assert!(
                service
                    .list_named_settings(other)
                    .await
                    .expect("list")
                    .is_empty()
            );
            assert_eq!(
                service
                    .get_named_setting(other, "portal.projects.view")
                    .await
                    .expect("read"),
                None
            );
            assert!(
                !service
                    .delete_named_setting(other, "portal.projects.view")
                    .await
                    .expect("delete"),
                "cannot delete someone else's setting"
            );
        }

        assert!(
            service
                .get_named_setting(&owner, "portal.projects.view")
                .await
                .expect("read")
                .is_some(),
            "still there for the owner"
        );
    }

    /// Named settings live beside the fixed fields and leave them alone.
    #[tokio::test]
    async fn named_settings_do_not_touch_theme_and_language() {
        let service = named_service(ServiceConfig::default()).await;
        let ctx = create_test_context();
        service
            .update_settings(
                &ctx,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .expect("fixed fields");

        service
            .put_named_setting(&ctx, "theme", serde_json::json!("light"))
            .await
            .expect("a named key may share a fixed field's name");

        let fixed = service.get_settings(&ctx).await.expect("read");
        assert_eq!(fixed.theme.as_deref(), Some("dark"));
    }

    #[tokio::test]
    async fn under_a_tenant_wide_grant_named_settings_stay_the_callers_own() {
        let repo = Arc::new(SeaOrmSettingsRepository::new());
        let db: Arc<DBProvider<toolkit_db::DbError>> = Arc::new(DBProvider::new(inmem_db().await));
        let authz: Arc<dyn AuthZResolverApi> = Arc::new(TenantOnlyAuthZ);
        let service = Service::new(
            db,
            repo,
            PolicyEnforcer::new(authz),
            ServiceConfig {
                named_settings_per_user: 1,
                ..ServiceConfig::default()
            },
        );
        let org = Uuid::new_v4();
        let caller = |subject| {
            SecurityContext::builder()
                .subject_id(subject)
                .subject_tenant_id(org)
                .build()
                .unwrap()
        };
        let me = caller(Uuid::from_u128(1));
        let colleague = caller(Uuid::from_u128(2));

        service
            .put_named_setting(&me, "portal.projects.view", serde_json::json!("table"))
            .await
            .expect("stored");

        assert!(
            service
                .list_named_settings(&colleague)
                .await
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            service
                .get_named_setting(&colleague, "portal.projects.view")
                .await
                .expect("read"),
            None
        );
        assert!(
            !service
                .delete_named_setting(&colleague, "portal.projects.view")
                .await
                .expect("delete"),
            "the colleague's delete does not reach my row"
        );
        service
            .put_named_setting(&colleague, "other.key", serde_json::json!(1))
            .await
            .expect("my row does not count against the colleague's bound");

        assert!(
            service
                .get_named_setting(&me, "portal.projects.view")
                .await
                .expect("read")
                .is_some()
        );
    }

    /// The `ClientHub` surface other gears use: the registered
    /// `NamedSettingsClientV1` resolves to the local client and keeps the SDK's
    /// `Option` / `bool` / canonical-error semantics.
    #[tokio::test]
    async fn named_settings_work_through_the_client_hub() {
        use crate::domain::local_client::LocalClient;
        use simple_user_settings_sdk::NamedSettingsClientV1;
        use toolkit::ClientHub;

        let service = Arc::new(named_service(ServiceConfig::default()).await);
        let hub = ClientHub::new();
        let client: Arc<dyn NamedSettingsClientV1> = Arc::new(LocalClient::new(service));
        hub.register(client);
        let named = hub.get::<dyn NamedSettingsClientV1>().expect("registered");
        let ctx = create_test_context();

        assert_eq!(
            named.get_named_setting(&ctx, "a.key").await.expect("get"),
            None
        );
        let stored = named
            .put_named_setting(&ctx, "a.key", serde_json::json!({"x": 1}))
            .await
            .expect("put");
        assert_eq!(stored.value, serde_json::json!({"x": 1}));
        assert_eq!(
            named.list_named_settings(&ctx).await.expect("list"),
            vec![stored]
        );
        assert!(
            named
                .delete_named_setting(&ctx, "a.key")
                .await
                .expect("delete")
        );
        assert!(
            !named
                .delete_named_setting(&ctx, "a.key")
                .await
                .expect("again")
        );

        let err = named
            .put_named_setting(&ctx, "bad key", serde_json::json!(1))
            .await
            .expect_err("malformed key");
        assert!(
            matches!(err, CanonicalError::InvalidArgument { .. }),
            "got {err:?}"
        );
    }

    /// One unreadable row is skipped by the list, not fatal to it; a direct
    /// read of that key still reports it.
    #[tokio::test]
    async fn a_corrupt_named_setting_does_not_take_the_list_down() {
        use sea_orm::ConnectionTrait;
        use sea_orm_migration::MigratorTrait;

        // A named shared-cache database, so a second, raw handle reaches it:
        // the service's own connection is sealed behind the secure layer.
        let url = format!(
            "sqlite:file:named-{}?mode=memory&cache=shared",
            Uuid::new_v4().simple()
        );
        let raw = sea_orm::Database::connect(&url).await.expect("raw handle");
        let db = connect_db(&url, ConnectOpts::default()).await.expect("db");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        let service = build_service(db, ServiceConfig::default());
        let ctx = create_test_context();

        for key in ["a.fine", "b.broken", "c.fine"] {
            service
                .put_named_setting(&ctx, key, serde_json::json!(key))
                .await
                .expect("stored");
        }
        raw.execute_unprepared(
            "UPDATE named_settings SET value = 'not json' WHERE key = 'b.broken'",
        )
        .await
        .expect("corrupt one row");

        let keys: Vec<String> = service
            .list_named_settings(&ctx)
            .await
            .expect("the list survives")
            .into_iter()
            .map(|s| s.key)
            .collect();
        assert_eq!(keys, ["a.fine", "c.fine"]);

        assert!(
            matches!(
                service.get_named_setting(&ctx, "b.broken").await,
                Err(DomainError::Internal(_))
            ),
            "the corrupt key itself still reports the corruption"
        );
    }

    /// The REST surface end to end: the gear's own routes and handlers, driven
    /// in-process, with the status codes and bodies a client sees.
    #[tokio::test]
    async fn named_settings_over_http() {
        use axum::body::{Body, to_bytes};
        use axum::http::{Request, StatusCode};
        use axum::{Extension, Router};
        use toolkit::api::OpenApiRegistryImpl;
        use tower::ServiceExt;

        let service = Arc::new(
            named_service(ServiceConfig {
                named_settings_per_user: 1,
                ..ServiceConfig::default()
            })
            .await,
        );
        let openapi = OpenApiRegistryImpl::new();
        let app = crate::api::rest::routes::register_routes(Router::new(), &openapi, service)
            .layer(Extension(create_test_context()));

        let call = |method: &str, path: &str, body: Option<&str>| {
            let request = Request::builder()
                .method(method)
                .uri(format!("/simple-user-settings/v1/named-settings{path}"))
                .header("content-type", "application/json")
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
                .unwrap();
            let app = app.clone();
            async move {
                let response = app.oneshot(request).await.unwrap();
                let status = response.status();
                let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
                let json = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default();
                (status, json)
            }
        };

        let (status, body) = call("PUT", "/portal.view", Some(r#"{"value":"table"}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            serde_json::json!({"key": "portal.view", "value": "table"})
        );

        let (status, body) = call("GET", "/portal.view", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["value"], "table");

        // The body bound is 4 x the value bound (4096 by default) + 1 KiB: a
        // value at its bound still fits when sent pretty-printed, and a body
        // past the bound is refused before it is parsed.
        let padded = format!(
            "{{\n{}\"value\": \"{}\"\n}}",
            " ".repeat(8_000),
            "x".repeat(4_000)
        );
        let (status, _) = call("PUT", "/portal.view", Some(&padded)).await;
        assert_eq!(status, StatusCode::OK, "a padded value at its bound");
        let huge = format!("{{\"value\": \"{}\"}}", "x".repeat(4 * 4096 + 1024));
        let (status, _) = call("PUT", "/portal.view", Some(&huge)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        let (status, body) = call("GET", "", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["settings"].as_array().map(Vec::len), Some(1));

        let (status, _) = call("GET", "/never.set", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = call("PUT", "/bad%20key", Some(r#"{"value":1}"#)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = call("PUT", "/second.key", Some(r#"{"value":1}"#)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "bound of 1 reached");

        let (status, _) = call("DELETE", "/portal.view", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call("DELETE", "/portal.view", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "deleting again is fine");
        let (status, _) = call("GET", "/portal.view", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = call("PUT", "/again.one", Some(r#"{"value":true}"#)).await;
        assert_eq!(status, StatusCode::OK, "room again after the delete");
        let (status, _) = call("DELETE", "", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "delete-all");
        let (_, body) = call("GET", "", None).await;
        assert_eq!(body["settings"], serde_json::json!([]), "nothing left");
        let (status, _) = call("DELETE", "", None).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "delete-all with nothing set"
        );
    }

    /// The erasure path: one call removes every named setting of the caller,
    /// and only the caller's, leaving the fixed fields alone.
    #[tokio::test]
    async fn delete_all_named_settings_removes_only_the_callers_rows() {
        let service = named_service(ServiceConfig::default()).await;
        let org = Uuid::new_v4();
        let caller = |subject| {
            SecurityContext::builder()
                .subject_id(subject)
                .subject_tenant_id(org)
                .build()
                .unwrap()
        };
        let leaving = caller(Uuid::from_u128(1));
        let staying = caller(Uuid::from_u128(2));

        service
            .update_settings(
                &leaving,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .expect("fixed fields");
        for key in ["a", "b", "c"] {
            service
                .put_named_setting(&leaving, key, serde_json::json!(key))
                .await
                .expect("stored");
        }
        service
            .put_named_setting(&staying, "a", serde_json::json!("mine"))
            .await
            .expect("stored");

        assert_eq!(
            service
                .delete_all_named_settings(&leaving)
                .await
                .expect("purge"),
            3
        );
        assert!(
            service
                .list_named_settings(&leaving)
                .await
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            service
                .delete_all_named_settings(&leaving)
                .await
                .expect("again"),
            0,
            "nothing left is not an error"
        );
        assert_eq!(
            service
                .list_named_settings(&staying)
                .await
                .expect("list")
                .len(),
            1,
            "another user's settings are untouched"
        );
        assert_eq!(
            service
                .get_settings(&leaving)
                .await
                .expect("read")
                .theme
                .as_deref(),
            Some("dark"),
            "the fixed fields are not part of it"
        );
    }

    /// Both named bounds customised at once are each enforced, independently.
    #[tokio::test]
    async fn both_named_bounds_hold_together_when_both_are_customised() {
        let service = named_service(ServiceConfig {
            named_settings_per_user: 2,
            named_value_max_bytes: 8,
            ..ServiceConfig::default()
        })
        .await;
        let ctx = create_test_context();

        // `"123456"` is 8 bytes as JSON.
        service
            .put_named_setting(&ctx, "a", serde_json::json!("123456"))
            .await
            .expect("at the size bound");
        let err = service
            .put_named_setting(&ctx, "b", serde_json::json!("1234567"))
            .await
            .expect_err("over the size bound");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "value"),
            "{err:?}"
        );
        service
            .put_named_setting(&ctx, "b", serde_json::json!(1))
            .await
            .expect("second key, small value");
        let err = service
            .put_named_setting(&ctx, "c", serde_json::json!(1))
            .await
            .expect_err("over the count bound");
        assert!(matches!(&err, DomainError::LimitReached(_)), "{err:?}");
        let err = service
            .put_named_setting(&ctx, "c", serde_json::json!("far too large a value"))
            .await
            .expect_err("both bounds broken");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "value"),
            "the size check comes first, before anything is written: {err:?}"
        );
        assert_eq!(
            service.list_named_settings(&ctx).await.expect("list").len(),
            2
        );
    }

    /// A PDP that grants a fixed set of tenants at once, as a subtree grant
    /// does: the scope names several tenants and no user.
    struct TenantsAuthZ(Vec<Uuid>);

    #[async_trait]
    impl AuthZResolverApi for TenantsAuthZ {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            self.0.clone(),
                        ))],
                    }],
                    ..Default::default()
                },
            })
        }
    }

    /// With a scope covering two tenants, one user's two rows stay apart: the
    /// read and the patch merge use the row of the tenant the request is in.
    #[tokio::test]
    async fn a_multi_tenant_grant_still_reads_the_requested_tenants_row() {
        let (org_a, org_b) = (Uuid::from_u128(0xA), Uuid::from_u128(0xB));
        let service = build_service_with(
            inmem_db().await,
            ServiceConfig::default(),
            Arc::new(TenantsAuthZ(vec![org_a, org_b])),
        );
        let person = Uuid::from_u128(1);
        let in_a = in_tenant(person, org_a);
        let in_b = in_tenant(person, org_b);

        service
            .update_settings(
                &in_a,
                SimpleUserSettingsUpdate {
                    theme: "dark".to_owned(),
                    language: "en".to_owned(),
                },
            )
            .await
            .expect("stored in A");

        let seen_in_b = service.get_settings(&in_b).await.expect("read in B");
        assert_eq!(seen_in_b.tenant_id, org_b);
        assert_eq!(seen_in_b.theme, None, "A's row is not B's");

        let patched_in_b = service
            .patch_settings(
                &in_b,
                SimpleUserSettingsPatch {
                    theme: Some("light".to_owned()),
                    language: None,
                },
            )
            .await
            .expect("patched in B");
        assert_eq!(
            patched_in_b.language, None,
            "A's language did not merge into B"
        );

        let seen_in_a = service.get_settings(&in_a).await.expect("read in A");
        assert_eq!(seen_in_a.theme.as_deref(), Some("dark"));
    }
}
