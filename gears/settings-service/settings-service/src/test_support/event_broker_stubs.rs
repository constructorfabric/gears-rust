//! The refusing half of a scripted `EventBrokerApi`.
//!
//! The SDK trait has nineteen methods and every scripted broker in the tests
//! scripts only the few its scenario drives; the others answer "not supported".
//! [`event_broker_impl`] writes that impl once: the test lists the methods it
//! scripts in the braces and the methods that refuse by name, and the macro
//! emits the `#[async_trait]` impl with both. The call site supplies the
//! SDK imports and the `unsupported()` error the refusing methods return.
//!
//! The file is shared by the lib's unit tests (`crate::test_support`) and the
//! `PostgreSQL` suites (`tests/support`, through a `#[path]` module).

/// `event_broker_impl!(impl EventBrokerApi for Broker { scripted methods }
/// unsupported: [method, ...])` expands to the trait impl: the scripted
/// methods verbatim, then one `Err(unsupported())` method per listed name.
macro_rules! event_broker_impl {
    (impl EventBrokerApi for $ty:ty { $($scripted:tt)* } unsupported: [$($name:ident),* $(,)?]) => {
        event_broker_impl!(@collect [$ty] [$($scripted)*] [] $($name)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] register_producer $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [register_producer (ProducerMode, &str) ProducerId]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] publish $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [publish (&EventV1, Ack) IngestOutcome]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] publish_batch $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [publish_batch (&[EventV1], Ack) IngestOutcome]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] get_producer_cursors $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [get_producer_cursors (ProducerId) ProducerCursors]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] reset_producer_chain $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [reset_producer_chain (ProducerId, ResetScope<'_>) ()]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] create_consumer_group $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [create_consumer_group (CreateConsumerGroupRequest) ConsumerGroup]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] get_consumer_group $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [get_consumer_group (&ConsumerGroupId) ConsumerGroup]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] list_consumer_groups $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [list_consumer_groups (ConsumerGroupQuery) Page<ConsumerGroup>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] delete_consumer_group $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [delete_consumer_group (&ConsumerGroupId) ()]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] join $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [join (JoinRequest) SubscriptionAssignment]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] get_subscription $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [get_subscription (SubscriptionId) Subscription]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] list_subscriptions $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [list_subscriptions () Vec<Subscription>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] leave $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [leave (SubscriptionId) ()]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] stream $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [stream (SubscriptionId) FrameStream]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] seek $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [seek (SubscriptionId, i64, &[SeekPosition]) Vec<SeekResult>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] list_topics $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [list_topics () Vec<Topic>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] list_topic_segments $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [list_topic_segments (&str, u32, PartitionRange) TopicSegment]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] list_event_types $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [list_event_types () Vec<EventType>]] $($rest)*);
    };
    (@collect $t:tt $s:tt [$($acc:tt)*] get_event_type $($rest:ident)*) => {
        event_broker_impl!(@collect $t $s [$($acc)* [get_event_type (&str) EventType]] $($rest)*);
    };
    (@collect [$ty:ty] [$($scripted:tt)*] [$([$name:ident ($($arg:ty),*) $ret:ty])*]) => {
        #[async_trait::async_trait]
        impl EventBrokerApi for $ty {
            $($scripted)*
            $(
                async fn $name(
                    &self,
                    _: &SecurityContext
                    $(, _: $arg)*
                ) -> Result<$ret, EventBrokerError> {
                    Err(unsupported())
                }
            )*
        }
    };
}

pub(crate) use event_broker_impl;
