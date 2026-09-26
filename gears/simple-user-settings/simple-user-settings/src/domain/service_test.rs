//! Integration tests for the Settings service.
//!
//! These tests use an in-memory `SQLite` database since `DBRunner` is a sealed trait
//! and cannot be mocked. All tests use real database operations.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use authz_resolver_sdk::{
        AuthZResolverApi, PolicyEnforcer,
        constraints::{Constraint, InPredicate, Predicate},
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use simple_user_settings_sdk::SettingsOwnerResolver;
    use simple_user_settings_sdk::models::{SimpleUserSettingsPatch, SimpleUserSettingsUpdate};
    use toolkit::ClientHub;
    use toolkit::api::canonical_prelude::CanonicalError;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
    use uuid::Uuid;

    use crate::domain::error::DomainError;
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

    /// A caller with a named subject in a named organization.
    ///
    /// Settings are keyed on `(user, tenant)`, so a test about *whose* settings
    /// these are names both halves instead of leaving either to chance.
    fn caller(subject_id: Uuid, tenant_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(subject_id)
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap()
    }

    fn build_service(db: Db, config: ServiceConfig) -> ConcreteService {
        build_service_on(db, config, Arc::new(ClientHub::new()))
    }

    /// A service that reads its owner resolver from `hub`, as the gear wires it.
    fn build_service_on(db: Db, config: ServiceConfig, hub: Arc<ClientHub>) -> ConcreteService {
        let repo = Arc::new(SeaOrmSettingsRepository::new());
        let db: Arc<DBProvider<toolkit_db::DbError>> = Arc::new(DBProvider::new(db));
        let authz: Arc<dyn AuthZResolverApi> = Arc::new(MockAuthZResolver);
        let policy_enforcer = PolicyEnforcer::new(authz);
        Service::new(db, repo, policy_enforcer, config, hub)
    }

    /// A hub on which the deployment has published `resolver`.
    fn published(resolver: Arc<dyn SettingsOwnerResolver>) -> Arc<ClientHub> {
        let hub = Arc::new(ClientHub::new());
        hub.register(resolver);
        hub
    }

    /// A service whose deployment has published `resolver`.
    async fn resolving_with(resolver: Arc<dyn SettingsOwnerResolver>) -> ConcreteService {
        build_service_on(
            inmem_db().await,
            ServiceConfig::default(),
            published(resolver),
        )
    }

    /// A deployment whose people hold more than one login.
    ///
    /// Answers with one fixed key for every caller, which is the shape that
    /// matters: two different subjects must reach the same settings. Records
    /// who it was asked about, so a test can tell it saw the real caller.
    struct OnePerson {
        owner: Option<Uuid>,
        asked: Mutex<Vec<(Uuid, Uuid)>>,
    }

    impl OnePerson {
        fn new(owner: Option<Uuid>) -> Arc<Self> {
            Arc::new(Self {
                owner,
                asked: Mutex::new(Vec::new()),
            })
        }

        /// `(subject, tenant)` of every caller it was asked about, in order.
        fn asked(&self) -> Vec<(Uuid, Uuid)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SettingsOwnerResolver for OnePerson {
        async fn settings_owner(
            &self,
            ctx: &SecurityContext,
        ) -> Result<Option<Uuid>, CanonicalError> {
            self.asked
                .lock()
                .unwrap()
                .push((ctx.subject_id(), ctx.subject_tenant_id()));
            Ok(self.owner)
        }
    }

    /// A resolver that is having a bad day, in a given way.
    struct Failing(fn() -> CanonicalError);

    #[async_trait]
    impl SettingsOwnerResolver for Failing {
        async fn settings_owner(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Option<Uuid>, CanonicalError> {
            Err((self.0)())
        }
    }

    /// A resolver whose directory never answers.
    struct Hangs;

    #[async_trait]
    impl SettingsOwnerResolver for Hangs {
        async fn settings_owner(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Option<Uuid>, CanonicalError> {
            std::future::pending().await
        }
    }

    fn update(theme: &str, language: &str) -> SimpleUserSettingsUpdate {
        SimpleUserSettingsUpdate {
            theme: theme.to_owned(),
            language: language.to_owned(),
        }
    }

    // =========================================================================
    // whose settings these are
    // =========================================================================

    /// The point of the resolver: one human, two logins, one set of settings.
    #[tokio::test]
    async fn two_subjects_resolving_to_one_person_share_their_settings() {
        let person = Uuid::new_v4();
        let resolver = OnePerson::new(Some(person));
        let service = resolving_with(resolver.clone()).await;

        let org = Uuid::new_v4();
        let email_login = Uuid::from_u128(1);
        let github_login = Uuid::from_u128(2);

        service
            .update_settings(&caller(email_login, org), update("dark", "en"))
            .await
            .expect("stored under the person");

        let seen = service
            .get_settings(&caller(github_login, org))
            .await
            .expect("read under the person");
        assert_eq!(seen.theme.as_deref(), Some("dark"));
        assert_eq!(seen.user_id, person, "settings belong to the person");

        assert_eq!(
            resolver.asked(),
            vec![(email_login, org), (github_login, org)],
            "the resolver is asked about the caller of each request"
        );
    }

    /// `patch` resolves the key like `get` and `update` do, so two logins
    /// patching different fields build up one row.
    #[tokio::test]
    async fn patches_from_two_logins_land_on_one_row() {
        let person = Uuid::new_v4();
        let service = resolving_with(OnePerson::new(Some(person))).await;
        let org = Uuid::new_v4();
        let first_login = caller(Uuid::from_u128(1), org);
        let second_login = caller(Uuid::from_u128(2), org);

        service
            .patch_settings(
                &first_login,
                SimpleUserSettingsPatch {
                    theme: Some("dark".to_owned()),
                    language: None,
                },
            )
            .await
            .expect("first patch");
        let patched = service
            .patch_settings(
                &second_login,
                SimpleUserSettingsPatch {
                    theme: None,
                    language: Some("fr".to_owned()),
                },
            )
            .await
            .expect("second patch");

        assert_eq!(patched.user_id, person);
        assert_eq!(
            patched.theme.as_deref(),
            Some("dark"),
            "kept the first patch"
        );
        assert_eq!(patched.language.as_deref(), Some("fr"));

        let seen = service.get_settings(&first_login).await.expect("read");
        assert_eq!(seen, patched);
    }

    /// The resolver answers the user half of the key only; the tenant still
    /// separates one person's settings in two organizations.
    #[tokio::test]
    async fn one_person_in_two_tenants_keeps_two_sets() {
        let person = Uuid::new_v4();
        let service = resolving_with(OnePerson::new(Some(person))).await;
        let in_org_a = caller(Uuid::from_u128(1), Uuid::from_u128(0xA));
        let in_org_b = caller(Uuid::from_u128(2), Uuid::from_u128(0xB));

        service
            .update_settings(&in_org_a, update("dark", "en"))
            .await
            .expect("stored in A");

        let seen_in_b = service.get_settings(&in_org_b).await.expect("read in B");
        assert_eq!(seen_in_b.user_id, person);
        assert_eq!(seen_in_b.tenant_id, in_org_b.subject_tenant_id());
        assert_eq!(seen_in_b.theme, None, "A's settings do not leak into B");

        service
            .update_settings(&in_org_b, update("light", "de"))
            .await
            .expect("stored in B");
        let seen_in_a = service.get_settings(&in_org_a).await.expect("read in A");
        assert_eq!(
            seen_in_a.theme.as_deref(),
            Some("dark"),
            "B did not overwrite A"
        );
    }

    /// Without a resolver the gear behaves exactly as it always has, which is
    /// what makes this addition safe for every deployment that has one login
    /// per human.
    #[tokio::test]
    async fn with_no_resolver_the_subject_is_still_the_key() {
        let service = build_service(inmem_db().await, ServiceConfig::default());

        let ctx = create_test_context();
        let stored = service
            .update_settings(&ctx, update("light", "en"))
            .await
            .expect("stored");
        assert_eq!(stored.user_id, ctx.subject_id());
    }

    /// The service reads the hub per request, so a resolver published after it
    /// was built — by a gear that initializes later — is not missed.
    #[tokio::test]
    async fn a_resolver_registered_after_the_service_was_built_is_used() {
        let hub = Arc::new(ClientHub::new());
        let service = build_service_on(inmem_db().await, ServiceConfig::default(), hub.clone());
        let ctx = caller(Uuid::from_u128(1), Uuid::new_v4());

        let before = service.get_settings(&ctx).await.expect("read before");
        assert_eq!(before.user_id, ctx.subject_id());

        let person = Uuid::new_v4();
        let resolver: Arc<dyn SettingsOwnerResolver> = OnePerson::new(Some(person));
        hub.register(resolver);

        let after = service.get_settings(&ctx).await.expect("read after");
        assert_eq!(after.user_id, person);
    }

    /// A resolver with no opinion about this caller yields to the subject
    /// rather than denying somebody their preferences.
    #[tokio::test]
    async fn a_resolver_that_declines_falls_back_to_the_subject() {
        let service = resolving_with(OnePerson::new(None)).await;

        let ctx = create_test_context();
        let stored = service
            .update_settings(&ctx, update("light", "en"))
            .await
            .expect("stored");
        assert_eq!(stored.user_id, ctx.subject_id());
    }

    // =========================================================================
    // when the resolver cannot answer
    // =========================================================================

    /// A resolver that fails is reported, not guessed around: filing settings
    /// under a key the next request will not produce loses them silently.
    #[tokio::test]
    async fn a_resolver_that_fails_fails_the_request() {
        let service = resolving_with(Arc::new(Failing(|| {
            CanonicalError::internal("directory unavailable").create()
        })))
        .await;
        let ctx = create_test_context();

        let err = service
            .get_settings(&ctx)
            .await
            .expect_err("the read must not fall back");
        let DomainError::Internal(message) = &err else {
            panic!("expected Internal, got {err:?}");
        };
        assert!(
            message.contains("directory unavailable"),
            "the resolver's cause survives for the log: {message}"
        );

        let err = service
            .update_settings(&ctx, update("dark", "en"))
            .await
            .expect_err("the write must not fall back either");
        assert!(matches!(err, DomainError::Internal(_)), "got {err:?}");
    }

    /// A resolver that is down says so as "unavailable", which a caller can
    /// retry, not as a bug in this gear.
    #[tokio::test]
    async fn an_unavailable_resolver_makes_the_request_unavailable() {
        let service = resolving_with(Arc::new(Failing(|| {
            CanonicalError::service_unavailable().create()
        })))
        .await;

        let err = service
            .get_settings(&create_test_context())
            .await
            .expect_err("must fail");
        assert!(matches!(err, DomainError::Unavailable(_)), "got {err:?}");
    }

    /// A resolver that refuses the caller is a denial, answered like any other.
    #[tokio::test]
    async fn a_resolver_that_refuses_the_caller_denies_the_request() {
        let service = resolving_with(Arc::new(Failing(|| {
            CanonicalError::unauthenticated()
                .with_reason("login not linked")
                .create()
        })))
        .await;

        let err = service
            .get_settings(&create_test_context())
            .await
            .expect_err("must fail");
        assert!(matches!(err, DomainError::Forbidden(_)), "got {err:?}");
    }

    /// A resolver that never answers is cut off by the configured bound
    /// instead of holding the request open.
    #[tokio::test]
    async fn a_resolver_that_hangs_is_cut_off() {
        let service = build_service_on(
            inmem_db().await,
            ServiceConfig {
                owner_resolver_timeout: Duration::from_millis(50),
                ..ServiceConfig::default()
            },
            published(Arc::new(Hangs)),
        );

        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            service.get_settings(&create_test_context()),
        )
        .await
        .expect("the service must give up on its own");
        let err = outcome.expect_err("must fail");
        assert!(matches!(err, DomainError::Unavailable(_)), "got {err:?}");
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
}
