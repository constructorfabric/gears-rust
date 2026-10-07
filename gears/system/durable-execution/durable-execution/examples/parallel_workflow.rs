#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Fan-out: build a monthly customer report from several sources and regions.
//!
//! Mechanism: a heterogeneous parallel tuple (billing and usage are queried
//! concurrently and return different types), a join, then a dynamic list of
//! per-region export branches joined in declaration order. Every branch result
//! is its own checkpoint, so a retry re-runs only unfinished branches.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example parallel_workflow`
mod support;
use durable_execution_sdk::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ReportRequest {
    customer_id: String,
    month: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct UsageSummary {
    api_calls: u64,
    storage_gb: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ReportHeader {
    billed_cents: u64,
    api_calls: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ShardFile {
    region: String,
    rows: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Report {
    billed_cents: u64,
    rows_total: u64,
    files: Vec<String>,
}

const REGIONS: [(&str, u64); 3] = [("eu", 120), ("us", 80), ("apac", 40)];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let builder = WorkflowBuilder::<ReportRequest>::new("reports.monthly.v1").parallel((
            support::step("billing_total", |_, request: ReportRequest| async move {
                // Stand-in for a billing service query.
                Ok(
                    match (request.customer_id.as_str(), request.month.as_str()) {
                        ("cust-42", "2026-09") => 12_345_u64,
                        _ => 0,
                    },
                )
            }),
            support::step("usage_summary", |_, _: ReportRequest| async move {
                Ok(UsageSummary {
                    api_calls: 9_000,
                    storage_gb: 42,
                })
            }),
        ));
        let (billing, usage) = builder.checkpoint().branches().expect("tuple stage");
        let builder = builder.then(support::step(
            "build_header",
            |_, (billed_cents, usage): (u64, UsageSummary)| async move {
                Ok(ReportHeader {
                    billed_cents,
                    api_calls: usage.api_calls,
                })
            },
        ));
        let header = builder.checkpoint();
        let header_dependency = header.clone();
        let builder = builder.parallel(
            REGIONS
                .iter()
                .map(|&(region, rows)| {
                    support::step(
                        &format!("export_{region}"),
                        move |_, _: ReportHeader| async move {
                            Ok(ShardFile {
                                region: region.to_owned(),
                                rows,
                            })
                        },
                    )
                })
                .collect::<Vec<_>>(),
        );
        let exports = builder.checkpoint().branches().expect("list stage");
        let workflow = builder
            .then(
                support::step("publish_report", move |ctx, shards: Vec<ShardFile>| {
                    let header = header.clone();
                    async move {
                        Ok(Report {
                            billed_cents: ctx.checkpoint(&header)?.billed_cents,
                            rows_total: shards.iter().map(|s| s.rows).sum(),
                            files: shards
                                .iter()
                                .map(|s| format!("report-2026-09-{}.csv", s.region))
                                .collect(),
                        })
                    }
                })
                .uses(&header_dependency),
            )
            .build()?;
        let reference = workflow.reference();
        app.registry.register(workflow.into_definition()).await?;

        let started = app
            .client
            .start(
                &app.owner,
                &reference,
                ReportRequest {
                    customer_id: "cust-42".into(),
                    month: "2026-09".into(),
                },
                StartOptions::default(),
            )
            .await?;
        app.wait(started.run_id).await?;

        let report = app
            .client
            .result(&app.owner, started.run_id, &reference)
            .await?;
        assert_eq!(report.rows_total, 240);
        assert_eq!(report.billed_cents, 12_345);
        assert_eq!(
            report.files,
            [
                "report-2026-09-eu.csv",
                "report-2026-09-us.csv",
                "report-2026-09-apac.csv"
            ]
        );
        assert_eq!(
            app.client
                .step_result(&app.owner, started.run_id, &billing)
                .await?,
            12_345
        );
        assert_eq!(
            app.client
                .step_result(&app.owner, started.run_id, &usage)
                .await?
                .storage_gb,
            42
        );
        for (branch, (region, rows)) in exports.iter().zip(REGIONS) {
            let shard = app
                .client
                .step_result(&app.owner, started.run_id, branch)
                .await?;
            assert_eq!((shard.region.as_str(), shard.rows), (region, rows));
        }
        Ok(())
    })
    .await
}
