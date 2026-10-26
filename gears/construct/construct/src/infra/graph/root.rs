use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The node key of a subject's profile root in a tenant.
#[must_use]
pub fn root_key(tenant_id: Uuid, subject_id: Uuid) -> String {
    format!("construct:{tenant_id}:root:{subject_id}")
}

/// What the root holds besides its subject: the profile version and the
/// removed facts whose soft delete may still be pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub version: u64,
    pub removals: Vec<String>,
}

impl Root {
    /// Read a stored root's payload.
    ///
    /// # Errors
    ///
    /// When the payload is not a root's, which graph storage's validation
    /// rules out; it is a defect, not a state to repair.
    pub fn from_payload(payload: Option<&Value>) -> Result<Self, DomainError> {
        let broken = || DomainError::internal("a profile root has no valid payload");
        let payload = payload.ok_or_else(broken)?;
        let version = payload["version"].as_u64().ok_or_else(broken)?;
        let removals = payload["removals"]
            .as_array()
            .ok_or_else(broken)?
            .iter()
            .map(|key| key.as_str().map(str::to_owned).ok_or_else(broken))
            .collect::<Result<_, _>>()?;
        Ok(Self { version, removals })
    }

    #[must_use]
    pub fn payload(&self, subject_id: Uuid) -> Value {
        json!({
            "subject_id": subject_id.to_string(),
            "version": self.version,
            "removals": self.removals,
        })
    }
}

/// The profile version, opaque to the planner: read it from a stored root,
/// and give the `expected_version` of the root write and the version that
/// write leaves. Graph storage returns no node version on read, so
/// this implementation keeps a counter in the root's payload; one that reads
/// graph storage's version replaces it without changing the writer.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-version:p1
pub trait ProfileVersion: Send + Sync {
    /// # Errors
    ///
    /// When the stored root carries no readable version.
    fn read(&self, root: Option<&Value>) -> Result<u64, DomainError>;

    /// The `expected_version` for the root write of a plan built on
    /// `version`, none for a subject without a root.
    fn expected(&self, version: Option<u64>) -> i64;

    /// The version the root has after that write.
    fn next(&self, version: Option<u64>) -> u64;
}

/// [`ProfileVersion`] as a counter in the root's payload. Graph storage starts
/// a node at version 1 and adds one on every write that changes it. The
/// writer creates the root at 1 and adds one to the counter on every write,
/// so every root write changes the node, and the counter is the stored
/// version.
#[derive(Debug, Default)]
pub struct PayloadCounter;

impl ProfileVersion for PayloadCounter {
    fn read(&self, root: Option<&Value>) -> Result<u64, DomainError> {
        Root::from_payload(root).map(|root| root.version)
    }

    fn expected(&self, version: Option<u64>) -> i64 {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-expected
        // `Some(0)` claims a key no node holds: the root of a new subject is
        // created exactly once, also by two first writes at the same time.
        version.map_or(0, |version| i64::try_from(version).unwrap_or(i64::MAX))
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-expected
    }

    fn next(&self, version: Option<u64>) -> u64 {
        version.map_or(1, |version| version + 1)
    }
}
