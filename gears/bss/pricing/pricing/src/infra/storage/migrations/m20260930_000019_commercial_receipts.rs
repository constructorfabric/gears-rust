//! Durable commercial receipts (D-505). No expiry cleanup or dependency on mutable catalog rows.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG, SQLITE).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            &[
                "DROP TABLE IF EXISTS bss.pricing_commercial_command",
                "DROP TABLE IF EXISTS bss.pricing_hold",
                "DROP TABLE IF EXISTS bss.pricing_acceptance",
            ],
            &[
                "DROP TABLE IF EXISTS pricing_commercial_command",
                "DROP TABLE IF EXISTS pricing_hold",
                "DROP TABLE IF EXISTS pricing_acceptance",
            ],
        )
        .await
    }
}
const PG: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss.pricing_acceptance (
  id uuid NOT NULL,
  tenant_id uuid NOT NULL,
  order_id uuid NOT NULL,
  order_version text NOT NULL,
  line_id uuid NOT NULL,
  request_digest text NOT NULL,
  terms_digest text NOT NULL,
  receipt_json text NOT NULL,
  accepted_at text NOT NULL,
  hold_until text NOT NULL,
  created_by uuid NOT NULL,
  CONSTRAINT pricing_acceptance_pkey PRIMARY KEY (tenant_id, id),
  CONSTRAINT pricing_acceptance_window CHECK (hold_until > accepted_at),
  CONSTRAINT pricing_acceptance_version CHECK (length(order_version) BETWEEN 1 AND 20 AND substr(order_version,1,1) <> '0' AND (length(order_version) < 20 OR order_version <= '18446744073709551615') AND order_version ~ '^[0-9]+$'),
  CONSTRAINT pricing_acceptance_request_digest_check CHECK (request_digest ~ '^[0-9a-f]{64}$'),
  CONSTRAINT pricing_acceptance_terms_digest_check CHECK (terms_digest ~ '^[0-9a-f]{64}$')
)",
    r"CREATE TABLE IF NOT EXISTS bss.pricing_hold (
  id uuid NOT NULL,
  tenant_id uuid NOT NULL,
  acceptance_id uuid NOT NULL,
  activation_at text NOT NULL,
  terms_digest text NOT NULL,
  receipt_json text NOT NULL,
  created_by uuid NOT NULL,
  created_at text NOT NULL,
  CONSTRAINT pricing_hold_pkey PRIMARY KEY (tenant_id, id),
  CONSTRAINT pricing_hold_acceptance_fk FOREIGN KEY (tenant_id, acceptance_id) REFERENCES bss.pricing_acceptance (tenant_id, id),
  CONSTRAINT pricing_hold_terms_digest_check CHECK (terms_digest ~ '^[0-9a-f]{64}$')
)",
    r"CREATE TABLE IF NOT EXISTS bss.pricing_commercial_command (
  id uuid NOT NULL,
  tenant_id uuid NOT NULL,
  caller_tenant_id uuid NOT NULL,
  caller_id uuid NOT NULL,
  operation text NOT NULL,
  idempotency_key text NOT NULL,
  request_digest text NOT NULL,
  receipt_kind text NOT NULL,
  receipt_id uuid NOT NULL,
  acceptance_id uuid,
  hold_id uuid,
  CONSTRAINT pricing_commercial_command_pkey PRIMARY KEY (tenant_id, id),
  CONSTRAINT pricing_commercial_command_target CHECK ((operation = 'check' AND receipt_kind = 'acceptance' AND acceptance_id IS NOT NULL AND receipt_id = acceptance_id AND hold_id IS NULL) OR (operation = 'hold' AND receipt_kind = 'hold' AND hold_id IS NOT NULL AND receipt_id = hold_id AND acceptance_id IS NULL)),
  CONSTRAINT pricing_commercial_command_acceptance_fk FOREIGN KEY (tenant_id, acceptance_id) REFERENCES bss.pricing_acceptance (tenant_id, id),
  CONSTRAINT pricing_commercial_command_hold_fk FOREIGN KEY (tenant_id, hold_id) REFERENCES bss.pricing_hold (tenant_id, id),
  CONSTRAINT pricing_commercial_command_key CHECK (length(idempotency_key) > 0),
  CONSTRAINT pricing_commercial_command_request_digest_check CHECK (request_digest ~ '^[0-9a-f]{64}$')
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_acceptance_business ON bss.pricing_acceptance (tenant_id, order_id, order_version, line_id)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_hold_acceptance ON bss.pricing_hold (tenant_id, acceptance_id)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_commercial_command_scope ON bss.pricing_commercial_command (tenant_id, caller_tenant_id, caller_id, operation, idempotency_key)",
    r"CREATE INDEX IF NOT EXISTS pricing_commercial_command_acceptance ON bss.pricing_commercial_command (tenant_id, acceptance_id)",
    r"CREATE INDEX IF NOT EXISTS pricing_commercial_command_hold ON bss.pricing_commercial_command (tenant_id, hold_id)",
];
const SQLITE: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS pricing_acceptance (
  id text NOT NULL,
  tenant_id text NOT NULL,
  order_id text NOT NULL,
  order_version text NOT NULL,
  line_id text NOT NULL,
  request_digest text NOT NULL,
  terms_digest text NOT NULL,
  receipt_json text NOT NULL,
  accepted_at text NOT NULL,
  hold_until text NOT NULL,
  created_by text NOT NULL,
  CONSTRAINT pricing_acceptance_pkey PRIMARY KEY (tenant_id, id),
  CONSTRAINT pricing_acceptance_window CHECK (hold_until > accepted_at),
  CONSTRAINT pricing_acceptance_version CHECK (length(order_version) BETWEEN 1 AND 20 AND substr(order_version,1,1) <> '0' AND (length(order_version) < 20 OR order_version <= '18446744073709551615') AND order_version NOT GLOB '*[^0-9]*'),
  CONSTRAINT pricing_acceptance_request_digest_check CHECK (length(request_digest) = 64 AND request_digest NOT GLOB '*[^0-9a-f]*'),
  CONSTRAINT pricing_acceptance_terms_digest_check CHECK (length(terms_digest) = 64 AND terms_digest NOT GLOB '*[^0-9a-f]*')
)",
    r"CREATE TABLE IF NOT EXISTS pricing_hold (
  id text NOT NULL,
  tenant_id text NOT NULL,
  acceptance_id text NOT NULL,
  activation_at text NOT NULL,
  terms_digest text NOT NULL,
  receipt_json text NOT NULL,
  created_by text NOT NULL,
  created_at text NOT NULL,
  CONSTRAINT pricing_hold_pkey PRIMARY KEY (tenant_id, id),
  CONSTRAINT pricing_hold_acceptance_fk FOREIGN KEY (tenant_id, acceptance_id) REFERENCES pricing_acceptance (tenant_id, id),
  CONSTRAINT pricing_hold_terms_digest_check CHECK (length(terms_digest) = 64 AND terms_digest NOT GLOB '*[^0-9a-f]*')
)",
    r"CREATE TABLE IF NOT EXISTS pricing_commercial_command (
  id text NOT NULL,
  tenant_id text NOT NULL,
  caller_tenant_id text NOT NULL,
  caller_id text NOT NULL,
  operation text NOT NULL,
  idempotency_key text NOT NULL,
  request_digest text NOT NULL,
  receipt_kind text NOT NULL,
  receipt_id text NOT NULL,
  acceptance_id text,
  hold_id text,
  CONSTRAINT pricing_commercial_command_pkey PRIMARY KEY (tenant_id, id),
  CONSTRAINT pricing_commercial_command_target CHECK ((operation = 'check' AND receipt_kind = 'acceptance' AND acceptance_id IS NOT NULL AND receipt_id = acceptance_id AND hold_id IS NULL) OR (operation = 'hold' AND receipt_kind = 'hold' AND hold_id IS NOT NULL AND receipt_id = hold_id AND acceptance_id IS NULL)),
  CONSTRAINT pricing_commercial_command_acceptance_fk FOREIGN KEY (tenant_id, acceptance_id) REFERENCES pricing_acceptance (tenant_id, id),
  CONSTRAINT pricing_commercial_command_hold_fk FOREIGN KEY (tenant_id, hold_id) REFERENCES pricing_hold (tenant_id, id),
  CONSTRAINT pricing_commercial_command_key CHECK (length(idempotency_key) > 0),
  CONSTRAINT pricing_commercial_command_request_digest_check CHECK (length(request_digest) = 64 AND request_digest NOT GLOB '*[^0-9a-f]*')
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_acceptance_business ON pricing_acceptance (tenant_id, order_id, order_version, line_id)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_hold_acceptance ON pricing_hold (tenant_id, acceptance_id)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_commercial_command_scope ON pricing_commercial_command (tenant_id, caller_tenant_id, caller_id, operation, idempotency_key)",
    r"CREATE INDEX IF NOT EXISTS pricing_commercial_command_acceptance ON pricing_commercial_command (tenant_id, acceptance_id)",
    r"CREATE INDEX IF NOT EXISTS pricing_commercial_command_hold ON pricing_commercial_command (tenant_id, hold_id)",
];
