//! Domain layer of the admission-control gear: the built-in policy set, the
//! admission service that sequences it with the engine call and event
//! emission, and the local client.

pub mod builtin;
pub mod local_client;
pub mod service;
