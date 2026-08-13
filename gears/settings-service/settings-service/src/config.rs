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
//! # Step-up configuration is not here
//!
//! An earlier draft required a JWKS endpoint and a step-up freshness window.
//! Neither traces to a PRD requirement, and the freshness window was actively
//! harmful: `cpt-cf-settings-service-fr-apply-preview-stepup` requires
//! credential re-verification before **every** Apply, unconditionally, so a
//! deployment-tunable staleness allowance is a knob for weakening a requirement
//! that has no carve-out. DESIGN.md §4.2 also assigns the step-up contract to
//! the `authn-resolver` gear, which this service resolves rather than defines —
//! so whatever configuration verification needs arrives with that contract.

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
