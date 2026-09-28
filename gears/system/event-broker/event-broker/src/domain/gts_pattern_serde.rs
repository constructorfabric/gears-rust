//! Serde bridge for `Vec<GtsIdPattern>`.
//!
//! An interest's event-type selectors are typed [`GtsIdPattern`]s in the domain -
//! a GTS pattern is never a bare `String` here - but `gts::GtsIdPattern` carries no
//! serde of its own, and a subscription (with its interests) is serialized into
//! the cluster cache. This module is the `#[serde(with = ...)]` bridge: patterns
//! serialize as their canonical string form and deserialize back through
//! `GtsIdPattern::try_new`, which validates the wildcard grammar and so rejects a
//! malformed pattern rather than admitting it as an opaque string.

use gts::GtsIdPattern;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serializer};

pub(crate) fn serialize<S>(patterns: &[GtsIdPattern], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(patterns.len()))?;
    for pattern in patterns {
        seq.serialize_element(pattern.pattern())?;
    }
    seq.end()
}

pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<GtsIdPattern>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|raw| GtsIdPattern::try_new(&raw).map_err(serde::de::Error::custom))
        .collect()
}
