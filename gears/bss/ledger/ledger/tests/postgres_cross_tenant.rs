//! Postgres-only end-to-end for the cross-tenant elevation gateway + the
//! audit-retrieval reads (Slice 6 Phase 2 Group 2C). Boots a container, migrates,
//! then drives `CrossTenantGateway::resolve_read_scope` inside a transaction and
//! asserts the elevation contract:
//!   (a) `target = home` (routine) writes NO audit record and returns the home
//!       scope;
//!   (b) `target != home` + role + reason writes ONE `cross-tenant-access`
//!       secured_audit_record and returns the target scope;
//!   (c) `target != home` + no reason is `MissingInvestigationReason` BEFORE any
//!       read or write (no audit record);
//!   (d) `target != home` + role=false is `CrossTenantAccessDenied` (no record).
//! Plus the audit-retrieval reads: a posted entry's who/when/source/correlation
//! dims, and a tamper-status read reflecting an inserted scope-freeze.
//!
//! A forced audit-append failure (case (e)) is omitted: the append is sealed by
//! the same in-txn chain machinery as the post path, and there is no hermetic
//! seam to make ONLY the append fail without also breaking the read — the
//! propagation is covered structurally (a `?` on the append in
//! `resolve_read_scope`). Ignored by default; run with `-- --ignored`.
#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use bss_ledger::infra::audit::retrieval::AuditRetrievalReader;
use bss_ledger::infra::authz::cross_tenant::{CrossTenantGateway, TargetScope};
use bss_ledger::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::pep_properties;
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn.query_one(pg(sql.to_owned())).await.unwrap();
    row.map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap())
}

async fn scalar_text(conn: &DatabaseConnection, sql: &str) -> Option<String> {
    let row = conn.query_one(pg(sql.to_owned())).await.unwrap();
    row.and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
}

/// Boot, migrate, return the migrate connection + the bss-search-path provider.
async fn setup(container_url: &str) -> (DatabaseConnection, DBProvider<DbError>) {
    let raw = Database::connect(container_url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{container_url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    (raw, DBProvider::<DbError>::new(tdb))
}

/// (a) A routine resolve (`target == home`) writes NO audit record and returns
/// the home scope.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn routine_resolve_writes_no_record_returns_home_scope() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;
    let home = Uuid::now_v7();

    let scope = provider
        .transaction(move |txn| {
            Box::pin(async move {
                CrossTenantGateway::new()
                    .resolve_read_scope(
                        txn,
                        home,
                        Some(TargetScope { tenant_id: home }),
                        true,
                        "actor-1",
                        Some("reason text"),
                        Some("INVESTIGATION"),
                        None,
                    )
                    .await
            })
        })
        .await
        .expect("routine resolve must succeed");

    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, home),
        "routine resolve returns the home scope"
    );
    let records = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(records, 0, "a routine resolve writes NO forensic record");
}

/// (b) A cross-tenant resolve with role + reason writes ONE `cross-tenant-access`
/// record (under the HOME tenant) and returns the TARGET scope.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_resolve_writes_record_returns_target_scope() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;
    let home = Uuid::now_v7();
    let target = Uuid::now_v7();

    let scope = provider
        .transaction(move |txn| {
            Box::pin(async move {
                CrossTenantGateway::new()
                    .resolve_read_scope(
                        txn,
                        home,
                        Some(TargetScope { tenant_id: target }),
                        true,
                        "investigator-7",
                        Some("fraud investigation #42"),
                        Some("FRAUD"),
                        None,
                    )
                    .await
            })
        })
        .await
        .expect("elevated resolve must succeed");

    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target),
        "elevated resolve returns the TARGET scope"
    );
    assert!(
        !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, home),
        "the returned scope opens the target, not the home tenant"
    );

    // Exactly one forensic record, written under the HOME tenant.
    let records = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(records, 1, "an elevated resolve writes ONE forensic record");

    // The record carries the reason_code + targetScope/reason in before_after.
    let reason_code = scalar_text(
        &raw,
        &format!(
            "SELECT reason_code FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(reason_code.as_deref(), Some("FRAUD"));
    let target_in_payload = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access' \
               AND before_after->'targetScope'->>'tenantId'='{target}'"
        ),
    )
    .await;
    assert_eq!(
        target_in_payload, 1,
        "the forensic record's before_after names the target tenant"
    );
    // No record under the target tenant (the actor's HOME owns the record).
    let under_target = count(
        &raw,
        &format!("SELECT COUNT(*) FROM bss.secured_audit_record WHERE tenant_id='{target}'"),
    )
    .await;
    assert_eq!(under_target, 0, "the record is owned by the home tenant");
}

/// (c) A cross-tenant resolve with NO reason is `MissingInvestigationReason`
/// BEFORE any read or write — no forensic record.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_resolve_without_reason_is_rejected_no_record() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;
    let home = Uuid::now_v7();
    let target = Uuid::now_v7();

    let err = provider
        .transaction(move |txn| {
            Box::pin(async move {
                CrossTenantGateway::new()
                    .resolve_read_scope(
                        txn,
                        home,
                        Some(TargetScope { tenant_id: target }),
                        true,
                        "investigator-7",
                        None, // no X-Investigation-Reason
                        None, // no reasonCode
                        None,
                    )
                    .await
            })
        })
        .await
        .expect_err("a reason-less cross-tenant resolve must be rejected");
    assert!(
        err.to_string().contains("MissingInvestigationReason"),
        "expected MissingInvestigationReason sentinel, got: {err}"
    );

    let records = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(records, 0, "no forensic record on a reason-less rejection");
}

/// (d) A cross-tenant resolve with an unauthorized role is
/// `CrossTenantAccessDenied` — no forensic record.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_resolve_without_role_is_denied_no_record() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;
    let home = Uuid::now_v7();
    let target = Uuid::now_v7();

    let err = provider
        .transaction(move |txn| {
            Box::pin(async move {
                CrossTenantGateway::new()
                    .resolve_read_scope(
                        txn,
                        home,
                        Some(TargetScope { tenant_id: target }),
                        false, // role NOT authorized
                        "investigator-7",
                        Some("reason"),
                        Some("FRAUD"),
                        None,
                    )
                    .await
            })
        })
        .await
        .expect_err("an unauthorized cross-tenant resolve must be denied");
    assert!(
        err.to_string().contains("CrossTenantAccessDenied"),
        "expected CrossTenantAccessDenied sentinel, got: {err}"
    );

    let records = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(records, 0, "no forensic record on a role denial");
}

/// API-surface reads: a posted entry's audit dims are retrievable (who/when/
/// source/correlation), and a tamper-status read reflects an inserted freeze.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn audit_retrieval_and_tamper_status_reads() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider) = setup(&url).await;
    let reader = AuditRetrievalReader::new(provider.clone());

    let tenant = Uuid::now_v7();
    let entry_id = Uuid::now_v7();
    let actor = Uuid::now_v7();
    let correlation = Uuid::now_v7();
    let period = "202606";

    // Insert one journal_entry row directly (the audit reader is a pure read).
    // A line-less header would trip the DEFERRABLE balanced-entry constraint
    // trigger at commit; this test only needs a readable header, so disable that
    // trigger for the raw insert (the append-only guard still allows INSERT).
    raw.execute(pg(
        "ALTER TABLE bss.ledger_journal_entry DISABLE TRIGGER trg_journal_entry_balanced",
    ))
    .await
    .unwrap();
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_journal_entry \
           (entry_id, tenant_id, legal_entity_id, period_id, entry_currency, \
            source_doc_type, source_business_id, posted_at_utc, effective_at, \
            origin, posted_by_actor_id, correlation_id, rounding_evidence) \
         VALUES ('{entry_id}','{tenant}','{tenant}','{period}','USD', \
            'INVOICE_POST','INV-AUDIT', now(), '2026-06-01', \
            'SYSTEM','{actor}','{correlation}', '{{}}'::jsonb)"
    )))
    .await
    .unwrap();
    raw.execute(pg(
        "ALTER TABLE bss.ledger_journal_entry ENABLE TRIGGER trg_journal_entry_balanced",
    ))
    .await
    .unwrap();

    let scope = AccessScope::for_tenant(tenant);
    let record = reader
        .audit_entry(&scope, tenant, entry_id)
        .await
        .expect("audit_entry read")
        .expect("entry must be found");
    assert_eq!(record.posted_by_actor_id, actor, "who");
    assert_eq!(record.correlation_id, correlation, "correlation");
    assert_eq!(record.source_doc_type, "INVOICE_POST", "source doc type");
    assert_eq!(record.source_business_id, "INV-AUDIT", "source business id");
    assert_eq!(record.origin, "SYSTEM", "origin");

    // A foreign tenant scope yields None (SQL-level BOLA, no existence leak).
    let foreign = AccessScope::for_tenant(Uuid::now_v7());
    let none = reader
        .audit_entry(&foreign, tenant, entry_id)
        .await
        .expect("audit_entry read (foreign)");
    assert!(
        none.is_none(),
        "a foreign-scope read must not see the entry"
    );

    // Document history: the one entry shows up under its source key.
    let history = reader
        .document_history(&scope, tenant, "INVOICE_POST", "INV-AUDIT")
        .await
        .expect("document_history read");
    assert_eq!(history.len(), 1, "the document's single entry");
    assert_eq!(history[0].entry_id, entry_id);

    // Tamper-status: clean (no freeze) ⇒ verified, not frozen.
    let clean = provider
        .transaction({
            let reader = reader.clone();
            let scope = scope.clone();
            move |txn| {
                let reader = reader.clone();
                let scope = scope.clone();
                Box::pin(async move {
                    reader
                        .tamper_status_in_txn(txn, &scope, tenant)
                        .await
                        .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
                })
            }
        })
        .await
        .expect("tamper-status (clean)");
    assert!(!clean.scope_frozen, "no freeze ⇒ not frozen");
    assert!(clean.verified, "no freeze ⇒ verified (MVP)");

    // Insert an ACTIVE freeze, then re-read: frozen + not verified + one row.
    raw.execute(pg(format!(
        "INSERT INTO bss.scope_freeze \
           (tenant_id, scope, period_id, reason, frozen_at, set_by) \
         VALUES ('{tenant}','tenant','ALL','broken chain', now(), 'verifier')"
    )))
    .await
    .unwrap();

    let frozen = provider
        .transaction({
            let reader = reader.clone();
            let scope = scope.clone();
            move |txn| {
                let reader = reader.clone();
                let scope = scope.clone();
                Box::pin(async move {
                    reader
                        .tamper_status_in_txn(txn, &scope, tenant)
                        .await
                        .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
                })
            }
        })
        .await
        .expect("tamper-status (frozen)");
    assert!(frozen.scope_frozen, "an active freeze ⇒ scope_frozen");
    assert!(!frozen.verified, "an active freeze ⇒ not verified");
    assert_eq!(frozen.freezes.len(), 1, "one freeze row");
    assert_eq!(frozen.freezes[0].reason, "broken chain");
}
