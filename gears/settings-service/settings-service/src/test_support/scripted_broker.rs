//! The scripted broker the consumer-side tests share: the activation client's
//! and the cache-invalidation consumer's. It records what the consumer does to
//! its consumer group, hands out one scripted stream per join, and refuses,
//! hangs or ends on demand; everything the consumers never call is refused.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use chrono::Utc;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    Ack, AssignedPartition, ConsumerGroup, ConsumerGroupId, ConsumerGroupKind, ConsumerGroupQuery,
    CreateConsumerGroupRequest, EventBrokerApi, EventBrokerError, EventV1, FrameStream,
    GtsInstanceId, IngestOutcome, JoinRequest, Page, PartitionRange, ProducerCursors, ProducerId,
    ProducerMode, ResetScope, SeekPosition, SeekResult, Subscription, SubscriptionAssignment,
    SubscriptionId, Topic, TopicSegment, WireFrame,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::event_broker_stubs::event_broker_impl;

/// What one scripted stream carries.
pub type FrameResult = Result<WireFrame, EventBrokerError>;

/// The tenant every created group is owned by.
const GROUP_TENANT: Uuid = Uuid::from_u128(0xa0);

/// A broker that records what the consumer does to its groups and hands out one
/// scripted stream per join.
pub struct ScriptedBroker {
    /// The topic every join is assigned a partition of.
    topic: &'static str,
    /// A created group's client agent must start with this; empty accepts any.
    agent_prefix: &'static str,
    /// Creations still to refuse.
    pub create_refusals: AtomicUsize,
    /// Joins still to refuse.
    pub join_refusals: AtomicUsize,
    /// Refusals are for a missing grant when set, an internal error otherwise.
    pub refuse_as_grant: AtomicBool,
    /// Every deletion is refused.
    pub delete_fails: AtomicBool,
    /// `create_consumer_group` never answers.
    pub create_hangs: AtomicBool,
    /// `join` never answers.
    pub join_hangs: AtomicBool,
    /// `leave` never answers.
    pub leave_hangs: AtomicBool,
    /// Every stream ends as soon as it is opened.
    pub streams_end_at_once: AtomicBool,
    /// When each creation was attempted.
    pub create_calls: Mutex<Vec<Instant>>,
    /// The groups created.
    pub creates: Mutex<Vec<ConsumerGroupId>>,
    /// The client agent of each created group.
    pub create_agents: Mutex<Vec<String>>,
    /// The joins made.
    pub joins: Mutex<Vec<JoinRequest>>,
    /// The home tenant of the identity each `join` was made under.
    pub join_homes: Mutex<Vec<Uuid>>,
    /// The positions each seek asked for.
    pub seeks: Mutex<Vec<Vec<SeekPosition>>>,
    /// The subscriptions left.
    pub leaves: Mutex<Vec<SubscriptionId>>,
    /// The groups deleted.
    pub deletes: Mutex<Vec<ConsumerGroupId>>,
    /// The sending side of each opened stream.
    pub streams: Mutex<Vec<UnboundedSender<FrameResult>>>,
    /// The receiving side of the stream the last join prepared.
    pub next_stream: Mutex<Option<UnboundedReceiver<FrameResult>>>,
}

/// The refusal a scripted call answers with: a missing grant or an internal
/// error.
pub fn refused(as_grant: bool) -> EventBrokerError {
    if as_grant {
        EventBrokerError::Unauthorized {
            detail: "no grant".to_owned(),
        }
    } else {
        EventBrokerError::Internal("refused by the scripted broker".to_owned())
    }
}

fn unsupported() -> EventBrokerError {
    EventBrokerError::Internal("not supported by the scripted broker".to_owned())
}

/// Take one from `counter`; `false` when it is spent.
pub fn spend(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

impl ScriptedBroker {
    /// A broker whose joins are assigned a partition of `topic`.
    pub fn on_topic(topic: &'static str) -> Self {
        Self {
            topic,
            agent_prefix: "",
            create_refusals: AtomicUsize::new(0),
            join_refusals: AtomicUsize::new(0),
            refuse_as_grant: AtomicBool::new(false),
            delete_fails: AtomicBool::new(false),
            create_hangs: AtomicBool::new(false),
            join_hangs: AtomicBool::new(false),
            leave_hangs: AtomicBool::new(false),
            streams_end_at_once: AtomicBool::new(false),
            create_calls: Mutex::new(Vec::new()),
            creates: Mutex::new(Vec::new()),
            create_agents: Mutex::new(Vec::new()),
            joins: Mutex::new(Vec::new()),
            join_homes: Mutex::new(Vec::new()),
            seeks: Mutex::new(Vec::new()),
            leaves: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            streams: Mutex::new(Vec::new()),
            next_stream: Mutex::new(None),
        }
    }

    /// Fail a creation whose client agent does not start with `prefix`.
    pub fn expecting_agent_prefix(mut self, prefix: &'static str) -> Self {
        self.agent_prefix = prefix;
        self
    }

    /// How many groups were created.
    pub fn creates(&self) -> usize {
        self.creates.lock().unwrap().len()
    }
    /// The groups deleted, in order.
    pub fn deletes(&self) -> Vec<ConsumerGroupId> {
        self.deletes.lock().unwrap().clone()
    }
    /// The joins made, in order.
    pub fn joins(&self) -> Vec<JoinRequest> {
        self.joins.lock().unwrap().clone()
    }
    /// How many subscriptions were left.
    pub fn leaves(&self) -> usize {
        self.leaves.lock().unwrap().len()
    }
    /// How many streams were opened and are still held.
    pub fn opened_streams(&self) -> usize {
        self.streams.lock().unwrap().len()
    }
    /// Deliver a frame on the most recent stream.
    pub fn send(&self, frame: FrameResult) {
        let streams = self.streams.lock().unwrap();
        streams.last().expect("a stream is open").send(frame).ok();
    }
    /// End the most recent stream.
    pub fn end_stream(&self) {
        self.streams.lock().unwrap().pop();
    }
}

event_broker_impl! {
    impl EventBrokerApi for ScriptedBroker {
        async fn create_consumer_group(
            &self,
            _: &SecurityContext,
            req: CreateConsumerGroupRequest,
        ) -> Result<ConsumerGroup, EventBrokerError> {
            assert!(req.client_agent.starts_with(self.agent_prefix));
            self.create_calls.lock().unwrap().push(Instant::now());
            if self.create_hangs.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if spend(&self.create_refusals) {
                return Err(refused(self.refuse_as_grant.load(Ordering::SeqCst)));
            }
            self.create_agents.lock().unwrap().push(req.client_agent);
            let id = ConsumerGroupId::new(Uuid::new_v4());
            self.creates.lock().unwrap().push(id);
            Ok(ConsumerGroup {
                id,
                tenant_id: GROUP_TENANT,
                owner_principal_id: Uuid::nil(),
                kind: ConsumerGroupKind::Anonymous,
                description: None,
                created_at: Utc::now(),
            })
        }
        async fn delete_consumer_group(
            &self,
            _: &SecurityContext,
            id: &ConsumerGroupId,
        ) -> Result<(), EventBrokerError> {
            self.deletes.lock().unwrap().push(*id);
            if self.delete_fails.load(Ordering::SeqCst) {
                return Err(refused(false));
            }
            Ok(())
        }
        async fn join(
            &self,
            ctx: &SecurityContext,
            req: JoinRequest,
        ) -> Result<SubscriptionAssignment, EventBrokerError> {
            self.join_homes
                .lock()
                .unwrap()
                .push(ctx.subject_tenant_id());
            self.joins.lock().unwrap().push(req);
            if self.join_hangs.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if spend(&self.join_refusals) {
                return Err(refused(self.refuse_as_grant.load(Ordering::SeqCst)));
            }
            let (tx, rx) = unbounded_channel();
            self.streams.lock().unwrap().push(tx);
            *self.next_stream.lock().unwrap() = Some(rx);
            Ok(SubscriptionAssignment {
                subscription_id: SubscriptionId(Uuid::new_v4()),
                topology_version: 7,
                assigned: vec![AssignedPartition {
                    topic: GtsInstanceId::try_new(self.topic).unwrap(),
                    partition: 0,
                }],
            })
        }
        async fn leave(
            &self,
            _: &SecurityContext,
            id: SubscriptionId,
        ) -> Result<(), EventBrokerError> {
            self.leaves.lock().unwrap().push(id);
            if self.leave_hangs.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        }
        async fn stream(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
        ) -> Result<FrameStream, EventBrokerError> {
            if self.streams_end_at_once.load(Ordering::SeqCst) {
                return Ok(Box::pin(futures_util::stream::empty()));
            }
            let rx = self
                .next_stream
                .lock()
                .unwrap()
                .take()
                .ok_or_else(unsupported)?;
            Ok(Box::pin(futures_util::stream::unfold(
                rx,
                |mut rx| async move { rx.recv().await.map(|frame| (frame, rx)) },
            )))
        }
        async fn seek(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
            topology_version: i64,
            positions: &[SeekPosition],
        ) -> Result<Vec<SeekResult>, EventBrokerError> {
            assert_eq!(topology_version, 7);
            self.seeks.lock().unwrap().push(positions.to_vec());
            Ok(Vec::new())
        }
    }
    unsupported: [
        register_producer, publish, publish_batch, get_producer_cursors,
        reset_producer_chain, get_consumer_group, list_consumer_groups, get_subscription,
        list_subscriptions, list_topics, list_topic_segments, list_event_types,
        get_event_type,
    ]
}
