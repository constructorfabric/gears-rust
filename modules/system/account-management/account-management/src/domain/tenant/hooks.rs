//! Cascade-hook contract for the hard-delete pipeline.
//!
//! Sibling AM features (future 2.6 user-groups, 2.7 tenant-metadata) own
//! rows keyed by `tenant_id` and MUST clean them up before AM deletes
//! the tenant itself. They register a [`TenantHardDeleteHook`] via
//! [`crate::AccountManagementModule::register_hard_delete_hook`] at
//! startup; the hard-delete pipeline invokes every hook (in registration
//! order) before the `IdP` deprovision call and before the final DB
//! teardown.
//!
//! Failure semantics are explicit:
//!
//! * [`HookError::Retryable`] defers the tenant to the next retention
//!   tick. The pipeline does NOT touch the `IdP` or the DB rows when any
//!   hook returns `Retryable`.
//! * [`HookError::Terminal`] skips the tenant for this tick and emits a
//!   structured audit record; an operator must intervene. Subsequent
//!   ticks will retry (the row is still
//!   `status = Deleted AND deleted_at IS NOT NULL`), but the
//!   pipeline will not make progress until the hook itself succeeds.

use std::sync::Arc;

use futures::future::BoxFuture;
use modkit_macros::domain_model;
use uuid::Uuid;

/// Outcome returned by a cascade hook.
#[domain_model]
#[derive(Debug, Clone)]
pub enum HookError {
    /// Transient — the pipeline defers the tenant to the next tick.
    Retryable { detail: String },
    /// Terminal — operator intervention required. The pipeline skips the
    /// tenant for this tick and emits an audit record.
    Terminal { detail: String },
}

/// Signature of a cascade hook. Hooks are `Fn(Uuid) -> BoxFuture` so
/// callers can construct closures that capture their own state cheaply.
///
/// Hooks run **outside** any DB transaction (per DESIGN §3.5 hard-delete
/// flow); they MAY open their own transactions internally. They MUST NOT
/// open a transaction that holds `tenants` locks at exit — the pipeline
/// opens its own transaction immediately after the hooks complete.
pub type TenantHardDeleteHook =
    Arc<dyn Fn(Uuid) -> BoxFuture<'static, Result<(), HookError>> + Send + Sync>;
