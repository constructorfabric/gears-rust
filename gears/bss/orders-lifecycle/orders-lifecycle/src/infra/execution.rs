//! Operational idempotency adapter: replay disclosure through the shared PEP.
//!
//! A matching settled record (from the advisory probe or the authoritative gate) is returned
//! only after current disclosure authority is rechecked (Foundation §2.1 step 3, §4.2; 08
//! §3.6). The stored snapshot is never rebuilt from today's order, and a create whose access
//! was lost is never replaced by a second create.
use bss_orders_lifecycle_sdk::catalog::Reason;
use toolkit_db::Db;

use crate::authz::{Action, AuthzFailure, Caller, Pep, Prefetch};
use crate::domain::idempotency::StoredResponse;
use crate::infra::storage::repo::idempotency::Replay;
use crate::infra::storage::scoped;

/// Recheck the caller's current authority to see `replay`, then return its stored response.
///
/// `target` is the request's own point prefetch for targeted operations; create replays
/// prefetch the created order and require a fresh `order × read` (the PEP's create mapping).
/// A refused create stored no order: the fresh create authorization that preceded the
/// registry probe is the disclosure check for its refusal body.
///
/// # Errors
/// The PEP's non-disclosing refusal/outage mapping; a read-store failure is unavailable.
pub async fn disclose_replay(
    pep: &Pep,
    db: &Db,
    caller: &Caller,
    action: Action,
    target: Option<&Prefetch>,
    replay: Replay,
) -> Result<StoredResponse, AuthzFailure> {
    match (action, target, replay.order_id) {
        (Action::OrderCreate, None, None) => Ok(replay.response),
        (Action::OrderCreate, None, Some(order_id)) => {
            let prefetch = scoped::prefetch(db, order_id)
                .await
                .map_err(|_| AuthzFailure::Unavailable)?;
            pep.recheck_replay_disclosure(caller, action, &prefetch)
                .await?;
            Ok(replay.response)
        }
        (Action::OrderCreate, Some(_), _) | (_, None, _) => Err(AuthzFailure::Integration),
        (_, Some(prefetch), order_id) => {
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
