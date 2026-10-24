//! Keyset paging (DESIGN §3.3): an opaque cursor and a `limit` of 1–500, 100 by default.
//!
//! A collection is read forward from a cursor the server issued; offsets are never accepted. The
//! cursor encodes the keyset position of the last item returned, so a page read twice gives the
//! same items whatever was inserted before the position in between.

use serde::{Deserialize, Serialize};

/// The number of items a page holds when the caller names none.
pub const DEFAULT_PAGE_LIMIT: u16 = 100;
/// The most items one page may hold.
pub const MAX_PAGE_LIMIT: u16 = 500;
/// The longest cursor a server issues; a longer one is refused unread.
pub const MAX_CURSOR_LEN: usize = 1024;

/// Why a paging argument is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PagingError {
    /// `limit` is outside 1–500.
    #[error("limit must be between 1 and 500")]
    LimitOutOfRange,
    /// The cursor is not one a server issued.
    #[error("cursor is not a valid page cursor")]
    InvalidCursor,
}

/// The page size a caller asked for, within 1–500.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct PageLimit(u16);

impl PageLimit {
    /// The page size for `requested`, or the default when the caller named none.
    ///
    /// # Errors
    ///
    /// [`PagingError::LimitOutOfRange`] for 0 or anything above 500.
    pub fn new(requested: Option<u32>) -> Result<Self, PagingError> {
        match requested {
            None => Ok(Self(DEFAULT_PAGE_LIMIT)),
            Some(limit) => u16::try_from(limit)
                .map_err(|_| PagingError::LimitOutOfRange)
                .and_then(Self::try_from),
        }
    }

    /// The page size.
    #[must_use]
    pub fn get(self) -> u16 {
        self.0
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self(DEFAULT_PAGE_LIMIT)
    }
}

impl TryFrom<u16> for PageLimit {
    type Error = PagingError;

    fn try_from(limit: u16) -> Result<Self, Self::Error> {
        if (1..=MAX_PAGE_LIMIT).contains(&limit) {
            Ok(Self(limit))
        } else {
            Err(PagingError::LimitOutOfRange)
        }
    }
}

impl From<PageLimit> for u16 {
    fn from(limit: PageLimit) -> Self {
        limit.0
    }
}

/// An opaque keyset position: URL-safe base64 without padding, as a server issued it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PageCursor(String);

impl PageCursor {
    /// Accepts a cursor token in its wire alphabet. Whether it names a position is the server's
    /// to decide when it decodes it.
    ///
    /// # Errors
    ///
    /// [`PagingError::InvalidCursor`] for an empty or oversized token or a character outside the
    /// URL-safe base64 alphabet.
    pub fn new(token: impl Into<String>) -> Result<Self, PagingError> {
        let token = token.into();
        if token.is_empty()
            || token.len() > MAX_CURSOR_LEN
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(PagingError::InvalidCursor);
        }
        Ok(Self(token))
    }

    /// The token as it travels.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PageCursor {
    type Error = PagingError;

    fn try_from(token: String) -> Result<Self, Self::Error> {
        Self::new(token)
    }
}

impl From<PageCursor> for String {
    fn from(cursor: PageCursor) -> Self {
        cursor.0
    }
}

/// Where the next page starts and the size this one was read with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PageInfo {
    /// The cursor of the next page; absent on the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
    /// The page size the page was read with.
    pub limit: PageLimit,
}

impl PageInfo {
    /// The position after a page read with `limit`; `next_cursor` is absent on the last page.
    #[must_use]
    pub fn new(next_cursor: Option<PageCursor>, limit: PageLimit) -> Self {
        Self { next_cursor, limit }
    }
}

/// One page of a keyset-paged collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Page<T> {
    /// The items, in the collection's keyset order.
    pub items: Vec<T>,
    /// The position of the next page.
    pub page_info: PageInfo,
}

impl<T> Page<T> {
    /// A page of `items` at `page_info`.
    #[must_use]
    pub fn new(items: Vec<T>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }

    /// Maps the items, keeping the paging position.
    #[must_use]
    pub fn map_items<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            page_info: self.page_info,
        }
    }
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
