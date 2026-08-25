//! SeaORM persistence for the ledger-owned `bss` schema.
//!
//! Provides the ordered migration chain, table entities, scoped repositories,
//! and OData-to-column mappings used by posting and inquiry services.

pub mod entity;
pub mod migrations;
/// `OData` field → `SeaORM` column mappers consumed by `paginate_odata` in the
/// list repos (the `FieldToColumn` / `ODataFieldMapping` impls).
pub mod odata_mapping;
pub mod repo;
