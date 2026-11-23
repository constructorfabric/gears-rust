//! Principal-lifecycle hook contract.
//!
//! Other gears keep state keyed by principals and tenants that Account
//! Management owns — access grants naming a user, records owned by a
//! tenant, and so on. When AM deletes such a principal or tenant, that state
//! must go too, or it lingers as orphans pointing at an identity that no
//! longer exists. AM has no dependency on those gears, so it cannot clean up
//! after them; instead it tells them.
//!
//! A gear that wants to hear about deletions implements
//! [`PrincipalLifecycleHookV1`], publishes a
//! [`PrincipalLifecycleHookSpecV1`](crate::gts::PrincipalLifecycleHookSpecV1)
//! plugin instance to the types registry, and registers the implementation
//! in `ClientHub` as a scoped client keyed by that instance id:
//!
//! ```ignore
//! use account_management_sdk::{PrincipalLifecycleHookSpecV1, PrincipalLifecycleHookV1};
//! use toolkit::client_hub::ClientScope;
//! use toolkit::gts::PluginV1;
//!
//! let (instance_id, instance_json) =
//!     PluginV1::<PrincipalLifecycleHookSpecV1>::build_registration(
//!         "acme.core._.my_gear_cleanup.v1",
//!         "acme",
//!         0,
//!     )?;
//! registry.register(vec![instance_json]).await?;
//! ctx.client_hub().register_scoped::<dyn PrincipalLifecycleHookV1>(
//!     ClientScope::gts_id(&instance_id),
//!     hook,
//! );
//! ```
//!
//! AM resolves every registered hook each time it delivers an event and
//! calls them all; there is no vendor selection and the priority is
//! ignored. Resolution happens at delivery time, never at AM's init, so the
//! relative init order of AM and the hook's gear does not matter.
//!
//! # Delivery contract
//!
//! * **Synchronous and in-band.** AM delivers the event inside the
//!   operation that caused it and does not report that operation as
//!   finished until every hook has succeeded. A failing hook fails the
//!   operation; see [`PrincipalLifecycleHookError`] for how each failure
//!   class surfaces.
//! * **At least once.** A failed operation is retried — by the client for a
//!   user delete, by the next retention tick for a tenant hard-delete — and
//!   every hook sees the event again, including hooks that succeeded the
//!   first time. Hooks MUST be idempotent.
//! * **Cancellable.** AM awaits the hook under a time budget and drops it
//!   when the budget runs out, when the caller of the operation goes away,
//!   or when AM shuts down; the hook then stops at whatever await point it
//!   had reached, and the operation fails as retryable. Hooks MUST be
//!   cancellation-safe: leave consistent state at every await point (for
//!   instance, one transaction per step) so the retry can carry on, and
//!   spawn no work that outlives the call.
//! * **After the fact, for users.** `UserDeprovisioned` is delivered after
//!   the identity provider removed the user, so the hook never strips a user
//!   that still exists. AM delivers it only for a user it confirmed, before
//!   deleting, to belong to `tenant_id`; a delete request naming a user of
//!   another tenant delivers nothing.
//! * **Before the fact, for tenants.** `TenantHardDelete` is delivered
//!   before AM tears the tenant down, so a hook can still read anything it
//!   needs. AM does not proceed until every hook succeeded.
//! * **Already authorized.** AM has authorized the operation before it
//!   delivers the event. The `SecurityContext` passed along identifies the
//!   actor for audit only — the end user for a user delete, AM's system
//!   actor for a retention sweep — and a hook MUST NOT gate its cleanup on
//!   that actor's own permissions, which typically cannot see every scope
//!   the cleanup has to reach.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A deletion other gears must react to. `#[non_exhaustive]`: AM may deliver
/// new kinds of events; a hook MUST treat an event it does not recognise as
/// handled and return `Ok(())`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PrincipalLifecycleEvent {
    /// A user was removed from the identity provider. `user_id` is the
    /// subject id the user's tokens carried; `tenant_id` is the tenant the
    /// user belonged to.
    UserDeprovisioned { tenant_id: Uuid, user_id: Uuid },
    /// A soft-deleted tenant reached the end of its retention window and is
    /// about to be removed for good, together with every user in it. Unlike
    /// a soft delete, this cannot be undone.
    ///
    /// AM deprovisions the tenant's users as part of this teardown without
    /// delivering a `UserDeprovisioned` for each of them.
    TenantHardDelete { tenant_id: Uuid },
}

impl PrincipalLifecycleEvent {
    /// Stable `snake_case` name of the event kind, for logs and metric
    /// labels. Carries no ids.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UserDeprovisioned { .. } => "user_deprovisioned",
            Self::TenantHardDelete { .. } => "tenant_hard_delete",
        }
    }
}

/// Why a hook could not handle an event.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PrincipalLifecycleHookError {
    /// Transient — a dependency was unavailable. The operation fails as
    /// retryable (HTTP 503 for a user delete; the tenant waits for the next
    /// retention tick) and the event is delivered again on retry.
    #[error("retryable: {detail}")]
    Retryable { detail: String },
    /// Permanent — retrying will not help without operator action. The
    /// operation fails as an internal error (HTTP 500 for a user delete; the
    /// tenant is parked for an operator).
    #[error("terminal: {detail}")]
    Terminal { detail: String },
}

/// A gear's handler for AM principal-lifecycle events. See the module docs
/// for how to register one and for the delivery contract.
#[async_trait]
pub trait PrincipalLifecycleHookV1: Send + Sync + 'static {
    /// Handle one event. MUST be idempotent: the same event can arrive more
    /// than once. MUST be cancellation-safe: AM may drop the returned future
    /// at any await point (see the module docs) and deliver the event again
    /// later.
    ///
    /// # Errors
    ///
    /// [`PrincipalLifecycleHookError::Retryable`] to have the operation
    /// fail as retryable and the event delivered again;
    /// [`PrincipalLifecycleHookError::Terminal`] when a retry cannot help.
    async fn on_event(
        &self,
        ctx: &SecurityContext,
        event: &PrincipalLifecycleEvent,
    ) -> Result<(), PrincipalLifecycleHookError>;
}
