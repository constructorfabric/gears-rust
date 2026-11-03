//! The storage backend a topic's events live in, as the domain sees it: the
//! registry name a topic routes to, the routing that picks it, the resolver
//! call sites ask for the built backend, and the conversions between the
//! domain's event and the SDK's (feature 0005 §2.1, §2.2).

mod conversions;
mod name;
mod resolver;
mod routing;

pub use conversions::{from_sdk_event, partition_input, to_sdk_event};
pub use name::{InvalidName, Name};
pub use resolver::BackendResolver;
pub use routing::Routing;

#[cfg(test)]
#[path = "name_tests.rs"]
mod name_tests;
#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;
