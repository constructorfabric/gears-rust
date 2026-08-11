//! Static configuration for the token-issuer gear.

use serde::Deserialize;
use toolkit_macros::ExpandVars;

/// Hard upper bound for capability and grant token lifetimes (24 hours).
///
/// Per-operation grant policy may impose a smaller maximum; this issuer-wide
/// ceiling is defense in depth and prevents effectively unbounded credentials.
pub const MAX_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;

/// Token-issuer configuration.
///
/// Deserialized from the gear config block; every field has a default so a
/// partial block still produces a usable config. Unknown keys are rejected to
/// catch typos early. `issuer_base_url` is `${VAR}`-expanded so it can be set
/// per environment.
#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(default, deny_unknown_fields)]
pub struct TokenIssuerConfig {
    /// Public base URL the issuer is reachable at (e.g. `https://core.example.com`).
    /// Used to derive the `cap`/`obo` issuer identifiers; must be non-empty.
    #[expand_vars]
    pub issuer_base_url: String,
    /// Vendor used to select the signing plugin instance.
    pub vendor: String,
    /// Capability-token lifetime, in seconds.
    pub cap_ttl_secs: u64,
    /// Reuse floor: a cached cap token is reused while its remaining TTL exceeds
    /// this, otherwise it is re-minted.
    pub cap_reuse_floor_secs: u64,
    /// OBO-token lifetime, in seconds (OBO surface; gated by `obo.enabled`).
    pub obo_ttl_secs: u64,
    /// Allowed clock skew, in seconds.
    pub clock_skew_secs: u64,
    /// Transit key name used to sign capability tokens.
    pub cap_key_name: String,
    /// Transit key name used to sign OBO tokens (gated by `obo.enabled`).
    pub obo_key_name: String,
    /// Audience asserted on OBO tokens (gated by `obo.enabled`).
    pub obo_audience: String,
    /// Default grant-token lifetime, in seconds. The `grants` gear clamps `exp`
    /// to the smallest per-operation `max_ttl` and passes an explicit TTL; this
    /// is the fallback / upper default a mint uses when none is supplied.
    pub grant_ttl_secs: u64,
    /// Transit key name used to sign grant tokens (`grant+jwt`). One class, one
    /// issuer, one Transit key — never shared with `cap`/`obo`.
    pub grant_key_name: String,
    /// `OpenBao` Transit mount path the signing plugin uses.
    pub transit_mount: String,
    /// OBO feature gate.
    pub obo: OboGate,
}

/// OBO feature gate. The whole OBO surface — issuance, JWKS, discovery, and
/// the re-mint route — is inert unless this is set (DESIGN.md § 3.3).
///
/// `enabled` defaults to `false`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OboGate {
    /// Whether OBO minting/issuer routes are enabled.
    pub enabled: bool,
}

impl Default for TokenIssuerConfig {
    fn default() -> Self {
        Self {
            issuer_base_url: String::new(),
            vendor: "constructorfabric".to_owned(),
            cap_ttl_secs: 300,
            cap_reuse_floor_secs: 150,
            obo_ttl_secs: 60,
            clock_skew_secs: 30,
            cap_key_name: "cap-token-sign".to_owned(),
            obo_key_name: "obo-token-sign".to_owned(),
            obo_audience: "public-api".to_owned(),
            grant_ttl_secs: 300,
            grant_key_name: "grant-token-sign".to_owned(),
            transit_mount: "transit".to_owned(),
            obo: OboGate::default(),
        }
    }
}

impl TokenIssuerConfig {
    /// Validates the config invariants.
    ///
    /// # Errors
    /// Returns `Err` with a description if `issuer_base_url` is blank, the TTL
    /// ordering `clock_skew_secs <= cap_reuse_floor_secs < cap_ttl_secs` is
    /// violated, capability/grant TTLs exceed [`MAX_TOKEN_TTL_SECS`], or
    /// `obo_ttl_secs` is out of bounds (`0 < obo_ttl_secs <= 60` and
    /// `clock_skew_secs < obo_ttl_secs`).
    pub fn validate(&self) -> Result<(), String> {
        if self.issuer_base_url.trim().is_empty() {
            return Err("issuer_base_url required".to_owned());
        }
        if !(self.clock_skew_secs <= self.cap_reuse_floor_secs
            && self.cap_reuse_floor_secs < self.cap_ttl_secs)
        {
            return Err(
                "require clock_skew_secs <= cap_reuse_floor_secs < cap_ttl_secs".to_owned(),
            );
        }
        if self.cap_ttl_secs > MAX_TOKEN_TTL_SECS {
            return Err(format!("require cap_ttl_secs <= {MAX_TOKEN_TTL_SECS}"));
        }
        if !(self.obo_ttl_secs > 0 && self.obo_ttl_secs <= 60) {
            return Err("require 0 < obo_ttl_secs <= 60".to_owned());
        }
        if self.clock_skew_secs >= self.obo_ttl_secs {
            return Err("require clock_skew_secs < obo_ttl_secs".to_owned());
        }
        if !(1..=MAX_TOKEN_TTL_SECS).contains(&self.grant_ttl_secs) {
            return Err(format!(
                "require 0 < grant_ttl_secs <= {MAX_TOKEN_TTL_SECS}"
            ));
        }
        Ok(())
    }

    /// Issuer identifier for capability tokens.
    #[must_use]
    pub fn cap_issuer(&self) -> String {
        format!("{}/issuers/cap", self.issuer_base_url.trim_end_matches('/'))
    }

    /// Issuer identifier for OBO tokens (gated by `obo.enabled`).
    #[must_use]
    pub fn obo_issuer(&self) -> String {
        format!("{}/issuers/obo", self.issuer_base_url.trim_end_matches('/'))
    }

    /// Issuer identifier for grant tokens (`grant+jwt`).
    #[must_use]
    pub fn grant_issuer(&self) -> String {
        format!(
            "{}/issuers/grant",
            self.issuer_base_url.trim_end_matches('/')
        )
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
