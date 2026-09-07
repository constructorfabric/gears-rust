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
//! # Step-up configuration
//!
//! Step-up is gated per declaration (`requires_step_up`, default required —
//! `cpt-cf-settings-service-fr-service-writes`) and enforced on interactive writes
//! (`cpt-cf-settings-service-fr-authn-role-gating`). DESIGN.md §4.2 *Value
//! Writer* fixes the R1 check: the presented token's signature against the
//! identity provider's JWKS, `sub` matching the session, `auth_time` within the
//! freshness window, and the required `acr`/`amr` — the default binding behind
//! the gear's `StepUpVerifier` port. The [`StepUpConfig`] section carries the
//! provider's JWKS endpoint and the window, which
//! `cpt-cf-settings-service-fr-validate-before-set` makes deployment-configured
//! and DESIGN caps at five minutes. With the section absent nothing is bound:
//! every write to a declaration that requires step-up refuses, reads keep
//! serving, and there is no bypass to configure.

use serde::Deserialize;

/// The cache backstop that bounds staleness when an invalidation broadcast is
/// missed. Fixed by DESIGN.md §4.2 *Cache & Invalidation*, not by the operator.
const DEFAULT_CACHE_TTL_SECONDS: u64 = 30;
const DEFAULT_AUDIT_RETENTION_DAYS: u32 = 365;

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

    /// How long an audit record without an explicit `retain_until` stays in the
    /// online window. Twelve months by default, and never configurable below
    /// that: init refuses a shorter value.
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u32,

    /// The step-up verifier binding. Absent, no verifier is bound.
    #[serde(default)]
    pub step_up: Option<StepUpConfig>,
}

/// The OIDC/JWKS step-up binding's deployment values.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepUpConfig {
    /// The identity provider's JWKS endpoint, fetched and cached in the
    /// background; never called on the write path.
    pub jwks_uri: String,

    /// The freshness window for `auth_time`, in seconds. Five minutes by
    /// default and never above it: init refuses a longer window.
    #[serde(default = "default_step_up_max_age_seconds")]
    pub max_age_seconds: u64,

    /// The issuer the token must carry, when the deployment pins one.
    #[serde(default)]
    pub issuer: Option<String>,

    /// The audience the token must carry, when the deployment pins one.
    #[serde(default)]
    pub audience: Option<String>,

    /// Authentication context class references that satisfy the requirement.
    #[serde(default)]
    pub acr_values: Vec<String>,

    /// Authentication methods that satisfy the requirement.
    #[serde(default)]
    pub amr_values: Vec<String>,
}

const DEFAULT_STEP_UP_MAX_AGE_SECONDS: u64 = 300;

const fn default_step_up_max_age_seconds() -> u64 {
    DEFAULT_STEP_UP_MAX_AGE_SECONDS
}

const fn default_cache_ttl_seconds() -> u64 {
    DEFAULT_CACHE_TTL_SECONDS
}

const fn default_audit_retention_days() -> u32 {
    DEFAULT_AUDIT_RETENTION_DAYS
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
