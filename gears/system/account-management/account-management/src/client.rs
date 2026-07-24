//! Newtype adapter that implements
//! [`account_management_sdk::AccountManagementClient`] by delegating to
//! the impl-side [`TenantService`] and [`UserService`].
//!
//! Construction happens once in
//! [`crate::gear::AccountManagementGear::init`] (or its bootstrap
//! sibling) and the resulting `Arc<dyn AccountManagementClient>` is
//! registered in `ClientHub` so every external consumer resolves
//! through the SDK trait, never the impl service directly.
//!
//! The adapter is a **thin shim**:
//!
//! * Forwards every call 1:1 to the underlying service.
//! * Maps every internal `DomainError` to
//!   `toolkit_canonical_errors::CanonicalError` via the single
//!   `From<DomainError> for CanonicalError` ladder in
//!   `infra::sdk_error_mapping` (the same ladder the REST boundary
//!   uses), per ADR 0005. Consumers may project the canonical envelope
//!   into the typed `account_management_sdk::AccountManagementError`
//!   view at their call site; no new error vocabulary is introduced at
//!   this boundary.
//! * Does NOT add any extra authorization / validation — those live
//!   in the service layer where they belong (PEP for tenants, plugin
//!   guards for users).
//!
//! Keeping the adapter zero-logic makes the SDK trait the single
//! source of truth for AM's public contract: any future service-side
//! refactor that doesn't change the SDK shape is transparent to
//! consumers, and any change to the SDK shape forces a synchronised
//! impl update via the trait bounds.

use std::sync::Arc;

use account_management_sdk::client::AccountManagementClient;
use account_management_sdk::idp_user::{IdpNewUser, IdpUser, IdpUserPatch, ListUsersQuery};
use account_management_sdk::metadata::{MetadataEntry, UpsertMetadataRequest};
use account_management_sdk::tenant::{CreateTenantRequest, Tenant, UpdateTenantRequest};
use async_trait::async_trait;
use gts::GtsTypeId;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::metadata::service::MetadataService;
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::service::TenantService;
use crate::domain::user::service::UserService;

/// `ClientHub`-shaped adapter implementing
/// [`AccountManagementClient`] for the production wiring.
///
/// Generic over [`TenantRepo`] so the same adapter type is used by
/// the production SeaORM-backed wiring and by the in-process test
/// harness with an `Arc<FakeTenantRepo>` repo — the only constraint
/// is `R: TenantRepo + 'static`.
#[allow(
    clippy::struct_field_names,
    reason = "AM-internal `*_service` suffix on every field is the established convention across `gear.rs` / `client.rs` / `TenantService::with_*` — stripping it would lose the obvious one-to-one mapping from `service` field to backing `Arc<XxxService>`"
)]
pub struct AccountManagementClientImpl<R: TenantRepo> {
    tenant_service: Arc<TenantService<R>>,
    user_service: Arc<UserService>,
    metadata_service: Arc<MetadataService>,
}

impl<R: TenantRepo> AccountManagementClientImpl<R> {
    /// Build the adapter from the three already-constructed services.
    /// `gear.rs` owns the wiring; this constructor just hooks them
    /// up behind the SDK trait.
    #[must_use]
    pub const fn new(
        tenant_service: Arc<TenantService<R>>,
        user_service: Arc<UserService>,
        metadata_service: Arc<MetadataService>,
    ) -> Self {
        Self {
            tenant_service,
            user_service,
            metadata_service,
        }
    }
}

#[async_trait]
impl<R> AccountManagementClient for AccountManagementClientImpl<R>
where
    R: TenantRepo + Send + Sync + 'static,
{
    // -----------------------------------------------------------------
    // Tenant CRUD
    // -----------------------------------------------------------------

    async fn create_tenant(
        &self,
        ctx: &SecurityContext,
        input: CreateTenantRequest,
    ) -> Result<Tenant, CanonicalError> {
        self.tenant_service
            .create_tenant(ctx, input)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_tenant(&self, ctx: &SecurityContext, id: Uuid) -> Result<Tenant, CanonicalError> {
        self.tenant_service
            .get_tenant(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_children(
        &self,
        ctx: &SecurityContext,
        parent_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Tenant>, CanonicalError> {
        self.tenant_service
            .list_children(ctx, parent_id, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: UpdateTenantRequest,
    ) -> Result<Tenant, CanonicalError> {
        self.tenant_service
            .update_tenant(ctx, id, patch)
            .await
            .map_err(CanonicalError::from)
    }

    async fn suspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, CanonicalError> {
        self.tenant_service
            .suspend_tenant(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn unsuspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, CanonicalError> {
        self.tenant_service
            .unsuspend_tenant(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_tenant(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Tenant, CanonicalError> {
        self.tenant_service
            .delete_tenant(ctx, tenant_id)
            .await
            .map_err(CanonicalError::from)
    }

    // -----------------------------------------------------------------
    // IdpUser CRUD
    // -----------------------------------------------------------------

    async fn create_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        payload: IdpNewUser,
    ) -> Result<IdpUser, CanonicalError> {
        self.user_service
            .create_user(ctx, tenant_id, payload)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<IdpUser, CanonicalError> {
        self.user_service
            .get_user(ctx, tenant_id, user_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        self.user_service
            .list_users(ctx, tenant_id, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.user_service
            .delete_user(ctx, tenant_id, user_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
        patch: IdpUserPatch,
    ) -> Result<IdpUser, CanonicalError> {
        self.user_service
            .update_user(ctx, tenant_id, user_id, patch)
            .await
            .map_err(CanonicalError::from)
    }

    // -----------------------------------------------------------------
    // Tenant metadata
    // -----------------------------------------------------------------

    async fn get_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        type_id: GtsTypeId,
    ) -> Result<MetadataEntry, CanonicalError> {
        self.metadata_service
            .get_metadata(ctx, tenant_id, type_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn resolve_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        type_id: GtsTypeId,
    ) -> Result<Option<MetadataEntry>, CanonicalError> {
        self.metadata_service
            .resolve_metadata(ctx, tenant_id, type_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataEntry>, CanonicalError> {
        self.metadata_service
            .list_metadata(ctx, tenant_id, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn upsert_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        input: UpsertMetadataRequest,
    ) -> Result<MetadataEntry, CanonicalError> {
        self.metadata_service
            .upsert_metadata(ctx, tenant_id, input)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        type_id: GtsTypeId,
    ) -> Result<(), CanonicalError> {
        self.metadata_service
            .delete_metadata(ctx, tenant_id, type_id)
            .await
            .map_err(CanonicalError::from)
    }
}
