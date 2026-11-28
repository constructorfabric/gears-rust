//! The registry evidence a judged record keeps (P-D-268): for each resolved schema and instance,
//! its UUID, the registry's resource version and the digest of its content, taken once at intake
//! and never read again from the registry.
use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// One resolved registry entity, as intake saw it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub gts_id: String,
    pub uuid: Uuid,
    pub resource_version: u64,
    /// `content_digest` of the entity's registry content, 64 lowercase hex digits.
    pub digest: String,
}

/// Every resolved registry entity of a record, by GTS id (P-D-268). Stored as a JSON object.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvidenceSet(pub BTreeMap<String, Evidence>);

impl EvidenceSet {
    /// Keeps `evidence` under its id.
    pub fn insert(&mut self, evidence: Evidence) {
        self.0.insert(evidence.gts_id.clone(), evidence);
    }

    /// Every entity of `self` and of `other`; an id in both keeps `other`'s.
    pub fn extend(&mut self, other: &Self) {
        for evidence in other.0.values() {
            self.insert(evidence.clone());
        }
    }

    /// `{gts_id: {uuid, resource_version, digest}}`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(id, e)| {
                    (
                        id.clone(),
                        json!({
                            "uuid": e.uuid.to_string(),
                            "resource_version": e.resource_version,
                            "digest": e.digest,
                        }),
                    )
                })
                .collect::<Map<String, Value>>(),
        )
    }

    /// Reads [`Self::to_json`]'s shape back, strictly: exactly the three members, a UUID in any
    /// form `Uuid::parse_str` accepts, a whole resource version and a 64-digit lowercase hex
    /// digest.
    ///
    /// # Errors
    /// A message naming the member that does not decode.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let entries = value
            .as_object()
            .ok_or_else(|| "evidence is not a JSON object".to_owned())?;
        let mut set = Self::default();
        for (id, entry) in entries {
            let member = |name: &str| {
                entry
                    .get(name)
                    .ok_or_else(|| format!("evidence {id} has no {name}"))
            };
            let fields = entry
                .as_object()
                .ok_or_else(|| format!("evidence {id} is not a JSON object"))?;
            if fields.len() != 3 {
                return Err(format!("evidence {id} has members other than its three"));
            }
            let uuid = member("uuid")?
                .as_str()
                .and_then(|text| Uuid::parse_str(text).ok())
                .ok_or_else(|| format!("evidence {id} has no UUID"))?;
            let resource_version = member("resource_version")?
                .as_u64()
                .ok_or_else(|| format!("evidence {id} has no resource version"))?;
            let digest = member("digest")?
                .as_str()
                .filter(|text| is_digest(text))
                .ok_or_else(|| format!("evidence {id} has no digest"))?
                .to_owned();
            set.insert(Evidence {
                gts_id: id.clone(),
                uuid,
                resource_version,
                digest,
            });
        }
        Ok(set)
    }
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
