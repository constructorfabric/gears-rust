//! REST handlers, grouped per `DESIGN.md:579-586`'s module tree. Every
//! handler is a `todo!()` shell - request/response wiring lands with
//! #4346.

pub mod consumer_groups;
pub mod delivery;
pub mod event_types;
pub mod ingest;
pub mod producers;
pub mod subscriptions;
pub mod topics;
