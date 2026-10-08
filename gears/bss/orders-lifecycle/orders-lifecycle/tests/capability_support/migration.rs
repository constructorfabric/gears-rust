//! Test-only DDL and role provisioning, executed solely inside an isolated container.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Probe;
#[async_trait::async_trait]
impl MigrationTrait for Probe {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let ddl = [
            "CREATE TABLE orders_capability_order (order_id uuid PRIMARY KEY, resource_tenant_id uuid NOT NULL, seller_tenant_id uuid NOT NULL, payer_tenant_id uuid NOT NULL, version integer NOT NULL, state text NOT NULL)",
            "CREATE TABLE orders_capability_history (order_id uuid REFERENCES orders_capability_order(order_id), version integer NOT NULL, old_payer_id uuid NOT NULL, PRIMARY KEY(order_id, version))",
            "CREATE TABLE orders_capability_audit (id uuid PRIMARY KEY, value text NOT NULL)",
            "CREATE TABLE orders_capability_private (id uuid PRIMARY KEY, value text NOT NULL)",
            "CREATE TABLE orders_capability_checkpoint (id uuid PRIMARY KEY, value text NOT NULL)",
            "CREATE TABLE orders_conformance_vector (id uuid PRIMARY KEY, captured_at timestamptz NOT NULL, preimage bytea NOT NULL, digest bytea GENERATED ALWAYS AS (sha256(preimage)) STORED)",
            "CREATE ROLE probe_business LOGIN PASSWORD 'fixture'",
            "CREATE ROLE probe_private LOGIN PASSWORD 'fixture'",
            "CREATE ROLE probe_verifier LOGIN PASSWORD 'fixture'",
            "CREATE ROLE probe_checkpoint LOGIN PASSWORD 'fixture'",
            "CREATE ROLE probe_retention LOGIN PASSWORD 'fixture'",
            "CREATE ROLE probe_maintenance LOGIN PASSWORD 'fixture'",
            "CREATE ROLE probe_discovery LOGIN PASSWORD 'fixture'",
            "GRANT SELECT, INSERT, UPDATE ON orders_capability_order TO probe_business",
            "GRANT SELECT ON orders_capability_history TO probe_business",
            "GRANT SELECT, INSERT ON orders_capability_audit TO probe_private",
            "GRANT SELECT, INSERT, UPDATE ON orders_capability_private TO probe_private",
            "GRANT SELECT ON orders_capability_audit, orders_capability_checkpoint TO probe_verifier",
            "GRANT SELECT, INSERT ON orders_capability_checkpoint TO probe_checkpoint",
            "GRANT SELECT, DELETE ON orders_capability_private TO probe_retention",
            "GRANT SELECT, UPDATE ON orders_capability_order TO probe_maintenance",
            "GRANT SELECT ON orders_capability_order TO probe_discovery",
        ];
        for sql in ddl {
            manager.get_connection().execute_unprepared(sql).await?;
        }
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("probe container is disposable".into()))
    }
}
