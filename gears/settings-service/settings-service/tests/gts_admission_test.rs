#![allow(
    clippy::expect_used,
    reason = "the fixture and its helpers are plain functions, which \
              `allow-expect-in-tests` does not reach; a failed expectation \
              there is the test failing"
)]
//! The gear's whole publication manifest, admitted by the real types registry.
//!
//! The production registry gear runs under `HostRuntime` over a `SQLite` file
//! (migration, init, seed), and a fixture gear that depends on it publishes
//! the Settings manifest from its `init` through the gear's own publication
//! descriptor: the setting-type base, the resource-type stubs, the
//! value-type catalogue and the permission catalogue. A double of the registry
//! accepts any document; this is the registry's validator saying yes to each.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cf_gears_settings_service::SettingsService;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::GearCtx;
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{DatabaseCapability, Gear, RunnableCapability, SystemCapability};
use toolkit::gts_publication::{GtsPublicationCapability as _, PreparedGtsPublication};
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit_security::PlatformSecurityContext;
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityKey, EntityLookup, FieldSelection,
    Origin, PlatformTypesRegistryApi, Projection,
};

const REGISTRY: &str = "types-registry";
const SETTINGS: &str = "settings-service";
const PUBLISHER: &str = "settings-publisher";

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

/// Every manifest document as the registry holds it: its resource version,
/// after checking the stored content is the published one.
async fn stored_versions(
    ctx: &GearCtx,
    documents: &[(gts::GtsId, Value)],
) -> Result<Vec<u64>, String> {
    let platform = ctx
        .client_hub()
        .get::<dyn PlatformTypesRegistryApi>()
        .map_err(|e| format!("resolve the platform API: {e}"))?;
    let request = BatchGetEntitiesRequest {
        items: documents
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
    let mut versions = Vec::with_capacity(documents.len());
    for (id, content) in documents {
        match answers.0.remove(&EntityKey::GtsId(id.clone())) {
            Some(EntityLookup::Found { entity, .. })
                if entity.content.as_ref() == Some(content) =>
            {
                let Some(Origin::Managed {
                    resource_version, ..
                }) = entity.origin
                else {
                    return Err(format!("{id}: no managed origin"));
                };
                versions.push(resource_version);
            }
            other => return Err(format!("{id} is not stored as published: {other:?}")),
        }
    }
    Ok(versions)
}

/// Publishes the Settings manifest twice from its `init`, as the runtime does
/// for the gear at two successive starts, and reports how many documents the
/// registry holds.
struct Publisher(mpsc::UnboundedSender<Result<usize, String>>);

impl Publisher {
    async fn publish_twice(ctx: &GearCtx) -> Result<usize, String> {
        let publication = PreparedGtsPublication::prepare(
            SettingsService::default().descriptor(),
            ctx.config_provider(),
            &ctx.client_hub(),
        )
        .map_err(|e| format!("prepare: {e:#}"))?;
        let documents = publication.gts_manifest().documents().to_vec();
        let deliver = || async {
            publication
                .publish(ctx.cancellation_token(), Arc::new(|_: &str| {}))
                .await
                .map_err(|e| format!("publish: {e:#}"))
        };
        deliver().await?;
        let first = stored_versions(ctx, &documents).await?;
        deliver().await?;
        let second = stored_versions(ctx, &documents).await?;
        if first != second {
            return Err(format!(
                "a repeat delivery wrote: {first:?} then {second:?}"
            ));
        }
        Ok(documents.len())
    }
}

#[async_trait]
impl Gear for Publisher {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let report = Self::publish_twice(ctx).await;
        let failure = report.as_ref().err().cloned();
        drop(self.0.send(report));
        failure.map_or(Ok(()), |reason| anyhow::bail!("{PUBLISHER}: {reason}"))
    }
}

#[tokio::test]
async fn the_real_registry_admits_the_whole_manifest_and_a_repeat_is_a_no_op() {
    let dir = std::env::temp_dir().join(format!("settings-gts-admission-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let dsn = format!("sqlite://{}?mode=rwc", dir.join("registry.db").display());
    let registry_entry =
        json!({ "config": {}, "database": { "dsn": dsn, "pool": { "max_conns": 4 } } });
    let figment = figment::Figment::new().merge(figment::providers::Serialized::defaults(
        json!({ "gears": { REGISTRY: registry_entry.clone() } }),
    ));
    let db_manager = Arc::new(
        toolkit_db::DbManager::from_figment(figment, dir.clone()).expect("database manager"),
    );

    // The registry as `#[toolkit::gear]` registers it, minus REST.
    let mut builder = RegistryBuilder::default();
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
        PUBLISHER,
        &[REGISTRY],
        Arc::new(Publisher(reports)) as Arc<dyn Gear>,
    );

    let cancel = CancellationToken::new();
    let config = Config(HashMap::from([
        (REGISTRY.to_owned(), registry_entry),
        (SETTINGS.to_owned(), json!({ "config": {} })),
        (PUBLISHER.to_owned(), json!({ "config": {} })),
    ]));
    let runtime = HostRuntime::new(
        builder.build_topo_sorted().expect("registry"),
        Arc::new(config),
        DbOptions::Manager(db_manager),
        Arc::new(ClientHub::default()),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    let mut host = tokio::spawn(runtime.run_gear_phases());
    let report = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::select! {
            report = received.recv() => report,
            boot = &mut host => panic!("the host ended before the publisher ran: {boot:?}"),
        }
    })
    .await
    .expect("the publisher reports within the timeout");
    cancel.cancel();
    drop(host.await);
    drop(std::fs::remove_dir_all(&dir));

    let published = report
        .expect("the publisher ran")
        .expect("every document is admitted");
    // The base, the resource-type stubs, the value-type catalogue and one
    // permission per enforced pair.
    assert_eq!(
        published,
        1 + 3 + settings_service_sdk::catalogue::CATALOGUE.len() + 12
    );
}
