//! Sole Orders schema owner and secure persistence boundary.
// These internal APIs are consumed by the subsequent engine/maintenance packages.
// Public business operations remain unavailable until those packages are delivered.
#[allow(dead_code)]
pub mod entity;
pub mod migrations;
#[allow(dead_code)]
pub(in crate::infra) mod repo;
#[allow(dead_code)]
pub mod scoped;
pub const TABLE_PREFIX: &str = "bss_orders__";
pub fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
    migrations::all()
}

#[allow(dead_code)]
pub mod interval;
#[cfg(test)]
mod tests;
