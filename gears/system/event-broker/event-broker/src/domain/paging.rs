//! Keyset paging over an in-memory list, sorted by one string key ascending.
//!
//! The lists this gear serves (topics, event types, consumer groups,
//! subscriptions) are generic entity collections with no natural offset: an
//! offset cursor over them silently skips or duplicates rows when the backing
//! map changes between page requests. Paging seeks on each item's own stable
//! key instead.
//!
//! This module owns the seek and the page boundaries only. A caller turns its
//! own query into a [`PageRequest`] and its own cursor token into a
//! [`PageCursor`]: the REST layer from an `OData` `CursorV1` (which it validates
//! against the order and `$filter` first), [`LocalBroker`] from the in-process
//! token that [`PageCursor`]'s `Display`/`FromStr` define.
//!
//! [`LocalBroker`]: crate::domain::local_broker::LocalBroker

use std::fmt;
use std::str::FromStr;

use toolkit::domain_model;

use crate::domain::error::{DomainError, ErrorCode};

/// Items per page when the caller names no limit.
const DEFAULT_LIMIT: u32 = 25;
/// The largest page; a larger request is clamped to it.
const MAX_LIMIT: u32 = 200;

/// Items per page: 1..=200, default 25.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimit(u32);

impl PageLimit {
    /// Clamps `raw` into 1..=200, so zero becomes one.
    #[must_use]
    pub fn clamped(raw: u64) -> Self {
        let bounded = raw.clamp(1, u64::from(MAX_LIMIT));
        Self(u32::try_from(bounded).unwrap_or(MAX_LIMIT))
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self(DEFAULT_LIMIT)
    }
}

/// A caller-named limit: zero is rejected, above 200 is clamped.
impl TryFrom<u32> for PageLimit {
    type Error = DomainError;

    fn try_from(raw: u32) -> Result<Self, Self::Error> {
        if raw == 0 {
            return Err(DomainError::Validation {
                code: ErrorCode::InvalidQuery,
                message: "limit must be at least 1".to_owned(),
            });
        }
        Ok(Self(raw.min(MAX_LIMIT)))
    }
}

impl From<PageLimit> for u32 {
    fn from(limit: PageLimit) -> Self {
        limit.0
    }
}

impl From<PageLimit> for u64 {
    fn from(limit: PageLimit) -> Self {
        u64::from(limit.0)
    }
}

impl From<PageLimit> for usize {
    fn from(limit: PageLimit) -> Self {
        usize::try_from(limit.0).unwrap_or(usize::MAX)
    }
}

/// Which way a page extends from its cursor.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDirection {
    /// The items strictly after the cursor's key.
    Forward,
    /// The items strictly before the cursor's key.
    Backward,
}

/// A keyset position: the key of the last item seen (`Forward`) or of the
/// first (`Backward`).
///
/// Its in-process token is `fwd:<key>` or `bwd:<key>`. Parsing splits at the
/// first `:`, and the direction prefix holds none, so a key may contain `:`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor {
    pub key: String,
    pub direction: PageDirection,
}

impl fmt::Display for PageCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.direction {
            PageDirection::Forward => "fwd",
            PageDirection::Backward => "bwd",
        };
        write!(f, "{prefix}:{}", self.key)
    }
}

/// The rejection does not quote the token back.
impl FromStr for PageCursor {
    type Err = DomainError;

    fn from_str(token: &str) -> Result<Self, Self::Err> {
        let (direction, key) = match token.split_once(':') {
            Some(("fwd", key)) => (PageDirection::Forward, key),
            Some(("bwd", key)) => (PageDirection::Backward, key),
            _ => {
                return Err(DomainError::Validation {
                    code: ErrorCode::InvalidQuery,
                    message: "cursor is not a valid page cursor".to_owned(),
                });
            }
        };
        Ok(Self {
            key: key.to_owned(),
            direction,
        })
    }
}

/// One page to take: from the start of the list, or from a cursor.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PageRequest {
    cursor: Option<PageCursor>,
    limit: PageLimit,
}

impl PageRequest {
    /// The first page.
    #[must_use]
    pub fn first(limit: PageLimit) -> Self {
        Self {
            cursor: None,
            limit,
        }
    }

    /// The page that extends from `cursor` in its direction.
    #[must_use]
    pub fn at(cursor: PageCursor, limit: PageLimit) -> Self {
        Self {
            cursor: Some(cursor),
            limit,
        }
    }

    /// Sorts `items` by `key_of` ascending, seeks to the cursor, and takes one
    /// page. `next` is set when items follow the page, `prev` when items
    /// precede it.
    #[must_use]
    pub fn page<T>(&self, mut items: Vec<T>, key_of: impl Fn(&T) -> String) -> KeysetPage<T> {
        items.sort_by_key(|item| key_of(item));
        let total = items.len();
        let limit = usize::from(self.limit);

        let (start, end) = match &self.cursor {
            None => (0, limit.min(total)),
            Some(cursor) if cursor.direction == PageDirection::Backward => {
                // `cursor.key` is the first item's key of the page this
                // `prev` cursor was built from - everything strictly before
                // it, taking the `limit` items closest to it.
                let before = items.partition_point(|item| key_of(item) < cursor.key);
                (before.saturating_sub(limit), before)
            }
            Some(cursor) => {
                // `cursor.key` is the last item's key of the previous page -
                // everything strictly after it.
                let after = items.partition_point(|item| key_of(item) <= cursor.key);
                (after, (after + limit).min(total))
            }
        };

        let page_items: Vec<T> = items.drain(start..end).collect();

        let next = match page_items.last() {
            Some(last) if end < total => Some(PageCursor {
                key: key_of(last),
                direction: PageDirection::Forward,
            }),
            _ => None,
        };
        let prev = match page_items.first() {
            Some(first) if start > 0 => Some(PageCursor {
                key: key_of(first),
                direction: PageDirection::Backward,
            }),
            _ => None,
        };

        KeysetPage {
            items: page_items,
            next,
            prev,
            limit: self.limit,
        }
    }
}

/// One page of items with the cursors to its neighbours.
#[domain_model]
#[derive(Debug, Clone)]
pub struct KeysetPage<T> {
    pub items: Vec<T>,
    pub next: Option<PageCursor>,
    pub prev: Option<PageCursor>,
    pub limit: PageLimit,
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
