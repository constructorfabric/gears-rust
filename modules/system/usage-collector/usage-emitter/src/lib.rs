//! Durable usage emission for source modules.
//!
//! Provides the complete emitter pipeline: module-scoped PDP authorization, transactional
//! outbox enqueue, and async delivery to the usage collector via `modkit-db` outbox workers.
//!
//! # Crate shape: single crate, not trait-SDK + impl
//!
//! Unlike other modules in the workspace, `usage-emitter` deliberately ships as a single
//! crate (trait + concrete types together) rather than a thin `*-sdk` crate. The trait
//! [`UsageEmitterRuntimeV1`] returns a concrete [`UsageEmitterFactory`], which produces a
//! concrete [`UsageEmitter`] and [`UsageRecordBuilder`]; these types carry private state
//! (`db`, `outbox`, `issued_at`, `allowed_metrics`) that the security invariant relies on,
//! so they cannot be reduced to opaque trait objects without losing the type-state
//! authorization handle. Every consumer therefore depends on the trait *and* the concrete
//! types simultaneously — splitting would not remove the impl-crate dependency. See the
//! crate `README.md` ("Why there is no `usage-emitter-sdk` crate") for the full rationale.
//!
//! # Usage
//!
//! Source modules should not construct [`UsageEmitterFactory`] directly. The `usage-collector`
//! or `usage-collector-rest-client` `ModKit` module builds and registers
//! `dyn UsageEmitterRuntimeV1` in `ClientHub` during `init()`.
//!
//! ```ignore
//! // In init():
//! let runtime = hub.get::<dyn UsageEmitterRuntimeV1>()?;
//! let factory = runtime.factory(Self::MODULE_NAME);
//!
//! // In a handler:
//! let emitter = factory
//!     .clone()
//!     .authorize(&ctx, resource_id, "resource_type")
//!     .await?;
//! let record = emitter
//!     .usage_record_builder("requests", 1.0)?
//!     .build()?;
//! emitter.enqueue(record).await?;
//! ```

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod api;
mod config;
mod domain;
mod error;
mod infra;

pub use api::UsageEmitterRuntimeV1;
pub use config::UsageEmitterConfig;
pub use domain::emitter::UsageEmitter;
pub use domain::factory::UsageEmitterFactory;
pub use domain::runtime::UsageEmitterRuntime;
pub use domain::usage_record_builder::UsageRecordBuilder;
pub use error::UsageEmitterError;
pub use infra::delivery_handler::DeliveryHandler;
