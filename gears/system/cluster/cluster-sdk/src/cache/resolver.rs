// Created: 2026-06-03 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-cache-primitive-resolver:p1
//! The fluent cache resolver and its startup capability-validation helper.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use crate::cache::backend::ClusterCacheBackend;
use crate::cache::facade::ClusterCacheV1;
use crate::cache::types::{CacheCapability, CacheConsistency};
use crate::error::ClusterError;
use crate::profile::{ClusterProfile, profile_scope};

/// A fluent builder that resolves a [`ClusterCacheV1`] for a profile and
/// validates declared capabilities at startup.
#[must_use = "a resolver builder resolves nothing until `.resolve()` is called"]
pub struct CacheResolverBuilder<'a> {
    hub: &'a ClientHub,
    profile_name: Option<&'static str>,
    requirements: Vec<CacheCapability>,
}

impl<'a> CacheResolverBuilder<'a> {
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
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-profile
        // @cpt-begin:cpt-cf-clst-flow-sdk-foundation-declare-profile:p1:inst-pass-marker
        self.profile_name = Some(P::NAME);
        // @cpt-end:cpt-cf-clst-flow-sdk-foundation-declare-profile:p1:inst-pass-marker
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-profile
        self
    }

    /// Declares a capability the bound backend must satisfy.
    pub fn require(mut self, capability: CacheCapability) -> Self {
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-require
        self.requirements.push(capability);
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-require
        self
    }

    /// Resolves the cache facade for the bound profile.
    ///
    /// # Errors
    /// - [`ClusterError::ProfileNotSpecified`] if no profile was set.
    /// - [`ClusterError::InvalidName`] if the bound profile's
    ///   [`NAME`](ClusterProfile::NAME) violates [`CLUSTER_NAME_RULE`](crate::CLUSTER_NAME_RULE).
    /// - [`ClusterError::ProfileNotBound`] if no cache backend is registered for
    ///   the profile scope.
    /// - [`ClusterError::CapabilityNotMet`] if a declared capability is
    ///   unsupported by the bound backend.
    pub fn resolve(self) -> Result<ClusterCacheV1, ClusterError> {
        let profile = self.profile_name.ok_or(ClusterError::ProfileNotSpecified)?;
        // @cpt-begin:cpt-cf-clst-flow-sdk-foundation-declare-profile:p1:inst-map-scope
        let scope = profile_scope(profile)?;
        // @cpt-end:cpt-cf-clst-flow-sdk-foundation-declare-profile:p1:inst-map-scope
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-lookup
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unbound
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unbound-return
        // @cpt-begin:cpt-cf-clst-flow-registration-observability-register:p1:inst-rb-resolve
        // @cpt-begin:cpt-cf-clst-flow-registration-observability-register:p1:inst-rb-after
        // @cpt-begin:cpt-cf-clst-flow-registration-observability-register:p1:inst-rb-unbound
        let inner: Arc<dyn ClusterCacheBackend> = self
            .hub
            .get_scoped(&scope)
            .map_err(|_| ClusterError::ProfileNotBound { profile })?;
        // @cpt-end:cpt-cf-clst-flow-registration-observability-register:p1:inst-rb-unbound
        // @cpt-end:cpt-cf-clst-flow-registration-observability-register:p1:inst-rb-after
        // @cpt-end:cpt-cf-clst-flow-registration-observability-register:p1:inst-rb-resolve
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unbound-return
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unbound
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-lookup
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-validate
        validate_cache_capabilities(inner.as_ref(), &self.requirements)?;
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-validate
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-return
        Ok(ClusterCacheV1::from_backend(inner))
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-return
    }
}

/// Validates declared cache capabilities against a backend's actual
/// characteristics (DESIGN §3.10).
///
/// # Errors
/// Returns [`ClusterError::CapabilityNotMet`] — naming the primitive, the
/// unmet capability, and the bound provider — for the first unsatisfied
/// requirement.
pub fn validate_cache_capabilities(
    backend: &dyn ClusterCacheBackend,
    reqs: &[CacheCapability],
) -> Result<(), ClusterError> {
    // Matched exhaustively (no catch-all): although `CacheCapability` is
    // `#[non_exhaustive]`, within this crate every variant must be handled, so
    // adding a future capability fails to compile here rather than being
    // silently treated as satisfied.
    // @cpt-begin:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-foreach
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unmet
    for cap in reqs {
        match cap {
            CacheCapability::Linearizable => {
                // @cpt-begin:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-lin
                if backend.consistency() != CacheConsistency::Linearizable {
                    // @cpt-begin:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-lin-return
                    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unmet-return
                    return Err(ClusterError::CapabilityNotMet {
                        primitive: "ClusterCacheV1",
                        capability: "Linearizable",
                        // Resolve through the trait object so the error names
                        // the concrete backend, not the `dyn` trait type.
                        provider: backend.provider_name(),
                    });
                    // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unmet-return
                    // @cpt-end:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-lin-return
                }
                // @cpt-end:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-lin
            }
            CacheCapability::PrefixWatch => {
                // @cpt-begin:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-pw
                if !backend.features().prefix_watch {
                    // @cpt-begin:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-pw-return
                    return Err(ClusterError::CapabilityNotMet {
                        primitive: "ClusterCacheV1",
                        capability: "PrefixWatch",
                        provider: backend.provider_name(),
                    });
                    // @cpt-end:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-pw-return
                }
                // @cpt-end:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-pw
            }
        }
    }
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-resolve:p1:inst-res-unmet
    // @cpt-end:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-foreach
    // @cpt-begin:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-ok
    Ok(())
    // @cpt-end:cpt-cf-clst-algo-cache-primitive-validate-capabilities:p1:inst-vc-ok
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod resolver_tests;
