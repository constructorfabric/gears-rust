//! The one deadline every cross-gear `CatalogVersion` call is made under.
//!
//! `CatalogVersionRegistryV1` is the gear's only synchronous dependency on
//! another gear, and its own contract admits the registry may be `unreachable`.
//! `request_version` is the half that awaits inside write transactions and is
//! what the seam below wraps; `committed_version` is awaited once, from the
//! read-model warm sweep, which takes [`deployment_budget`] and renders an
//! elapsed call its own way. "Unreachable" in a trait doc is a returned error; on a wire it
//! is also a peer that accepted the call and never answered, and nothing in a
//! `Result` distinguishes the two. Ten of the awaits sit inside an open write
//! transaction, so an unanswering peer pins a transaction, its row locks and a
//! pool connection on every mutating path at once — the publish commit, the
//! window mutation, the cutover, the retirement, the supersession, the
//! grandfathering flip, the membership moves, the approval commit and the
//! repricing apply.
//!
//! The deployment's budget is resolved once at init and read here, rather than
//! threaded through the eight service constructors and the free functions
//! beneath them. Threading it costs a third parameter on every one of those
//! constructors and on every test that builds one, and buys the ability to give
//! two callers different budgets — which is exactly the drift a single seam
//! exists to prevent, since a deployment has one registry.

use std::sync::OnceLock;
use std::time::Duration;

use toolkit_security::SecurityContext;

use crate::config::DEFAULT_REGISTRY_CALL_TIMEOUT_SECS;
use crate::domain::error::DomainError;
use crate::domain::ports::{CatalogVersionRegistryV1, PendingVersionRef, registry_failure};

/// Set once by `init`, read by [`deployment_budget`].
static BUDGET: OnceLock<Duration> = OnceLock::new();

/// Adopt the deployment's configured budget.
///
/// Idempotent and first-writer-wins, because a process has one configuration:
/// a second `init` in the same process — the shape every in-process test harness
/// has — must not be able to move the budget under a call already in flight.
pub(crate) fn adopt_deployment_budget(budget: Duration) {
    if BUDGET.set(budget).is_err() {
        tracing::debug!(
            budget_ms = budget.as_millis(),
            "bss-pricing: the registry call budget was already adopted; the first stands"
        );
    }
}

/// The budget in force for this process.
///
/// Falls back to [`DEFAULT_REGISTRY_CALL_TIMEOUT_SECS`] rather than to "no
/// budget": a caller reached from a test harness or a unit that never ran `init`
/// still has to be bounded, because an unbounded await is the failure this
/// module exists to remove and a missing config is not a licence to reinstate it.
#[must_use]
pub(crate) fn deployment_budget() -> Duration {
    BUDGET
        .get()
        .copied()
        .unwrap_or(Duration::from_secs(DEFAULT_REGISTRY_CALL_TIMEOUT_SECS))
}

/// Every cross-gear call to the catalog-version registry goes through here.
///
/// **Why one seam and not twelve `timeout(..)` calls:** the gear awaits
/// `request_version` from twelve places, ten of them inside an open write
/// transaction, so a hung peer pins a transaction, its row locks and a pool
/// connection on every mutating path at once. A budget applied at the call
/// sites drifts; applied here it cannot.
///
/// # Errors
/// Whatever [`registry_failure`] makes of the registry's own refusal, and
/// [`DomainError::CatalogVersionUnavailable`] when the budget expires first —
/// the same fail-closed answer, and the same 503, as a registry that answered
/// "unreachable". The expiry is deliberately not its own variant: from the
/// caller's side a peer that said nothing and a peer that said "I am down" are
/// one state with one remedy, and the retry is safe because the request id is
/// derived from the subject rather than minted, so a retry re-claims the same
/// handle instead of stranding a second one.
pub(crate) async fn request_version_within(
    registry: &dyn CatalogVersionRegistryV1,
    ctx: &SecurityContext,
    request_id: &str,
    budget: Duration,
) -> Result<PendingVersionRef, DomainError> {
    match tokio::time::timeout(budget, registry.request_version(ctx, request_id)).await {
        Ok(answer) => answer.map_err(|err| {
            let failure = registry_failure(err);
            // Reported here and not by `infra::error_mapping`'s 503 arm, which sees
            // neither the registry's own sentence nor this request's id: the
            // rendered answer carries no detail at all, so this is the only record
            // of *why* the peer refused. A `Rejected` answer is deliberately not
            // logged — it is a 400 whose detail travels to the caller, who is the
            // party that can act on it.
            if let DomainError::CatalogVersionUnavailable(detail) = &failure {
                tracing::error!(
                    request_id = %request_id,
                    detail = %detail,
                    "bss-pricing: the catalog-version registry is unavailable; the transaction \
                     rolls back and the deterministic request id lets the retry re-claim the \
                     same handle"
                );
            }
            failure
        }),
        Err(_elapsed) => {
            tracing::warn!(
                request_id = %request_id,
                budget_ms = budget.as_millis(),
                "bss-pricing: the catalog-version registry did not answer inside its budget; \
                 the transaction rolls back and the deterministic request id lets the retry \
                 re-claim the same handle"
            );
            Err(DomainError::CatalogVersionUnavailable(format!(
                "the catalog version registry did not answer within {}ms",
                budget.as_millis()
            )))
        }
    }
}

/// The seam under the process budget — what every production call site uses.
///
/// A second function rather than a default argument because `budget` stays
/// explicit in [`request_version_within`]: a test that wants a one-millisecond
/// deadline must be able to state one without touching a process-wide value the
/// rest of the binary shares.
///
/// # Errors
/// As [`request_version_within`].
pub(crate) async fn request_version_now(
    registry: &dyn CatalogVersionRegistryV1,
    ctx: &SecurityContext,
    request_id: &str,
) -> Result<PendingVersionRef, DomainError> {
    request_version_within(registry, ctx, request_id, deployment_budget()).await
}

#[cfg(test)]
#[path = "registry_deadline_tests.rs"]
mod registry_deadline_tests;
