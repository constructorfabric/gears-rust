// Created: 2026-09-07 by Virtuozzo International GmbH
//! The verifier over a resolver whose answers the test dictates.
//!
//! What is pinned: the platform's `AuthN` resolver decides whether the token is
//! genuine and whose it is; the freshness and assurance rules are this gear's
//! own and are read from the token the resolver vouched for; and an absent
//! `step_up` section binds a verifier rather than none.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::AuthnStepUpVerifier;
use crate::config::{SettingsServiceConfig, StepUpConfig};
use crate::domain::stepup::{StepUpRefusal, StepUpRequirement, StepUpSubject, StepUpVerifier};

/// Admits the tokens it was given, each as the platform subject the test
/// names, and refuses everything else as unauthorized.
struct FixedResolver {
    admitted: HashMap<String, Uuid>,
}

#[async_trait]
impl AuthNResolverClient for FixedResolver {
    async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let subject = self
            .admitted
            .get(bearer_token)
            .ok_or_else(|| AuthNResolverError::Unauthorized("unknown token".to_owned()))?;
        Ok(AuthenticationResult {
            security_context: SecurityContext::builder()
                .subject_id(*subject)
                .subject_tenant_id(Uuid::nil())
                .build()
                .expect("context"),
        })
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        unreachable!("not exercised")
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// A compact JWT whose payload is `claims`; header and signature are noise,
/// since the fake resolver — like the real one — is the only party that reads
/// them.
fn jwt(claims: &Value) -> String {
    format!("hdr.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

fn hub_with(admitted: Vec<(&str, Uuid)>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AuthNResolverClient>(Arc::new(FixedResolver {
        admitted: admitted
            .into_iter()
            .map(|(t, id)| (t.to_owned(), id))
            .collect(),
    }));
    hub
}

fn requirement() -> StepUpRequirement {
    StepUpRequirement {
        max_age: Duration::from_mins(5),
        acr_values: Vec::new(),
        amr_values: Vec::new(),
    }
}

fn verifier(hub: Arc<ClientHub>, requirement: StepUpRequirement) -> AuthnStepUpVerifier {
    AuthnStepUpVerifier::new(hub, requirement, None, None)
}

fn subject(id: Uuid) -> StepUpSubject {
    StepUpSubject {
        subject_id: id,
        session_sub: Some("idp-sub-1".to_owned()),
    }
}

#[tokio::test]
async fn a_fresh_token_the_resolver_admits_for_this_session_verifies() {
    let id = Uuid::new_v4();
    let fresh = jwt(&json!({ "sub": id.to_string(), "auth_time": now() - 10 }));
    let v = verifier(hub_with(vec![(&fresh, id)]), requirement());
    assert_eq!(v.verify(Some(&fresh), &subject(id)).await, Ok(()));
    // The scheme sent along is stripped, not refused.
    assert_eq!(
        v.verify(Some(&format!("Bearer {fresh}")), &subject(id))
            .await,
        Ok(())
    );
}

#[tokio::test]
async fn a_token_the_resolver_refuses_is_a_signature_refusal() {
    let id = Uuid::new_v4();
    let v = verifier(hub_with(vec![]), requirement());
    let forged = jwt(&json!({ "sub": id.to_string(), "auth_time": now() }));
    assert!(matches!(
        v.verify(Some(&forged), &subject(id)).await,
        Err(StepUpRefusal::Signature(_))
    ));
}

#[tokio::test]
async fn an_absent_or_blank_token_is_missing_before_the_resolver_is_asked() {
    let id = Uuid::new_v4();
    // An empty hub would answer `NotConfigured`; a missing token is refused
    // first, so nothing is asked of anybody.
    let v = verifier(Arc::new(ClientHub::new()), requirement());
    assert_eq!(
        v.verify(None, &subject(id)).await,
        Err(StepUpRefusal::Missing)
    );
    assert_eq!(
        v.verify(Some("   "), &subject(id)).await,
        Err(StepUpRefusal::Missing)
    );
}

#[tokio::test]
async fn a_hub_without_the_resolver_is_not_configured_and_is_asked_again_later() {
    let id = Uuid::new_v4();
    let fresh = jwt(&json!({ "sub": id.to_string(), "auth_time": now() }));
    let hub = Arc::new(ClientHub::new());
    let v = verifier(Arc::clone(&hub), requirement());
    assert_eq!(
        v.verify(Some(&fresh), &subject(id)).await,
        Err(StepUpRefusal::NotConfigured)
    );
    // Nothing is cached on failure: once the resolver is wired, the next
    // verification finds it.
    hub.register::<dyn AuthNResolverClient>(Arc::new(FixedResolver {
        admitted: HashMap::from([(fresh.clone(), id)]),
    }));
    assert_eq!(v.verify(Some(&fresh), &subject(id)).await, Ok(()));
}

#[tokio::test]
async fn another_subjects_token_is_a_mismatch_unless_the_session_sub_rescues_it() {
    let id = Uuid::new_v4();
    let other = Uuid::new_v4();
    // Admitted as another platform subject, and the token's `sub` is not the
    // session's either: refused.
    let stranger = jwt(&json!({ "sub": other.to_string(), "auth_time": now() }));
    // Admitted as another platform subject, but the token's `sub` is what the
    // session's own token carried — a provider whose ids differ from the
    // platform's — so the binding holds.
    let by_idp_sub = jwt(&json!({ "sub": "idp-sub-1", "auth_time": now() }));
    let v = verifier(
        hub_with(vec![(&stranger, other), (&by_idp_sub, other)]),
        requirement(),
    );
    assert_eq!(
        v.verify(Some(&stranger), &subject(id)).await,
        Err(StepUpRefusal::SubjectMismatch)
    );
    assert_eq!(v.verify(Some(&by_idp_sub), &subject(id)).await, Ok(()));
    let no_rescue = StepUpSubject {
        subject_id: id,
        session_sub: None,
    };
    assert_eq!(
        v.verify(Some(&by_idp_sub), &no_rescue).await,
        Err(StepUpRefusal::SubjectMismatch)
    );
}

#[tokio::test]
async fn a_token_without_auth_time_is_missing_it_even_when_the_resolver_admits_it() {
    let id = Uuid::new_v4();
    let no_time = jwt(&json!({ "sub": id.to_string() }));
    // A token that is not a JWT at all — a static development token — has no
    // payload to read, so no `auth_time` either.
    let opaque = "e2e-token-tenant-a".to_owned();
    let v = verifier(hub_with(vec![(&no_time, id), (&opaque, id)]), requirement());
    assert_eq!(
        v.verify(Some(&no_time), &subject(id)).await,
        Err(StepUpRefusal::AuthTimeMissing)
    );
    assert_eq!(
        v.verify(Some(&opaque), &subject(id)).await,
        Err(StepUpRefusal::AuthTimeMissing)
    );
}

#[tokio::test]
async fn an_auth_time_older_than_the_window_is_stale() {
    let id = Uuid::new_v4();
    let stale = jwt(&json!({ "sub": id.to_string(), "auth_time": now() - 3_600 }));
    let just_inside = jwt(&json!({ "sub": id.to_string(), "auth_time": now() - 25 }));
    let v = verifier(
        hub_with(vec![(&stale, id), (&just_inside, id)]),
        StepUpRequirement {
            max_age: Duration::from_secs(30),
            ..requirement()
        },
    );
    assert_eq!(
        v.verify(Some(&stale), &subject(id)).await,
        Err(StepUpRefusal::Stale)
    );
    assert_eq!(v.verify(Some(&just_inside), &subject(id)).await, Ok(()));
}

#[tokio::test]
async fn the_required_assurance_is_matched_against_acr_or_amr() {
    let id = Uuid::new_v4();
    let strict = StepUpRequirement {
        max_age: Duration::from_mins(5),
        acr_values: vec!["urn:mfa".to_owned()],
        amr_values: vec!["pwd".to_owned()],
    };
    let weak = jwt(
        &json!({ "sub": id.to_string(), "auth_time": now(), "acr": "urn:pwd", "amr": ["pwd"] }),
    );
    let strong = jwt(
        &json!({ "sub": id.to_string(), "auth_time": now(), "acr": "urn:mfa", "amr": ["pwd", "otp"] }),
    );
    let no_amr = jwt(&json!({ "sub": id.to_string(), "auth_time": now(), "acr": "urn:mfa" }));
    let v = verifier(
        hub_with(vec![(&weak, id), (&strong, id), (&no_amr, id)]),
        strict,
    );
    assert_eq!(
        v.verify(Some(&weak), &subject(id)).await,
        Err(StepUpRefusal::Assurance)
    );
    assert_eq!(v.verify(Some(&strong), &subject(id)).await, Ok(()));
    assert_eq!(
        v.verify(Some(&no_amr), &subject(id)).await,
        Err(StepUpRefusal::Assurance)
    );
}

#[tokio::test]
async fn a_pinned_issuer_or_audience_the_token_lacks_is_a_claims_refusal() {
    let id = Uuid::new_v4();
    let unpinned = jwt(
        &json!({ "sub": id.to_string(), "auth_time": now(), "iss": "https://other", "aud": "them" }),
    );
    let pinned = jwt(&json!({
        "sub": id.to_string(), "auth_time": now(),
        "iss": "https://idp.example/realms/vhp", "aud": ["account", "settings-console"]
    }));
    let hub = hub_with(vec![(&unpinned, id), (&pinned, id)]);
    let v = AuthnStepUpVerifier::new(
        hub,
        requirement(),
        Some("https://idp.example/realms/vhp".to_owned()),
        Some("settings-console".to_owned()),
    );
    assert!(matches!(
        v.verify(Some(&unpinned), &subject(id)).await,
        Err(StepUpRefusal::Claims(_))
    ));
    assert_eq!(v.verify(Some(&pinned), &subject(id)).await, Ok(()));
}

#[tokio::test]
async fn no_step_up_section_binds_a_five_minute_verifier_that_admits_a_fresh_token() {
    // The case the old binding refused: with no section at all the verifier is
    // still bound, with the default window, and a fresh token commits.
    let config: SettingsServiceConfig =
        serde_json::from_value(json!({})).expect("an empty config parses");
    let id = Uuid::new_v4();
    let fresh = jwt(&json!({ "sub": id.to_string(), "auth_time": now() - 10 }));
    let v = AuthnStepUpVerifier::from_config(hub_with(vec![(&fresh, id)]), &config.step_up)
        .expect("bound");
    assert_eq!(v.requirement().max_age, Duration::from_mins(5));
    assert_eq!(v.verify(Some(&fresh), &subject(id)).await, Ok(()));
}

#[test]
fn a_window_above_five_minutes_is_refused_at_construction() {
    let config = StepUpConfig {
        max_age_seconds: 600,
        ..StepUpConfig::default()
    };
    assert!(AuthnStepUpVerifier::from_config(Arc::new(ClientHub::new()), &config).is_err());
    let config = StepUpConfig {
        max_age_seconds: 30,
        ..StepUpConfig::default()
    };
    let v = AuthnStepUpVerifier::from_config(Arc::new(ClientHub::new()), &config).expect("bound");
    assert_eq!(v.requirement().max_age, Duration::from_secs(30));
}

#[test]
fn a_blank_issuer_audience_or_assurance_entry_is_refused_at_construction() {
    // A blank pin can never be carried by a token, so it would refuse every
    // step-up-gated write at runtime with no sign at boot; a blank assurance
    // entry can never be met and is silently useless. Both are a templating
    // mistake, and the boot is where it is caught.
    let refused = [
        StepUpConfig {
            issuer: Some(String::new()),
            ..StepUpConfig::default()
        },
        StepUpConfig {
            audience: Some("   ".to_owned()),
            ..StepUpConfig::default()
        },
        StepUpConfig {
            acr_values: vec!["urn:mace:incommon:iap:silver".to_owned(), String::new()],
            ..StepUpConfig::default()
        },
        StepUpConfig {
            amr_values: vec![" mfa".to_owned()],
            ..StepUpConfig::default()
        },
    ];
    for (i, config) in refused.iter().enumerate() {
        let err = AuthnStepUpVerifier::from_config(Arc::new(ClientHub::new()), config)
            .err()
            .unwrap_or_else(|| panic!("config #{i} is refused"));
        assert!(
            err.to_string().contains("step_up."),
            "names the field: `{err}`"
        );
    }

    let config = StepUpConfig {
        issuer: Some("https://idp.example".to_owned()),
        audience: Some("settings".to_owned()),
        acr_values: vec!["urn:mace:incommon:iap:silver".to_owned()],
        amr_values: vec!["mfa".to_owned()],
        ..StepUpConfig::default()
    };
    AuthnStepUpVerifier::from_config(Arc::new(ClientHub::new()), &config).expect("bound");
}

#[test]
fn a_padded_issuer_or_audience_is_refused_at_construction() {
    // `carries()` compares the claim exactly, so a pin with surrounding
    // whitespace — a templating leftover — can never match a real token:
    // the same refusal the assurance entries already get.
    for config in [
        StepUpConfig {
            issuer: Some(" https://idp.example".to_owned()),
            ..StepUpConfig::default()
        },
        StepUpConfig {
            audience: Some("settings\n".to_owned()),
            ..StepUpConfig::default()
        },
    ] {
        let err = AuthnStepUpVerifier::from_config(Arc::new(ClientHub::new()), &config)
            .err()
            .expect("a padded pin is refused");
        assert!(
            err.to_string().contains("step_up."),
            "names the field: `{err}`"
        );
    }
}

/// A resolver that cannot answer, failing every call the way the test says.
struct DownResolver(fn() -> AuthNResolverError);

#[async_trait]
impl AuthNResolverClient for DownResolver {
    async fn authenticate(
        &self,
        _bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err((self.0)())
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        unreachable!("not exercised")
    }
}

fn hub_over(resolver: DownResolver) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AuthNResolverClient>(Arc::new(resolver));
    hub
}

#[tokio::test]
async fn a_resolver_that_cannot_answer_is_unavailable_not_a_signature_refusal() {
    // The token was never looked at. Telling the person to re-authenticate
    // would name the wrong fault and could not succeed until AuthN is back.
    let id = Uuid::new_v4();
    let fresh = jwt(&json!({ "sub": id.to_string(), "auth_time": now() }));
    let outages: [fn() -> AuthNResolverError; 4] = [
        || AuthNResolverError::NoPluginAvailable,
        || AuthNResolverError::ServiceUnavailable("plugin not ready".to_owned()),
        || AuthNResolverError::TokenAcquisitionFailed("idp unreachable".to_owned()),
        || AuthNResolverError::Internal("boom".to_owned()),
    ];
    for outage in outages {
        let v = verifier(hub_over(DownResolver(outage)), requirement());
        let refusal = v.verify(Some(&fresh), &subject(id)).await;
        assert!(
            matches!(refusal, Err(StepUpRefusal::Unavailable(_))),
            "{refusal:?} for {}",
            outage()
        );
        assert_eq!(refusal.unwrap_err().code(), "unavailable");
    }

    // The one verdict the resolver does pass stays a refusal of the token.
    let v = verifier(
        hub_over(DownResolver(|| {
            AuthNResolverError::Unauthorized("bad signature".to_owned())
        })),
        requirement(),
    );
    assert!(matches!(
        v.verify(Some(&fresh), &subject(id)).await,
        Err(StepUpRefusal::Signature(detail)) if detail == "bad signature"
    ));
}
