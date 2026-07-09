// Created: 2026-06-03 by Constructor Tech
//! The fluent leader-election resolver and its startup capability-validation
//! helper.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use crate::error::ClusterError;
use crate::leader::backend::LeaderElectionBackend;
use crate::leader::facade::LeaderElectionV1;
use crate::leader::types::LeaderElectionCapability;
use crate::profile::{ClusterProfile, profile_scope};

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
    /// # Errors
    /// - [`ClusterError::ProfileNotSpecified`] if no profile was set.
    /// - [`ClusterError::InvalidName`] if the bound profile's
    ///   [`NAME`](ClusterProfile::NAME) violates [`CLUSTER_NAME_RULE`](crate::CLUSTER_NAME_RULE).
    /// - [`ClusterError::ProfileNotBound`] if no leader-election backend is
    ///   registered for the profile scope.
    /// - [`ClusterError::CapabilityNotMet`] if a declared capability is
    ///   unsupported by the bound backend.
    pub fn resolve(self) -> Result<LeaderElectionV1, ClusterError> {
        let profile = self.profile_name.ok_or(ClusterError::ProfileNotSpecified)?;
        let scope = profile_scope(profile)?;
        let inner: Arc<dyn LeaderElectionBackend> = self.hub.get_scoped(&scope).map_err(|err| {
            tracing::debug!(profile, error = %err, "cluster backend lookup failed for profile");
            ClusterError::ProfileNotBound { profile }
        })?;
        validate_leader_election_capabilities(inner.as_ref(), &self.requirements)?;
        Ok(LeaderElectionV1::from_backend(inner))
    }
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
