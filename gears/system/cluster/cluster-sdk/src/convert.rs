// Created: 2026-08-12 by Constructor Tech
//! The `ClusterError` ⇄ `CanonicalError` codec — DESIGN-DEPLOYABLE-GEAR §6.9, §12.2.
//!
//! **Two-layer model.** [`ClusterError`] stays the frozen Rust-facing contract
//! consumers match on (invariant I3 — no new variants, no widened fields).
//! `CanonicalError`'s RFC 9457 [`Problem`] envelope is what crosses the boundary.
//! [`ClusterWireError`] is the typed enum in between, and
//! `#[derive(ContractError)]` generates its two `Problem` conversions, so cluster
//! defines no bespoke error DTO and no hand-written code-mapping table.
//!
//! # Why this module is unfeatured
//!
//! The **encode** direction runs in the cluster gear, which links no client
//! (§6.9). Only the decode direction has a remote caller, and it costs nothing to
//! compile alongside.
//!
//! # The mapping is not injective, and that is the whole difficulty
//!
//! Three canonical categories are reached by two `ClusterError` variants each:
//!
//! | Category | Reached by | Distinguished by |
//! |---|---|---|
//! | `ServiceUnavailable` | `Shutdown`, `Provider{ConnectionLost}` | `error_code` |
//! | `DeadlineExceeded` | `LockTimeout`, `Provider{Timeout}` | `error_code` |
//! | `Internal` | `Provider{AuthFailure}`, `Provider{Other}` | `error_code` |
//!
//! `Shutdown` is terminal while `Provider{ConnectionLost}` is retryable, and
//! [`RestartingWatch`](crate::restart::RestartingWatch) reads retryability from
//! [`ProviderErrorKind`]. Inferring the kind from the canonical category would
//! therefore make the auto-restart combinator retry a shutdown forever. So
//! **[`ProviderErrorKind`] travels explicitly**: [`ClusterWireError`] carries one
//! variant per kind, each with its own `error_code`, and the derived
//! `TryFrom<Problem>` keys on `(error_domain, error_code)` and never on the
//! category.
//!
//! Splitting `Provider` per kind is what makes §6.9's table expressible at all —
//! `#[derive(ContractError)]` takes one `#[canonical(..)]` per *variant*, and the
//! five kinds map to four different categories. It also keeps
//! `cpt-cf-clst-constraint-no-serde` intact: nothing here requires a serde derive
//! on [`ProviderErrorKind`] itself, nor a mirror of it.
//!
//! # A lease-keyed `NotFound` is not a cache miss
//!
//! `renew` / `release` / `resign` predicate on `(name, owner, fence, deadline)`,
//! so "zero rows" covers expired, released and fenced-out alike — deliberately
//! indistinguishable, so tokens are not probeable (§5.8.1). Which
//! [`ClusterError`] that becomes depends on **which operation asked**, which is
//! why [`to_cluster_error`] takes a [`LeaseContext`] and is a real function rather
//! than a `From` impl on the canonical variant.
//!
//! The mapping applies only when the code is **unrecognised**, never over a typed
//! answer. §6.2 has the server return `lock_expired` for a lease predicate that
//! matched nothing, so [`LeaseContext`] is the defence for a *bare* canonical
//! `NotFound` — one arriving from an intermediary, or from a peer that did not
//! type its error — rather than the normal path. Letting it override a typed code
//! would make it possible for the caller's guess to contradict the server.
//!
//! # Both conversion directions match exhaustively, deliberately
//!
//! [`ClusterError`] and [`ClusterWireError`] are both `#[non_exhaustive]`, which
//! constrains downstream crates but not this one. So there is no catch-all arm in
//! either `From` impl: adding a variant to either enum **fails this build**, which
//! for a frozen error model (invariant I3) and an additive-only wire (invariant
//! I12) is the outcome to want. A catch-all would turn the same change into a
//! variant silently collapsing to `Provider{Other}` — retryability and all —
//! discovered in production rather than at compile time.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use toolkit_canonical_errors::{Problem, ProblemCategory};
use toolkit_contract::ContractError;
#[cfg(feature = "grpc-client")]
use toolkit_contract::runtime::transport_error::TransportError;

use crate::cache::{CacheEntry, CacheEvent, CacheWatchEvent};
use crate::dto::{
    CacheEntryDto, CacheWatchEventDto, CacheWatchEventKind, LeaderWatchEventDto,
    LeaderWatchEventKind, WireError,
};
use crate::error::{ClusterError, ProviderErrorKind};
use crate::intern::intern;
use crate::leader::LeaderWatchEvent;

/// The error domain every cluster wire error carries.
pub const CLUSTER_ERROR_DOMAIN: &str = "cluster.v1";

/// The typed wire form of [`ClusterError`].
///
/// One variant per `ClusterError` variant, except that `Provider` fans out into
/// five — see the [module docs](self). Payload fields are serialised into
/// `context["data"]` by the derive, and an unknown
/// `(error_domain, error_code)` pair from a newer peer bounces back as the
/// original [`Problem`] rather than panicking or being silently dropped, which is
/// exactly §6.11's skew rule.
///
/// `&'static str` fields on [`ClusterError`] become `String` here and are
/// re-promoted by [`intern`] on the way back, which is what keeps the frozen
/// error model frozen (invariant I3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ContractError)]
#[error_domain("cluster.v1")]
#[non_exhaustive]
pub enum ClusterWireError {
    /// [`ClusterError::CapabilityNotMet`].
    #[error_code("capability_not_met")]
    #[canonical(FailedPrecondition)]
    CapabilityNotMet {
        /// The primitive being resolved.
        primitive: String,
        /// The declared capability that is unmet.
        capability: String,
        /// The **server-side** provider that cannot satisfy it.
        provider: String,
    },

    /// [`ClusterError::ProfileNotBound`].
    #[error_code("profile_not_bound")]
    #[canonical(NotFound)]
    ProfileNotBound {
        /// The profile with no bound backend.
        profile: String,
    },

    /// [`ClusterError::ProfileNotSpecified`].
    #[error_code("profile_not_specified")]
    #[canonical(InvalidArgument)]
    ProfileNotSpecified,

    /// [`ClusterError::InvalidName`].
    #[error_code("invalid_name")]
    #[canonical(InvalidArgument)]
    InvalidName {
        /// The offending value.
        name: String,
        /// The rule the value must satisfy.
        reason: String,
    },

    /// [`ClusterError::InvalidConfig`].
    #[error_code("invalid_config")]
    #[canonical(InvalidArgument)]
    InvalidConfig {
        /// A human-readable description of the misconfiguration.
        reason: String,
    },

    /// [`ClusterError::LockContended`].
    #[error_code("lock_contended")]
    #[canonical(Aborted)]
    LockContended {
        /// The contended lock name.
        name: String,
    },

    /// [`ClusterError::LockTimeout`]. `waited` is populated server-side, because
    /// the server is what did the waiting (§6.9).
    #[error_code("lock_timeout")]
    #[canonical(DeadlineExceeded)]
    LockTimeout {
        /// The lock that was not acquired in time.
        name: String,
        /// How long the acquisition blocked, in milliseconds.
        waited_ms: u64,
    },

    /// [`ClusterError::LockExpired`].
    #[error_code("lock_expired")]
    #[canonical(FailedPrecondition)]
    LockExpired {
        /// The lock name whose lease is gone.
        name: String,
    },

    /// [`ClusterError::Unsupported`].
    #[error_code("unsupported")]
    #[canonical(Unimplemented)]
    Unsupported {
        /// The unsupported feature.
        feature: String,
    },

    /// [`ClusterError::CasConflict`].
    ///
    /// `current` is **SHOULD** on the Rust contract ("when cheaply obtainable"),
    /// and the wire keeps that latitude: the two `current_*` fields are
    /// independently optional, so decision 17a — whether the server pays to ship
    /// the full entry or only its version — is a **behavioural** choice the server
    /// makes per response, not a wire change in either direction (invariant I12).
    /// The decoder reconstructs [`CacheEntry`] only when both are present, since
    /// [`CacheEntry`] has no representation for "version known, value not"; a
    /// version-only conflict therefore decodes as `current: None` and the caller
    /// re-reads, which §6.9 states is contract-legal.
    #[error_code("cas_conflict")]
    #[canonical(Aborted)]
    CasConflict {
        /// The key whose compare-and-swap failed.
        key: String,
        /// The current version, when the server reports one.
        current_version: Option<u64>,
        /// The current value, when the server chose to pay for it.
        current_value: Option<Vec<u8>>,
    },

    /// [`ClusterError::Shutdown`]. Shares `ServiceUnavailable` with
    /// `ProviderConnectionLost` and is distinguished from it by `error_code` —
    /// this one is terminal (ADR-003).
    #[error_code("shutdown")]
    #[canonical(ServiceUnavailable)]
    Shutdown,

    /// `Provider{ConnectionLost}` — retryable.
    #[error_code("provider_connection_lost")]
    #[canonical(ServiceUnavailable)]
    ProviderConnectionLost {
        /// A human-readable description of the provider failure.
        message: String,
    },

    /// `Provider{Timeout}` — retryable.
    #[error_code("provider_timeout")]
    #[canonical(DeadlineExceeded)]
    ProviderTimeout {
        /// A human-readable description of the provider failure.
        message: String,
    },

    /// `Provider{ResourceExhausted}` — retryable with backoff.
    #[error_code("provider_resource_exhausted")]
    #[canonical(ResourceExhausted)]
    ProviderResourceExhausted {
        /// A human-readable description of the provider failure.
        message: String,
    },

    /// `Provider{AuthFailure}` — **`Internal`, not `Unauthenticated`**.
    ///
    /// The failure is the *cluster gear's* credentials against Postgres or Redis,
    /// not the caller's against cluster. `Unauthenticated` would send the
    /// platform's internal-auth interceptors and retry middleware down a
    /// token-refresh path that cannot help, and would point the operator at the
    /// wrong credential (§6.9).
    #[error_code("provider_auth_failure")]
    #[canonical(Internal)]
    ProviderAuthFailure {
        /// A human-readable description of the provider failure.
        message: String,
    },

    /// `Provider{Other}`. Also where an unrecognised `(domain, code)` pair from a
    /// newer server lands after decode (§6.11).
    #[error_code("provider_other")]
    #[canonical(Internal)]
    ProviderOther {
        /// A human-readable description of the provider failure.
        message: String,
    },
}

impl From<ClusterError> for ClusterWireError {
    fn from(value: ClusterError) -> Self {
        match value {
            ClusterError::CapabilityNotMet {
                primitive,
                capability,
                provider,
            } => Self::CapabilityNotMet {
                primitive: primitive.to_owned(),
                capability: capability.to_owned(),
                provider: provider.to_owned(),
            },
            ClusterError::ProfileNotBound { profile } => Self::ProfileNotBound {
                profile: profile.to_owned(),
            },
            ClusterError::ProfileNotSpecified => Self::ProfileNotSpecified,
            ClusterError::InvalidName { name, reason } => Self::InvalidName {
                name,
                reason: reason.to_owned(),
            },
            ClusterError::InvalidConfig { reason } => Self::InvalidConfig { reason },
            ClusterError::LockContended { name } => Self::LockContended { name },
            ClusterError::LockTimeout { name, waited } => Self::LockTimeout {
                name,
                waited_ms: duration_to_ms(waited),
            },
            ClusterError::LockExpired { name } => Self::LockExpired { name },
            ClusterError::Unsupported { feature } => Self::Unsupported {
                feature: feature.to_owned(),
            },
            ClusterError::CasConflict { key, current } => {
                let (current_version, current_value) = match current {
                    Some(entry) => (Some(entry.version), Some(entry.value)),
                    None => (None, None),
                };
                Self::CasConflict {
                    key,
                    current_version,
                    current_value,
                }
            }
            ClusterError::Shutdown => Self::Shutdown,
            ClusterError::Provider { kind, message } => match kind {
                ProviderErrorKind::ConnectionLost => Self::ProviderConnectionLost { message },
                ProviderErrorKind::Timeout => Self::ProviderTimeout { message },
                ProviderErrorKind::ResourceExhausted => Self::ProviderResourceExhausted { message },
                ProviderErrorKind::AuthFailure => Self::ProviderAuthFailure { message },
                ProviderErrorKind::Other => Self::ProviderOther { message },
            },
        }
    }
}

impl From<ClusterWireError> for ClusterError {
    fn from(value: ClusterWireError) -> Self {
        match value {
            ClusterWireError::CapabilityNotMet {
                primitive,
                capability,
                provider,
            } => Self::CapabilityNotMet {
                primitive: intern(&primitive),
                capability: intern(&capability),
                provider: intern(&provider),
            },
            ClusterWireError::ProfileNotBound { profile } => Self::ProfileNotBound {
                profile: intern(&profile),
            },
            ClusterWireError::ProfileNotSpecified => Self::ProfileNotSpecified,
            ClusterWireError::InvalidName { name, reason } => Self::InvalidName {
                name,
                reason: intern(&reason),
            },
            ClusterWireError::InvalidConfig { reason } => Self::InvalidConfig { reason },
            ClusterWireError::LockContended { name } => Self::LockContended { name },
            ClusterWireError::LockTimeout { name, waited_ms } => Self::LockTimeout {
                name,
                waited: Duration::from_millis(waited_ms),
            },
            ClusterWireError::LockExpired { name } => Self::LockExpired { name },
            ClusterWireError::Unsupported { feature } => Self::Unsupported {
                feature: intern(&feature),
            },
            ClusterWireError::CasConflict {
                key,
                current_version,
                current_value,
            } => Self::CasConflict {
                key,
                // Both or neither: `CacheEntry` cannot express "version known,
                // value not", so a version-only conflict decodes as absent and
                // the caller re-reads (see the variant's docs).
                current: current_version.zip(current_value).map(|(version, value)| {
                    CacheEntry::from(CacheEntryDto {
                        value,
                        version,
                        expires_at_ms: None,
                    })
                }),
            },
            ClusterWireError::Shutdown => Self::Shutdown,
            ClusterWireError::ProviderConnectionLost { message } => Self::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message,
            },
            ClusterWireError::ProviderTimeout { message } => Self::Provider {
                kind: ProviderErrorKind::Timeout,
                message,
            },
            ClusterWireError::ProviderResourceExhausted { message } => Self::Provider {
                kind: ProviderErrorKind::ResourceExhausted,
                message,
            },
            ClusterWireError::ProviderAuthFailure { message } => Self::Provider {
                kind: ProviderErrorKind::AuthFailure,
                message,
            },
            ClusterWireError::ProviderOther { message } => Self::Provider {
                kind: ProviderErrorKind::Other,
                message,
            },
        }
    }
}

/// Which lease-keyed operation is asking, for the reverse mapping a bare
/// canonical `NotFound` needs (§6.9).
///
/// A lease predicate that matches nothing is reported as "zero rows" — one server
/// answer covering expired, released and fenced-out alike. The [`ClusterError`]
/// the consumer can act on differs by operation, and only the caller knows which
/// operation it made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaseContext<'a> {
    /// Not a lease-keyed call. A `NotFound` here is decoded as-is.
    None,
    /// A lock renewal. The guard holds the name, so [`ClusterError::LockExpired`]
    /// is constructible, and DESIGN §3.3 pattern C already establishes it as the
    /// authoritative loss signal.
    LockRenew {
        /// The lock name the guard holds.
        name: &'a str,
    },
    /// A lock release, or an election resignation: idempotent by absence, so
    /// there is no error to map (§6.10).
    LeaseRelease,
    /// An election renewal. Absence means this instance no longer holds the
    /// claim — **not** an error to the caller. The event pump turns it into
    /// `Status(Lost)` and keeps the subscription open (§6.6, §12.12).
    ElectionRenew {
        /// The election name.
        name: &'a str,
    },
    /// An `await_change` against a subscription whose replica went away. Terminal
    /// and non-retryable, so `RestartingWatch` propagates rather than
    /// resubscribing (§6.9).
    ElectionSubscription,
}

/// Decodes a [`Problem`] into a [`ClusterError`].
///
/// The single client-side entry point. `ctx` supplies which lease-keyed operation
/// asked, so a bare canonical `NotFound` maps to the variant that operation's
/// caller can act on; pass [`LeaseContext::None`] for everything else.
///
/// Three properties this function guarantees, each of which §6.9 requires:
///
/// - a recognised `(error_domain, error_code)` pair reconstructs its exact
///   variant, retryability included;
/// - an **unrecognised** pair — a newer server — becomes
///   `Provider{Other}` carrying the original detail, never a panic and never a
///   silent `Ok`;
/// - a lease-keyed release or resign that matched nothing is not an error at all,
///   reported here as `None`.
#[must_use]
pub fn to_cluster_error(problem: Problem, ctx: LeaseContext<'_>) -> Option<ClusterError> {
    let detail = problem.detail.clone();
    let category_is_not_found = problem.status == ProblemCategory::NotFound.http_status();

    match ClusterWireError::try_from(problem) {
        Ok(wire) => Some(ClusterError::from(wire)),
        // Unrecognised by this build. A bare canonical `NotFound` on a
        // lease-keyed call is the stale-lease class, mapped by which operation
        // asked; anything else is the skew catch-all.
        Err(_) if category_is_not_found => lease_absence(ctx, &detail),
        Err(_) => Some(ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: format!("unrecognised cluster error: {detail}"),
        }),
    }
}

#[allow(
    clippy::match_same_arms,
    reason = "a lock renewal and an election renewal produce the same value for different reasons — one is the guard's terminal loss signal, the other is what the election event pump turns into `Status(Lost)` while keeping the subscription open. §12.2 wanted a distinct sentinel for the second; the frozen error model (I3) has none, so they coincide today. Collapsing the arms would erase the distinction and make a future divergence look like a change of behaviour rather than a restoration of intent"
)]
fn lease_absence(ctx: LeaseContext<'_>, detail: &str) -> Option<ClusterError> {
    match ctx {
        LeaseContext::LockRenew { name } => Some(ClusterError::LockExpired {
            name: name.to_owned(),
        }),
        // Idempotent by absence: the effect the caller wanted has already
        // happened, so there is nothing to report.
        LeaseContext::LeaseRelease => None,
        // The pump turns this into `Status(Lost)` and keeps the subscription
        // open; `LockExpired` naming the election is the loss signal it reads.
        LeaseContext::ElectionRenew { name } => Some(ClusterError::LockExpired {
            name: name.to_owned(),
        }),
        // The subscription is gone because its replica went away (§5.8.1).
        LeaseContext::ElectionSubscription => Some(ClusterError::Shutdown),
        LeaseContext::None => Some(ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: format!("unrecognised cluster error: {detail}"),
        }),
    }
}

/// Synthesises the error a transport failure with **no canonical body** becomes —
/// channel down, pod gone.
///
/// `Provider{ConnectionLost}` and therefore retryable, so an unreachable cluster
/// gear behaves for a consumer exactly like an unreachable Postgres: same
/// recovery path, no new consumer branch (§6.9).
#[must_use]
pub fn transport_failure(message: impl std::fmt::Display) -> ClusterError {
    ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: format!("cluster unreachable: {message}"),
    }
}

impl From<ClusterWireError> for WireError {
    fn from(value: ClusterWireError) -> Self {
        let problem = Problem::from(value);
        Self::from(problem)
    }
}

impl From<Problem> for WireError {
    fn from(value: Problem) -> Self {
        Self {
            error_domain: value.error_domain.unwrap_or_default(),
            error_code: value.error_code.unwrap_or_default(),
            detail: value.detail,
            // Rendered rather than carried structurally: protobuf has no
            // untyped-value field, so `WireError::data` is JSON text.
            data: value
                .context
                .get("data")
                .map_or_else(|| "null".to_owned(), ToString::to_string),
        }
    }
}

impl From<WireError> for Problem {
    /// Rebuilds only what `TryFrom<Problem> for ClusterWireError` reads —
    /// `error_domain`, `error_code` and `context["data"]` — plus the detail. The
    /// envelope's presentational fields (`type`, `title`, `status`) are not
    /// carried on a watch event, so a decoded `Problem` is reconstruction input,
    /// not something to render.
    fn from(value: WireError) -> Self {
        let mut context = serde_json::Map::new();
        // A payload this build cannot parse must not become a decode panic: it
        // degrades to an absent payload, which the derive's field reads already
        // treat as a reconstruction failure and bounce back as the `Problem`.
        let data = serde_json::from_str(&value.data).unwrap_or(serde_json::Value::Null);
        context.insert("data".to_owned(), data);
        Self {
            problem_type: String::new(),
            title: String::new(),
            status: 0,
            detail: value.detail,
            instance: None,
            trace_id: None,
            context: serde_json::Value::Object(context),
            error_code: Some(value.error_code),
            error_domain: Some(value.error_domain),
        }
    }
}

/// Milliseconds, saturating rather than truncating.
///
/// `Duration::as_millis` is `u128`; a lock wait that overflows `u64` milliseconds
/// is 584 million years, so saturation is unreachable in practice and is here so
/// the conversion is total.
fn duration_to_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

/// The gRPC code a wire error travels under (§6.9).
///
/// Server-side counterpart of [`to_cluster_error`], and it lives here rather than
/// in the serving gear for the same reason the rest of the codec does: the match
/// is **exhaustive**, and `#[non_exhaustive]` does not apply within the defining
/// crate. Adding a [`ClusterWireError`] variant therefore fails *this* build. In
/// the gear it could only be written with a catch-all arm, and a catch-all is
/// exactly the silent `Internal` collapse the module docs rule out.
///
/// The mapping is not injective and is not meant to be: `Shutdown` and
/// `ProviderConnectionLost` share `Unavailable`, `LockTimeout` and
/// `ProviderTimeout` share `DeadlineExceeded`. The discriminant travels in the
/// problem trailer's `error_code`, which is what
/// [`to_cluster_error`] reconstructs from — the code is a hint for middleware,
/// never the reconstruction key.
#[cfg(feature = "grpc-client")]
fn grpc_code(wire: &ClusterWireError) -> tonic::Code {
    use tonic::Code;

    match *wire {
        ClusterWireError::CapabilityNotMet { .. } | ClusterWireError::LockExpired { .. } => {
            Code::FailedPrecondition
        }
        ClusterWireError::ProfileNotBound { .. } => Code::NotFound,
        ClusterWireError::ProfileNotSpecified
        | ClusterWireError::InvalidName { .. }
        | ClusterWireError::InvalidConfig { .. } => Code::InvalidArgument,
        ClusterWireError::LockContended { .. } | ClusterWireError::CasConflict { .. } => {
            Code::Aborted
        }
        ClusterWireError::LockTimeout { .. } | ClusterWireError::ProviderTimeout { .. } => {
            Code::DeadlineExceeded
        }
        ClusterWireError::Unsupported { .. } => Code::Unimplemented,
        ClusterWireError::Shutdown | ClusterWireError::ProviderConnectionLost { .. } => {
            Code::Unavailable
        }
        ClusterWireError::ProviderResourceExhausted { .. } => Code::ResourceExhausted,
        // Not `Unauthenticated`: the failure is the cluster gear's credentials
        // against Postgres or Redis, not the caller's against cluster (§6.9).
        ClusterWireError::ProviderAuthFailure { .. } | ClusterWireError::ProviderOther { .. } => {
            Code::Internal
        }
    }
}

/// Projects a [`ClusterError`] onto the [`tonic::Status`] a service impl returns
/// (§6.9) — the encode direction the serving gear needs, symmetric with
/// [`to_cluster_error`].
///
/// The typed payload rides the `x-toolkit-problem-bin` trailer, which is where
/// `toolkit_contract::grpc::map_tonic_status` looks for it, so a client
/// reconstructs the exact variant — `ProviderErrorKind` and retryability included
/// — rather than inferring one from the code. The code and message are what a peer
/// that does not speak the envelope sees, and they are set from the same values.
///
/// A trailer that cannot be attached is never fatal: the [`tonic::Status`] still
/// carries code and message, and the client's decoder already treats a missing
/// envelope as the skew case (§6.11).
#[cfg(feature = "grpc-client")]
#[must_use]
pub fn to_status(error: ClusterError) -> tonic::Status {
    // Taken before the conversion: `Display` on `ClusterError` is the
    // operator-facing text, and `#[derive(ContractError)]` fills `Problem::detail`
    // with the variant's type name instead. A peer that does not speak the
    // envelope sees only this string, and the skew path (§6.11) reads `detail`,
    // so both get the real message rather than `ClusterWireError::LockExpired`.
    let message = error.to_string();
    let wire = ClusterWireError::from(error);
    let code = grpc_code(&wire);
    // Cheap, and only for the one variant whose payload can outgrow the trailer:
    // the key and version, so a conflict that does not fit can be re-rendered
    // without the value rather than cloned wholesale (see `shed_current_value`).
    let conflict = match &wire {
        ClusterWireError::CasConflict {
            key,
            current_version,
            current_value: Some(_),
        } => Some((key.clone(), *current_version)),
        _ => None,
    };
    let mut problem = Problem::from(wire);
    problem.detail.clone_from(&message);

    if let Some((key, current_version)) = conflict
        && !fits_problem_trailer(&problem)
    {
        problem = shed_current_value(key, current_version, &message);
    }

    let mut status = tonic::Status::new(code, message);

    if let Err(trailer_error) =
        toolkit_transport_grpc::attach_problem(status.metadata_mut(), &problem)
    {
        tracing::warn!(
            %trailer_error,
            error_code = problem.error_code.as_deref().unwrap_or_default(),
            "cluster: could not attach the problem trailer; the status carries code and message only"
        );
    }
    status
}

/// Whether `problem` still fits the problem trailer once serialised.
///
/// Measured against `attach_problem`'s own cap and by its own method
/// (`serde_json::to_vec`), because the question being asked is precisely
/// "would `attach_problem` degrade this one?".
#[cfg(feature = "grpc-client")]
fn fits_problem_trailer(problem: &Problem) -> bool {
    serde_json::to_vec(problem)
        .is_ok_and(|bytes| bytes.len() <= toolkit_transport_grpc::MAX_PROBLEM_TRAILER_BYTES)
}

/// Re-renders an oversized `CasConflict` as the **version-only** conflict §6.9
/// decision 17a already sanctions.
///
/// `current` is **SHOULD**, "if cheaply obtainable", and a value that does not
/// fit the trailer is by definition not cheaply obtainable — so the server
/// declines to ship it and the caller re-reads, which §6.9 states is
/// contract-legal. This is a **behavioural** choice made per response, not a wire
/// change: both `current_*` fields are already independently optional, so
/// invariant I12 is untouched and a peer of any vintage decodes the result.
///
/// The alternative is what this exists to prevent: `attach_problem` degrades an
/// oversized envelope to a three-field projection that carries no
/// `error_code`, does not deserialise as a [`Problem`] at all, and therefore
/// reaches the client as an *untyped* status — losing the variant the CAS retry
/// loop branches on.
#[cfg(feature = "grpc-client")]
fn shed_current_value(key: String, current_version: Option<u64>, message: &str) -> Problem {
    tracing::debug!(
        %key,
        "cluster: the conflicting entry does not fit the problem trailer; reporting a \
         version-only conflict so the caller re-reads (section 6.9 decision 17a)"
    );
    let mut problem = Problem::from(ClusterWireError::CasConflict {
        key,
        current_version,
        current_value: None,
    });
    problem.detail.clear();
    problem.detail.push_str(message);
    problem
}

/// Reconstructs a [`CacheWatchEvent`] from the flat wire event (§6.8).
///
/// The union lives in the Rust type on both sides of the wire; the flat
/// discriminated message exists only because protogen cannot express a `oneof`
/// over payload-free variants (see [`CacheWatchEventDto`]). This is the decode
/// half, and the gear's `to_dto` is the encode half — so nothing above the §3.1
/// seam ever sees the flat shape.
///
/// # A malformed frame degrades rather than failing
///
/// The wire type cannot express "the kind decides which fields are present", so a
/// peer *can* send `Changed` with no key. Every such frame becomes
/// [`CacheWatchEvent::Reset`] — "you may have missed something, re-read" — which
/// is the same safe reading that makes `Reset` the enum's `_UNSPECIFIED = 0`
/// default. Dropping the frame silently would be the one unsafe option, and
/// failing the stream would turn one bad frame into a lost subscription.
#[must_use]
pub fn to_cache_watch_event(dto: CacheWatchEventDto) -> CacheWatchEvent {
    match (dto.kind, dto.key, dto.dropped, dto.error) {
        (CacheWatchEventKind::Changed, Some(key), _, _) => {
            CacheWatchEvent::Event(CacheEvent::Changed { key })
        }
        (CacheWatchEventKind::Deleted, Some(key), _, _) => {
            CacheWatchEvent::Event(CacheEvent::Deleted { key })
        }
        (CacheWatchEventKind::Expired, Some(key), _, _) => {
            CacheWatchEvent::Event(CacheEvent::Expired { key })
        }
        (CacheWatchEventKind::Lagged, _, dropped, _) => CacheWatchEvent::Lagged {
            dropped: dropped.unwrap_or_default(),
        },
        (CacheWatchEventKind::Closed, _, _, error) => CacheWatchEvent::Closed(terminal(error)),
        // `Reset`, and every frame whose payload does not match its kind.
        _ => CacheWatchEvent::Reset,
    }
}

/// Reconstructs a [`LeaderWatchEvent`] from the flat wire event, on exactly the
/// same terms as [`to_cache_watch_event`] (§6.6, §6.8).
#[must_use]
pub fn to_leader_watch_event(dto: LeaderWatchEventDto) -> LeaderWatchEvent {
    match (dto.kind, dto.status, dto.dropped, dto.error) {
        (LeaderWatchEventKind::Status, Some(status), _, _) => {
            LeaderWatchEvent::Status(status.into())
        }
        (LeaderWatchEventKind::Lagged, _, dropped, _) => LeaderWatchEvent::Lagged {
            dropped: dropped.unwrap_or_default(),
        },
        (LeaderWatchEventKind::Closed, _, _, error) => LeaderWatchEvent::Closed(terminal(error)),
        _ => LeaderWatchEvent::Reset,
    }
}

/// The error a terminal `Closed` event carries, through the one codec.
///
/// A `Closed` that carries **no** error is a protocol violation rather than a
/// state either side can describe. It becomes a retryable
/// `Provider{ConnectionLost}` so [`RestartingWatch`](crate::restart::RestartingWatch)
/// resubscribes: a subscription lost to a malformed frame is recoverable, and the
/// alternative reading — a terminal, non-retryable close — would strand a
/// consumer permanently on one bad message.
fn terminal(error: Option<WireError>) -> ClusterError {
    error.map_or_else(
        || transport_failure("a terminal watch event carried no error"),
        |wire| {
            to_cluster_error(Problem::from(wire), LeaseContext::None).unwrap_or_else(|| {
                // Unreachable: `LeaseContext::None` never yields release-by-absence.
                transport_failure("a terminal watch event carried an undecodable error")
            })
        },
    )
}

/// Decodes a [`tonic::Status`] into the [`ClusterError`] a lease-keyed call
/// reports — the client-side counterpart of [`to_status`], and the one place a
/// remote backend turns a failed RPC into the frozen error model (§6.9).
///
/// `None` is release-by-absence and nothing else: a `release` or `resign` whose
/// token matched no record achieved what its caller wanted, so there is no error
/// (§6.10). Every other input yields `Some`.
///
/// # Two paths in, and the second is why this is not `to_cluster_error`
///
/// - **A cluster status** carries the typed `Problem` on the
///   `x-toolkit-problem-bin` trailer, which is what reconstructs the exact
///   variant — `ProviderErrorKind` and retryability included — rather than
///   inferring one from the gRPC code.
/// - **A status with no envelope** is a transport failure: the channel is down,
///   the pod is gone, or an intermediary answered. It becomes
///   `Provider{ConnectionLost}` and is therefore retryable, so an unreachable
///   cluster gear behaves for a consumer exactly like an unreachable Postgres —
///   same recovery path, no new consumer branch (§6.9).
///
/// # It deliberately does not go through `CanonicalError`
///
/// [`toolkit_contract::grpc::map_tonic_status`] is used for the extraction, and
/// its `TransportError::Problem` is read directly. Converting onward to
/// `CanonicalError` — which is what the *generated* gRPC client does — is lossy
/// in exactly the fields this codec keys on: `TryFrom<Problem> for CanonicalError`
/// keeps neither `error_domain`, nor `error_code`, nor `context["data"]`, so
/// every cluster error would arrive as `Provider{Other}` and `Shutdown` would
/// become retryable. Measured, not inferred - see the item's commit message.
#[cfg(feature = "grpc-client")]
#[must_use]
pub fn from_lease_status(status: &tonic::Status, ctx: LeaseContext<'_>) -> Option<ClusterError> {
    match toolkit_contract::grpc::map_tonic_status(status) {
        TransportError::Problem { problem, .. } => to_cluster_error(*problem, ctx),
        // A bare gRPC `NotFound` **is** the case [`LeaseContext`] exists for: an
        // intermediary, or a peer that did not type its error. Routing it through
        // the one codec rather than a second table is what keeps the two answers
        // from drifting — and without this, a `release` whose token matched
        // nothing would come back as a retryable connection loss and be retried
        // forever against a record that is already gone.
        TransportError::Grpc {
            code: tonic::Code::NotFound,
            ..
        } => to_cluster_error(bare_not_found(status.message()), ctx),
        // Everything else arrives with no envelope this build can read, and the
        // gRPC code is all that is left to reconstruct from. §6.9's
        // transport-failure licence is real but **conditional** — it names
        // "channel down, pod gone" — so it belongs to the codes that mean that,
        // and not to the ones that mean the server answered and said no.
        ref other => Some(untyped_failure(other, status)),
    }
}

/// Whether the peer told us it truncated the problem envelope.
///
/// `attach_problem` sets this header whenever it degrades an oversized envelope
/// to its three-field projection. It is the one signal that distinguishes "the
/// channel is gone" from "the server answered, and its answer did not fit" —
/// the discrimination [`untyped_failure`] would otherwise lack.
#[cfg(feature = "grpc-client")]
fn envelope_was_truncated(status: &tonic::Status) -> bool {
    status
        .metadata()
        .get(toolkit_transport_grpc::PROBLEM_TRUNCATED_HEADER)
        .is_some()
}

/// What an `Unimplemented` names when no envelope survived. Deliberately a
/// constant: interning the peer's text would feed an unbounded string into a
/// table [`intern`](crate::intern) documents as bounded (invariant I15's
/// cardinality rule). The text itself goes to the log.
#[cfg(feature = "grpc-client")]
const UNIMPLEMENTED_FEATURE: &str = "this cluster.v1 rpc";

/// The [`ClusterError`] a status with **no readable envelope** becomes.
///
/// # Why this is a table and not a catch-all
///
/// It used to be `other => transport_failure(other)` — a retryable
/// `Provider{ConnectionLost}` for every code alike. `NotFound` was already
/// carved out of it, and the comment that carved it out states the general
/// hazard exactly: without it, "a `release` whose token matched nothing would
/// come back as a retryable connection loss and be **retried forever** against a
/// record that is already gone". That reasoning is not special to `NotFound`.
/// `RetryPolicy::default()` sets `max_retries: None`, so *every* code that
/// cannot succeed on retry and is nonetheless classified retryable is an
/// infinite loop against an answer that will not change.
///
/// So every [`tonic::Code`] gets an explicit verdict below. Two rules govern
/// them:
///
/// - **Every code that is retryable today stays retryable, with the same variant
///   and the same message.** Flipping a genuinely transient failure to terminal
///   would strand a consumer on a blip — a worse bug than the one being fixed —
///   so the diff moves codes in one direction only.
/// - **A code that means "the server answered and the answer will not change"
///   becomes non-retryable.** That is §6.10's rule for `NonIdempotentWrite`
///   restated: a CAS conflict must never be the signal that licenses a retry.
///
/// The match is exhaustive rather than defaulted, for the same reason the two
/// `From` impls are (see the [module docs](self)): a `tonic` upgrade that adds a
/// code should fail this build rather than silently inherit a verdict.
#[cfg(feature = "grpc-client")]
fn untyped_failure(failure: &TransportError, status: &tonic::Status) -> ClusterError {
    use tonic::Code;

    let TransportError::Grpc { code, .. } = *failure else {
        // Not a gRPC status at all — `Network`, and the rest of the taxonomy the
        // generated client raises before a status exists. The channel really is
        // the problem, which is §6.9's case unmodified.
        return transport_failure(failure);
    };

    let truncated = envelope_was_truncated(status);
    if truncated {
        tracing::warn!(
            ?code,
            message = status.message(),
            "cluster: the peer's problem envelope exceeded the trailer budget and was truncated \
             in transit; reconstructing from the gRPC code alone"
        );
    }

    match code {
        // --- Retryable. Unchanged from the pre-fix behaviour, deliberately.
        //
        // §6.9's "transport failure with no canonical body (channel down, pod
        // gone)" is exactly these. `Unknown` is here because it is genuinely
        // unclassifiable and the conservative reading of an unclassifiable
        // failure is the recoverable one. `Ok` cannot arrive (`map_tonic_status`
        // turns it into `Network` before this point) and is written out rather
        // than defaulted.
        //
        // Truncation does not move any of them: cluster encodes `Unavailable`
        // for `Shutdown` and `ProviderConnectionLost`, and only the second
        // carries a `message` large enough to be truncated — so a truncated
        // `Unavailable` is the retryable one of the pair, not the terminal one.
        Code::Unavailable
        | Code::DeadlineExceeded
        | Code::Cancelled
        | Code::ResourceExhausted
        | Code::Unknown
        | Code::Ok => transport_failure(failure),

        // --- §6.11's rolling-deployment skew, and the design's own table:
        // `Unimplemented` <-> `Unsupported` <-> not retryable. The method does
        // not exist on the peer, so no number of retries will find it — and
        // `auto_restart` would resubscribe against it forever.
        Code::Unimplemented => {
            tracing::warn!(
                message = status.message(),
                "cluster: the peer does not implement this method; treating it as a version skew \
                 (section 6.11) rather than a transient failure"
            );
            ClusterError::Unsupported {
                feature: UNIMPLEMENTED_FEATURE,
            }
        }

        // --- Not retryable. The server answered; the answer will not change on
        // a second identical request.
        //
        // `Aborted` is `ERR-1`'s landing site: it is what cluster encodes a
        // `CasConflict` and a `LockContended` as, and §6.10 rules
        // `compare_and_swap` a `NonIdempotentWrite` that "must not be
        // auto-retried". `PermissionDenied`/`Unauthenticated` are
        // `Provider{Other}` and not `Provider{AuthFailure}` on purpose: §6.9
        // reserves `AuthFailure` for the *gear's* credentials against its own
        // backend, and mislabelling a caller-side rejection would point the
        // operator at the wrong credential and invite a token-refresh retry that
        // cannot help. `NotFound` is handled by [`LeaseContext`] before this
        // function is reached and is written out for exhaustiveness.
        Code::Aborted
        | Code::FailedPrecondition
        | Code::InvalidArgument
        | Code::OutOfRange
        | Code::AlreadyExists
        | Code::PermissionDenied
        | Code::Unauthenticated
        | Code::Internal
        | Code::DataLoss
        | Code::NotFound => ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: untyped_message(failure, truncated),
        },
    }
}

/// The operator-facing text for a non-retryable untyped status.
///
/// "cluster unreachable" — [`transport_failure`]'s text, which the retryable arm
/// keeps verbatim — is a claim about the *channel*, and it is false here: these
/// codes mean the peer answered. A truncated envelope says so outright, and
/// naming that is what turns "why did my CAS come back as `Provider{Other}`?"
/// into a one-line answer.
#[cfg(feature = "grpc-client")]
fn untyped_message(failure: &TransportError, truncated: bool) -> String {
    if truncated {
        format!("cluster answered with an oversized problem envelope: {failure}")
    } else {
        format!("cluster returned an untyped status: {failure}")
    }
}

/// The `Problem` a bare gRPC `NotFound` stands in for: the right category, and
/// deliberately no `(error_domain, error_code)` pair, so
/// [`to_cluster_error`] takes its unrecognised-code branch and applies the
/// [`LeaseContext`].
#[cfg(feature = "grpc-client")]
fn bare_not_found(detail: &str) -> Problem {
    Problem {
        problem_type: String::new(),
        title: String::new(),
        status: ProblemCategory::NotFound.http_status(),
        detail: detail.to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::Value::Null,
        error_code: None,
        error_domain: None,
    }
}

/// [`from_lease_status`] for a call that is not lease-keyed — every cache
/// operation, and the acquisitions that mint a lease rather than presenting one.
///
/// Total, because release-by-absence is the only source of `None` and
/// [`LeaseContext::None`] cannot produce it. The unreachable arm is still written
/// out rather than unwrapped: it costs one line and turns a future change in
/// [`to_cluster_error`] into a wrong message instead of a panic on a consumer's
/// request path.
#[cfg(feature = "grpc-client")]
#[must_use]
pub fn from_status(status: &tonic::Status) -> ClusterError {
    from_lease_status(status, LeaseContext::None).unwrap_or_else(|| ClusterError::Provider {
        kind: ProviderErrorKind::Other,
        message: format!(
            "cluster returned an undecodable status: {}",
            status.message()
        ),
    })
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod convert_tests;
