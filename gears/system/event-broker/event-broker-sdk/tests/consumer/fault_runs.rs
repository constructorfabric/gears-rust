//! The fault runs the plain (`recovery`) and the transactional (`transactional`)
//! re-join tests share: a consumer on a `SeekFaultBroker` whose first stream fails,
//! `FIRST` published to partition 0 and `SECOND` to partition 1, and what the
//! consumer did by its stop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use event_broker_sdk::consumer::ConsumerHandle;
use event_broker_sdk::{
    Ack, ConsumerError, ConsumerGroupId, ConsumerGroupQuery, ConsumerRuntimeEvent, EventBrokerApi,
    SubscriptionId,
};
use uuid::Uuid;

use super::common::{TopicFixture, two_partition_event};
use super::doubles::{RecordingRuntimeListener, SeekFaultBroker};

/// Bounds each await of a fault run: 5 s against healthy runs of about 1.3 s, and far
/// below the 60 s default `session_timeout`, the longest a member left behind by a
/// re-join keeps its share once its stream has closed.
const BOUND: Duration = Duration::from_secs(5);

pub(super) const FIRST: Uuid = Uuid::from_u128(0x1701);
pub(super) const SECOND: Uuid = Uuid::from_u128(0x1702);

/// `(id, partition, offset)` of every event a fault run's handler was handed.
pub(super) type SharedPlacements = Arc<Mutex<Vec<(Uuid, u32, i64)>>>;

/// The consumer a fault run watches.
pub(super) trait FaultRunConsumer {
    /// Starts it on `broker`, reporting to `listener`, with a handler that records
    /// each event it is handed into `handled` and lets the consumer go on streaming.
    async fn start(
        self,
        broker: Arc<dyn EventBrokerApi>,
        listener: RecordingRuntimeListener,
        handled: SharedPlacements,
    ) -> ConsumerHandle;
}

/// When a fault run publishes its two events.
enum PublishAt {
    /// Once the consumer re-joins after its first stream broke off.
    TheRejoin,
    /// As soon as the consumer has started.
    TheStart,
}

/// What a consumer did around a fault on its first stream.
pub(super) struct FaultRun {
    /// `(id, partition, offset)` of every event handled, by partition.
    pub(super) placements: Vec<(Uuid, u32, i64)>,
    runtime_events: Vec<ConsumerRuntimeEvent>,
    /// The subscriptions left before the consumer was stopped.
    pub(super) leave_calls: Vec<SubscriptionId>,
    /// The one consumer group the broker holds by the stop: the one it created for
    /// the consumer's alias.
    pub(super) group: ConsumerGroupId,
}

impl FaultRun {
    /// Starts `consumer` on `faulty`, whose first stream breaks off, waits for the
    /// re-join, publishes the two `event_type` events and records what the handler
    /// was handed within `BOUND` of that, the runtime events, the subscriptions left
    /// before the stop (whose own leave is not counted), and the one consumer group
    /// the broker holds by then; a run fails if it holds any other number.
    pub(super) async fn after_a_first_stream_fault(
        fx: &TopicFixture,
        event_type: &str,
        faulty: SeekFaultBroker,
        consumer: impl FaultRunConsumer,
    ) -> Self {
        let (run, stopped) =
            Self::record(fx, event_type, faulty, consumer, PublishAt::TheRejoin).await;
        stopped.expect("consumer stops");
        run
    }

    /// Starts `consumer` on `faulty`, whose first stream open is refused, publishes
    /// the two `event_type` events at once and records what the handler was handed
    /// within `BOUND` of that, with the outcome of the stop. It waits for the
    /// deliveries, not for a re-join, so a run that ends on the refusal fails on the
    /// caller's assert.
    pub(super) async fn after_a_refused_stream_open(
        fx: &TopicFixture,
        event_type: &str,
        faulty: SeekFaultBroker,
        consumer: impl FaultRunConsumer,
    ) -> (Self, Result<(), ConsumerError>) {
        Self::record(fx, event_type, faulty, consumer, PublishAt::TheStart).await
    }

    async fn record(
        fx: &TopicFixture,
        event_type: &str,
        faulty: SeekFaultBroker,
        consumer: impl FaultRunConsumer,
        publish_at: PublishAt,
    ) -> (Self, Result<(), ConsumerError>) {
        let leave_calls = faulty.leave_calls.clone();
        let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);
        let listener = RecordingRuntimeListener::default();
        let runtime_events = listener.events.clone();
        let handled = SharedPlacements::default();

        let handle = consumer.start(broker, listener, handled.clone()).await;

        if let PublishAt::TheRejoin = publish_at {
            let rejoining = || {
                runtime_events.lock().unwrap().iter().any(|event| {
                    matches!(event, ConsumerRuntimeEvent::SubscriptionRejoining { .. })
                })
            };
            tokio::time::timeout(BOUND, async {
                while !rejoining() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the consumer re-joins after the fault");
        }

        for (id, subject, partition) in [(FIRST, "first", 0), (SECOND, "second", 1)] {
            let event = two_partition_event(&fx.ctx, event_type, id, subject, partition);
            fx.broker
                .publish(&fx.ctx, &event, Ack::Accepted)
                .await
                .expect("event published");
        }
        let both_handled = || handled.lock().unwrap().len() >= 2;
        // A stall shows as a missing event in the caller's assert, not as a panic here.
        tokio::time::timeout(BOUND, async {
            while !both_handled() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .ok();
        let left_before_stop = leave_calls.lock().unwrap().clone();
        let stopped = tokio::time::timeout(BOUND, handle.stop())
            .await
            .expect("the consumer stops within the bound");

        let mut handled = handled.lock().unwrap().clone();
        handled.sort_by_key(|&(_, partition, _)| partition);
        let runtime_events = runtime_events.lock().unwrap().clone();
        let groups = fx
            .broker
            .list_consumer_groups(&fx.ctx, ConsumerGroupQuery::default())
            .await
            .expect("consumer groups listed");
        let [group] = groups.items.as_slice() else {
            panic!("expected the consumer's one group, saw {:?}", groups.items);
        };
        let run = Self {
            placements: handled,
            runtime_events,
            leave_calls: left_before_stop,
            group: group.id,
        };
        (run, stopped)
    }

    /// The group and the replaced subscription the one re-join reported.
    pub(super) fn rejoin(&self) -> (ConsumerGroupId, SubscriptionId) {
        let rejoins: Vec<(ConsumerGroupId, SubscriptionId)> = self
            .runtime_events
            .iter()
            .filter_map(|event| match event {
                ConsumerRuntimeEvent::SubscriptionRejoining {
                    consumer_group,
                    previous_subscription,
                } => Some((*consumer_group, *previous_subscription)),
                _ => None,
            })
            .collect();
        let [rejoin] = rejoins.as_slice() else {
            panic!("expected one re-join, saw {rejoins:?}");
        };
        *rejoin
    }
}
