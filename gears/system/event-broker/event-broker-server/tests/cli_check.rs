use std::fs::{self, File};
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::process::Command;

const BROKER_BINARY: &str = env!("CARGO_BIN_EXE_cf-gears-event-broker-server");
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

async fn run_check(config: &str, environment: &[(&str, &str)]) -> Result<Output> {
    let home = tempfile::tempdir()?;
    let config_path = home.path().join("broker.yaml");
    let stdout_path = home.path().join("stdout");
    let stderr_path = home.path().join("stderr");
    fs::write(&config_path, config)?;

    let mut child = Command::new(BROKER_BINARY)
        .arg("--config")
        .arg(config_path)
        .arg("check")
        .env_clear()
        .envs(environment.iter().copied())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .stdin(Stdio::null())
        .stdout(File::create(&stdout_path)?)
        .stderr(File::create(&stderr_path)?)
        .kill_on_drop(true)
        .spawn()?;
    let Ok(status) = tokio::time::timeout(CHECK_TIMEOUT, child.wait()).await else {
        let cleanup = child.kill().await;
        anyhow::bail!(
            "{BROKER_BINARY} check timed out after {CHECK_TIMEOUT:?}; cleanup: {cleanup:?}; \
             stdout: {}; stderr: {}",
            String::from_utf8_lossy(&fs::read(stdout_path)?).escape_debug(),
            String::from_utf8_lossy(&fs::read(stderr_path)?).escape_debug(),
        );
    };
    Ok(Output {
        status: status?,
        stdout: fs::read(stdout_path)?,
        stderr: fs::read(stderr_path)?,
    })
}

fn standalone_config() -> Value {
    json!({
        "server": {},
        "oop_http": {
            "listen_addr": "127.0.0.1:0",
            "advertise_uri": "http://127.0.0.1:1"
        },
        "gears": {
            "event-broker": {
                "config": {
                    "mode": "standalone",
                    "process": {"cluster": {"mode": "memory"}},
                    "publish": {"max_wait_secs": 50},
                    "streaming": {"authz_eval_deadline_secs": 10, "authz_scope_ttl_secs": 23},
                    "backends": {
                        "main": {"type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"}
                    },
                    "backend_routing": {"gts.cf.core.events.topic.v1~": "main"}
                }
            }
        }
    })
}

#[tokio::test]
async fn offline_check_validates_the_active_roles_deployment_rules() -> Result<()> {
    let standalone = standalone_config();
    let mut publish = standalone.clone();
    publish["gears"]["event-broker"]["config"]["publish"]["max_wait_secs"] = json!(51);
    let mut scope = standalone.clone();
    scope["gears"]["event-broker"]["config"]["streaming"]["authz_scope_ttl_secs"] = json!(22);
    let mut routing = standalone.clone();
    routing["gears"]["event-broker"]["config"]["backend_routing"] = json!({});
    let mut backend = standalone.clone();
    backend["gears"]["event-broker"]["config"]["backends"] = json!({
        "bad key/SHOULD_NOT_APPEAR": {
            "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"
        }
    });
    let mut dispatcher = standalone.clone();
    dispatcher["gears"] = json!({
        "event-broker": {
            "config": {
                "mode": "cluster_dispatcher",
                "process": {"cluster": {"mode": "remote"}}
            }
        },
        "cluster-sdk": {
            "clients": {"consumes": {"cluster": {"grpc_endpoint": "http://127.0.0.1:0"}}}
        }
    });

    for (case, config, expected) in [
        (
            "standalone at both valid boundaries",
            standalone,
            (Some(0), "", ""),
        ),
        (
            "publish hold above the limit",
            publish,
            (
                Some(1),
                "",
                concat!(
                    "Error: publish: max_wait_secs is 51; in standalone and cluster_ingest modes ",
                    "it is at most 50 seconds, so a synchronous publish's hold stays inside the ",
                    "api-gateway request deadline\n",
                ),
            ),
        ),
        (
            "delivery scope TTL equal to the refresh lead",
            scope,
            (
                Some(1),
                "",
                concat!(
                    "Error: streaming: authz_scope_ttl_secs is 22 and authz_eval_deadline_secs is 10; ",
                    "in standalone and cluster_delivery modes authz_scope_ttl_secs must be greater ",
                    "than 2 x authz_eval_deadline_secs + 2, so a stream's consume scope is refreshed ",
                    "ahead of its expiry\n",
                ),
            ),
        ),
        (
            "missing topic base route",
            routing,
            (
                Some(1),
                "",
                concat!(
                    "Error: backend_routing: there is no route for `gts.cf.core.events.topic.v1~`; ",
                    "it is required so that every topic resolves to a registry entry ",
                    "(feature 0005 section 2.4, C4)\n",
                ),
            ),
        ),
        (
            "invalid backend name",
            backend,
            (
                Some(1),
                "",
                concat!(
                    "Error: backends: key 0 (in sorted order) is not a registry name; a registry name ",
                    "is 1 to 255 characters of [a-zA-Z0-9_-] (feature 0005 section 2.1)\n",
                ),
            ),
        ),
        (
            "remote dispatcher without backends",
            dispatcher,
            (Some(0), "", ""),
        ),
    ] {
        let output = run_check(&config.to_string(), &[]).await?;
        let exit = output.status.code();
        let stdout = std::str::from_utf8(&output.stdout)?;
        let stderr = std::str::from_utf8(&output.stderr)?;
        println!(
            "{case} via {BROKER_BINARY}: {}; stdout=\"{}\"; stderr=\"{}\"",
            output.status,
            stdout.escape_debug(),
            stderr.escape_debug(),
        );
        assert_eq!(
            (exit, stdout, stderr),
            expected,
            "{case} via {BROKER_BINARY}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn offline_check_explains_each_process_cluster_rule_without_echoing_configuration()
-> Result<()> {
    const PUBLISH_ERROR: &str = concat!(
        "Error: publish: max_wait_secs is 51; in standalone and cluster_ingest modes ",
        "it is at most 50 seconds, so a synchronous publish's hold stays inside the ",
        "api-gateway request deadline\n",
    );
    const COUNT_ERROR: &str = "Error: invalid composition configuration field: \
        event-broker.process.cluster.profiles must contain 1 to 64 names\n";
    const DISTRIBUTED_ERROR: &str = "Error: invalid composition configuration field: \
        event-broker.process.cluster.mode must be remote for distributed deployment modes\n";
    const PROFILES_ERROR: &str = "Error: invalid composition configuration field: \
        event-broker.process.cluster.profiles must be a list of strings\n";
    const GEAR_ERROR: &str =
        "Error: invalid composition configuration field: gears.event-broker.config\n";

    let valid = standalone_config();
    let with_cluster = |cluster| {
        let mut cfg = valid.clone();
        cfg["gears"]["event-broker"]["config"]["process"]["cluster"] = cluster;
        cfg
    };
    let mut rows = [
        ("no profiles", json!({"mode":"memory","profiles":[]}), (Some(1), "", COUNT_ERROR)),
        ("one profile and one second", json!({"mode":"memory","profiles":["p00"],"sweep_interval":"1s"}), (Some(0), "", "")),
        ("64 profiles", json!({"mode":"memory","profiles":(0..64).map(|n| format!("p{n:02}")).collect::<Vec<_>>()}), (Some(0), "", "")),
        ("65 profiles", json!({"mode":"memory","profiles":(0..65).map(|n| format!("p{n:02}")).collect::<Vec<_>>()}), (Some(1), "", COUNT_ERROR)),
        ("duplicate profiles", json!({"mode":"memory","profiles":["aaa","aaa"]}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.profiles must contain unique names\n")),
        ("invalid profile name", json!({"mode":"memory","profiles":["bad/qa-secret-profile"]}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.profiles must use names matching [a-zA-Z0-9_-]{1,255}\n")),
        ("zero sweep", json!({"mode":"memory","sweep_interval":"0s"}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.sweep_interval must be positive\n")),
        ("missing mode", json!({}), (Some(1), "",
            "Error: invalid composition configuration field: event-broker.process.cluster.mode is required\n")),
        ("invalid mode wins over invalid profiles", json!({"mode":"qa-secret-mode","profiles":"qa-secret-profile"}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.mode must be memory or remote\n")),
        ("malformed sweep", json!({"mode":"memory","sweep_interval":"qa-secret-duration"}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.sweep_interval must be a duration\n")),
        ("unexpected cluster key", json!({"mode":"memory","qa-secret-property":"process.cluster.profiles must be a list of strings"}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster only supports mode, profiles and sweep_interval\n")),
        ("remote rejects profiles first", json!({"mode":"remote","profiles":null,"sweep_interval":null}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.profiles is only supported in memory mode\n")),
        ("remote rejects sweep", json!({"mode":"remote","sweep_interval":null}), (Some(1), "",
            "Error: invalid composition configuration field: \
             event-broker.process.cluster.sweep_interval is only supported in memory mode\n")),
    ]
    .map(|(case, cluster, expected)| (case, with_cluster(cluster), expected))
    .to_vec();

    let with_long_wait = |mut cfg: Value| {
        cfg["gears"]["event-broker"]["config"]["publish"]["max_wait_secs"] = json!(51);
        cfg
    };
    let invalid_memory = with_long_wait(with_cluster(json!({"mode":"memory","profiles":[]})));
    let mut distributed = invalid_memory.clone();
    distributed["gears"]["event-broker"]["config"]["mode"] = json!("cluster_ingest");
    let mut absent_process = with_long_wait(valid.clone());
    absent_process["gears"]["event-broker"]["config"]["process"] = Value::Null;
    let missing_endpoint = with_long_wait(with_cluster(json!({"mode":"remote"})));
    let mut missing_http = invalid_memory.clone();
    missing_http
        .as_object_mut()
        .expect("app config")
        .remove("oop_http");
    let mut foreign_gear = invalid_memory.clone();
    foreign_gear["gears"]["qa-secret-gear"] = json!({});
    let mut misleading = valid.clone();
    misleading["gears"]["event-broker"]["config"]["publish"] = json!({
        "process.cluster.profiles must be a list of strings": "qa-secret-property",
    });
    let mut unknown_process_key = valid.clone();
    unknown_process_key["gears"]["event-broker"]["config"]["process"]["qa-secret-property"] =
        json!("process.cluster.sweep_interval must be a duration");
    rows.extend([
        ("distributed memory takes precedence", distributed, (Some(1), "", DISTRIBUTED_ERROR)),
        ("profile count takes precedence", invalid_memory, (Some(1), "", COUNT_ERROR)),
        ("absent process takes precedence", absent_process, (Some(1), "",
            "Error: invalid composition configuration field: event-broker.process\n")),
        ("missing remote endpoint takes precedence", missing_endpoint, (Some(1), "",
            "Error: invalid composition configuration field: \
             gears.cluster-sdk.clients.consumes.cluster.grpc_endpoint\n")),
        ("absent HTTP takes precedence", missing_http, (Some(1), "",
            "Error: invalid composition configuration field: oop_http\n")),
        ("foreign gear takes precedence", foreign_gear, (Some(1), "",
            "Error: invalid composition configuration field: gears (move foreign gear sections to the host)\n")),
        ("valid process retains deployment error", with_long_wait(valid.clone()), (Some(1), "", PUBLISH_ERROR)),
        ("unrelated field cannot select a process diagnostic", misleading, (Some(1), "",
            "Error: invalid composition configuration field: gears.event-broker.config\n")),
        ("unexpected process key", unknown_process_key, (Some(1), "",
            "Error: invalid composition configuration field: event-broker.process only supports the cluster field\n")),
    ]);

    let with_deployment_mode_and_profiles = |mode, profiles| {
        let mut cfg = valid.clone();
        cfg["gears"]["event-broker"]["config"] = json!({
            "mode": mode,
            "process": {"cluster": {"mode": "memory", "profiles": profiles}}
        });
        cfg
    };
    rows.extend([
        (
            "invalid deployment mode and malformed profiles",
            with_deployment_mode_and_profiles("bad-deployment-mode", json!(43)),
            (Some(1), "", GEAR_ERROR),
        ),
        (
            "invalid deployment mode alone",
            with_deployment_mode_and_profiles("bad-deployment-mode", json!(["event-broker"])),
            (Some(1), "", GEAR_ERROR),
        ),
        (
            "malformed profiles alone",
            with_deployment_mode_and_profiles("standalone", json!(43)),
            (Some(1), "", PROFILES_ERROR),
        ),
    ]);

    let config_path = "/gears/event-broker/config";
    let invalid_profiles = with_cluster(json!({"mode":"memory","profiles":"qa-secret-profile"}));
    let rendered = json!({"config": invalid_profiles.pointer(config_path).expect("broker config")})
        .to_string();
    let mut inherited = valid.clone();
    inherited["gears"]["event-broker"] = json!({});
    let inherited = inherited.to_string();

    let yaml = r#"
server: {}
oop_http:
  listen_addr: "127.0.0.1:0"
  advertise_uri: "http://127.0.0.1:1"
gears:
  event-broker:
    config:
      mode: standalone
      gts_publication: {}
      process:
        cluster:
          profiles:
            - secondary
            - MiX_9-a
          sweep_interval: 250ms
          mode: memory
      backends:
        main:
          type: "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"
      backend_routing:
        "gts.cf.core.events.topic.v1~": main
"#;
    let invalid_yaml = yaml.replace(
        "profiles:\n            - secondary\n            - MiX_9-a",
        "profiles: qa-secret-profile",
    );
    let rendered_env = [("TOOLKIT_MODULE_CONFIG", rendered.as_str())];
    let mut cases = rows
        .into_iter()
        .map(|(case, cfg, expected)| (case, cfg.to_string(), &[][..], expected))
        .collect::<Vec<_>>();
    cases.extend([
        (
            "nested YAML settings",
            yaml.to_owned(),
            &[][..],
            (Some(0), "", ""),
        ),
        (
            "malformed local YAML profiles",
            invalid_yaml,
            &[][..],
            (Some(1), "", PROFILES_ERROR),
        ),
        (
            "malformed rendered profiles",
            inherited,
            &rendered_env[..],
            (Some(1), "", PROFILES_ERROR),
        ),
        (
            "local config wins over invalid rendered profiles",
            valid.to_string(),
            &rendered_env[..],
            (Some(0), "", ""),
        ),
    ]);
    for (case, source, environment, expected) in cases {
        let mut environment = environment.to_vec();
        environment.push(("TOOLKIT_DIRECTORY_ENDPOINT", "invalid directory endpoint"));
        let output = run_check(&source, &environment).await?;
        assert_eq!(
            (
                output.status.code(),
                std::str::from_utf8(&output.stdout)?,
                std::str::from_utf8(&output.stderr)?,
            ),
            expected,
            "{case} via {BROKER_BINARY}"
        );
    }
    Ok(())
}
