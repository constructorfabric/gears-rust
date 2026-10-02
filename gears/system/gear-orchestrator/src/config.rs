//! Operator-facing configuration schema for the gear-orchestrator gear.

use std::collections::HashMap;

/// The gear's config section (`gears.gear-orchestrator.config`).
///
/// `deny_unknown_fields` so a misspelled key (e.g. `trusted_registrar`) fails at
/// startup instead of silently deserializing to an empty set.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestratorConfig {
    /// Peer identities permitted to act on any gear's registration (see
    /// [`crate::server::make_directory_service`]). Empty by default.
    #[serde(default)]
    pub trusted_registrars: Vec<String>,
    /// Platform-controlled Kubernetes namespaces a `ServiceAccount` peer must
    /// live in to register.
    ///
    /// Empty **disables** the namespace check (name-only fallback): a
    /// `ServiceAccount` named `billing` from *any* namespace is then authorized
    /// for gear `billing`. Set this in any cluster where untrusted workloads can
    /// mint tokens, or a tenant SA sharing a gear's name could take over its
    /// registration.
    #[serde(default)]
    pub platform_namespaces: Vec<String>,
    /// SPIFFE trust domains a workload peer must belong to.
    ///
    /// Empty **disables** the trust-domain check (name-only fallback): a workload
    /// named `billing` from *any* trust domain is then authorized for gear
    /// `billing`. Same guidance as `platform_namespaces`.
    #[serde(default)]
    pub trust_domains: Vec<String>,
    /// Additional gRPC-service-name -> owning-gear map, for gears the binary does
    /// **not** compile in (e.g. remote / out-of-process gears). Pins which gear
    /// may advertise a name so ownership is decided here rather than by whichever
    /// gear self-registers first (which would let a gear squat a name it was
    /// never assigned).
    ///
    /// Compiled-in gears do not need entries here: the runtime derives their
    /// ownership from the compiled registry and installs it as authoritative,
    /// overriding any config entry that disagrees (see
    /// `GearManager::merge_authoritative_grpc_service_owners`). Use this only for
    /// names the local binary cannot know about. Names absent everywhere keep
    /// first-registration ownership. Empty by default.
    #[serde(default)]
    pub grpc_service_owners: HashMap<String, String>,
}
