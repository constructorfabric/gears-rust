// Created: 2026-09-07 by Constructor Tech
//! The step-up verifier port.
//!
//! Setting a value on a declaration that requires elevated confirmation needs
//! proof that a person re-authenticated at the identity provider just now.
//! The domain states that rule here and nothing more: how the token's
//! signature is checked is the binding's business — in this gear the
//! platform's own `AuthN` resolver validates it exactly as it validates every
//! session token, and the freshness claims are read from the token it vouched
//! for. A binding that cannot fail is not a binding at all.

use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

/// The subject type of an interactive human session, as the authorization
/// design states it (a GTS type identifier). Every subject type outside
/// [`INTERACTIVE_SUBJECT_TYPES`] — and an unlabelled one — is a service
/// principal for the purposes of step-up: no ceremony a machine performs
/// proves that a person is present.
pub const USER_SUBJECT_TYPE: &str = "gts.cf.core.security.subject_user.v1~";

/// Subject types that denote an interactive human session.
///
/// The platform has not settled on one vocabulary for this field. The
/// authorization design states a GTS type id, and `oidc-authn-plugin`'s s2s
/// branch defaults to [`USER_SUBJECT_TYPE`] accordingly — but the Keycloak
/// identity-provider plugin writes the bare word `user` into the `user_type`
/// attribute when it
/// provisions a person, and that attribute is what a deployed realm maps into
/// the claim. A machine is unambiguous either way
/// (`gts.cf.core.security.subject_service.v1~`), so accepting both widens
/// nothing: it recognises the human case deployments actually produce.
///
/// Both arms stay listed until the platform normalises the field. Dropping
/// either one silently refuses every interactive write on the stands that use
/// it, with a `403` that names a service principal and explains nothing.
pub const INTERACTIVE_SUBJECT_TYPES: &[&str] = &[USER_SUBJECT_TYPE, "user"];

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
    /// The platform's `AuthN` resolver could not be reached from the hub, so no
    /// token can be verified: every write that needs step-up refuses.
    NotConfigured,
    /// The `AuthN` resolver did not authenticate the token: signature, expiry,
    /// issuer, or a token that is not one at all.
    Signature(String),
    /// The token's `sub` is not the session's subject.
    SubjectMismatch,
    /// `auth_time` is absent.
    AuthTimeMissing,
    /// `auth_time` is older than the freshness window.
    Stale,
    /// `acr` or `amr` does not meet the required assurance.
    Assurance,
    /// A claim the deployment pinned — issuer or audience — is not carried.
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
    /// requirement's window. The gear never calls the identity provider on
    /// this path.
    async fn verify(
        &self,
        token: Option<&str>,
        subject: &StepUpSubject,
    ) -> Result<(), StepUpRefusal>;

    /// The requirement the deployment configured, for the `401` challenge.
    fn requirement(&self) -> &StepUpRequirement;
}

/// The JSON payload of a compact JWT, decoded **without** verifying it.
///
/// Safe in exactly two uses, and no third: reading claims from a token whose
/// signature the `AuthN` resolver verified a moment earlier over these same
/// bytes, and binding a step-up token to the session by the `sub` that the
/// session's own — already authenticated — token carried. Anything else is
/// trusting input. `None` when the token is not a compact JWT at all.
#[must_use]
pub fn unverified_payload(token: &str) -> Option<serde_json::Value> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
