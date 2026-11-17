//! `model_registry__providers` repository. Every query runs through
//! `SecureConn` under an `AccessScope` the service built from the validated
//! chain or `tenant_id = T0` (DESIGN §4 Authorization rules 6 and 7).
//! Versioned writes are compare-and-set on `version` (ADR-0004).

use model_registry_sdk::odata::ProviderFilterField;
use model_registry_sdk::{ProviderV1, SharingMode};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, QuerySelect};
use toolkit_db::odata::sea_orm_filter::{PaginateOdataTryError, paginate_odata_try};
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity::{model, provider};
use super::error_mapping::{InsertFailure, classify_insert, map_scope_error};
use super::mapper;
use super::odata_mapper::{ProviderODataMapper, with_default_order};
use crate::config::PageLimits;
use crate::domain::error::{DomainError, Resource};
use crate::domain::visibility::ProviderCandidate;

#[derive(Debug, Clone, Copy)]
pub struct ProviderRepo {
    limits: PageLimits,
}

#[derive(FromQueryResult)]
struct CandidateRow {
    id: Uuid,
    tenant_id: Uuid,
    slug: String,
    enabled: bool,
    sharing_mode: String,
}

/// Own rows in any mode plus ancestor rows in `shared` mode (DESIGN §3.6
/// step 2).
fn candidate_condition(t0: Uuid) -> Condition {
    Condition::any()
        .add(provider::Column::TenantId.eq(t0))
        .add(provider::Column::SharingMode.eq(SharingMode::Shared.as_str()))
}

impl ProviderRepo {
    #[must_use]
    pub fn new(limits: PageLimits) -> Self {
        Self { limits }
    }

    /// The SQL predicate of the management list (own plus inherited shared).
    #[must_use]
    pub fn management_condition(t0: Uuid) -> Condition {
        candidate_condition(t0)
    }

    async fn candidates_where(
        conn: &impl DBRunner,
        chain: &[Uuid],
        extra: Option<Condition>,
    ) -> Result<Vec<ProviderCandidate>, DomainError> {
        let t0 = chain.first().copied().unwrap_or_default();
        let scope = AccessScope::for_tenants(chain.to_vec());
        let mut select = provider::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(candidate_condition(t0));
        if let Some(extra) = extra {
            select = select.filter(extra);
        }
        let rows: Vec<CandidateRow> = select
            .project_all(conn, |q| {
                q.select_only()
                    .column(provider::Column::Id)
                    .column(provider::Column::TenantId)
                    .column(provider::Column::Slug)
                    .column(provider::Column::Enabled)
                    .column(provider::Column::SharingMode)
                    .into_model::<CandidateRow>()
            })
            .await
            .map_err(map_scope_error)?;
        rows.into_iter()
            .map(|r| {
                let sharing_mode = SharingMode::from_wire(&r.sharing_mode).ok_or_else(|| {
                    DomainError::internal(format!(
                        "stored providers.sharing_mode `{}` is outside its domain",
                        r.sharing_mode
                    ))
                })?;
                Ok(ProviderCandidate {
                    id: r.id,
                    tenant_id: r.tenant_id,
                    slug: r.slug,
                    enabled: r.enabled,
                    sharing_mode,
                })
            })
            .collect()
    }

    /// All candidate providers of the chain (`chain[0]` is T0).
    pub async fn candidates(
        &self,
        conn: &impl DBRunner,
        chain: &[Uuid],
    ) -> Result<Vec<ProviderCandidate>, DomainError> {
        Self::candidates_where(conn, chain, None).await
    }

    /// The candidate providers of the chain with `slug`: at most one per
    /// chain tenant.
    pub async fn candidates_by_slug(
        &self,
        conn: &impl DBRunner,
        chain: &[Uuid],
        slug: &str,
    ) -> Result<Vec<ProviderCandidate>, DomainError> {
        Self::candidates_where(
            conn,
            chain,
            Some(Condition::all().add(provider::Column::Slug.eq(slug))),
        )
        .await
    }

    pub async fn find_by_id(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ProviderV1>, DomainError> {
        provider::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(provider::Column::Id.eq(id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(mapper::provider_from_row)
            .transpose()
    }

    /// Keyset page in `slug asc, id asc` unless the caller ordered otherwise.
    pub async fn list(
        &self,
        conn: &impl DBRunner,
        scope: &AccessScope,
        predicate: Condition,
        query: &ODataQuery,
    ) -> Result<Page<ProviderV1>, DomainError> {
        let base = provider::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(predicate);
        let query = with_default_order(query, "slug");
        paginate_odata_try::<
            ProviderFilterField,
            ProviderODataMapper,
            provider::Entity,
            ProviderV1,
            _,
            _,
            _,
        >(
            base,
            conn,
            &query,
            ("id", SortDir::Asc),
            self.limits.limit_cfg(),
            mapper::provider_from_row,
        )
        .await
        .map_err(|e| match e {
            PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
                DomainError::from_db_message(&message)
            }
            PaginateOdataTryError::OData(e) => DomainError::OData(e),
            PaginateOdataTryError::MapError(e) => e,
        })
    }

    /// Inserts under `tenant_id = provider.tenant_id`. A duplicate slug in the
    /// tenant is `ProviderAlreadyExists`.
    pub async fn insert(
        &self,
        tx: &impl DBRunner,
        provider: &ProviderV1,
    ) -> Result<(), DomainError> {
        let scope = AccessScope::for_tenant(provider.tenant_id);
        match secure_insert::<provider::Entity>(mapper::provider_to_active(provider), &scope, tx)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(match classify_insert(&e) {
                InsertFailure::UniqueViolation => DomainError::ProviderAlreadyExists {
                    slug: provider.slug.clone(),
                },
                _ => map_scope_error(e),
            }),
        }
    }

    async fn find_locked(
        tx: &impl DBRunner,
        t0: Uuid,
        id: Uuid,
        exclusive: bool,
    ) -> Result<Option<ProviderV1>, DomainError> {
        let select = provider::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(t0))
            .filter(Condition::all().add(provider::Column::Id.eq(id)));
        let select = if exclusive {
            select.lock_exclusive()
        } else {
            select.lock_shared()
        };
        select
            .one(tx)
            .await
            .map_err(map_scope_error)?
            .map(mapper::provider_from_row)
            .transpose()
    }

    /// Loads the row under `tenant_id = t0` with an exclusive row lock; `None`
    /// when T0 owns no such provider.
    pub async fn find_for_update(
        &self,
        tx: &impl DBRunner,
        t0: Uuid,
        id: Uuid,
    ) -> Result<Option<ProviderV1>, DomainError> {
        Self::find_locked(tx, t0, id, true).await
    }

    /// Loads the row under `tenant_id = t0` with a shared row lock, so a
    /// concurrent type migration waits for the transaction; `None` when T0
    /// owns no such provider.
    pub async fn find_for_share(
        &self,
        tx: &impl DBRunner,
        t0: Uuid,
        id: Uuid,
    ) -> Result<Option<ProviderV1>, DomainError> {
        Self::find_locked(tx, t0, id, false).await
    }

    /// Rewrites every column of the row under `tenant_id = provider.tenant_id`,
    /// only if the stored `version` is still `prior_version`; otherwise
    /// `OPTIMISTIC_LOCK_FAILURE`. `provider.version` must already be the
    /// incremented value.
    pub async fn save(
        &self,
        tx: &impl DBRunner,
        provider: &ProviderV1,
        prior_version: i64,
    ) -> Result<ProviderV1, DomainError> {
        let rows = provider::Entity::update_many()
            .set(mapper::provider_to_update_active(provider))
            .secure()
            .scope_with(&AccessScope::for_tenant(provider.tenant_id))
            .filter(
                Condition::all()
                    .add(provider::Column::Id.eq(provider.id))
                    .add(provider::Column::Version.eq(prior_version)),
            )
            .exec_with_returning(tx)
            .await
            .map_err(map_scope_error)?;
        rows.into_iter().next().map_or_else(
            || {
                Err(DomainError::OptimisticLockFailure {
                    resource: Resource::Provider,
                    id: provider.id,
                    detail: "the provider changed concurrently",
                })
            },
            mapper::provider_from_row,
        )
    }

    /// Deletes the provider's models and the provider row at `prior_version`
    /// under `tenant_id = t0`. Returns whether the provider row was deleted.
    pub async fn delete(
        &self,
        tx: &impl DBRunner,
        t0: Uuid,
        id: Uuid,
        prior_version: i64,
    ) -> Result<bool, DomainError> {
        let scope = AccessScope::for_tenant(t0);
        model::Entity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(model::Column::ProviderId.eq(id)))
            .exec(tx)
            .await
            .map_err(map_scope_error)?;
        let result = provider::Entity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(provider::Column::Id.eq(id))
                    .add(provider::Column::Version.eq(prior_version)),
            )
            .exec(tx)
            .await
            .map_err(map_scope_error)?;
        Ok(result.rows_affected > 0)
    }
}
