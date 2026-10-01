//! Stable reason codes shared by the engine plugin and the management surface.

/// Reason codes the engine reports with a denial or a rejected caller.
pub mod reason {
    /// An enforcing assignment's document denied the operation.
    pub const POLICY_DENIED: &str = "POLICY_DENIED";
    /// The resource tenant is not reachable from the caller's tenant.
    pub const TENANT_BOUNDARY: &str = "TENANT_BOUNDARY";
    /// The caller presented an anonymous security context.
    pub const SECURITY_CONTEXT_REQUIRED: &str = "SECURITY_CONTEXT_REQUIRED";
}
