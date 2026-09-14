//! Tenant-deprovision cascade port.
//!
//! The tenant-deprovision saga must delete every machine credential the
//! tenant owns before the tenant itself goes away, or live
//! `client_credentials` secrets outlive their tenant. Module wiring constructs
//! [`ServiceAccountFacade`] first and passes this port into [`TenantFacade`]'s
//! constructor, so the cascade cannot be omitted.
//!
//! Depending on the concrete type would reintroduce the
//! `tenant_facade` → `service_account_facade` edge this port exists to
//! remove. The port also keeps the cascade independently testable: tenant-
//! facade tests inject a recorder without standing up the concrete service-
//! account facade and its Keycloak admin dependencies.
//!
//! [`ServiceAccountFacade`]: crate::domain::service_account_facade::ServiceAccountFacade
//! [`TenantFacade`]: crate::domain::tenant_facade::TenantFacade

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::PluginError;

/// Deletes every service account owned by a tenant being deprovisioned.
#[async_trait]
pub trait TenantAccountPurgeHook: Send + Sync {
    /// Delete every service account owned by `tenant_id`, returning how many
    /// were removed.
    ///
    /// # Errors
    ///
    /// Implementors MUST tolerate a concurrent delete (the account is gone
    /// either way) and MUST propagate everything else, so the deprovision
    /// caller retries rather than reporting a tenant torn down while live
    /// credentials survive.
    async fn purge_tenant_accounts(&self, tenant_id: Uuid) -> Result<usize, PluginError>;
}
