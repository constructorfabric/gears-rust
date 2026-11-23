use std::ffi::OsString;
use std::fs::{self, File};
use std::future::{Future, poll_fn};
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::task::Poll;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use http::uri::PathAndQuery;
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};
use tonic::client::Grpc;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::{Request, Status};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

use self::wire::{AuthenticateRequest, AuthenticateResponse};

#[expect(
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::trivially_copy_pass_by_ref,
    reason = "the generated Prost and Tonic source owns these items"
)]
pub mod wire {
    tonic::include_proto!("test_flight_control.v1");
}

pub const BIN: &str = env!("CARGO_BIN_EXE_test-flight-control");
pub const METHOD: &str = "/test_flight_control.v1.BearerValidation/Authenticate";
pub const INSTANCE: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~cf.builtin.static_authn_resolver.plugin.v1";
pub const VENDOR: &str = "constructorfabric";
pub const KNOWN: &str = "planted-user-bearer-fixture-core";
pub const NIL: &str = "planted-nil-bearer-fixture-core";
pub const UNKNOWN: &str = "planted-unknown-bearer-fixture-core";
pub const WRONG_WORKLOAD: &str = "planted-wrong-workload-fixture-core";
pub const STARTUP: Duration = Duration::from_secs(30);
pub const CALL: Duration = Duration::from_secs(3);
const JOIN: Duration = Duration::from_secs(5);
pub const LOG_RECHECK: Duration = Duration::from_millis(25);

pub struct Core {
    pub(super) child: Child,
    pub(super) home: tempfile::TempDir,
    log: PathBuf,
    pub(super) secret: String,
    pub(super) additional_bearers: &'static [&'static str],
}

impl Core {
    pub(super) fn start(home: tempfile::TempDir, config: &Value, secret: String) -> Result<Self> {
        let config_path = home.path().join("core.yaml");
        fs::write(&config_path, config.to_string())?;
        let log = home.path().join("capture.log");
        let file = File::create(&log)?;
        let binary = std::env::var_os("TEST_FLIGHT_CONTROL_TEST_BINARY")
            .map_or_else(|| PathBuf::from(BIN), PathBuf::from);
        let alternative = home.path().join("alternative");
        let inherited = std::env::vars_os().chain([(
            OsString::from("aPp__SERVER__HOME_DIR"),
            alternative.into_os_string(),
        )]);
        let child = fixture_command(binary, inherited)
            .arg("--config")
            .arg(&config_path)
            .arg("run")
            .stdout(Stdio::from(file.try_clone()?))
            .stderr(Stdio::from(file))
            .kill_on_drop(true)
            .spawn()?;
        Ok(Self {
            child,
            home,
            log,
            secret,
            additional_bearers: &[],
        })
    }

    pub(super) fn log(&self) -> Result<String> {
        let log = fs::read_to_string(&self.log)?;
        self.check_confidential(&log, "process log")?;
        Ok(log)
    }

    pub(super) fn check_confidential(&self, value: &str, source: &str) -> Result<()> {
        for planted in [KNOWN, NIL, UNKNOWN, WRONG_WORKLOAD, self.secret.as_str()]
            .into_iter()
            .chain(self.additional_bearers.iter().copied())
        {
            ensure!(!value.contains(planted), "credential appeared in {source}");
        }
        let binary_metadata = MetadataValue::from_bytes(KNOWN.as_bytes());
        let encoded = std::str::from_utf8(binary_metadata.as_encoded_bytes())?;
        ensure!(
            !value.contains(encoded),
            "encoded credential appeared in {source}"
        );
        Ok(())
    }

    pub(super) async fn admitted_endpoint(&mut self) -> Result<SocketAddr> {
        timeout(STARTUP, async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    anyhow::bail!("core exited before admission ({status})");
                }
                let log = self.log()?;
                let events = events(&log);
                let bound = events.iter().find(|event| {
                    event["fields"]["message"] == "plugin instance admitted and bound"
                        && event["fields"]["gts_id"] == INSTANCE
                });
                let endpoint = events
                    .iter()
                    .find(|event| event["fields"]["message"] == "gRPC hub listening");
                let app_grpc = events.iter().any(|event| {
                    event["fields"]["message"]
                        == "late gRPC services installed on the running listener"
                });
                if let (Some(bound), Some(endpoint), true) = (bound, endpoint, app_grpc) {
                    assert_eq!(bound["fields"]["gts_id"], INSTANCE);
                    let address: SocketAddr = endpoint["fields"]["bound_addr"]
                        .as_str()
                        .context("gRPC hub address field")?
                        .parse()?;
                    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
                    println!("Admitted and bound {INSTANCE} on {address}");
                    return Ok(address);
                }
                sleep(LOG_RECHECK).await;
            }
        })
        .await
        .context("core did not bind the admitted plugin and start its gRPC routes")?
    }

    pub(super) async fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
        }
        let child_id = self.child.id();
        timeout(JOIN, self.child.wait())
            .await
            .context("core child did not exit")??;
        if let Some(id) = child_id {
            println!("Joined fixture child {id}");
        }
        Ok(())
    }

    pub(super) fn request(&self, bearer: &str) -> Request<AuthenticateRequest> {
        let mut request = Request::new(message(bearer));
        let mut secret = MetadataValue::try_from(self.secret.as_str())
            .unwrap_or_else(|_| panic!("generated workload credential is valid ASCII"));
        secret.set_sensitive(true);
        request.metadata_mut().insert(INTERNAL_TOKEN_HEADER, secret);
        request
    }

    pub(super) async fn finish<T>(&mut self, result: std::thread::Result<Result<T>>) -> Result<T> {
        self.stop().await?;
        self.log()?;
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                self.check_confidential(&format!("{error:#}"), "verification error")?;
                Err(error)
            }
            Err(payload) => resume_unwind(payload),
        }
    }
}

pub async fn capture_outcome<T>(
    verification: impl Future<Output = Result<T>>,
) -> std::thread::Result<Result<T>> {
    let mut verification = std::pin::pin!(verification);
    poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| verification.as_mut().poll(cx))) {
            Ok(result) => result.map(Ok),
            Err(payload) => Poll::Ready(Err(payload)),
        },
    )
    .await
}

pub fn fixture_command(
    binary: PathBuf,
    inherited: impl Iterator<Item = (OsString, OsString)>,
) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .envs(inherited.filter(|(name, _)| {
            // Figment trims the OS name before matching its case-insensitive prefix.
            !name
                .to_string_lossy()
                .trim()
                .as_bytes()
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"APP__"))
        }))
        .env_remove("RUST_LOG")
        .env_remove("RUST_BACKTRACE")
        .env_remove("RUST_LIB_BACKTRACE");
    command
}

pub fn events(log: &str) -> Vec<Value> {
    log.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub fn fixture_config(home: &Path, secret: &str, vendor: &str) -> Value {
    let internal_auth = json!({
        "provider": "shared_secret",
        "secret": secret,
        "peer_name": "e2e-platform",
    });
    json!({
        "server": { "home_dir": home },
        "logging": { "default": { "console_level": "debug", "console_format": "json" } },
        "database": { "servers": { "sqlite": { "engine": "sqlite" } } },
        "gears": {
            "api-gateway": {
                "config": {
                    "bind_addr": "127.0.0.1:0",
                    "auth_disabled": false,
                    "require_auth_by_default": true,
                    "internal_auth": internal_auth,
                },
            },
            "grpc-hub": {
                "config": {
                    "listen_addr": "127.0.0.1:0",
                    "internal_auth": internal_auth,
                    "internal_auth_enforcement": "required",
                },
            },
            "service-discovery": { "config": { "trusted_registrars": ["e2e-platform"] } },
            "types-registry": {
                "database": { "server": "sqlite", "file": "types_registry.db" },
                "config": {},
            },
            "authn-resolver": { "config": { "vendor": vendor } },
            "static-authn-plugin": {
                "config": {
                    "vendor": vendor,
                    "mode": "static_tokens",
                    "tokens": [
                        {
                            "token": KNOWN,
                            "identity": {
                                "subject_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                                "subject_type": "user",
                                "subject_tenant_id": "00000000-0000-0000-0000-000000000002",
                                "token_scopes": ["read:data", "write:data"],
                            },
                        },
                        {
                            "token": NIL,
                            "identity": {
                                "subject_id": "00000000-0000-0000-0000-000000000000",
                                "subject_tenant_id": "00000000-0000-0000-0000-000000000000",
                                "token_scopes": [],
                            },
                        },
                    ],
                },
            },
            "test-flight-control-authn": { "config": {} },
        },
    })
}

pub fn message(bearer: &str) -> AuthenticateRequest {
    AuthenticateRequest {
        bearer_token: bearer.into(),
    }
}

pub async fn authenticate(
    grpc: &mut Grpc<Channel>,
    request: Request<AuthenticateRequest>,
) -> Result<AuthenticateResponse, Status> {
    timeout(CALL, async {
        grpc.ready()
            .await
            .map_err(|_| Status::unavailable("fixture channel unavailable"))?;
        grpc.unary(
            request,
            PathAndQuery::from_static(METHOD),
            tonic_prost::ProstCodec::default(),
        )
        .await
        .map(tonic::Response::into_inner)
    })
    .await
    .map_err(|_| Status::deadline_exceeded("fixture test call timed out"))?
}
