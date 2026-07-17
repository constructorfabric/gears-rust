#[path = "producer/builder.rs"]
mod builder;
#[cfg(feature = "test-util")]
#[path = "producer/direct.rs"]
mod direct;
#[cfg(all(feature = "db", feature = "outbox", feature = "test-util"))]
#[path = "producer/outbox.rs"]
mod outbox;
