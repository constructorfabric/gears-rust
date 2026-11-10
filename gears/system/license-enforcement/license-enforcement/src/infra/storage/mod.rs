//! The gear store (DESIGN §3.7).

pub mod entity;
pub mod migrations;
mod odata;
mod repo;

pub use repo::Store;
