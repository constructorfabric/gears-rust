// Created: 2026-09-06 by Constructor Tech
//! `ValueRepository` over `setting_values`.

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::value::{StoredValue, ValueDraft, ValueRepository};
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};

/// The repository. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct ValueRepo;

fn to_domain(model: setting_value::Model) -> StoredValue {
    StoredValue {
        id: model.id,
        declaration_id: model.declaration_id,
        tenant_id: model.tenant_id,
        value: model.value,
        secret_ref: model.secret_ref,
        data_classification: model.data_classification,
        needs_review: model.needs_review,
        needs_review_detail: model.needs_review_detail,
        last_change_at: model.last_change_at,
        updated_at: model.updated_at,
        set_by: model.set_by,
    }
}

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

fn map_write_error(err: &toolkit_db::secure::ScopeError) -> DomainError {
    if err.is_unique_violation() {
        DomainError::Conflict {
            detail: "a value already exists for this setting at this scope".to_owned(),
        }
    } else {
        db_error(err)
    }
}

/// Only the subject-less track: rows carrying a subject pair belong to the
/// subject dimension and never answer a request that named no subject.
fn subjectless() -> sea_orm::Condition {
    sea_orm::Condition::all().add(setting_value::Column::SubjectType.is_null())
}

#[async_trait]
impl ValueRepository for ValueRepo {
    async fn resync_classification<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        data_classification: &str,
    ) -> Result<u64, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-2
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-4
        // One statement over every row of the declaration. The table check
        // tying a `secret` classification to the presence of `secret_ref` is
        // the database's and is not restated here; a re-sync that would break
        // it fails at the constraint.
        let outcome = ValueEntity::update_many()
            .col_expr(
                setting_value::Column::DataClassification,
                Expr::value(data_classification.to_owned()),
            )
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(outcome.rows_affected)
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-4
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-2
    }

    async fn find_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<StoredValue>, DomainError> {
        if tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        // `tenant_id IN (...)`: an exact-match set over ids, never a prefix or
        // pattern scan over a stored path.
        let rows = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(to_domain).collect())
    }

    async fn find_one<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Option<StoredValue>, DomainError> {
        let row = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.eq(tenant_id))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(row.map(to_domain))
    }

    async fn list_flagged<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
    ) -> Result<Vec<StoredValue>, DomainError> {
        if declaration_ids.is_empty() || tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = ValueEntity::find()
            .filter(setting_value::Column::NeedsReview.eq(true))
            .filter(setting_value::Column::DeclarationId.is_in(declaration_ids.iter().copied()))
            .filter(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(to_domain).collect())
    }

    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: ValueDraft,
    ) -> Result<StoredValue, DomainError> {
        let at = time::OffsetDateTime::now_utc();
        let active = setting_value::ActiveModel {
            id: Set(Uuid::new_v4()),
            declaration_id: Set(draft.declaration_id),
            tenant_id: Set(draft.tenant_id),
            subject_type: Set(None),
            subject_id: Set(None),
            value: Set(draft.value),
            secret_ref: Set(draft.secret_ref),
            data_classification: Set(draft.data_classification),
            needs_review: Set(draft.needs_review),
            needs_review_detail: Set(draft.needs_review_detail),
            last_change_at: Set(at),
            created_at: Set(at),
            updated_at: Set(at),
            set_by: Set(draft.set_by),
        };
        let model = toolkit_db::secure::secure_insert::<ValueEntity>(active, scope, conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        Ok(to_domain(model))
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        value: Option<serde_json::Value>,
        secret_ref: Option<String>,
        set_by: &str,
    ) -> Result<StoredValue, DomainError> {
        let at = time::OffsetDateTime::now_utc();
        let outcome = ValueEntity::update_many()
            .col_expr(setting_value::Column::Value, Expr::value(value))
            .col_expr(setting_value::Column::SecretRef, Expr::value(secret_ref))
            .col_expr(setting_value::Column::NeedsReview, Expr::value(false))
            .col_expr(
                setting_value::Column::NeedsReviewDetail,
                Expr::value(Option::<String>::None),
            )
            .col_expr(setting_value::Column::LastChangeAt, Expr::value(at))
            .col_expr(setting_value::Column::UpdatedAt, Expr::value(at))
            .col_expr(setting_value::Column::SetBy, Expr::value(set_by.to_owned()))
            .filter(setting_value::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        if outcome.rows_affected == 0 {
            return Err(DomainError::NotFound { resource: "value" });
        }
        let row = ValueEntity::find()
            .filter(setting_value::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_error)?
            .ok_or(DomainError::NotFound { resource: "value" })?;
        Ok(to_domain(row))
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        let outcome = ValueEntity::delete_many()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.eq(tenant_id))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(outcome.rows_affected > 0)
    }
}
