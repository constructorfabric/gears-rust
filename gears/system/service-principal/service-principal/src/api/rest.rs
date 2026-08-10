//! REST API surface for the service-principal gear.

pub mod dto;
pub mod error;
pub mod handlers;
pub mod routes;

pub use routes::register_routes;
