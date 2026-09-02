//! Registration policy: the deployment allowlist for **new logical entities**
//! (DESIGN §3.2, SPEC §10.3).
//!
//! Two independent parameters per GTS Identifier Region — which vendors may
//! appear in the candidate's last segment, and whether an entity there may be
//! tenant-owned. Both default **closed**. The only implicit allowance is global
//! platform-vendor (`cf`) registration.
//!
//! Policy decides *what* a region admits; authorization decides *who* may write
//! there. It runs before the PDP and before any existence lookup (acceptance step
//! 3, T7), so a grant cannot open a closed region and a refusal cannot probe the
//! namespace.
//!
//! # Why an exact key is compared by string equality and not as a pattern
//!
//! The P0 matrix relies on DESIGN §3.2's separation of the base type from `…~*`:
//! the exact key `gts.cf.core.rg.type.v1~` keeps the base closed while
//! `gts.cf.core.rg.type.v1~*` opens its subtree. That is **not** expressible
//! through `GtsIdPattern` — a bare identifier is an implicit derived-type coverage
//! envelope (GTS spec §3.6), so `…v1~` and `…v1~*` accept the same set, and
//! `matches_pattern` on an exact key would make it govern the whole subtree. So a
//! key without a wildcard is canonicalized through `GtsId::try_new` and compared by
//! equality; only a key carrying `*` becomes a `GtsIdPattern`.
//!
//! # `tenant_ownable` is resolved and not consulted
//!
//! ponytail: ceiling C6 / SPEC §9 fix every P0 row to `ownership_scope = 1`,
//! `owner_tenant_id = NULL`, so the parameter has nothing to decide. It is still
//! parsed, validated and *resolvable* ([`RegistrationPolicy::tenant_ownable`])
//! because a P1-ready deployment carries it and silently dropping it would let an
//! operator believe a parameter is enforced. [`RegistrationPolicy::admits`] refuses
//! any candidate asking for tenant ownership **whatever the entry says** — a
//! fail-closed assertion about an unreachable request shape. The upgrade is one
//! line: consult the resolved value instead of refusing.

use std::collections::BTreeMap;

use gts::{GtsId, GtsIdPattern};
use thiserror::Error;
use toolkit_macros::domain_model;

use crate::domain::enums::OwnershipScope;
use crate::policy_config::PolicyEntry;

/// The vendor whose global registrations are admitted without an entry.
const PLATFORM_VENDOR: &str = "cf";

/// The wildcard vendor token: any vendor.
const ANY_VENDOR: &str = "*";

/// Why a configured policy cannot be compiled.
#[domain_model]
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyConfigError {
    #[error("region '{key}' is not a valid GTS identifier or pattern: {reason}")]
    InvalidRegion { key: String, reason: String },
    #[error("region '{key}' lists an empty vendor token")]
    EmptyVendor { key: String },
}

/// Why a candidate is not admitted.
///
/// Names the parameter and the region, because those are the two things an
/// operator has to edit. A `region` of `None` means no entry provided the
/// parameter at all and the closed default applied — a distinct situation from
/// an entry that provided it and excluded this vendor.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRefusal {
    pub gts_id: String,
    pub parameter: &'static str,
    pub region: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for PolicyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.region {
            Some(region) => write!(
                f,
                "registration policy refuses '{}': {} (parameter '{}', region '{}')",
                self.gts_id, self.detail, self.parameter, region
            ),
            None => write!(
                f,
                "registration policy refuses '{}': {} (parameter '{}', no region provides it)",
                self.gts_id, self.detail, self.parameter
            ),
        }
    }
}

/// How an entry key matches a candidate identifier.
#[derive(Debug)]
enum Matcher {
    /// A key with no wildcard: the canonical identifier, compared by equality.
    /// See the module header for why this is not a pattern.
    Exact(String),
    Pattern(GtsIdPattern),
}

#[derive(Debug)]
struct CompiledEntry {
    /// The key as configured, for the refusal message.
    key: String,
    matcher: Matcher,
    /// Length of the key's literal prefix — everything before the wildcard.
    literal_prefix: usize,
    allowed_vendors: Option<Vec<String>>,
    tenant_ownable: Option<bool>,
}

impl CompiledEntry {
    fn matches(&self, candidate: &GtsId) -> bool {
        match &self.matcher {
            Matcher::Exact(id) => candidate.id() == id,
            Matcher::Pattern(pattern) => candidate.matches_pattern(pattern),
        }
    }

    /// Specificity, most specific last. An exact key beats any pattern
    /// regardless of length; among patterns the longest literal prefix wins.
    /// Trailing-only wildcards make matching regions nested, so ties cannot
    /// arise between two patterns that both match.
    fn specificity(&self) -> (bool, usize) {
        (
            matches!(self.matcher, Matcher::Exact(_)),
            self.literal_prefix,
        )
    }
}

/// A compiled registration policy. Immutable, cheap to consult, and holding no
/// entity state.
#[domain_model]
#[derive(Debug, Default)]
pub struct RegistrationPolicy {
    entries: Vec<CompiledEntry>,
}

impl RegistrationPolicy {
    /// Compile the configured entries, rejecting any key that is not an exact
    /// canonical identifier or a pattern with one trailing wildcard.
    ///
    /// # Errors
    /// [`PolicyConfigError::InvalidRegion`] for a key `gts-rust` rejects, and
    /// [`PolicyConfigError::EmptyVendor`] for a blank vendor token, which would
    /// otherwise sit in the set matching nothing.
    pub fn compile(configured: &BTreeMap<String, PolicyEntry>) -> Result<Self, PolicyConfigError> {
        let mut entries = Vec::with_capacity(configured.len());
        for (key, entry) in configured {
            let literal_prefix = key.find(ANY_VENDOR).unwrap_or(key.len());
            let matcher = if key.contains(ANY_VENDOR) {
                // `try_new` is what enforces "one trailing wildcard on a token
                // boundary"; reproducing that rule here would be a second
                // grammar to keep in step (`constraint-gts-implementation`).
                Matcher::Pattern(GtsIdPattern::try_new(key).map_err(|e| {
                    PolicyConfigError::InvalidRegion {
                        key: key.clone(),
                        reason: e.to_string(),
                    }
                })?)
            } else {
                Matcher::Exact(
                    GtsId::try_new(key)
                        .map_err(|e| PolicyConfigError::InvalidRegion {
                            key: key.clone(),
                            reason: e.to_string(),
                        })?
                        .id()
                        .to_owned(),
                )
            };
            if let Some(vendors) = &entry.allowed_vendors
                && vendors.iter().any(|v| v.trim().is_empty())
            {
                return Err(PolicyConfigError::EmptyVendor { key: key.clone() });
            }
            entries.push(CompiledEntry {
                key: key.clone(),
                matcher,
                literal_prefix,
                allowed_vendors: entry.allowed_vendors.as_ref().map(|vendors| {
                    vendors
                        .iter()
                        .map(|vendor| vendor.trim().to_owned())
                        .collect()
                }),
                tenant_ownable: entry.tenant_ownable,
            });
        }
        Ok(Self { entries })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The most specific matching entry that **names** the parameter selected by
    /// `pick`. Entries that omit it are skipped, so a less-specific entry may
    /// still provide it.
    fn resolve<'a, T>(
        &'a self,
        candidate: &GtsId,
        pick: impl Fn(&'a CompiledEntry) -> Option<T>,
    ) -> Option<(&'a CompiledEntry, T)> {
        self.entries
            .iter()
            .filter(|e| e.matches(candidate))
            .filter_map(|e| pick(e).map(|v| (e, v)))
            .max_by_key(|(e, _)| e.specificity())
    }

    /// The resolved vendor set for this candidate, with the region that supplied
    /// it. `None` means no entry provided the parameter — the closed default.
    ///
    /// The selected set **replaces** a less-specific one rather than extending
    /// it, which is what makes a narrow entry able to restrict a broad one.
    #[must_use]
    pub fn allowed_vendors(&self, candidate: &GtsId) -> Option<(&str, &[String])> {
        self.resolve(candidate, |e| e.allowed_vendors.as_deref())
            .map(|(e, vendors)| (e.key.as_str(), vendors))
    }

    /// The resolved `tenant_ownable` for this candidate. Inert in P0 — see the
    /// module header — and `None` means the closed default, which is `false`.
    #[must_use]
    pub fn tenant_ownable(&self, candidate: &GtsId) -> Option<(&str, bool)> {
        self.resolve(candidate, |e| e.tenant_ownable)
            .map(|(e, ownable)| (e.key.as_str(), ownable))
    }

    /// Whether the policy admits a **declared creation** of this candidate.
    ///
    /// Only creation: a revision or a deletion bypasses this gate entirely, so
    /// closing a region cannot freeze the entities already in it. That decision
    /// is the caller's (acceptance step 3, T7) — this method answers the
    /// question, it does not decide when to ask it.
    ///
    /// # Errors
    /// [`PolicyRefusal`] naming the parameter and the region.
    pub fn admits(
        &self,
        candidate: &GtsId,
        ownership_scope: OwnershipScope,
    ) -> Result<(), PolicyRefusal> {
        // The last *named* segment, not simply the last one. A UUID tail is not a
        // named segment and its `vendor()` is `""`, so reading the last segment
        // blindly would refuse an explicit-UUID-tail candidate for "vendor ''" —
        // a message about the wrong thing, one step before the identifier-profile
        // gate refuses it for the reason that actually applies (§8.1 step 4).
        let vendor = candidate
            .segments()
            .iter()
            .rev()
            .find(|segment| segment.uuid_tail().is_none())
            .map_or("", gts::GtsIdSegment::vendor);

        // The implicit allowance is *global platform-vendor registration*, so it
        // covers the vendor parameter only — a `cf` candidate asking for tenant
        // ownership still falls to the check below.
        if vendor != PLATFORM_VENDOR {
            match self.allowed_vendors(candidate) {
                Some((region, vendors)) => {
                    let admitted = vendors
                        .iter()
                        .any(|v| v == ANY_VENDOR || v.as_str() == vendor);
                    if !admitted {
                        return Err(PolicyRefusal {
                            gts_id: candidate.id().to_owned(),
                            parameter: "allowed_vendors",
                            region: Some(region.to_owned()),
                            detail: format!("vendor '{vendor}' is not admitted here"),
                        });
                    }
                }
                None => {
                    return Err(PolicyRefusal {
                        gts_id: candidate.id().to_owned(),
                        parameter: "allowed_vendors",
                        region: None,
                        detail: format!(
                            "vendor '{vendor}' is not admitted; registration policy is closed by \
                             default and only global '{PLATFORM_VENDOR}' is implicit"
                        ),
                    });
                }
            }
        }

        if ownership_scope == OwnershipScope::Tenant {
            // ponytail: see the module header. P0 has no tenant-owned rows at
            // all, so this refuses rather than consulting `tenant_ownable`.
            return Err(PolicyRefusal {
                gts_id: candidate.id().to_owned(),
                parameter: "tenant_ownable",
                region: self.tenant_ownable(candidate).map(|(r, _)| r.to_owned()),
                detail: "P0 admits global entities only; tenant ownership is not available yet"
                    .to_owned(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod policy_tests;
