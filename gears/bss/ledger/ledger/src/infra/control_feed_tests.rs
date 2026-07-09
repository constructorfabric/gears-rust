use bss_ledger_sdk::{
    BillRunFinishedV1, IssuedInvoiceManifest, IssuedInvoiceManifestV1, PspSettlementFeedV1,
    PspSettlementReport,
};
use uuid::Uuid;

use super::InProcessControlFeeds;

fn manifest() -> IssuedInvoiceManifest {
    IssuedInvoiceManifest {
        invoice_ids: vec!["inv-1".to_owned(), "inv-2".to_owned()],
        count: 2,
        gross_total_minor: 4_200,
    }
}

fn report() -> PspSettlementReport {
    PspSettlementReport {
        report_id: "psp-rep-1".to_owned(),
        settled_minor: 9_900,
        currency: "EUR".to_owned(),
    }
}

// --- empty store: every read is None (the inert-until-the-feed-lands contract) ---

#[tokio::test]
async fn empty_store_manifest_is_none() {
    let feeds = InProcessControlFeeds::new();
    assert_eq!(
        feeds
            .latest_manifest(Uuid::now_v7(), "2026-06")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn empty_store_bill_run_is_none() {
    let feeds = InProcessControlFeeds::new();
    assert_eq!(
        feeds.is_finished(Uuid::now_v7(), "2026-06").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn empty_store_psp_report_is_none() {
    let feeds = InProcessControlFeeds::new();
    assert_eq!(
        feeds
            .settlement_report(Uuid::now_v7(), "2026-06")
            .await
            .unwrap(),
        None
    );
}

// --- ingest then read returns exactly the value pushed ---

#[tokio::test]
async fn ingest_then_read_manifest() {
    let feeds = InProcessControlFeeds::new();
    let tenant = Uuid::now_v7();
    feeds.ingest_manifest(tenant, "2026-06", manifest());
    assert_eq!(
        feeds.latest_manifest(tenant, "2026-06").await.unwrap(),
        Some(manifest())
    );
}

#[tokio::test]
async fn ingest_then_read_bill_run() {
    let feeds = InProcessControlFeeds::new();
    let tenant = Uuid::now_v7();
    feeds.ingest_bill_run_finished(tenant, "2026-06", true);
    assert_eq!(
        feeds.is_finished(tenant, "2026-06").await.unwrap(),
        Some(true)
    );
}

#[tokio::test]
async fn ingest_then_read_psp_report() {
    let feeds = InProcessControlFeeds::new();
    let tenant = Uuid::now_v7();
    feeds.ingest_psp_report(tenant, "2026-06", report());
    assert_eq!(
        feeds.settlement_report(tenant, "2026-06").await.unwrap(),
        Some(report())
    );
}

// --- last writer wins (the feed is a snapshot, not an append log) ---

#[tokio::test]
async fn ingest_overwrites_prior_manifest() {
    let feeds = InProcessControlFeeds::new();
    let tenant = Uuid::now_v7();
    feeds.ingest_manifest(tenant, "2026-06", manifest());
    let newer = IssuedInvoiceManifest {
        invoice_ids: vec!["inv-9".to_owned()],
        count: 1,
        gross_total_minor: 100,
    };
    feeds.ingest_manifest(tenant, "2026-06", newer.clone());
    assert_eq!(
        feeds.latest_manifest(tenant, "2026-06").await.unwrap(),
        Some(newer)
    );
}

// --- per-(tenant, period) isolation: a push for one key never bleeds into another ---

#[tokio::test]
async fn manifest_is_isolated_per_tenant() {
    let feeds = InProcessControlFeeds::new();
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    feeds.ingest_manifest(a, "2026-06", manifest());
    assert_eq!(
        feeds.latest_manifest(a, "2026-06").await.unwrap(),
        Some(manifest())
    );
    assert_eq!(feeds.latest_manifest(b, "2026-06").await.unwrap(), None);
}

#[tokio::test]
async fn manifest_is_isolated_per_period() {
    let feeds = InProcessControlFeeds::new();
    let tenant = Uuid::now_v7();
    feeds.ingest_manifest(tenant, "2026-06", manifest());
    assert_eq!(
        feeds.latest_manifest(tenant, "2026-06").await.unwrap(),
        Some(manifest())
    );
    assert_eq!(
        feeds.latest_manifest(tenant, "2026-07").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn bill_run_is_isolated_per_tenant_period() {
    let feeds = InProcessControlFeeds::new();
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    feeds.ingest_bill_run_finished(a, "2026-06", true);
    assert_eq!(feeds.is_finished(a, "2026-06").await.unwrap(), Some(true));
    // Same tenant, other period and other tenant, same period: both untouched.
    assert_eq!(feeds.is_finished(a, "2026-07").await.unwrap(), None);
    assert_eq!(feeds.is_finished(b, "2026-06").await.unwrap(), None);
}

#[tokio::test]
async fn psp_report_is_isolated_per_tenant() {
    let feeds = InProcessControlFeeds::new();
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    feeds.ingest_psp_report(a, "2026-06", report());
    assert_eq!(
        feeds.settlement_report(a, "2026-06").await.unwrap(),
        Some(report())
    );
    assert_eq!(feeds.settlement_report(b, "2026-06").await.unwrap(), None);
}
