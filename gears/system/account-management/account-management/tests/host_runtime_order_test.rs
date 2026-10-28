#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Local registry handoff through `HostRuntime`, without registry `deps` edges.
//!
//! Boot the linked account-management, resource-group, tenant-resolver,
//! authz-resolver and static `IdP` gears using their production registrations.
//! A consumer depending on AM must find the seeded registry APIs, admitted root
//! type, bound AM TR plugin and AM client in its own init. Only the REST host
//! and probes are test registrations; the registry receives all linked inventory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use account_management_sdk::{AccountManagementClient, IdpPluginClient, IdpPluginSpecV1};
use async_trait::async_trait;
use serde_json::{Value, json};
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::api::{OpenApiRegistry, OpenApiRegistryImpl};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::contracts::{ApiGatewayCapability, Gear, RunnableCapability};
use toolkit::registry::{GearRegistry, Registrator, RegistryBuilder, SystemCap};
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit::{GearCtx, gts::PluginV1};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, Origin, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions, TypesRegistryApi,
};

// Linked as a host links them: explicitly, not through any gear's `deps`.
use account_management as _;
use static_idp_plugin as _;
use types_registry as _;

const ROOT_TENANT_TYPE: &str = "gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~";
const AM_TR_PLUGIN: &str = "cf.builtin.account_management_tenant_resolver.plugin.v1";
const STATIC_IDP: &str = "cf.builtin.static_idp.plugin.v1";

const CONSUMER: &str = "am-init-consumer";
const REST_HOST: &str = "test-rest-host";
/// Gears that published or read through the registry in `init` and declared it in `deps`.
const DECOUPLED: &[&str] = &[
    "account-management",
    "resource-group",
    "tenant-resolver",
    "authz-resolver",
    "static-idp-plugin",
];

const HOST_TIMEOUT: Duration = Duration::from_secs(60);

/// What the consumer found in its `init()`.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent observations of one boot"
)]
struct Found {
    am_client: bool,
    tenant_api: bool,
    root_type_managed: bool,
    tr_plugin_managed: bool,
    tr_plugin_bound: bool,
}

/// An ordinary gear depending on account-management, as the BSS ledger does.
struct Consumer {
    found: mpsc::UnboundedSender<Result<Found, String>>,
    started: mpsc::UnboundedSender<()>,
}

fn managed(read: Result<Option<Origin>, String>) -> Result<bool, String> {
    Ok(matches!(read?, Some(Origin::Managed { .. })))
}

fn origin_only() -> ReadOptions {
    ReadOptions {
        projection: Projection::Select(FieldSelection::with(&[EntityField::Origin])),
        ..ReadOptions::default()
    }
}

impl Consumer {
    async fn look(ctx: &GearCtx) -> Result<Found, String> {
        let hub = ctx.client_hub();
        let platform = hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| format!("no platform API before an ordinary init: {e}"))?;
        let marker = PlatformSecurityContext::outbound_marker();

        let root = GtsTypeId::try_new(ROOT_TENANT_TYPE).map_err(|e| e.to_string())?;
        let root_type_managed = managed(
            platform
                .get_type_schema(&marker, &root, origin_only())
                .await
                .map(|schema| schema.origin)
                .map_err(|e| format!("root type: {e}")),
        )?;

        let tr_id = TenantResolverPluginSpecV1::gts_make_instance_id(AM_TR_PLUGIN);
        let tr_plugin_managed = managed(
            platform
                .get_instance(&marker, &tr_id, origin_only())
                .await
                .map(|instance| instance.origin)
                .map_err(|e| format!("AM tenant-resolver plugin instance: {e}")),
        )?;

        Ok(Found {
            am_client: hub.get::<dyn AccountManagementClient>().is_ok(),
            tenant_api: hub.get::<dyn TypesRegistryApi>().is_ok(),
            root_type_managed,
            tr_plugin_managed,
            tr_plugin_bound: hub
                .try_get_scoped::<dyn TenantResolverPluginClient>(&ClientScope::gts_id(
                    tr_id.as_ref(),
                ))
                .is_some(),
        })
    }
}

#[async_trait]
impl Gear for Consumer {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let found = Self::look(ctx).await;
        let failure = found.as_ref().err().cloned();
        drop(self.found.send(found));
        match failure {
            None => Ok(()),
            Some(reason) => anyhow::bail!("{CONSUMER}: {reason}"),
        }
    }
}

#[async_trait]
impl RunnableCapability for Consumer {
    /// Started after account-management's own `start`: the whole host is up.
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        self.started.send(()).ok();
        Ok(())
    }

    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A REST host that serves nothing: the gears' REST registration runs, no socket opens.
struct RestHost(OpenApiRegistryImpl);

#[async_trait]
impl Gear for RestHost {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ApiGatewayCapability for RestHost {
    fn rest_prepare(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _hc: Arc<toolkit::RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn rest_finalize(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _hc: Arc<toolkit::RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn as_registry(&self) -> &dyn OpenApiRegistry {
        &self.0
    }

    /// Bound at once, so the directory phase does not wait; with no directory client in
    /// the hub it advertises nothing.
    fn bound_endpoint(&self) -> Option<String> {
        Some("http://127.0.0.1:9".to_owned())
    }
}

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

/// Every linked gear as its `#[toolkit::gear]` registered it, plus the probes.
fn registry(consumer: &Arc<Consumer>) -> GearRegistry {
    let mut builder = RegistryBuilder::default();
    for registrator in inventory::iter::<Registrator> {
        (registrator.0)(&mut builder);
    }
    let host = Arc::new(RestHost(OpenApiRegistryImpl::new()));
    builder.register_core_with_meta(REST_HOST, &[], Arc::clone(&host) as Arc<dyn Gear>);
    builder.register_rest_host_with_meta(REST_HOST, host as Arc<dyn ApiGatewayCapability>);
    builder.register_core_with_meta(
        CONSUMER,
        &["account-management"],
        Arc::clone(consumer) as Arc<dyn Gear>,
    );
    builder.register_stateful_with_meta(
        CONSUMER,
        Arc::clone(consumer) as Arc<dyn RunnableCapability>,
    );
    builder.build_topo_sorted().expect("registry")
}

#[tokio::test]
async fn an_ordinary_consumer_finds_am_and_its_registry_publications_in_its_init() {
    let dir = tempfile::tempdir().expect("temp dir");
    let database = |file: &str| {
        json!({
            "dsn": format!("sqlite://{}?mode=rwc", dir.path().join(file).display()),
            "pool": { "max_conns": 4 },
        })
    };
    let sections = HashMap::from([
        (
            "types-registry".to_owned(),
            json!({ "database": database("types_registry.db"), "config": {} }),
        ),
        (
            "resource-group".to_owned(),
            json!({ "database": database("resource_group.db") }),
        ),
        (
            "account-management".to_owned(),
            json!({
                "database": database("account_management.db"),
                "config": {
                    "root_tenant_type": { "gts_id": ROOT_TENANT_TYPE, "idp_provisioning": false },
                    "tr_plugin": { "enabled": true, "priority": 0 },
                },
            }),
        ),
    ]);
    let figment = figment::Figment::new().merge(figment::providers::Serialized::defaults(
        json!({ "gears": sections }),
    ));
    let db_manager = Arc::new(
        toolkit_db::DbManager::from_figment(figment, dir.path().to_path_buf())
            .expect("database manager"),
    );

    let (found_tx, mut found_rx) = mpsc::unbounded_channel();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let consumer = Arc::new(Consumer {
        found: found_tx,
        started: started_tx,
    });
    let registry = registry(&consumer);

    // No gear reaches the registry through `deps` any more; the registry is a system gear,
    // and every one of them is an ordinary gear initialized after it.
    let entries = registry.gears();
    for name in DECOUPLED {
        let entry = entries
            .iter()
            .find(|entry| entry.name() == *name)
            .unwrap_or_else(|| panic!("{name} is linked"));
        assert!(
            !entry.deps().contains(&"types-registry"),
            "{name} still declares the registry: {:?}",
            entry.deps()
        );
    }
    // The invariant that replaces those edges, read from the registrations themselves rather
    // than from a topological order that may put the registry first by chance: the registry
    // is a system gear, and the gears publishing through it in `init` are not.
    let system = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name() == name)
            .unwrap_or_else(|| panic!("{name} is linked"))
            .caps()
            .has::<SystemCap>()
    };
    assert!(
        system("types-registry"),
        "the registry must initialize first"
    );
    for name in [
        "account-management",
        "resource-group",
        "static-idp-plugin",
        CONSUMER,
    ] {
        assert!(
            !system(name),
            "{name} must initialize after every system gear"
        );
    }
    let order: Vec<&str> = registry
        .gears_by_system_priority()
        .iter()
        .map(|entry| entry.name())
        .collect();
    let at = |name: &str| order.iter().position(|n| *n == name).unwrap();
    for name in [
        "account-management",
        "resource-group",
        "static-idp-plugin",
        CONSUMER,
    ] {
        assert!(at("types-registry") < at(name), "{order:?}");
    }

    let hub = Arc::new(ClientHub::default());
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        registry,
        Arc::new(Config(sections)),
        DbOptions::Manager(db_manager),
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    let mut host = tokio::spawn(runtime.run_gear_phases());

    let found = tokio::time::timeout(HOST_TIMEOUT, async {
        tokio::select! {
            found = found_rx.recv() => found.expect("the consumer reports"),
            boot = &mut host => panic!("the host ended before the consumer ran: {:?}", boot),
        }
    })
    .await
    .expect("the consumer runs within the timeout")
    .expect("the consumer found everything it reads");

    assert!(
        found.am_client,
        "AccountManagementClient is published in AM's init"
    );
    assert!(
        found.tenant_api,
        "the tenant API is published before ordinary inits"
    );
    assert!(
        found.root_type_managed,
        "AM's root type is admitted in its init"
    );
    assert!(
        found.tr_plugin_managed,
        "AM's tenant-resolver plugin is admitted in its init"
    );
    assert!(
        found.tr_plugin_bound,
        "and bound only then, before the consumer runs"
    );

    tokio::time::timeout(HOST_TIMEOUT, async {
        tokio::select! {
            started = started_rx.recv() => started.expect("the consumer starts"),
            boot = &mut host => panic!("the host ended before it started: {:?}", boot),
        }
    })
    .await
    .expect("the host starts within the timeout");

    // The static IdP plugin is ordered after nothing but the registry; once the host is
    // up its instance is admitted and its client bound under that instance.
    let idp_id = PluginV1::<IdpPluginSpecV1>::build_registration(STATIC_IDP, "cf", 100)
        .expect("registration")
        .0;
    let idp = hub
        .get::<dyn PlatformTypesRegistryApi>()
        .expect("platform API")
        .get_instance(
            &PlatformSecurityContext::outbound_marker(),
            &idp_id,
            origin_only(),
        )
        .await
        .expect("the static IdP instance");
    assert!(matches!(idp.origin, Some(Origin::Managed { .. })));
    assert!(
        hub.try_get_scoped::<dyn IdpPluginClient>(&ClientScope::gts_id(idp_id.as_ref()))
            .is_some()
    );

    cancel.cancel();
    tokio::time::timeout(HOST_TIMEOUT, host)
        .await
        .expect("the host stops within the timeout")
        .expect("host task")
        .expect("a clean shutdown");
}
