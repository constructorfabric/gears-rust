//! Persistence layer: migrations, entities, repos (added incrementally).

pub mod entity;
pub mod migrations;
/// `OData` field → `SeaORM` column mappers consumed by `paginate_odata` in the
/// list repos (the `FieldToColumn` / `ODataFieldMapping` impls).
pub mod odata_mapping;
pub mod repo;
