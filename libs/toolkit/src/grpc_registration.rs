//! gRPC registration data shared by gear capabilities and the runtime.

/// Represents a gRPC service registration callback used by the gRPC hub.
///
/// Each gear that exposes gRPC services provides one or more of these.
/// The `register` closure adds the service into the provided `RoutesBuilder`.
#[cfg(feature = "otel")]
pub struct RegisterGrpcServiceFn {
    pub service_name: &'static str,
    pub register: Box<dyn Fn(&mut tonic::service::RoutesBuilder) + Send + Sync>,
}

#[cfg(not(feature = "otel"))]
pub struct RegisterGrpcServiceFn {
    pub service_name: &'static str,
}
