//! OBO-token claim assembly and signing (DESIGN.md § 3.1).
//!
//! An OBO token (`typ=obo+jwt`, `iss={core}/issuers/obo`, `aud=public-api`,
//! key `obo-token-sign`) is what an adapter presents back at the public edge.
//! Identity (`sub`, `user_type`, `tenant_id`) is copied from the verified cap
//! token; `act` records the acting adapter; `scope` is the Gate-2 down-scoped
//! grant (space-joined, never the wildcard `*`).

use serde::{Deserialize, Serialize};
use token_issuer_sdk::{CapabilityClaims, SigningClientV1, SigningKeyRef, TokenIssuerError};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::jws::assemble_and_sign;

/// JOSE `typ` header for OBO tokens.
pub const OBO_TYP: &str = "obo+jwt";

/// Claims carried inside a minted OBO token (DESIGN.md § 3.1).
// These claims ARE the JWT payload: serialized at signing, deserialized by every
// verifier.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OboClaims {
    /// OBO issuer identifier.
    pub iss: String,
    /// Audience — the public edge (`public-api`).
    pub aud: String,
    /// Subject (the original caller user id), copied from the cap token.
    pub sub: Uuid,
    /// Optional user type, copied from the cap token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,
    /// Tenant the subject acts in, copied from the cap token's `subject_tenant`.
    pub tenant_id: Uuid,
    /// Actor: the adapter (GTS ID) acting on behalf of the subject.
    pub act: String,
    /// Down-scoped grant (space-joined; never `*`).
    pub scope: String,
    /// Unique token identifier (fresh per mint).
    pub jti: Uuid,
    /// Issued-at time (Unix seconds).
    pub iat: i64,
    /// Expiry time (Unix seconds).
    pub exp: i64,
}

/// Builds OBO claims from a verified cap token and a Gate-2 down-scoped grant.
///
/// Identity is copied verbatim from the cap; `tenant_id` comes from the cap's
/// `subject_tenant`; `act` is the acting adapter; `scope` is the space-joined
/// grant. `granted` must already be down-scoped (the wildcard is debug-asserted
/// absent). `exp` is `now + ttl`, decoupled from the cap's `exp` (DESIGN.md
/// § 3.1, § 2.1): the OBO carries identity, not authorization — the PDP re-checks live
/// permissions — so a re-mint near cap expiry still yields a full-TTL OBO instead
/// of a near-zero (or, within clock skew, already-expired) one.
#[must_use]
pub fn build_obo_claims(
    cap: &CapabilityClaims,
    granted: &[String],
    adapter_gts: &str,
    obo_iss: &str,
    aud: &str,
    ttl: u64,
    now: i64,
) -> OboClaims {
    debug_assert!(
        !granted.iter().any(|s| s == "*"),
        "OBO scope must never contain the wildcard"
    );
    OboClaims {
        iss: obo_iss.to_owned(),
        aud: aud.to_owned(),
        sub: cap.sub,
        user_type: cap.user_type.clone(),
        tenant_id: cap.subject_tenant,
        act: adapter_gts.to_owned(),
        scope: granted.join(" "),
        jti: Uuid::new_v4(),
        iat: now,
        exp: now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)),
    }
}

/// Assembles and ES256-signs an OBO JWT for `claims` using the `obo_key` via the
/// signing port. Header: `{ alg:ES256, typ:obo+jwt, kid:{obo_key}-v{version} }`.
///
/// # Errors
/// Returns [`TokenIssuerError`] if claim serialization or signing fails.
pub async fn sign_obo(
    signer: &dyn SigningClientV1,
    ctx: &SecurityContext,
    obo_key: &SigningKeyRef,
    claims: &OboClaims,
) -> Result<String, TokenIssuerError> {
    assemble_and_sign(signer, ctx, obo_key, OBO_TYP, claims, |_| {}).await
}

#[cfg(test)]
#[path = "obo_tests.rs"]
mod tests;
