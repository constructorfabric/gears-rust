//! REST API layer for the Types Registry gear.

pub mod dto;
/// The crate's error ladder, transport-free; kept reachable at its REST path.
pub use super::error;
mod etag;
pub mod handlers;
mod params;
mod paths;
pub mod routes;
mod select;
