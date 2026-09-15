// Created: 2026-09-07 by Constructor Tech
//! The step-up verifier port.
//!
//! Setting a value on a declaration that requires elevated confirmation needs
//! proof that a person re-authenticated at the identity provider just now.
//! The domain states that rule here and nothing more: the OIDC/JWKS claims
//! check is one binding, a platform-wide elevated session will be another,
//! and a binding that cannot fail is not a binding at all.

use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

/// The subject type of an interactive human session, as the platform's
/// authentication resolver labels it. Every other subject type — and an
/// unlabelled one — is a service principal for the purposes of step-up: no
/// ceremony a machine performs proves that a person is present.
pub const USER_SUBJECT_TYPE: &str = "gts.cf.core.security.subject_user.v1~";

/// What a deployment requires of a step-up token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepUpRequirement {
    /// How old `auth_time` may be. Deployment-configured and never above five
    /// minutes.
    pub max_age: Duration,
    /// Authentication context class references, any of which satisfies the
    /// requirement; empty means none required.
    pub acr_values: Vec<String>,
    /// Authentication methods, any of which satisfies the requirement; empty
    /// means none required.
    pub amr_values: Vec<String>,
}

impl StepUpRequirement {
    /// The longest freshness window the design allows.
    pub const MAX_AGE_CEILING: Duration = Duration::from_mins(5);
}

/// Why a token does not prove a recent re-authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepUpRefusal {
    /// No token was presented.
    Missing,
    /// No verifier is bound: every write that needs step-up refuses.
    NotConfigured,
    /// The signature did not verify against the provider's keys, or the token
    /// is malformed.
    Signature(String),
    /// The token's `sub` is not the session's subject.
    SubjectMismatch,
    /// `auth_time` is absent.
    AuthTimeMissing,
    /// `auth_time` is older than the freshness window.
    Stale,
    /// `acr` or `amr` does not meet the required assurance.
    Assurance,
    /// A standard claim failed: expired, wrong issuer or audience.
    Claims(String),
}

impl StepUpRefusal {
    /// A short, stable code for logs, metrics and the challenge's description.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::NotConfigured => "not_configured",
            Self::Signature(_) => "signature",
            Self::SubjectMismatch => "subject_mismatch",
            Self::AuthTimeMissing => "auth_time_missing",
            Self::Stale => "stale",
            Self::Assurance => "assurance",
            Self::Claims(_) => "claims",
        }
    }
}

/// The session the token must confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepUpSubject {
    /// The platform subject id of the session.
    pub subject_id: Uuid,
    /// The `sub` the session's own token carried, when it can be read, so a
    /// provider whose `sub` is not the platform id still binds correctly.
    pub session_sub: Option<String>,
}

/// The port.
#[async_trait]
pub trait StepUpVerifier: Send + Sync {
    /// Whether `token` proves that `subject` re-authenticated within the
    /// requirement's window. The provider is never called on this path.
    async fn verify(
        &self,
        token: Option<&str>,
        subject: &StepUpSubject,
    ) -> Result<(), StepUpRefusal>;

    /// The requirement the deployment configured, for the `401` challenge.
    fn requirement(&self) -> &StepUpRequirement;
}

/// The binding when nothing is configured: refuses every write that needs
/// step-up while reads keep serving. Deliberately not a bypass.
pub struct NoStepUpVerifier {
    requirement: StepUpRequirement,
}

impl Default for NoStepUpVerifier {
    fn default() -> Self {
        Self {
            requirement: StepUpRequirement {
                max_age: StepUpRequirement::MAX_AGE_CEILING,
                acr_values: Vec::new(),
                amr_values: Vec::new(),
            },
        }
    }
}

#[async_trait]
impl StepUpVerifier for NoStepUpVerifier {
    async fn verify(
        &self,
        _token: Option<&str>,
        _subject: &StepUpSubject,
    ) -> Result<(), StepUpRefusal> {
        Err(StepUpRefusal::NotConfigured)
    }

    fn requirement(&self) -> &StepUpRequirement {
        &self.requirement
    }
}
