//! The head watcher against a scripted source: which watches it opens, what it
//! records from them, and how it reopens one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
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

/// Records every watch opened, and serves each from the next scripted channel;
/// a watch with no channel left never yields.
#[derive(Default)]
struct ScriptedSource {
    opened: Mutex<Vec<(GtsInstanceId, Vec<i32>)>>,
    streams: Mutex<VecDeque<mpsc::UnboundedReceiver<Item>>>,
}

impl ScriptedSource {
    /// The sender feeding the next watch opened.
    fn next_watch(&self) -> mpsc::UnboundedSender<Item> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(rx);
        tx
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
        let next = self
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        Ok(match next {
            Some(rx) => Box::pin(UnboundedReceiverStream::new(rx)),
            None => Box::pin(tokio_stream::pending()),
        })
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
