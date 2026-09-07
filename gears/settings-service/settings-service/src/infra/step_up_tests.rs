// Created: 2026-09-07 by Constructor Tech
//! The claims check over a key provider whose answers the test dictates.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jsonwebtoken::Header;
use serde_json::{Value, json};
use toolkit_auth::{ClaimsError, KeyProvider, ValidationConfig};
use uuid::Uuid;

use super::OidcStepUpVerifier;
use crate::domain::stepup::{StepUpRefusal, StepUpRequirement, StepUpSubject, StepUpVerifier};

/// Decodes nothing: hands back the claims registered for a token string, or
/// refuses as an invalid signature.
struct FixedKeys {
    tokens: std::collections::HashMap<String, Value>,
}

#[async_trait]
impl KeyProvider for FixedKeys {
    fn name(&self) -> &'static str {
        "fixed"
    }

    async fn validate_and_decode(&self, token: &str) -> Result<(Header, Value), ClaimsError> {
        self.tokens
            .get(token)
            .cloned()
            .map(|claims| (Header::default(), claims))
            .ok_or(ClaimsError::InvalidSignature)
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn verifier(tokens: Vec<(&str, Value)>, requirement: StepUpRequirement) -> OidcStepUpVerifier {
    OidcStepUpVerifier::new(
        Arc::new(FixedKeys {
            tokens: tokens.into_iter().map(|(t, c)| (t.to_owned(), c)).collect(),
        }),
        ValidationConfig {
            require_exp: false,
            ..ValidationConfig::default()
        },
        requirement,
    )
}

fn requirement() -> StepUpRequirement {
    StepUpRequirement {
        max_age: Duration::from_mins(5),
        acr_values: Vec::new(),
        amr_values: Vec::new(),
    }
}

fn subject(id: Uuid) -> StepUpSubject {
    StepUpSubject {
        subject_id: id,
        session_sub: Some("idp-sub-1".to_owned()),
    }
}

#[tokio::test]
async fn a_fresh_token_bound_to_the_session_verifies_without_calling_the_provider() {
    let id = Uuid::new_v4();
    let v = verifier(
        vec![
            (
                "fresh",
                json!({ "sub": id.to_string(), "auth_time": now() - 10 }),
            ),
            (
                "by-idp-sub",
                json!({ "sub": "idp-sub-1", "auth_time": now() - 10 }),
            ),
        ],
        requirement(),
    );
    assert_eq!(v.verify(Some("fresh"), &subject(id)).await, Ok(()));
    assert_eq!(v.verify(Some("by-idp-sub"), &subject(id)).await, Ok(()));
}

#[tokio::test]
async fn every_way_a_token_fails_to_prove_a_recent_ceremony_is_refused() {
    let id = Uuid::new_v4();
    let v = verifier(
        vec![
            (
                "stale",
                json!({ "sub": id.to_string(), "auth_time": now() - 3_600 }),
            ),
            (
                "other",
                json!({ "sub": Uuid::new_v4().to_string(), "auth_time": now() - 10 }),
            ),
            ("no-time", json!({ "sub": id.to_string() })),
        ],
        requirement(),
    );
    assert_eq!(
        v.verify(None, &subject(id)).await,
        Err(StepUpRefusal::Missing)
    );
    assert_eq!(
        v.verify(Some(""), &subject(id)).await,
        Err(StepUpRefusal::Missing)
    );
    assert!(matches!(
        v.verify(Some("forged"), &subject(id)).await,
        Err(StepUpRefusal::Signature(_))
    ));
    assert_eq!(
        v.verify(Some("stale"), &subject(id)).await,
        Err(StepUpRefusal::Stale)
    );
    assert_eq!(
        v.verify(Some("other"), &subject(id)).await,
        Err(StepUpRefusal::SubjectMismatch)
    );
    assert_eq!(
        v.verify(Some("no-time"), &subject(id)).await,
        Err(StepUpRefusal::AuthTimeMissing)
    );
}

#[tokio::test]
async fn the_required_assurance_is_matched_against_acr_or_amr() {
    let id = Uuid::new_v4();
    let strict = StepUpRequirement {
        max_age: Duration::from_mins(5),
        acr_values: vec!["urn:mfa".to_owned()],
        amr_values: vec!["pwd".to_owned()],
    };
    let v = verifier(
        vec![
            (
                "weak",
                json!({ "sub": id.to_string(), "auth_time": now(), "acr": "urn:pwd", "amr": ["pwd"] }),
            ),
            (
                "strong",
                json!({ "sub": id.to_string(), "auth_time": now(), "acr": "urn:mfa", "amr": ["pwd", "otp"] }),
            ),
            (
                "no-amr",
                json!({ "sub": id.to_string(), "auth_time": now(), "acr": "urn:mfa" }),
            ),
        ],
        strict,
    );
    assert_eq!(
        v.verify(Some("weak"), &subject(id)).await,
        Err(StepUpRefusal::Assurance)
    );
    assert_eq!(v.verify(Some("strong"), &subject(id)).await, Ok(()));
    assert_eq!(
        v.verify(Some("no-amr"), &subject(id)).await,
        Err(StepUpRefusal::Assurance)
    );
}

#[test]
fn a_window_above_five_minutes_is_refused_at_construction() {
    let config = crate::config::StepUpConfig {
        jwks_uri: "https://idp.example/keys".to_owned(),
        max_age_seconds: 600,
        issuer: None,
        audience: None,
        acr_values: Vec::new(),
        amr_values: Vec::new(),
    };
    assert!(OidcStepUpVerifier::from_config(&config).is_err());
}
