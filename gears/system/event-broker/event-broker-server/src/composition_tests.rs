use toolkit::GearRegistry;

#[test]
fn the_executable_registers_only_the_event_broker_gear() {
    let registry = GearRegistry::discover_and_build().expect("discover binary inventory");
    let names: Vec<_> = registry
        .gears()
        .iter()
        .map(toolkit::registry::GearEntry::name)
        .collect();
    assert_eq!(names, ["event-broker"]);
}

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::bootstrap::{AppConfig, OopCompositionContext, OopCompositionError};

fn config(mode: &str, cluster: &Value) -> AppConfig {
    serde_json::from_value(json!({
        "server":{},
        "oop_http":{"listen_addr":"127.0.0.1:0","advertise_uri":"http://127.0.0.1:1"},
        "gears":{
            "event-broker":{"config":{"mode":mode,"process":{"cluster":cluster}}},
            "cluster-sdk":{"clients":{"consumes":{"cluster":{"grpc_endpoint":"http://127.0.0.1:1"}}}}
        }
    })).expect("test config")
}

fn context() -> OopCompositionContext {
    OopCompositionContext {
        cancel: CancellationToken::new(),
        internal_token_provider: None,
    }
}

#[tokio::test]
async fn the_broker_composes_each_configured_role_without_contacting_a_remote_cluster() {
    for (mode, role) in [
        ("standalone", "event-broker"),
        ("cluster_ingest", "event-broker-ingest"),
        ("cluster_delivery", "event-broker-delivery"),
        ("cluster_dispatcher", "event-broker-dispatcher"),
    ] {
        let broker = config(mode, &json!({"mode":"remote"}));
        let composed = crate::composition::compose_production(&broker, &context())
            .expect("compose remote client without a listener");
        assert_eq!(
            (composed.registration_name.as_str(), composed.clients.len()),
            (role, 2)
        );
        composed
            .cleanup
            .finish()
            .await
            .expect("empty remote cleanup");
    }
}

#[tokio::test]
async fn standalone_memory_transfers_its_workers_to_the_composed_cleanup_owner() {
    let broker = config(
        "standalone",
        &json!({"mode":"memory","profiles":["alpha","beta"]}),
    );
    let runtime = tokio::runtime::Handle::current().metrics();
    let before = runtime.num_alive_tasks();
    let composed =
        crate::composition::compose_production(&broker, &context()).expect("prepare memory");
    assert_eq!(runtime.num_alive_tasks() - before, 2);
    assert_eq!(
        (composed.registration_name.as_str(), composed.clients.len()),
        ("event-broker", 2)
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), composed.cleanup.finish())
        .await
        .expect("bounded join")
        .expect("join memory workers");
    assert_eq!(runtime.num_alive_tasks(), before);
}

#[test]
fn the_executable_rejects_incompatible_process_configuration_before_startup() {
    let mut cases = Vec::new();
    let good = || config("standalone", &json!({"mode":"memory"}));
    let invalid = || config("standalone", &json!({"mode":"memory","profiles":[]}));
    let mut missing = invalid();
    missing.oop_http = None;
    cases.push((missing, "oop_http"));
    let mut disabled = invalid();
    disabled.oop_http.as_mut().expect("HTTP").auth_disabled = true;
    cases.push((disabled, "oop_http.auth_disabled"));
    let mut foreign = invalid();
    foreign.gears.insert("qa-secret-gear".into(), json!({}));
    cases.push((foreign, "gears (move foreign gear sections to the host)"));
    let mut absent = good();
    absent.gears.remove("event-broker");
    cases.push((absent, "gears.event-broker.config"));
    let mut malformed = good();
    malformed
        .gears
        .insert("event-broker".into(), json!({"config":{"mode":"unknown"}}));
    cases.push((malformed, "gears.event-broker.config"));
    for process in [
        Value::Null,
        json!({"cluster":{"mode":"memory"}}),
        json!({"cluster":{"mode":"memory","sweep_interval":"250ms"}}),
    ] {
        let mut misleading = good();
        let config = &mut misleading
            .gears
            .get_mut("event-broker")
            .expect("broker config")["config"];
        config["process"] = process;
        config["publish"] =
            json!({"process.cluster.profiles must be a list of strings": "qa-secret-property"});
        cases.push((misleading, "gears.event-broker.config"));
    }
    let mut process = good();
    process.gears.insert(
        "event-broker".into(),
        json!({"config":{"mode":"standalone","publish":{"max_wait_secs":51}}}),
    );
    cases.push((process, "event-broker.process"));
    for (process, field) in [
        (
            json!({"cluster":{"mode":"memory"},"qa-secret-property":"process.cluster.mode is required"}),
            "event-broker.process only supports the cluster field",
        ),
        (json!({}), "event-broker.process.cluster is required"),
        (
            json!("qa-secret-property"),
            "event-broker.process must be a mapping",
        ),
        (
            json!({"cluster":null}),
            "event-broker.process.cluster must be a mapping",
        ),
    ] {
        let mut cfg = good();
        cfg.gears.get_mut("event-broker").expect("broker config")["config"]["process"] = process;
        cases.push((cfg, field));
    }
    for (cluster, field) in [
        (
            json!({"mode":"memory","profiles":[]}),
            "event-broker.process.cluster.profiles must contain 1 to 64 names",
        ),
        (
            json!({"mode":"memory","profiles":["a","a"]}),
            "event-broker.process.cluster.profiles must contain unique names",
        ),
        (
            json!({"mode":"memory","profiles":["bad/qa-secret-profile"]}),
            "event-broker.process.cluster.profiles must use names matching [a-zA-Z0-9_-]{1,255}",
        ),
        (
            json!({"mode":"memory","sweep_interval":"0s"}),
            "event-broker.process.cluster.sweep_interval must be positive",
        ),
        (
            json!({"mode":"memory","profiles":"qa-secret-profile"}),
            "event-broker.process.cluster.profiles must be a list of strings",
        ),
        (json!({}), "event-broker.process.cluster.mode is required"),
        (
            json!({"mode":"qa-secret-mode","profiles":"qa-secret-profile"}),
            "event-broker.process.cluster.mode must be memory or remote",
        ),
        (
            json!({"a-qa-secret-property":"process.cluster.mode is required","mode":"qa-secret-mode"}),
            "event-broker.process.cluster only supports mode, profiles and sweep_interval",
        ),
        (
            json!({"mode":"qa-secret-mode","z-qa-secret-property":"process.cluster.mode is required"}),
            "event-broker.process.cluster.mode must be memory or remote",
        ),
        (
            json!({"mode":"memory","profiles":["ok",31],"sweep_interval":"qa-secret-duration"}),
            "event-broker.process.cluster.profiles must be a list of strings",
        ),
        (
            json!({"mode":"memory","sweep_interval":"qa-secret-duration"}),
            "event-broker.process.cluster.sweep_interval must be a duration",
        ),
        (
            json!({"mode":"memory","qa-secret-property":"process.cluster.mode is required"}),
            "event-broker.process.cluster only supports mode, profiles and sweep_interval",
        ),
        (
            json!({"mode":"remote","profiles":null,"sweep_interval":null}),
            "event-broker.process.cluster.profiles is only supported in memory mode",
        ),
        (
            json!({"mode":"remote","sweep_interval":null}),
            "event-broker.process.cluster.sweep_interval is only supported in memory mode",
        ),
        (
            json!("qa-secret-property"),
            "event-broker.process.cluster must be a mapping",
        ),
    ] {
        let mut cfg = config("standalone", &cluster);
        cfg.gears.get_mut("event-broker").expect("broker config")["config"]["publish"] =
            json!({"max_wait_secs":51});
        cases.push((cfg, field));
    }
    let mut distributed = config("cluster_delivery", &json!({"mode":"memory","profiles":[]}));
    distributed
        .gears
        .get_mut("event-broker")
        .expect("broker config")["config"]["publish"] = json!({"max_wait_secs":51});
    cases.push((
        distributed,
        "event-broker.process.cluster.mode must be remote for distributed deployment modes",
    ));
    for section in [None, Some(json!({"clients":{}}))] {
        let mut remote = config("standalone", &json!({"mode":"remote"}));
        remote.gears.get_mut("event-broker").expect("broker config")["config"]["publish"] =
            json!({"max_wait_secs":51});
        if let Some(section) = section {
            remote.gears.insert("cluster-sdk".into(), section);
        } else {
            remote.gears.remove("cluster-sdk");
        }
        cases.push((
            remote,
            "gears.cluster-sdk.clients.consumes.cluster.grpc_endpoint",
        ));
    }
    for endpoint in [json!(null), json!(""), json!(9)] {
        let mut remote = config("standalone", &json!({"mode":"remote"}));
        remote.gears.insert(
            "cluster-sdk".into(),
            json!({"clients":{"consumes":{"cluster":{"grpc_endpoint":endpoint}}}}),
        );
        cases.push((
            remote,
            "gears.cluster-sdk.clients.consumes.cluster.grpc_endpoint",
        ));
    }
    for (cfg, field) in cases {
        let error = crate::composition::validate(&cfg)
            .err()
            .expect("invalid composition must fail before starting");
        assert_eq!(error, OopCompositionError::InvalidConfig { field });
    }
}

#[tokio::test]
async fn an_unusable_remote_uri_fails_before_the_factory_returns_a_proxy() {
    let mut remote = config("standalone", &json!({"mode":"remote"}));
    remote.gears.insert(
        "cluster-sdk".into(),
        json!({"clients":{"consumes":{"cluster":{"grpc_endpoint":"not an HTTP URI"}}}}),
    );
    let err = super::compose_production(&remote, &context()).expect_err("reject invalid SDK URI");
    assert_eq!(
        err,
        OopCompositionError::ClientConstruction {
            component: "remote cluster client"
        }
    );
}

#[cfg(feature = "e2e-flight-control")]
#[tokio::test]
async fn an_invalid_fixture_endpoint_fails_before_either_memory_worker_starts() {
    let broker = config(
        "standalone",
        &json!({"mode":"memory","profiles":["alpha","beta"]}),
    );
    let runtime = tokio::runtime::Handle::current().metrics();
    let before = runtime.num_alive_tasks();
    let result = super::compose_fixture(&broker, &context(), "http://localhost:9000");
    assert_eq!(
        result.expect_err("literal address required"),
        OopCompositionError::InvalidConfig {
            field: "E2E_FLIGHT_CONTROL_ENDPOINT"
        }
    );
    assert_eq!(runtime.num_alive_tasks(), before);
}

#[tokio::test]
async fn the_composed_local_marker_suppresses_embedded_directory_registration_at_gear_init() {
    use std::sync::Arc;

    use event_broker::test_support::type_registry::empty_platform_registry_registration;
    use toolkit::runtime::{DbOptions, RunOptions, ShutdownOptions};

    for with_composition in [false, true] {
        let mut broker = config("cluster_ingest", &json!({"mode":"remote"}));
        broker.gears.get_mut("event-broker").expect("broker config")["config"]["backends"] =
            json!({"main":{"type":"gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"}});
        broker.gears.get_mut("event-broker").expect("broker config")["config"]["backend_routing"] =
            json!({"gts.cf.core.events.topic.v1~":"main"});
        let composed = super::compose_production(&broker, &context()).expect("compose ingest");
        let mut clients = vec![empty_platform_registry_registration()];
        if with_composition {
            clients.extend(composed.clients);
        }
        let options = RunOptions::new(
            Arc::new(broker),
            DbOptions::None,
            ShutdownOptions::Token(CancellationToken::new()),
            uuid::Uuid::from_u128(1),
        )
        .with_clients(clients);
        let failure = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            toolkit::runtime::run(options),
        )
        .await
        .expect("bounded host init")
        .expect_err("expected gear init preflight");
        let expected = if with_composition {
            "Database is not configured for gear 'event-broker'"
        } else {
            "cannot register with DirectoryService: bound to a wildcard address (0.0.0.0:0) with no `registration.advertise_addr` configured - set one to a routable host"
        };
        assert_eq!(failure.root_cause().to_string(), expected);
        composed
            .cleanup
            .finish()
            .await
            .expect("empty remote cleanup");
    }
}
