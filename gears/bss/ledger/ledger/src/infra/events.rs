//! Events surface: the ledger's published event payloads and the publisher
//! adapter that writes them through the platform event broker.
//!
//! Payloads are infra types (serde + `TypedEvent`), not domain models and not
//! REST DTOs. They carry internal identifiers only — never PII.

pub mod alarm_catalog;
pub mod payloads;
pub mod publisher;
pub mod schemas;
