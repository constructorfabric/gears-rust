//! Subject settings service tests on an in-memory `SQLite` database, with a
//! stub for the tenant's defaults.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, PolicyEnforcer,
    models::{EvaluationRequest, EvaluationResponse},
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::{DBProvider, Db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::subject_settings::{
    SubjectKey, SubjectSettings, SubjectSettingsService, TenantDefaults,
};
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;
use crate::test_support::{
    AllowResolver, DenyResolver, context_in, context_of, inmem_db, seed_subject_settings,
};

type ConcreteService = SubjectSettingsService<SeaOrmSubjectSettingsRepository>;

/// The tenant's personalization default, the same for every tenant. Records
/// the tenants it was asked for.
struct StubDefaults {
    personalization_default: bool,
    asked_for: Mutex<Vec<Uuid>>,
}

impl StubDefaults {
    fn new(personalization_default: bool) -> Arc<Self> {
        Arc::new(Self {
            personalization_default,
            asked_for: Mutex::new(Vec::new()),
        })
    }

    fn asked_for(&self) -> Vec<Uuid> {
        self.asked_for.lock().unwrap().clone()
    }
}

#[async_trait]
impl TenantDefaults for StubDefaults {
    async fn personalization_default(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        self.asked_for.lock().unwrap().push(tenant_id);
        Ok(self.personalization_default)
    }
}

/// The settings service cannot be reached.
struct UnavailableDefaults;

#[async_trait]
impl TenantDefaults for UnavailableDefaults {
    async fn personalization_default(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        Err(DomainError::Unavailable("settings service down".to_owned()))
    }
}

/// One request to the policy service: resource type, action, resource id and
/// the tenant named as the owner of the resource.
type AskedOfPolicyService = (String, String, Option<Uuid>, Option<String>);

/// Records what the service asks the policy service for, then allows it like
/// `AllowResolver`.
#[derive(Default)]
struct RecordingResolver {
    asked: Mutex<Vec<AskedOfPolicyService>>,
}

#[async_trait]
impl AuthZResolverApi for RecordingResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.asked.lock().unwrap().push((
            request.resource.resource_type.clone(),
            request.action.name.clone(),
            request.resource.id,
            request
                .resource
                .properties
                .get(pep_properties::OWNER_TENANT_ID)
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        ));
        AllowResolver.evaluate(ctx, request).await
    }
}

fn service_with(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    defaults: Arc<dyn TenantDefaults>,
) -> ConcreteService {
    SubjectSettingsService::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmSubjectSettingsRepository::new()),
        defaults,
        PolicyEnforcer::new(resolver),
    )
}

const ERASING: SubjectSettings = SubjectSettings {
    personalization_enabled: false,
    erasure_in_progress: true,
};

#[tokio::test]
async fn subject_without_a_row_gets_the_tenant_default() {
    for personalization_default in [true, false] {
        let db = inmem_db().await;
        let defaults = StubDefaults::new(personalization_default);
        let svc = service_with(&db, Arc::new(AllowResolver), defaults.clone());
        let tenant = Uuid::new_v4();

        let settings = svc
            .settings(&context_in(tenant), Uuid::new_v4())
            .await
            .expect("settings");

        assert_eq!(
            settings,
            SubjectSettings {
                personalization_enabled: personalization_default,
                erasure_in_progress: false,
            }
        );
        assert_eq!(
            defaults.asked_for(),
            vec![tenant],
            "asked for the caller's tenant"
        );
    }
}

#[tokio::test]
async fn stored_row_wins_over_the_tenant_default() {
    let db = inmem_db().await;
    let defaults = StubDefaults::new(true);
    let svc = service_with(&db, Arc::new(AllowResolver), defaults.clone());
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let settings = svc
        .settings(&context_in(tenant), subject)
        .await
        .expect("settings");

    assert_eq!(settings, ERASING);
    assert!(
        defaults.asked_for().is_empty(),
        "no default read for a stored row"
    );
}

#[tokio::test]
async fn reading_the_default_stores_nothing() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(true));
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_in(tenant);
    svc.settings(&ctx, subject).await.expect("first read");

    let off = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(false));
    let settings = off.settings(&ctx, subject).await.expect("second read");

    assert_eq!(
        settings,
        SubjectSettings::new_subject(false),
        "a changed tenant default applies to a subject without a row"
    );
}

#[tokio::test]
async fn settings_of_another_tenant_are_not_visible() {
    let db = inmem_db().await;
    let defaults = StubDefaults::new(true);
    let svc = service_with(&db, Arc::new(AllowResolver), defaults.clone());
    let (own, other, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: other,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let settings = svc
        .settings(&context_in(own), subject)
        .await
        .expect("settings");

    assert_eq!(
        settings,
        SubjectSettings::new_subject(true),
        "the other tenant's row is not read; the subject is new in this tenant"
    );
    assert_eq!(defaults.asked_for(), vec![own]);
}

#[tokio::test]
async fn caller_without_permission_is_denied_before_any_read() {
    let db = inmem_db().await;
    let defaults = StubDefaults::new(true);
    let svc = service_with(&db, Arc::new(DenyResolver), defaults.clone());

    let denied = svc
        .settings(&context_in(Uuid::new_v4()), Uuid::new_v4())
        .await;

    assert!(
        matches!(denied, Err(DomainError::Forbidden(_))),
        "got {denied:?}"
    );
    assert!(defaults.asked_for().is_empty());
}

#[tokio::test]
async fn unavailable_tenant_default_is_an_unavailable_error() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), Arc::new(UnavailableDefaults));

    let failed = svc
        .settings(&context_in(Uuid::new_v4()), Uuid::new_v4())
        .await;

    assert!(
        matches!(failed, Err(DomainError::Unavailable(_))),
        "got {failed:?}"
    );
}

#[tokio::test]
async fn service_asks_for_the_subject_settings_resource_and_the_tenant() {
    let db = inmem_db().await;
    let resolver = Arc::new(RecordingResolver::default());
    let svc = service_with(&db, resolver.clone(), StubDefaults::new(false));
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    svc.settings(&context_in(tenant), subject)
        .await
        .expect("settings");

    assert_eq!(
        *resolver.asked.lock().unwrap(),
        vec![(
            "construct.subject_settings".to_owned(),
            "get".to_owned(),
            Some(subject),
            Some(tenant.to_string()),
        )]
    );
}

#[tokio::test]
async fn owner_reads_its_own_settings() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(false));
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let own = svc
        .owner_settings(&context_of(subject, tenant), subject)
        .await
        .expect("own settings");

    assert_eq!(own, ERASING);
}

#[tokio::test]
async fn owner_changes_personalization_and_the_next_read_sees_it() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(true));
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(subject, tenant);

    let stored = svc
        .set_personalization(&ctx, subject, false)
        .await
        .expect("first change creates the row");
    assert_eq!(stored, SubjectSettings::new_subject(false));
    assert_eq!(
        svc.owner_settings(&ctx, subject).await.expect("read"),
        stored
    );

    let stored = svc
        .set_personalization(&ctx, subject, true)
        .await
        .expect("second change updates the row");
    assert_eq!(stored, SubjectSettings::new_subject(true));
    assert_eq!(
        svc.owner_settings(&ctx, subject).await.expect("read"),
        stored
    );
}

#[tokio::test]
async fn turning_personalization_on_does_not_end_an_erasure() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(true));
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let stored = svc
        .set_personalization(&context_of(subject, tenant), subject, true)
        .await
        .expect("change");

    assert_eq!(
        stored,
        SubjectSettings {
            personalization_enabled: true,
            erasure_in_progress: true,
        }
    );
}

#[tokio::test]
async fn another_subject_is_refused_before_the_policy_service_is_asked() {
    let db = inmem_db().await;
    let resolver = Arc::new(RecordingResolver::default());
    let svc = service_with(&db, resolver.clone(), StubDefaults::new(true));
    let (tenant, owner) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: owner,
        },
        ERASING,
    )
    .await;
    let someone_else = context_of(Uuid::new_v4(), tenant);

    let read = svc.owner_settings(&someone_else, owner).await;
    let change = svc.set_personalization(&someone_else, owner, true).await;

    assert!(
        matches!(read, Err(DomainError::Forbidden(_))),
        "got {read:?}"
    );
    assert!(
        matches!(change, Err(DomainError::Forbidden(_))),
        "got {change:?}"
    );
    assert!(resolver.asked.lock().unwrap().is_empty());
    let unchanged = svc
        .owner_settings(&context_of(owner, tenant), owner)
        .await
        .expect("owner read");
    assert_eq!(unchanged, ERASING, "the refused change stored nothing");
}

#[tokio::test]
async fn the_owner_reads_only_its_row_of_its_own_tenant() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(true));
    let (own_tenant, other_tenant, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: other_tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let own = svc
        .owner_settings(&context_of(subject, own_tenant), subject)
        .await
        .expect("own settings");

    assert_eq!(
        own,
        SubjectSettings::new_subject(true),
        "the row of the other tenant is not read; the subject is new in its own tenant"
    );
}

#[tokio::test]
async fn a_change_in_one_tenant_leaves_the_same_subject_in_another_tenant_alone() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(true));
    let (first, second, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: second,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    svc.set_personalization(&context_of(subject, first), subject, false)
        .await
        .expect("change in the first tenant");

    let in_second = svc
        .owner_settings(&context_of(subject, second), subject)
        .await
        .expect("read in the second tenant");
    assert_eq!(in_second, ERASING);
}

#[tokio::test]
async fn owner_without_permission_is_denied_and_nothing_is_stored() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(subject, tenant);
    let denied = service_with(&db, Arc::new(DenyResolver), StubDefaults::new(true));

    let change = denied.set_personalization(&ctx, subject, false).await;
    let read = denied.owner_settings(&ctx, subject).await;

    assert!(
        matches!(change, Err(DomainError::Forbidden(_))),
        "got {change:?}"
    );
    assert!(
        matches!(read, Err(DomainError::Forbidden(_))),
        "got {read:?}"
    );
    let allowed = service_with(&db, Arc::new(AllowResolver), StubDefaults::new(true));
    assert_eq!(
        allowed.owner_settings(&ctx, subject).await.expect("read"),
        SubjectSettings::new_subject(true),
        "the denied change stored nothing"
    );
}

#[tokio::test]
async fn a_change_asks_for_the_update_action() {
    let db = inmem_db().await;
    let resolver = Arc::new(RecordingResolver::default());
    let svc = service_with(&db, resolver.clone(), StubDefaults::new(true));
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    svc.set_personalization(&context_of(subject, tenant), subject, false)
        .await
        .expect("change");

    let asked = resolver.asked.lock().unwrap().clone();
    assert_eq!(
        asked.first(),
        Some(&(
            "construct.subject_settings".to_owned(),
            "update".to_owned(),
            Some(subject),
            Some(tenant.to_string()),
        ))
    );
}
