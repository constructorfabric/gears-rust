use std::{sync::Arc, time::Duration};

use cluster_sdk::{ClusterClient, RemoteClusterClient, observability::otel::OtelClusterMetrics};
use event_broker::{
    DeploymentMode, EventBrokerConfig, ExternallyManagedDirectoryPresence,
    config::{ProcessClusterConfig, checks},
};
use toolkit::bootstrap::{AppConfig, OopComposition, OopCompositionContext, OopCompositionError};
use toolkit::runtime::{ClientCleanup, ClientRegistration};

use crate::cluster_memory::{EbMemoryOptions, RunningMemory, prepare_memory};

const ENDPOINT: &str = "gears.cluster-sdk.clients.consumes.cluster.grpc_endpoint";

/// Validated broker registration name and cluster binding ready for client construction.
pub struct Plan<'a> {
    name: &'static str,
    cluster: Cluster<'a>,
}

enum Cluster<'a> {
    Memory(EbMemoryOptions),
    Remote(&'a str),
}

fn invalid(field: &'static str) -> OopCompositionError {
    OopCompositionError::InvalidConfig { field }
}

enum ConfiguredCluster<'broker, 'app> {
    Memory {
        profiles: &'broker [String],
        sweep_interval: Duration,
    },
    Remote(&'app str),
}

fn process_rule(error: &checks::Error) -> Option<OopCompositionError> {
    let field = match error {
        checks::Error::MemoryProcessOutsideStandalone => {
            "event-broker.process.cluster.mode must be remote for distributed deployment modes"
        }
        checks::Error::MemoryProcessProfileCount => {
            "event-broker.process.cluster.profiles must contain 1 to 64 names"
        }
        checks::Error::DuplicateMemoryProcessProfile => {
            "event-broker.process.cluster.profiles must contain unique names"
        }
        checks::Error::InvalidMemoryProcessProfile => {
            "event-broker.process.cluster.profiles must use names matching [a-zA-Z0-9_-]{1,255}"
        }
        checks::Error::ZeroMemoryProcessSweepInterval => {
            "event-broker.process.cluster.sweep_interval must be positive"
        }
        _ => return None,
    };
    Some(invalid(field))
}

/// Validate the executable's composition and the broker role's deployment rules offline.
///
/// # Errors
///
/// Returns a composition error or a broker deployment error. Composition errors take precedence.
pub fn check(config: &AppConfig) -> anyhow::Result<()> {
    let broker = read_config(config)?;
    resolve_cluster(config, &broker)?;
    checks::check(&broker, broker.mode).map_err(|error| match process_rule(&error) {
        Some(composition) => anyhow::Error::from(composition),
        None => anyhow::Error::from(error),
    })?;
    Ok(())
}

/// Prepare the broker's runtime composition plan before client startup.
///
/// # Errors
///
/// Returns the first invalid composition field.
pub fn validate(config: &AppConfig) -> Result<Plan<'_>, OopCompositionError> {
    let broker = read_config(config)?;
    let configured = resolve_cluster(config, &broker)?;
    broker.check_process(broker.mode).map_err(|error| {
        process_rule(&error).unwrap_or_else(|| invalid("event-broker.process.cluster"))
    })?;
    let name = match broker.mode {
        DeploymentMode::Standalone => "event-broker",
        DeploymentMode::ClusterIngest => "event-broker-ingest",
        DeploymentMode::ClusterDelivery => "event-broker-delivery",
        DeploymentMode::ClusterDispatcher => "event-broker-dispatcher",
    };
    let cluster = match configured {
        ConfiguredCluster::Memory {
            profiles,
            sweep_interval,
        } => Cluster::Memory(EbMemoryOptions {
            profiles: profiles.to_vec(),
            sweep_interval,
            metrics: Arc::new(OtelClusterMetrics::from_global_meter("eb-memory")),
        }),
        ConfiguredCluster::Remote(endpoint) => Cluster::Remote(endpoint),
    };
    Ok(Plan { name, cluster })
}

fn read_config(config: &AppConfig) -> Result<EventBrokerConfig, OopCompositionError> {
    let http = config
        .oop_http
        .as_ref()
        .ok_or_else(|| invalid("oop_http"))?;
    if http.auth_disabled {
        return Err(invalid("oop_http.auth_disabled"));
    }
    if config
        .gears
        .keys()
        .any(|gear| !matches!(gear.as_str(), "event-broker" | "cluster-sdk"))
    {
        return Err(invalid("gears (move foreign gear sections to the host)"));
    }
    toolkit::gear_config_required(config, "event-broker").map_err(|_| {
        let rule = process_decode_rule(config).and_then(|rule| {
            // Omitting the optional process section isolates independent gear decode failures.
            let mut without_process = config.clone();
            without_process
                .gears
                .get_mut("event-broker")?
                .get_mut("config")?
                .as_object_mut()?
                .remove("process");
            toolkit::gear_config_required::<EventBrokerConfig>(&without_process, "event-broker")
                .is_ok()
                .then_some(rule)
        });
        invalid(rule.unwrap_or("gears.event-broker.config"))
    })
}

fn process_decode_rule(config: &AppConfig) -> Option<&'static str> {
    let gear = config.gears.get("event-broker")?;
    let process = gear.pointer("/config/process")?;
    let Some(fields) = process.as_object() else {
        return (!process.is_null()).then_some("event-broker.process must be a mapping");
    };

    for name in fields.keys() {
        if name != "cluster" {
            return Some("event-broker.process only supports the cluster field");
        }
        if let Some(rule) = cluster_decode_rule(config) {
            return Some(rule);
        }
    }
    fields
        .is_empty()
        .then_some("event-broker.process.cluster is required")
}

fn cluster_decode_rule(config: &AppConfig) -> Option<&'static str> {
    let gear = config.gears.get("event-broker")?;
    let cluster = gear.pointer("/config/process/cluster")?;
    let Some(fields) = cluster.as_object() else {
        return Some("event-broker.process.cluster must be a mapping");
    };

    // Mode and unknown properties fail in map order. The selected mode handles both deferred fields.
    let mut mode = None;
    for (name, value) in fields {
        match name.as_str() {
            "mode" => match value.as_str() {
                Some(selected @ ("memory" | "remote")) => mode = Some(selected),
                _ => return Some("event-broker.process.cluster.mode must be memory or remote"),
            },
            "profiles" | "sweep_interval" => {}
            _ => {
                return Some(
                    "event-broker.process.cluster only supports mode, profiles and sweep_interval",
                );
            }
        }
    }

    let profiles = fields.get("profiles");
    let sweep = fields.get("sweep_interval");
    match mode {
        None => Some("event-broker.process.cluster.mode is required"),
        Some("remote") => {
            if profiles.is_some() {
                Some("event-broker.process.cluster.profiles is only supported in memory mode")
            } else if sweep.is_some() {
                Some("event-broker.process.cluster.sweep_interval is only supported in memory mode")
            } else {
                None
            }
        }
        Some(mode) => {
            if profiles.is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|names| names.iter().any(|name| !name.is_string()))
            }) {
                return Some("event-broker.process.cluster.profiles must be a list of strings");
            }
            let duration = sweep?;
            // Isolate the duration so the diagnostic follows the model's accepted syntax.
            let mut isolated = AppConfig::default();
            let section = isolated.gears.entry("event-broker".to_owned()).or_default();
            section["config"] = [("mode", mode.into()), ("sweep_interval", duration.clone())]
                .into_iter()
                .collect();
            toolkit::gear_config_required::<ProcessClusterConfig>(&isolated, "event-broker")
                .is_err()
                .then_some("event-broker.process.cluster.sweep_interval must be a duration")
        }
    }
}

fn resolve_cluster<'broker, 'app>(
    config: &'app AppConfig,
    broker: &'broker EventBrokerConfig,
) -> Result<ConfiguredCluster<'broker, 'app>, OopCompositionError> {
    let process = broker
        .process
        .as_ref()
        .ok_or_else(|| invalid("event-broker.process"))?;
    match &process.cluster {
        ProcessClusterConfig::Memory {
            profiles,
            sweep_interval,
        } => Ok(ConfiguredCluster::Memory {
            profiles,
            sweep_interval: *sweep_interval,
        }),
        ProcessClusterConfig::Remote {} => {
            let endpoint = config
                .gears
                .get("cluster-sdk")
                .and_then(|gear| gear.pointer("/clients/consumes/cluster/grpc_endpoint"))
                .and_then(|value| value.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid(ENDPOINT))?;
            Ok(ConfiguredCluster::Remote(endpoint))
        }
    }
}

/// Compose the broker's role-specific registration and cluster client for production.
///
/// # Errors
///
/// Returns the first invalid composition field or a cluster client construction error.
pub fn compose_production(
    config: &AppConfig,
    context: &OopCompositionContext,
) -> Result<OopComposition, OopCompositionError> {
    compose(validate(config)?, context, vec![])
}

#[cfg(feature = "e2e-flight-control")]
pub fn compose_e2e(
    config: &AppConfig,
    context: &OopCompositionContext,
) -> Result<OopComposition, OopCompositionError> {
    let endpoint = std::env::var(crate::flight_control_auth::ENDPOINT_FIELD).unwrap_or_default();
    compose_fixture(config, context, &endpoint)
}

#[cfg(feature = "e2e-flight-control")]
fn compose_fixture(
    config: &AppConfig,
    context: &OopCompositionContext,
    endpoint: &str,
) -> Result<OopComposition, OopCompositionError> {
    let plan = validate(config)?;
    let auth = crate::flight_control_auth::authenticator(endpoint, context)?;
    compose(plan, context, vec![ClientRegistration::new(Arc::new(auth))])
}

fn compose(
    plan: Plan<'_>,
    context: &OopCompositionContext,
    mut clients: Vec<ClientRegistration>,
) -> Result<OopComposition, OopCompositionError> {
    let registration_name = plan.name.to_owned();
    clients.push(ClientRegistration::new(Arc::new(
        ExternallyManagedDirectoryPresence,
    )));
    let (cluster, cleanup) = match plan.cluster {
        Cluster::Memory(options) => {
            let RunningMemory { client, cleanup } = prepare_memory(options)?.start();
            (
                ClientRegistration::new::<dyn ClusterClient>(client),
                cleanup,
            )
        }
        Cluster::Remote(endpoint) => {
            let client = RemoteClusterClient::connect_lazy(
                endpoint,
                context.internal_token_provider.as_ref(),
            )
            .map_err(|_| OopCompositionError::ClientConstruction {
                component: "remote cluster client",
            })?;
            (
                ClientRegistration::remote::<dyn ClusterClient>(Arc::new(client)),
                ClientCleanup::empty(),
            )
        }
    };
    clients.push(cluster);
    Ok(OopComposition {
        registration_name,
        clients,
        cleanup,
    })
}

#[cfg(test)]
#[path = "composition_tests.rs"]
mod tests;
