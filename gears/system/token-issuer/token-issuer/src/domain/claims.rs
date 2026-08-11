//! Capability-claim assembly and scope canonicalization.

// DE0708 (`no_non_fips_hasher`) — SHA-256 in this module only folds the
// canonical scope set into a fixed-size cache key; it is not used for any
// cryptographic / integrity purpose, so the non-FIPS RustCrypto hasher is
// acceptable. Scoped to this module (narrowest scope that covers the import).
#![allow(unknown_lints)]
#![allow(
    de0708_no_non_fips_hasher,
    reason = "scope-set cache key; non-cryptographic"
)]

use sha2::{Digest, Sha256};
use token_issuer_sdk::{
    CapabilityClaims, GrantClaims, MintCapabilityRequest, MintGrantRequest, TokenIssuerError,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::cache::CacheKey;

/// Canonicalizes a space-delimited scope string: whitespace-split, dedup,
/// lexicographic (byte) sort, single-space join.
///
/// Two callers asserting the same scope set always produce identical output, so
/// the result is safe to hash into a cache key.
#[must_use]
pub fn canonical_scopes(scopes: &str) -> String {
    let mut v: Vec<&str> = scopes.split_whitespace().collect();
    v.sort_unstable();
    v.dedup();
    v.join(" ")
}

/// SHA-256 of an already-canonicalized scope string.
#[must_use]
pub fn scopes_hash(canonical: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher.finalize().into()
}

/// Derives the cache key for a set of capability claims.
///
/// The key is the 8-tuple `(sub, subject_tenant, context_tenant,
/// context_project_id, aud, sha256(scopes), operation, resource_type)`; `scopes`
/// is hashed as-is (claims already carry the canonical form). `operation` and
/// `resource_type` are baked into the signed token, so they must be part of the
/// key — otherwise requests differing only in those fields would collapse onto a
/// cached token whose claims don't match the request.
#[must_use]
pub fn cache_key_for(c: &CapabilityClaims) -> CacheKey {
    CacheKey {
        sub: c.sub,
        subject_tenant: c.subject_tenant,
        context_tenant: c.context_tenant,
        context_project_id: c.context_project_id,
        aud: c.aud.clone(),
        scopes_hash: scopes_hash(&c.scopes),
        operation: c.operation.clone(),
        resource_type: c.resource_type.clone(),
    }
}

/// Builds capability claims from the verified caller context and mint request.
///
/// Identity (`sub`, `subject_tenant`, `user_type`) and `scopes` come only from
/// the [`SecurityContext`]; the request supplies the audience, tenant/project
/// context, and audit hints. `act` is left `None` (set by the caller when
/// acting on behalf of another principal).
///
/// By design the cap carries the caller's *full* token scopes — it is not
/// down-scoped here. Narrowing to what an adapter may act on happens at OBO
/// re-mint time (Gate 2, see [`crate::domain::downscope`]).
///
/// # Errors
/// Returns [`TokenIssuerError::Internal`] if the expiration timestamp overflows.
pub fn build_cap_claims(
    ctx: &SecurityContext,
    req: &MintCapabilityRequest,
    issuer: &str,
    ttl_secs: u64,
    now: i64,
) -> Result<CapabilityClaims, TokenIssuerError> {
    Ok(CapabilityClaims {
        iss: issuer.to_owned(),
        aud: req.audience.clone(),
        sub: ctx.subject_id(),
        subject_tenant: ctx.subject_tenant_id(),
        user_type: ctx.subject_type().map(str::to_owned),
        context_tenant: req.context_tenant,
        context_project_id: req.context_project_id,
        scopes: canonical_scopes(&ctx.token_scopes().join(" ")),
        jti: Uuid::new_v4(),
        iat: now,
        exp: checked_expiration(now, ttl_secs)?,
        act: None,
        operation: req.operation.clone(),
        resource_type: req.resource_type.clone(),
    })
}

/// Builds grant claims from the verified caller context and mint request.
///
/// Identity (`sub`, `subject_tenant`) comes only from the [`SecurityContext`]; the
/// resolved resource identity, operations, `context_tenant`, optional `project_id`,
/// and audience come from the request. `exp` is `now + ttl_secs` (the gear has
/// already clamped `ttl_secs` to the smallest per-operation `max_ttl`). The token
/// carries no `nbf`.
///
/// # Errors
/// Returns [`TokenIssuerError::Internal`] if the expiration timestamp overflows.
pub fn build_grant_claims(
    ctx: &SecurityContext,
    req: &MintGrantRequest,
    issuer: &str,
    now: i64,
) -> Result<GrantClaims, TokenIssuerError> {
    Ok(GrantClaims {
        iss: issuer.to_owned(),
        aud: req.audience.clone(),
        sub: ctx.subject_id(),
        subject_tenant: ctx.subject_tenant_id(),
        context_tenant: req.context_tenant,
        project_id: req.project_id,
        resource_id: req.resource_id,
        resource_name: req.resource_name.clone(),
        resource_type: req.resource_type.clone(),
        operations: req.operations.clone(),
        iat: now,
        exp: checked_expiration(now, req.ttl_secs)?,
        jti: Uuid::new_v4(),
    })
}

/// Converts a bounded TTL to an absolute expiration without saturating.
///
/// # Errors
/// Returns [`TokenIssuerError::Internal`] when the clock plus TTL cannot be
/// represented as an `i64` Unix timestamp.
pub(crate) fn checked_expiration(now: i64, ttl_secs: u64) -> Result<i64, TokenIssuerError> {
    let ttl = i64::try_from(ttl_secs)
        .map_err(|_| TokenIssuerError::Internal("token TTL is not representable".to_owned()))?;
    now.checked_add(ttl)
        .ok_or_else(|| TokenIssuerError::Internal("token expiration timestamp overflow".to_owned()))
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
