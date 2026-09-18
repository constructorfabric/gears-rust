// Created: 2026-09-07 by Constructor Tech
//! `AccessRepository` over `tenant_permissions`.

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::access::{AccessRepository, Restriction, RestrictionDraft, TenantAccess};
use crate::domain::error::DomainError;
use crate::infra::storage::entity::tenant_permission::{self, Entity as PermissionEntity};

/// The repository. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct AccessRepo;

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

fn to_domain(model: tenant_permission::Model) -> Result<Restriction, DomainError> {
    let access = TenantAccess::parse(&model.access).ok_or_else(|| DomainError::Internal {
        diagnostic: format!(
            "restriction {} carries the unknown access `{}`",
            model.id, model.access
        ),
    })?;
    Ok(Restriction {
        id: model.id,
        declaration_id: model.declaration_id,
        tenant_id: model.tenant_id,
        access,
        set_by: model.set_by,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

#[async_trait]
impl AccessRepository for AccessRepo {
    async fn find_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<Restriction>, DomainError> {
        self.find_for_declarations(conn, scope, &[declaration_id], tenant_ids)
            .await
    }

    async fn find_for_declarations<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
    ) -> Result<Vec<Restriction>, DomainError> {
        if declaration_ids.is_empty() || tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-2
        let rows = PermissionEntity::find()
            .filter(tenant_permission::Column::DeclarationId.is_in(declaration_ids.iter().copied()))
            .filter(tenant_permission::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        // @cpt-end:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-2
        rows.into_iter().map(to_domain).collect()
    }

    async fn find_one<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Option<Restriction>, DomainError> {
        let row = PermissionEntity::find()
            .filter(tenant_permission::Column::DeclarationId.eq(declaration_id))
            .filter(tenant_permission::Column::TenantId.eq(tenant_id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_error)?;
        row.map(to_domain).transpose()
    }

    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: RestrictionDraft,
    ) -> Result<Restriction, DomainError> {
        let at = time::OffsetDateTime::now_utc();
        if let Some(existing) = self
            .find_one(conn, scope, draft.declaration_id, draft.tenant_id)
            .await?
        {
            PermissionEntity::update_many()
                .col_expr(
                    tenant_permission::Column::Access,
                    Expr::value(draft.access.as_str().to_owned()),
                )
                .col_expr(tenant_permission::Column::SetBy, Expr::value(draft.set_by))
                .col_expr(tenant_permission::Column::UpdatedAt, Expr::value(at))
                .filter(tenant_permission::Column::Id.eq(existing.id))
                .secure()
                .scope_with(scope)
                .exec(conn)
                .await
                .map_err(db_error)?;
            return self
                .find_one(conn, scope, draft.declaration_id, draft.tenant_id)
                .await?
                .ok_or_else(|| DomainError::Internal {
                    diagnostic: "restriction vanished inside its own transaction".to_owned(),
                });
        }
        let active = tenant_permission::ActiveModel {
            id: Set(Uuid::new_v4()),
            declaration_id: Set(draft.declaration_id),
            tenant_id: Set(draft.tenant_id),
            access: Set(draft.access.as_str().to_owned()),
            set_by: Set(draft.set_by),
            created_at: Set(at),
            updated_at: Set(at),
        };
        let model = toolkit_db::secure::secure_insert::<PermissionEntity>(active, scope, conn)
            .await
            .map_err(db_error)?;
        to_domain(model)
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        let outcome = PermissionEntity::delete_many()
            .filter(tenant_permission::Column::DeclarationId.eq(declaration_id))
            .filter(tenant_permission::Column::TenantId.eq(tenant_id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(outcome.rows_affected > 0)
    }
}

#[cfg(test)]
#[path = "access_repo_tests.rs"]
mod access_repo_tests;
