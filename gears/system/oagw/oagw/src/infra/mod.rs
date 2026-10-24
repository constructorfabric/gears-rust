pub(crate) mod metrics;
/// Built-in auth, guard and transform plugin implementations and registries.
pub(crate) mod plugin;
/// HTTP/WebSocket proxy built on Pingora.
pub(crate) mod proxy;
/// Per-replica proxy configuration cache (ADR-0019).
pub(crate) mod proxy_cache;
/// In-memory repository implementations.
pub(crate) mod storage;
pub(crate) mod type_provisioning;
