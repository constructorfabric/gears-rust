//! Sole Orders schema owner and secure persistence boundary.
// The entities and repositories cover the whole S1 schema; the operations that read or write
// part of it (policy administration, submit and fulfillment children, audit pages) are later
// packages, so those adapters have no production caller yet.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the date-policy election and state-TTL policy entities have no production \
                  reader until policy administration is delivered"
    )
)]
#[cfg_attr(
    test,
    allow(
        dead_code,
        reason = "the storage tests reach only part of the schema, so whether the lint fires \
                  differs per test target; `expect` cannot be verified there"
    )
)]
pub mod entity;
pub mod migrations;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "repository functions for undelivered operations (fulfillment children, policy \
                  administration, audit pages, registry receipts) have no production caller yet"
    )
)]
#[cfg_attr(
    test,
    allow(
        dead_code,
        reason = "the storage tests reach only part of the repositories, so whether the lint \
                  fires differs per test target; `expect` cannot be verified there"
    )
)]
pub(in crate::infra) mod repo;
pub mod scoped;
/// Query-builder expressions for the PostgreSQL functions the storage statements call.
pub(in crate::infra) mod sql;
/// The closed registry, attempt and control status sets and their stored tokens.
pub(in crate::infra) mod status;
pub mod term;
/// Prefix of every Orders table name.
pub const TABLE_PREFIX: &str = "bss_orders__";
/// All Orders schema migrations, in apply order.
pub fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
    migrations::all()
}

pub mod interval;
#[cfg(test)]
mod tests;
