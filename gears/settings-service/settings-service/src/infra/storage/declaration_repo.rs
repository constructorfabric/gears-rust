// Created: 2026-08-26 by Constructor Tech
//! Persistence for declarations.

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, ExprTrait, QueryFilter};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::{
    Declaration, DeclarationDraft, DeclarationMetadata, DeclarationRepository,
};
use crate::domain::error::DomainError;
use crate::infra::storage::declaration_odata_mapper::DeclarationODataMapper;
use crate::infra::storage::entity::declaration::{self, Entity as DeclarationEntity};
use settings_service_sdk::odata::DeclarationFilterField;

/// Page bounds for declaration listings.
///
/// Larger than categories: a category holds tens of declarations and an
/// administrator browsing one expects to see it whole, where the category list
/// itself is short by nature.
const DECLARATION_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// Persistence for declarations.
pub struct DeclarationRepo;

fn to_domain(model: declaration::Model) -> Declaration {
    Declaration {
        id: model.id,
        key: model.key,
        leaf_slug: model.leaf_slug,
        value_type_id: model.value_type_id,
        category_id: model.category_id,
        scope_class: model.scope_class,
        mode: model.mode,
        status: model.status,
        domain_affinity: model.domain_affinity,
        licence_feature: model.licence_feature,
        owner_module: model.owner_module,
        description: model.description,
        default_value: model.default_value,
        has_secret_trait: model.has_secret_trait,
        data_classification: model.data_classification,
        requires_step_up: model.requires_step_up,
        anonymous_exposable: model.anonymous_exposable,
        source: model.source,
        last_change_at: model.last_change_at,
        updated_at: model.updated_at,
    }
}

fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

fn map_write_error(err: &toolkit_db::secure::ScopeError) -> DomainError {
    if err.is_unique_violation() {
        DomainError::Conflict {
            detail:
                "a declaration with this key, or this leaf name in this category, already exists"
                    .to_owned(),
        }
    } else {
        DomainError::Internal {
            diagnostic: err.to_string(),
        }
    }
}

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

/// The domain-affinity arm of the visibility rule, as a query predicate.
///
/// The null arm is what keeps an undomained declaration universally visible;
/// without it every declaration with no domain vanishes for every scoped
/// administrator.
fn apply_visibility(
    select: sea_orm::Select<DeclarationEntity>,
    visibility: &DomainVisibility,
) -> sea_orm::Select<DeclarationEntity> {
    match visibility {
        DomainVisibility::Unrestricted => select,
        DomainVisibility::Restricted(domains) => select.filter(
            declaration::Column::DomainAffinity
                .is_null()
                .or(declaration::Column::DomainAffinity.is_in(domains.clone())),
        ),
    }
}

#[async_trait]
impl DeclarationRepository for DeclarationRepo {
    async fn find_by_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &str,
    ) -> Result<Option<Declaration>, DomainError> {
        let found = DeclarationEntity::find()
            .filter(declaration::Column::Key.eq(key))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(found.map(to_domain))
    }

    async fn find_by_key_prefix<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key_prefix: &str,
    ) -> Result<Vec<Declaration>, DomainError> {
        // `_` and `%` in the prefix are LIKE wildcards; a setting path may
        // carry `_`, so the caller re-checks the stripped path exactly. The
        // pattern still narrows the scan to the right neighbourhood.
        let rows = DeclarationEntity::find()
            .filter(declaration::Column::Key.like(format!("{key_prefix}%")))
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
        draft: DeclarationDraft,
    ) -> Result<Declaration, DomainError> {
        let at = now();
        let active = declaration::ActiveModel {
            id: Set(Uuid::new_v4()),
            key: Set(draft.key),
            leaf_slug: Set(draft.leaf_slug),
            value_type_id: Set(draft.value_type_id),
            category_id: Set(draft.category_id),
            default_value: Set(draft.default_value),
            scope_class: Set(draft.scope_class),
            mode: Set(draft.mode),
            requires_step_up: Set(draft.requires_step_up),
            anonymous_exposable: Set(draft.anonymous_exposable),
            domain_affinity: Set(draft.domain_affinity),
            has_secret_trait: Set(draft.has_secret_trait),
            data_classification: Set(draft.data_classification),
            source: Set(draft.source),
            owner_module: Set(draft.owner_module),
            licence_feature: Set(draft.licence_feature),
            status: Set("active".to_owned()),
            description: Set(draft.description),
            last_change_at: Set(at),
            created_at: Set(at),
            updated_at: Set(at),
            created_by: Set(draft.created_by),
        };
        let model = toolkit_db::secure::secure_insert::<DeclarationEntity>(active, scope, conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        Ok(to_domain(model))
    }

    async fn update_metadata<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        metadata: DeclarationMetadata,
    ) -> Result<(), DomainError> {
        // Metadata only: `last_change_at` is the definition arm of the
        // effective recency and moves for the Schema Default and the type, not
        // for a description.
        DeclarationEntity::update_many()
            .col_expr(declaration::Column::Mode, Expr::value(metadata.mode))
            .col_expr(
                declaration::Column::Description,
                Expr::value(metadata.description),
            )
            .col_expr(
                declaration::Column::DomainAffinity,
                Expr::value(metadata.domain_affinity),
            )
            .col_expr(
                declaration::Column::LicenceFeature,
                Expr::value(metadata.licence_feature),
            )
            .col_expr(
                declaration::Column::DataClassification,
                Expr::value(metadata.data_classification),
            )
            .col_expr(
                declaration::Column::RequiresStepUp,
                Expr::value(metadata.requires_step_up),
            )
            .col_expr(
                declaration::Column::AnonymousExposable,
                Expr::value(metadata.anonymous_exposable),
            )
            .col_expr(declaration::Column::LastChangeAt, Expr::value(now()))
            .col_expr(declaration::Column::UpdatedAt, Expr::value(now()))
            .filter(declaration::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        Ok(())
    }

    async fn set_status<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        status: &str,
    ) -> Result<(), DomainError> {
        let at = now();
        DeclarationEntity::update_many()
            .col_expr(declaration::Column::Status, Expr::value(status.to_owned()))
            .col_expr(declaration::Column::LastChangeAt, Expr::value(at))
            .col_expr(declaration::Column::UpdatedAt, Expr::value(at))
            .filter(declaration::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        Ok(())
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        id: Uuid,
    ) -> Result<Option<Declaration>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-6
        // The visibility predicate rides in the query even for a single row.
        // Fetching first and filtering after would make "not visible" and "not
        // present" two code paths, and only one of them is guaranteed to answer
        // the same way.
        let found = apply_visibility(DeclarationEntity::find(), visibility)
            .filter(declaration::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(|err| DomainError::Internal {
                diagnostic: err.to_string(),
            })?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-6
        Ok(found.map(to_domain))
    }

    async fn find_by_category<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        category_id: Uuid,
    ) -> Result<Vec<Declaration>, DomainError> {
        let rows = DeclarationEntity::find()
            .filter(declaration::Column::CategoryId.eq(category_id))
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(to_domain).collect())
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        query: &ODataQuery,
    ) -> Result<Page<Declaration>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-6
        let base = apply_visibility(DeclarationEntity::find(), visibility)
            .secure()
            .scope_with(scope);

        // Tiebreaker is `key`, which `uq_declaration_key` makes unique, so a
        // page boundary can neither repeat nor skip a row.
        let page = paginate_odata::<DeclarationFilterField, DeclarationODataMapper, _, _, _, _>(
            base,
            conn,
            query,
            ("key", SortDir::Asc),
            DECLARATION_LIMIT_CFG,
            |m: declaration::Model| m,
        )
        .await
        .map_err(|err| DomainError::Validation {
            field: "query".to_owned(),
            code: crate::field::ODATA_QUERY,
            message: err.to_string(),
        })?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-6

        Ok(Page {
            items: page.items.into_iter().map(to_domain).collect(),
            page_info: page.page_info,
        })
    }
}
