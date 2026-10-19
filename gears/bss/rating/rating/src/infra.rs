//! Infrastructure of the foundation: persistence, the outbox work queues, advisory leadership
//! over the Cluster SDK, the clock, the metrics adapter and the error mapping.

pub mod clock;
pub mod cluster;
pub mod error_mapping;
pub mod metrics;
pub mod queues;
pub mod storage;
