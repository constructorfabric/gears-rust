//! Rating's port onto the usage-collector feed (DESIGN [12 §3.3]; T-D-84).
//!
//! The feed is the collector's version 2 contract, `UsageCollectorClientV2::read_usage_feed` (PR
//! #5359, approved, not merged; nothing serves it before stage 2 of the collector's rollout). The
//! reader depends on this port instead of the SDK trait, so feature 04 is built and accepted
//! against `FakeUsageFeed`; the adapter over the SDK lands once the contract merges.
//!
//! The port hands every entry over as the JSON object it arrived as. Capture stores that object
//! complete before anything reads it, so an entry Rating cannot read is still captured (a capture
//! reject, V65) and an optional field the collector adds later is kept for audit.
//!
//! [12 §3.3]: ../../../../docs/DESIGN.md#contract-12-3-3

use std::fmt;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_gts::gts_id;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::error::{Cause, DomainError};

/// The GTS base type every meter derives from (usage-collector v2 `USAGE_RECORD_BASE_TYPE`).
pub const METER_BASE_TYPE: &str = gts_id!("cf.core.uc.usage_record.v2~");

/// The longest meter id the collector accepts (`usage_record.v2.schema.json`).
pub const MAX_METER_ID_LEN: usize = 512;

/// The most meters one feed subscription may name (usage-collector YAML).
pub const MAX_SUBSCRIPTION_METERS: usize = 100;

/// The largest page the feed serves (`MAX_PAGE_SIZE`).
pub const MAX_PAGE_LIMIT: u16 = 1000;

/// The longest cursor token the feed issues (`maxLength: 4096`).
pub const MAX_CURSOR_LEN: usize = 4096;

/// Whether the collector refuses `c` in a meter id, an attribution component, an idempotency key
/// or a reason code: an ASCII control character, U+0000–U+001F and U+007F (`models.rs`
/// `check_attribution_component`, `MeterTypeId::new`). Every text field Rating reads applies this
/// one rule, so Rating refuses no entry the collector admitted.
#[must_use]
pub const fn is_refused_char(c: char) -> bool {
    c.is_ascii_control()
}

/// A meter: a GTS type derived from [`METER_BASE_TYPE`] with exactly one further segment, e.g.
/// `gts.cf.core.uc.usage_record.v2~cf.oss.cloudlet_hours.v1~`. The collector's own grammar on top
/// of the GTS grammar; Rating checks the shape and leaves the segment grammar to the collector and
/// types-registry.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeterId(String);

impl MeterId {
    /// A meter id in the collector's shape.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] on `gts_type_id` for an id longer than
    /// [`MAX_METER_ID_LEN`], one holding an ASCII control character ([`is_refused_char`]), or one
    /// that does not add exactly one `~`-terminated segment to [`METER_BASE_TYPE`].
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let refuse = |detail: &str| DomainError::InvalidArgument {
            field: "gts_type_id",
            detail: detail.to_owned(),
            reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
        };
        if value.len() > MAX_METER_ID_LEN || value.chars().any(is_refused_char) {
            return Err(refuse(
                "a meter id is at most 512 bytes without ASCII control characters",
            ));
        }
        let segment = value
            .strip_prefix(METER_BASE_TYPE)
            .and_then(|rest| rest.strip_suffix('~'))
            .ok_or_else(|| refuse("a meter id derives from gts.cf.core.uc.usage_record.v2~"))?;
        if segment.is_empty() || segment.contains('~') {
            return Err(refuse(
                "a meter id adds exactly one segment to gts.cf.core.uc.usage_record.v2~",
            ));
        }
        Ok(Self(value))
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MeterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The meters one source reads: sorted, deduplicated, 1 to [`MAX_SUBSCRIPTION_METERS`] of them, so
/// the same set is the same subscription whatever order configuration lists it in.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedSubscription(Vec<MeterId>);

impl FeedSubscription {
    /// The subscription of `meters`.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] on `meters` when there are none or more than
    /// [`MAX_SUBSCRIPTION_METERS`] distinct ones.
    pub fn new(meters: impl IntoIterator<Item = MeterId>) -> Result<Self, DomainError> {
        let mut meters: Vec<MeterId> = meters.into_iter().collect();
        meters.sort();
        meters.dedup();
        if meters.is_empty() || meters.len() > MAX_SUBSCRIPTION_METERS {
            return Err(DomainError::InvalidArgument {
                field: "meters",
                detail: format!("a feed subscription names 1 to {MAX_SUBSCRIPTION_METERS} meters"),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            });
        }
        Ok(Self(meters))
    }

    /// The meters, sorted.
    #[must_use]
    pub fn meters(&self) -> &[MeterId] {
        &self.0
    }
}

/// The collector's opaque continuation token. Only ever sent back with the subscription of the
/// source that received it (12 §4.6).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedCursor(String);

impl FeedCursor {
    /// A cursor as the feed issued it or the checkpoint stored it.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] on `cursor` for an empty token or one longer than
    /// [`MAX_CURSOR_LEN`].
    pub fn parse(token: impl Into<String>) -> Result<Self, DomainError> {
        let token = token.into();
        if token.is_empty() || token.len() > MAX_CURSOR_LEN {
            return Err(DomainError::InvalidArgument {
                field: "cursor",
                detail: format!("a feed cursor is 1 to {MAX_CURSOR_LEN} bytes"),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            });
        }
        Ok(Self(token))
    }

    /// The token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a read begins, named: a source without a stored cursor reads from the oldest entry the
/// collector retains.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedStart<'a> {
    /// The oldest retained entry; never refused.
    Oldest,
    /// After a page the feed delivered.
    After(&'a FeedCursor),
}

impl<'a> FeedStart<'a> {
    /// The start of a source whose checkpoint holds `cursor`.
    #[must_use]
    pub const fn from_checkpoint(cursor: Option<&'a FeedCursor>) -> Self {
        match cursor {
            Some(cursor) => Self::After(cursor),
            None => Self::Oldest,
        }
    }
}

/// One page: settled entries in feed order, each the JSON object it arrived as, and the cursor
/// that continues them (present on every page of a live read, absent only at a bounded replay's
/// `until`).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct FeedPage {
    /// The entries, in feed order.
    pub entries: Vec<Value>,
    /// The continuation.
    pub next: Option<FeedCursor>,
}

/// Why a read returned no page.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    /// `InvalidArgument(CursorBeyondRetention)`: retention removed entries after the cursor that
    /// Rating never captured. The reader records a source loss and restarts from `Oldest`.
    #[error("the cursor is beyond the collector's retention")]
    CursorBeyondRetention,
    /// The collector or its transport is unavailable; the reader backs off. The SDK's error, when
    /// there is one, is the source.
    #[error("usage feed unavailable: {0}")]
    Unavailable(String, #[source] Option<Cause>),
    /// The collector refused the read (a rejected cursor, a filter mismatch, a denied read). Not
    /// transient on its own; the reader backs off and reports it.
    #[error("usage feed refused the read: {0}")]
    Refused(String),
}

/// The usage feed, as the reader consumes it.
#[async_trait]
pub trait UsageFeedPort: Send + Sync {
    /// One page of `subscription` from `start`, at most `limit` entries, read as `ctx`.
    ///
    /// # Errors
    ///
    /// [`FeedError`].
    async fn read_usage_feed(
        &self,
        ctx: &SecurityContext,
        subscription: &FeedSubscription,
        start: FeedStart<'_>,
        until: Option<&FeedCursor>,
        limit: u16,
    ) -> Result<FeedPage, FeedError>;
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
