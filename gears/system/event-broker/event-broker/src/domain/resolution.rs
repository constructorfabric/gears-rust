//! One topic's effective settings, and where each of them came from.
//!
//! A topic is described in two places: the specification `types-registry`
//! holds, which says what the stream is, and `event-broker` configuration,
//! which says how this deployment runs it. Neither is the whole answer, so
//! everything that needs a partition count, a retention bound or a backend
//! reads the record assembled here instead of either source directly.
//!
//! Resolution is per field and by authorship, and the order never varies:
//! what an operator wrote for this topic, then what they wrote for its type,
//! then what the topic itself declares, then the built-in tier. A built-in
//! value never displaces a statement, which is the property that makes a
//! declared retention survive a deployment that mentions none.

use std::time::Duration;
use toolkit::domain_model;

use toolkit_gts::GtsInstanceId;

use crate::config::{
    BUILT_IN_PARTITIONS, DEFAULT_RETENTION_DURATION, EventBrokerConfig, RetentionSettings,
    TopicSettingsEntry, TopicSettingsError,
};
use crate::domain::backend::{self, Routing};

/// Which statement produced an effective value.
///
/// Carried alongside every resolved field so an operator can tell whether the
/// number bounding their data came from their own configuration, from the
/// topic's author, or from a default nobody chose - without reproducing the
/// resolution by hand.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The configuration entry keyed by this topic's own identifier.
    TopicEntry,
    /// The configuration entry keyed by the instance-less key for its type.
    TypeEntry,
    /// The topic's own registered specification.
    Specification,
    /// Nothing stated it, so the broker's built-in value stands.
    BuiltIn,
    /// What the topic's storage backend holds: once a topic is provisioned,
    /// its partition count is the backend's, whatever configuration says
    /// later (feature 0007 §2.6).
    Backend,
}

/// One effective value and the statement behind it.
///
/// Constructed only through the named constructors below: a positional pair
/// would let a caller attach the wrong source to a value silently, and the
/// source is the whole reason this type exists.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sourced<T> {
    value: T,
    source: Source,
}

impl<T> Sourced<T> {
    /// From the entry keyed by the topic's own identifier.
    pub const fn configured_for_topic(value: T) -> Self {
        Self {
            value,
            source: Source::TopicEntry,
        }
    }

    /// From the entry keyed by the instance-less key for the topic's type.
    pub const fn configured_for_type(value: T) -> Self {
        Self {
            value,
            source: Source::TypeEntry,
        }
    }

    /// From what the topic's own specification declares.
    pub const fn declared(value: T) -> Self {
        Self {
            value,
            source: Source::Specification,
        }
    }

    /// Nothing stated it.
    pub const fn built_in(value: T) -> Self {
        Self {
            value,
            source: Source::BuiltIn,
        }
    }

    /// What the topic's storage backend holds.
    pub const fn held_by_backend(value: T) -> Self {
        Self {
            value,
            source: Source::Backend,
        }
    }

    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub const fn source(&self) -> Source {
        self.source
    }

    /// Unwraps to the value alone, for a caller that only has to act on it.
    #[must_use]
    pub fn into_value(self) -> T {
        self.value
    }
}

/// What a topic's own specification contributes to resolution.
///
/// A struct rather than the bare value it currently carries: the specification
/// tier sits in the middle of the ladder, and a second declared field would
/// otherwise change every signature that passes this one through.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declaration {
    /// The retention the topic declares, if it declares one. Advisory: an
    /// operator's configuration overrides it, and where configuration is silent
    /// this is what the broker enforces.
    pub retention: Option<Duration>,
}

/// One topic's settings, resolved, with the provenance of each.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectiveSettings {
    partitions: Sourced<i32>,
    retention: Sourced<RetentionSettings>,
    /// The registry entry the topic routes to, never the entry's type or
    /// settings: this record is persisted, and nothing the broker persists
    /// carries an address or a setting (feature 0005 §2.1).
    backend: Sourced<backend::Name>,
}

impl EffectiveSettings {
    /// Built through [`EffectiveSettingsBuilder`]: three resolved fields, each
    /// carrying its own source, are not something to assemble positionally.
    #[must_use]
    pub fn builder(
        partitions: Sourced<i32>,
        backend: Sourced<backend::Name>,
    ) -> EffectiveSettingsBuilder {
        EffectiveSettingsBuilder {
            partitions,
            retention: None,
            backend,
        }
    }

    #[must_use]
    pub const fn partitions(&self) -> &Sourced<i32> {
        &self.partitions
    }

    #[must_use]
    pub const fn retention(&self) -> &Sourced<RetentionSettings> {
        &self.retention
    }

    #[must_use]
    pub const fn backend(&self) -> &Sourced<backend::Name> {
        &self.backend
    }

    /// The same settings with the partition count the topic's backend holds,
    /// which is in force once the topic is provisioned.
    #[must_use]
    pub fn with_partitions_held_by_backend(mut self, partitions: i32) -> Self {
        self.partitions = Sourced::held_by_backend(partitions);
        self
    }
}

#[domain_model]
pub struct EffectiveSettingsBuilder {
    partitions: Sourced<i32>,
    retention: Option<Sourced<RetentionSettings>>,
    backend: Sourced<backend::Name>,
}

impl EffectiveSettingsBuilder {
    #[must_use]
    pub fn retention(mut self, retention: Sourced<RetentionSettings>) -> Self {
        self.retention = Some(retention);
        self
    }

    /// Builds the record, filling anything unset with the built-in tier.
    ///
    /// Infallible rather than asserted: the one path that reaches this sets
    /// every field, and a default that says "nothing stated this" is a truer
    /// answer for a caller that did not than a panic would be.
    #[must_use]
    pub fn build(self) -> EffectiveSettings {
        EffectiveSettings {
            partitions: self.partitions,
            retention: self.retention.unwrap_or_else(|| {
                Sourced::built_in(RetentionSettings {
                    duration: DEFAULT_RETENTION_DURATION,
                    size_bytes: None,
                })
            }),
            backend: self.backend,
        }
    }
}

/// Resolves one topic's settings from configuration and its own declaration.
///
/// The backend comes from `routing` alone: a topic's specification says
/// nothing about where its log lives, and the route is the operator's
/// statement of it.
///
/// # Errors
/// [`TopicSettingsError::PartitionsOutOfRange`] when a configured count cannot
/// describe a partitioned topic, and [`TopicSettingsError::MalformedTopicId`]
/// when the identifier does not decompose into a type and an instance part.
/// Both are operator mistakes, and neither can be reached by anything a
/// producer or a consumer sends.
pub fn resolve(
    config: &EventBrokerConfig,
    routing: &Routing,
    topic: &GtsInstanceId,
    declared: &Declaration,
) -> Result<EffectiveSettings, TopicSettingsError> {
    let (own, type_default) = config.entries_for(topic)?;

    let partitions = pick(
        own.and_then(|entry| entry.partitions),
        type_default.and_then(|entry| entry.partitions),
    )
    .unwrap_or_else(|| Sourced::built_in(BUILT_IN_PARTITIONS));
    if *partitions.value() < 1 {
        return Err(TopicSettingsError::PartitionsOutOfRange {
            topic: topic.as_ref().to_owned(),
            partitions: *partitions.value(),
        });
    }

    Ok(
        EffectiveSettings::builder(partitions, routing.resolve(topic))
            .retention(retention(own, type_default, declared))
            .build(),
    )
}

/// The first tier that stated a value, with the source that goes with it.
fn pick<T>(from_topic: Option<T>, from_type: Option<T>) -> Option<Sourced<T>> {
    from_topic
        .map(Sourced::configured_for_topic)
        .or_else(|| from_type.map(Sourced::configured_for_type))
}

/// Both bounds resolve independently, so an entry may raise a duration without
/// restating a byte bound. The duration has a third tier the byte bound does
/// not: a topic declares how long its events matter, while how much disk a
/// deployment spends on them is not something the specification can express.
///
/// The record carries one source for the pair, taken from the duration, because
/// that is the field the specification can reach and therefore the only one
/// whose provenance an operator can be surprised by.
fn retention(
    own: Option<&TopicSettingsEntry>,
    type_default: Option<&TopicSettingsEntry>,
    declared: &Declaration,
) -> Sourced<RetentionSettings> {
    let own_block = own.and_then(|entry| entry.retention.as_ref());
    let type_block = type_default.and_then(|entry| entry.retention.as_ref());

    let duration = pick(
        own_block.and_then(|block| block.duration),
        type_block.and_then(|block| block.duration),
    )
    .or_else(|| declared.retention.map(Sourced::declared))
    .unwrap_or_else(|| Sourced::built_in(DEFAULT_RETENTION_DURATION));

    let size_bytes = pick(
        own_block
            .map(|block| block.size_bytes)
            .filter(|size| size.is_stated()),
        type_block
            .map(|block| block.size_bytes)
            .filter(|size| size.is_stated()),
    )
    .and_then(|sourced| sourced.into_value().bytes());

    let source = duration.source();
    let settings = RetentionSettings {
        duration: duration.into_value(),
        size_bytes,
    };
    match source {
        Source::TopicEntry => Sourced::configured_for_topic(settings),
        Source::TypeEntry => Sourced::configured_for_type(settings),
        Source::Specification => Sourced::declared(settings),
        Source::BuiltIn => Sourced::built_in(settings),
        Source::Backend => Sourced::held_by_backend(settings),
    }
}

#[cfg(test)]
#[path = "resolution_tests.rs"]
mod resolution_tests;
