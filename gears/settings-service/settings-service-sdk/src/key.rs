// Created: 2026-08-11 by Constructor Tech
// @cpt-algo:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1
//! The setting key value object.
//!
//! A setting key is a GTS **type** identifier made of exactly two segments: the
//! abstract base type every setting derives from, then the setting's own derived
//! half. Both segments are types and both end with `~`.
//!
//! ```text
//! gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~
//! └────── base type, owned by this gear ─────┘└─ derived half: vendor.package.category.name ─┘
//! ```
//!
//! The derived half carries exactly four name tokens before its version, with
//! **the category always third**. An admin-authored setting is composed as
//! `<vendor>.settings.<category>.<name>.v1`; a module supplies its own half and
//! the category is read from the same position. The trailing `~` is what makes
//! the key a type rather than an instance — and a type is what a policy can name
//! as its resource, which an instance identifier could not be (ADR-002).
//!
//! The value's **shape** is not in the key. It is a separate curated catalog type
//! (`gts.cf.toolkit.settings.type_*~`) named by the declaration's `value_type_id`,
//! so a value-shape change is an evolution of the same setting, not a new key.
//!
//! Grammar validation is delegated to `gts-id`, the platform's single source of
//! truth for GTS identifiers. This module adds only the rules `gts-id` cannot
//! know about: that a setting key is exactly the base followed by one derived
//! type, and where the category and leaf name sit within the derived half.

use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use crate::gts::SETTING_TYPE_BASE;

/// Terminator that marks the end of a GTS **type** segment.
pub const TYPE_TERMINATOR: char = '~';

/// Number of segments a setting key must have: the base type and the derived half.
const SETTING_KEY_SEGMENTS: usize = 2;

/// Why a candidate setting key was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingKeyError {
    /// The identifier is not the base type followed by exactly one derived half.
    #[error(
        "a setting key is the setting base type followed by one derived half ({SETTING_KEY_SEGMENTS} segments), got {count}"
    )]
    SegmentCount {
        /// How many GTS segments the identifier actually had.
        count: usize,
    },

    /// The first segment is not the setting base type this gear owns.
    #[error("a setting key derives from `{SETTING_TYPE_BASE}`, not from `{found}`")]
    WrongBaseType {
        /// The base segment the candidate actually carried.
        found: String,
    },

    /// The derived half does not end with `~`, so it is an instance, not a type.
    ///
    /// A setting is a GTS *type* on purpose: a policy names one setting as its
    /// resource, and only a type can be a policy resource. An instance-shaped
    /// half would silently produce a key nothing can be authorized against.
    #[error(
        "the derived half must end with `{TYPE_TERMINATOR}`: a setting is a GTS type so that a policy can name it"
    )]
    DerivedNotAType,

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

    /// The derived half carries no authored category and leaf name.
    ///
    /// GTS allows a UUID tail for machine-generated identifiers. A setting is not
    /// one: its category and leaf name are what an administrator browses and a
    /// module author writes down, and a UUID supplies neither.
    #[error("a setting key names its category and leaf; an anonymous derived half has neither")]
    AnonymousDerivedHalf,

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
    /// Byte index of the `~` that terminates the base-type segment.
    separator: usize,
    /// Namespace token of the derived half: the owning category's slug.
    category: String,
    /// Type token of the derived half: the setting's own leaf name.
    leaf: String,
}

impl SettingKey {
    /// Parse a candidate setting key.
    ///
    /// # Errors
    ///
    /// Returns [`SettingKeyError`] when the candidate is not a valid GTS
    /// identifier, or is valid but is not the setting base type followed by one
    /// derived type.
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
        let [base, derived] = segments.as_slice() else {
            return Err(SettingKeyError::SegmentCount {
                count: segments.len(),
            });
        };
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-3

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-4
        // The base is fixed, not merely "some type": every setting derives from
        // the one abstract type this gear registers at init, and a key rooted
        // anywhere else is not a setting whatever else it may be.
        //
        // Compared on the candidate's own bytes rather than on the parsed
        // segment: `gts-id` renders a segment without its `gts.` prefix, and the
        // constant is the wire form a caller can read off a policy or an audit
        // record.
        let separator = raw
            .find(TYPE_TERMINATOR)
            .ok_or(SettingKeyError::DerivedNotAType)?;
        let found = &raw[..=separator];
        if found != SETTING_TYPE_BASE {
            return Err(SettingKeyError::WrongBaseType {
                found: found.to_owned(),
            });
        }
        debug_assert!(base.is_type(), "the base compared equal to a type");
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-4

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-5
        if !derived.is_type() {
            return Err(SettingKeyError::DerivedNotAType);
        }
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-5

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-6
        // The derived half's namespace token is the owning category and its type
        // token is the leaf name, for admin and module authors alike.
        //
        // A UUID tail carries neither: `namespace()` and `type_name()` answer
        // `""` for it, so accepting one would produce a key with an empty
        // category and leaf that still compared equal to itself. A setting is
        // named by its author, never generated.
        let (Some(category), Some(leaf)) = (
            non_empty(derived.namespace()),
            non_empty(derived.type_name()),
        ) else {
            return Err(SettingKeyError::AnonymousDerivedHalf);
        };
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-6

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-key-parse:p1:inst-gf-key-7
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
    /// Builds the derived half as `<vendor>.settings.<category>.<name>.v1~` and
    /// roots it under the setting base type. The value type is **not** an input:
    /// the value's shape is a separate catalog type named by the declaration's
    /// `value_type_id`, so the same key survives a value-shape change.
    ///
    /// The category slug sits **inside** the key, which makes the key a function
    /// of its category: moving a setting to another category, or renaming the
    /// category itself, re-keys the setting, and no alias to the old key is
    /// retained. That is why a category key is refused on update rather than
    /// merely discouraged — an in-place change would re-key every declaration
    /// filed under it with nothing left pointing at the old name.
    ///
    /// # Errors
    ///
    /// Returns [`SettingKeyError`] when the composed key is not a valid setting
    /// key — an uppercase vendor, a `/` in a slug, a name that is not a GTS token.
    pub fn compose(vendor: &str, category: &str, name: &str) -> Result<Self, SettingKeyError> {
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-3
        // The `settings` package is fixed rather than supplied: an admin-authored
        // derived half always sits at `<vendor>.settings.<category>.<name>.v1~`,
        // the trailing terminator making it a type.
        let derived = format!("{vendor}.settings.{category}.{name}.v1{TYPE_TERMINATOR}");
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-3

        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-4
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-1
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-2
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-5
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-6
        // Grammar validation, the segment-level rejection, the leaf slug and the
        // embedded category slug all come from one parse of the composed
        // candidate. Validating the parts separately would let a composed key and
        // a parsed one disagree about any of them.
        Self::parse(&format!("{SETTING_TYPE_BASE}{derived}"))
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-6
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-5
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-2
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-1
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-key-construction:p1:inst-decl-key-4
    }

    /// Compose a module-contributed key.
    ///
    /// A contributed derived half is `<vendor>.<package>.<category>.<name>.v<major>~`:
    /// the module names its own vendor and package, the category is the slug
    /// its settings file under, and the major is the setting's own version —
    /// bumped when a behavior-affecting field changes, so that `…retry_policy.v1~`
    /// and `…retry_policy.v2~` are two declarations on one version-stripped
    /// path. The value type is not an input, exactly as for [`Self::compose`].
    ///
    /// # Errors
    ///
    /// Returns [`SettingKeyError`] when the composed key is not a valid setting
    /// key — an uppercase segment, a `/`, a token the GTS grammar refuses. A
    /// major of zero cannot be asked for: the parameter is non-zero by type.
    pub fn contributed(
        vendor: &str,
        package: &str,
        category: &str,
        name: &str,
        major: NonZeroU32,
    ) -> Result<Self, SettingKeyError> {
        let derived = format!("{vendor}.{package}.{category}.{name}.v{major}{TYPE_TERMINATOR}");
        Self::parse(&format!("{SETTING_TYPE_BASE}{derived}"))
    }

    /// The setting's major version — the `N` of the derived half's `.vN~`.
    #[must_use]
    pub fn major(&self) -> u32 {
        let derived = self.derived_half().trim_end_matches(TYPE_TERMINATOR);
        derived
            .rsplit_once(".v")
            .and_then(|(_, digits)| digits.parse().ok())
            .unwrap_or(1)
    }

    /// The derived half without its version — what "the same setting across
    /// versions" means.
    ///
    /// `gts.cf.core.settings.setting_type.v1~cf.toolkit.cat.sett1.v2~` yields
    /// `cf.toolkit.cat.sett1`; every major of one setting shares this path, and
    /// succession between them is derived from it rather than stored.
    #[must_use]
    pub fn version_stripped_path(&self) -> &str {
        let derived = self.derived_half().trim_end_matches(TYPE_TERMINATOR);
        derived.rsplit_once(".v").map_or(derived, |(path, _)| path)
    }

    /// The full key, byte-identical to what was parsed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The base-type half, including its trailing `~`.
    ///
    /// Always [`SETTING_TYPE_BASE`]; exposed so a caller can slice the key
    /// without re-deriving the split.
    #[must_use]
    pub fn base_type(&self) -> &str {
        &self.raw[..=self.separator]
    }

    /// The derived half — the setting's own type, including its trailing `~`.
    #[must_use]
    pub fn derived_half(&self) -> &str {
        &self.raw[self.separator + TYPE_TERMINATOR.len_utf8()..]
    }

    /// The owning category's slug.
    ///
    /// Always present: the GTS grammar guarantees the derived half carries a
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
