//! Runtime helpers consumed by `#[toolkit::provides]`.
//!
//! Kept separate from the proc-macro crate so the generated code references
//! stable, version-controlled host APIs rather than re-importing them from
//! `toolkit_contract`. Provider gears don't import these directly — the
//! macro emits paths like `::toolkit::wiring::read_wiring(...)`.

use std::sync::Arc;

use toolkit_contract::policy::{PolicyStack, TracingPolicy};
use toolkit_contract::wiring::ClientWiring;

use crate::context::GearCtx;

/// Read the [`ClientWiring`] for a single provided contract from the
/// module's config section.
///
/// Path: `gears.<gear>.config.client_wiring.<key>`. If `client_wiring`
/// or the contract `key` is absent, returns [`ClientWiring::Local`] —
/// gears whose only provided contract has a local-only deployment can
/// run with no config at all.
///
/// `key` is the `snake_case` form of the contract trait identifier (e.g.
/// `payment_api` for `PaymentApi`); the macro takes care of casing.
///
/// # Errors
/// Returns a context-bearing `anyhow::Error` if `client_wiring.<key>` is
/// present but cannot be deserialized into a [`ClientWiring`].
pub fn read_wiring(ctx: &GearCtx, key: &str) -> anyhow::Result<ClientWiring> {
    let raw = ctx.raw_config();
    let Some(section) = raw.get("client_wiring") else {
        return Ok(ClientWiring::Local);
    };
    let Some(per_contract) = section.get(key) else {
        return Ok(ClientWiring::Local);
    };
    serde_json::from_value::<ClientWiring>(per_contract.clone()).map_err(|e| {
        anyhow::anyhow!(
            "gear `{gear}`: invalid client_wiring.{key}: {e}",
            gear = ctx.gear_name()
        )
    })
}

/// Default [`PolicyStack`] applied to in-process local clients built by
/// `#[toolkit::provides]`. Contains [`TracingPolicy`] only; richer stacks
/// can be opted into via the `policies = [...]` macro argument.
#[must_use]
pub fn default_policy_stack() -> Arc<PolicyStack> {
    let mut s = PolicyStack::new();
    s.push(Arc::new(TracingPolicy));
    Arc::new(s)
}

/// Build a [`PolicyStack`] from a list of already-constructed policy
/// instances (the macro emits `Box::new(Policy::default())` per entry
/// when the user passes `policies = [...]`).
#[must_use]
pub fn policy_stack_from(policies: Vec<Arc<dyn toolkit_contract::policy::Policy>>) -> Arc<PolicyStack> {
    let mut s = PolicyStack::new();
    for p in policies {
        s.push(p);
    }
    Arc::new(s)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "wiring_tests.rs"]
mod tests;
