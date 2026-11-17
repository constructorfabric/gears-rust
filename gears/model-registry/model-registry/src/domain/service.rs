//! Catalog service (DESIGN §3.2): the order of checks and side effects of
//! every operation, in the sequences of §3.6.
//!
//! Reads run their row queries in one read-only snapshot transaction
//! (`principle-snapshot-reads`). Writes run in one read-write transaction
//! that locks the row, compares `expected_version` and the fields whose check
//! was skipped against the locked row, and writes with a compare-and-set on
//! `version` (ADR-0004). No dependency call runs inside a transaction. Every
//! operation except `resolve_model` takes its PDP decision first; writes read
//! no ancestor chain unless they report an inherited entry.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use model_registry_sdk::{
    ApprovalState, ApprovalV1, CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1,
    ModelInfoV1, ModelV1, ProviderEntryV1, ProviderV1, SharingMode, UpdateModelRequestV1,
    UpdateProviderRequestV1,
};
use toolkit_db::secure::{TxAccessMode, TxConfig, TxIsolationLevel};
use toolkit_db::{DBProvider, DbTx};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::access_guard::{
    AccessGuard, MODEL_RESOURCE, PROP_CANONICAL_ID, PROP_GTS_TYPE, PROP_PROVIDER_ID,
    PROVIDER_RESOURCE, actions,
};
use super::clock::Clock;
use super::credential_check::{SecretRefSource, check_secret_ref};
use super::error::{DomainError, Resource};
use super::gts_validator::{self, SchemaSource};
use super::precondition::Precondition;
use super::validation::{self, Limits};
use super::visibility::{Catalog, ProviderCandidate};
use crate::config::{ModelRegistryConfig, PageLimits};
use crate::infra::chain_cache::ChainCache;
use crate::infra::storage::mapper::{apply_model_update, apply_provider_update};
use crate::infra::storage::model_repo::ModelRepo;
use crate::infra::storage::provider_repo::ProviderRepo;

#[domain_model]
pub struct Service {
    db: Arc<DBProvider<DomainError>>,
    providers: ProviderRepo,
    models: ModelRepo,
    chain_cache: Arc<ChainCache>,
    guard: AccessGuard,
    schemas: Arc<dyn SchemaSource>,
    secret_refs: Arc<dyn SecretRefSource>,
    clock: Arc<dyn Clock>,
    page_limits: PageLimits,
    limits: Limits,
}

/// Canonical id split on the first `::` into `(slug, name)`.
fn split_canonical_id(canonical_id: &str) -> Option<(&str, &str)> {
    let (slug, name) = canonical_id.split_once("::")?;
    (!slug.is_empty() && !name.is_empty()).then_some((slug, name))
}

fn empty_page<T>(limits: PageLimits, query: &ODataQuery) -> Page<T> {
    Page {
        items: Vec::new(),
        page_info: PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit: limits.clamp(query.limit),
        },
    }
}

/// The read-only snapshot configuration of every catalog read.
fn snapshot() -> TxConfig {
    TxConfig {
        isolation: Some(TxIsolationLevel::RepeatableRead),
        access_mode: Some(TxAccessMode::ReadOnly),
    }
}

fn stale(resource: Resource, id: Uuid, detail: &'static str) -> DomainError {
    DomainError::OptimisticLockFailure {
        resource,
        id,
        detail,
    }
}

/// The security events of DESIGN §4 Observability: a `NOT_PERMITTED` or
/// `INHERITED_READ_ONLY` outcome is logged with subject, tenant, operation,
/// and resource id.
fn security_event<T>(
    ctx: &SecurityContext,
    resource_id: Option<&dyn std::fmt::Display>,
    result: Result<T, DomainError>,
) -> Result<T, DomainError> {
    let subject = ctx.subject_id();
    let tenant_id = ctx.subject_tenant_id();
    match &result {
        Err(DomainError::NotPermitted { operation, .. }) => {
            let resource_id = resource_id.map(tracing::field::display);
            tracing::warn!(%subject, %tenant_id, operation, resource_id, "not permitted");
        }
        Err(DomainError::InheritedReadOnly { operation, id, .. }) => {
            tracing::warn!(%subject, %tenant_id, operation, resource_id = %id, "write against an inherited entry");
        }
        _ => {}
    }
    result
}

/// Builds the effective catalog from the chain inside a transaction.
async fn catalog_in(
    providers: ProviderRepo,
    tx: &DbTx<'_>,
    chain: &[Uuid],
) -> Result<Catalog, DomainError> {
    let candidates = providers.candidates(tx, chain).await?;
    Ok(Catalog::build(chain, candidates))
}

impl Service {
    #[must_use]
    pub fn new(
        db: Arc<DBProvider<DomainError>>,
        chain_cache: Arc<ChainCache>,
        guard: AccessGuard,
        schemas: Arc<dyn SchemaSource>,
        secret_refs: Arc<dyn SecretRefSource>,
        clock: Arc<dyn Clock>,
        config: &ModelRegistryConfig,
    ) -> Self {
        let page_limits = config.page_limits();
        Self {
            db,
            providers: ProviderRepo::new(page_limits),
            models: ModelRepo::new(page_limits),
            chain_cache,
            guard,
            schemas,
            secret_refs,
            clock,
            page_limits,
            limits: Limits {
                max_json_document_bytes: config.max_json_document_bytes,
            },
        }
    }

    async fn chain(&self, ctx: &SecurityContext) -> Result<Arc<[Uuid]>, DomainError> {
        self.chain_cache.chain(ctx, ctx.subject_tenant_id()).await
    }

    /// Runs `f` in one read-only snapshot transaction. Never called while a
    /// write transaction is open.
    async fn read_snapshot<T, F>(&self, f: F) -> Result<T, DomainError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(
                &'a DbTx<'a>,
            )
                -> Pin<Box<dyn Future<Output = Result<T, DomainError>> + Send + 'a>>
            + Send,
    {
        self.db.transaction_with_config(snapshot(), f).await
    }

    /// `InheritedReadOnly` when T0 can see the provider as an inherited
    /// candidate, otherwise `ProviderNotFound`.
    async fn missing_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        operation: &'static str,
    ) -> DomainError {
        let visible = async {
            let chain = self.chain(ctx).await?;
            let providers = self.providers;
            self.read_snapshot(move |tx| {
                Box::pin(async move {
                    let catalog = catalog_in(providers, tx, &chain).await?;
                    let row = providers
                        .find_by_id(tx, &AccessScope::for_tenants(chain.to_vec()), id)
                        .await?;
                    Ok(row.is_some_and(|p| catalog.provider_is_candidate(&p)))
                })
            })
            .await
        };
        match visible.await {
            Ok(true) => DomainError::InheritedReadOnly {
                resource: Resource::Provider,
                operation,
                id,
            },
            Ok(false) => DomainError::ProviderNotFound { id },
            Err(e) => e,
        }
    }

    /// `InheritedReadOnly` when T0 can see the model as an inherited
    /// candidate, otherwise `ModelNotFound`.
    async fn missing_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        operation: &'static str,
    ) -> DomainError {
        let visible = async {
            let chain = self.chain(ctx).await?;
            let providers = self.providers;
            let models = self.models;
            self.read_snapshot(move |tx| {
                Box::pin(async move {
                    let catalog = catalog_in(providers, tx, &chain).await?;
                    let row = models
                        .find_by_id(tx, &AccessScope::for_tenants(chain.to_vec()), id)
                        .await?;
                    Ok(row.is_some_and(|m| catalog.model_is_candidate(&m)))
                })
            })
            .await
        };
        match visible.await {
            Ok(true) => DomainError::InheritedReadOnly {
                resource: Resource::Model,
                operation,
                id,
            },
            Ok(false) => DomainError::model_not_found(id.to_string()),
            Err(e) => e,
        }
    }

    /// The entry of an own model: never inherited or shadowed.
    fn own_model_entry(model: ModelV1, provider_enabled: bool) -> ModelEntryV1 {
        let approved = model.approval.state == ApprovalState::Approved;
        let mut entry = ModelEntryV1::new(model);
        entry.provider_enabled = provider_enabled;
        entry.available_for_eval = provider_enabled && approved;
        entry
    }

    fn own_provider_entry(provider: ProviderV1) -> ProviderEntryV1 {
        let available_for_eval = provider.enabled;
        let mut entry = ProviderEntryV1::new(provider);
        entry.available_for_eval = available_for_eval;
        entry
    }

    // ==================== Resolution ====================

    pub async fn resolve_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError> {
        let result = self.resolve_model_inner(ctx, canonical_id).await;
        security_event(ctx, Some(&canonical_id), result)
    }

    pub async fn list_available_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError> {
        let result = self.list_available_models_inner(ctx, query).await;
        security_event(ctx, None, result)
    }

    // ==================== Models ====================

    pub async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelEntryV1, DomainError> {
        let provider_id = req.provider_id;
        let result = self.create_model_inner(ctx, req).await;
        security_event(ctx, Some(&provider_id), result)
    }

    pub async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelEntryV1, DomainError> {
        let result = self.get_model_inner(ctx, id).await;
        security_event(ctx, Some(&id), result)
    }

    pub async fn list_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelEntryV1>, DomainError> {
        let result = self.list_models_inner(ctx, query).await;
        security_event(ctx, None, result)
    }

    /// [`Self::update_model`] under `precondition`; `req.expected_version` is
    /// not read.
    pub async fn update_model_if(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
        precondition: Precondition,
    ) -> Result<ModelEntryV1, DomainError> {
        let result = Box::pin(self.update_model_inner(ctx, id, req, precondition)).await;
        security_event(ctx, Some(&id), result)
    }

    pub async fn delete_model_if(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        precondition: Precondition,
    ) -> Result<(), DomainError> {
        let result = self.delete_model_inner(ctx, id, precondition).await;
        security_event(ctx, Some(&id), result)
    }

    /// DESIGN §3.6 `seq-resolve-model`.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), canonical_id))]
    async fn resolve_model_inner(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, DomainError> {
        const OP: &str = "resolve_model";
        let Some((slug, name)) = split_canonical_id(canonical_id) else {
            return Err(DomainError::model_not_found(canonical_id));
        };
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let providers = self.providers;
        let models = self.models;
        let slug_owned = slug.to_owned();
        let name_owned = name.to_owned();
        let found: Option<(ProviderCandidate, Option<ModelV1>)> = self
            .read_snapshot(move |tx| {
                Box::pin(async move {
                    let candidates = providers
                        .candidates_by_slug(tx, &chain, &slug_owned)
                        .await?;
                    let catalog = Catalog::build(&chain, candidates);
                    let Some(winner) = catalog.winner(&slug_owned).cloned() else {
                        return Ok(None);
                    };
                    let model = models
                        .find_for_resolution(tx, &chain, winner.id, &name_owned)
                        .await?;
                    Ok(Some((winner, model)))
                })
            })
            .await?;
        let Some((winner, model)) = found else {
            return Err(DomainError::model_not_found(canonical_id));
        };
        let model = model
            .filter(|m| m.tenant_id == t0 || m.sharing_mode == SharingMode::Shared)
            .ok_or_else(|| DomainError::model_not_found(canonical_id))?;
        if !winner.enabled {
            return Err(DomainError::ProviderDisabled {
                canonical_id: canonical_id.to_owned(),
            });
        }
        if model.approval.state != ApprovalState::Approved {
            return Err(DomainError::ModelNotApproved {
                canonical_id: canonical_id.to_owned(),
                state: model.approval.state,
            });
        }
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::EVALUATE,
                Some(model.id),
                &[
                    (
                        PROP_PROVIDER_ID,
                        serde_json::json!(model.provider_id.to_string()),
                    ),
                    (PROP_CANONICAL_ID, serde_json::json!(model.canonical_id)),
                    (
                        PROP_GTS_TYPE,
                        serde_json::json!(model.info.gts_type.to_string()),
                    ),
                ],
                OP,
            )
            .await?;
        if !decision.matches(&model) {
            return Err(DomainError::not_permitted(OP));
        }
        Ok(model)
    }

    /// DESIGN §3.6 `seq-list-available-models`: one `evaluate` decision, then
    /// the catalog and the page from one snapshot.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    async fn list_available_models_inner(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, DomainError> {
        const OP: &str = "list_available_models";
        let t0 = ctx.subject_tenant_id();
        let chain = self.chain(ctx).await?;
        let decision = self
            .guard
            .decide(ctx, &MODEL_RESOURCE, actions::EVALUATE, None, &[], OP)
            .await?;
        let scope = decision.scope(&chain);
        let providers = self.providers;
        let models = self.models;
        let page_limits = self.page_limits;
        let query = query.clone();
        self.read_snapshot(move |tx| {
            Box::pin(async move {
                let catalog = catalog_in(providers, tx, &chain).await?;
                let allow_list = catalog.allow_list();
                if allow_list.is_empty() {
                    return Ok(empty_page(page_limits, &query));
                }
                models
                    .list(
                        tx,
                        &scope,
                        ModelRepo::available_condition(t0, &allow_list),
                        &query,
                    )
                    .await
            })
        })
        .await
    }

    // ==================== Providers ====================

    // The provider operations scope their permission and validation errors
    // to the provider resource type.

    pub async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderEntryV1, DomainError> {
        let result = self.create_provider_inner(ctx, req).await;
        security_event(ctx, None, result).map_err(DomainError::for_provider)
    }

    pub async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderEntryV1, DomainError> {
        let result = self.get_provider_inner(ctx, id).await;
        security_event(ctx, Some(&id), result).map_err(DomainError::for_provider)
    }

    pub async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderEntryV1>, DomainError> {
        let result = self.list_providers_inner(ctx, query).await;
        security_event(ctx, None, result).map_err(DomainError::for_provider)
    }

    pub async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderEntryV1, DomainError> {
        let precondition = req.expected_version.into();
        self.update_provider_if(ctx, id, req, precondition).await
    }

    /// [`Self::update_provider`] under `precondition`; `req.expected_version`
    /// is not read.
    pub async fn update_provider_if(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
        precondition: Precondition,
    ) -> Result<ProviderEntryV1, DomainError> {
        let result = self.update_provider_inner(ctx, id, req, precondition).await;
        security_event(ctx, Some(&id), result).map_err(DomainError::for_provider)
    }

    pub async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        self.delete_provider_if(ctx, id, expected_version.into())
            .await
    }

    pub async fn delete_provider_if(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        precondition: Precondition,
    ) -> Result<(), DomainError> {
        let result = self.delete_provider_inner(ctx, id, precondition).await;
        security_event(ctx, Some(&id), result).map_err(DomainError::for_provider)
    }

    /// DESIGN §3.6 `seq-provider-write`, create.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), slug = req.slug()))]
    async fn create_provider_inner(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderEntryV1, DomainError> {
        const OP: &str = "create_provider";
        validation::validate_create_provider(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &PROVIDER_RESOURCE, actions::CREATE, None, &[], OP)
            .await?;
        gts_validator::check_provider_type(req.gts_type())?;
        gts_validator::provider_type_validator(
            self.schemas.as_ref(),
            ctx,
            req.gts_type(),
            "gts_type",
        )
        .await?;
        let now = self.clock.now();
        let subject = ctx.subject_id();
        let mut provider = ProviderV1::new(
            Uuid::now_v7(),
            t0,
            req.slug(),
            req.name(),
            req.gts_type().clone(),
            subject,
            now,
        );
        provider.enabled = req.enabled();
        provider.sharing_mode = req.sharing_mode();
        provider.managed = req.managed();
        provider.metadata = req.metadata().cloned();
        if !decision.matches(&provider) {
            return Err(DomainError::not_permitted(OP));
        }
        let repo = self.providers;
        let to_insert = provider.clone();
        self.db
            .transaction(move |tx| Box::pin(async move { repo.insert(tx, &to_insert).await }))
            .await?;
        tracing::info!(provider_id = %provider.id, %t0, %subject, "provider created");
        Ok(Self::own_provider_entry(provider))
    }

    /// DESIGN §3.6 `seq-management-read`, point read.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    async fn get_provider_inner(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderEntryV1, DomainError> {
        const OP: &str = "get_provider";
        let decision = self
            .guard
            .decide(ctx, &PROVIDER_RESOURCE, actions::GET, Some(id), &[], OP)
            .await?;
        let chain = self.chain(ctx).await?;
        let providers = self.providers;
        let (catalog, provider) = self
            .read_snapshot(move |tx| {
                Box::pin(async move {
                    let catalog = catalog_in(providers, tx, &chain).await?;
                    let provider = providers
                        .find_by_id(tx, &AccessScope::for_tenants(chain.to_vec()), id)
                        .await?;
                    Ok((catalog, provider))
                })
            })
            .await?;
        let provider = provider
            .filter(|p| catalog.provider_is_candidate(p))
            .ok_or(DomainError::ProviderNotFound { id })?;
        if !decision.matches(&provider) {
            return Err(DomainError::not_permitted(OP));
        }
        Ok(catalog.provider_entry(provider))
    }

    /// DESIGN §3.6 `seq-management-read`, provider list.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    async fn list_providers_inner(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderEntryV1>, DomainError> {
        const OP: &str = "list_providers";
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &PROVIDER_RESOURCE, actions::LIST, None, &[], OP)
            .await?;
        let chain = self.chain(ctx).await?;
        let scope = decision.scope(&chain);
        let providers = self.providers;
        let query = query.clone();
        let (catalog, page) = self
            .read_snapshot(move |tx| {
                Box::pin(async move {
                    let catalog = catalog_in(providers, tx, &chain).await?;
                    let page = providers
                        .list(tx, &scope, ProviderRepo::management_condition(t0), &query)
                        .await?;
                    Ok((catalog, page))
                })
            })
            .await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|p| catalog.provider_entry(p))
                .collect(),
            page_info: page.page_info,
        })
    }

    /// DESIGN §3.6 `seq-provider-write`, update and type migration.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    #[allow(clippy::too_many_lines)]
    async fn update_provider_inner(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
        precondition: Precondition,
    ) -> Result<ProviderEntryV1, DomainError> {
        const OP: &str = "update_provider";
        validation::validate_update_provider(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &PROVIDER_RESOURCE, actions::UPDATE, Some(id), &[], OP)
            .await?;
        let pre = {
            let conn = self.db.conn()?;
            self.providers
                .find_by_id(&conn, &AccessScope::for_tenant(t0), id)
                .await?
        };
        let Some(pre) = pre else {
            return Err(self.missing_provider(ctx, id, OP).await);
        };
        if !decision.matches(&pre) {
            return Err(DomainError::not_permitted(OP));
        }

        // A `gts_type` different from the stored one is a type migration; an
        // equal one is re-compared under the lock.
        let migration = req
            .gts_type
            .as_ref()
            .filter(|t| **t != pre.gts_type)
            .cloned();
        let recompare_gts = req.gts_type.is_some() && migration.is_none();
        let pre_versions: Option<BTreeMap<Uuid, i64>> = if let Some(new_type) = &migration {
            gts_validator::check_provider_type(new_type)?;
            let validator = gts_validator::provider_type_validator(
                self.schemas.as_ref(),
                ctx,
                new_type,
                "gts_type",
            )
            .await?;
            let pre_models = {
                let conn = self.db.conn()?;
                self.models.list_by_provider(&conn, t0, id).await?
            };
            let violations: Vec<_> = pre_models
                .iter()
                .flat_map(|m| {
                    gts_validator::check_settings(
                        &validator,
                        &m.info.provider_settings,
                        "gts_type",
                        Some(&m.canonical_id),
                    )
                })
                .collect();
            if !violations.is_empty() {
                return Err(DomainError::invalid_all(violations));
            }
            Some(pre_models.iter().map(|m| (m.id, m.version)).collect())
        } else {
            None
        };

        let subject = ctx.subject_id();
        let now = self.clock.now();
        let providers = self.providers;
        let models = self.models;
        let pre_type = pre.gts_type.clone();
        let pre_version = pre.version;
        let migration_type = migration.clone();
        let updated = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(mut locked) = providers.find_for_update(tx, t0, id).await? else {
                        return Ok(None);
                    };
                    if !decision.matches(&locked) {
                        return Err(DomainError::not_permitted(OP));
                    }
                    if !precondition.holds(locked.version) {
                        return Err(stale(Resource::Provider, id, "stale expected_version"));
                    }
                    if recompare_gts && locked.gts_type != pre_type {
                        return Err(stale(
                            Resource::Provider,
                            id,
                            "provider gts_type changed concurrently",
                        ));
                    }
                    let locked_models = if let Some(expected) = &pre_versions {
                        if locked.version != pre_version {
                            return Err(stale(
                                Resource::Provider,
                                id,
                                "the provider changed during type migration",
                            ));
                        }
                        let locked_versions = models.lock_versions_by_provider(tx, t0, id).await?;
                        if &locked_versions != expected {
                            return Err(stale(
                                Resource::Provider,
                                id,
                                "the model set changed during type migration",
                            ));
                        }
                        locked_versions.len() as u64
                    } else {
                        0
                    };
                    let before = (locked.enabled, locked.sharing_mode);
                    let prior = locked.version;
                    apply_provider_update(&mut locked, &req, subject, now);
                    // A type migration changes a constrained property.
                    if !decision.matches(&locked) {
                        return Err(DomainError::not_permitted(OP));
                    }
                    let saved = providers.save(tx, &locked, prior).await?;
                    if let Some(new_type) = &migration_type {
                        let moved = models
                            .migrate_type(tx, t0, id, new_type, subject, now)
                            .await?;
                        if moved != locked_models {
                            return Err(stale(
                                Resource::Provider,
                                id,
                                "the model set changed during type migration",
                            ));
                        }
                    }
                    Ok(Some((before, saved, locked_models)))
                })
            })
            .await?;
        let Some(((enabled_before, mode_before), provider, moved)) = updated else {
            return Err(self.missing_provider(ctx, id, OP).await);
        };
        tracing::info!(provider_id = %id, %t0, %subject, version = provider.version, "provider updated");
        if enabled_before != provider.enabled {
            tracing::info!(provider_id = %id, %t0, %subject, old = enabled_before, new = provider.enabled, "provider enabled changed");
        }
        if mode_before != provider.sharing_mode {
            tracing::info!(provider_id = %id, %t0, %subject, old = mode_before.as_str(), new = provider.sharing_mode.as_str(), "provider sharing_mode changed");
        }
        if let Some(new_type) = migration {
            tracing::info!(provider_id = %id, %t0, %subject, old = %pre.gts_type, new = %new_type, models = moved, "provider type migrated");
        }
        Ok(Self::own_provider_entry(provider))
    }

    /// DESIGN §3.6 `seq-provider-write`, delete.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    async fn delete_provider_inner(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        precondition: Precondition,
    ) -> Result<(), DomainError> {
        const OP: &str = "delete_provider";
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &PROVIDER_RESOURCE, actions::DELETE, Some(id), &[], OP)
            .await?;
        let repo = self.providers;
        let deleted = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(provider) = repo.find_for_update(tx, t0, id).await? else {
                        return Ok(false);
                    };
                    if !decision.matches(&provider) {
                        return Err(DomainError::not_permitted(OP));
                    }
                    if !precondition.holds(provider.version) {
                        return Err(stale(Resource::Provider, id, "stale expected_version"));
                    }
                    if !repo.delete(tx, t0, id, provider.version).await? {
                        return Err(stale(
                            Resource::Provider,
                            id,
                            "the provider changed concurrently",
                        ));
                    }
                    Ok(true)
                })
            })
            .await?;
        if !deleted {
            return Err(self.missing_provider(ctx, id, OP).await);
        }
        tracing::info!(provider_id = %id, %t0, subject = %ctx.subject_id(), "provider deleted");
        Ok(())
    }

    // ==================== Models ====================

    /// DESIGN §3.6 `seq-model-write`, create.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), provider_id = %req.provider_id))]
    async fn create_model_inner(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelEntryV1, DomainError> {
        const OP: &str = "create_model";
        validation::validate_create_model(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(
                ctx,
                &MODEL_RESOURCE,
                actions::CREATE,
                None,
                &[(
                    PROP_PROVIDER_ID,
                    serde_json::json!(req.provider_id.to_string()),
                )],
                OP,
            )
            .await?;
        let provider = {
            let conn = self.db.conn()?;
            self.providers
                .find_by_id(&conn, &AccessScope::for_tenant(t0), req.provider_id)
                .await?
        };
        let Some(provider) = provider else {
            return Err(self.missing_provider(ctx, req.provider_id, OP).await);
        };
        let validator = gts_validator::provider_type_validator(
            self.schemas.as_ref(),
            ctx,
            &provider.gts_type,
            "provider_settings",
        )
        .await?;
        gts_validator::validate_settings(&validator, &req.provider_settings)?;
        if let Some(secret_ref) = &req.secret_ref {
            check_secret_ref(self.secret_refs.as_ref(), ctx, secret_ref).await?;
        }

        let now = self.clock.now();
        let subject = ctx.subject_id();
        let mut info = ModelInfoV1::new(
            provider.gts_type.clone(),
            req.provider_model_id,
            req.display_name,
            req.context_window,
        );
        info.base_url = req.base_url;
        info.secret_ref = req.secret_ref;
        info.description = req.description;
        info.family = req.family;
        info.vendor = req.vendor;
        info.managed = req.managed;
        info.architecture = req.architecture;
        info.size_bytes = req.size_bytes;
        info.format = req.format;
        info.region = req.region;
        info.hosted_by = req.hosted_by;
        info.last_release_at = req.last_release_at;
        info.reasoning_level = req.reasoning_level;
        info.model_version = req.model_version;
        info.sort_order = req.sort_order;
        info.icon = req.icon;
        info.multiplier_display = req.multiplier_display;
        info.cost = req.cost;
        info.performance = req.performance;
        info.additional_info = req.additional_info;
        info.supported_api = req.supported_api;
        info.capabilities = req.capabilities;
        info.disabled_capabilities = req.disabled_capabilities;
        info.default_parameters = req.default_parameters;
        info.allow_parameter_override = req.allow_parameter_override;
        info.allow_extra_params = req.allow_extra_params;
        info.provider_settings = req.provider_settings;
        let canonical_id = format!("{}::{}", provider.slug, req.name);
        let mut model = ModelV1::new(
            Uuid::now_v7(),
            t0,
            provider.id,
            req.name,
            canonical_id,
            info,
            subject,
            now,
        );
        model.sharing_mode = req.sharing_mode.unwrap_or_default();
        model.approval = ApprovalV1::new(
            req.approval_state.unwrap_or(ApprovalState::Approved),
            subject,
            now,
        );
        if !decision.matches(&model) {
            return Err(DomainError::not_permitted(OP));
        }
        let providers = self.providers;
        let models = self.models;
        let to_insert = model.clone();
        let validated_type = provider.gts_type.clone();
        let provider_id = provider.id;
        let provider_enabled = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    // A shared lock on the provider row: a concurrent type
                    // migration (exclusive lock) waits for this insert.
                    let Some(locked) = providers.find_for_share(tx, t0, provider_id).await? else {
                        return Err(DomainError::ProviderNotFound { id: provider_id });
                    };
                    if locked.gts_type != validated_type {
                        return Err(DomainError::ProviderTypeChanged { provider_id });
                    }
                    models.insert(tx, &to_insert).await?;
                    Ok(locked.enabled)
                })
            })
            .await?;
        tracing::info!(model_id = %model.id, canonical_id = %model.canonical_id, %t0, %subject, approval = model.approval.state.as_str(), "model created");
        Ok(Self::own_model_entry(model, provider_enabled))
    }

    /// DESIGN §3.6 `seq-management-read`, point read.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    async fn get_model_inner(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelEntryV1, DomainError> {
        const OP: &str = "get_model";
        let decision = self
            .guard
            .decide(ctx, &MODEL_RESOURCE, actions::GET, Some(id), &[], OP)
            .await?;
        let chain = self.chain(ctx).await?;
        let providers = self.providers;
        let models = self.models;
        let (catalog, model) = self
            .read_snapshot(move |tx| {
                Box::pin(async move {
                    let catalog = catalog_in(providers, tx, &chain).await?;
                    let model = models
                        .find_by_id(tx, &AccessScope::for_tenants(chain.to_vec()), id)
                        .await?;
                    Ok((catalog, model))
                })
            })
            .await?;
        let model = model
            .filter(|m| catalog.model_is_candidate(m))
            .ok_or_else(|| DomainError::model_not_found(id.to_string()))?;
        if !decision.matches(&model) {
            return Err(DomainError::not_permitted(OP));
        }
        catalog
            .model_entry(model)
            .ok_or_else(|| DomainError::internal("candidate model without a candidate provider"))
    }

    /// DESIGN §3.6 `seq-management-read`, admin list.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    async fn list_models_inner(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelEntryV1>, DomainError> {
        const OP: &str = "list_models";
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &MODEL_RESOURCE, actions::LIST, None, &[], OP)
            .await?;
        let chain = self.chain(ctx).await?;
        let scope = decision.scope(&chain);
        let providers = self.providers;
        let models = self.models;
        let query = query.clone();
        let (catalog, page) = self
            .read_snapshot(move |tx| {
                Box::pin(async move {
                    let catalog = catalog_in(providers, tx, &chain).await?;
                    let page = models
                        .list(
                            tx,
                            &scope,
                            ModelRepo::management_condition(t0, &catalog.inherited_candidate_ids()),
                            &query,
                        )
                        .await?;
                    Ok((catalog, page))
                })
            })
            .await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .filter_map(|m| catalog.model_entry(m))
                .collect(),
            page_info: page.page_info,
        })
    }

    pub async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
    ) -> Result<ModelEntryV1, DomainError> {
        let precondition = req.expected_version.into();
        self.update_model_if(ctx, id, req, precondition).await
    }

    /// DESIGN §3.6 `seq-model-write`, update.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    #[allow(clippy::too_many_lines)]
    async fn update_model_inner(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
        precondition: Precondition,
    ) -> Result<ModelEntryV1, DomainError> {
        const OP: &str = "update_model";
        validation::validate_update_model(&req, self.limits)?;
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &MODEL_RESOURCE, actions::UPDATE, Some(id), &[], OP)
            .await?;
        let pre = {
            let conn = self.db.conn()?;
            self.models
                .find_by_id(&conn, &AccessScope::for_tenant(t0), id)
                .await?
        };
        let Some(pre) = pre else {
            return Err(self.missing_model(ctx, id, OP).await);
        };
        if !decision.matches(&pre) {
            return Err(DomainError::not_permitted(OP));
        }

        // Settings are validated only when the sent value differs from the
        // stored one; a resent value is re-compared under the lock.
        let settings_changed = req
            .provider_settings
            .as_ref()
            .is_some_and(|s| *s != pre.info.provider_settings);
        let recompare_settings = req.provider_settings.is_some() && !settings_changed;
        let validated_type: Option<gts::GtsTypeId> = if settings_changed {
            let validator = gts_validator::provider_type_validator(
                self.schemas.as_ref(),
                ctx,
                &pre.info.gts_type,
                "provider_settings",
            )
            .await?;
            if let Some(settings) = &req.provider_settings {
                gts_validator::validate_settings(&validator, settings)?;
            }
            Some(pre.info.gts_type.clone())
        } else {
            None
        };
        // The credential check runs only for a non-null value different from
        // the stored one; null clears without a call.
        let secret_changed = matches!(
            &req.secret_ref,
            Some(Some(r)) if Some(r) != pre.info.secret_ref.as_ref()
        );
        let recompare_secret = matches!(&req.secret_ref, Some(Some(_))) && !secret_changed;
        if secret_changed && let Some(Some(secret_ref)) = &req.secret_ref {
            check_secret_ref(self.secret_refs.as_ref(), ctx, secret_ref).await?;
        }

        let subject = ctx.subject_id();
        let now = self.clock.now();
        let providers = self.providers;
        let models = self.models;
        let pre_settings = pre.info.provider_settings.clone();
        let pre_secret = pre.info.secret_ref.clone();
        let updated = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(mut locked) = models.find_for_update(tx, t0, id).await? else {
                        return Ok(None);
                    };
                    if !decision.matches(&locked) {
                        return Err(DomainError::not_permitted(OP));
                    }
                    if !precondition.holds(locked.version) {
                        return Err(stale(Resource::Model, id, "stale expected_version"));
                    }
                    if recompare_settings && locked.info.provider_settings != pre_settings {
                        return Err(stale(
                            Resource::Model,
                            id,
                            "provider_settings changed concurrently",
                        ));
                    }
                    if recompare_secret && locked.info.secret_ref != pre_secret {
                        return Err(stale(
                            Resource::Model,
                            id,
                            "secret_ref changed concurrently",
                        ));
                    }
                    if validated_type.is_some_and(|t| t != locked.info.gts_type) {
                        return Err(DomainError::ProviderTypeChanged {
                            provider_id: locked.provider_id,
                        });
                    }
                    let before = (locked.approval.state, locked.sharing_mode);
                    let prior = locked.version;
                    apply_model_update(&mut locked, &req, subject, now);
                    let saved = models.save(tx, &locked, prior).await?;
                    let provider_enabled = providers
                        .find_by_id(tx, &AccessScope::for_tenant(t0), saved.provider_id)
                        .await?
                        .is_some_and(|p| p.enabled);
                    Ok(Some((before, saved, provider_enabled)))
                })
            })
            .await?;
        let Some(((state_before, mode_before), model, provider_enabled)) = updated else {
            return Err(self.missing_model(ctx, id, OP).await);
        };
        tracing::info!(model_id = %id, %t0, %subject, version = model.version, "model updated");
        if state_before != model.approval.state {
            tracing::info!(model_id = %id, %t0, %subject, old = state_before.as_str(), new = model.approval.state.as_str(), "model approval changed");
        }
        if mode_before != model.sharing_mode {
            tracing::info!(model_id = %id, %t0, %subject, old = mode_before.as_str(), new = model.sharing_mode.as_str(), "model sharing_mode changed");
        }
        Ok(Self::own_model_entry(model, provider_enabled))
    }

    pub async fn delete_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        self.delete_model_if(ctx, id, expected_version.into()).await
    }

    /// DESIGN §3.6 `seq-model-write`, delete.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %id))]
    async fn delete_model_inner(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        precondition: Precondition,
    ) -> Result<(), DomainError> {
        const OP: &str = "delete_model";
        let t0 = ctx.subject_tenant_id();
        let decision = self
            .guard
            .decide(ctx, &MODEL_RESOURCE, actions::DELETE, Some(id), &[], OP)
            .await?;
        let repo = self.models;
        let deleted = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let Some(model) = repo.find_for_update(tx, t0, id).await? else {
                        return Ok(false);
                    };
                    if !decision.matches(&model) {
                        return Err(DomainError::not_permitted(OP));
                    }
                    if !precondition.holds(model.version) {
                        return Err(stale(Resource::Model, id, "stale expected_version"));
                    }
                    if !repo.delete(tx, t0, id, model.version).await? {
                        return Err(stale(Resource::Model, id, "the model changed concurrently"));
                    }
                    Ok(true)
                })
            })
            .await?;
        if !deleted {
            return Err(self.missing_model(ctx, id, OP).await);
        }
        tracing::info!(model_id = %id, %t0, subject = %ctx.subject_id(), "model deleted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{snapshot, split_canonical_id};
    use toolkit_db::secure::{TxAccessMode, TxIsolationLevel};

    #[test]
    fn canonical_id_splits_on_the_first_separator() {
        assert_eq!(
            split_canonical_id("openai::gpt-4o"),
            Some(("openai", "gpt-4o"))
        );
        assert_eq!(split_canonical_id("a::b::c"), Some(("a", "b::c")));
        assert_eq!(split_canonical_id("openai"), None);
        assert_eq!(split_canonical_id("::gpt"), None);
        assert_eq!(split_canonical_id("openai::"), None);
    }

    #[test]
    fn reads_use_a_read_only_repeatable_read_snapshot() {
        let cfg = snapshot();
        assert_eq!(cfg.isolation, Some(TxIsolationLevel::RepeatableRead));
        assert_eq!(cfg.access_mode, Some(TxAccessMode::ReadOnly));
    }
}
