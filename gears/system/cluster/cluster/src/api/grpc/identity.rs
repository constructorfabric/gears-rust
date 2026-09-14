//! Caller identity for an inbound coordination RPC (DESIGN.md).
//!
//! Cluster is **platform-plane** infrastructure: coordination state is not
//! tenant-scoped, so a call carries an `InternalCredential` and the server acts on
//! the [`PlatformSecurityContext`] resolved from it. There is no tenant `AuthZ` and
//! no tenant `SecurityContext` anywhere on this path.
//!
//! # The identity is stamped by the transport, read from the request extensions
//!
//! Authentication is **not** performed in this handler. The platform-plane check
//! lives in `grpc-hub`'s `InternalAuthGrpcLayer`, which validates
//! `x-toolkit-internal-token` on every inbound RPC and, on success, inserts a
//! [`PlatformSecurityContext`] into the request extensions
//! (`cpt-cf-adr-platform-plane-auth`). [`CallerResolver`] reads that extension; it
//! never touches the raw credential, and `x-secctx-bin` — scoped to *in-process*
//! Profile 1 metadata and dropped from the cross-process contract by ADR-0008 — is
//! not on this path either (§4.6).
//!
//! # Enforcement is a grpc-hub decision, not cluster's
//!
//! When the layer stamped no context, the caller falls back to
//! [`UNAUTHENTICATED_CALLER`]. That happens in two cases, and both are correct
//! here: enforcement is disabled (`grpc-hub`'s `gears.grpc-hub.config.internal_auth`
//! unset — the in-process / trusted-network profile where the process boundary is
//! the trust root), or a `Permissive` listener let an anonymous call through. The
//! fallback is byte-for-byte the pre-retrofit `TrustedNetwork` behaviour, so
//! turning enforcement on is a single `grpc-hub` config flip and nothing above
//! [`CallerResolver::resolve`] moves. The credential belongs to the *process*, and
//! `grpc-hub` owns it once for every gear it serves — which is why cluster reads
//! the result rather than configuring a second authenticator of its own.
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
//!   which a distributed lock between them requires.

use cluster_sdk::lease::LeaseToken;
use tonic::{Request, Status};
use toolkit_security::{PlatformIdentity, PlatformSecurityContext};
use uuid::Uuid;

/// The caller name reported when the platform-plane layer stamped no identity.
///
/// Every such caller shares it, so the ownership cross-check degenerates to a
/// no-op and the nonce is all that separates two holders' tokens. That is the
/// honest consequence of serving with enforcement disabled (see the [module
/// docs](self)): a coordination port fronted only by a `NetworkPolicy` cannot tell
/// its callers apart, and this constant names that outcome rather than hiding it.
pub const UNAUTHENTICATED_CALLER: &str = "unauthenticated";

/// Separates the caller name from the per-acquisition nonce inside an owner
/// string. Neither a Kubernetes `ServiceAccount` name nor a SPIFFE workload
/// component may contain it, so the split is unambiguous.
const OWNER_SEPARATOR: char = '/';

/// The seam a service handler resolves its caller through — a namespace, not
/// state.
///
/// It holds nothing, because authentication — and the decision to enforce it at
/// all — now lives entirely in `grpc-hub`'s `InternalAuthGrpcLayer`.
/// [`CallerResolver::resolve`] reads that layer's result, and nothing above it
/// changes when the deployment turns enforcement on.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallerResolver;

impl CallerResolver {
    /// Resolves the caller behind `request`.
    ///
    /// Reads the [`PlatformSecurityContext`] the platform-plane layer stamped into
    /// the request extensions, falling back to [`UNAUTHENTICATED_CALLER`] when the
    /// layer stamped none — enforcement disabled, or a permissive anonymous call.
    /// An absent context is accepted on purpose: with nothing upstream validating a
    /// credential, rejecting here would refuse the honest caller and admit the
    /// dishonest one, which is worse than admitting both.
    ///
    /// # Errors
    /// Infallible today. The `Result<Caller, Status>` is kept so the four service
    /// handlers' call sites stay stable if a future revision needs to reject here.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Result is the stable seam the four service handlers call \
                  through; keeping it infallible-but-fallible avoids churning every \
                  call site the day enforcement grows a reject path"
    )]
    pub fn resolve<T>(request: &Request<T>) -> Result<Caller, Status> {
        let ctx = request
            .extensions()
            .get::<PlatformSecurityContext>()
            .cloned()
            .unwrap_or_else(|| {
                PlatformSecurityContext::new(PlatformIdentity::Shared {
                    name: UNAUTHENTICATED_CALLER.to_owned(),
                })
            });
        Ok(Caller::new(ctx))
    }
}

/// The authenticated caller of one RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    ctx: PlatformSecurityContext,
}

impl Caller {
    /// Wraps an already-resolved context — the seam the platform-plane layer's
    /// result is handed through, and what the tests construct.
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
    /// renewal reports [`ClusterError::LockExpired`], which a token
    /// matching nothing already reports, so a caller cannot use `renew` to
    /// discover that a *live* lease exists under another owner. A release or
    /// resignation returns `Ok` having done nothing, which an absent
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
