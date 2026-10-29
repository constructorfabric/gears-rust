//! Infrastructure of Rating: persistence, the outbox work queues and their depth, advisory
//! leadership over the Cluster SDK, the feed-read grant check, the clock, the metrics adapter and
//! the error mapping.

pub mod clock;
pub mod cluster;
pub mod error_mapping;
pub mod feed_grant;
pub mod metrics;
pub mod queue_depth;
pub mod queues;
pub mod storage;
pub mod tenant_hierarchy;
