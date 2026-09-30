#[path = "producer/builder.rs"]
mod builder;
#[path = "producer/direct.rs"]
mod direct;
// The producer-outbox tests run against the real in-process gear harness, so
// each failure mode has to be provoked on the broker itself: the two
// partition-key tests need a second catalog event type whose partition key
// points into the payload, because the broker fixes one partition key per type
// and cannot be repointed at runtime; transient rate-limit retry uses
// `EventBrokerHarness::set_publish_rate_limited`; and broker-forgot-producer
// rotation uses `EventBrokerHarness::forget_producer`.
#[cfg(all(feature = "db", feature = "outbox"))]
#[path = "producer/outbox.rs"]
mod outbox;
