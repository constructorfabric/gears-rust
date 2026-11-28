#[cfg(feature = "fixture-authn")]
mod authn_bridge;
mod registered_gears;
#[cfg(all(feature = "eb-test-policy", feature = "authz-hierarchy"))]
/// Test authorization policy that narrows the shipped policy's evaluation results.
mod test_policy;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use toolkit::bootstrap::{AppConfig, run_server};

/// Command-line options for the test Flight Control executable.
#[derive(Parser)]
#[command(about = "Test flight-control fixture", version)]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run,
}

impl Cli {
    async fn run(&self) -> Result<()> {
        ensure!(
            cfg!(all(
                feature = "registry",
                feature = "directory",
                feature = "http-host",
                feature = "grpc-host",
                feature = "authn",
                feature = "fixture-authn",
            )),
            "test-flight-control run requires the complete core profile",
        );
        ensure!(
            !cfg!(any(
                feature = "authz-hierarchy",
                feature = "eb-test-policy",
                feature = "cluster",
            )) || cfg!(feature = "event-broker"),
            "test-flight-control run requires the complete event-broker profile",
        );

        let config = AppConfig::load_or_default(self.config.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid flight-control configuration"))?;
        for (gear, field) in [("grpc-hub", "listen_addr"), ("api-gateway", "bind_addr")] {
            let loopback = config
                .gears
                .get(gear)
                .and_then(|entry| entry["config"][field].as_str())
                .and_then(|address| address.parse::<SocketAddr>().ok())
                .is_some_and(|address| address.ip().is_loopback());
            ensure!(
                loopback,
                "{gear} {field} must be a literal loopback address"
            );
        }
        #[cfg(all(
            feature = "authn",
            feature = "grpc-host",
            feature = "http-host",
            feature = "fixture-authn"
        ))]
        validate_core_auth(&config).map_err(anyhow::Error::msg)?;
        #[cfg(all(feature = "eb-test-policy", feature = "authz-hierarchy"))]
        test_policy::validate_config(&config)?;
        run_server(config).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run => cli.run().await,
    }
}

#[cfg(all(
    feature = "authn",
    feature = "grpc-host",
    feature = "http-host",
    feature = "fixture-authn"
))]
fn validate_core_auth(config: &AppConfig) -> Result<(), &'static str> {
    use static_authn_plugin::config::{AuthNMode, StaticAuthNPluginConfig};
    use toolkit::gear_config_or_default;
    use toolkit_security::{InternalAuthConfig, SharedSecretInternalAuthenticator};

    const METHOD: &str = "/test_flight_control.v1.BearerValidation/Authenticate";

    let plugin: StaticAuthNPluginConfig = gear_config_or_default(config, "static-authn-plugin")
        .map_err(|_| "invalid static-authn-plugin configuration")?;
    if !matches!(plugin.mode, AuthNMode::StaticTokens) {
        return Err("static-authn-plugin requires static_tokens");
    }
    let gateway: api_gateway::ApiGatewayConfig = gear_config_or_default(config, "api-gateway")
        .map_err(|_| "invalid api-gateway configuration")?;
    if gateway.auth_disabled || !gateway.require_auth_by_default {
        return Err("api-gateway requires tenant authentication");
    }
    let grpc: grpc_hub::GrpcHubConfig =
        gear_config_or_default(config, "grpc-hub").map_err(|_| "invalid grpc-hub configuration")?;
    let credential = |gear: &str, provider: Option<&InternalAuthConfig>| {
        let InternalAuthConfig::SharedSecret { secret, peer_name } = provider? else {
            return None;
        };
        SharedSecretInternalAuthenticator::try_new(secret.clone(), peer_name.clone()).ok()?;
        let key = config.gears.get(gear)?["config"]["internal_auth"]["secret"].as_str()?;
        let metadata = tonic::metadata::MetadataValue::try_from(key).ok()?;
        let text = metadata.to_str().ok()?;
        (text.trim().len() == text.len()).then_some(key)
    };
    let gateway_key = credential("api-gateway", gateway.internal_auth.as_ref())
        .ok_or("api-gateway requires a valid shared_secret")?;
    let grpc_key = credential("grpc-hub", grpc.internal_auth.as_ref())
        .ok_or("grpc-hub requires a valid shared_secret")?;
    if !toolkit_security::crypto::constant_time_eq(gateway_key.as_bytes(), grpc_key.as_bytes()) {
        return Err("fixture workload credentials differ");
    }
    if grpc.internal_auth_enforcement != toolkit_transport_grpc::InternalAuthEnforcement::Required {
        return Err("grpc-hub requires workload authentication");
    }
    let check_prefix = |prefix: &str| {
        if !prefix.starts_with('/') {
            return Err("invalid grpc-hub exemption");
        }
        if METHOD
            .strip_prefix(prefix)
            .is_some_and(|rest| prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/'))
        {
            return Err("private authentication method is exempt");
        }
        Ok(())
    };
    match grpc.internal_auth_exempt_methods {
        Some(prefixes) => prefixes.iter().try_for_each(|prefix| check_prefix(prefix)),
        None => toolkit_transport_grpc::DEFAULT_EXEMPT_PREFIXES
            .iter()
            .try_for_each(|prefix| check_prefix(prefix)),
    }
}

#[cfg(all(test, feature = "core"))]
mod config_tests {
    use super::*;
    use serde_json::{Value, json};

    fn static_core() -> AppConfig {
        let provider = json!({"provider": "shared_secret", "secret": "disposable-test-value"});
        AppConfig {
            gears: [
                (
                    "api-gateway",
                    json!({"bind_addr": "127.0.0.1:0", "internal_auth": provider}),
                ),
                ("grpc-hub", json!({"internal_auth": provider})),
                (
                    "static-authn-plugin",
                    json!({"mode": "static_tokens", "tokens": [
                        {"token": "first-test-user", "identity": {}},
                        {"token": "second-test-user", "identity": {}}
                    ]}),
                ),
            ]
            .into_iter()
            .map(|(gear, config)| (gear.into(), json!({"config": config})))
            .collect(),
            ..AppConfig::default()
        }
    }

    #[test]
    fn the_fixture_accepts_the_shipped_safe_defaults_for_a_static_core() {
        for exemptions in [
            Value::Null,
            json!([]),
            json!([
                "/grpc.health.v1.Health/",
                "/grpc.reflection.v1.ServerReflection/",
                "/test_flight_control.v1.BearerValidation/Other",
                "/test_flight_control.v1.BearerValidation/Authenticat",
                "/test_flight_control.v1.BearerValid"
            ]),
        ] {
            let mut config = static_core();
            if !exemptions.is_null() {
                config.gears.get_mut("grpc-hub").unwrap()["config"]["internal_auth_exempt_methods"] =
                    exemptions;
            }
            assert_eq!(super::validate_core_auth(&config), Ok(()));
        }
    }

    #[test]
    fn the_fixture_requires_workload_credentials_that_survive_grpc_metadata() {
        for (case, value, expected) in [
            (
                "leading space",
                " sample",
                Err("api-gateway requires a valid shared_secret"),
            ),
            ("interior space", "sample value", Ok(())),
            ("interior tab", "sample\tvalue", Ok(())),
        ] {
            let mut config = static_core();
            for gear in ["api-gateway", "grpc-hub"] {
                config.gears.get_mut(gear).unwrap()["config"]["internal_auth"]["secret"] =
                    json!(value);
            }
            assert_eq!(
                validate_core_auth(&config),
                expected,
                "both listeners: {case}"
            );
        }

        for (case, value) in [
            ("leading space", " sample"),
            ("trailing space", "sample "),
            ("spaces only", "   "),
            ("edge tab", "sample\t"),
            ("embedded non-ASCII", "sampl\u{e9}value"),
            ("embedded CR", "sample\rvalue"),
            ("embedded LF", "sample\nvalue"),
        ] {
            for (gear, expected) in [
                (
                    "api-gateway",
                    Err("api-gateway requires a valid shared_secret"),
                ),
                ("grpc-hub", Err("grpc-hub requires a valid shared_secret")),
            ] {
                let mut config = static_core();
                config.gears.get_mut(gear).unwrap()["config"]["internal_auth"]["secret"] =
                    json!(value);
                assert_eq!(validate_core_auth(&config), expected, "{gear}: {case}");
            }
        }
    }

    #[test]
    fn the_fixture_rejects_configuration_outside_its_authentication_contract() {
        let kube = json!({"provider": "kube", "verify": {"audiences": ["test-only"]}});
        let empty = json!({"provider": "shared_secret", "secret": ""});
        for (gear, field, value, expected) in [
            (
                "static-authn-plugin",
                "mode",
                None,
                "static-authn-plugin requires static_tokens",
            ),
            (
                "static-authn-plugin",
                "mode",
                Some(json!("accept_all")),
                "static-authn-plugin requires static_tokens",
            ),
            (
                "static-authn-plugin",
                "mode",
                Some(json!("invalid-test-mode")),
                "invalid static-authn-plugin configuration",
            ),
            (
                "api-gateway",
                "auth_disabled",
                Some(json!(true)),
                "api-gateway requires tenant authentication",
            ),
            (
                "api-gateway",
                "require_auth_by_default",
                Some(json!(false)),
                "api-gateway requires tenant authentication",
            ),
            (
                "api-gateway",
                "auth_disabled",
                Some(json!([])),
                "invalid api-gateway configuration",
            ),
            (
                "grpc-hub",
                "internal_auth_enforcement",
                Some(json!([])),
                "invalid grpc-hub configuration",
            ),
            (
                "api-gateway",
                "internal_auth",
                None,
                "api-gateway requires a valid shared_secret",
            ),
            (
                "grpc-hub",
                "internal_auth",
                None,
                "grpc-hub requires a valid shared_secret",
            ),
            (
                "api-gateway",
                "internal_auth",
                Some(kube.clone()),
                "api-gateway requires a valid shared_secret",
            ),
            (
                "grpc-hub",
                "internal_auth",
                Some(kube),
                "grpc-hub requires a valid shared_secret",
            ),
            (
                "api-gateway",
                "internal_auth",
                Some(empty.clone()),
                "api-gateway requires a valid shared_secret",
            ),
            (
                "grpc-hub",
                "internal_auth",
                Some(empty),
                "grpc-hub requires a valid shared_secret",
            ),
            (
                "grpc-hub",
                "internal_auth",
                Some(json!({"provider":"shared_secret","secret":"different-test-value"})),
                "fixture workload credentials differ",
            ),
            (
                "grpc-hub",
                "internal_auth_enforcement",
                Some(json!("permissive")),
                "grpc-hub requires workload authentication",
            ),
            (
                "grpc-hub",
                "internal_auth_exempt_methods",
                Some(json!([
                    "/grpc.health.v1.Health/",
                    "/test_flight_control.v1.BearerValidation/Authenticate"
                ])),
                "private authentication method is exempt",
            ),
            (
                "grpc-hub",
                "internal_auth_exempt_methods",
                Some(json!(["/test_flight_control.v1.BearerValidation"])),
                "private authentication method is exempt",
            ),
            (
                "grpc-hub",
                "internal_auth_exempt_methods",
                Some(json!(["/test_flight_control.v1.BearerValidation/"])),
                "private authentication method is exempt",
            ),
            (
                "grpc-hub",
                "internal_auth_exempt_methods",
                Some(json!(["/"])),
                "private authentication method is exempt",
            ),
            (
                "grpc-hub",
                "internal_auth_exempt_methods",
                Some(json!([""])),
                "invalid grpc-hub exemption",
            ),
            (
                "grpc-hub",
                "internal_auth_exempt_methods",
                Some(json!(["invalid-synthetic-prefix"])),
                "invalid grpc-hub exemption",
            ),
        ] {
            let mut config = static_core();
            let fields = config.gears.get_mut(gear).unwrap()["config"]
                .as_object_mut()
                .unwrap();
            match value {
                Some(value) => {
                    fields.insert(field.into(), value);
                }
                None => {
                    fields.remove(field);
                }
            }
            assert_eq!(
                super::validate_core_auth(&config),
                Err(expected),
                "{gear} {field}"
            );
        }
    }
}
