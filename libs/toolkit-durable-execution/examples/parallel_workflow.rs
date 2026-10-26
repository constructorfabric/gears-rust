//! Static parallel branches followed by a fan-out derived from each run input.
mod support;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use toolkit_durable_execution::prelude::*;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Region {
    name: String,
    rows: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Request {
    customer: String,
    regions: Vec<Region>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Header {
    billed: u64,
    usage: u64,
    regions: Vec<Region>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Shard {
    region: String,
    rows: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Report {
    billed: u64,
    rows: u64,
    files: Vec<String>,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let builder = WorkflowBuilder::<Request>::new("reports.monthly.v1").parallel((
            Step::new(
                "billing",
                Duration::from_secs(10),
                |_, request: Request| async move { Ok((12_345u64, request.regions)) },
            ),
            Step::new(
                "usage",
                Duration::from_secs(10),
                |_, _: Request| async move { Ok(9_000u64) },
            ),
        ));
        let (billing, _usage) = builder.checkpoint().branches();
        let builder = builder.then(Step::new(
            "header",
            Duration::from_secs(10),
            |_, ((billed, regions), usage): ((u64, Vec<Region>), u64)| async move {
                Ok(Header {
                    billed,
                    usage,
                    regions,
                })
            },
        ));
        let header = builder.checkpoint();
        let builder = builder
            .then(Step::new(
                "plan_exports",
                Duration::from_secs(10),
                |_, header: Header| async move { Ok(header.regions) },
            ))
            .for_each(
                Step::new(
                    "export",
                    Duration::from_secs(10),
                    |_, region: Region| async move {
                        Ok(Shard {
                            region: region.name,
                            rows: region.rows,
                        })
                    },
                ),
                FanOutOptions::default(),
            );
        let exports = builder.checkpoint();
        let workflow = builder
            // `with_deps` hands the earlier `header` checkpoint to the handler as a typed value.
            .then(Step::with_deps(
                "publish",
                Duration::from_secs(10),
                &header,
                |_, shards: Vec<Shard>, header: Header| async move {
                    Ok(Report {
                        billed: header.billed,
                        rows: shards.iter().map(|s| s.rows).sum(),
                        files: shards
                            .iter()
                            .map(|s| format!("report-{}.csv", s.region))
                            .collect(),
                    })
                },
            ))
            .build()?;
        let reference = app.registry.register_workflow(workflow).await?.reference;
        for regions in [
            vec![
                Region {
                    name: "eu".into(),
                    rows: 120,
                },
                Region {
                    name: "us".into(),
                    rows: 80,
                },
                Region {
                    name: "apac".into(),
                    rows: 40,
                },
            ],
            vec![],
        ] {
            let expected: Vec<_> = regions
                .iter()
                .map(|r| format!("report-{}.csv", r.name))
                .collect();
            let started = app
                .client
                .start(
                    &app.owner,
                    &reference,
                    Request {
                        customer: "cust-42".into(),
                        regions,
                    },
                    StartOptions::new(),
                )
                .await?;
            app.wait(started.run_id()).await?;
            let report = app.client.result(&app.owner, started.handle()).await?;
            assert_eq!(report.files, expected);
            assert_eq!(report.billed, 12_345);
            assert_eq!(
                app.client
                    .step_result(&app.owner, started.handle(), &billing)
                    .await?
                    .0,
                12_345
            );
            if report.files.is_empty() {
                assert_eq!(report.rows, 0);
                assert!(
                    app.client
                        .step_result(&app.owner, started.handle(), &exports)
                        .await?
                        .is_empty()
                );
            } else {
                assert_eq!(report.rows, 240);
                assert_eq!(
                    app.client
                        .step_result(&app.owner, started.handle(), &exports.branch(0))
                        .await?
                        .region,
                    "eu"
                );
            }
        }
        Ok(())
    })
    .await
}
