//! The head watcher against a scripted source: which watches it opens, what it
//! records from them, and how it reopens one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;
use toolkit_gts::GtsInstanceId;

use crate::domain::model::Sequence;
use crate::domain::streaming::source::PartitionKey;

use super::heads::{HeadSource, HeadStream, HeadWatcher, PartitionHead};
use super::source::SourceError;
use super::topics::{TopicManager, TopicPolicy};

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1";
const RECONCILE: Duration = Duration::from_millis(50);

type Item = Result<PartitionHead, SourceError>;

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new(TOPIC).expect("static gts id is valid")
}

fn key(partition: i32) -> PartitionKey {
    PartitionKey::new(topic(), partition)
}

fn head(partition: i32, head: i64) -> PartitionHead {
    PartitionHead {
        key: key(partition),
        head: Sequence::assigned(head),
    }
}

/// Records every watch opened and when, and serves each from the next scripted
/// open: a channel, or a failure to open; a watch with nothing left scripted
/// never yields.
#[derive(Default)]
struct ScriptedSource {
    opened: Mutex<Vec<(GtsInstanceId, Vec<i32>)>>,
    opened_at: Mutex<Vec<Instant>>,
    streams: Mutex<VecDeque<Result<mpsc::UnboundedReceiver<Item>, SourceError>>>,
}

impl ScriptedSource {
    /// The sender feeding the next watch opened.
    fn next_watch(&self) -> mpsc::UnboundedSender<Item> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(Ok(rx));
        tx
    }

    /// The next `count` watches fail to open.
    fn fail_opens(&self, count: usize) {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..count {
            streams.push_back(Err(SourceError::Unavailable));
        }
    }

    /// The time between each open and the next.
    fn gaps(&self) -> Vec<Duration> {
        let at = self
            .opened_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        at.windows(2).map(|pair| pair[1] - pair[0]).collect()
    }

    fn opened_at(&self) -> Vec<Instant> {
        self.opened_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn opened(&self) -> Vec<(GtsInstanceId, Vec<i32>)> {
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl HeadSource for ScriptedSource {
    async fn watch(
        &self,
        topic: &GtsInstanceId,
        partitions: &[i32],
    ) -> Result<HeadStream, SourceError> {
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((topic.clone(), partitions.to_vec()));
        self.opened_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Instant::now());
        let next = self
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        match next {
            Some(Ok(rx)) => Ok(Box::pin(UnboundedReceiverStream::new(rx))),
            Some(Err(error)) => Err(error),
            None => Ok(Box::pin(tokio_stream::pending())),
        }
    }
}

async fn settle() {
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
}

fn start(source: &Arc<ScriptedSource>, topics: &Arc<TopicManager>) -> CancellationToken {
    let stop = CancellationToken::new();
    tokio::spawn(
        HeadWatcher::new(Arc::clone(source), Arc::clone(topics), RECONCILE).run(stop.clone()),
    );
    stop
}

#[tokio::test(start_paused = true)]
async fn a_topic_is_watched_over_its_live_partitions_and_its_heads_recorded() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let p0 = topics.attach(&key(0));
    let p1 = topics.attach(&key(1));
    let source = Arc::new(ScriptedSource::default());
    let heads = source.next_watch();
    let stop = start(&source, &topics);

    settle().await;
    heads.send(Ok(head(1, 5))).unwrap();
    heads.send(Ok(head(1, 3))).unwrap();
    settle().await;

    assert_eq!(source.opened(), vec![(topic(), vec![0, 1])]);
    assert_eq!(
        (p0.known_head(), p1.known_head()),
        (None, Some(Sequence::assigned(5))),
        "a head below the known one does not move it back"
    );
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_failed_watch_is_reopened_after_its_backoff() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    let first = source.next_watch();
    let second = source.next_watch();
    let stop = start(&source, &topics);

    settle().await;
    first.send(Err(SourceError::Unavailable)).unwrap();
    settle().await;
    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0])],
        "not before the backoff"
    );

    tokio::time::advance(Duration::from_millis(250)).await;
    settle().await;
    second.send(Ok(head(0, 9))).unwrap();
    settle().await;

    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0]), (topic(), vec![0])]
    );
    assert_eq!(
        topics.get(&key(0)).and_then(|p| p.known_head()),
        Some(Sequence::assigned(9))
    );
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_partition_going_live_reopens_the_watch_over_the_new_set() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    let stop = start(&source, &topics);

    settle().await;
    let _p2 = topics.attach(&key(2));
    tokio::time::advance(RECONCILE).await;
    settle().await;

    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0]), (topic(), vec![0, 2])]
    );
    stop.cancel();
}

/// Each gap that falls outside the jittered window `[backoff / 2, backoff]` of
/// the backoff expected before it, as `(gap index, gap, backoff)`.
fn outside_backoff(gaps: &[Duration], backoffs: &[Duration]) -> Vec<(usize, Duration, Duration)> {
    gaps.iter()
        .zip(backoffs)
        .enumerate()
        .filter(|(_, (gap, backoff))| **gap < **backoff / 2 || **gap > **backoff)
        .map(|(index, (gap, backoff))| (index, *gap, *backoff))
        .collect()
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

#[tokio::test(start_paused = true)]
async fn a_watch_that_fails_after_its_snapshot_heads_backs_off() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let _p1 = topics.attach(&key(1));
    let source = Arc::new(ScriptedSource::default());
    let first = source.next_watch();
    let second = source.next_watch();
    let _third = source.next_watch();
    let stop = start(&source, &topics);

    settle().await;
    first.send(Ok(head(0, 5))).unwrap();
    first.send(Ok(head(1, 7))).unwrap();
    first.send(Err(SourceError::Unavailable)).unwrap();
    settle().await;
    tokio::time::advance(ms(250)).await;
    settle().await;
    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0, 1]), (topic(), vec![0, 1])]
    );

    second.send(Ok(head(0, 5))).unwrap();
    second.send(Ok(head(1, 7))).unwrap();
    second.send(Err(SourceError::Unavailable)).unwrap();
    settle().await;
    tokio::time::advance(ms(250)).await;
    settle().await;
    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0, 1]), (topic(), vec![0, 1])],
        "first heads of two partitions are no proof of health: the backoff grew to 500 ms"
    );

    tokio::time::advance(ms(250)).await;
    settle().await;
    assert_eq!(
        source.opened(),
        vec![
            (topic(), vec![0, 1]),
            (topic(), vec![0, 1]),
            (topic(), vec![0, 1])
        ]
    );
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_head_below_the_first_does_not_count_as_a_change() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    let first = source.next_watch();
    let second = source.next_watch();
    let _third = source.next_watch();
    let stop = start(&source, &topics);

    settle().await;
    for head_seq in [5, 3, 4] {
        first.send(Ok(head(0, head_seq))).unwrap();
    }
    first.send(Err(SourceError::Unavailable)).unwrap();
    settle().await;
    tokio::time::advance(ms(250)).await;
    settle().await;
    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0]), (topic(), vec![0])]
    );

    for head_seq in [5, 3, 4] {
        second.send(Ok(head(0, head_seq))).unwrap();
    }
    second.send(Err(SourceError::Unavailable)).unwrap();
    settle().await;
    tokio::time::advance(ms(250)).await;
    settle().await;
    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0]), (topic(), vec![0])],
        "a head that rises again but stays below the first is no proof of health: \
         the backoff grew to 500 ms"
    );

    tokio::time::advance(ms(250)).await;
    settle().await;
    assert_eq!(
        source.opened(),
        vec![(topic(), vec![0]), (topic(), vec![0]), (topic(), vec![0])]
    );
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn consecutive_failures_double_the_backoff_up_to_the_ceiling() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    source.fail_opens(10);
    let stop = start(&source, &topics);

    tokio::time::sleep(Duration::from_secs(200)).await;

    assert_eq!(source.opened(), vec![(topic(), vec![0]); 11]);
    let backoffs = [
        ms(250),
        ms(500),
        ms(1_000),
        ms(2_000),
        ms(4_000),
        ms(8_000),
        ms(16_000),
        ms(30_000),
        ms(30_000),
        ms(30_000),
    ];
    let gaps = source.gaps();
    assert_eq!(gaps.len(), backoffs.len(), "gaps: {gaps:?}");
    assert_eq!(outside_backoff(&gaps, &backoffs), vec![], "gaps: {gaps:?}");
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_head_after_the_snapshot_resets_the_backoff() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    source.fail_opens(3);
    let healthy = source.next_watch();
    healthy.send(Ok(head(0, 5))).unwrap();
    healthy.send(Ok(head(0, 6))).unwrap();
    healthy.send(Err(SourceError::Unavailable)).unwrap();
    let stop = start(&source, &topics);

    tokio::time::sleep(Duration::from_secs(10)).await;

    assert_eq!(source.opened(), vec![(topic(), vec![0]); 5]);
    let gaps = source.gaps();
    assert_eq!(gaps.len(), 4, "gaps: {gaps:?}");
    assert_eq!(
        outside_backoff(&gaps, &[ms(250), ms(500), ms(1_000), ms(250)]),
        vec![],
        "gaps: {gaps:?}"
    );
    assert_eq!(
        topics.get(&key(0)).and_then(|p| p.known_head()),
        Some(Sequence::assigned(6))
    );
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_watch_open_for_the_ceiling_resets_the_backoff() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    source.fail_opens(3);
    let long_lived = source.next_watch();
    long_lived.send(Ok(head(0, 5))).unwrap();
    let stop = start(&source, &topics);

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(source.opened(), vec![(topic(), vec![0]); 4]);
    let gaps = source.gaps();
    assert_eq!(
        outside_backoff(&gaps, &[ms(250), ms(500), ms(1_000)]),
        vec![],
        "gaps: {gaps:?}"
    );

    tokio::time::sleep(Duration::from_secs(30)).await;
    let failed_at = Instant::now();
    long_lived.send(Err(SourceError::Unavailable)).unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;

    assert_eq!(source.opened(), vec![(topic(), vec![0]); 5]);
    let reopened_after = source.opened_at()[4] - failed_at;
    assert!(
        (ms(125)..=ms(250)).contains(&reopened_after),
        "reopened {reopened_after:?} after the failure"
    );
    stop.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_re_sent_equal_head_does_not_reset_the_backoff() {
    let topics = Arc::new(TopicManager::new(TopicPolicy::default()));
    let _p0 = topics.attach(&key(0));
    let source = Arc::new(ScriptedSource::default());
    source.fail_opens(2);
    let repeating = source.next_watch();
    repeating.send(Ok(head(0, 5))).unwrap();
    repeating.send(Ok(head(0, 5))).unwrap();
    repeating.send(Err(SourceError::Unavailable)).unwrap();
    let stop = start(&source, &topics);

    tokio::time::sleep(Duration::from_secs(10)).await;

    assert_eq!(source.opened(), vec![(topic(), vec![0]); 4]);
    let gaps = source.gaps();
    assert_eq!(gaps.len(), 3, "gaps: {gaps:?}");
    assert!(
        gaps[2] > ms(250),
        "reopened within the floor window: gaps: {gaps:?}"
    );
    assert_eq!(
        outside_backoff(&gaps, &[ms(250), ms(500), ms(1_000)]),
        vec![],
        "an equal head is no change: gaps: {gaps:?}"
    );
    stop.cancel();
}
