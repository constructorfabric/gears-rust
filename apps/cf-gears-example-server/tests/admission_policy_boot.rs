//! The application admits both manifests before policy validation and engine selection.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(
    feature = "admission-control",
    feature = "static-authn",
    feature = "static-authz",
    feature = "static-tenants"
))]

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const BUNDLE_TYPE: &str = "gts.cf.core.policy_engine.bundle.v1~";
const UNKNOWN_TYPE: &str = "gts.cf.core.admission_boot.unknown.v1~";

struct Boot {
    child: Child,
    address: SocketAddr,
    output: PathBuf,
    // Kept until Drop has killed and reaped the child.
    _home: TempDir,
}

impl Boot {
    fn start(resource_type: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut config: Value =
            serde_saphyr::from_str(include_str!("../../../config/quickstart.yaml")).unwrap();
        config["gears"]["api-gateway"]["config"]["bind_addr"] = json!(address.to_string());
        config["gears"]["api-gateway"]["config"]["prefix_path"] = json!("");
        config["gears"]["api-gateway"]["config"]["enable_docs"] = json!(false);
        config["gears"]["admission-control"]["config"]["engine"] =
            json!({ "vendor": "constructorfabric" });
        config["gears"]["policy-engine"]["config"]["builtin_policies"] = json!([{
            "id": "boot-registry-lookup",
            "resource_types": [resource_type],
            "content": "package builtin.boot_registry_lookup\ndefault deny := false\n"
        }]);
        config["logging"] = json!({ "default": config["logging"]["default"].clone() });
        config["logging"]["default"]["file"] =
            json!(home.path().join("application.log").to_str().unwrap());
        let path = home.path().join("boot.yaml");
        std::fs::write(&path, serde_saphyr::to_string(&config).unwrap()).unwrap();
        let output = home.path().join("console.log");
        let log = File::create(&output).unwrap();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_cf-gears-example-server"))
            .args(["--config", path.to_str().unwrap(), "run"])
            .current_dir(home.path())
            .env("APP__SERVER__HOME_DIR", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        Self {
            child,
            address,
            output,
            _home: home,
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.output).unwrap()
    }

    fn outcome(&mut self) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Outcome::Exited(status);
            }
            if ready(self.address).unwrap_or(false) {
                return Outcome::Ready;
            }
            assert!(Instant::now() < deadline, "boot timed out:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Boot {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

#[derive(Debug)]
enum Outcome {
    Ready,
    Exited(ExitStatus),
}

fn ready(address: SocketAddr) -> std::io::Result<bool> {
    let timeout = Duration::from_millis(200);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response.starts_with("HTTP/1.1 200 ") && response.contains("\"ready\":true"))
}

#[test]
fn admitted_policy_schema_and_engine_instance_reach_ready_together() {
    let mut boot = Boot::start(BUNDLE_TYPE);
    let outcome = boot.outcome();
    assert!(
        matches!(outcome, Outcome::Ready),
        "{outcome:?}:\n{}",
        boot.log()
    );
    let log = boot.log();
    assert!(log.contains("policy-engine initialised"), "{log}");
    assert!(log.contains("admission engine resolved"), "{log}");
}

#[test]
fn an_unknown_builtin_resource_type_stops_boot_before_ready() {
    let mut boot = Boot::start(UNKNOWN_TYPE);
    let outcome = boot.outcome();
    assert!(
        matches!(outcome, Outcome::Exited(status) if !status.success()),
        "{outcome:?}:\n{}",
        boot.log()
    );
    let log = boot.log();
    assert!(log.contains("RESOURCE_TYPE_UNKNOWN"), "{log}");
    assert!(!log.contains("admission engine resolved"), "{log}");
}
