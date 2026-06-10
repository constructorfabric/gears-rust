// Created: 2026-06-04 by Constructor Tech
//! Service-discovery primitive — instance registration with metadata, filtered
//! discovery, a reactive topology watch, and a module-declared serving-intent
//! signal.
//!
//! Provides the [`ServiceDiscoveryBackend`] plugin trait, the
//! [`ServiceDiscoveryV1`] facade, the [`ServiceHandle`] registration handle with
//! its typed command channel, the [`ServiceWatch`] topology stream, the domain
//! types and extensible [`DiscoveryFilter`], the capability/features
//! descriptors, and the fluent [`ServiceDiscoveryResolverBuilder`] with startup
//! capability validation.
//!
//! **Serving intent, not health (ADR-008):** [`InstanceState`] is module-declared
//! serving intent. Liveness comes from the backend's TTL-bounded heartbeat
//! renewal — a stuck instance disappears from discovery only when heartbeating
//! stops, not by flipping its own state.
//!
//! **Metadata is not scoped (DESIGN §3.8):** metadata keys and values are a
//! per-instance attribute namespace; coordination namespacing lives on the
//! service `name`.
//!
//! The heartbeat-renewal algorithm and instance-id auto-assignment are **backend
//! contracts** documented on [`ServiceDiscoveryBackend`]; the SDK is
//! contract-only and runs no timers or id generation. The
//! Apply-Discovery-Filter algorithm, by contrast, is pure predicate logic and
//! lives here as [`DiscoveryFilter::matches`] — the consumer reuses it to filter
//! the unfiltered watch client-side.
//!
//! `scoped()` (DECOMPOSITION §2.7) and [`ServiceWatch`] auto-restart
//! (DECOMPOSITION §2.8) are intentionally out of scope for this primitive and
//! delivered by their dedicated features.

pub mod backend;
pub mod facade;
pub mod handle;
pub mod resolver;
mod scoped;
pub mod types;
pub mod watch;

pub(crate) use scoped::ScopedServiceDiscoveryBackend;

pub use backend::ServiceDiscoveryBackend;
pub use facade::ServiceDiscoveryV1;
pub use handle::{ServiceCommandReceiver, ServiceHandle, ServiceRequest, ServiceResponder};
pub use resolver::{ServiceDiscoveryResolverBuilder, validate_service_discovery_capabilities};
pub use types::{
    DiscoveryFilter, InstanceState, MetaMatch, ServiceDiscoveryCapability,
    ServiceDiscoveryFeatures, ServiceInstance, ServiceRegistration, StateFilter,
};
pub use watch::{ServiceWatch, ServiceWatchEvent, ServiceWatchSender, TopologyChange};
