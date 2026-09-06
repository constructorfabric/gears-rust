// Created: 2026-08-26 by Constructor Tech
//! Persistence for declarations.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, ExprTrait, QueryFilter};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::{Declaration, DeclarationRepository};
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
