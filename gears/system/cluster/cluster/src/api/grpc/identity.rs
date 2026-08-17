// Created: 2026-08-12 by Constructor Tech
//! Caller identity for an inbound coordination RPC (DESIGN-DEPLOYABLE-GEAR §4.6).
//!
//! Cluster is **platform-plane** infrastructure: coordination state is not
//! tenant-scoped, so a call carries an `InternalCredential` and the server
//! resolves a [`PlatformSecurityContext`] from it. There is no tenant `AuthZ` and no
//! tenant `SecurityContext` anywhere on this path.
//!
//! # The credential is read from platform-plane metadata, never `x-secctx-bin`
//!
//! [`extract_internal_token_grpc`] reads `x-toolkit-internal-token`.
//! `attach_secctx`/`x-secctx-bin` is scoped to *in-process* gRPC metadata in
//! Profile 1, and ADR-0008 drops it from the cross-process contract entirely, so
//! this module never touches it (§4.6).
//!
//! # v1 ships without enforcement, by decision
//!
//! **The first deployable version serves this port with no inbound authenticator**
//! ([`CallerAuthentication::TrustedNetwork`]), so every caller is reported as
//! `unauthenticated` and the `NetworkPolicy` is the only boundary. That is a
//! recorded deployment decision rather than an oversight, and it is stated in the
//! operator documentation rather than left to be discovered (`D-26`).
//!
//! The retrofit lands at the **framework** level: the platform-plane check belongs
//! in the generated gRPC server projection, which is where `#[toolkit::grpc_contract]`
//! currently omits it. A gear *could* build its own validator instead — nothing
//! prevents it, and `gear-orchestrator` does exactly that — but the credential
//! belongs to the process, and a second `InternalAuthConfig` beside the one the
//! operator already writes at `oop_http.internal_auth` is one that can silently
//! disagree with it. So cluster waits for the framework rather than duplicating the
//! configuration.
//!
//! **What is built and waiting** is [`CallerAuthentication::Validated`], which takes
//! a [`DynInternalAuthenticator`] from wherever it eventually arrives. Both modes are
//! implemented and tested, and every service impl takes the resolved [`Caller`]
//! rather than the metadata — so switching modes is one construction site and
//! nothing above [`CallerResolver::resolve`] moves.
//!
//! # Lease ownership, and the cross-check the backend will not do
//!
//! The lease methods on the plugin-facing traits are **token-only**: they
//! predicate on `(name, owner, fence, deadline)` and know nothing about who is
//! connected (§5.8.1). Verifying that the *transport* caller is `token.owner` is
//! therefore the serving gear's authorization decision, and it lives here.
//!
//! An owner is `{caller}/{nonce}` (see [`Caller::mint_owner`]). Both halves earn
//! their place:
//!
//! - the **caller** half is what makes the cross-check possible at all, and it is
//!   the `ClientId` §4.6 specifies;
//! - the **nonce** half is what keeps a token unguessable. `fence` counts from 1
//!   and a lock name is often well known, so an owner of just the caller's name
//!   would let one replica of a gear forge a sibling replica's token by guessing a
//!   small integer. It also makes two replicas of one workload distinct holders,
//!   which is what a distributed lock between them requires.

use std::fmt;

use cluster_sdk::lease::LeaseToken;
use secrecy::ExposeSecret;
use tonic::Status;
use tonic::metadata::MetadataMap;
use toolkit_security::{
    DynInternalAuthenticator, InternalAuthNError, InternalAuthenticator, PlatformIdentity,
    PlatformSecurityContext,
};
use toolkit_transport_grpc::extract_internal_token_grpc;
use uuid::Uuid;

/// The caller name reported when no inbound authenticator is configured.
///
/// Every caller shares it, so the ownership cross-check degenerates to a no-op
/// and the nonce is all that separates two holders' tokens. That is the honest
/// consequence of shipping v1 without enforcement (see the [module docs](self)),
/// and it is why [`CallerAuthentication::TrustedNetwork`] is a named mode rather
/// than a silent fallback.
pub const UNAUTHENTICATED_CALLER: &str = "unauthenticated";

/// Separates the caller name from the per-acquisition nonce inside an owner
/// string. Neither a Kubernetes `ServiceAccount` name nor a SPIFFE workload
/// component may contain it, so the split is unambiguous.
const OWNER_SEPARATOR: char = '/';

/// How an inbound credential becomes a caller identity.
#[derive(Clone)]
pub enum CallerAuthentication {
    /// The credential is required and validated, and the caller's identity is
    /// whatever the authenticator resolves. This is the target state.
    Validated(DynInternalAuthenticator),
    /// **No inbound authenticator is configured.** The credential is read but not
    /// validated, and its absence is not an error, so the boundary is the
    /// `NetworkPolicy` on the coordination port and nothing else (Risk 2).
    ///
    /// Deployment must state this consequence rather than let it pass silently
    /// (`D-26`); the mode exists so the choice is visible in the code and in a
    /// startup log rather than being inferred from an absent field.
    TrustedNetwork,
}

impl fmt::Debug for CallerAuthentication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Validated(_) => f.write_str("Validated(..)"),
            Self::TrustedNetwork => f.write_str("TrustedNetwork"),
        }
    }
}

/// Resolves the caller of one RPC from its metadata.
#[derive(Debug, Clone)]
pub struct CallerResolver {
    mode: CallerAuthentication,
}

impl CallerResolver {
    /// A resolver that validates every inbound credential.
    #[must_use]
    pub fn validated(authenticator: DynInternalAuthenticator) -> Self {
        Self {
            mode: CallerAuthentication::Validated(authenticator),
        }
    }

    /// A resolver for a deployment with no inbound authenticator, logging the
    /// consequence once at construction.
    #[must_use]
    pub fn trusted_network() -> Self {
        tracing::warn!(
            "cluster: the coordination port has no inbound authenticator; every caller is \
             reported as `{UNAUTHENTICATED_CALLER}` and the NetworkPolicy is the only boundary \
             (DESIGN section 4.6, ask A1)"
        );
        Self {
            mode: CallerAuthentication::TrustedNetwork,
        }
    }

    /// The mode this resolver runs in, for diagnostics and readiness reporting.
    #[must_use]
    pub fn mode(&self) -> &CallerAuthentication {
        &self.mode
    }

    /// Resolves the caller behind `metadata`.
    ///
    /// # Errors
    /// [`Status::unauthenticated`] when a credential is required and is missing,
    /// malformed or rejected; [`Status::unavailable`] when the validation backend
    /// itself is unreachable, which is a transient failure of *cluster's*
    /// dependency rather than a decision about the caller.
    pub async fn resolve(&self, metadata: &MetadataMap) -> Result<Caller, Status> {
        // Read the credential the same way in both modes: the difference is what
        // is done with it, not where it is found.
        let token = extract_internal_token_grpc(metadata);

        let identity = match self.mode {
            CallerAuthentication::Validated(ref authenticator) => {
                let token = token?;
                authenticator
                    .authenticate(token.expose_secret())
                    .await
                    .map_err(|error| authn_error_to_status(&error))?
            }
            // An absent credential is accepted here on purpose: with nothing
            // validating it, requiring it would reject the honest caller and
            // admit the dishonest one, which is worse than admitting both.
            CallerAuthentication::TrustedNetwork => PlatformIdentity::Shared {
                name: UNAUTHENTICATED_CALLER.to_owned(),
            },
        };

        Ok(Caller {
            ctx: PlatformSecurityContext::new(identity),
        })
    }
}

/// A neutral platform-plane failure becomes a status the caller can act on.
///
/// [`InternalAuthNError::Unavailable`] is the one that must **not** be
/// `Unauthenticated`: the caller's credential may be perfectly good and the
/// validation backend simply unreachable, so it is a retryable `Unavailable`
/// against cluster, exactly as `Provider{ConnectionLost}` is (§6.9).
fn authn_error_to_status(error: &InternalAuthNError) -> Status {
    match *error {
        InternalAuthNError::Unavailable => Status::unavailable("internal-auth backend unavailable"),
        // The message is deliberately coarse and never echoes the credential.
        // The catch-all is required — the enum is `#[non_exhaustive]` and defined
        // in another crate — and `unauthenticated` is the safe default: a
        // validation outcome this build does not understand must not be read as
        // success.
        InternalAuthNError::InvalidToken | InternalAuthNError::Other(_) | _ => {
            Status::unauthenticated("invalid internal credential")
        }
    }
}

/// The authenticated caller of one RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    ctx: PlatformSecurityContext,
}

impl Caller {
    /// Wraps an already-resolved context — the seam a future framework
    /// interceptor hands its result through, and what the tests construct.
    #[must_use]
    pub fn new(ctx: PlatformSecurityContext) -> Self {
        Self { ctx }
    }

    /// The platform-plane context, as the contract traits name it (§6.2).
    #[must_use]
    pub fn context(&self) -> &PlatformSecurityContext {
        &self.ctx
    }

    /// The caller's `ClientId` — the name half of every lease this caller owns.
    #[must_use]
    pub fn name(&self) -> &str {
        self.ctx.identity().peer_name()
    }

    /// Mints the owner string for one acquisition (see the [module docs](self)).
    ///
    /// Fresh per acquisition, never per caller: two acquisitions of *different*
    /// names by one caller must not share an owner, or a `release` of one would
    /// match the other's record. A v4 UUID makes a collision cryptographically
    /// improbable, which is the same basis the in-process defaults' holder marker
    /// rests on.
    #[must_use]
    pub fn mint_owner(&self) -> String {
        format!("{}{OWNER_SEPARATOR}{}", self.name(), Uuid::new_v4())
    }

    /// Whether `token` was minted for this caller.
    ///
    /// The caller half of the owner must match; the nonce is not this decision's
    /// business. A token whose owner carries no separator was not minted by this
    /// service — an in-process holder marker, or a fabrication — and is not this
    /// caller's either way.
    ///
    /// **What each caller does with a `false` differs, and neither may leak.** A
    /// renewal reports [`ClusterError::LockExpired`], which is what a token
    /// matching nothing already reports, so a caller cannot use `renew` to
    /// discover that a *live* lease exists under another owner. A release or
    /// resignation returns `Ok` having done nothing, which is what an absent
    /// record already returns (§6.10, §12.6).
    ///
    /// [`ClusterError::LockExpired`]: cluster_sdk::ClusterError::LockExpired
    #[must_use]
    pub fn owns(&self, token: &LeaseToken) -> bool {
        token
            .owner
            .rsplit_once(OWNER_SEPARATOR)
            .is_some_and(|(caller, _nonce)| caller == self.name())
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod identity_tests;
