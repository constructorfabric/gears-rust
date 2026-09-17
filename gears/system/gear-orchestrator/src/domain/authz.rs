//! Registration authorization — pure policy for the `DirectoryService`.
//!
//! Identity in, allow/deny out: no transport (`tonic`) types. The gRPC adapter
//! ([`crate::server`]) reads the authenticated peer off the request and calls
//! [`registration_authorized`] rather than owning the decision itself.

use std::collections::HashSet;

use toolkit_security::PlatformIdentity;

/// Registration-authorization policy for the `DirectoryService`.
///
/// All three sets are empty by default. `trusted_registrars` lists peers
/// allowed to act on *any* gear; `platform_namespaces` / `trust_domains` are the
/// Kubernetes namespaces / SPIFFE trust domains a per-gear identity must belong
/// to (empty disables the respective qualifier check — see
/// [`registration_authorized`]).
#[derive(Debug, Default, Clone)]
pub struct RegistrationPolicy {
    /// Peer names permitted to act on any gear's registration.
    pub trusted_registrars: HashSet<String>,
    /// Platform-controlled Kubernetes namespaces a `ServiceAccount` identity
    /// must live in to register.
    pub platform_namespaces: HashSet<String>,
    /// SPIFFE trust domains a workload identity must belong to.
    pub trust_domains: HashSet<String>,
}

/// Why [`registration_authorized`] denied a peer.
///
/// Surfaced in the denial log so an operator can tell a name mismatch from a
/// namespace / trust-domain that isn't allowlisted — the latter is silent by
/// default (an empty allowlist skips the check) and the likeliest cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// The name is neither the gear's own name nor a `trusted_registrars` entry.
    NameMismatch,
    /// A `ServiceAccount`'s namespace is not in `platform_namespaces`.
    NamespaceNotAllowed,
    /// A SPIFFE workload's trust domain is not in `trust_domains`.
    TrustDomainNotAllowed,
    /// An unrecognised / future identity variant (fails closed).
    UnknownIdentity,
}

impl DenyReason {
    /// A stable `snake_case` label for structured logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NameMismatch => "name_mismatch",
            Self::NamespaceNotAllowed => "namespace_not_allowlisted",
            Self::TrustDomainNotAllowed => "trust_domain_not_allowlisted",
            Self::UnknownIdentity => "unknown_identity",
        }
    }
}

/// Whether `identity` may register/deregister/heartbeat `gear_name`, or the
/// [`DenyReason`] if not.
///
/// A peer may act on the gear whose name matches its own, or on any gear whose
/// name is in `trusted_registrars` (`name_ok`). A per-gear identity
/// ([`PlatformIdentity::KubernetesServiceAccount`], [`PlatformIdentity::Spiffe`])
/// must *also* pass a qualifier check — its K8s namespace / SPIFFE trust domain
/// must be in `platform_namespaces` / `trust_domains` — because the name alone is
/// not unique (a `billing` `ServiceAccount` in *any* namespace could otherwise
/// claim gear `billing`). An empty allowlist disables that check (name-only
/// fallback). The qualifier is checked first, so a foreign namespace / trust
/// domain is reported as such even when the name would also mismatch.
///
/// [`PlatformIdentity::Shared`] is routed through `name_ok` as well: a shared
/// secret resolves every caller to one label, so authorizing it unconditionally
/// would make the policy inert. [`PlatformIdentity::Unknown`] and future variants
/// fail closed.
///
/// # Errors
/// Returns the [`DenyReason`] describing which check failed when `identity` is
/// not authorized for `gear_name`.
pub fn registration_authorized(
    identity: &PlatformIdentity,
    gear_name: &str,
    policy: &RegistrationPolicy,
) -> Result<(), DenyReason> {
    // A qualifier (K8s namespace / SPIFFE trust domain) is accepted when its
    // allowlist is unset (fallback) or explicitly lists it.
    let qualifier_ok = |allowlist: &HashSet<String>, value: &str| {
        allowlist.is_empty() || allowlist.contains(value)
    };
    // The name component may act on its own gear, or on any gear if a trusted
    // registrar.
    let name_ok = |name: &str| name == gear_name || policy.trusted_registrars.contains(name);

    match identity {
        PlatformIdentity::Shared { name } => {
            name_ok(name).then_some(()).ok_or(DenyReason::NameMismatch)
        }
        PlatformIdentity::KubernetesServiceAccount {
            namespace,
            service_account,
            pod: _,
        } => {
            if !qualifier_ok(&policy.platform_namespaces, namespace) {
                return Err(DenyReason::NamespaceNotAllowed);
            }
            name_ok(service_account)
                .then_some(())
                .ok_or(DenyReason::NameMismatch)
        }
        PlatformIdentity::Spiffe {
            trust_domain,
            name,
            version: _,
        } => {
            if !qualifier_ok(&policy.trust_domains, trust_domain) {
                return Err(DenyReason::TrustDomainNotAllowed);
            }
            name_ok(name).then_some(()).ok_or(DenyReason::NameMismatch)
        }
        // `Unknown` and any future non_exhaustive variant fail closed.
        _ => Err(DenyReason::UnknownIdentity),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// A [`RegistrationPolicy`] from `&str` slices.
    fn policy(trusted: &[&str], namespaces: &[&str], domains: &[&str]) -> RegistrationPolicy {
        let set = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        RegistrationPolicy {
            trusted_registrars: set(trusted),
            platform_namespaces: set(namespaces),
            trust_domains: set(domains),
        }
    }

    /// A per-gear (`ServiceAccount`) platform identity named `name`.
    fn sa_identity(name: &str) -> PlatformIdentity {
        PlatformIdentity::KubernetesServiceAccount {
            namespace: "toolkit".to_owned(),
            service_account: name.to_owned(),
            pod: None,
        }
    }

    fn spiffe_identity(trust_domain: &str, name: &str) -> PlatformIdentity {
        PlatformIdentity::Spiffe {
            trust_domain: trust_domain.to_owned(),
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
        }
    }

    /// The predicate over every platform-plane identity variant, in isolation,
    /// with no namespace / trust-domain allowlist (the name-only fallback).
    #[test]
    fn registration_authorized_covers_every_provider() {
        let open = policy(&[], &[], &[]);
        let trusted = policy(&["flight-control"], &[], &[]);

        // Per-gear identity: only its own gear, unless a trusted registrar.
        assert!(registration_authorized(&sa_identity("billing"), "billing", &open).is_ok());
        assert_eq!(
            registration_authorized(&sa_identity("billing"), "catalog", &open),
            Err(DenyReason::NameMismatch)
        );
        assert!(
            registration_authorized(&sa_identity("flight-control"), "billing", &trusted).is_ok()
        );

        // SPIFFE workload name is the gear name -> strict with zero config.
        let spiffe = spiffe_identity("example.org", "billing");
        assert!(registration_authorized(&spiffe, "billing", &open).is_ok());
        assert_eq!(
            registration_authorized(&spiffe, "catalog", &open),
            Err(DenyReason::NameMismatch)
        );

        // Shared secret: its label is routed through `name_ok` like any other
        // identity — it may act on the gear whose name equals its label, but not
        // on an arbitrary gear unless the label is a trusted registrar.
        let shared = PlatformIdentity::Shared {
            name: "toolkit-internal".to_owned(),
        };
        assert!(registration_authorized(&shared, "toolkit-internal", &open).is_ok());
        assert_eq!(
            registration_authorized(&shared, "anything", &open),
            Err(DenyReason::NameMismatch)
        );
        assert!(
            registration_authorized(
                &shared,
                "anything",
                &policy(&["toolkit-internal"], &[], &[])
            )
            .is_ok()
        );

        // Unknown / future variant fails closed.
        assert_eq!(
            registration_authorized(&PlatformIdentity::Unknown, "billing", &open),
            Err(DenyReason::UnknownIdentity)
        );
    }

    /// With an allowlist configured, the identity's qualifier (K8s namespace /
    /// SPIFFE trust domain) must match — a same-named `ServiceAccount` in a
    /// foreign namespace (or workload in a foreign trust domain) is rejected
    /// even though the name matches the gear.
    #[test]
    fn registration_authorized_enforces_qualifier_when_configured() {
        let ns = policy(&[], &["platform"], &[]);
        let td = policy(&[], &[], &["platform.example"]);

        let sa = |namespace: &str, gear: &str| PlatformIdentity::KubernetesServiceAccount {
            namespace: namespace.to_owned(),
            service_account: gear.to_owned(),
            pod: None,
        };

        // Right name in the platform namespace -> allowed; foreign namespace ->
        // rejected (the vuln this closes), and reported as the qualifier miss.
        assert!(registration_authorized(&sa("platform", "billing"), "billing", &ns).is_ok());
        assert_eq!(
            registration_authorized(&sa("tenant-x", "billing"), "billing", &ns),
            Err(DenyReason::NamespaceNotAllowed)
        );

        // SPIFFE: same rule on the trust domain.
        assert!(
            registration_authorized(
                &spiffe_identity("platform.example", "billing"),
                "billing",
                &td
            )
            .is_ok()
        );
        assert_eq!(
            registration_authorized(&spiffe_identity("evil.example", "billing"), "billing", &td),
            Err(DenyReason::TrustDomainNotAllowed)
        );
    }

    /// A trusted registrar is still bound by the qualifier allowlist: the rule
    /// is `qualifier_ok && name_ok`, so "trusted" grants *cross-gear* authority,
    /// not a bypass of the namespace / trust-domain check. Guards against a
    /// regression to "trusted means skip the qualifier", which would let an SA
    /// named `flight-control` in *any* namespace own every gear.
    #[test]
    fn trusted_registrar_is_still_bound_by_the_qualifier_allowlist() {
        let sa = |namespace: &str, gear: &str| PlatformIdentity::KubernetesServiceAccount {
            namespace: namespace.to_owned(),
            service_account: gear.to_owned(),
            pod: None,
        };

        // Both sets populated: `flight-control` may act on any gear, but only
        // from the `platform` namespace / `platform.example` trust domain.
        let pol = policy(&["flight-control"], &["platform"], &["platform.example"]);

        // Trusted registrar in an allowed qualifier, acting cross-gear -> allowed.
        assert!(
            registration_authorized(&sa("platform", "flight-control"), "billing", &pol).is_ok()
        );
        assert!(
            registration_authorized(
                &spiffe_identity("platform.example", "flight-control"),
                "billing",
                &pol
            )
            .is_ok()
        );

        // Same trusted registrar from a foreign qualifier -> denied on the
        // qualifier. Trusted does not skip the namespace / trust-domain check.
        assert_eq!(
            registration_authorized(&sa("tenant-x", "flight-control"), "billing", &pol),
            Err(DenyReason::NamespaceNotAllowed)
        );
        assert_eq!(
            registration_authorized(
                &spiffe_identity("evil.example", "flight-control"),
                "billing",
                &pol
            ),
            Err(DenyReason::TrustDomainNotAllowed)
        );
    }
}
