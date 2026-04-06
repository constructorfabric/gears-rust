//! Cursor-based pagination with Stream API
//!
//! This module provides a reusable cursor-based pager that converts a page-fetching function
//! into a Stream of pages or items, hiding cursor management from SDK users.
//!
//! # Example
//!
//! ```rust,ignore
//! use modkit_sdk::pager::{CursorPager, PagerError};
//! use modkit_sdk::odata::{items_stream, pages_stream, QueryBuilder};
//! use futures_util::StreamExt;
//!
//! // Stream of pages
//! let pages = pages_stream(
//!     QueryBuilder::<UserSchema>::new()
//!         .filter(NAME.contains("john"))
//!         .page_size(50),
//!     |query| async move { client.list_users(query).await },
//! );
//!
//! // Stream of items
//! let items = items_stream(
//!     QueryBuilder::<UserSchema>::new()
//!         .filter(NAME.contains("john"))
//!         .page_size(50),
//!     |query| async move { client.list_users(query).await },
//! );
//!
//! // Consume the stream
//! while let Some(result) = items.next().await {
//!     match result {
//!         Ok(user) => println!("User: {:?}", user),
//!         Err(PagerError::Fetch(e)) => eprintln!("Fetch error: {}", e),
//!         Err(PagerError::InvalidCursor(c)) => eprintln!("Invalid cursor: {}", c),
//!     }
//! }
//! ```

use futures_core::Stream;
use modkit_odata::{ODataQuery, Page};
use pin_project_lite::pin_project;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Error type for pagination operations.
///
/// This enum wraps both fetcher errors and cursor decoding failures,
/// ensuring that invalid cursors are not silently ignored.
#[derive(Debug)]
pub enum PagerError<E> {
    /// Error from the fetcher function.
    Fetch(E),
    /// Invalid cursor string that failed to decode.
    InvalidCursor(String),
}

impl<E: fmt::Display> fmt::Display for PagerError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(e) => write!(f, "Fetch error: {e}"),
            Self::InvalidCursor(cursor) => write!(f, "Invalid cursor: {cursor}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for PagerError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fetch(e) => Some(e),
            Self::InvalidCursor(_) => None,
        }
    }
}

pin_project! {
    /// A cursor-based pager that implements `Stream` for paginated items.
    ///
    /// This pager manages cursor state internally and fetches pages on-demand,
    /// yielding individual items from the stream.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The item type
    /// * `E` - The error type
    /// * `F` - The fetcher function type
    /// * `Fut` - The future returned by the fetcher
    pub struct CursorPager<T, E, F, Fut>
    where
        F: FnMut(ODataQuery) -> Fut,
        Fut: Future<Output = Result<Page<T>, E>>,
    {
        base_query: ODataQuery,
        next_cursor: Option<String>,
        buffer: VecDeque<T>,
        done: bool,
        fetcher: F,
        #[pin]
        current_fetch: Option<Fut>,
    }
}

impl<T, E, F, Fut> CursorPager<T, E, F, Fut>
where
    F: FnMut(ODataQuery) -> Fut,
    Fut: Future<Output = Result<Page<T>, E>>,
{
    /// Create a new cursor pager with the given base query and fetcher function.
    ///
    /// # Arguments
    ///
    /// * `base_query` - The base `OData` query (without cursor)
    /// * `fetcher` - Function that fetches a page given an `ODataQuery`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let pager = CursorPager::new(query, |q| async move {
    ///     client.list_users(q).await
    /// });
    /// ```
    pub fn new(base_query: ODataQuery, fetcher: F) -> Self {
        Self {
            base_query,
            next_cursor: None,
            buffer: VecDeque::new(),
            done: false,
            fetcher,
            current_fetch: None,
        }
    }
}

impl<T, E, F, Fut> Stream for CursorPager<T, E, F, Fut>
where
    F: FnMut(ODataQuery) -> Fut,
    Fut: Future<Output = Result<Page<T>, E>>,
{
    type Item = Result<T, PagerError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            if let Some(item) = this.buffer.pop_front() {
                return Poll::Ready(Some(Ok(item)));
            }

            if *this.done {
                return Poll::Ready(None);
            }

            if let Some(fut) = this.current_fetch.as_mut().as_pin_mut() {
                match fut.poll(cx) {
                    Poll::Ready(Ok(page)) => {
                        this.current_fetch.set(None);

                        this.next_cursor.clone_from(&page.page_info.next_cursor);

                        if this.next_cursor.is_none() {
                            *this.done = true;
                        }

                        this.buffer.extend(page.items);

                        continue;
                    }
                    Poll::Ready(Err(e)) => {
                        this.current_fetch.set(None);
                        *this.done = true;
                        return Poll::Ready(Some(Err(PagerError::Fetch(e))));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Allocation strategy: base_query cloned once per page fetch.
            // Filter AST is built once in QueryBuilder and reused here.
            let mut query = this.base_query.clone();
            if let Some(cursor_str) = this.next_cursor.as_ref() {
                if let Ok(cursor) = modkit_odata::CursorV1::decode(cursor_str) {
                    query = query.with_cursor(cursor);
                } else {
                    *this.done = true;
                    return Poll::Ready(Some(Err(PagerError::InvalidCursor(cursor_str.clone()))));
                }
            }

            let fut = (this.fetcher)(query);
            this.current_fetch.set(Some(fut));
        }
    }
}

pin_project! {
    /// A cursor-based pager that implements `Stream` for pages.
    ///
    /// This pager yields entire pages instead of individual items.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The item type
    /// * `E` - The error type
    /// * `F` - The fetcher function type
    /// * `Fut` - The future returned by the fetcher
    pub struct PagesPager<T, E, F, Fut>
    where
        F: FnMut(ODataQuery) -> Fut,
        Fut: Future<Output = Result<Page<T>, E>>,
    {
        base_query: ODataQuery,
        next_cursor: Option<String>,
        done: bool,
        fetcher: F,
        #[pin]
        current_fetch: Option<Fut>,
    }
}

impl<T, E, F, Fut> PagesPager<T, E, F, Fut>
where
    F: FnMut(ODataQuery) -> Fut,
    Fut: Future<Output = Result<Page<T>, E>>,
{
    /// Create a new pages pager with the given base query and fetcher function.
    ///
    /// # Arguments
    ///
    /// * `base_query` - The base `OData` query (without cursor)
    /// * `fetcher` - Function that fetches a page given an `ODataQuery`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let pager = PagesPager::new(query, |q| async move {
    ///     client.list_users(q).await
    /// });
    /// ```
    pub fn new(base_query: ODataQuery, fetcher: F) -> Self {
        Self {
            base_query,
            next_cursor: None,
            done: false,
            fetcher,
            current_fetch: None,
        }
    }
}

impl<T, E, F, Fut> Stream for PagesPager<T, E, F, Fut>
where
    F: FnMut(ODataQuery) -> Fut,
    Fut: Future<Output = Result<Page<T>, E>>,
{
    type Item = Result<Page<T>, PagerError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            if *this.done {
                return Poll::Ready(None);
            }

            if let Some(fut) = this.current_fetch.as_mut().as_pin_mut() {
                match fut.poll(cx) {
                    Poll::Ready(Ok(page)) => {
                        this.current_fetch.set(None);

                        this.next_cursor.clone_from(&page.page_info.next_cursor);

                        if this.next_cursor.is_none() {
                            *this.done = true;
                        }

                        return Poll::Ready(Some(Ok(page)));
                    }
                    Poll::Ready(Err(e)) => {
                        this.current_fetch.set(None);
                        *this.done = true;
                        return Poll::Ready(Some(Err(PagerError::Fetch(e))));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Allocation strategy: base_query cloned once per page fetch.
            // Filter AST is built once in QueryBuilder and reused here.
            let mut query = this.base_query.clone();
            if let Some(cursor_str) = this.next_cursor.as_ref() {
                if let Ok(cursor) = modkit_odata::CursorV1::decode(cursor_str) {
                    query = query.with_cursor(cursor);
                } else {
                    *this.done = true;
                    return Poll::Ready(Some(Err(PagerError::InvalidCursor(cursor_str.clone()))));
                }
            }

            let fut = (this.fetcher)(query);
            this.current_fetch.set(Some(fut));

            // Poll the newly-installed future immediately so it can register the current waker
            // naturally, avoiding a manual `wake_by_ref()` and the associated spurious wakeup.
        }
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)]
#[path = "pager_tests.rs"]
mod tests;
