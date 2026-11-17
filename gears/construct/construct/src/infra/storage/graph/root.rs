use std::collections::BTreeSet;

use graph_storage_sdk::models::NodeView;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::subject_settings::SubjectKey;

/// The node key of a subject's first profile root in its tenant, that of
/// generation 0. Only tests use it: every read and write looks the
/// generation up.
#[cfg(test)]
#[must_use]
pub fn root_key(subject: &SubjectKey) -> NodeKey {
    generation_root(subject, 0)
}

/// The node key of a subject's profile root in `generation`. Generation 0 has
/// the key every profile had before erasure existed. An erasure soft-deletes
/// the root and moves the generation on, because graph storage never gives a
/// deleted key out again; the profile created after it gets the next key.
///
/// @cpt-dod:cpt-cf-construct-dod-erasure-root-key:p1
#[must_use]
pub fn generation_root(subject: &SubjectKey, generation: u64) -> NodeKey {
    // @cpt-begin:cpt-cf-construct-algo-erasure-root-key:p1:inst-root-key
    let first = format!(
        "construct:{}:root:{}",
        subject.tenant_id, subject.subject_id
    );
    if generation == 0 {
        NodeKey(first)
    } else {
        NodeKey(format!("{first}:{generation}"))
    }
    // @cpt-end:cpt-cf-construct-algo-erasure-root-key:p1:inst-root-key
}

/// The generation of `key` when it is a profile root key of `subject`, as
/// [`generation_root`] builds it; none for any other key.
#[must_use]
pub fn root_generation(subject: &SubjectKey, key: &NodeKey) -> Option<u64> {
    let first = generation_root(subject, 0);
    let rest = key.0.strip_prefix(first.0.as_str())?;
    if rest.is_empty() {
        return Some(0);
    }
    // A key with a generation carries only digits, never 0 or a leading
    // zero, so each generation has one key: `parse` alone takes a `+` too.
    let generation = rest.strip_prefix(':')?;
    if generation.starts_with('0') || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    generation.parse().ok()
}

/// [`ProfileGenerations`] where no subject was ever erased: every profile is
/// in generation 0. For tests of the graph layer alone.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FirstGeneration;

#[cfg(test)]
#[async_trait::async_trait]
impl crate::domain::deletion::ProfileGenerations for FirstGeneration {
    async fn generation(&self, _key: SubjectKey) -> Result<u64, DomainError> {
        Ok(0)
    }
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
    pub removals: BTreeSet<NodeKey>,
}

impl Root {
    /// The subject a stored root names, read through the root's own payload
    /// type.
    ///
    /// # Errors
    ///
    /// When the payload is not a root's, a defect; the message names the
    /// root.
    pub fn subject_of(key: &NodeKey, payload: Option<&Value>) -> Result<Uuid, DomainError> {
        let payload = payload.ok_or_else(|| {
            DomainError::internal(format!("profile root {} has no payload", key.0))
        })?;
        RootPayload::<Vec<String>>::deserialize(payload)
            .map(|parsed| parsed.subject_id)
            .map_err(|err| {
                DomainError::internal(format!(
                    "profile root {} has no valid payload: {err}",
                    key.0
                ))
            })
    }

    /// Read the payload of `subject_id`'s stored root.
    ///
    /// # Errors
    ///
    /// When the payload is not a root's, which graph storage's validation
    /// rules out, or names another subject; either is a defect, not a state
    /// to repair. The message names the root and what is wrong with it.
    pub fn from_payload(
        key: &NodeKey,
        subject_id: Uuid,
        payload: Option<&Value>,
    ) -> Result<Self, DomainError> {
        let payload = payload.ok_or_else(|| {
            DomainError::internal(format!("profile root {} has no payload", key.0))
        })?;
        let parsed = RootPayload::<Vec<String>>::deserialize(payload).map_err(|err| {
            DomainError::internal(format!(
                "profile root {} has no valid payload: {err}",
                key.0
            ))
        })?;
        if parsed.subject_id != subject_id {
            return Err(DomainError::internal(format!(
                "profile root {} names subject {}, not {subject_id}",
                key.0, parsed.subject_id
            )));
        }
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

/// The bits of a profile version below its root's generation: the counter.
const COUNTER_BITS: u32 = 32;
const COUNTER: u64 = (1 << COUNTER_BITS) - 1;

/// [`ProfileVersion`] as a counter in the root's payload. Graph storage starts
/// a node at version 1 and adds one on every write that changes it. The
/// writer creates the root at 1 and adds one to the counter on every write,
/// so every root write changes the node, and the counter is the stored
/// version.
///
/// The version a read gives names the root's generation above the counter,
/// so a root of a later generation never has the version a root of an
/// earlier one had: a plan built before an erasure conflicts with the
/// profile created after it, and is planned again. A root of generation 0
/// has its counter as its version, as before erasure existed.
#[derive(Debug, Default)]
pub struct PayloadCounter;

/// The generation a root key names: the part after the subject, none for
/// generation 0.
fn generation_of(root: &NodeView) -> Result<u64, DomainError> {
    let Some(generation) = root.node_key.split(':').nth(4) else {
        return Ok(0);
    };
    generation
        .parse::<u64>()
        .ok()
        .filter(|generation| *generation <= COUNTER)
        .ok_or_else(|| {
            DomainError::internal(format!(
                "profile root {} names no generation a version can carry",
                root.node_key
            ))
        })
}

impl ProfileVersion for PayloadCounter {
    fn read(&self, root: &NodeView) -> Result<u64, DomainError> {
        let counter = root
            .payload
            .as_ref()
            .and_then(|payload| payload[VERSION].as_u64())
            .filter(|counter| *counter <= COUNTER)
            .ok_or_else(|| {
                DomainError::internal(format!(
                    "profile root {} has no version counter",
                    root.node_key
                ))
            })?;
        Ok((generation_of(root)? << COUNTER_BITS) | counter)
    }

    fn expected(&self, version: Option<u64>) -> Result<i64, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-expected
        // `Some(0)` claims a key no node holds: the root of a new subject is
        // created exactly once, also by two first writes at the same time.
        // Graph storage's node version is the counter alone.
        Ok(version.map_or(0, |version| i64::from(counter_of(version))))
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-expected
    }

    fn next(&self, version: Option<u64>) -> Result<u64, DomainError> {
        version.map_or(Ok(1), |version| {
            if u64::from(counter_of(version)) == COUNTER {
                return Err(DomainError::internal(format!(
                    "profile version {version} cannot advance"
                )));
            }
            Ok(version + 1)
        })
    }

    fn stamp(&self, payload: &mut Map<String, Value>, version: u64) {
        payload.insert(VERSION.to_owned(), Value::from(counter_of(version)));
    }
}

/// The counter of a profile version, below its generation.
fn counter_of(version: u64) -> u32 {
    u32::try_from(version & COUNTER).unwrap_or(u32::MAX)
}
