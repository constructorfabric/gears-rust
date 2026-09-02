//! The gear's inbound API surface (DESIGN.md).
//!
//! Two planes, deliberately split (§2.2): the coordination **data
//! plane** is gRPC, and the lifecycle/admin plane is REST. Only the first exists
//! today — [`grpc`] holds the four hand-written service impls of item `S1`; the
//! admin routes are item `S4`.

pub mod grpc;
