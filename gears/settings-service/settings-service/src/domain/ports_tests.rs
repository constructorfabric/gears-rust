//! The metrics ports: the test binding of every port, and the finite label
//! sets the DESIGN names (§7 *Feature Metrics*).

use std::sync::Arc;

use super::{
    LifecycleMetrics, NoMetrics, ReadMetrics, SecurityMetrics, StepUpOperation, WriteMetrics,
};
use crate::domain::declaration::DeclarationStatus;
use crate::domain::metric_labels::{
    AuditKind, AuthzReason, ChangeType, DeclarationSource, LookupResult, Operation, Outcome,
    ReadSource, ResolveFailure, RetentionResult, ScopeClass, ScopeKind, SecretUseResult,
    StepUpResult,
};

/// Compiles only for a type that is every metrics port at once.
fn is_every_port<T: WriteMetrics + LifecycleMetrics + ReadMetrics + SecurityMetrics>() {}

#[test]
fn the_test_binding_implements_every_metrics_port_and_counts_nothing() {
    is_every_port::<NoMetrics>();

    // Each port is object-safe, so a binding can be handed around as the
    // trait object the services hold.
    let write: Arc<dyn WriteMetrics> = Arc::new(NoMetrics);
    let lifecycle: Arc<dyn LifecycleMetrics> = Arc::new(NoMetrics);
    let read: Arc<dyn ReadMetrics> = Arc::new(NoMetrics);
    let security: Arc<dyn SecurityMetrics> = Arc::new(NoMetrics);

    write.value_write(Outcome::Committed);
    write.step_up(StepUpOperation::Set, StepUpResult::Verified);
    write.purge_released();
    write.purge_release_failed();
    write.change(ChangeType::Set, Outcome::Committed);
    lifecycle.needs_review(DeclarationSource::AdminAuthored, 1);
    lifecycle.retention_pass(RetentionResult::Ok, 0);
    lifecycle.service_up(true);
    lifecycle.declarations(
        DeclarationSource::AdminAuthored,
        DeclarationStatus::Active,
        1,
    );
    lifecycle.values(ScopeClass::Global, ScopeKind::Platform, 1);
    read.lookup(LookupResult::Hit, ScopeClass::Global);
    read.read(ReadSource::Cache, ScopeClass::Global, 0.001);
    read.resolve_failure(ResolveFailure::Unavailable);
    security.authz_denial(Operation::Read, AuthzReason::Denied);
    security.secret_use(SecretUseResult::Allowed);
    security.audit_event(AuditKind::Mutation);
}
