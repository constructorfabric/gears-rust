//! `pricing_outbox` — the transactional event outbox
//! (`design/01-foundation.md` §3.7).
//!
//! Transactional is the point: an event exists if and only if its publish
//! commit happened, because the row is written in that same commit. Delivery is
//! **at-least-once** and carries dedup and correlation keys, so a consumer
//! dedups rather than assuming exactly-once, and ordering is per
//! `(tenant_id, aggregate_id)` — not global, because a global sequence would
//! serialize every tenant's publishing behind one counter and no consumer needs
//! cross-aggregate order.
//!
//! Four physical guards. `chk_pricing_outbox_event_name` pins the **frozen**
//! event-name set — the fourteen names of `domain::events::CatalogEvent`. A name
//! here is a contract a consumer is entitled to keep receiving forever, so a typo
//! must fail at insert rather than become an event nobody is subscribed to.
//! `uq_pricing_outbox_sequence` makes the per-aggregate order a total order — two
//! rows at the same seq would leave the relay free to pick either.
//! `uq_pricing_outbox_dedup_key` makes the dedup key actually dedup, at the writer
//! rather than at every consumer. `idx_pricing_outbox_undrained` is the relay's
//! cursor: the drain scans only rows still unpublished, so the index does not grow
//! with delivered history.
//!
//! # The frozen set is frozen in two places, and they are pinned to each other
//!
//! `CatalogEvent` in the domain and `chk_pricing_outbox_event_name` here have to
//! agree, and `postgres_schema_stores::every_frozen_event_name_is_enqueueable` is
//! what makes that a check: it drives every name in `CatalogEvent::ALL` into the
//! table, so a name added to the enum alone does not merely fail to be emittable, it
//! fails that test. D-248 was written without knowing `PriceOverlayPublished` would
//! cost a table rebuild, and the first run of that case found out — with a **driver**
//! refusal (`code: 275, CHECK constraint failed`) rather than an assertion.
//!
//! **Adding the fifteenth name is not a one-line change on `SQLite`.** Postgres drops
//! and re-adds a named constraint; `SQLite` cannot alter a `CHECK` at all, so the
//! table has to be rebuilt — create, copy, drop, rename — and a rebuild takes the
//! **indexes** with it, because `DROP TABLE` drops them. Recreating all three after
//! the rename is not tidiness: leaving `uq_pricing_outbox_dedup_key` out drops
//! at-least-once delivery's dedup silently, and nothing in the fast tier would see
//! the duplicate events.
//!
//! **Backend differences.** `jsonb` becomes `text` on `SQLite`; the partial
//! index and both unique constraints carry over unchanged.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_outbox (
            tenant_id      uuid        NOT NULL,
            outbox_id      uuid        NOT NULL,
            aggregate_id   uuid        NOT NULL,
            correlation_id uuid        NOT NULL,
            dedup_key      text        NOT NULL,
            enqueued_at    timestamptz NOT NULL DEFAULT now(),
            event_name     text        NOT NULL,
            payload        jsonb       NOT NULL,
            published_at   timestamptz,
            seq            bigint      NOT NULL,
            CONSTRAINT chk_pricing_outbox_event_name CHECK (event_name IN ('PlanCreated','PlanUpdated','PlanPublished','PlanRetired', 'PlanMigrationScheduled','PlanPublishDegraded','BundleUpdated', 'PriceCreated','PriceUpdated','PriceWindowScheduled', 'PriceWindowActivated','PriceWindowExpired','PriceWindowCancelled', 'PriceOverlayPublished')),
            CONSTRAINT chk_pricing_outbox_sequence CHECK (seq >= 0),
            CONSTRAINT pricing_outbox_pkey PRIMARY KEY (outbox_id)
        )",
    "CREATE INDEX idx_pricing_outbox_undrained ON bss.pricing_outbox USING btree (tenant_id, aggregate_id, seq) WHERE (published_at IS NULL)",
    "CREATE UNIQUE INDEX uq_pricing_outbox_dedup_key ON bss.pricing_outbox USING btree (tenant_id, dedup_key)",
    "CREATE UNIQUE INDEX uq_pricing_outbox_sequence ON bss.pricing_outbox USING btree (tenant_id, aggregate_id, seq)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_outbox"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_outbox (
            tenant_id      text   NOT NULL,
            outbox_id      text   NOT NULL,
            aggregate_id   text   NOT NULL,
            correlation_id text   NOT NULL,
            dedup_key      text   NOT NULL,
            enqueued_at    text   NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            event_name     text   NOT NULL,
            payload        text   NOT NULL,
            published_at   text,
            seq            bigint NOT NULL,
            PRIMARY KEY (outbox_id),
            CONSTRAINT chk_pricing_outbox_event_name CHECK (event_name IN ('PlanCreated','PlanUpdated','PlanPublished','PlanRetired', 'PlanMigrationScheduled','PlanPublishDegraded','BundleUpdated', 'PriceCreated','PriceUpdated','PriceWindowScheduled', 'PriceWindowActivated','PriceWindowExpired','PriceWindowCancelled', 'PriceOverlayPublished')),
            CONSTRAINT chk_pricing_outbox_sequence CHECK (seq >= 0)
        )",
    "CREATE INDEX idx_pricing_outbox_undrained ON pricing_outbox (tenant_id, aggregate_id, seq) WHERE published_at IS NULL",
    "CREATE UNIQUE INDEX uq_pricing_outbox_dedup_key ON pricing_outbox (tenant_id, dedup_key)",
    "CREATE UNIQUE INDEX uq_pricing_outbox_sequence ON pricing_outbox (tenant_id, aggregate_id, seq)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_outbox"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP_STATEMENTS, SQLITE_UP_STATEMENTS).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            PG_DOWN_STATEMENTS,
            SQLITE_DOWN_STATEMENTS,
        )
        .await
    }
}
