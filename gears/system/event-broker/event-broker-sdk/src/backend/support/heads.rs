//! Heads a backend instance has learned, shared with its readers and watchers
//! in the same process.
//!
//! A backend that assigns sequences itself publishes each head as its append
//! commits, and one whose medium pushes changes publishes what it hears. A
//! tail read waits here rather than sleeping blind, and a native watch is a
//! filtered view of it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::backend::{BackendError, Head, TopicPartition};
use crate::models::Event;
use crate::sequence::Sequence;

const CHANNEL_CAPACITY: usize = 4096;

type Latest = Arc<Mutex<HashMap<TopicPartition, Sequence>>>;

pub struct LocalHeads {
    latest: Latest,
    changes: broadcast::Sender<Head>,
}

impl Default for LocalHeads {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalHeads {
    #[must_use]
    pub fn new() -> Self {
        let (changes, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            latest: Arc::default(),
            changes,
        }
    }

    /// Records `head` if it is above what is known for its partition, and
    /// tells waiters and watchers. A head at or below the known one is
    /// dropped: heads only move forward.
    pub fn publish(&self, head: Head) {
        {
            let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            match latest.get_mut(&head.at) {
                Some(known) if *known >= head.sequence => return,
                Some(known) => *known = head.sequence,
                None => {
                    latest.insert(head.at.clone(), head.sequence);
                }
            }
        }
        // No receiver is not an error: nobody is waiting.
        let _ = self.changes.send(head);
    }

    /// The highest head published for `at`, if any.
    #[must_use]
    pub fn known(&self, at: &TopicPartition) -> Option<Sequence> {
        known(&self.latest, at)
    }

    /// A watch over `partitions`: first what `current` resolves to (the
    /// medium's current head of each partition), then every head published
    /// above what was already yielded.
    ///
    /// Subscribes before `current` runs, so a head published while it is read
    /// is not lost. A receiver that falls behind skips to the latest known
    /// head of each partition: only the latest matters. An error from
    /// `current` is yielded and ends the stream.
    pub fn watch<F>(
        &self,
        partitions: &[TopicPartition],
        current: F,
    ) -> BoxStream<'static, Result<Head, BackendError>>
    where
        F: Future<Output = Result<Vec<Head>, BackendError>> + Send + 'static,
    {
        let state = WatchState {
            rx: self.changes.subscribe(),
            latest: Arc::clone(&self.latest),
            wanted: partitions.iter().cloned().collect(),
            emitted: HashMap::new(),
            pending: VecDeque::new(),
            current: Some(Box::pin(current)),
            ended: false,
        };
        stream::unfold(state, WatchState::next).boxed()
    }

    /// Resolves once a head above `offset` is known for `at`, or after
    /// `timeout`; `true` when one is known.
    pub async fn wait_above(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        timeout: Duration,
    ) -> bool {
        let mut rx = self.changes.subscribe();
        if self.known(at).is_some_and(|head| head > offset) {
            return true;
        }
        let deadline = Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Err(_) | Ok(Err(broadcast::error::RecvError::Closed)) => return false,
                Ok(Ok(head)) if head.at == *at && head.sequence > offset => return true,
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    if self.known(at).is_some_and(|head| head > offset) {
                        return true;
                    }
                }
            }
        }
    }

    /// A waiting read: runs `probe` until it returns events or `wait` has
    /// passed, waking on a published head and, for writers this process does
    /// not hear, every `poll`.
    ///
    /// An empty probe waits for a head above the one known before it ran,
    /// not merely above `offset`: a head can stand above every readable
    /// event (retention removed the tail), and waiting on it would re-probe
    /// without pause.
    ///
    /// # Errors
    /// Whatever `probe` fails with.
    pub async fn read_waiting<F, Fut>(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        wait: Duration,
        poll: Duration,
        mut probe: F,
    ) -> Result<Vec<Event>, BackendError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Vec<Event>, BackendError>>,
    {
        let deadline = Instant::now() + wait;
        loop {
            // Read before the probe, so a head published while it runs still
            // counts as new and wakes the next probe at once.
            let seen = self.known(at).map_or(offset, |head| head.max(offset));
            let events = probe().await?;
            let now = Instant::now();
            if !events.is_empty() || now >= deadline {
                return Ok(events);
            }
            self.wait_above(at, seen, poll.min(deadline - now)).await;
        }
    }
}

fn known(latest: &Latest, at: &TopicPartition) -> Option<Sequence> {
    latest
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(at)
        .copied()
}

struct WatchState {
    rx: broadcast::Receiver<Head>,
    latest: Latest,
    wanted: HashSet<TopicPartition>,
    emitted: HashMap<TopicPartition, Sequence>,
    pending: VecDeque<Result<Head, BackendError>>,
    current: Option<BoxFuture<'static, Result<Vec<Head>, BackendError>>>,
    ended: bool,
}

impl WatchState {
    async fn next(mut self) -> Option<(Result<Head, BackendError>, Self)> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some((item, self));
            }
            if self.ended {
                return None;
            }
            if let Some(current) = self.current.take() {
                match current.await {
                    Ok(heads) => {
                        for head in heads {
                            self.offer(head);
                        }
                    }
                    Err(err) => {
                        self.pending.push_back(Err(err));
                        self.ended = true;
                    }
                }
                continue;
            }
            match self.rx.recv().await {
                Ok(head) => self.offer(head),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let heads: Vec<Head> = self
                        .wanted
                        .iter()
                        .filter_map(|at| {
                            known(&self.latest, at).map(|sequence| Head {
                                at: at.clone(),
                                sequence,
                            })
                        })
                        .collect();
                    for head in heads {
                        self.offer(head);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Queues `head` when it names a watched partition above what this watch
    /// already yielded for it.
    fn offer(&mut self, head: Head) {
        if !self.wanted.contains(&head.at) {
            return;
        }
        if self
            .emitted
            .get(&head.at)
            .is_some_and(|seen| *seen >= head.sequence)
        {
            return;
        }
        self.emitted.insert(head.at.clone(), head.sequence);
        self.pending.push_back(Ok(head));
    }
}
