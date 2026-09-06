// Created: 2026-08-12 by Constructor Tech
//! Deployment-owned bootstrap configuration for the settings-service gear.
//!
//! # Why this struct has no `Default`
//!
//! Every other gear in this repo reads its configuration with
//! `ctx.config_or_default()`, backed by a `Default` impl. This one deliberately
//! does not, and the difference is the point.
//!
//! This service is where the platform's settings live. If it started with an
//! invented value for how it verifies step-up credentials, the failure would not
//! look like a failure — the gear would come up healthy and enforce the wrong
//! thing. Bootstrap values are deployment-owned and are never themselves managed
//! settings, so there is no scope to resolve them from and nothing to fall back
//! to. An absent required value is a deployment error, and the gear says so at
//! startup rather than serving traffic on a guess.
//!
//! Fields that carry a value fixed by the design, rather than by the deployment,
//! may default — [`SettingsServiceConfig::cache_ttl_seconds`] is the only one.
//!
//! # Step-up configuration is not here yet
//!
//! Step-up is gated per declaration (`requires_step_up`, default required —
//! `cpt-cf-settings-service-fr-service-writes`) and enforced on interactive writes
//! and behavior-affecting declaration actions (`cpt-cf-settings-service-fr-authn-role-gating`).
//! DESIGN.md §4.2 *Value Writer* fixes the R1 check: the presented token's
//! signature against the identity provider's JWKS, `sub` matching the session, and
//! `auth_time` within the step-up freshness window — a binding behind a
//! ClientHub-resolved `StepUpVerifier` port owned by this gear. Two values are
//! deployment configuration and, per DESIGN.md §4.9, load at gear init: the
//! provider's **JWKS endpoint**, and the **freshness window**, which
//! `cpt-cf-settings-service-fr-validate-before-set` makes deployment-configured
//! and DESIGN caps at five minutes. Both arrive with the verifier binding in
//! DECOMPOSITION entry 2.8; declaring them before anything reads them would be
//! configuration nobody can validate.

use serde::Deserialize;

/// The cache backstop that bounds staleness when an invalidation broadcast is
/// missed. Fixed by DESIGN.md §4.2 *Cache & Invalidation*, not by the operator.
const DEFAULT_CACHE_TTL_SECONDS: u64 = 30;

/// Bootstrap configuration, read once at gear init.
///
/// `deny_unknown_fields` is deliberate: a mistyped key in a deployment file
/// would otherwise be silently ignored, leaving the operator believing they had
/// configured something they had not.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsServiceConfig {
    /// Upper bound on how long a cached effective value may be served after a
    /// missed invalidation broadcast, in seconds.
    ///
    /// Defaults to 30. Unlike the two above, this is a design-fixed backstop
    /// rather than a deployment decision, so a default is a real answer instead
    /// of a guess.
    #[serde(default = "default_cache_ttl_seconds")]
    pub cache_ttl_seconds: u64,
}

const fn default_cache_ttl_seconds() -> u64 {
    DEFAULT_CACHE_TTL_SECONDS
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
