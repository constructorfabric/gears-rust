//! Test helpers shared by the service and repository tests: an in-memory
//! database with the gear's migrations, a security context, and stub policy
//! services.

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::ScopeError;
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::subject_settings::SubjectSettings;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::subject_settings_entity;

/// Allows every request and constrains the answer to the subject's tenant and,
/// when the request names one, the resource id (like a real PDP).
pub struct AllowResolver;

#[async_trait]
impl AuthZResolverApi for AllowResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
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

/// Denies every request: a subject without permission.
pub struct DenyResolver;

#[async_trait]
impl AuthZResolverApi for DenyResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

pub async fn inmem_db() -> Db {
    use sea_orm_migration::MigratorTrait;

    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory database");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");
    db
}

pub fn context_in(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Stores one subject's settings row in `tenant_id`, through the secure ORM
/// with a scope for that tenant. `None` for the erasure flag leaves the column
/// out of the insert, so the table default applies. The insert's error is
/// returned, not unwrapped, so a test can expect a refusal.
pub async fn insert_subject_settings(
    db: &Db,
    tenant_id: Uuid,
    subject_id: Uuid,
    personalization_enabled: bool,
    erasure_in_progress: Option<bool>,
) -> Result<(), ScopeError> {
    use sea_orm::{ActiveValue, EntityTrait};
    use toolkit_db::secure::SecureInsertExt;

    let row = subject_settings_entity::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        subject_id: ActiveValue::Set(subject_id),
        personalization_enabled: ActiveValue::Set(personalization_enabled),
        erasure_in_progress: erasure_in_progress.map_or(ActiveValue::NotSet, ActiveValue::Set),
    };
    let conn = db.conn().expect("connection");
    subject_settings_entity::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(tenant_id), &row)?
        .exec(&conn)
        .await?;
    Ok(())
}

/// Stores one subject's settings, both columns set.
pub async fn seed_subject_settings(
    db: &Db,
    tenant_id: Uuid,
    subject_id: Uuid,
    settings: SubjectSettings,
) {
    insert_subject_settings(
        db,
        tenant_id,
        subject_id,
        settings.personalization_enabled,
        Some(settings.erasure_in_progress),
    )
    .await
    .expect("insert subject settings");
}
