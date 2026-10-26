use graph_storage_sdk::models::NodeView;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::subject_settings::SubjectKey;

/// The node key of a subject's profile root in its tenant.
#[must_use]
pub fn root_key(subject: &SubjectKey) -> NodeKey {
    NodeKey(format!(
        "construct:{}:root:{}",
        subject.tenant_id, subject.subject_id
    ))
}

/// The root's payload besides the version, which [`ProfileVersion`] owns.
/// It borrows the removals when written and owns them when read.
#[derive(Debug, Serialize, Deserialize)]
struct RootPayload<R> {
    subject_id: Uuid,
    removals: R,
}

/// What the root holds besides its subject and its version: the keys of the
/// removed facts whose soft delete may still be pending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Root {
    pub removals: Vec<NodeKey>,
}

impl Root {
    /// Read a stored root's payload.
    ///
    /// # Errors
    ///
    /// When the payload is not a root's, which graph storage's validation
    /// rules out; it is a defect, not a state to repair. The message names
    /// the root and what is wrong with it.
    pub fn from_payload(key: &NodeKey, payload: Option<&Value>) -> Result<Self, DomainError> {
        let payload = payload.ok_or_else(|| {
            DomainError::internal(format!("profile root {} has no payload", key.0))
        })?;
        let parsed = RootPayload::<Vec<String>>::deserialize(payload).map_err(|err| {
            DomainError::internal(format!(
                "profile root {} has no valid payload: {err}",
                key.0
            ))
        })?;
        Ok(Self {
            removals: parsed.removals.into_iter().map(NodeKey).collect(),
        })
    }

    /// The payload of this root for `subject_id`, stamped with `version`.
    ///
    /// # Errors
    ///
    /// When the payload cannot be built, a defect.
    pub fn payload(
        &self,
        subject_id: Uuid,
        versions: &dyn ProfileVersion,
        version: u64,
    ) -> Result<Value, DomainError> {
        let payload = RootPayload {
            subject_id,
            removals: self
                .removals
                .iter()
                .map(|key| key.0.as_str())
                .collect::<Vec<_>>(),
        };
        let Ok(Value::Object(mut fields)) = serde_json::to_value(payload) else {
            return Err(DomainError::internal(
                "a profile root payload is not an object",
            ));
        };
        versions.stamp(&mut fields, version);
        Ok(Value::Object(fields))
    }
}

/// The profile version, opaque to the planner: it reads the version of a
/// stored root, gives the root write's expected version and the next version,
/// and stamps the version on the root it writes. Graph storage returns no node
/// version on read, so this implementation keeps a counter in the root's
/// payload; one that reads graph storage's own version replaces it without
/// changing the writer or the read, and with a new root type that drops the
/// counter.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-version:p1
pub trait ProfileVersion: Send + Sync {
    /// # Errors
    ///
    /// When the stored root carries no readable version.
    fn read(&self, root: &NodeView) -> Result<u64, DomainError>;

    /// The `expected_version` for the root write of a plan built on
    /// `version`, none for a subject without a root.
    ///
    /// # Errors
    ///
    /// When the version has no `expected_version`, a defect of the stored root.
    fn expected(&self, version: Option<u64>) -> Result<i64, DomainError>;

    /// The version the root has after that write.
    ///
    /// # Errors
    ///
    /// When the version cannot advance, a defect of the stored root.
    fn next(&self, version: Option<u64>) -> Result<u64, DomainError>;

    /// Record `version` on the root payload being written.
    fn stamp(&self, payload: &mut Map<String, Value>, version: u64);
}

/// The member of the root payload that holds the counter.
const VERSION: &str = "version";

/// [`ProfileVersion`] as a counter in the root's payload. Graph storage starts
/// a node at version 1 and adds one on every write that changes it. The
/// writer creates the root at 1 and adds one to the counter on every write,
/// so every root write changes the node, and the counter is the stored
/// version.
#[derive(Debug, Default)]
pub struct PayloadCounter;

impl ProfileVersion for PayloadCounter {
    fn read(&self, root: &NodeView) -> Result<u64, DomainError> {
        root.payload
            .as_ref()
            .and_then(|payload| payload[VERSION].as_u64())
            .ok_or_else(|| {
                DomainError::internal(format!(
                    "profile root {} has no version counter",
                    root.node_key
                ))
            })
    }

    fn expected(&self, version: Option<u64>) -> Result<i64, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-expected
        // `Some(0)` claims a key no node holds: the root of a new subject is
        // created exactly once, also by two first writes at the same time.
        version.map_or(Ok(0), |version| {
            i64::try_from(version).map_err(|_| {
                DomainError::internal(format!(
                    "profile version {version} is beyond graph storage's node versions"
                ))
            })
        })
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-expected
    }

    fn next(&self, version: Option<u64>) -> Result<u64, DomainError> {
        version.map_or(Ok(1), |version| {
            version.checked_add(1).ok_or_else(|| {
                DomainError::internal(format!("profile version {version} cannot advance"))
            })
        })
    }

    fn stamp(&self, payload: &mut Map<String, Value>, version: u64) {
        payload.insert(VERSION.to_owned(), Value::from(version));
    }
}
