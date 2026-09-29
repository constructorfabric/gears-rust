//! Notification dispatch (`features/notifications.md`).
//!
//! Every committed mutation leaves its events in the storage plugin's
//! notification outbox. The plugin owns the one pipeline that drains it and
//! calls back into [`NotificationDispatcher`] once per claimed event; the
//! dispatcher fans the event out to every registered sink and tells the
//! pipeline whether to acknowledge, retry, or dead-letter it. Bootstrap
//! resolves the sinks and starts the pipeline, and [`DeliveryLifecycle`]
//! stops it on shutdown.

mod dispatcher;
mod lifecycle;
mod system;

pub use dispatcher::{DispatchLimits, NotificationDispatcher};
pub use lifecycle::DeliveryLifecycle;
pub use system::{DISPATCHER_SUBJECT_ID, DISPATCHER_SUBJECT_TYPE, dispatcher_context};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "notifications_tests.rs"]
mod notifications_tests;
