//! Per-gear publication (SPEC §10.1, D16, D18, D21): [`publish`] runs [`reconcile`] in a
//! [`supervised`] background task; this module holds the per-identifier status it reports.
//! T33 wires `publish_gts`.

pub mod publish;
pub mod reconcile;
pub mod supervised;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

/// Plain identifier/document pair from `gts_declarations()`.
pub type GtsDeclaration = (String, serde_json::Value);

/// Publisher `SemVer` precedence: prereleases count; build metadata affects display only.
/// Length is bounded by [`Self::MAX_LEN`]. Use the publishing crate’s `CARGO_PKG_VERSION` (D18).
#[derive(Debug, Clone)]
pub struct PublisherVersion(semver::Version);

impl PublisherVersion {
    /// Maximum version-text bytes, checked before parsing to bound untrusted input.
    pub const MAX_LEN: usize = 128;
}

/// Why a [`PublisherVersion`] was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublisherVersionError {
    /// The input exceeds [`PublisherVersion::MAX_LEN`] bytes.
    #[error("publisher version is {len} bytes long, more than the {max} allowed")]
    TooLong { len: usize, max: usize },
    /// The input is not a valid `SemVer` 2.0.0 version.
    #[error("publisher version is not valid SemVer: {message}")]
    Invalid { message: String },
}

impl FromStr for PublisherVersion {
    type Err = PublisherVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() > Self::MAX_LEN {
            return Err(PublisherVersionError::TooLong {
                len: s.len(),
                max: Self::MAX_LEN,
            });
        }
        semver::Version::parse(s)
            .map(Self)
            .map_err(|e| PublisherVersionError::Invalid {
                message: e.to_string(),
            })
    }
}

impl TryFrom<&str> for PublisherVersion {
    type Error = PublisherVersionError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for PublisherVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for PublisherVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PublisherVersion {}

impl PartialOrd for PublisherVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PublisherVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp_precedence(&other.0)
    }
}

impl Hash for PublisherVersion {
    // Ignore build metadata, matching cmp_precedence; destructuring detects added fields.
    fn hash<H: Hasher>(&self, state: &mut H) {
        let semver::Version {
            major,
            minor,
            patch,
            pre,
            build: _,
        } = &self.0;
        major.hash(state);
        minor.hash(state);
        patch.hash(state);
        pre.hash(state);
    }
}

/// Publishing gear and its own `CARGO_PKG_VERSION` (SPEC D18); shared helpers only forward it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublisherContext {
    pub name: String,
    pub version: PublisherVersion,
}

/// Per-identifier progress; readiness requires every declaration to be satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicationStatus {
    /// State of every declared identifier, keyed by GTS identifier.
    pub entities: BTreeMap<String, PublicationState>,
}

/// State of one declared identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationState {
    /// Not settled yet; the publisher is still working on it.
    Pending(PendingReason),
    /// Registered, or already present with the declared content.
    Admitted,
    /// Refused for good; the publisher no longer retries it.
    Rejected(RejectionReason),
    /// A newer release of the same publisher owns the entity (SPEC D18).
    Superseded {
        stored_version: PublisherVersion,
        offered_version: PublisherVersion,
        entity: SupersededEntity,
    },
}

/// Why an identifier is still pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingReason {
    /// Not submitted yet, or submitted and awaiting a terminal outcome.
    Waiting,
    /// The registry could not be reached.
    RegistryUnreachable { message: String },
    /// Call-level refusal (e.g. auth/permission); the declaration may still be valid.
    RegistryRefused { message: String },
    /// The registry is up, but something the declaration depends on is not registered yet.
    BlockedDependency { message: String },
    /// A concurrent writer changed the entity; the next cycle re-reads it.
    Conflict { message: String },
}

/// Why an identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionReason {
    /// The registry refused the candidate; `reason` is its machine-readable code.
    Registry { reason: String, message: String },
    /// Invalid local/synchronous declaration; reason is the field-violation code.
    InvalidDeclaration { reason: String, message: String },
    /// Publisher exited before settlement: failed, panicked, returned, or cancelled.
    PublisherStopped { message: String },
}

/// What the superseding release did with the entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupersededEntity {
    /// The entity is live: it counts as satisfied for readiness (SPEC D21).
    Live,
    /// The newer release deleted it: it does not count as satisfied.
    Deleted,
}

impl PublicationStatus {
    /// A status with every identifier [`PendingReason::Waiting`].
    pub fn pending<I>(ids: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        Self {
            entities: ids
                .into_iter()
                .map(|id| (id.into(), PublicationState::Pending(PendingReason::Waiting)))
                .collect(),
        }
    }

    /// No identifier is pending: the publisher has nothing left to do.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !self
            .entities
            .values()
            .any(|state| matches!(state, PublicationState::Pending(_)))
    }

    /// Admitted or superseded-live for every identifier (SPEC D21, Required); empty sets satisfy
    /// readiness.
    #[must_use]
    pub fn satisfies_readiness(&self) -> bool {
        self.entities.values().all(|state| {
            matches!(
                state,
                PublicationState::Admitted
                    | PublicationState::Superseded {
                        entity: SupersededEntity::Live,
                        ..
                    }
            )
        })
    }

    /// Pending identifiers become [`RejectionReason::PublisherStopped`]; settled outcomes are kept.
    #[must_use]
    pub fn stopped(mut self, message: &str) -> Self {
        for state in self.entities.values_mut() {
            if matches!(state, PublicationState::Pending(_)) {
                *state = PublicationState::Rejected(RejectionReason::PublisherStopped {
                    message: message.to_owned(),
                });
            }
        }
        self
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod publication_tests;
