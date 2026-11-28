//! Owns the loopback processes used by the executable cutover tests.

use std::fs::{self, File};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use reqwest::{Client, Url};
#[cfg(feature = "e2e-flight-control")]
use reqwest::{Method, StatusCode, header};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

pub const OWNER: &str = "local-cutover-owner-fixture";
pub const OWNER_SUBJECT: uuid::Uuid = uuid::uuid!("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
pub const OWNER_TENANT: uuid::Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000002");
pub const UNKNOWN: &str = "local-cutover-unknown-fixture";
const TOPIC: &str = "gts.cf.core.events.topic.v1~";
#[cfg(feature = "e2e-flight-control")]
const EVENT: &str = "gts.cf.core.events.event.v1~";
#[cfg(feature = "e2e-flight-control")]
const REGISTRY_ENTITIES: &str = "/types-registry/v2/platform/entities";
const POLICY: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~cf.e2e.eb_test_policy.plugin.v1";
const AUTH_PLUGIN: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~cf.builtin.static_authn_resolver.plugin.v1";
const STARTUP: Duration = Duration::from_secs(30);
const CALL: Duration = Duration::from_secs(3);
const REAP: Duration = Duration::from_secs(5);
const RECHECK: Duration = Duration::from_millis(40);

pub struct Process {
    child: Child,
    executable: PathBuf,
    log: PathBuf,
}

impl Process {
    fn start(mut command: Command, home: &Path, name: &str) -> Result<Self> {
        let executable = fs::canonicalize(command.get_program())?;
        let log = home.join(format!("{name}.log"));
        let file = File::create_new(&log)?;
        command
            .stdout(Stdio::from(file.try_clone()?))
            .stderr(Stdio::from(file));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn()?;
        println!("Started {name} pid {}", child.id());
        Ok(Self {
            child,
            executable,
            log,
        })
    }

    #[cfg(feature = "e2e-flight-control")]
    pub fn require_live(&mut self) -> Result<()> {
        ensure!(
            self.child.try_wait()?.is_none(),
            "owned broker exited before HTTP acceptance"
        );
        Ok(())
    }

    #[cfg(feature = "e2e-flight-control")]
    #[allow(
        dead_code,
        reason = "standalone and rebalance check ownership before each probe"
    )]
    pub(super) fn require_owned(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            anyhow::bail!("owned broker exited before readiness ({status})");
        }
        self.verify_identity()
    }

    #[cfg(feature = "e2e-flight-control")]
    #[allow(
        dead_code,
        reason = "standalone and rebalance report broker ownership and reuse Core"
    )]
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    fn log(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.log)?)
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) fn bound_http(&self, workload: &str) -> Result<Option<SocketAddr>> {
        let log = self.safe_log(workload)?;
        let mut events = log
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok());
        let Some(event) = events.find(|event| {
            event["fields"]["message"]
                == "OoP HTTP server bound (probes live; gear routes attach after start)"
        }) else {
            return Ok(None);
        };
        let addr: SocketAddr = event["fields"]["addr"]
            .as_str()
            .context("broker bound address")?
            .parse()?;
        ensure!(
            addr.ip() == Ipv4Addr::LOCALHOST && addr.port() != 0,
            "unexpected broker listener"
        );
        Ok(Some(addr))
    }

    pub async fn exited(&mut self) -> Result<ExitStatus> {
        let started = Instant::now();
        let until = started + STARTUP;
        loop {
            if let Some(status) = self.child.try_wait()? {
                println!(
                    "Joined child {} ({status}) in {} ms",
                    self.child.id(),
                    started.elapsed().as_millis()
                );
                return Ok(status);
            }
            ensure!(
                Instant::now() < until,
                "process stayed up past the startup deadline"
            );
            sleep(RECHECK).await;
        }
    }

    pub(super) fn verify_identity(&self) -> Result<()> {
        let pid = self.child.id();
        #[cfg(target_os = "linux")]
        {
            let process = PathBuf::from(format!("/proc/{pid}"));
            let stat = fs::read_to_string(process.join("stat"))?;
            let (_, fields) = stat
                .rsplit_once(") ")
                .context("process status has a command")?;
            let group = fields
                .split_whitespace()
                .nth(2)
                .context("process group")?
                .parse::<u32>()?;
            let actual = fs::read_link(process.join("exe"))?;
            ensure!(
                group == pid && !actual.as_os_str().is_empty() && actual == self.executable,
                "owned process identity changed"
            );
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "pgid=,comm="])
                .output()?;
            let output = std::str::from_utf8(&output.stdout)?.trim();
            let (group, program) = output
                .split_once(char::is_whitespace)
                .context("process identity")?;
            ensure!(
                group.parse::<u32>()? == pid
                    && !program.trim().is_empty()
                    && fs::canonicalize(program.trim())? == self.executable,
                "owned process identity changed"
            );
        }
        Ok(())
    }

    #[cfg(all(unix, feature = "e2e-flight-control"))]
    pub fn terminate(&mut self) -> Result<()> {
        self.require_live()?;
        self.verify_identity()?;
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()?;
        ensure!(status.success(), "could not signal owned process");
        Ok(())
    }

    #[cfg(feature = "e2e-flight-control")]
    #[allow(
        dead_code,
        reason = "standalone and rebalance require a clean exit on completion"
    )]
    pub(super) async fn finish(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            self.terminate()?;
            let Ok(status) = timeout(REAP, self.exited()).await else {
                self.stop()?;
                anyhow::bail!("child did not exit cleanly within {REAP:?}");
            };
            let status = status?;
            ensure!(status.success(), "child failed during shutdown ({status})");
            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.stop()
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        self.verify_identity()?;
        let pid = self.child.id();
        // On Windows Child::kill uses the owned process handle.
        let started = Instant::now();
        self.child.kill()?;
        let until = started + REAP;
        loop {
            if let Some(status) = self.child.try_wait()? {
                println!(
                    "Joined child {pid} ({status}) in {} ms",
                    started.elapsed().as_millis()
                );
                return Ok(());
            }
            ensure!(Instant::now() < until, "owned child did not exit");
            std::thread::sleep(RECHECK);
        }
    }

    pub fn safe_log(&self, workload: &str) -> Result<String> {
        let log = self.log()?;
        ensure!(
            [OWNER, UNKNOWN, workload]
                .into_iter()
                .all(|value| !log.contains(value)),
            "credential appeared in process output"
        );
        Ok(log)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("Child teardown failed: {e}");
        }
    }
}

#[derive(Debug)]
struct Binaries {
    broker: PathBuf,
    core: PathBuf,
}

#[cfg(feature = "e2e-flight-control")]
#[allow(
    dead_code,
    reason = "explicit selection is used by standalone and rebalance targets"
)]
impl Binaries {
    fn selected() -> Result<Self> {
        let broker = Self::explicit("E2E_BINARY")?;
        let core = Self::explicit("E2E_FLIGHT_CONTROL_BINARY")?;
        ensure!(
            broker != core,
            "broker and flight control must use distinct executables"
        );
        Ok(Self { broker, core })
    }

    fn explicit(variable: &str) -> Result<PathBuf> {
        let path = PathBuf::from(
            std::env::var_os(variable).with_context(|| format!("{variable} is required"))?,
        );
        ensure!(
            path.is_absolute(),
            "{variable} must select an absolute executable path"
        );
        let path = fs::canonicalize(path)
            .with_context(|| format!("{variable} must select an existing executable"))?;
        let metadata = path.metadata()?;
        ensure!(metadata.is_file(), "{variable} must select a file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            ensure!(
                metadata.permissions().mode() & 0o111 != 0,
                "{variable} must be executable"
            );
        }
        Ok(path)
    }
}

pub struct Pair {
    core: Process,
    home: TempDir,
    pub workload: String,
    pub grpc: String,
    #[cfg(feature = "e2e-flight-control")]
    pub gateway: Url,
    pub base: String,
    pub client: Client,
    config: PathBuf,
    broker_binary: PathBuf,
}

fn core_config(home: &Path, auth: &Value) -> Value {
    let subject = OWNER_SUBJECT;
    let t0 = "00000000-0000-0000-0000-000000000001";
    let ta = OWNER_TENANT;
    let tb = "00000000-0000-0000-0000-000000000003";
    let tc = "00000000-0000-0000-0000-000000000004";
    let tx = "00000000-0000-0000-0000-000000000005";
    json!({
        "server":{"home_dir":home},
        "logging":{"default":{"console_level":"debug","console_format":"json"}},
        "database":{"servers":{"sqlite":{"engine":"sqlite"}}},
        "gears":{
            "grpc-hub":{"config":{"listen_addr":"127.0.0.1:0","internal_auth":auth,"internal_auth_enforcement":"required"}},
            "api-gateway":{"config":{"bind_addr":"127.0.0.1:0","internal_auth":auth,"auth_disabled":false,"require_auth_by_default":true}},
            "service-discovery":{"config":{"trusted_registrars":["e2e-platform"]}},
            "types-registry":{"database":{"server":"sqlite","file":"registry.db"},"config":{}},
            "authn-resolver":{"config":{"vendor":"constructorfabric"}},
            "static-authn-plugin":{"config":{"vendor":"constructorfabric","mode":"static_tokens","tokens":[
                {"token":OWNER,"identity":{"subject_id":subject,"subject_tenant_id":ta,"subject_type":"user","token_scopes":[]}}
            ]}},
            "tenant-resolver":{"config":{"vendor":"fixture-tenants"}},
            "static-tr-plugin":{"config":{"vendor":"fixture-tenants","tenants":[{"id":t0,"name":"T0"},
                {"id":ta,"name":"TA","parent_id":t0},{"id":tb,"name":"TB","parent_id":t0},
                {"id":tc,"name":"TC","parent_id":ta},{"id":tx,"name":"TX","parent_id":ta,"self_managed":true}]}},
            "authz-resolver":{"config":{"vendor":"fixture-shipped"}},
            "tr-authz-plugin":{"config":{"vendor":"fixture-shipped","priority":1}},
            "eb-test-policy":{"config":{}},
            "test-flight-control-authn":{"config":{}}
        }
    })
}

fn command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env_clear();
    for name in ["PATH", "SystemRoot", "TMPDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd
}

#[derive(Clone, PartialEq, prost::Message)]
struct ResolveGrpcServiceRequest {
    #[prost(string, tag = "1")]
    service_name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ResolveGrpcServiceResponse {
    #[prost(string, tag = "1")]
    endpoint_uri: String,
}

async fn resolve_auth_service(endpoint: &str, workload: &str) -> Result<()> {
    let transport = tonic::transport::Endpoint::from_shared(endpoint.to_owned())?
        .connect_timeout(CALL)
        .timeout(CALL)
        .connect()
        .await?;
    let mut directory = tonic::client::Grpc::new(transport);
    directory.ready().await?;
    let mut request = tonic::Request::new(ResolveGrpcServiceRequest {
        service_name: "test_flight_control.v1.BearerValidation".into(),
    });
    request
        .metadata_mut()
        .insert("x-toolkit-internal-token", workload.parse()?);
    let response: tonic::Response<ResolveGrpcServiceResponse> = directory
        .unary(
            request,
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/directory.v1.DirectoryService/ResolveGrpcService",
            ),
            tonic_prost::ProstCodec::default(),
        )
        .await?;
    assert_eq!(
        response.into_inner(),
        ResolveGrpcServiceResponse {
            endpoint_uri: endpoint.to_owned()
        }
    );
    Ok(())
}

async fn admitted(core: &mut Process, client: &Client, workload: &str) -> Result<(String, Url)> {
    let until = Instant::now() + STARTUP;
    loop {
        if let Some(status) = core.child.try_wait()? {
            anyhow::bail!("Core exited before admission ({status})");
        }
        core.verify_identity()?;
        let log = core.safe_log(workload)?;
        let events: Vec<Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let field = |message: &str, name: &str| {
            events
                .iter()
                .find(|event| event["fields"]["message"] == message)
                .and_then(|event| event["fields"][name].as_str())
        };
        if events.iter().any(|e| {
            e["fields"]["message"] == "late gRPC services installed on the running listener"
        }) && let Some(grpc) = field("gRPC hub listening", "bound_addr")
            && let Some(http) = field(
                "REST host endpoint published for directory registration",
                "endpoint",
            )
        {
            let addr: SocketAddr = grpc.parse()?;
            let base = Url::parse(http)?;
            ensure!(
                addr.ip() == Ipv4Addr::LOCALHOST
                    && base.scheme() == "http"
                    && base.host_str() == Some("127.0.0.1"),
                "unexpected fixture listener"
            );
            let mut active = true;
            for id in [POLICY, AUTH_PLUGIN] {
                core.verify_identity()?;
                let url = base.join(&format!(
                    "/types-registry/v2/platform/entities/{id}?$select=content"
                ))?;
                let response = client
                    .get(url)
                    .header("x-toolkit-internal-token", workload)
                    .send()
                    .await?;
                if !response.status().is_success() {
                    active = false;
                    break;
                }
                let body: Value = response.json().await?;
                active &= body["gts_id"] == id && body["lifecycle_status"] == "active";
            }
            if active {
                let endpoint = format!("http://{addr}");
                core.verify_identity()?;
                resolve_auth_service(&endpoint, workload).await?;
                core.verify_identity()?;
                return Ok((endpoint, base));
            }
        }
        ensure!(
            Instant::now() < until,
            "Core did not admit its fixture plugins"
        );
        sleep(RECHECK).await;
    }
}

impl Pair {
    pub async fn start() -> Result<Self> {
        let addr = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?;
        Self::start_with_addr(addr).await
    }

    #[cfg(feature = "e2e-flight-control")]
    pub async fn start_at(addr: SocketAddr) -> Result<Self> {
        Self::start_with_addr(addr).await
    }

    async fn start_with_addr(addr: SocketAddr) -> Result<Self> {
        let binaries = Binaries {
            broker: std::env::var_os("E2E_BINARY").map_or_else(
                || PathBuf::from(env!("CARGO_BIN_EXE_cf-gears-event-broker-server")),
                PathBuf::from,
            ),
            core: std::env::var_os("E2E_FLIGHT_CONTROL_BINARY")
                .map(PathBuf::from)
                .context("E2E_FLIGHT_CONTROL_BINARY is required for the split-process test")?,
        };
        Self::launch(addr, binaries, &[]).await
    }

    #[cfg(feature = "e2e-flight-control")]
    #[allow(
        dead_code,
        reason = "standalone and rebalance require explicit executable selection"
    )]
    pub(super) async fn start_selected(documents: &[Value]) -> Result<Self> {
        let binaries = Binaries::selected()?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        Self::launch(addr, binaries, documents).await
    }

    async fn launch(addr: SocketAddr, binaries: Binaries, documents: &[Value]) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let core_home = home.path().join("flight-control");
        let eb_home = home.path().join("event-broker");
        fs::create_dir(&core_home)?;
        fs::create_dir(&eb_home)?;
        let workload = uuid::Uuid::new_v4().to_string();
        let auth = json!({"provider":"shared_secret","secret":workload,"peer_name":"e2e-platform"});
        let config = core_home.join("config.yaml");
        let mut settings = core_config(&core_home, &auth);
        if !documents.is_empty() {
            settings["gears"]["types-registry"]["config"]["registration_policy"] = json!({
                "gts.cf.core.events.event.v1~x.eb.*": {"allowed_vendors":["x"]},
                "gts.cf.core.events.topic.v1~x.eb.*": {"allowed_vendors":["x"]},
            });
        }
        fs::write(&config, settings.to_string())?;
        let mut cmd = command(&binaries.core);
        cmd.arg("--config").arg(config).arg("run");
        let started = Instant::now();
        let mut core = Process::start(cmd, &core_home, "flight-control")?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CALL)
            .build()?;
        let admission = timeout(STARTUP, admitted(&mut core, &client, &workload)).await;
        let (grpc, gateway) = match admission {
            Ok(Ok(endpoints)) => {
                println!(
                    "Core {} admitted in {} ms",
                    core.child.id(),
                    started.elapsed().as_millis()
                );
                endpoints
            }
            Ok(Err(error)) => {
                let output = core.safe_log(&workload)?;
                return Err(error.context(format!("Core failed before admission:\n{output}")));
            }
            Err(_) => {
                let output = core.safe_log(&workload)?;
                anyhow::bail!("Core admission timed out after {STARTUP:?}:\n{output}");
            }
        };
        let base = format!("http://{addr}");
        let config = eb_home.join("config.yaml");
        let eb = json!({"server":{"home_dir":eb_home},
        "logging":{"default":{"console_level":"info","console_format":"json"}},
        "database":{"servers":{"sqlite":{"engine":"sqlite"}}},
        "oop_http":{"listen_addr":addr.to_string(),"advertise_uri":base,"allow_loopback_advertise":true,"auth_disabled":false,"internal_auth":auth},
        "gears":{"event-broker":{"database":{"server":"sqlite","file":"event_broker.db"},
            "clients":{"consumes":{"types-registry":{"endpoint":gateway.as_str()},"authz-resolver":{"endpoint":gateway.as_str()}}},
            "config":{"mode":"standalone","process":{"cluster":{"mode":"memory"}},
                "backends":{"main":{"type":"gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"}},
                "backend_routing":{(TOPIC):"main"},"topics":{(TOPIC):{}}}
        }}});
        fs::write(&config, eb.to_string())?;
        Ok(Self {
            core,
            home,
            workload,
            grpc,
            #[cfg(feature = "e2e-flight-control")]
            gateway,
            base,
            client,
            config,
            broker_binary: binaries.broker,
        })
    }

    #[cfg(feature = "e2e-flight-control")]
    #[allow(
        dead_code,
        reason = "standalone and rebalance retain the same Core through broker restarts"
    )]
    pub(super) fn core_pid(&self) -> Result<u32> {
        self.core.verify_identity()?;
        Ok(self.core.child.id())
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) async fn registry_documents(
        &self,
        documents: &[Value],
    ) -> Result<Vec<header::HeaderValue>> {
        let mut etags = Vec::new();
        for document in documents {
            let (id, kind) = Self::document_identity(document)?;
            let path = format!("{REGISTRY_ENTITIES}/{id}?$select=content");
            let response = self.registry_request(Method::GET, &path)?.send().await?;
            let status = response.status();
            let mime = response.headers().get(header::CONTENT_TYPE).cloned();
            let etag = response.headers().get(header::ETAG).cloned();
            let actual = self.registry_json(response).await?;
            assert_eq!(
                (status, mime.as_ref().and_then(|v| v.to_str().ok()), &actual),
                (
                    StatusCode::OK,
                    Some("application/json"),
                    &Self::registry_row(id, kind, document)
                ),
            );
            let etag = etag.context("admitted fixture must have an ETag")?;
            println!(
                "Core {} read {id} at ETag {}: {actual}",
                self.core_pid()?,
                etag.to_str()?
            );
            etags.push(etag);
        }
        Ok(etags)
    }

    #[cfg(feature = "e2e-flight-control")]
    fn document_identity(document: &Value) -> Result<(&str, &'static str)> {
        if let Some(id) = document.get("id").and_then(Value::as_str) {
            Ok((id, "instance"))
        } else {
            let id = document
                .get("$id")
                .and_then(Value::as_str)
                .and_then(|id| id.strip_prefix("gts://"))
                .context("event schema fixture id")?;
            Ok((id, "type_schema"))
        }
    }

    #[cfg(feature = "e2e-flight-control")]
    fn registry_row(id: &str, kind: &str, content: &Value) -> Value {
        let gts_namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"gts");
        json!({
            "gts_id": id,
            "gts_uuid": uuid::Uuid::new_v5(&gts_namespace, id.as_bytes()),
            "kind": kind,
            "lifecycle_status": "active",
            "ownership": "global",
            "content": content,
        })
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) async fn admit_documents(
        &self,
        broker: &mut Process,
        documents: &[Value],
    ) -> Result<Vec<header::HeaderValue>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        let mut items = Vec::new();
        let mut results = Vec::new();
        for content in documents {
            let (gts_id, _) = Self::document_identity(content)?;
            items.push(json!({"gts_id": gts_id, "content": content}));
            results.push(json!({
                "gts_id": gts_id, "status": "succeeded", "resource_version": 1, "error": null,
            }));
        }
        timeout(STARTUP, async {
            broker.require_owned()?;
            let response = self
                .registry_request(Method::POST, REGISTRY_ENTITIES)?
                .header("Idempotency-Key", "cutover-fixture-registration")
                .json(&json!({"items": items}))
                .send()
                .await?;
            let status = response.status();
            let location = response.headers().get(header::LOCATION).cloned();
            let receipt = self.registry_json(response).await?;
            ensure!(
                status == StatusCode::ACCEPTED,
                "fixture submit returned {status}: {receipt}"
            );
            let operation_id: uuid::Uuid = receipt["operation_id"]
                .as_str()
                .context("registry receipt has no operation id")?
                .parse()?;
            let path = format!("/types-registry/v2/platform/operations/{operation_id}");
            let location = self.gateway.join(
                location
                    .context("registry receipt has no Location")?
                    .to_str()?,
            )?;
            assert_eq!(location, self.gateway.join(&path)?);
            loop {
                broker.require_owned()?;
                let response = self.registry_request(Method::GET, &path)?.send().await?;
                let status = response.status();
                let mime = response.headers().get(header::CONTENT_TYPE).cloned();
                let operation = self.registry_json(response).await?;
                assert_eq!(
                    (status, mime.as_ref().and_then(|v| v.to_str().ok())),
                    (StatusCode::OK, Some("application/json")),
                    "{operation}"
                );
                if operation["status"] == "completed" {
                    assert_eq!(operation["items"], json!(results), "{operation}");
                    println!(
                        "Core {} completed registry operation in {} ms: {operation}",
                        self.core_pid()?,
                        started.elapsed().as_millis()
                    );
                    break;
                }
                sleep(RECHECK).await;
            }
            let etags = self.registry_documents(documents).await?;
            broker.require_owned()?;
            Ok(etags)
        })
        .await
        .context("fixture registration did not complete before its deadline")?
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) fn registry_request(
        &self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder> {
        self.core.verify_identity()?;
        Ok(self
            .client
            .request(method, self.gateway.join(path)?)
            .header("x-toolkit-internal-token", &self.workload))
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) async fn registry_json(&self, response: reqwest::Response) -> Result<Value> {
        self.core.verify_identity()?;
        let body = response.text().await?;
        ensure!(
            ![OWNER, UNKNOWN, &self.workload]
                .iter()
                .any(|value| body.contains(value)),
            "credential in registry response"
        );
        Ok(serde_json::from_str(&body)?)
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) async fn await_base_schemas(
        &self,
        broker: &mut Process,
    ) -> Result<Vec<header::HeaderValue>> {
        let started = Instant::now();
        let declarations = event_broker_sdk::gts_declarations();
        let ids: Vec<_> = declarations.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, [EVENT, TOPIC]);
        let admitted = timeout(STARTUP, async {
            let mut etags = Vec::new();
            for (id, schema) in declarations {
                let content: Value = serde_json::from_str(&schema)?;
                let path = format!("{REGISTRY_ENTITIES}/{id}?$select=content");
                loop {
                    broker.require_owned()?;
                    let response = self.registry_request(Method::GET, &path)?.send().await?;
                    let status = response.status();
                    let mime = response.headers().get(header::CONTENT_TYPE).cloned();
                    let etag = response.headers().get(header::ETAG).cloned();
                    let body = self.registry_json(response).await?;
                    if status == StatusCode::NOT_FOUND {
                        sleep(RECHECK).await;
                        continue;
                    }
                    ensure!(
                        status == StatusCode::OK,
                        "registry read for {id}: {status} {body}"
                    );
                    assert_eq!(
                        mime.as_ref().and_then(|v| v.to_str().ok()),
                        Some("application/json")
                    );
                    assert_eq!(body, Self::registry_row(&id, "type_schema", &content));
                    let etag = etag.context("selected schema must have an ETag")?;
                    broker.require_owned()?;
                    println!(
                        "Core {} observed broker {} schema {id} in {} ms, ETag {}: {body}",
                        self.core_pid()?,
                        broker.pid(),
                        started.elapsed().as_millis(),
                        etag.to_str()?,
                    );
                    etags.push(etag);
                    break;
                }
            }
            Ok(etags)
        })
        .await;
        match admitted {
            Ok(Ok(etags)) => Ok(etags),
            Ok(Err(error)) => Err(error.context(format!(
                "broker failed before base schema publication:\n{}",
                broker.safe_log(&self.workload)?
            ))),
            Err(_) => anyhow::bail!(
                "base schemas absent after {STARTUP:?}:\n{}",
                broker.safe_log(&self.workload)?
            ),
        }
    }

    #[cfg(feature = "e2e-flight-control")]
    pub(super) async fn await_catalogue(
        &self,
        broker: &mut Process,
        base: &str,
        documents: &[Value],
    ) -> Result<Vec<Value>> {
        let started = Instant::now();
        timeout(STARTUP, async {
            let mut pages = Vec::new();
            for document in documents {
                let (id, _) = Self::document_identity(document)?;
                let resource = if id.starts_with(TOPIC) {
                    "topics"
                } else {
                    "event-types"
                };
                let mut url = Url::parse(&format!("{base}/event-broker/v1/{resource}"))?;
                url.query_pairs_mut()
                    .append_pair("$filter", &format!("id eq '{id}'"));
                loop {
                    self.core.verify_identity()?;
                    broker.require_owned()?;
                    let response = self
                        .client
                        .get(url.clone())
                        .bearer_auth(OWNER)
                        .send()
                        .await?;
                    let status = response.status();
                    let body = self.registry_json(response).await?;
                    if status == StatusCode::SERVICE_UNAVAILABLE {
                        sleep(RECHECK).await;
                        continue;
                    }
                    assert_eq!(status, StatusCode::OK, "catalogue read for {id}: {body}");
                    let items = body["items"].as_array().context("catalogue page items")?;
                    if items.is_empty() {
                        sleep(RECHECK).await;
                        continue;
                    }
                    let [item] = items.as_slice() else {
                        anyhow::bail!("exact filter returned more than one entity: {body}");
                    };
                    assert_eq!(item["id"], id);
                    assert_eq!(
                        body["page_info"],
                        json!({"next_cursor": null, "prev_cursor": null, "limit": 25})
                    );
                    broker.require_owned()?;
                    println!(
                        "Broker {} served {id} in {} ms: {body}",
                        broker.pid(),
                        started.elapsed().as_millis()
                    );
                    pages.push(body);
                    break;
                }
            }
            Ok(pages)
        })
        .await
        .context("broker did not serve its fixture catalogue before the deadline")?
    }

    pub async fn check_merged_config_offline(&self) -> Result<()> {
        let invalid = json!({"mode":"cluster_ingest","process":{"cluster":{"mode":"memory"}}});
        let mut config: Value = serde_json::from_slice(&fs::read(&self.config)?)?;
        let gear = config["gears"]["event-broker"]
            .as_object_mut()
            .context("EB fixture configuration")?;
        let valid = gear.remove("config").context("local fixture gear config")?;
        let inherited = self.home.path().join("use-rendered-config.yaml");
        fs::write(&inherited, config.to_string())?;
        for (label, path, rendered, expected) in [
            ("local-role", &self.config, &invalid, 0),
            ("rendered-role", &inherited, &valid, 0),
            ("invalid-rendered-role", &inherited, &invalid, 1),
        ] {
            let mut cmd = command(&self.broker_binary);
            cmd.args(["-vv", "check"])
                .env("TOOLKIT_CONFIG_PATH", path)
                .env(
                    "TOOLKIT_MODULE_CONFIG",
                    json!({"config":rendered}).to_string(),
                )
                .env("TOOLKIT_DIRECTORY_ENDPOINT", "invalid directory endpoint");
            let mut process = Process::start(cmd, self.home.path(), label)?;
            assert_eq!(process.exited().await?.code(), Some(expected));
            let log = process.safe_log(&self.workload)?;
            if expected == 1 {
                ensure!(
                    log.contains(
                        "invalid composition configuration field: event-broker.process.cluster"
                    ),
                    "merged role rejection"
                );
            }
        }
        Ok(())
    }

    pub fn broker(&self, name: &str, args: &[&str], endpoint: Option<&str>) -> Result<Process> {
        let mut cmd = command(&self.broker_binary);
        cmd.arg("--config")
            .arg(&self.config)
            .args(args)
            .env("TOOLKIT_DIRECTORY_ENDPOINT", &self.grpc);
        if let Some(endpoint) = endpoint {
            cmd.env("E2E_FLIGHT_CONTROL_ENDPOINT", endpoint);
        }
        Process::start(cmd, self.home.path(), name)
    }

    pub async fn refusal_exit(&self, broker: &mut Process) -> Result<ExitStatus> {
        let until = Instant::now() + STARTUP;
        let url = format!("{}/event-broker/v1/consumer-groups", self.base);
        loop {
            if let Some(status) = broker.child.try_wait()? {
                return Ok(status);
            }
            if let Ok(response) = self.client.get(&url).bearer_auth(OWNER).send().await
                && response.status().is_success()
            {
                let page = response.json::<Value>().await?;
                assert_eq!(
                    page,
                    json!({"items":[],"page_info":{"limit":25,"next_cursor":null,"prev_cursor":null}})
                );
                anyhow::bail!(
                    "expected startup refusal, observed HTTP 200 and complete authenticated empty owner page: {page}"
                );
            }
            ensure!(Instant::now() < until, "broker neither refused nor served");
            sleep(RECHECK).await;
        }
    }

    pub fn finish(mut self) -> Result<()> {
        self.core.stop()?;
        self.core.safe_log(&self.workload)?;
        Ok(())
    }

    #[cfg(feature = "e2e-flight-control")]
    #[allow(
        dead_code,
        reason = "standalone and rebalance require a clean exit on completion"
    )]
    pub(super) async fn finish_selected(&mut self) -> Result<()> {
        let stopped = self.core.finish().await;
        let output = self.core.safe_log(&self.workload)?;
        stopped.with_context(|| format!("Core shutdown failed:\n{output}"))
    }
}

#[cfg(feature = "e2e-flight-control")]
#[expect(
    clippy::doc_markdown,
    clippy::trivially_copy_pass_by_ref,
    reason = "Prost generates the enum documentation and conversion methods"
)]
pub mod auth_wire {
    tonic::include_proto!("test_flight_control.v1");
}

#[cfg(feature = "e2e-flight-control")]
impl std::fmt::Debug for auth_wire::AuthenticateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticateRequest")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "e2e-flight-control")]
impl Pair {
    pub async fn remote_cluster(&self) -> Result<(Process, String)> {
        let binary = std::env::var_os("E2E_REMOTE_FLIGHT_CONTROL_BINARY")
            .context("E2E_REMOTE_FLIGHT_CONTROL_BINARY is required for remote cluster tests")?;
        let home = self.home.path().join("remote-cluster");
        fs::create_dir(&home)?;
        let auth =
            json!({"provider":"shared_secret","secret":self.workload,"peer_name":"e2e-platform"});
        let mut cfg = core_config(&home, &auth);
        cfg["gears"]["cluster"] = json!({"config":{"profiles":{
            "orders":{"cache":{"provider":"standalone"}},
            "event-broker":{"cache":{"provider":"standalone"}}
        }}});
        let path = home.join("config.yaml");
        fs::write(&path, cfg.to_string())?;
        let mut cmd = command(Path::new(&binary));
        cmd.arg("--config").arg(path).arg("run");
        let mut process = Process::start(cmd, &home, "remote")?;
        let (endpoint, _) = admitted(&mut process, &self.client, &self.workload).await?;
        Ok((process, endpoint))
    }

    pub fn broker_config(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(&self.config)?)?)
    }

    pub fn write_broker_config(&self, config: &Value) -> Result<()> {
        Ok(fs::write(&self.config, config.to_string())?)
    }

    pub fn rendered_broker(
        &self,
        name: &str,
        directory: &str,
        rendered: &Value,
    ) -> Result<Process> {
        let mut cmd = command(&self.broker_binary);
        cmd.arg("--config")
            .arg(&self.config)
            .args(["--e2e-flight-control", "run"])
            .env("TOOLKIT_DIRECTORY_ENDPOINT", directory)
            .env("E2E_FLIGHT_CONTROL_ENDPOINT", &self.grpc)
            .env("TOOLKIT_MODULE_CONFIG", rendered.to_string());
        Process::start(cmd, self.home.path(), name)
    }

    pub async fn subscription(&self, base: &str) -> Result<String> {
        let group = self
            .client
            .post(format!("{base}/event-broker/v1/consumer-groups"))
            .bearer_auth(OWNER)
            .send()
            .await?;
        ensure!(
            group.status() == reqwest::StatusCode::CREATED,
            "group creation returned {}",
            group.status()
        );
        let group = group
            .headers()
            .get("location")
            .context("group Location")?
            .to_str()?
            .rsplit('/')
            .next()
            .context("group id")?;
        let subscription = self
            .client
            .post(format!("{base}/event-broker/v1/subscriptions"))
            .bearer_auth(OWNER)
            .json(
                &json!({"consumer_group":group,"client_agent":"cutover-stream",
                "interests":[],"session_timeout":"PT120S"}),
            )
            .send()
            .await?;
        if subscription.status() != reqwest::StatusCode::CREATED {
            let status = subscription.status();
            let problem = subscription.text().await?;
            ensure!(
                [OWNER, UNKNOWN, &self.workload]
                    .into_iter()
                    .all(|marker| !problem.contains(marker)),
                "credential in subscription problem"
            );
            anyhow::bail!("subscription creation returned {status}: {problem}");
        }
        let id = subscription
            .headers()
            .get("location")
            .context("subscription Location")?
            .to_str()?
            .rsplit('/')
            .next()
            .context("subscription id")?
            .parse::<uuid::Uuid>()?;
        Ok(format!(
            "{base}/event-broker/v1/events:sse?subscription={id}"
        ))
    }

    pub fn stop_core(&mut self) -> Result<()> {
        self.core.stop()
    }

    #[cfg(unix)]
    pub async fn legacy_gateway(&self) -> Result<(Process, String)> {
        let binary = std::env::var_os("E2E_LEGACY_GATEWAY_BINARY")
            .context("E2E_LEGACY_GATEWAY_BINARY is required for the paired MIME test")?;
        let core: Value = serde_json::from_slice(&fs::read(
            self.home.path().join("flight-control/config.yaml"),
        )?)?;
        let addr = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?;
        let home = self.home.path().join("legacy-gateway");
        fs::create_dir(&home)?;
        let mut cfg = self.broker_config()?;
        cfg["server"]["home_dir"] = json!(home);
        for gear in [
            "api-gateway",
            "types-registry",
            "authn-resolver",
            "static-authn-plugin",
        ] {
            cfg["gears"][gear] = core["gears"][gear].clone();
        }
        cfg["gears"]["api-gateway"]["config"]["bind_addr"] = json!(addr.to_string());
        cfg["gears"]["cluster"] =
            json!({"config":{"profiles":{"event-broker":{"cache":{"provider":"standalone"}}}}});
        cfg["gears"]["authz-resolver"] = json!({"config":{"vendor":"constructorfabric"}});
        cfg["gears"]["static-authz-plugin"] =
            json!({"config":{"vendor":"constructorfabric","priority":100}});
        let path = home.join("config.yaml");
        fs::write(&path, cfg.to_string())?;
        let mut cmd = command(Path::new(&binary));
        cmd.arg("--config").arg(&path).arg("run");
        let mut process = Process::start(cmd, &home, "gateway-comparator")?;
        let base = format!("http://{addr}");
        let deadline = Instant::now() + STARTUP;
        loop {
            process.require_live()?;
            let log = process.safe_log(&self.workload)?;
            let bound = log
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|event| {
                    event["fields"]["message"]
                        == "REST host endpoint published for directory registration"
                        && event["fields"]["endpoint"] == base
                });
            if bound
                && let Ok(response) = self
                    .client
                    .get(format!("{base}/event-broker/v1/consumer-groups"))
                    .bearer_auth(OWNER)
                    .send()
                    .await
                && response.status() == 200
            {
                assert_eq!(
                    response.json::<Value>().await?,
                    json!({"items":[],"page_info":{"limit":25,"next_cursor":null,"prev_cursor":null}})
                );
                process.verify_identity()?;
                println!(
                    "Legacy gateway {} --config {} run, owned address {base}",
                    process.executable.display(),
                    path.display()
                );
                return Ok((process, base));
            }
            ensure!(
                Instant::now() < deadline,
                "legacy gateway did not publish its own listener and serve owner page"
            );
            sleep(RECHECK).await;
        }
    }

    pub async fn restart_core(&mut self) -> Result<()> {
        let home = self.home.path().join("flight-control");
        let config = home.join("config.yaml");
        let mut value: Value = serde_json::from_slice(&fs::read(&config)?)?;
        let grpc = Url::parse(&self.grpc)?;
        value["gears"]["grpc-hub"]["config"]["listen_addr"] =
            grpc.socket_addrs(|| None)?[0].to_string().into();
        value["gears"]["api-gateway"]["config"]["bind_addr"] =
            self.gateway.socket_addrs(|| None)?[0].to_string().into();
        fs::write(&config, value.to_string())?;
        let mut cmd = command(&self.core.executable);
        cmd.arg("--config").arg(config).arg("run");
        self.core = Process::start(cmd, &home, "restarted-flight-control")?;
        let (grpc, gateway) = admitted(&mut self.core, &self.client, &self.workload).await?;
        assert_eq!((grpc, gateway), (self.grpc.clone(), self.gateway.clone()));
        Ok(())
    }

    pub fn core_log(&self) -> Result<String> {
        self.core.safe_log(&self.workload)
    }

    pub async fn authenticated_page(
        &self,
        broker: &mut Process,
    ) -> Result<(String, reqwest::Response)> {
        let until = Instant::now() + STARTUP;
        loop {
            broker.require_live()?;
            if let Some(addr) = broker.bound_http(&self.workload)? {
                let base = format!("http://{addr}");
                let groups = format!("{base}/event-broker/v1/consumer-groups");
                if let Ok(response) = self.client.get(groups).bearer_auth(OWNER).send().await
                    && response.status() != reqwest::StatusCode::SERVICE_UNAVAILABLE
                {
                    broker.require_live()?;
                    return Ok((base, response));
                }
            }
            ensure!(
                Instant::now() < until,
                "broker did not serve the authenticated route"
            );
            sleep(RECHECK).await;
        }
    }

    pub fn fixture_denial_traces(&self) -> Result<Vec<Option<String>>> {
        let mut traces = Vec::new();
        for event in self
            .core
            .safe_log(&self.workload)?
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        {
            if event["fields"]["message"] != "authn_resolver call failed"
                || event["fields"]["operation"] != "authenticate"
            {
                continue;
            }
            let span = event["spans"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&event["span"]))
                .find(|span| span["name"] == "fixture_bearer_validation")
                .context("Core denial lacks fixture span")?;
            ensure!(
                span["trace_id"] == span["parent.trace_id"],
                "inconsistent fixture trace fields"
            );
            traces.push(span["trace_id"].as_str().map(str::to_owned));
        }
        Ok(traces)
    }

    pub async fn assert_unknown_bearer_refused(&self) -> Result<()> {
        use auth_wire::authenticate_response::Outcome;
        use auth_wire::bearer_validation_client::BearerValidationClient;
        use auth_wire::{AuthenticateRequest, AuthenticateResponse, Failure};
        let transport = tonic::transport::Endpoint::from_shared(self.grpc.clone())?
            .timeout(Duration::from_secs(3))
            .connect_timeout(Duration::from_secs(3))
            .connect()
            .await?;
        let mut client = BearerValidationClient::new(transport);
        let mut request = tonic::Request::new(AuthenticateRequest {
            bearer_token: UNKNOWN.into(),
        });
        request
            .metadata_mut()
            .insert("x-toolkit-internal-token", self.workload.parse()?);
        assert_eq!(
            client.authenticate(request).await?.into_inner(),
            AuthenticateResponse {
                outcome: Some(Outcome::Failure(Failure::InvalidToken as i32))
            }
        );
        Ok(())
    }
}

#[cfg(feature = "e2e-flight-control")]
pub mod raw_grpc {
    use bytes::{Buf, BufMut, Bytes};
    use tonic::{
        Status,
        codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
    };
    // Forward the original protobuf bytes so the observed RPC retains every application field.
    #[derive(Clone, Copy)]
    pub struct Wire;
    impl Encoder for Wire {
        type Item = Bytes;
        type Error = Status;
        fn encode(&mut self, item: Bytes, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
            dst.put(item);
            Ok(())
        }
    }
    impl Decoder for Wire {
        type Item = Bytes;
        type Error = Status;
        fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
            Ok(Some(src.copy_to_bytes(src.remaining())))
        }
    }
    impl Codec for Wire {
        type Encode = Bytes;
        type Decode = Bytes;
        type Encoder = Wire;
        type Decoder = Wire;
        fn encoder(&mut self) -> Wire {
            Wire
        }
        fn decoder(&mut self) -> Wire {
            Wire
        }
    }
}
