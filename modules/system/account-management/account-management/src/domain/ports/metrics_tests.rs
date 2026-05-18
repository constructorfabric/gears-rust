//! Unit tests for the AM observability port traits and label taxonomy.
//!
//! Kept in a sibling file (not an inline `#[cfg(test)] mod tests`) per
//! the workspace convention enforced by dylint `DE1101`. The tests pin:
//!
//! * Every port trait is object-safe and `Arc<dyn Trait>`-coercible.
//! * Closed-set label enums map to the literal strings the legacy
//!   `emit_metric` call sites used.
//! * Sealed-newtype `From<&Failure>` bridges delegate to the source
//!   type's own `as_metric_label()` (no duplicated variant→string
//!   mapping on this side).

use super::*;
use std::sync::Arc;

/// All six traits are object-safe and shareable via `Arc<dyn ...>`
/// — this is the shape the module wire-up uses when handing
/// per-trait views to services. The assertions are pure
/// compile-time: a `?Sized` bound on `T` fails to monomorphise if
/// the trait is not object-safe.
#[test]
fn ports_are_object_safe_and_shareable() {
    const fn assert_object_safe<T: ?Sized>() {}
    assert_object_safe::<dyn BootstrapMetricsPort>();
    assert_object_safe::<dyn ConversionMetricsPort>();
    assert_object_safe::<dyn DependencyMetricsPort>();
    assert_object_safe::<dyn IntegrityMetricsPort>();
    assert_object_safe::<dyn MetadataMetricsPort>();
    assert_object_safe::<dyn TenantMetricsPort>();
    assert_object_safe::<dyn StorageMetricsPort>();

    // And confirm the runtime coercion `Arc<NoopMetrics> →
    // Arc<dyn Trait>` actually compiles — that's the shape DI
    // uses.
    let noop: Arc<NoopMetrics> = Arc::new(NoopMetrics);
    let bootstrap: Arc<dyn BootstrapMetricsPort> = noop;
    bootstrap.bootstrap_lifecycle(BootstrapPhase::Completed, BootstrapOutcome::Success, None);
}

#[test]
fn enum_label_strings_are_stable() {
    // Spot-check: enum label strings match the literal values used
    // at existing call sites (catalog ground-truth from
    // domain/metrics.rs constants and the emit_metric grep).
    assert_eq!(BootstrapPhase::IdpPrecheck.as_str(), "idp_precheck");
    assert_eq!(
        BootstrapOutcome::DeferredToReaper.as_str(),
        "deferred_to_reaper"
    );
    assert_eq!(ConversionOp::ExpirePending.as_str(), "expire_pending");
    assert_eq!(DependencyTarget::ResourceGroup.as_str(), "resource_group");
    assert_eq!(
        DependencyOp::RegisterUserGroupType.as_str(),
        "register_user_group_type"
    );
    assert_eq!(HierarchyDepthMode::Strict.as_str(), "strict");
    assert_eq!(HierarchyDepthOutcome::Warn.as_str(), "warn");
    assert_eq!(
        IntegrityRunOutcome::SkippedInProgress.as_str(),
        "skipped_in_progress"
    );
    assert_eq!(IntegrityPhase::Repair.as_str(), "repair");
    assert_eq!(IntegrityBucket::Deferred.as_str(), "deferred");
    assert_eq!(
        IntegrityLockEvent::EvictedBySweep.as_str(),
        "evicted_by_sweep"
    );
    assert_eq!(
        TenantRetentionJob::ProvisioningReaper.as_str(),
        "provisioning_reaper"
    );
    assert_eq!(SerializableRetryOutcome::Exhausted.as_str(), "exhausted");
}

#[test]
fn newtype_const_labels_match_call_site_literals() {
    assert_eq!(BootstrapClassification::FRESH.as_str(), "fresh");
    assert_eq!(
        BootstrapClassification::DEFERRED_TO_REAPER.as_str(),
        "deferred_to_reaper"
    );
    assert_eq!(DependencyOutcome::SUCCESS.as_str(), "success");
    assert_eq!(DependencyOutcome::ALREADY_GONE.as_str(), "already_gone");
    assert_eq!(
        TenantRetentionOutcome::IDP_UNCONFIRMED.as_str(),
        "idp_unconfirmed"
    );
    assert_eq!(
        TenantRetentionOutcome::TERMINAL_LOST_CLAIM.as_str(),
        "terminal_lost_claim"
    );
}

#[test]
fn newtype_from_failure_bridges_use_as_metric_label() {
    // The From impls must delegate to the SDK / domain types'
    // own `as_metric_label()` so the variant→string mapping is
    // owned by the source type, never duplicated here.
    let provision_fail = IdpProvisionFailure::Ambiguous {
        detail: String::new(),
    };
    assert_eq!(
        DependencyOutcome::from(&provision_fail).as_str(),
        provision_fail.as_metric_label(),
    );

    let deprov_fail = IdpDeprovisionFailure::NotFound {
        detail: String::new(),
    };
    assert_eq!(
        DependencyOutcome::from(&deprov_fail).as_str(),
        deprov_fail.as_metric_label(),
    );

    let user_fail = IdpUserOperationFailure::Unavailable {
        detail: String::new(),
    };
    assert_eq!(
        DependencyOutcome::from(&user_fail).as_str(),
        user_fail.as_metric_label(),
    );

    let hd_outcome = HardDeleteOutcome::Cleaned;
    assert_eq!(
        TenantRetentionOutcome::from(&hd_outcome).as_str(),
        hd_outcome.as_metric_label(),
    );
}
