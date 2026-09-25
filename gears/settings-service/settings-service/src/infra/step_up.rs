// Created: 2026-09-07 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-step-up-verifier:p1
//! The `StepUpVerifier` binding over the platform's `AuthN` resolver.
//!
//! The presented token is validated by `AuthNResolverClient::authenticate` —
//! the same path, trust store and key cache that admitted the caller's session
//! token at the gateway — so this gear carries no key-set address, no HTTP
//! client and no trust policy of its own. What the resolver does not answer is
//! *how recently* the person authenticated: a `SecurityContext` carries no
//! `auth_time`, `acr` or `amr`, so those are read from the token's own payload
//! once the resolver has vouched for its signature.
//!
//! The client is fetched from the hub on first use, never at init, and the
//! resolver is not in this gear's `deps` (DESIGN.md §4.9): a gear that reads
//! settings during its own init must not be able to close a cycle through
//! this one. A hub that cannot hand it over is one refusal among the others,
//! not a configuration state.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverClient;
use authn_resolver_sdk::AuthNResolverError;
use serde_json::Value;
use toolkit::ClientHub;

use crate::config::StepUpConfig;
use crate::domain::stepup::{
    StepUpRefusal, StepUpRequirement, StepUpSubject, StepUpVerifier, unverified_payload,
};

/// The binding.
pub struct AuthnStepUpVerifier {
    hub: Arc<ClientHub>,
    resolver: OnceLock<Arc<dyn AuthNResolverClient>>,
    requirement: StepUpRequirement,
    issuer: Option<String>,
    audience: Option<String>,
}

impl AuthnStepUpVerifier {
    /// Over the hub the resolver will be fetched from, with the deployment's
    /// requirement and its optional issuer and audience pins.
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        requirement: StepUpRequirement,
        issuer: Option<String>,
        audience: Option<String>,
    ) -> Self {
        Self {
            hub,
            resolver: OnceLock::new(),
            requirement,
            issuer,
            audience,
        }
    }

    /// From the gear's configuration.
    ///
    /// # Errors
    /// When the window exceeds five minutes; when a pinned `issuer` or
    /// `audience` is blank, or an `acr_values` / `amr_values` entry is blank
    /// or padded with whitespace — a pin no token can carry would refuse every
    /// step-up-gated write at runtime with no sign at boot, and an entry no
    /// claim can equal is silently useless, so both are caught here.
    pub fn from_config(hub: Arc<ClientHub>, config: &StepUpConfig) -> anyhow::Result<Self> {
        let max_age = Duration::from_secs(config.max_age_seconds);
        if max_age > StepUpRequirement::MAX_AGE_CEILING {
            anyhow::bail!(
                "step_up.max_age_seconds is {} but the freshness window may not exceed {} seconds",
                config.max_age_seconds,
                StepUpRequirement::MAX_AGE_CEILING.as_secs()
            );
        }
        for (field, pinned) in [
            ("step_up.issuer", config.issuer.as_deref()),
            ("step_up.audience", config.audience.as_deref()),
        ] {
            if pinned.is_some_and(|p| p.trim().is_empty() || p.trim() != p) {
                anyhow::bail!(
                    "{field} is blank or padded with whitespace: the claim is compared exactly, so \
                     a pin no token can carry would refuse every step-up-gated write; pin the \
                     exact value or leave the field out"
                );
            }
        }
        for (field, values) in [
            ("step_up.acr_values", &config.acr_values),
            ("step_up.amr_values", &config.amr_values),
        ] {
            check_assurance_entries(field, values)?;
        }
        Ok(Self::new(
            hub,
            StepUpRequirement {
                max_age,
                acr_values: config.acr_values.clone(),
                amr_values: config.amr_values.clone(),
            },
            config.issuer.clone(),
            config.audience.clone(),
        ))
    }

    /// The resolver, fetched from the hub the first time it is needed and
    /// kept: a client the hub handed out once is the one it would hand out
    /// again.
    fn resolver(&self) -> Result<Arc<dyn AuthNResolverClient>, StepUpRefusal> {
        if let Some(resolver) = self.resolver.get() {
            return Ok(Arc::clone(resolver));
        }
        let resolver = self
            .hub
            .get::<dyn AuthNResolverClient>()
            .map_err(|_| StepUpRefusal::NotConfigured)?;
        Ok(Arc::clone(self.resolver.get_or_init(|| resolver)))
    }
}

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Whether `claim` meets `required`: any of the required values, as the one
/// string or one of the array the claim carries; nothing required, met.
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

/// Whether `claim` names `pinned`: a string equal to it, or an array holding
/// it — `aud` may be either.
fn carries(claim: Option<&Value>, pinned: &str) -> bool {
    match claim {
        Some(Value::String(one)) => one == pinned,
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).any(|m| m == pinned),
        _ => false,
    }
}

#[async_trait]
impl StepUpVerifier for AuthnStepUpVerifier {
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
        // The resolver takes the raw token; a client that sent the scheme
        // along is not wrong about the token.
        let token = token.strip_prefix("Bearer ").map_or(token, str::trim);
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-1
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-2
        // The platform validates the token as it validates every session
        // token — same trust store, same key cache, no address of our own.
        let authenticated = self
            .resolver()?
            .authenticate(token)
            .await
            .map_err(|e| match e {
                // The resolver looked at the token and said no.
                AuthNResolverError::Unauthorized(detail) => StepUpRefusal::Signature(detail),
                // The resolver could not look. That is an outage of the
                // platform's AuthN, not a verdict on the token; a challenge
                // here would send the person to re-authenticate against a
                // dependency that is down.
                outage @ (AuthNResolverError::NoPluginAvailable
                | AuthNResolverError::ServiceUnavailable(_)
                | AuthNResolverError::TokenAcquisitionFailed(_)
                | AuthNResolverError::Internal(_)) => {
                    // The resolver's own text goes to the log; the refusal —
                    // which becomes a 503 detail — says only what failed.
                    tracing::warn!(
                        error = %crate::log_text::LogSafe(&outage),
                        "step-up: the AuthN resolver could not be asked"
                    );
                    StepUpRefusal::Unavailable("the AuthN resolver could not be asked".to_owned())
                }
            })?;
        // The resolver vouched for the signature over exactly these bytes, so
        // the payload may now be read for what a `SecurityContext` does not
        // carry. Not a JWT, or not readable: no claim it names is present.
        let claims = unverified_payload(token).unwrap_or(Value::Null);
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-3
        // One person's ceremony does not confirm another's write: the token's
        // subject must be this session's — the platform id the resolver
        // returned, or the `sub` the session's own token carried when the
        // provider's ids differ.
        let same_subject = authenticated.security_context.subject_id() == subject.subject_id;
        let same_sub = subject
            .session_sub
            .as_deref()
            .is_some_and(|s| claims.get("sub").and_then(Value::as_str) == Some(s));
        if !(same_subject || same_sub) {
            return Err(StepUpRefusal::SubjectMismatch);
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-3
        // A deployment that pinned an issuer or an audience still gets its pin,
        // whatever the resolver's own opinion of the issuer.
        if let Some(issuer) = &self.issuer
            && !carries(claims.get("iss"), issuer)
        {
            return Err(StepUpRefusal::Claims(format!(
                "the token's issuer is not `{issuer}`"
            )));
        }
        if let Some(audience) = &self.audience
            && !carries(claims.get("aud"), audience)
        {
            return Err(StepUpRefusal::Claims(format!(
                "the token's audience does not include `{audience}`"
            )));
        }
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

/// The most entries one assurance list may hold, and the longest entry.
const MAX_ASSURANCE_ENTRIES: usize = 32;
const MAX_ASSURANCE_ENTRY_LEN: usize = 255;

/// Refuse at boot an assurance list the challenge could not carry faithfully.
///
/// Each entry is compared exactly against a claim and written into the
/// space-separated `acr_values` parameter of the challenge, so it must be one
/// visible ASCII token: a space would split it in two, a control character
/// makes the header unbuildable so it is dropped, and anything outside ASCII
/// is no claim value an issuer sends. A repeated entry says nothing new, and
/// an entry or a list past its bound is a typo nobody writes on purpose.
fn check_assurance_entries(field: &str, values: &[String]) -> anyhow::Result<()> {
    if values.len() > MAX_ASSURANCE_ENTRIES {
        anyhow::bail!(
            "{field} holds {} entries; at most {MAX_ASSURANCE_ENTRIES} are accepted",
            values.len()
        );
    }
    let mut seen = std::collections::HashSet::new();
    for entry in values {
        if entry.is_empty()
            || entry.len() > MAX_ASSURANCE_ENTRY_LEN
            || !entry.bytes().all(|b| b.is_ascii_graphic())
        {
            anyhow::bail!(
                "{field} holds the entry `{}`, which no claim can equal and the challenge cannot \
                 carry: each entry is one token of 1 to {MAX_ASSURANCE_ENTRY_LEN} visible ASCII \
                 characters, with no space or control character",
                entry.escape_debug()
            );
        }
        if !seen.insert(entry.as_str()) {
            anyhow::bail!("{field} lists the entry `{entry}` twice");
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "step_up_tests.rs"]
mod step_up_tests;
