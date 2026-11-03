//! `ValueRepository` over `setting_values`.

use std::collections::HashSet;

use async_trait::async_trait;
use sea_orm::ExprTrait as _;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QuerySelect, QueryTrait};
use settings_service_sdk::odata::DeclarationFilterField;
use toolkit_db::odata::{LimitCfg, filter_node_to_condition, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_odata::filter::convert_expr_to_filter_node;
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::odata::ReviewOrderField;
use crate::domain::precondition;
use crate::domain::value::{
    DistinctValue, ListedValue, ReviewRequest, StoredValue, ValueDraft, ValueRepository,
};
use crate::infra::storage::clock::{now, stamp_after};
use crate::infra::storage::declaration_odata_mapper::DeclarationODataMapper;
use crate::infra::storage::declaration_repo;
use crate::infra::storage::entity::declaration::{self, Entity as DeclarationEntity};
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};
use crate::infra::storage::odata_mapper::ReviewODataMapper;

/// Page bounds of the review listing: the browse's, which it is the row-level
/// companion of.
const REVIEW_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// How many references one `IN` list carries.
const REFERENCE_CHUNK: usize = 500;

/// The repository. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct ValueRepo;

/// One group of the distinct-values read.
#[derive(Debug, sea_orm::FromQueryResult)]
struct DistinctRow {
    value: Option<serde_json::Value>,
    data_classification: String,
    n: i64,
}

/// One row of the listing projection.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ListedRow {
    tenant_id: Uuid,
    value: Option<serde_json::Value>,
    data_classification: String,
}

/// Converts a stored setting value row to the domain type.
pub(crate) fn to_domain(model: setting_value::Model) -> StoredValue {
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
        set_by_tenant: model.set_by_tenant,
    }
}

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

/// Project an insert failure.
///
/// The unique index on the pair guards the first row: the caller compared the
/// absent-state tag, so a row that appeared between that comparison and this
/// insert is the other writer's, and this write is the stale one.
fn map_write_error(err: &toolkit_db::secure::ScopeError) -> DomainError {
    if err.is_unique_violation() {
        precondition::stale()
    } else {
        db_error(err)
    }
}

/// Only the subject-less track: rows carrying a subject pair belong to the
/// subject dimension and never answer a request that named no subject.
pub(crate) fn subjectless() -> sea_orm::Condition {
    sea_orm::Condition::all().add(setting_value::Column::SubjectType.is_null())
}

/// The ids of the declarations whose own subject-less override at `tenant` is
/// flagged for review, as a subquery — served by `idx_values_needs_review`.
///
/// One predicate for every surface that narrows to "flagged at the target"
/// (the browse and the search), so the two cannot read the flag differently.
pub(crate) fn flagged_declarations_at(tenant: Uuid) -> sea_orm::sea_query::SelectStatement {
    ValueEntity::find()
        .select_only()
        .column(setting_value::Column::DeclarationId)
        .filter(setting_value::Column::NeedsReview.eq(true))
        .filter(setting_value::Column::TenantId.eq(tenant))
        .filter(subjectless())
        .into_query()
}

#[async_trait]
impl ValueRepository for ValueRepo {
    // @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-classification:p1
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

    async fn count_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<u64, DomainError> {
        if tenant_ids.is_empty() {
            return Ok(0);
        }
        // `SELECT COUNT(*)` over the predicate of `find_in_tenants`.
        ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .count(conn)
            .await
            .map_err(db_error)
    }

    async fn distinct_values_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<DistinctValue>, DomainError> {
        if tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        // `SELECT value, data_classification, COUNT(id) ... GROUP BY value,
        // data_classification` over the predicate of `find_in_tenants`: the
        // distinct pairs and their multiplicity, nothing else of a row. The
        // secret rows (`value IS NULL`) group together per classification.
        let groups = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .project_all(conn, |q| {
                q.select_only()
                    .column(setting_value::Column::Value)
                    .column(setting_value::Column::DataClassification)
                    .column_as(Expr::col(setting_value::Column::Id).count(), "n")
                    .group_by(setting_value::Column::Value)
                    .group_by(setting_value::Column::DataClassification)
                    .into_model::<DistinctRow>()
            })
            .await
            .map_err(db_error)?;
        Ok(groups
            .into_iter()
            .map(|group| DistinctValue {
                value: group.value,
                data_classification: group.data_classification,
                count: u64::try_from(group.n).unwrap_or_default(),
            })
            .collect())
    }

    async fn list_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<ListedValue>, DomainError> {
        if tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        // The predicate of `find_in_tenants`, three columns of it.
        let rows = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .project_all(conn, |q| {
                q.select_only()
                    .column(setting_value::Column::TenantId)
                    .column(setting_value::Column::Value)
                    .column(setting_value::Column::DataClassification)
                    .into_model::<ListedRow>()
            })
            .await
            .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|row| ListedValue {
                tenant_id: row.tenant_id,
                value: row.value,
                data_classification: row.data_classification,
            })
            .collect())
    }

    async fn find_many_in_tenants<C: DBRunner>(
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

    async fn find_all<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
    ) -> Result<Vec<StoredValue>, DomainError> {
        let rows = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(subjectless())
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(to_domain).collect())
    }

    async fn lock_all<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
    ) -> Result<Vec<StoredValue>, DomainError> {
        let rows = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(subjectless())
            .lock_exclusive()
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(to_domain).collect())
    }

    async fn secret_refs<C: DBRunner>(
        &self,
        conn: &C,
        declaration_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DomainError> {
        // Unscoped and not narrowed to the subject-less track, for the reason
        // `count_referencing_declarations` gives: the cascade removes every row
        // whatever the caller's scope, so the collect must see them all.
        let rows = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::SecretRef.is_not_null())
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.secret_ref.map(|secret_ref| (row.tenant_id, secret_ref)))
            .collect())
    }

    async fn references_held_by_others<C: DBRunner>(
        &self,
        conn: &C,
        declaration_id: Uuid,
        refs: &[String],
    ) -> Result<HashSet<String>, DomainError> {
        let mut held = HashSet::new();
        for chunk in refs.chunks(REFERENCE_CHUNK) {
            let rows = ValueEntity::find()
                .filter(setting_value::Column::DeclarationId.ne(declaration_id))
                .filter(setting_value::Column::SecretRef.is_in(chunk.iter().cloned()))
                .secure()
                .scope_with(&AccessScope::allow_all())
                .all(conn)
                .await
                .map_err(db_error)?;
            held.extend(rows.into_iter().filter_map(|row| row.secret_ref));
        }
        Ok(held)
    }

    async fn flag<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        detail: Option<String>,
    ) -> Result<(), DomainError> {
        let outcome = ValueEntity::update_many()
            .col_expr(
                setting_value::Column::NeedsReview,
                Expr::value(detail.is_some()),
            )
            .col_expr(
                setting_value::Column::NeedsReviewDetail,
                Expr::value(detail),
            )
            .col_expr(setting_value::Column::UpdatedAt, Expr::value(now()))
            .filter(setting_value::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        // A row that is gone, or outside the scope, matched nothing; the caller
        // is told so rather than left believing the flag was applied.
        if outcome.rows_affected == 0 {
            return Err(DomainError::NotFound { resource: "value" });
        }
        Ok(())
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

    async fn count_flagged<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        source: &str,
    ) -> Result<u64, DomainError> {
        use crate::infra::storage::entity::declaration::{self, Entity as DeclarationEntity};
        let of_source = sea_orm::sea_query::Query::select()
            .column(declaration::Column::Id)
            .from(DeclarationEntity)
            .and_where(declaration::Column::Source.eq(source))
            .to_owned();
        ValueEntity::find()
            .filter(setting_value::Column::NeedsReview.eq(true))
            .filter(subjectless())
            .filter(setting_value::Column::DeclarationId.in_subquery(of_source))
            .secure()
            .scope_with(scope)
            .count(conn)
            .await
            .map_err(db_error)
    }

    async fn list_review<C: DBRunner>(
        &self,
        conn: &C,
        request: &ReviewRequest<'_>,
    ) -> Result<Page<StoredValue>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-6
        // The declarations a row may belong to, as a subquery: visible to the
        // caller, not hidden on its chain, within the narrowed grant and the
        // filter's selectors — all in the query, so the page is cut and counted
        // after them and comes back full.
        let mut declarations = declaration_repo::exclude_hidden_for(
            declaration_repo::apply_visibility(DeclarationEntity::find(), request.visibility),
            request.hidden_for,
        );
        if let Some(expr) = request.declarations {
            let invalid = |message: String| DomainError::Validation {
                field: "$filter".to_owned(),
                code: crate::field::ODATA_QUERY,
                message,
            };
            let node = convert_expr_to_filter_node::<DeclarationFilterField>(expr)
                .map_err(|e| invalid(e.to_string()))?;
            declarations = declarations.filter(
                filter_node_to_condition::<DeclarationFilterField, DeclarationODataMapper>(&node)
                    .map_err(invalid)?,
            );
        }
        let permitted = declarations
            .secure()
            .scope_with(request.scope)
            .into_inner()
            .select_only()
            .column(declaration::Column::Id)
            .into_query();

        // The subtree is the filter on the rows, as it is for every read over
        // the target's descendants: the caller's grant narrows the settings, and
        // the tenants were gated and walked before this.
        let base = ValueEntity::find()
            .filter(setting_value::Column::NeedsReview.eq(true))
            .filter(subjectless())
            .filter(setting_value::Column::TenantId.is_in(request.tenant_ids.iter().copied()))
            .filter(setting_value::Column::DeclarationId.in_subquery(permitted))
            .secure()
            .scope_with(&AccessScope::allow_all());

        // A fixed, total order: declaration, then tenant — `uq_value_scope`
        // makes the pair unique for subject-less rows — so a page boundary
        // inside one setting's rows neither repeats nor skips one. The caller's
        // own `$filter` and order are not read; the binding the cursor carries
        // is.
        let query = ODataQuery {
            filter: None,
            order: ODataOrderBy(vec![OrderKey {
                field: "declaration_id".to_owned(),
                dir: SortDir::Asc,
            }]),
            limit: request.query.limit,
            cursor: request.query.cursor.clone(),
            filter_hash: request.query.filter_hash.clone(),
            select: None,
        };
        let page = paginate_odata::<ReviewOrderField, ReviewODataMapper, _, _, _, _>(
            base,
            conn,
            &query,
            ("tenant_id", SortDir::Asc),
            REVIEW_LIMIT_CFG,
            to_domain,
        )
        .await
        .map_err(|err| match err {
            // The database failing is not the caller's cursor.
            toolkit_odata::Error::Db(_) | toolkit_odata::Error::ParsingUnavailable(_) => {
                db_error(err)
            }
            toolkit_odata::Error::InvalidFilter(_)
            | toolkit_odata::Error::InvalidOrderByField(_)
            | toolkit_odata::Error::OrderMismatch
            | toolkit_odata::Error::FilterMismatch
            | toolkit_odata::Error::InvalidCursor
            | toolkit_odata::Error::InvalidLimit
            | toolkit_odata::Error::OrderWithCursor
            | toolkit_odata::Error::CursorInvalidBase64
            | toolkit_odata::Error::CursorInvalidJson
            | toolkit_odata::Error::CursorInvalidVersion
            | toolkit_odata::Error::CursorInvalidKeys
            | toolkit_odata::Error::CursorInvalidFields
            | toolkit_odata::Error::CursorInvalidDirection => DomainError::Validation {
                field: "cursor".to_owned(),
                code: crate::field::ODATA_QUERY,
                message: err.to_string(),
            },
        })?;
        Ok(page)
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-6
    }

    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: ValueDraft,
    ) -> Result<StoredValue, DomainError> {
        let at = now();
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
            set_by_tenant: Set(draft.set_by_tenant),
        };
        let model = toolkit_db::secure::secure_insert::<ValueEntity>(active, scope, conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        Ok(to_domain(model))
    }

    #[allow(clippy::too_many_arguments)]
    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        value: Option<serde_json::Value>,
        secret_ref: Option<String>,
        set_by: &str,
        set_by_tenant: Option<Uuid>,
        expected: time::OffsetDateTime,
    ) -> Result<StoredValue, DomainError> {
        // Strictly after the version being replaced: the tag moves with the
        // row even when two writes share a microsecond.
        let at = stamp_after(Some(expected));
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
            .col_expr(
                setting_value::Column::SetByTenant,
                Expr::value(set_by_tenant),
            )
            .filter(setting_value::Column::Id.eq(id))
            // The row at the version the tag was compared against, and no
            // other: two writers holding one tag both pass the comparison, and
            // this is what keeps the second from landing on top of the first.
            .filter(setting_value::Column::LastChangeAt.eq(expected))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        if outcome.rows_affected == 0 {
            return Err(precondition::stale());
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
        expected: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        let outcome = ValueEntity::delete_many()
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .filter(setting_value::Column::TenantId.eq(tenant_id))
            .filter(subjectless())
            .filter(setting_value::Column::LastChangeAt.eq(expected))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        if outcome.rows_affected == 0 {
            return Err(precondition::stale());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "value_repo_tests.rs"]
mod value_repo_tests;
