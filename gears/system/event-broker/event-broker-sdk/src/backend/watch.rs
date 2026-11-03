//! How head changes are observed (feature 0007 §2.5).

use std::time::Duration;

use futures_util::stream::BoxStream;

use super::append::Head;
use super::error::BackendError;
use super::ids::TopicPartition;

pub enum Watch<'a> {
    /// The medium pushes head changes.
    Native(&'a dyn HeadWatch),
    /// The gear polls `resolve(Latest)` for each watched partition at this
    /// interval ([`support::poll_heads`](super::support::poll_heads)).
    Poll(Duration),
}

pub trait HeadWatch: Send + Sync {
    /// First the current head of each named partition, then every change.
    /// May coalesce: only the latest head per partition matters. Ends when
    /// dropped. Errors arrive as stream items; the subscriber resubscribes
    /// with backoff.
    fn watch(
        &self,
        partitions: &[TopicPartition],
    ) -> BoxStream<'static, Result<Head, BackendError>>;
}
