//! Domain layer of the admission-control gear: the admission service that
//! sequences request checks, the engine call and event emission, and the
//! local client.

pub mod local_client;
pub mod service;
