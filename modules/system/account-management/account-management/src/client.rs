//! Newtype adapter that implements
//! [`account_management_sdk::AccountManagementClient`] by delegating to
//! the impl-side [`TenantService`] and [`UserService`].
//!
//! Construction happens once in
//! [`crate::module::AccountManagementModule::init`] (or its bootstrap
//! sibling) and the resulting `Arc<dyn AccountManagementClient>` is
//! registered in `ClientHub` so every external consumer resolves
//! through the SDK trait, never the impl service directly.
//!
//! The adapter is a **thin shim**:
//!
//! * Forwards every call 1:1 to the underlying service.
//! * Maps every internal `DomainError` to `AccountManagementError` via the
//!   `From<DomainError> for AccountManagementError` impl in
//!   `infra::sdk_error_mapping`. The REST handler (when it lands)
//!   lifts further to `modkit_canonical_errors::CanonicalError` via
//!   the `account_management_error_to_canonical` helper in the same
//!   module. No new error vocabulary is introduced at this boundary.
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

use account_management_sdk::AccountManagementError;
use account_management_sdk::client::AccountManagementClient;
use account_management_sdk::idp_user::{IdpNewUser, IdpUser, ListUsersQuery};
use account_management_sdk::metadata::{MetadataEntry, UpsertMetadataRequest};
use account_management_sdk::tenant::{CreateTenantRequest, Tenant, UpdateTenantRequest};
use async_trait::async_trait;
use gts::GtsSchemaId;
use modkit_odata::{ODataQuery, Page};
use modkit_security::SecurityContext;
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
    reason = "AM-internal `*_service` suffix on every field is the established convention across `module.rs` / `client.rs` / `TenantService::with_*` — stripping it would lose the obvious one-to-one mapping from `service` field to backing `Arc<XxxService>`"
)]
pub struct AccountManagementClientImpl<R: TenantRepo> {
    tenant_service: Arc<TenantService<R>>,
    user_service: Arc<UserService>,
    metadata_service: Arc<MetadataService>,
}

impl<R: TenantRepo> AccountManagementClientImpl<R> {
    /// Build the adapter from the three already-constructed services.
    /// `module.rs` owns the wiring; this constructor just hooks them
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
    ) -> Result<Tenant, AccountManagementError> {
        self.tenant_service
            .create_tenant(ctx, input)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn get_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, AccountManagementError> {
        self.tenant_service
            .get_tenant(ctx, id)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn list_children(
        &self,
        ctx: &SecurityContext,
        parent_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Tenant>, AccountManagementError> {
        self.tenant_service
            .list_children(ctx, parent_id, query)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn update_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: UpdateTenantRequest,
    ) -> Result<Tenant, AccountManagementError> {
        self.tenant_service
            .update_tenant(ctx, id, patch)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn suspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, AccountManagementError> {
        self.tenant_service
            .suspend_tenant(ctx, id)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn unsuspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, AccountManagementError> {
        self.tenant_service
            .unsuspend_tenant(ctx, id)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn delete_tenant(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Tenant, AccountManagementError> {
        self.tenant_service
            .delete_tenant(ctx, tenant_id)
            .await
            .map_err(AccountManagementError::from)
    }

    // -----------------------------------------------------------------
    // IdpUser CRUD
    // -----------------------------------------------------------------

    async fn create_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        payload: IdpNewUser,
    ) -> Result<IdpUser, AccountManagementError> {
        self.user_service
            .create_user(ctx, tenant_id, payload)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn get_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<IdpUser, AccountManagementError> {
        self.user_service
            .get_user(ctx, tenant_id, user_id)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, AccountManagementError> {
        self.user_service
            .list_users(ctx, tenant_id, query)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn delete_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), AccountManagementError> {
        self.user_service
            .delete_user(ctx, tenant_id, user_id)
            .await
            .map_err(AccountManagementError::from)
    }

    // -----------------------------------------------------------------
    // Tenant metadata
    // -----------------------------------------------------------------

    async fn get_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<MetadataEntry, AccountManagementError> {
        self.metadata_service
            .get_metadata(ctx, tenant_id, schema_id)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn resolve_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<Option<MetadataEntry>, AccountManagementError> {
        self.metadata_service
            .resolve_metadata(ctx, tenant_id, schema_id)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn list_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataEntry>, AccountManagementError> {
        self.metadata_service
            .list_metadata(ctx, tenant_id, query)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn upsert_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        input: UpsertMetadataRequest,
    ) -> Result<MetadataEntry, AccountManagementError> {
        self.metadata_service
            .upsert_metadata(ctx, tenant_id, input)
            .await
            .map_err(AccountManagementError::from)
    }

    async fn delete_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<(), AccountManagementError> {
        self.metadata_service
            .delete_metadata(ctx, tenant_id, schema_id)
            .await
            .map_err(AccountManagementError::from)
    }
}
