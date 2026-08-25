//! Tests for the one deadline every cross-gear `CatalogVersion` call is made
//! under.
//!
//! Armed against the failure the seam exists to remove: a registry that accepted
//! the call and never answers. That is not a `Result` the double can return — it
//! is the *absence* of one — so the double parks, and what is asserted is that
//! the caller comes back at all, with the fail-closed answer.

use std::time::Duration;

use async_trait::async_trait;
use bss_pricing_sdk::catalog_version::CatalogVersion;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use super::{deployment_budget, request_version_within};
use crate::config::{DEFAULT_REGISTRY_CALL_TIMEOUT_SECS, MAX_REGISTRY_CALL_TIMEOUT_SECS};
use crate::domain::error::DomainError;
use crate::domain::ports::{
    CatalogVersionRegistryV1, PendingVersionRef, registry_unreachable, unconfigured_registry,
};

/// The seam never reads the context, so any context serves.
fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

/// A registry that accepted the call and answers nothing.
///
/// The park is an hour and the budget every case gives it is a millisecond, so
/// the timeout is what completes the future and the sleep is dropped unpolled —
/// the case costs no wall time. It parks rather than returning
/// `registry_unreachable`, because an error is the case the gear already handled;
/// what it could not survive was silence.
struct SilentRegistry;

#[async_trait]
impl CatalogVersionRegistryV1 for SilentRegistry {
    async fn request_version(
        &self,
        _ctx: &SecurityContext,
        _request_id: &str,
    ) -> Result<PendingVersionRef, CanonicalError> {
        tokio::time::sleep(Duration::from_hours(1)).await;
        Err(unconfigured_registry())
    }

    async fn committed_version(
        &self,
        _ctx: &SecurityContext,
        _pending_ref: &str,
    ) -> Result<Option<CatalogVersion>, CanonicalError> {
        tokio::time::sleep(Duration::from_hours(1)).await;
        Err(unconfigured_registry())
    }
}

/// A registry that answers inside any budget.
struct PromptRegistry;

#[async_trait]
impl CatalogVersionRegistryV1 for PromptRegistry {
    async fn request_version(
        &self,
        _ctx: &SecurityContext,
        request_id: &str,
    ) -> Result<PendingVersionRef, CanonicalError> {
        Ok(PendingVersionRef {
            request_id: request_id.to_owned(),
            pending_ref: format!("pending-{request_id}"),
        })
    }

    async fn committed_version(
        &self,
        _ctx: &SecurityContext,
        _pending_ref: &str,
    ) -> Result<Option<CatalogVersion>, CanonicalError> {
        Ok(None)
    }
}

/// A registry that is down, and says so inside the budget.
struct DownRegistry;

#[async_trait]
impl CatalogVersionRegistryV1 for DownRegistry {
    async fn request_version(
        &self,
        _ctx: &SecurityContext,
        _request_id: &str,
    ) -> Result<PendingVersionRef, CanonicalError> {
        Err(registry_unreachable("10.0.0.7 refused the connection"))
    }

    async fn committed_version(
        &self,
        _ctx: &SecurityContext,
        _pending_ref: &str,
    ) -> Result<Option<CatalogVersion>, CanonicalError> {
        Err(registry_unreachable("10.0.0.7 refused the connection"))
    }
}

#[tokio::test]
async fn a_registry_that_never_answers_is_the_fail_closed_answer_and_not_a_hang() {
    let answer = request_version_within(
        &SilentRegistry,
        &ctx(),
        "req-silent",
        Duration::from_millis(1),
    )
    .await;

    // The variant, not `is_err()`: a refusal the registry *made* is a permanent
    // 400 and a caller must not retry it, so a case that accepted any error
    // would pass on the one answer this seam must never give.
    match answer {
        Err(DomainError::CatalogVersionUnavailable(detail)) => assert!(
            detail.contains("1ms"),
            "the refusal names the budget it exceeded, because that is the knob an operator \
             moves: {detail}"
        ),
        other => panic!("a silent registry must fail closed and retriably, got {other:?}"),
    }
}

#[tokio::test]
async fn a_registry_that_answers_inside_the_budget_is_passed_through_untouched() {
    let pending = request_version_within(
        &PromptRegistry,
        &ctx(),
        "req-prompt",
        Duration::from_millis(1),
    )
    .await
    .expect("a prompt registry answers");

    // The positive control. Without it the case above is satisfied by a seam
    // that refuses everything, which is a deadline of zero.
    assert_eq!(pending.request_id, "req-prompt");
    assert_eq!(pending.pending_ref, "pending-req-prompt");
}

#[tokio::test]
async fn the_registrys_own_outage_keeps_its_own_projection() {
    let answer =
        request_version_within(&DownRegistry, &ctx(), "req-down", Duration::from_millis(1)).await;

    // The seam is not allowed to relabel what the registry said. `registry_failure`
    // is the sole producer of the gear's rejection vocabulary, and a seam that
    // rebuilt the answer itself would be a second one.
    match answer {
        Err(DomainError::CatalogVersionUnavailable(detail)) => assert!(
            !detail.contains("did not answer within"),
            "an answered outage must not be reported as a lapsed budget: {detail}"
        ),
        other => panic!("an unreachable registry fails closed and retriably, got {other:?}"),
    }
}

#[test]
fn a_process_that_adopted_no_budget_is_still_bounded() {
    // The fallback, and it is the point: a unit or a harness that never ran
    // `init` still gets a deadline, because an unbounded await is the failure
    // this module removes and a missing config is not a licence to reinstate it.
    //
    // The **value** and not a range. Any legal adopted budget satisfies
    // `> ZERO && <= MAX`, so a range makes this case pass on a binary where a
    // neighbour booted a gear and `adopt_deployment_budget` won the `OnceLock`
    // first — measuring that some budget is in force, which is not what the name
    // claims. Should that day come this goes red and names both numbers, which is
    // the outcome worth having: the test's own premise moved.
    assert_eq!(
        deployment_budget(),
        Duration::from_secs(DEFAULT_REGISTRY_CALL_TIMEOUT_SECS),
        "an unadopted process runs on the compiled-in default and on nothing else"
    );
    const {
        assert!(DEFAULT_REGISTRY_CALL_TIMEOUT_SECS <= MAX_REGISTRY_CALL_TIMEOUT_SECS);
    }
}
