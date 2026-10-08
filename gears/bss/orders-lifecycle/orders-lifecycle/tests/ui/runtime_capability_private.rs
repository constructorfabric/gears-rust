// D-184 privacy gate on the RUNTIME capability module (reported in the privacy pass, so kept
// separate from type-mismatch cases). Neither capability exposes or accepts a raw scope.
#![allow(dead_code, unused)]
#[path = "../../src/infra/storage/entity/mod.rs"]
mod entity;
#[path = "../../src/infra/maintenance/scope.rs"]
mod scope;
use scope::{DiscoveryScope, TargetScope};
use toolkit_security::AccessScope;

// A discovered target cannot be widened by replacing its private scope.
fn widen(existing: &TargetScope) -> TargetScope {
    TargetScope {
        scope: AccessScope::deny_all(),
        ..existing.clone()
    }
}
fn main() {}
