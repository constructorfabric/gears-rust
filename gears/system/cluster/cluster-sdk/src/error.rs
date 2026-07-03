// Created: 2026-06-03 by Constructor Tech
//! Unified error model for the cluster SDK contract.
//!
//! [`ClusterError`] is the single error type returned across every cluster
//! primitive facade (see DESIGN §3.1). [`ProviderErrorKind`] classifies
//! backend/provider errors into programmatic retryability categories so
//! consumers branch on the kind without parsing error strings. There is
//! deliberately no `NotStarted` variant: pre-resolution access surfaces as
//! [`ClusterError::ProfileNotBound`] because the resolver enforces backend
//! presence at consumer construction time.
//!
//! `ClusterError` is `Clone` so it can ride the watch-union `Closed(_)` signal
//! to multiple watchers. The structured provider `source` chain from DESIGN
//! §3.1 is intentionally omitted for now to preserve `Clone`; the `message`
//! field carries the human-readable description, and an `Arc`-wrapped source
//! can be reintroduced later without a `Clone` regression if a chain is needed.

use thiserror::Error;

/// Programmatic classification of a backend/provider error.
///
/// Consumers — and the watch auto-restart combinator — branch on
/// [`ProviderErrorKind::is_retryable`] instead of inspecting error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderErrorKind {
    /// The connection to the backend was lost or dropped; retryable once the
    /// connection is re-established.
    ConnectionLost,
    /// A backend operation timed out; retryable.
    Timeout,
    /// Authentication against the backend failed; not retryable without
    /// operator intervention.
    AuthFailure,
    /// The backend rejected the operation due to resource exhaustion;
    /// retryable with backoff.
    ResourceExhausted,
    /// Any other backend error; not retryable by default.
    Other,
}

impl ProviderErrorKind {
    /// Returns `true` when an operation failing with this kind may succeed on
    /// retry (`ConnectionLost`, `Timeout`, `ResourceExhausted`) and `false`
    /// for kinds that require intervention (`AuthFailure`, `Other`).
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::ConnectionLost | Self::Timeout | Self::ResourceExhausted
        )
    }
}

/// The unified error type returned by every cluster primitive facade
/// (DESIGN §3.1).
///
/// Primitive-specific variants (`LockContended`/`LockTimeout`/`LockExpired`,
/// `CasConflict { current }`) are added by the lock and cache features, which
/// own the types those variants reference; `#[non_exhaustive]` keeps that
/// additive.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum ClusterError {
    /// The bound backend cannot satisfy a declared capability. Surfaced at
    /// resolution (startup), naming the primitive, capability, and provider.
    #[error(
        "capability not met for `{primitive}`: required `{capability}` is unsupported by provider `{provider}`"
    )]
    CapabilityNotMet {
        /// The primitive being resolved (for example `ClusterCacheV1` or
        /// `LeaderElectionV1`).
        primitive: &'static str,
        /// The declared capability that is unmet.
        capability: &'static str,
        /// The provider/backend that cannot satisfy the capability.
        provider: &'static str,
    },

    /// No backend is bound for the requested profile.
    #[error("no backend bound for profile `{profile}`")]
    ProfileNotBound {
        /// The profile that has no bound backend.
        profile: &'static str,
    },

    /// A profile was required but none was specified by the consumer.
    #[error("no profile specified")]
    ProfileNotSpecified,

    /// A profile or coordination name violated the cluster name rule.
    #[error("invalid cluster name `{name}`: must match `{reason}`")]
    InvalidName {
        /// The offending value.
        name: String,
        /// The rule the value must satisfy.
        reason: &'static str,
    },

    /// A configuration value was invalid — for example an eventually-consistent
    /// cache used for a consistency-sensitive default without explicit opt-in.
    #[error("invalid configuration: {reason}")]
    InvalidConfig {
        /// A human-readable description of the misconfiguration.
        reason: String,
    },

    /// A non-blocking lock acquisition failed because the named lock is already
    /// held by another holder. Not retryable on its own — the consumer chooses
    /// whether to back off or fall through.
    #[error("lock `{name}` is already held")]
    LockContended {
        /// The contended lock name.
        name: String,
    },

    /// A blocking lock acquisition did not succeed within the wait timeout.
    /// `waited` reports how long the consumer blocked before giving up.
    #[error("timed out after {waited:?} acquiring lock `{name}`")]
    LockTimeout {
        /// The lock name that was not acquired in time.
        name: String,
        /// How long the consumer waited before the timeout fired.
        waited: std::time::Duration,
    },

    /// A lock operation (typically an extension) was attempted after the lock's
    /// TTL had already elapsed, so the consumer no longer holds it and must
    /// abort the protected operation.
    #[error("lock `{name}` expired before the operation completed")]
    LockExpired {
        /// The lock name whose TTL elapsed.
        name: String,
    },

    /// The bound backend does not support a required feature.
    #[error("feature `{feature}` is unsupported by the bound provider")]
    Unsupported {
        /// The unsupported feature.
        feature: &'static str,
    },

    /// A version-based compare-and-swap failed because the stored version no
    /// longer matched the expected version. `current` carries the present
    /// entry when the backend can supply it cheaply.
    #[error("compare-and-swap conflict on key `{key}`")]
    CasConflict {
        /// The key whose compare-and-swap failed.
        key: String,
        /// The current entry, when cheaply obtainable.
        current: Option<crate::cache::CacheEntry>,
    },

    /// The cluster subsystem is shutting down; the operation was refused.
    #[error("cluster is shutting down")]
    Shutdown,

    /// A backend/provider error, classified for programmatic retryability.
    #[error("provider error ({kind:?}): {message}")]
    Provider {
        /// The retryability classification of the underlying error.
        kind: ProviderErrorKind,
        /// A human-readable description of the provider failure.
        message: String,
    },
}

impl ClusterError {
    /// Returns `true` only if this is a [`ClusterError::Provider`] error whose
    /// [`ProviderErrorKind`] is retryable. All other variants are never
    /// retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Provider { kind, .. } if kind.is_retryable())
    }
}

#[cfg(test)]
mod tests {
    use super::{ClusterError, ProviderErrorKind};

    #[test]
    fn retryable_kinds_match_classification() {
        assert!(ProviderErrorKind::ConnectionLost.is_retryable());
        assert!(ProviderErrorKind::Timeout.is_retryable());
        assert!(ProviderErrorKind::ResourceExhausted.is_retryable());
        assert!(!ProviderErrorKind::AuthFailure.is_retryable());
        assert!(!ProviderErrorKind::Other.is_retryable());
    }

    #[test]
    fn cluster_error_retryable_only_for_retryable_provider() {
        let timeout = ClusterError::Provider {
            kind: ProviderErrorKind::Timeout,
            message: "deadline exceeded".to_owned(),
        };
        assert!(timeout.is_retryable());

        let auth = ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            message: "bad credentials".to_owned(),
        };
        assert!(!auth.is_retryable());

        assert!(!ClusterError::Shutdown.is_retryable());
        assert!(!ClusterError::ProfileNotSpecified.is_retryable());
    }

    #[test]
    fn display_names_the_offending_capability() {
        let err = ClusterError::CapabilityNotMet {
            primitive: "ClusterCacheV1",
            capability: "Linearizable",
            provider: "memory",
        };
        let rendered = err.to_string();
        assert!(rendered.contains("ClusterCacheV1"));
        assert!(rendered.contains("Linearizable"));
        assert!(rendered.contains("memory"));
    }

    #[test]
    fn cluster_error_is_clone() {
        let err = ClusterError::ProfileNotBound { profile: "orders" };
        let cloned = err.clone();
        assert_eq!(err.to_string(), cloned.to_string());
    }

    #[test]
    fn lock_variants_are_never_retryable() {
        assert!(
            !ClusterError::LockContended {
                name: "rate-limit".to_owned()
            }
            .is_retryable()
        );
        assert!(
            !ClusterError::LockTimeout {
                name: "rate-limit".to_owned(),
                waited: std::time::Duration::from_secs(5),
            }
            .is_retryable()
        );
        assert!(
            !ClusterError::LockExpired {
                name: "rate-limit".to_owned()
            }
            .is_retryable()
        );
    }

    #[test]
    fn lock_variants_name_the_lock_in_their_message() {
        assert!(
            ClusterError::LockContended {
                name: "ledger".to_owned()
            }
            .to_string()
            .contains("ledger")
        );
        let timeout = ClusterError::LockTimeout {
            name: "ledger".to_owned(),
            waited: std::time::Duration::from_millis(250),
        }
        .to_string();
        assert!(timeout.contains("ledger"));
        assert!(timeout.contains("250ms"));
        assert!(
            ClusterError::LockExpired {
                name: "ledger".to_owned()
            }
            .to_string()
            .contains("ledger")
        );
    }
}
