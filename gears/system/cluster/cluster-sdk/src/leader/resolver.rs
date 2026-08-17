// Created: 2026-06-03 by Constructor Tech
//! The fluent leader-election resolver and its startup capability-validation
//! helper.

use toolkit::client_hub::ClientHub;

use crate::binding;
use crate::dto::LeaderElectionDescriptor;
use crate::error::ClusterError;
use crate::intern::intern;
use crate::leader::backend::LeaderElectionBackend;
use crate::leader::facade::LeaderElectionV1;
use crate::leader::types::LeaderElectionCapability;
use crate::profile::{ClusterProfile, validate_cluster_name};

/// A fluent builder that resolves a [`LeaderElectionV1`] for a profile and
/// validates declared capabilities at startup.
#[must_use = "a resolver builder resolves nothing until `.resolve()` is called"]
pub struct LeaderElectionResolverBuilder<'a> {
    hub: &'a ClientHub,
    profile_name: Option<&'static str>,
    requirements: Vec<LeaderElectionCapability>,
}

impl<'a> LeaderElectionResolverBuilder<'a> {
    pub(crate) fn new(hub: &'a ClientHub) -> Self {
        Self {
            hub,
            profile_name: None,
            requirements: Vec::new(),
        }
    }

    /// Binds the resolution to a typed profile. The marker is passed by type;
    /// only its [`ClusterProfile::NAME`] is read.
    pub fn profile<P: ClusterProfile>(mut self, _marker: P) -> Self {
        self.profile_name = Some(P::NAME);
        self
    }

    /// Declares a capability the bound backend must satisfy.
    pub fn require(mut self, capability: LeaderElectionCapability) -> Self {
        self.requirements.push(capability);
        self
    }

    /// Resolves the leader-election facade for the bound profile.
    ///
    /// # Why this is `async`
    ///
    /// A remote binding cannot validate a declared capability without a
    /// [`ProfileDescriptor`](crate::ProfileDescriptor), and fetching one is I/O
    /// (DESIGN-DEPLOYABLE-GEAR §4.7.1, obstacle A). **This is the only SDK
    /// signature the deployable model changes** (invariant I2) — the facades, the
    /// typed-profile resolver, `scoped()`, the watch-event unions and
    /// `auto_restart` all keep their shapes.
    ///
    /// It costs a consumer nothing beyond an `.await`: facades are resolved in a
    /// gear's `start`, never its `init`, and both are already `async fn`.
    ///
    /// In Profile 1 the await is a formality — the local client's descriptor is
    /// intrinsic, so its future is ready on the first poll and the bounded timeout
    /// can never fire.
    ///
    /// # What it resolves through
    ///
    /// The process's single `dyn ClusterClient` (invariant I4), never a
    /// per-profile hub registration — see the `binding` module for the four steps
    /// and for what an empty hub yields (§4.9.1, §4.9.3).
    ///
    /// # Errors
    /// - [`ClusterError::ProfileNotSpecified`] if no profile was set.
    /// - [`ClusterError::InvalidName`] if the bound profile's
    ///   [`NAME`](ClusterProfile::NAME) violates [`CLUSTER_NAME_RULE`](crate::CLUSTER_NAME_RULE).
    /// - [`ClusterError::ProfileNotBound`] if a cluster client is registered but
    ///   binds no leader-election backend for the profile.
    /// - [`ClusterError::CapabilityNotMet`] if a declared capability is unsupported
    ///   by the bound backend and the descriptor was obtainable in time; otherwise
    ///   validation defers to readiness and this returns `Ok` (§4.7.1).
    pub async fn resolve(self) -> Result<LeaderElectionV1, ClusterError> {
        let profile = self.profile_name.ok_or(ClusterError::ProfileNotSpecified)?;
        validate_cluster_name(profile)?;
        let requirements = self.requirements;
        let backend = binding::bind(
            self.hub,
            profile,
            "leader_election",
            |client| client.leader_election_backend(profile),
            || binding::unbound_leader_election(profile),
            move |descriptor| {
                validate_leader_election_capabilities_from(
                    &descriptor.leader_election,
                    &requirements,
                )
            },
        )
        .await?;
        Ok(LeaderElectionV1::from_backend(backend))
    }
}

/// Validates declared leader-election capabilities against the profile's
/// **descriptor** (DESIGN-DEPLOYABLE-GEAR §5.5) — the form the resolve path uses
/// in both deployment profiles. See
/// `validate_cache_capabilities_from`.
///
/// # Errors
/// Returns [`ClusterError::CapabilityNotMet`] — naming the primitive, the unmet
/// capability, and the operator-facing provider name — for the first unsatisfied
/// requirement.
pub fn validate_leader_election_capabilities_from(
    descriptor: &LeaderElectionDescriptor,
    reqs: &[LeaderElectionCapability],
) -> Result<(), ClusterError> {
    // Matched exhaustively (no catch-all), for the reason
    // `validate_leader_election_capabilities` gives.
    for cap in reqs {
        match cap {
            LeaderElectionCapability::Linearizable => {
                if !descriptor.features.linearizable {
                    return Err(ClusterError::CapabilityNotMet {
                        primitive: "LeaderElectionV1",
                        capability: "Linearizable",
                        // Interned rather than the error widened (invariant I3).
                        provider: intern(&descriptor.provider),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Validates declared leader-election capabilities against a backend's actual
/// characteristics (DESIGN §3.10).
///
/// # Errors
/// Returns [`ClusterError::CapabilityNotMet`] — naming the primitive, the
/// unmet capability, and the bound provider — for the first unsatisfied
/// requirement.
pub fn validate_leader_election_capabilities(
    backend: &dyn LeaderElectionBackend,
    reqs: &[LeaderElectionCapability],
) -> Result<(), ClusterError> {
    // Matched exhaustively (no catch-all): although `LeaderElectionCapability`
    // is `#[non_exhaustive]`, within this crate every variant must be handled,
    // so adding a future capability fails to compile here rather than being
    // silently treated as satisfied.
    for cap in reqs {
        match cap {
            LeaderElectionCapability::Linearizable => {
                if !backend.features().linearizable {
                    return Err(ClusterError::CapabilityNotMet {
                        primitive: "LeaderElectionV1",
                        capability: "Linearizable",
                        // Resolve through the trait object so the error names
                        // the concrete backend, not the `dyn` trait type.
                        provider: backend.provider_name(),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod resolver_tests;
