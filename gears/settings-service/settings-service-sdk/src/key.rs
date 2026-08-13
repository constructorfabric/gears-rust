// Created: 2026-08-11 by Constructor Tech
// @cpt-algo:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1
//! The setting key value object.
//!
//! A setting key is a GTS instance identifier made of exactly two segments:
//! `<value-type>~<setting-instance-id>`. The first segment is a curated value
//! type terminated by `~`; the second is the setting's own instance id and
//! carries no trailing `~`.
//!
//! ```text
//! gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1
//!  seg1 vendor=cf   package=settings ns=types   type=bool_flag
//!                        seg2 vendor=acme package=settings ns=network type=enable_proxy
//! ```
//!
//! Grammar validation is delegated to `gts-id`, the platform's single source of
//! truth for GTS identifiers. This module adds only the rules `gts-id` cannot
//! know about: that a setting key is exactly a type followed by an instance,
//! and where the category and leaf name sit within the instance segment.
//!
//! Catalog membership of the value type — that it comes from
//! `gts.cf.settings.types.*~` — is deliberately not checked here. Resolving it
//! requires the types registry, so it belongs to declaration creation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Terminator that marks the end of a GTS **type** segment.
pub const TYPE_TERMINATOR: char = '~';

/// Number of segments a setting key must have: a value type and an instance.
const SETTING_KEY_SEGMENTS: usize = 2;

/// Why a candidate setting key was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingKeyError {
    /// The identifier is not a value type followed by an instance id.
    #[error(
        "a setting key must be a value type followed by an instance id ({SETTING_KEY_SEGMENTS} segments), got {count}"
    )]
    SegmentCount {
        /// How many GTS segments the identifier actually had.
        count: usize,
    },

    /// The first segment is not a GTS type.
    #[error("the value-type half must be a GTS type, so it must end with `{TYPE_TERMINATOR}`")]
    ValueTypeNotAType,

    /// The second segment ends with `~`, making it a type rather than an instance.
    #[error(
        "the instance half must not end with `{TYPE_TERMINATOR}`; a trailing terminator marks a GTS type"
    )]
    TrailingSeparator,

    /// The identifier as a whole is not a valid GTS id.
    #[error("invalid GTS identifier: {cause}")]
    InvalidId {
        /// What the GTS validator objected to.
        cause: String,
    },

    /// One segment of the identifier is invalid.
    #[error("segment #{num} `{segment}` is invalid: {cause}")]
    InvalidSegment {
        /// 1-based segment number.
        num: usize,
        /// Byte offset of the segment within the full identifier.
        offset: usize,
        /// The offending segment, reported so the caller can point at it.
        segment: String,
        /// What the GTS validator objected to.
        cause: String,
    },

    /// The instance half is an anonymous UUID rather than an authored name.
    ///
    /// GTS allows a trailing UUID tail for machine-generated instances. A
    /// setting is not one: its category and leaf name are what an administrator
    /// browses and a module author writes down, and a UUID supplies neither.
    #[error("a setting key names its instance; an anonymous UUID instance has no category or leaf")]
    AnonymousInstance,

    /// The candidate carries leading or trailing whitespace.
    ///
    /// Refused rather than trimmed: this type stores the key verbatim so a
    /// stored key and a supplied key compare byte-identically, and quietly
    /// accepting a padded form would make two spellings of one key.
    #[error("a setting key carries no leading or trailing whitespace")]
    SurroundingWhitespace,
}

/// `None` for the empty string, which is how `gts-id` reports a token that the
/// segment shape does not carry.
fn non_empty(token: &str) -> Option<String> {
    (!token.is_empty()).then(|| token.to_owned())
}

impl From<gts_id::GtsIdError> for SettingKeyError {
    fn from(err: gts_id::GtsIdError) -> Self {
        // Flattened rather than wrapped: `GtsIdError` is neither `Clone` nor
        // `PartialEq`, and keeping a third-party error out of this SDK's public
        // surface means a `gts-id` version bump cannot break our consumers.
        // `GtsIdError` is one struct with an optional segment locator, not two
        // variants: its presence is what distinguishes a segment-level failure
        // from an identifier-level one.
        match err.segment {
            Some(segment) => Self::InvalidSegment {
                num: segment.num,
                offset: segment.offset,
                segment: segment.segment,
                cause: err.cause,
            },
            None => Self::InvalidId { cause: err.cause },
        }
    }
}

/// A parsed setting key.
///
/// Holds the key verbatim: parsing never trims, lowercases, or otherwise
/// normalizes, so a stored key and a supplied key compare byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SettingKey {
    raw: String,
    /// Byte index of the `~` that terminates the value-type segment.
    separator: usize,
    /// Namespace token of the instance segment: the owning category's slug.
    category: String,
    /// Type token of the instance segment: the setting's own leaf name.
    leaf: String,
}

impl SettingKey {
    /// Parse a candidate setting key.
    ///
    /// # Errors
    ///
    /// Returns [`SettingKeyError`] when the candidate is not a valid GTS
    /// identifier, or is valid but is not a value type followed by an instance.
    pub fn parse(raw: &str) -> Result<Self, SettingKeyError> {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-1
        // Wildcards are a pattern-matching feature; a concrete setting key never has one.
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-2
        // `GtsId::try_new` trims before parsing. This type stores the candidate
        // verbatim, so accepting surrounding whitespace would both break the
        // byte-identical round-trip and shift every byte offset below by the
        // length of the leading run. Refuse it instead of silently normalizing.
        if raw != raw.trim() {
            return Err(SettingKeyError::SurroundingWhitespace);
        }
        let segments = gts_id::GtsId::try_new(raw)?.into_segments();
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-2
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-1

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-3
        let [value_type, instance] = segments.as_slice() else {
            return Err(SettingKeyError::SegmentCount {
                count: segments.len(),
            });
        };
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-3

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-4
        if !value_type.is_type() {
            return Err(SettingKeyError::ValueTypeNotAType);
        }
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-4

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-5
        if instance.is_type() {
            return Err(SettingKeyError::TrailingSeparator);
        }
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-5

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-6
        // The instance segment's namespace token is the owning category and its
        // type token is the leaf name, for admin and module authors alike.
        //
        // An anonymous-instance UUID tail carries neither: `namespace()` and
        // `type_name()` answer `""` for it, so accepting one would produce a key
        // with an empty category and leaf that still compared equal to itself.
        // A setting is named by its author, never generated.
        let (Some(category), Some(leaf)) = (
            non_empty(instance.namespace()),
            non_empty(instance.type_name()),
        ) else {
            return Err(SettingKeyError::AnonymousInstance);
        };
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-6

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-7
        // The value-type segment ends with its terminator, and the checks above
        // established there are exactly two segments of which the first is the
        // type — so the first terminator in the candidate is the split point.
        //
        // Located in the candidate rather than derived from segment lengths:
        // `gts-id` reports segment #1 without the `gts.` prefix, and any future
        // change to how a segment renders itself would silently shift an
        // arithmetic offset while this stays correct.
        let separator = raw
            .find(TYPE_TERMINATOR)
            .ok_or(SettingKeyError::ValueTypeNotAType)?;
        Ok(Self {
            raw: raw.to_owned(),
            separator,
            category,
            leaf,
        })
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-7
    }

    /// Compose an admin-authored key.
    ///
    /// Builds the instance id as `<vendor>.settings.<category>.<name>.v1` and
    /// joins it to `value_type`, which must already end with `~`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingKeyError`] when `value_type` is not a GTS type, or when
    /// the composed key is not a valid setting key.
    pub fn compose(
        value_type: &str,
        vendor: &str,
        category: &str,
        name: &str,
    ) -> Result<Self, SettingKeyError> {
        // Checked before splicing so the error names the caller's own input
        // rather than a position inside the joined string.
        if !value_type.ends_with(TYPE_TERMINATOR) {
            return Err(SettingKeyError::ValueTypeNotAType);
        }
        Self::parse(&format!(
            "{value_type}{vendor}.settings.{category}.{name}.v1"
        ))
    }

    /// The full key, byte-identical to what was parsed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The value-type half, including its trailing `~`.
    #[must_use]
    pub fn value_type(&self) -> &str {
        &self.raw[..=self.separator]
    }

    /// The instance-id half, without a trailing `~`.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.raw[self.separator + TYPE_TERMINATOR.len_utf8()..]
    }

    /// The owning category's slug.
    ///
    /// Always present: the GTS grammar guarantees the instance segment carries a
    /// namespace token, and that position is the category for both authoring
    /// parties — an admin key puts it there by construction, and the reconciler
    /// reads a module's category from the same position.
    #[must_use]
    pub fn category_slug(&self) -> &str {
        &self.category
    }

    /// The setting's own leaf name, which uniqueness is enforced on within a category.
    #[must_use]
    pub fn leaf_slug(&self) -> &str {
        &self.leaf
    }
}

impl fmt::Display for SettingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SettingKey {
    type Err = SettingKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Serializes as the bare key string, so the wire shape is the key itself.
impl Serialize for SettingKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Deserializes through [`SettingKey::parse`], so a malformed key never enters
/// the type; consumers cannot receive a `SettingKey` that would not round-trip.
impl<'de> Deserialize<'de> for SettingKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod key_tests;
