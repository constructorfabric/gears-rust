//! Authorized pricing catalog reads over the SKU heads.
//! @cpt-dod:cpt-cf-bss-products-dod-browse-published-only:p1
use crate::{
    api::rest::authz_error_to_canonical,
    authz::{access_scope, actions, resource_types},
    domain::{error::DomainError, validation::ValidationReport},
    infra::storage::{RepoError, entity::sku, repo},
};
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_pricing_sdk::product_catalog::{
    CatalogSku, CatalogSkuPage, CatalogTaxCategory, ProductCatalogClientV1, catalog_unreachable,
};
use bss_products_sdk::models::{Lifecycle, Sku};
use sea_orm::{ColumnTrait, Condition};
use std::{collections::BTreeSet, sync::Arc};
use toolkit_canonical_errors::{CanonicalError, resource_error};
#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, filter_node_to_condition};
use toolkit_db::{Db, secure::AccessScope};
use toolkit_odata::filter::{FieldKind, FilterField, parse_odata_filter};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// In-process catalog transport with the same PDP as the REST door.
#[derive(Clone)]
pub struct BrowseCatalogProvider {
    db: Db,
    enforcer: Arc<PolicyEnforcer>,
}
impl BrowseCatalogProvider {
    /// Bind reads to this database and the caller's authorization policy.
    #[must_use]
    pub fn new(db: Db, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { db, enforcer }
    }

    pub(crate) async fn scope(&self, ctx: &SecurityContext) -> Result<AccessScope, CanonicalError> {
        access_scope(
            &self.enforcer,
            ctx,
            &resource_types::SKU,
            actions::READ,
            None,
        )
        .await
        .map_err(|e| {
            authz_error_to_canonical(e, |reason| {
                SkuResource::permission_denied()
                    .with_reason(reason)
                    .create()
            })
        })
    }

    /// Browse the served lifecycle set with a validated `OData` predicate, under the scope the
    /// caller already holds: the browse door asks the PDP once (RS-41).
    pub(crate) async fn browse(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        filter: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<CatalogSkuPage, CanonicalError> {
        let limit = std::cmp::min(limit, 200);
        if limit == 0 {
            return Err(invalid("limit", "limit must be at least one"));
        }
        let mut condition = Condition::all().add(
            crate::infra::storage::repo::sku_repo::effective_lifecycle_in(
                crate::infra::storage::stored_now().date(),
                &["published", "deprecated"],
            ),
        );
        if let Some(filter) = filter.filter(|s| !s.is_empty()) {
            if filter.len() > 32_768 {
                return Err(invalid("$filter", "filter is too long"));
            }
            let parsed = parse_odata_filter::<CatalogField>(filter)
                .map_err(|e| invalid("$filter", e.to_string()))?;
            condition = condition.add(
                filter_node_to_condition::<CatalogField, CatalogMapping>(&parsed)
                    .map_err(|e| invalid("$filter", e))?,
            );
        }
        let query = repo::SkuQuery {
            catalog_filter: Some(condition),
            lifecycle: None,
            limit: u64::from(limit),
            after_code: cursor.filter(|s| !s.is_empty()).map(str::to_owned),
        };
        let conn = self.db.conn().map_err(|e| no_connection(&e))?;
        let mut rows = repo::list_skus(&conn, scope, tenant, &query)
            .await
            .map_err(|e| read_failure(&e))?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_cursor = if more {
            rows.last().map(|s| s.code.clone())
        } else {
            None
        };
        Ok(CatalogSkuPage {
            items: rows.into_iter().map(catalog_sku_of).collect(),
            next_cursor,
        })
    }
}

/// A connection this read could not take (`Db::conn` refuses only inside a transaction): the
/// repository's logged 500, its text off the wire (RS-09).
fn no_connection(e: &toolkit_db::DbError) -> CanonicalError {
    crate::api::rest::repo_error_to_canonical(&RepoError::Db(format!("catalog connection: {e}")))
}

/// A repository failure behind a catalog read. A pool that could not hand out a connection is the
/// catalog that did not answer, a 503 with a fixed detail; any other failure (a statement, a row
/// that does not read) is the repository's logged 500. The driver's text stays in the log, never
/// in a wire detail (RS-07, RS-09).
fn read_failure(e: &RepoError) -> CanonicalError {
    if let RepoError::Driver {
        source: sea_orm::DbErr::ConnectionAcquire(_) | sea_orm::DbErr::Conn(_),
        ..
    } = e
    {
        tracing::error!(error = %e, "bss-products: the catalog's database did not answer");
        return catalog_unreachable("the product catalog's database did not answer");
    }
    crate::api::rest::repo_error_to_canonical(e)
}

impl BrowseCatalogProvider {
    /// The published and deprecated SKUs of `ids`, in code order, read set-based (RS-40): one
    /// statement per [`IDS_PER_READ`] distinct ids, where it read each id on its own.
    async fn skus_by_id(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<CatalogSku>, CanonicalError> {
        let distinct: Vec<Uuid> = ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let conn = self.db.conn().map_err(|e| no_connection(&e))?;
        let mut rows = Vec::with_capacity(distinct.len());
        for chunk in distinct.chunks(IDS_PER_READ) {
            let query = repo::SkuQuery {
                catalog_filter: Some(
                    Condition::all()
                        .add(sku::Column::Id.is_in(chunk.iter().copied()))
                        .add(
                            crate::infra::storage::repo::sku_repo::effective_lifecycle_in(
                                crate::infra::storage::stored_now().date(),
                                &["published", "deprecated"],
                            ),
                        ),
                ),
                lifecycle: None,
                limit: u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                after_code: None,
            };
            rows.extend(
                repo::list_skus(&conn, scope, tenant, &query)
                    .await
                    .map_err(|e| read_failure(&e))?,
            );
        }
        rows.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(rows.into_iter().map(catalog_sku_of).collect())
    }

    /// The tenant's tax-category dictionary: the distinct codes its published SKUs carry, in one
    /// read (RS-14).
    pub(crate) async fn tax_categories(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<CatalogTaxCategory>, CanonicalError> {
        let conn = self.db.conn().map_err(|e| no_connection(&e))?;
        Ok(repo::distinct_tax_categories(&conn, scope, tenant)
            .await
            .map_err(|e| read_failure(&e))?
            .into_iter()
            .map(|code| CatalogTaxCategory {
                display_name: code.clone(),
                code,
            })
            .collect())
    }
}

/// The ids one `get_skus` statement binds: well under either dialect's bound-parameter limit.
const IDS_PER_READ: usize = 1000;

pub(crate) fn invalid(field: &str, detail: impl Into<String>) -> CanonicalError {
    let mut report = ValidationReport::new();
    report.violate("VALIDATION", field, detail);
    DomainError::Validation(report).into()
}

fn catalog_sku_of(s: Sku) -> CatalogSku {
    CatalogSku {
        sku_id: s.id,
        sku_code: s.code,
        name: s.name,
        metering_unit: s.unit,
        status: s.lifecycle.as_str().to_owned(),
        plan_tier: None,
        sku_type: s.r#type.as_str().to_owned(),
        sellable: s.sellable,
        usage_type_ref: s.usage_type_ref,
        deprecated: s.lifecycle == Lifecycle::Deprecated,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CatalogField {
    Id,
    Code,
    Name,
}
impl FilterField for CatalogField {
    const FIELDS: &'static [Self] = &[Self::Id, Self::Code, Self::Name];
    fn name(&self) -> &'static str {
        match self {
            Self::Id => "entity_id",
            Self::Code => "sku_code",
            Self::Name => "name",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Id => FieldKind::Uuid,
            Self::Code | Self::Name => FieldKind::String,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "entity_id" | "sku_id" => Some(Self::Id),
            "entity_code" | "sku_code" => Some(Self::Code),
            "name" => Some(Self::Name),
            _ => None,
        }
    }
}
struct CatalogMapping;
impl FieldToColumn<CatalogField> for CatalogMapping {
    type Column = sku::Column;
    fn map_field(field: CatalogField) -> Self::Column {
        match field {
            CatalogField::Id => sku::Column::Id,
            CatalogField::Code => sku::Column::Code,
            CatalogField::Name => sku::Column::Name,
        }
    }
}

#[async_trait]
impl ProductCatalogClientV1 for BrowseCatalogProvider {
    async fn get_skus(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<Vec<CatalogSku>, CanonicalError> {
        let scope = self.scope(ctx).await?;
        self.skus_by_id(&scope, ctx.subject_tenant_id(), ids).await
    }
    async fn search_skus(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        limit: u64,
        cursor: Option<&str>,
    ) -> Result<CatalogSkuPage, CanonicalError> {
        // The page is 1 to 200 whatever the caller asks (PS-49: the contract's `limit` is `u64`).
        let limit = u32::try_from(std::cmp::min(limit, 200)).unwrap_or(200);
        let filter = q
            .filter(|s| !s.is_empty())
            .map(|q| format!("startswith(name,'{}')", q.replace('\'', "''")));
        let scope = self.scope(ctx).await?;
        self.browse(
            &scope,
            ctx.subject_tenant_id(),
            filter.as_deref(),
            limit,
            cursor,
        )
        .await
    }
    async fn list_tax_categories(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<CatalogTaxCategory>, CanonicalError> {
        let scope = self.scope(ctx).await?;
        self.tax_categories(&scope, ctx.subject_tenant_id()).await
    }
}
#[cfg(test)]
#[path = "catalog_provider_tests.rs"]
mod catalog_provider_tests;
