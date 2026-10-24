//! Operational idempotency adapter: replay disclosure through the shared PEP.
//!
//! A matching settled record (from the advisory probe or the authoritative gate) is returned
//! only after current disclosure authority is rechecked (Foundation §2.1 step 3, §4.2; 08
//! §3.6). The stored snapshot is never rebuilt from today's order, and a create whose access
//! was lost is never replaced by a second create.
use bss_orders_sdk::catalog::Reason;
use toolkit_db::Db;

use crate::authz::{Action, AuthzFailure, Caller, Pep, Prefetch};
use crate::domain::idempotency::StoredResponse;
use crate::infra::engine::errors::is_temporary_store_failure;
use crate::infra::storage::repo::idempotency::Replay;
use crate::infra::storage::scoped;

/// Which replay is disclosed: a create, which has no target, or a targeted operation with the
/// request's own point prefetch. A create with a target or a targeted action without one cannot
/// be expressed.
#[derive(Debug, Clone, Copy)]
pub enum ReplayTarget<'a> {
    /// A create replay; disclosure prefetches the created order.
    Create,
    /// A targeted replay of `action`, rechecked on the request's own point prefetch.
    Order {
        /// The requested action, rechecked on current facts.
        action: Action,
        /// The request's own point prefetch.
        prefetch: &'a Prefetch,
    },
}

/// Recheck the caller's current authority to see `replay`, then return its stored response.
///
/// A targeted replay rechecks its own action on the request's point prefetch; a create replay
/// prefetches the created order and requires a fresh `order × read` (the PEP's create mapping).
/// A refused create stored no order: the fresh create authorization that preceded the
/// registry probe is the disclosure check for its refusal body.
///
/// # Errors
/// The PEP's non-disclosing refusal/outage mapping; a temporary read-store failure is
/// unavailable, a permanent one fails closed as an integration failure (logged either way).
pub async fn disclose_replay(
    pep: &Pep,
    db: &Db,
    caller: &Caller,
    target: ReplayTarget<'_>,
    replay: Replay,
) -> Result<StoredResponse, AuthzFailure> {
    match (target, replay.order_id) {
        (ReplayTarget::Create, None) => Ok(replay.response),
        (ReplayTarget::Create, Some(order_id)) => {
            let prefetch = scoped::prefetch(db, order_id)
                .await
                .map_err(|e| prefetch_failure(&e))?;
            pep.recheck_replay_disclosure(caller, Action::OrderCreate, &prefetch)
                .await?;
            Ok(replay.response)
        }
        (ReplayTarget::Order { action, prefetch }, order_id) => {
            // The fingerprint binds the target; a record of another order is never disclosed.
            if order_id.is_some_and(|id| id != prefetch.order_id()) {
                return Err(AuthzFailure::Refused {
                    reason: Reason::OrderNotFound,
                    proof: None,
                });
            }
            pep.recheck_replay_disclosure(caller, action, prefetch)
                .await?;
            Ok(replay.response)
        }
    }
}

/// Log a create-replay prefetch failure and classify it: only a temporary store outage is a
/// retryable 503; a permanent one (missing grant, schema drift) fails closed. `AuthzFailure`
/// has no internal variant, so the fail-closed 500 is its integration outcome.
fn prefetch_failure(error: &anyhow::Error) -> AuthzFailure {
    let outcome = if is_temporary_store_failure(error) {
        AuthzFailure::Unavailable
    } else {
        AuthzFailure::Integration
    };
    tracing::warn!(
        target: "orders.engine.store",
        operation = "create",
        outcome = ?outcome,
        error = %error,
        "replay disclosure prefetch failed"
    );
    outcome
}
