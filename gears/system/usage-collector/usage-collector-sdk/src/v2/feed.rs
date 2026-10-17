//! The usage feed's position, page, start mode and subscription.
//!
//! The feed is the replay-safe read path a charging consumer uses instead of
//! `list_usage_records`, and these are the types its two Rust surfaces speak:
//! the Plugin SPI reads a page over the plugin's own [`FeedPosition`], and the
//! SDK client trait speaks them over the wire cursor via
//! [`crate::v2::api::UsageCollectorClientV2::read_usage_feed`].

use thiserror::Error;

use crate::v2::models::{MeterTypeId, UsageRecord};
use crate::v2::stored::MeterRef;

/// The largest [`FeedPosition`] a plugin may issue, in bytes.
///
/// The gateway carries a position inside a `toolkit_odata::CursorV1`, whose
/// wire token `usage-collector-v2.yaml` caps at `maxLength: 4096` characters;
/// base64url of 512 bytes fits inside that with room for the cursor's own
/// fields.
///
/// DESIGN §3.1 puts a further obligation on top of this number: a position's
/// encoded size "may not grow with a subscription's breadth". This constant does
/// not catch that, so a plugin's own tests have to assert it directly.
pub const MAX_FEED_POSITION_BYTES: usize = 512;

/// A point in the feed's order, issued and interpreted by the storage plugin
/// alone.
///
/// Opaque to the gateway and never on the wire (DESIGN §3.1). Its internal
/// structure is the plugin's own choice: the SDK bounds its size and claims
/// nothing about its meaning.
///
/// Ordering is deliberately not provided: bytewise order on the plugin's own
/// encoding is not the feed's order. A plugin that needs to compare its own
/// positions decodes them first.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeedPosition(Vec<u8>);

/// Why a [`FeedPosition`] could not be built.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum FeedPositionInvalid {
    /// The plugin supplied no bytes.
    #[error(
        "a feed position must carry at least one byte: zero bytes is a plugin that did not fill \
         the position rather than a position that needs none"
    )]
    Empty,

    /// The plugin supplied more bytes than the wire cursor can carry.
    #[error(
        "a feed position of {actual} bytes exceeds the {MAX_FEED_POSITION_BYTES}-byte bound the \
         wire cursor can carry; a position this large cannot be handed to a consumer"
    )]
    TooLarge {
        /// How many bytes the plugin supplied.
        actual: usize,
    },
}

impl FeedPosition {
    /// Builds a position from a plugin's own encoding.
    ///
    /// # Errors
    ///
    /// [`FeedPositionInvalid::Empty`] when `bytes` is empty, and
    /// [`FeedPositionInvalid::TooLarge`] when it exceeds
    /// [`MAX_FEED_POSITION_BYTES`].
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, FeedPositionInvalid> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(FeedPositionInvalid::Empty);
        }
        if bytes.len() > MAX_FEED_POSITION_BYTES {
            return Err(FeedPositionInvalid::TooLarge {
                actual: bytes.len(),
            });
        }
        Ok(Self(bytes))
    }

    /// The plugin's encoding, borrowed.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes the position encodes to.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false`: [`FeedPosition::new`] never admits an empty sequence.
    /// Present because `clippy::len_without_is_empty` asks for it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for FeedPosition {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Where a feed read begins — **named**, never inferred from an absent position.
///
/// The gateway compiles a request carrying no wire cursor to
/// [`FeedStart::Oldest`] rather than passing an absent position down.
///
/// `#[non_exhaustive]` per DESIGN §2.2's additive-evolution constraint: a start
/// mode admitted later is a variant rather than a further argument, so a plugin
/// matches this with a wildcard arm and answers
/// [`crate::v2::error::UsageCollectorPluginError::Internal`] there.
///
/// Generic over the position each surface speaks: `FeedStart<FeedPosition>` on
/// the Plugin SPI, and `FeedStart<&CursorV1>` on the SDK client trait.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FeedStart<P> {
    /// The oldest entry the subscription still retains. Never the head, and
    /// never refused on the retention floor.
    Oldest,
    /// Continue after a position the feed issued.
    After(P),
}

/// Settled entries in feed order, plus the cursor that continues them.
///
/// Everything before `next` is delivered and final under the compiled scope the
/// read ran under (DESIGN §3.1).
///
/// Generic over the cursor kind, which is what keeps a plugin's position off the
/// wire: the SPI returns `FeedPage<FeedPosition, StoredUsageRecord>`, and the SDK
/// client and REST return `FeedPage<CursorV1>`, so handing a raw position to a
/// consumer is a type error rather than a review obligation.
///
/// The entry kind is a parameter for the same reason: a plugin's page carries
/// [`StoredUsageRecord`](crate::v2::StoredUsageRecord)s, which name their meter by
/// registry reference alone, while the page a consumer reads carries
/// [`UsageRecord`]s. It **defaults to [`UsageRecord`]**, so `FeedPage<CursorV1>`
/// still spells the consumer-facing page.
// No `Eq`: a derive bounds type parameters only, and both entry kinds derive
// `PartialEq` without `Eq`. `PartialEq` is conditional on the cursor kind for the
// same reason: `toolkit_odata::CursorV1` derives `Clone` and `Debug` only.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedPage<C, R = UsageRecord> {
    /// The page's entries, in feed order.
    pub entries: Vec<R>,
    /// The continuation. `Some` on every page of a live read, short pages
    /// included; `None` once a bounded replay has reached its `until`.
    pub next: Option<C>,
}

/// The set of GTS types one consumer reads.
///
/// It bounds that consumer's pages and its cursor (DESIGN §3.1). Held as a
/// sorted, deduplicated sequence, so the same set of types is the same
/// subscription whatever order the caller listed it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedSubscription(Vec<MeterTypeId>);

/// Why a [`FeedSubscription`] could not be built.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum FeedSubscriptionInvalid {
    /// The caller named no GTS type.
    #[error(
        "a feed subscription must name at least one GTS type: a subscription reading nothing has \
         no page to serve"
    )]
    Empty,
}

impl FeedSubscription {
    /// Builds a subscription, deduplicating and ordering the types.
    ///
    /// # Errors
    ///
    /// [`FeedSubscriptionInvalid::Empty`] when `types` yields nothing.
    pub fn new(
        types: impl IntoIterator<Item = MeterTypeId>,
    ) -> Result<Self, FeedSubscriptionInvalid> {
        let mut types: Vec<MeterTypeId> = types.into_iter().collect();
        // `MeterTypeId` implements neither `Ord` nor `PartialOrd`, deliberately:
        // its rustdoc tells a caller needing an order to use the string form.
        types.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        types.dedup_by(|a, b| a.as_str() == b.as_str());
        if types.is_empty() {
            return Err(FeedSubscriptionInvalid::Empty);
        }
        Ok(Self(types))
    }

    /// The subscribed types, sorted and deduplicated.
    #[must_use]
    pub fn types(&self) -> &[MeterTypeId] {
        &self.0
    }
}

/// The meters one feed read is bounded by, as the Plugin SPI speaks them.
///
/// The reference-keyed counterpart of [`FeedSubscription`], which names the
/// same set by GTS *identifier* for the consumer-facing surfaces. Held as a
/// non-empty, deduplicated sequence ordered by reference, so the same set of
/// meters is the same subscription whatever order a caller listed it in.
///
/// [`crate::v2::UsageCollectorPluginV2::read_feed_page`] takes this rather
/// than `&[MeterRef]` because the slice expressed neither obligation: a
/// plugin could be handed an empty subscription, which bounds a page to
/// nothing, or a repeated reference, which is why `PgRecordStore::feed_page`
/// deduplicated defensively on the way in. Both are decided here instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribedMeters(Vec<MeterRef>);

/// Why a [`SubscribedMeters`] could not be built.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubscribedMetersInvalid {
    /// The caller named no meter.
    #[error(
        "a feed subscription must name at least one meter: a page is bounded by its \
         subscription, so one naming nothing has no page to serve"
    )]
    Empty,

    /// Two entries share a registry reference but name different identifiers.
    #[error(
        "subscription resolves registry reference {reference} to two distinct identifiers; one \
         identifier maps to one reference for the life of an installation"
    )]
    ConflictingIdentifiers {
        /// The reference both entries carried.
        reference: uuid::Uuid,
    },
}

impl SubscribedMeters {
    /// Builds the set, deduplicating and ordering by reference.
    ///
    /// Deduplication reads the whole [`MeterRef`]: the same meter named twice
    /// is one meter. Two entries sharing [`MeterRef::uuid`] but differing in
    /// [`MeterRef::id`] are **refused**, not collapsed — `types-registry`
    /// maps one identifier to one reference for the life of an installation
    /// (`cpt-cf-types-registry-adr-storage-identity-query-model`), so such a
    /// pair is that guarantee already broken. Collapsing it would drop one
    /// label and serve every entry of *both* meters under the survivor, the
    /// one relabelling route the re-attachment guards do not close.
    ///
    /// # Errors
    ///
    /// [`SubscribedMetersInvalid::Empty`] when `meters` yields nothing, and
    /// [`SubscribedMetersInvalid::ConflictingIdentifiers`] when one reference
    /// arrives under two identifiers.
    pub fn new(
        meters: impl IntoIterator<Item = MeterRef>,
    ) -> Result<Self, SubscribedMetersInvalid> {
        let mut meters: Vec<MeterRef> = meters.into_iter().collect();
        meters.sort_by(|a, b| {
            a.uuid
                .cmp(&b.uuid)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        meters.dedup();
        // Sorted by reference first, so a reference under two identifiers is
        // now an adjacent pair agreeing on `uuid` and differing in `id`.
        if let Some(pair) = meters.windows(2).find(|w| w[0].uuid == w[1].uuid) {
            return Err(SubscribedMetersInvalid::ConflictingIdentifiers {
                reference: pair[0].uuid,
            });
        }
        if meters.is_empty() {
            return Err(SubscribedMetersInvalid::Empty);
        }
        Ok(Self(meters))
    }

    /// The subscribed meters, ordered by reference and deduplicated.
    #[must_use]
    pub fn refs(&self) -> &[MeterRef] {
        &self.0
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "feed_tests.rs"]
mod feed_tests;
