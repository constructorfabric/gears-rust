//! The raw read path's typed keyset and page.
//!
//! [`Keyset`] is the structured last-row sort tuple a storage plugin returns
//! from `UsageCollectorPluginV2::list_usage_records` in place of a page
//! carrying an encoded string; the gateway turns it into a
//! `toolkit_odata::CursorV1`, as it turns a [`crate::v2::feed::FeedPosition`]
//! into one on the feed path (`cpt-cf-usage-collector-dod-gateway-owned-cursor`).
//!
//! **This type deliberately implements neither `Serialize` nor `Deserialize`.**
//! The wire cursor is the gateway's to mint, so a keyset has no wire form of its
//! own, and the absence is enforced by the compiler.

use thiserror::Error;
use toolkit_odata::SortDir;

use crate::v2::stored::StoredUsageRecord;

/// The largest JSON-encoded [`Keyset`] values array the wire cursor can carry.
///
/// Pinned by `keyset_tests.rs`.
pub const MAX_KEYSET_BYTES: usize = 2243;

/// The largest page any read path serves in one call — what the SPI means
/// by "the published bound", published here rather than nowhere.
///
/// REST refuses a larger `$top` as `InvalidArgument` and defaults an absent
/// one to it. The ledger read paths clamp to it as defense in depth; the
/// feed path refuses a `limit` outside `1..=MAX_PAGE_SIZE` as `Internal`
/// instead, a page size the caller did not ask for being a break of its
/// exactly-resumable-scan contract. Raising it is breaking for every
/// coupled consumer.
pub const MAX_PAGE_SIZE: u64 = 1000;

/// Why a [`Keyset`] could not be built.
///
/// A plugin maps either variant to `UsageCollectorPluginError::internal`, and
/// that mapping is right for both: neither is reachable from caller input.
/// [`Self::TooLarge`] once was — an attribution value of JSON-escaped control
/// characters was ingested happily and made a later order on that field a
/// **500 the caller provoked** — until `ResourceRef::new` and
/// `SubjectRef::new` began refusing those characters, where it is a 400
/// instead. With them gone the widest admissible keyset is the 4-byte-scalar
/// case [`MAX_KEYSET_BYTES`] is derived against
/// (`models_tests::no_admissible_attribution_value_can_outgrow_the_keyset_budget`),
/// so either variant is now a plugin that filled the keyset from something
/// other than a validated record.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeysetInvalid {
    /// The plugin supplied no boundary values.
    #[error(
        "a keyset carries one boundary value per key of the order it was dispatched under, and \
         every admissible order is non-empty: zero values is a plugin that did not fill the \
         keyset rather than a keyset that needs none"
    )]
    Empty,

    /// The plugin's boundary values, JSON-encoded, exceed what the wire
    /// cursor can carry.
    #[error(
        "a keyset whose values encode to {actual} JSON bytes exceeds the {MAX_KEYSET_BYTES}-byte \
         budget the wire cursor can carry; a continuation this large cannot be handed to a \
         consumer"
    )]
    TooLarge {
        /// The JSON-encoded length of the values array, in bytes — see
        /// [`MAX_KEYSET_BYTES`] for why this is not the values' summed
        /// `String::len()`.
        actual: usize,
    },
}

/// The typed last-row sort tuple behind an opaque cursor (DESIGN §3.1).
///
/// Its values are positionally aligned with the keys of the
/// `toolkit_odata::ODataQuery::order` the read was dispatched under — one value
/// per key, in sequence — and `direction` is that order's single direction. The
/// gateway verifies both on return rather than trusting them, neither being
/// expressible in the type.
///
/// Ordering is deliberately not provided: comparing two keysets means
/// comparing them under an order neither carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyset {
    values: Vec<String>,
    direction: SortDir,
}

impl Keyset {
    /// Builds a keyset from the last in-page row's sort values.
    ///
    /// # Errors
    ///
    /// [`KeysetInvalid::Empty`] when `values` is empty, and
    /// [`KeysetInvalid::TooLarge`] when the values' JSON-encoded form (see
    /// [`MAX_KEYSET_BYTES`]) exceeds that bound. The bound is checked here,
    /// where a plugin can still act on it, rather than at encoding time in
    /// the gateway, where the only remaining move is to fail a consumer's
    /// read.
    pub fn new(
        values: impl IntoIterator<Item = impl Into<String>>,
        direction: SortDir,
    ) -> Result<Self, KeysetInvalid> {
        let values: Vec<String> = values.into_iter().map(Into::into).collect();
        if values.is_empty() {
            return Err(KeysetInvalid::Empty);
        }
        // `Vec<String>` has no failure mode under `serde_json`; `map_or` still
        // folds the unreachable `Err` into the measurement rather than
        // `expect`ing it away — treating an unmeasurable value as too large is
        // the same fail-closed choice `TooLarge` already makes.
        let actual = serde_json::to_string(&values).map_or(usize::MAX, |encoded| encoded.len());
        if actual > MAX_KEYSET_BYTES {
            return Err(KeysetInvalid::TooLarge { actual });
        }
        Ok(Self { values, direction })
    }

    /// The boundary values, positionally aligned with the dispatched order.
    #[must_use]
    pub fn values(&self) -> &[String] {
        &self.values
    }

    /// The single sort direction the dispatched order carried.
    #[must_use]
    pub fn direction(&self) -> SortDir {
        self.direction
    }
}

/// A page of ledger entries and the keyset that continues it.
///
/// Public fields, and **not** `#[non_exhaustive]`: an out-of-crate plugin
/// *constructs* this as its SPI return value, and `#[non_exhaustive]` on a
/// struct forbids external struct-literal construction outright. **Recorded so
/// a later edit does not "fix" it.**
///
/// Structurally identical to [`crate::v2::feed::FeedPage`] and deliberately a
/// separate type: the feed's type publishes a settled-entries guarantee the raw
/// read path does not have.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordPage {
    /// The page's entries, as persisted, in the dispatched order.
    pub items: Vec<StoredUsageRecord>,
    /// The last in-page row's keyset where a further page exists, and
    /// `None` where this page is the last.
    pub next: Option<Keyset>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "keyset_tests.rs"]
mod keyset_tests;
