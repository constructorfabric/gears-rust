// Created: 2026-04-17 by Constructor Tech
//! Public API trait for the `model-registry` module.
//!
//! [`ModelRegistryClientV1`] is registered in `ClientHub` by the module:
//! ```ignore
//! let mr = hub.get::<dyn ModelRegistryClientV1>()?;
//! ```

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use toolkit_odata::{ODataQuery, Page};

use crate::errors::ModelRegistryError;
use crate::models::{
    CreateModelRequest, CreateProviderRequest, Model, Provider, UpdateModelRequest,
    UpdateProviderRequest,
};

/// Public API trait for the Model Registry (Version 1).
///
/// This trait is registered in `ClientHub` by the model-registry module:
/// ```ignore
/// let mr = hub.get::<dyn ModelRegistryClientV1>()?;
/// let model = mr.get_tenant_model(ctx, "openai::gpt-4o").await?;
/// ```
///
/// All methods require `SecurityContext` for tenant scoping and authorization.
#[async_trait]
pub trait ModelRegistryClientV1: Send + Sync {
    // ==================== Models — read (P1) ====================

    /// Get a model by canonical ID within the caller's tenant context.
    ///
    /// Returns the model with its approval status resolved from
    /// `ModelApproval` (P1: written directly by admins; P2 onward: routed
    /// through Approval Service). Uses cache-first lookup with DB fallback.
    async fn get_tenant_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<Model, ModelRegistryError>;

    /// List models available to the caller's tenant with `OData` filtering.
    ///
    /// Supports `$filter` on: `lifecycle_status`, `approval_status`,
    /// `info.gts_type`, `info.supported_api`, `info.provider_model_id`,
    /// `info.capabilities.*` (e.g. `vision`, `function_calling`, `streaming`,
    /// `reasoning.effort`), `info.vendor`, `info.family`. Per-provider
    /// parameter and cost fields are not filterable in v1 — see
    /// `docs/DESIGN.md` §3.3.
    ///
    /// Returns `Model` (the default `P = serde_json::Value` for
    /// heterogeneous lists). Consumers narrowed to a specific provider (e.g.
    /// when they've already filtered on
    /// `info.gts_type eq 'gts.cf.genai.model.info.v1~cf.genai._.openai.v1~'`)
    /// can call [`Model::try_into_typed`] on each result.
    async fn list_tenant_models(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<Model>, ModelRegistryError>;

    // ==================== Models — manual management (P1) ====================
    //
    // P1 admin catalog management without auto-discovery
    // (`cpt-cf-model-registry-fr-manual-model-management`). Status changes
    // (`approve` / `reject` / `revoke`) flow through `update_model` with
    // `UpdateModelRequest::approval_status` — no dedicated action endpoints.
    //
    // Same SDK methods continue to work in P2; only the implementation of
    // status writes shifts from a direct DB update to an Approval Service
    // workflow call (DESIGN §1.2 driver `fr-model-approval`).

    /// Manually register a new model in the catalog.
    ///
    /// Provider must already exist (registered via [`Self::create_provider`]
    /// or inherited from an ancestor tenant). The `canonical_id` is derived
    /// from `req.provider_slug` + `req.info.provider_model_id`.
    ///
    /// In P1 the optional `req.approval_status` is written directly to
    /// `ModelApproval`; defaults to [`crate::models::ApprovalStatus::Pending`]
    /// when `None`. In P2 the same field initiates the Approval Service
    /// workflow.
    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequest,
    ) -> Result<Model, ModelRegistryError>;

    /// Update an existing model's mutable fields (PATCH semantics).
    ///
    /// `canonical_id`, `provider_slug`, `info.provider_model_id`, and
    /// `info.gts_type` are immutable — to change them, soft-delete and
    /// recreate.
    ///
    /// The `req.approval_status` field is the unified entry point for
    /// approve / reject / revoke transitions:
    /// - **P1**: writes directly to `ModelApproval`.
    /// - **P2 onward**: routes through the Approval Service workflow while
    ///   non-status field updates remain direct.
    async fn update_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
        req: UpdateModelRequest,
    ) -> Result<Model, ModelRegistryError>;

    /// Soft-delete a model by canonical ID (sets `lifecycle_status` to
    /// [`crate::models::LifecycleStatus::Deprecated`]).
    ///
    /// Record is retained but hidden from default `list_tenant_models`
    /// responses. Resurrection requires recreate via [`Self::create_model`]
    /// after the previous record is purged.
    async fn delete_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<(), ModelRegistryError>;

    // ==================== Providers (P1) ====================

    /// Get a provider by ID.
    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Provider, ModelRegistryError>;

    /// List providers for the caller's tenant with `OData` filtering.
    async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<Provider>, ModelRegistryError>;

    /// Register a new provider for the caller's tenant.
    async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequest,
    ) -> Result<Provider, ModelRegistryError>;

    /// Update a provider (PATCH semantics).
    async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequest,
    ) -> Result<Provider, ModelRegistryError>;

    /// Delete a provider by ID.
    async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ModelRegistryError>;
}
