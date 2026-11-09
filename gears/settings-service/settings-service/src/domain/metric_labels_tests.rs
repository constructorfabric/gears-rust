//! The finite label sets the DESIGN names (§7 *Feature Metrics*), pinned to the
//! text each is exported as: renaming one is a change to a published series.

use super::{
    AuditKind, AuthzReason, ChangeType, DeclarationSource, LookupResult, Operation, Outcome,
    ReadSource, ResolveFailure, RetentionResult, ScopeClass, ScopeKind, SecretUseResult,
    StepUpResult,
};
use crate::domain::declaration::DeclarationStatus;
use crate::domain::resolution::{scope_class, scope_kind};
use crate::domain::stepup::StepUpRefusal;

fn texts<T: Copy>(all: &[T], as_str: impl Fn(T) -> &'static str) -> Vec<&'static str> {
    all.iter().copied().map(as_str).collect()
}

#[test]
fn the_label_sets_are_the_designs_finite_sets() {
    assert_eq!(
        texts(Outcome::ALL, Outcome::as_str),
        ["committed", "rejected"]
    );
    assert_eq!(
        texts(ChangeType::ALL, ChangeType::as_str),
        ["set", "revert", "remove", "clone"]
    );
    assert_eq!(
        texts(LookupResult::ALL, LookupResult::as_str),
        ["hit", "miss"]
    );
    assert_eq!(
        texts(ReadSource::ALL, ReadSource::as_str),
        ["cache", "resolve"]
    );
    assert_eq!(
        texts(ResolveFailure::ALL, ResolveFailure::as_str),
        ["unavailable", "internal"]
    );
    assert_eq!(
        texts(AuthzReason::ALL, AuthzReason::as_str),
        ["denied", "unavailable", "constraints"]
    );
    assert_eq!(
        texts(Operation::ALL, Operation::as_str),
        ["read", "write", "create", "update", "delete", "delegate"],
        "`read_unmasked` is a probe, not an operation of the counted enforcement point"
    );
    assert_eq!(
        texts(SecretUseResult::ALL, SecretUseResult::as_str),
        ["allowed", "denied"]
    );
    assert_eq!(
        texts(AuditKind::ALL, AuditKind::as_str),
        ["mutation", "secret_use"]
    );
    assert_eq!(
        texts(RetentionResult::ALL, RetentionResult::as_str),
        ["ok", "failed"]
    );
}

#[test]
fn the_scope_and_inventory_label_sets_are_the_designs() {
    assert_eq!(
        texts(ScopeClass::ALL, ScopeClass::as_str),
        ["global", "cascading", "local"]
    );
    assert_eq!(
        texts(ScopeKind::ALL, ScopeKind::as_str),
        ["platform", "tenant"]
    );
    assert_eq!(
        texts(DeclarationSource::ALL, DeclarationSource::as_str),
        ["admin_authored", "module_contributed"]
    );
}

#[test]
fn the_label_sets_speak_the_stored_words() {
    // A stored column and the metric's label are one word: the typed set and
    // the constants the rest of the domain matches on must not drift apart.
    assert_eq!(ScopeClass::Global.as_str(), scope_class::GLOBAL);
    assert_eq!(ScopeClass::Cascading.as_str(), scope_class::CASCADING);
    assert_eq!(ScopeClass::Local.as_str(), scope_class::LOCAL);
    assert_eq!(ScopeKind::Platform.as_str(), scope_kind::PLATFORM);
    assert_eq!(ScopeKind::Tenant.as_str(), scope_kind::TENANT);
    for class in ScopeClass::ALL {
        assert_eq!(ScopeClass::parse(class.as_str()), Some(*class));
    }
    assert_eq!(ScopeClass::parse("elsewhere"), None);
    assert_eq!(DeclarationStatus::Active.as_str(), "active");
    assert_eq!(DeclarationStatus::Retired.as_str(), "retired");
}

#[test]
fn every_refusal_code_is_a_step_up_result_label() {
    // A refusal's own code is the `result` value; one added without a label
    // breaks the match in `From<&StepUpRefusal>`, and one renamed breaks here.
    let every = [
        StepUpRefusal::Missing,
        StepUpRefusal::NotConfigured,
        StepUpRefusal::Unavailable(String::new()),
        StepUpRefusal::Signature(String::new()),
        StepUpRefusal::SubjectMismatch,
        StepUpRefusal::AuthTimeMissing,
        StepUpRefusal::Stale,
        StepUpRefusal::Assurance,
        StepUpRefusal::Claims(String::new()),
    ];
    for refusal in &every {
        assert_eq!(StepUpResult::from(refusal).as_str(), refusal.code());
    }
    assert_eq!(StepUpResult::Verified.as_str(), "verified");
    assert_eq!(StepUpResult::ServicePrincipal.as_str(), "service_principal");
    assert_eq!(
        StepUpResult::ALL.len(),
        every.len() + 2,
        "a result that is neither a refusal nor the two outcomes"
    );
}
