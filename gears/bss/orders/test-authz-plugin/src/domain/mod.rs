//! Domain layer for the e2e policy `AuthZ` resolver plugin: the compiled policy and the
//! service the plugin client delegates to.

mod client;
pub mod policy;
pub mod service;

pub use policy::Policy;
pub use service::Service;
