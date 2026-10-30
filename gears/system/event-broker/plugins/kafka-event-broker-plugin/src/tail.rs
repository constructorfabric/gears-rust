//! An in-memory copy of one partition of a compacted topic, kept by reading
//! it forward (`docs/DESIGN.md` §3.3): the logs topic and each fence-topic
//! partition.

use std::collections::HashMap;
use std::sync::Arc;

use event_broker_sdk::backend::BackendError;

use crate::clients::{self, Span};
use crate::error::{self, Operation};
use crate::shared::Shared;

/// The latest value per key of one partition, up to `position`.
#[derive(Debug, Default)]
pub struct Tail {
    /// The next offset to read.
    position: i64,
    entries: HashMap<Vec<u8>, Vec<u8>>,
}

/// Records one catch-up step reads at most.
const STEP_RECORDS: usize = 10_000;

impl Tail {
    /// The latest value of `key` seen so far.
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.entries.get(key).map(Vec::as_slice)
    }

    /// A value this instance itself just committed.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.entries.insert(key, value);
    }

    /// Reads forward until every record below the partition's high
    /// watermark - as of this call, open transactions included - is
    /// decided and applied. Called after `init_transactions` of the
    /// transactional id that writes a key, it sees every value of that key
    /// any earlier producer committed.
    pub async fn catch_up(
        &mut self,
        shared: &Shared,
        topic: &str,
        partition: i32,
        operation: Operation,
    ) -> Result<(), BackendError> {
        let timeout = shared.settings.request_timeout;
        let (_, high) = clients::watermarks(
            shared.watermark_consumer(operation)?,
            topic.to_owned(),
            partition,
            timeout,
        )
        .await
        .map_err(|e| error::map(operation, &e))?;
        while self.position < high {
            let lease = shared.lease(operation).await?;
            let consumed = clients::consume(
                lease.consumer(),
                Span {
                    topic,
                    partition,
                    start: self.position,
                    end: high,
                    max: STEP_RECORDS,
                    timeout,
                    eof_ends: false,
                },
            )
            .await;
            let consumed = match consumed {
                Ok(consumed) => consumed,
                Err(err) => {
                    lease.discard();
                    return Err(error::map(operation, &err));
                }
            };
            drop(lease);
            if !consumed.complete {
                return Err(error::timed_out(operation, "reading a compacted topic"));
            }
            let full = consumed.records.len() >= STEP_RECORDS;
            for record in consumed.records {
                self.position = record.offset + 1;
                let Some(key) = record.key else { continue };
                match record.value {
                    Some(value) => self.entries.insert(key, value),
                    None => self.entries.remove(&key),
                };
            }
            if !full {
                self.position = high;
            }
        }
        Ok(())
    }
}

/// A [`Tail`] shared by the instance's tasks.
pub type SharedTail = Arc<tokio::sync::Mutex<Tail>>;
