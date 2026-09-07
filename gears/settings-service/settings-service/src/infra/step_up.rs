// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-step-up-verifier:p1
//! The default `StepUpVerifier` binding: local claims inspection of the
//! presented token against the identity provider's published JWKS.
//!
//! The provider is never called on the write path. Keys are fetched and
//! cached by `toolkit-auth`'s JWKS provider, which refreshes in the
//! background and on an unknown key id.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_auth::{KeyProvider, ValidationConfig, validate_claims};

use crate::config::StepUpConfig;
use crate::domain::stepup::{StepUpRefusal, StepUpRequirement, StepUpSubject, StepUpVerifier};

/// The binding.
pub struct OidcStepUpVerifier {
    keys: Arc<dyn KeyProvider>,
    claims: ValidationConfig,
    requirement: StepUpRequirement,
}

impl OidcStepUpVerifier {
    /// Over a key provider and the deployment's requirement.
    #[must_use]
    pub fn new(
        keys: Arc<dyn KeyProvider>,
        claims: ValidationConfig,
        requirement: StepUpRequirement,
    ) -> Self {
        Self {
            keys,
            claims,
            requirement,
        }
    }

    /// From the gear's configuration, fetching keys from its JWKS endpoint.
    ///
    /// # Errors
    /// When the window exceeds five minutes, or the HTTP client for the JWKS
    /// endpoint cannot be built.
    pub fn from_config(config: &StepUpConfig) -> anyhow::Result<Self> {
        let max_age = Duration::from_secs(config.max_age_seconds);
        if max_age > StepUpRequirement::MAX_AGE_CEILING {
            anyhow::bail!(
                "step_up.max_age_seconds is {} but the freshness window may not exceed {} seconds",
                config.max_age_seconds,
                StepUpRequirement::MAX_AGE_CEILING.as_secs()
            );
        }
        let keys = toolkit_auth::JwksKeyProvider::new(config.jwks_uri.clone())
            .map_err(|e| anyhow::anyhow!("step-up JWKS client: {e}"))?;
        Ok(Self::new(
            Arc::new(keys),
            ValidationConfig {
                allowed_issuers: config.issuer.iter().cloned().collect(),
                allowed_audiences: config.audience.iter().cloned().collect(),
                ..ValidationConfig::default()
            },
            StepUpRequirement {
                max_age,
                acr_values: config.acr_values.clone(),
                amr_values: config.amr_values.clone(),
            },
        ))
    }
}

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn meets(required: &[String], claim: Option<&Value>) -> bool {
    if required.is_empty() {
        return true;
    }
    match claim {
        Some(Value::String(one)) => required.iter().any(|r| r == one),
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(Value::as_str)
            .any(|m| required.iter().any(|r| r == m)),
        _ => false,
    }
}

#[async_trait]
impl StepUpVerifier for OidcStepUpVerifier {
    async fn verify(
        &self,
        token: Option<&str>,
        subject: &StepUpSubject,
    ) -> Result<(), StepUpRefusal> {
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-1
        let token = token
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or(StepUpRefusal::Missing)?;
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-1
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-2
        let (_, claims) = self
            .keys
            .validate_and_decode(token)
            .await
            .map_err(|e| StepUpRefusal::Signature(e.to_string()))?;
        validate_claims(&claims, &self.claims).map_err(|e| StepUpRefusal::Claims(e.to_string()))?;
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-3
        // One person's ceremony does not confirm another's write: the token's
        // subject must be this session's — the platform id, or the `sub` the
        // session's own token carried when the provider's ids differ.
        let sub = claims
            .get("sub")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let is_session = sub == subject.subject_id.to_string()
            || subject.session_sub.as_deref().is_some_and(|s| s == sub);
        if sub.is_empty() || !is_session {
            return Err(StepUpRefusal::SubjectMismatch);
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-3
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-4
        // `auth_time` is the claim that separates a re-authenticated token from
        // the morning's session token.
        let auth_time = claims
            .get("auth_time")
            .and_then(Value::as_i64)
            .ok_or(StepUpRefusal::AuthTimeMissing)?;
        let age = now_unix().saturating_sub(auth_time);
        if age < 0 || u64::try_from(age).unwrap_or(u64::MAX) > self.requirement.max_age.as_secs() {
            return Err(StepUpRefusal::Stale);
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-4
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-5
        if !meets(&self.requirement.acr_values, claims.get("acr"))
            || !meets(&self.requirement.amr_values, claims.get("amr"))
        {
            return Err(StepUpRefusal::Assurance);
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-5
        Ok(())
    }

    fn requirement(&self) -> &StepUpRequirement {
        &self.requirement
    }
}

#[cfg(test)]
#[path = "step_up_tests.rs"]
mod step_up_tests;
