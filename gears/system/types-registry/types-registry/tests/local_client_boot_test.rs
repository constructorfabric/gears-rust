#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The local `ClientHub` handoff through the real host lifecycle (T31 slice 3).
//!
//! The production registry gear and a fixture consumer run under `HostRuntime`:
//! migration → registry init and seed → consumer init, before `post_init`. The
//! consumer resolves the platform API from `ClientHub` in its own `init()` and
//! reconciles documents that depend on a seeded, configured type.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::GearCtx;
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{DatabaseCapability, Gear, RunnableCapability, SystemCapability};
use toolkit::lifecycle::Status;
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityKey, EntityLookup, FieldSelection,
    GtsId, GtsTypeId, Origin, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection,
    PublisherContext, ReadOptions, ReconcileOptions, ReconcileOutcome, ReconcileReport,
    Reconciliation, TypesRegistryApi, TypesRegistryApiExt, TypesRegistryClient,
};

use types_registry_sdk::models::MAX_BATCH_GET_KEYS;

mod common;

const REGISTRY: &str = "types-registry";
const CONSUMER: &str = "boot-consumer";

/// Configured in the registry: the base every consumer document depends on.
const BASE: &str = "gts.cf.core.boottest.base.v1~";
const WIDGET: &str = "gts.cf.core.boottest.base.v1~cf.core.boottest.widget.v1~";

/// Generous: only the failure cases are expected to spend any of it.
const HOST_TIMEOUT: Duration = Duration::from_secs(60);

fn base() -> Value {
    json!({
        "$id": format!("gts://{BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {"name": {"type": "string"}}
    })
}

fn derived(gts_id: &str, parent: &str, description: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": description,
        "type": "object",
        "allOf": [{"$ref": format!("gts://{parent}")}]
    })
}

/// What the fixture consumer saw in its `init()`.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent observations of one boot"
)]
struct Report {
    up_to_date: bool,
    /// The platform read of the first declaration after reconciling.
    resource_version: u64,
    /// The tenant API read the same content from the same hub.
    tenant_parity: bool,
    /// The legacy client does not see what the new API wrote.
    legacy_sees_it: bool,
}

/// What the fixture gear does in its `init()`.
enum Behavior {
    /// Only report that it ran: the registry's `init()` has completed.
    Observe,
    /// Read every expected seed the moment the platform API is resolvable.
    Verify(Vec<(GtsId, Value)>),
    /// Publish owned documents through the platform API.
    Publish {
        declarations: Vec<(GtsId, Value)>,
        version: &'static str,
        deadline: Duration,
    },
}

/// A gear depending on the registry, run after its `init()`.
struct Consumer {
    behavior: Behavior,
    reports: mpsc::UnboundedSender<Result<Option<Report>, String>>,
}

impl Consumer {
    async fn act(&self, ctx: &GearCtx) -> Result<Option<Report>, String> {
        match &self.behavior {
            Behavior::Observe => Ok(None),
            Behavior::Verify(expected) => verify(ctx, expected).await.map(|()| None),
            Behavior::Publish {
                declarations,
                version,
                deadline,
            } => publish(ctx, declarations, version, *deadline)
                .await
                .map(Some),
        }
    }
}

async fn publish(
    ctx: &GearCtx,
    declarations: &[(GtsId, Value)],
    version: &str,
    deadline: Duration,
) -> Result<Report, String> {
    {
        let hub = ctx.client_hub();
        let platform = hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| format!("resolve the platform API: {e}"))?;
        let mut options = ReconcileOptions::default();
        options.deadline = deadline;
        let publisher = PublisherContext {
            name: CONSUMER.to_owned(),
            version: version.parse().expect("publisher version"),
        };
        let reconciled = platform
            .reconcile_entities_and_await(
                &PlatformSecurityContext::outbound_marker(),
                &publisher,
                declarations,
                &options,
                ctx.cancellation_token(),
            )
            .await
            .map_err(|e| format!("reconcile: {}", explain(&e)))?;
        // The startup gate: up to date, or every desired identifier admitted. An
        // accepted operation or a bare `Reconciled` is not enough on its own.
        let up_to_date = match reconciled {
            Reconciliation::UpToDate => true,
            Reconciliation::Reconciled(outcomes) => {
                for (id, _) in declarations {
                    match outcomes.get(id) {
                        Some(ReconcileOutcome::Admitted) => {}
                        Some(ReconcileOutcome::Rejected(cause)) => {
                            return Err(format!("{id} rejected: {cause}"));
                        }
                        Some(ReconcileOutcome::Pending(cause)) => {
                            return Err(format!("{id} pending: {cause:?}"));
                        }
                        None => return Err(format!("{id} has no outcome")),
                    }
                }
                false
            }
        };

        let (first, content) = &declarations[0];
        let type_id = GtsTypeId::try_new(first.id()).expect("a Type Schema identifier");
        let read_options = ReadOptions {
            projection: Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::Origin,
            ])),
            ..ReadOptions::default()
        };
        let read = platform
            .get_type_schema(
                &PlatformSecurityContext::outbound_marker(),
                &type_id,
                read_options.clone(),
            )
            .await
            .map_err(|e| format!("platform read: {e}"))?;
        let Some(Origin::Managed {
            resource_version, ..
        }) = read.origin
        else {
            return Err("the platform read carries no managed origin".to_owned());
        };
        let tenant = hub
            .get::<dyn TypesRegistryApi>()
            .map_err(|e| format!("resolve the tenant API: {e}"))?;
        let tenant_read = tenant
            .get_type_schema(&SecurityContext::anonymous(), &type_id, read_options)
            .await
            .map_err(|e| format!("tenant read: {e}"))?;
        let legacy = hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| format!("resolve the legacy client: {e}"))?;
        Ok(Report {
            up_to_date,
            resource_version,
            tenant_parity: tenant_read.content.as_ref() == Some(content)
                && tenant_read.content == read.content,
            legacy_sees_it: legacy.get_type_schema(first.id()).await.is_ok(),
        })
    }
}

/// Every expected seed is stored once with its document by the time a dependent gear
/// first resolves the platform API, read in pages the batch read admits.
async fn verify(ctx: &GearCtx, expected: &[(GtsId, Value)]) -> Result<(), String> {
    let platform = ctx
        .client_hub()
        .get::<dyn PlatformTypesRegistryApi>()
        .map_err(|e| format!("resolve the platform API: {e}"))?;
    let mut problems = Vec::new();
    for page in expected.chunks(MAX_BATCH_GET_KEYS) {
        let request = BatchGetEntitiesRequest {
            items: page
                .iter()
                .map(|(id, _)| BatchGetItem::from(EntityKey::GtsId(id.clone())))
                .collect(),
            projection: Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::Origin,
            ])),
            fresh: true,
        };
        let mut answers = platform
            .batch_get_entities(&PlatformSecurityContext::outbound_marker(), request)
            .await
            .map_err(|e| format!("batch read: {e}"))?;
        for (id, content) in page {
            match answers.0.remove(&EntityKey::GtsId(id.clone())) {
                Some(EntityLookup::Found { entity, .. })
                    if matches!(
                        entity.origin,
                        Some(Origin::Managed {
                            resource_version: 1,
                            ..
                        })
                    ) && entity.content.as_ref() == Some(content) => {}
                other => problems.push(format!("{id}: {other:?}")),
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("not seeded: {}", problems.join("; ")))
    }
}

/// Name every identifier that did not settle as admitted, and why.
fn explain(error: &toolkit_canonical_errors::CanonicalError) -> String {
    let Some(report) = ReconcileReport::from_canonical(error) else {
        return error.to_string();
    };
    let mut unsettled: Vec<String> = report
        .outcomes
        .iter()
        .filter_map(|(id, outcome)| match outcome {
            ReconcileOutcome::Admitted => None,
            ReconcileOutcome::Rejected(cause) => Some(format!("{id} rejected: {cause}")),
            ReconcileOutcome::Pending(cause) => Some(format!("{id} pending: {cause:?}")),
        })
        .collect();
    unsettled.sort();
    format!("stopped ({:?}): {}", report.stop, unsettled.join("; "))
}

#[async_trait]
impl Gear for Consumer {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let report = self.act(ctx).await;
        let failure = report.as_ref().err().cloned();
        // The receiver outlives every boot; a closed channel only means the test ended.
        drop(self.reports.send(report));
        match failure {
            None => Ok(()),
            Some(reason) => anyhow::bail!("{CONSUMER}: {reason}"),
        }
    }
}

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

/// One host: the registry, optionally with a database and a consumer.
struct Host<'d> {
    dir: &'d common::TestDir,
    database: bool,
    registry_config: Value,
    behavior: Behavior,
    cancelled: bool,
}

struct Outcome {
    /// The lifecycle's result: `Ok` after a healthy boot was cancelled.
    boot: anyhow::Result<()>,
    report: Option<Result<Option<Report>, String>>,
    hub: Arc<ClientHub>,
}

impl Host<'_> {
    fn new(dir: &common::TestDir) -> Host<'_> {
        Host {
            dir,
            database: true,
            registry_config: json!({ "entities": [base()] }),
            behavior: Behavior::Observe,
            cancelled: false,
        }
    }

    async fn run(self) -> Outcome {
        let dsn = format!(
            "sqlite://{}?mode=rwc",
            self.dir.path().join("boot.db").display()
        );
        let database = json!({ "dsn": dsn, "pool": { "max_conns": 4 } });
        let mut registry_entry = json!({ "config": self.registry_config });
        if self.database {
            registry_entry["database"] = database.clone();
        }
        let figment = figment::Figment::new().merge(figment::providers::Serialized::defaults(
            json!({ "gears": { REGISTRY: registry_entry.clone() } }),
        ));
        let db_manager = Arc::new(
            toolkit_db::DbManager::from_figment(figment, self.dir.path().to_path_buf())
                .expect("database manager"),
        );

        let mut builder = RegistryBuilder::default();
        // As `#[toolkit::gear]` registers it, minus REST: this fixture has no REST host.
        let lifecycle = Arc::new(TypesRegistryGear::default().into_gear());
        let gear = lifecycle.inner_arc();
        builder.register_core_with_meta(REGISTRY, &[], gear.clone() as Arc<dyn Gear>);
        builder.register_db_with_meta(REGISTRY, gear.clone() as Arc<dyn DatabaseCapability>);
        builder.register_system_with_meta(REGISTRY, gear as Arc<dyn SystemCapability>);
        builder.register_stateful_with_meta(
            REGISTRY,
            Arc::clone(&lifecycle) as Arc<dyn RunnableCapability>,
        );
        let (reports, mut received) = mpsc::unbounded_channel();
        builder.register_core_with_meta(
            CONSUMER,
            &[REGISTRY],
            Arc::new(Consumer {
                behavior: self.behavior,
                reports,
            }) as Arc<dyn Gear>,
        );
        let registry = builder.build_topo_sorted().expect("registry");

        let hub = Arc::new(ClientHub::default());
        let cancel = CancellationToken::new();
        if self.cancelled {
            cancel.cancel();
        }
        let config = Config(HashMap::from([
            (REGISTRY.to_owned(), registry_entry),
            (CONSUMER.to_owned(), json!({ "config": {} })),
        ]));
        let runtime = HostRuntime::new(
            registry,
            Arc::new(config),
            DbOptions::Manager(db_manager),
            Arc::clone(&hub),
            cancel.clone(),
            uuid::Uuid::new_v4(),
            None,
        );
        let mut host = tokio::spawn(runtime.run_gear_phases());

        // A healthy host runs until cancelled: wait for the consumer's report, or
        // for the lifecycle to end on its own (a boot failure).
        let wait = async {
            tokio::select! {
                report = received.recv() => {
                    if matches!(report, Some(Ok(_))) {
                        // The rest of the boot: `post_init`, then the registry's
                        // runnable reaches `Running` before the host is stopped.
                        common::await_delivery("the registry runnable starts", || async {
                            (lifecycle.status() == Status::Running).then_some(())
                        })
                        .await;
                        cancel.cancel();
                        let boot = host.await.expect("host task");
                        assert_eq!(lifecycle.status(), Status::Stopped, "the registry stopped");
                        (report, boot)
                    } else {
                        // A failed consumer fails the boot on its own.
                        (report, host.await.expect("host task"))
                    }
                }
                boot = &mut host => (received.try_recv().ok(), boot.expect("host task")),
            }
        };
        let (report, boot) = tokio::time::timeout(HOST_TIMEOUT, wait)
            .await
            .expect("the host must boot or fail within the timeout");
        Outcome { boot, report, hub }
    }
}

fn widget(description: &str) -> Vec<(GtsId, Value)> {
    vec![(
        GtsId::try_new(WIDGET).unwrap(),
        derived(WIDGET, BASE, description),
    )]
}

fn publishing(declarations: Vec<(GtsId, Value)>, version: &'static str) -> Behavior {
    Behavior::Publish {
        declarations,
        version,
        deadline: Duration::from_secs(20),
    }
}

/// The consumer's report of a healthy boot.
fn published(outcome: Outcome) -> Report {
    outcome.boot.expect("a healthy boot");
    outcome
        .report
        .expect("the consumer ran")
        .expect("it published")
        .expect("it was asked to publish")
}

#[tokio::test]
async fn first_boot_restart_and_update_through_the_platform_api() {
    let dir = common::TestDir::new("tr-boot-lifecycle");

    let mut first = Host::new(&dir);
    first.behavior = publishing(widget("first"), "1.0.0");
    let report = published(first.run().await);
    assert!(!report.up_to_date, "the first boot creates the widget");
    assert_eq!(report.resource_version, 1);
    assert!(
        report.tenant_parity,
        "the tenant API reads what the platform API reads"
    );
    assert!(
        !report.legacy_sees_it,
        "the legacy catalogue is independent"
    );

    let mut restart = Host::new(&dir);
    restart.behavior = publishing(widget("first"), "1.0.0");
    let report = published(restart.run().await);
    assert!(report.up_to_date, "an equal restart submits nothing");
    assert_eq!(report.resource_version, 1);

    let mut update = Host::new(&dir);
    update.behavior = publishing(widget("second"), "1.1.0");
    let report = published(update.run().await);
    assert!(!report.up_to_date);
    assert_eq!(
        report.resource_version, 2,
        "the update revises the widget once"
    );
}

#[tokio::test]
async fn a_registry_without_a_database_publishes_neither_api() {
    let dir = common::TestDir::new("tr-boot-no-db");

    let mut legacy_only = Host::new(&dir);
    legacy_only.database = false;
    let legacy_only = legacy_only.run().await;
    legacy_only.boot.expect("a legacy-only host still boots");
    assert!(
        matches!(legacy_only.report, Some(Ok(None))),
        "the registry's init completed"
    );
    assert!(
        legacy_only
            .hub
            .try_get::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
    assert!(legacy_only.hub.try_get::<dyn TypesRegistryApi>().is_none());
    assert!(
        legacy_only
            .hub
            .try_get::<dyn TypesRegistryClient>()
            .is_some()
    );

    let mut with_consumer = Host::new(&dir);
    with_consumer.database = false;
    with_consumer.behavior = publishing(widget("first"), "1.0.0");
    let with_consumer = with_consumer.run().await;
    let error = with_consumer
        .boot
        .expect_err("the consumer cannot resolve the API");
    assert!(
        format!("{error:#}").contains("resolve the platform API"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_seed_over_the_default_batch_limit_is_stored_whole_before_the_api_is_published() {
    let dir = common::TestDir::new("tr-boot-many");
    let mut entities = vec![(GtsId::try_new(BASE).unwrap(), base())];
    entities.extend(base_instances(150));
    let documents: Vec<Value> = entities.iter().map(|(_, doc)| doc.clone()).collect();
    let mut host = Host::new(&dir);
    // The default `limits.batch_candidates` (100) is below the configured set alone.
    host.registry_config = json!({ "entities": documents });
    host.behavior = Behavior::Verify(entities);

    let outcome = host.run().await;

    outcome
        .boot
        .expect("the seed fits no single batch and still boots");
    assert!(
        matches!(outcome.report, Some(Ok(None))),
        "the consumer found every seed on its first read: {:?}",
        outcome.report
    );
}

/// `count` instances of the configured base, by identifier.
fn base_instances(count: usize) -> Vec<(GtsId, Value)> {
    (0..count)
        .map(|n| {
            let id = format!("{BASE}cf.core.boottest.i{n:03}.v1");
            let document = json!({"id": id, "name": format!("instance {n}")});
            (GtsId::try_new(&id).unwrap(), document)
        })
        .collect()
}

/// One count from the registry's tables, read after the host is gone.
async fn count(dir: &common::TestDir, sql: &str) -> i64 {
    let path = dir.path().join("boot.db");
    let db = Database::connect(format!("sqlite://{}?mode=ro", path.display()))
        .await
        .expect("open the boot database");
    db.query_one_raw(Statement::from_string(
        db.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .expect("count")
    .expect("one row")
    .try_get_by_index::<i64>(0)
    .expect("a count")
}

#[tokio::test]
async fn a_seed_failing_in_its_last_batch_publishes_no_api_though_earlier_batches_stored() {
    let dir = common::TestDir::new("tr-boot-many-orphan");
    // Sorts after every linked and configured identifier: its batch goes out last.
    let orphan = "gts.cf.zz.boottest.missing.v1~cf.zz.boottest.orphan.v1~";
    let mut documents = vec![base()];
    documents.extend(base_instances(150).into_iter().map(|(_, doc)| doc));
    documents.push(derived(orphan, "gts.cf.zz.boottest.missing.v1~", "orphan"));
    let mut host = Host::new(&dir);
    host.registry_config = json!({ "entities": documents });
    host.behavior = Behavior::Observe;

    let outcome = host.run().await;

    let error = format!("{:#}", outcome.boot.expect_err("the orphan fails boot"));
    assert!(
        error.contains(orphan) && error.contains("dependency_not_found"),
        "{error}"
    );
    assert!(outcome.report.is_none(), "the consumer never initializes");
    assert!(
        outcome
            .hub
            .try_get::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
    assert!(outcome.hub.try_get::<dyn TypesRegistryApi>().is_none());
    // Not an all-or-nothing refusal: the earlier batches are durable, and only the
    // barrier keeps them from being served.
    assert!(
        count(
            &dir,
            "SELECT COUNT(DISTINCT operation_id) FROM types_registry__operation_item"
        )
        .await
            >= 2
    );
    assert_eq!(
        count(
            &dir,
            "SELECT COUNT(*) FROM types_registry__entity \
             WHERE gts_id LIKE 'gts.cf.core.boottest.%'"
        )
        .await,
        151
    );
    assert_eq!(
        count(
            &dir,
            &format!("SELECT COUNT(*) FROM types_registry__entity WHERE gts_id = '{orphan}'")
        )
        .await,
        0
    );
}

#[tokio::test]
async fn an_invalid_seed_fails_boot_before_any_api_is_published() {
    let dir = common::TestDir::new("tr-boot-invalid-seed");
    let orphan = "gts.cf.core.boottest.missing.v1~cf.core.boottest.orphan.v1~";
    let mut host = Host::new(&dir);
    host.registry_config = json!({
        "entities": [derived(orphan, "gts.cf.core.boottest.missing.v1~", "orphan")]
    });
    host.behavior = publishing(widget("first"), "1.0.0");

    let outcome = host.run().await;

    let error = format!(
        "{:#}",
        outcome.boot.expect_err("an unresolved seed fails boot")
    );
    assert!(
        error.contains(orphan) && error.contains("dependency_not_found"),
        "{error}"
    );
    assert!(outcome.report.is_none(), "the consumer never initializes");
    assert!(
        outcome
            .hub
            .try_get::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
    assert!(outcome.hub.try_get::<dyn TypesRegistryApi>().is_none());
}

#[tokio::test]
async fn a_consumer_declaration_with_a_missing_dependency_fails_its_boot_within_budget() {
    let dir = common::TestDir::new("tr-boot-missing-dependency");
    let stray = "gts.cf.core.boottest.absent.v1~cf.core.boottest.stray.v1~";
    let mut host = Host::new(&dir);
    host.behavior = Behavior::Publish {
        declarations: vec![(
            GtsId::try_new(stray).unwrap(),
            derived(stray, "gts.cf.core.boottest.absent.v1~", "stray"),
        )],
        version: "1.0.0",
        deadline: Duration::from_secs(3),
    };

    let started = tokio::time::Instant::now();
    let outcome = host.run().await;

    let error = format!(
        "{:#}",
        outcome.boot.expect_err("a pending dependency fails boot")
    );
    assert!(error.contains(CONSUMER) && error.contains(stray), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "it never waits for a later registrant"
    );
}

#[tokio::test]
async fn a_rejected_consumer_declaration_fails_its_boot_naming_it() {
    let dir = common::TestDir::new("tr-boot-rejected");
    let mut invalid = derived(WIDGET, BASE, "bad dialect");
    invalid["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    let mut host = Host::new(&dir);
    host.behavior = publishing(vec![(GtsId::try_new(WIDGET).unwrap(), invalid)], "1.0.0");

    let outcome = host.run().await;

    let error = format!(
        "{:#}",
        outcome.boot.expect_err("a rejected declaration fails boot")
    );
    assert!(
        error.contains(CONSUMER) && error.contains(WIDGET),
        "{error}"
    );
}

#[tokio::test]
async fn an_expired_seed_budget_fails_boot() {
    let dir = common::TestDir::new("tr-boot-deadline");
    let mut host = Host::new(&dir);
    host.registry_config = json!({
        "entities": [base()],
        "worker": { "operation_timeout": "1ns" }
    });

    let outcome = host.run().await;

    let error = format!("{:#}", outcome.boot.expect_err("no budget, no boot"));
    assert!(error.contains("did not finish within"), "{error}");
    assert!(
        outcome
            .hub
            .try_get::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
}

#[tokio::test]
async fn a_cancelled_startup_publishes_no_api() {
    let dir = common::TestDir::new("tr-boot-cancelled");
    let mut host = Host::new(&dir);
    host.cancelled = true;

    let outcome = host.run().await;

    assert!(
        outcome
            .hub
            .try_get::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
    assert!(outcome.hub.try_get::<dyn TypesRegistryApi>().is_none());
}

/// The SDK README's gear example, run against the ordinary registry: its document
/// needs no configured base and no registration policy, and a restart is a no-op.
#[tokio::test]
async fn the_documented_example_publishes_and_restarts() {
    const EXAMPLE: &str = "gts.cf.example.events.user_created.v1~";
    let documented = || {
        vec![(
            GtsId::try_new(EXAMPLE).unwrap(),
            json!({
                "$id": format!("gts://{EXAMPLE}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object"
            }),
        )]
    };
    let dir = common::TestDir::new("tr-boot-readme");

    let mut cold = Host::new(&dir);
    cold.registry_config = json!({});
    cold.behavior = publishing(documented(), "1.0.0");
    let report = published(cold.run().await);
    assert!(!report.up_to_date);
    assert_eq!(report.resource_version, 1);

    let mut restart = Host::new(&dir);
    restart.registry_config = json!({});
    restart.behavior = publishing(documented(), "1.0.0");
    let report = published(restart.run().await);
    assert!(report.up_to_date, "an equal restart submits nothing");
    assert_eq!(report.resource_version, 1);
}
