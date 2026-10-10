//! Per-route credential requirement shared by `OperationSpec`, listeners and the gateway.

/// Which credential a route requires. Exhaustive on purpose: a new policy must be
/// handled explicitly by every listener and the gateway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RouteAuth {
    /// No credential required (`.anonymous()`). A gear listener with a tenant plane still
    /// validates a presented bearer; the gateway ignores it.
    Anonymous,
    /// A validated tenant bearer is required (`.authenticated()`).
    Authenticated,
    /// A validated `X-ToolKit-Internal-Token` is required; a bearer alone is refused
    /// (`.platform_authenticated()`).
    Platform,
}
