//! Metadata persistence for the control plane: `SeaORM` entities, tenant-scoped
//! repositories (`SecureORM`), the migration registry, and the persistence
//! facade ([`Store`]) that owns the `DBProvider` and all transaction logic.

pub mod db;
pub mod entity;
pub mod mapper;
pub mod migrations;
pub mod repo;
pub mod store;

pub use store::Store;
