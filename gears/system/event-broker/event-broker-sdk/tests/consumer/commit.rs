//! Auto-commit metadata against the real broker: the offset the runtime commits
//! carries the resolved group/topic/partition and a real frontier offset.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::{
    BatchHandlerOutcome, CommitOffset, ConsumerBuilder, ConsumerCommitMode, ConsumerGroupId,
    ConsumerGroupRef, OffsetManagerError, OffsetStore, Position, Sequence, TopicId, gts_id,
};
use serde_json::json;
use uuid::Uuid;

use super::common::{publish_json, topic, topic_fixture};
use super::doubles::{RecordingBatchHandler, SharedCommits};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.autocommit.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.autocommit.v1~");

/// A custom offset store that records every commit's full coordinates - the
/// `.topics([..]) + custom OffsetStore` shape the delivery fix confirmed works.
#[derive(Clone, Default)]
struct RecordingOffsetManager {
    commits: SharedCommits,
}

#[async_trait]
impl OffsetStore for RecordingOffsetManager {
    async fn load_position(
        &self,
        _group: &ConsumerGroupId,
        _topic: &TopicId,
        _partition: u32,
    ) -> Result<Position, OffsetManagerError> {
        Ok(Position::Earliest)
    }
}

#[async_trait]
impl CommitOffset for RecordingOffsetManager {
    async fn commit(
        &self,
        group: &ConsumerGroupId,
        topic: &TopicId,
        partition: u32,
        offset: Sequence,
    ) -> Result<(), OffsetManagerError> {
        self.commits.lock().expect("recording commits").push((
            *group,
            *topic,
            partition,
            offset.as_i64(),
        ));
        Ok(())
    }
}

async fn wait_for_first_non_negative_commit(
    commits: &SharedCommits,
) -> (ConsumerGroupId, TopicId, u32, i64) {
    for _ in 0..300 {
        if let Some(commit) = commits
            .lock()
            .expect("recording commits")
            .iter()
            .copied()
            .find(|commit| commit.3 >= 0)
        {
            return commit;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("auto-commit did not persist a recorded event offset");
}

#[tokio::test]
async fn async_auto_commit_uses_resolved_group_topic_partition_and_frontier_offset() {
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let offset_manager = RecordingOffsetManager::default();
    let commits = offset_manager.commits.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("auto-commit-metadata"))
        .topics([topic(TOPIC)])
        .commit_mode(ConsumerCommitMode::auto(Duration::from_millis(5)))
        .offset_manager(offset_manager)
        .batch_handler(RecordingBatchHandler {
            name: "default",
            calls,
            outcome: BatchHandlerOutcome::Success,
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_json(
        &fx.broker,
        &fx.ctx,
        EVENT_TYPE,
        "subject-1",
        None,
        json!({ "ok": true }),
    )
    .await;

    let commit = wait_for_first_non_negative_commit(&commits).await;
    handle.stop().await.expect("consumer stops");

    assert_ne!(commit.0, ConsumerGroupId::new(Uuid::nil()));
    assert_eq!(commit.1, TopicId::from_gts(TOPIC));
    assert_eq!(commit.2, 0);
    assert!(commit.3 >= 0);
}
