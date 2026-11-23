#![cfg(feature = "bootstrap")]
#![cfg(test)]

use std::{
    borrow::Cow,
    fmt::Debug,
    future::Future,
    io::{self, ErrorKind},
    net::{SocketAddr, TcpListener},
    pin::Pin,
    process::Output,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::FutureExt;
use parking_lot::Mutex;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use toolkit::{
    DirectoryClient, GearCtx,
    bootstrap::{
        AppConfig, OopComposition, OopCompositionContext, OopCompositionError,
        OopCompositionRunError, OopRunOptions, TOOLKIT_MODULE_CONFIG_ENV, run_oop_with_composition,
        run_oop_with_options,
    },
    contracts::{Gear, OpenApiRegistry, RestApiCapability, RunnableCapability},
    registry::{Registrator, RegistryBuilder},
    runtime::{ClientCleanup, ClientCleanupError, ClientRegistration},
};
use toolkit_contract::runtime::config::CredentialState;

const BOUND: Duration = Duration::from_secs(10);
const KEY: &str = "composition_fixture";
const CHILD_TMPDIR: &str = "TOOLKIT_TEST_OOP_COMPOSITION_CHILD_TMPDIR";
static ACTIVE: Mutex<Option<Arc<Fixture>>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Serve,
    Legacy,
    Invalid,
    Construction,
    FailInit,
    HoldInit,
    HoldStopAndCleanup,
    HoldCleanup,
    FailCleanup,
}

#[derive(Debug, PartialEq, Eq)]
struct Local {
    value: usize,
}
#[derive(Debug, PartialEq, Eq)]
struct Remote {
    value: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct ConfigSeen {
    key: String,
    section: Value,
    credential: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ClientsSeen {
    local: (usize, bool, bool, bool),
    remote: (usize, bool, bool, bool),
    directory: bool,
    recorder: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Lifecycle {
    events: Vec<&'static str>,
    workers_started: usize,
    workers_joined: usize,
    cleanup_entered: usize,
    cleanup_completed: usize,
    // Process cancellation, independent abort, and completed gear stop at cleanup entry.
    cleanup_state: Option<(bool, bool, bool)>,
}

struct Fixture {
    case: Case,
    local: Arc<Local>,
    remote: Arc<Remote>,
    lifecycle: Mutex<Lifecycle>,
    factory_config: Mutex<Vec<ConfigSeen>>,
    init_config: Mutex<Vec<ConfigSeen>>,
    clients: Mutex<Vec<ClientsSeen>>,
    process: Mutex<Option<CancellationToken>>,
    abort: CancellationToken,
    worker_started: CancellationToken,
    worker_aborted: CancellationToken,
    init_entered: CancellationToken,
    started: CancellationToken,
    stop_entered: CancellationToken,
    stop_completed: CancellationToken,
    cleanup_entered: CancellationToken,
    release_stop: CancellationToken,
    release_cleanup: CancellationToken,
}

impl Fixture {
    fn new(case: Case) -> Self {
        Self {
            case,
            local: Arc::new(Local { value: 17 }),
            remote: Arc::new(Remote { value: 29 }),
            lifecycle: Mutex::default(),
            factory_config: Mutex::default(),
            init_config: Mutex::default(),
            clients: Mutex::default(),
            process: Mutex::default(),
            abort: CancellationToken::new(),
            worker_started: CancellationToken::new(),
            worker_aborted: CancellationToken::new(),
            init_entered: CancellationToken::new(),
            started: CancellationToken::new(),
            stop_entered: CancellationToken::new(),
            stop_completed: CancellationToken::new(),
            cleanup_entered: CancellationToken::new(),
            release_stop: CancellationToken::new(),
            release_cleanup: CancellationToken::new(),
        }
    }

    fn cancel(&self) {
        self.process
            .lock()
            .as_ref()
            .expect("process has started")
            .cancel();
    }
}

struct Session {
    home: tempfile::TempDir,
    fixture: Arc<Fixture>,
}

impl Session {
    fn new(case: Case) -> Self {
        let home = match std::env::var_os(CHILD_TMPDIR) {
            Some(root) => tempfile::tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .expect("create fixture home");
        let fixture = Arc::new(Fixture::new(case));
        assert!(ACTIVE.lock().replace(fixture.clone()).is_none());
        Self { home, fixture }
    }

    fn options(&self, mut config: Value) -> OopRunOptions {
        config["server"]["home_dir"] = json!(self.home.path());
        let path = self.home.path().join("config.yaml");
        std::fs::write(
            &path,
            serde_saphyr::to_string(&config).expect("fixture yaml"),
        )
        .expect("write config");
        OopRunOptions {
            gear_name: "bootstrap-placeholder".to_owned(),
            config_gear_name: Some(KEY.to_owned()),
            instance_id: Some(uuid::uuid!("00000000-0000-0000-0000-000000000031")),
            config_path: Some(path),
            version: Some("1.2.3".to_owned()),
            directory_endpoint: "http://127.0.0.1:9".to_owned(),
            ..OopRunOptions::default()
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(process) = self.fixture.process.lock().as_ref() {
            process.cancel();
        }
        self.fixture.abort.cancel();
        ACTIVE.lock().take();
    }
}

#[derive(Clone, Copy)]
enum RenderedConfig<'a> {
    Unset,
    Set(&'a str),
}

fn session<F: Future<Output = ()>>(rendered: RenderedConfig<'_>, test: F) {
    let rendered = match rendered {
        RenderedConfig::Unset => None,
        RenderedConfig::Set(value) => Some(value),
    };
    temp_env::with_var(TOOLKIT_MODULE_CONFIG_ENV, rendered, || {
        // Enumerate under the outer lock so earlier sessions have restored their keys.
        let override_keys: Vec<_> = std::env::vars_os()
            .filter_map(|(key, _)| {
                key.to_string_lossy()
                    .trim()
                    .to_ascii_uppercase()
                    .starts_with("APP__")
                    .then_some(key)
            })
            .collect();
        temp_env::with_vars_unset(override_keys, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(test);
        });
    });
}

fn composed(
    options: OopRunOptions,
) -> Pin<Box<impl Future<Output = Result<(), OopCompositionRunError>> + Send>> {
    Box::pin(run_oop_with_composition(options, compose))
}

fn config() -> Value {
    json!({"oop_http":{"listen_addr":"127.0.0.1:0","allow_loopback_advertise":true}, "gears":{KEY:{"config":{"vendor":"fixture"}}}})
}

async fn bounded<F: Future>(future: F) -> F::Output {
    timeout(BOUND, future)
        .await
        .expect("fixture operation timed out")
}

async fn until<F: Future, E: Future>(run: &mut Pin<Box<F>>, event: E) -> E::Output
where
    F::Output: Debug,
{
    bounded(async {
        tokio::select! {
            value = event => value,
            result = run.as_mut() => panic!("bootstrap finished before fixture event: {result:?}"),
        }
    })
    .await
}

#[derive(Debug, thiserror::Error)]
#[error("fixture init failure")]
struct InitFailure;

#[async_trait]
impl Gear for Fixture {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        self.lifecycle.lock().events.push("init");
        let hub = ctx.client_hub();
        if self.case == Case::Legacy {
            hub.get::<dyn DirectoryClient>()?;
        } else {
            let recorder = hub.get::<Fixture>()?;
            let local = hub.get::<Local>()?;
            let remote = hub.get::<Remote>()?;
            self.clients.lock().push(ClientsSeen {
                local: (
                    local.value,
                    hub.try_get_local::<Local>().is_some(),
                    hub.has_remote_proxy::<Local>(),
                    Arc::ptr_eq(&self.local, &local),
                ),
                remote: (
                    remote.value,
                    hub.try_get_local::<Remote>().is_some(),
                    hub.has_remote_proxy::<Remote>(),
                    Arc::ptr_eq(&self.remote, &remote),
                ),
                directory: hub.try_get::<dyn DirectoryClient>().is_some(),
                recorder: std::ptr::eq(self, recorder.as_ref()),
            });
        }
        let section = ctx
            .config_provider()
            .get_gear_config(ctx.gear_name())
            .cloned()
            .unwrap_or(Value::Null);
        self.init_config.lock().push(ConfigSeen {
            key: ctx.gear_name().to_owned(),
            section,
            credential: ctx.internal_token_provider().as_ref().map(|provider| {
                match provider.current() {
                    CredentialState::Available(secret) => secret.expose_secret().to_owned(),
                    other => panic!("unexpected credential {other:?}"),
                }
            }),
        });
        self.init_entered.cancel();
        if self.case == Case::HoldInit {
            bounded(std::future::pending::<()>()).await;
        }
        if self.case == Case::FailInit {
            return Err(InitFailure.into());
        }
        Ok(())
    }
}

impl RestApiCapability for Fixture {
    fn register_rest(
        &self,
        _: &GearCtx,
        router: axum::Router,
        _: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
}

#[async_trait]
impl RunnableCapability for Fixture {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        self.lifecycle.lock().events.push("start");
        if self.case == Case::Legacy {
            *self.process.lock() = Some(cancel);
        }
        self.started.cancel();
        Ok(())
    }
    async fn stop(&self, _: CancellationToken) -> anyhow::Result<()> {
        if self.case == Case::HoldStopAndCleanup {
            self.lifecycle.lock().events.push("stop-enter");
            self.stop_entered.cancel();
            bounded(self.release_stop.cancelled()).await;
            self.lifecycle.lock().events.push("stop-exit");
        } else {
            self.lifecycle.lock().events.push("stop");
        }
        self.stop_completed.cancel();
        Ok(())
    }
}

fn register_fixture(builder: &mut RegistryBuilder) {
    let fixture = ACTIVE.lock().as_ref().expect("fixture session").clone();
    builder.register_core_with_meta(KEY, &[], fixture.clone());
    if fixture.case != Case::Legacy {
        builder.register_rest_with_meta(KEY, fixture.clone());
    }
    builder.register_stateful_with_meta(KEY, fixture);
}
inventory::submit!(Registrator(register_fixture));

fn compose(
    config: &AppConfig,
    ctx: &OopCompositionContext,
) -> Result<OopComposition, OopCompositionError> {
    let f = ACTIVE.lock().as_ref().expect("fixture session").clone();
    f.lifecycle.lock().events.push("factory");
    f.factory_config.lock().push(ConfigSeen {
        key: KEY.to_owned(),
        section: config.gears.get(KEY).cloned().unwrap_or(Value::Null),
        credential: ctx
            .internal_token_provider
            .as_ref()
            .map(|provider| match provider.current() {
                CredentialState::Available(secret) => secret.expose_secret().to_owned(),
                other => panic!("unexpected credential {other:?}"),
            }),
    });
    match f.case {
        Case::Invalid => return Err(OopCompositionError::InvalidConfig { field: "process" }),
        Case::Construction => {
            return Err(OopCompositionError::ClientConstruction {
                component: "fixture",
            });
        }
        _ => {}
    }
    *f.process.lock() = Some(ctx.cancel.clone());
    let clients = vec![
        ClientRegistration::new(f.local.clone()),
        ClientRegistration::remote(f.remote.clone()),
        ClientRegistration::new(f.clone()),
    ];
    let orderly = CancellationToken::new();
    let worker = tokio::spawn({
        let f = f.clone();
        let orderly = orderly.clone();
        async move {
            f.lifecycle.lock().workers_started += 1;
            f.worker_started.cancel();
            bounded(async {
                tokio::select! {
                    () = orderly.cancelled() => {},
                    () = f.abort.cancelled() => f.worker_aborted.cancel(),
                }
            })
            .await;
        }
    });
    let abort = f.abort.clone();
    let process = ctx.cancel.clone();
    let cleanup = ClientCleanup::new(
        Box::pin(async move {
            {
                let mut log = f.lifecycle.lock();
                log.cleanup_entered += 1;
                log.cleanup_state = Some((
                    process.is_cancelled(),
                    f.abort.is_cancelled(),
                    f.stop_completed.is_cancelled(),
                ));
                log.events.push(if f.case == Case::HoldStopAndCleanup {
                    "cleanup-enter"
                } else {
                    "cleanup"
                });
            }
            f.cleanup_entered.cancel();
            if matches!(f.case, Case::HoldStopAndCleanup | Case::HoldCleanup) {
                bounded(f.release_cleanup.cancelled()).await;
            }
            orderly.cancel();
            bounded(worker).await.expect("worker joins");
            {
                let mut log = f.lifecycle.lock();
                log.workers_joined += 1;
                log.cleanup_completed += 1;
                if f.case == Case::HoldStopAndCleanup {
                    log.events.push("cleanup-exit");
                }
            }
            if f.case == Case::FailCleanup {
                Err(ClientCleanupError::WorkerFailed)
            } else {
                Ok(())
            }
        }),
        abort,
    );
    Ok(OopComposition {
        registration_name: "composition-worker".to_owned(),
        clients,
        cleanup,
    })
}

fn sources(source: &(dyn std::error::Error + 'static)) -> Vec<String> {
    std::iter::successors(Some(source), |e| e.source())
        .map(ToString::to_string)
        .collect()
}

#[test]
fn composition_receives_effective_config_and_existing_credentials() {
    session(
        RenderedConfig::Set(
            r#"{"config":{"vendor":"master","priority":3},"clients":{"consumes":{"upstream":{"endpoint":"http://127.0.0.1:9"}}}}"#,
        ),
        async {
            for local in [false, true] {
                let s = Session::new(Case::Serve);
                let f = &s.fixture;
                let mut cfg = config();
                cfg["oop_http"]["internal_auth"] = json!({"provider":"shared_secret","secret":"composition-secret","peer_name":"composition-peer"});
                let (input, expected) = if local {
                    (
                        json!({"config":{"vendor":"local"},"clients":{"consumes":{"override":{"endpoint":"http://127.0.0.1:8"}}}}),
                        json!({"config":{"vendor":"local"},"clients":{"consumes":{"override":{"endpoint":"http://127.0.0.1:8"}}}}),
                    )
                } else {
                    (
                        json!({}),
                        json!({"config":{"vendor":"master","priority":3},"clients":{"consumes":{"upstream":{"endpoint":"http://127.0.0.1:9"}}}}),
                    )
                };
                cfg["gears"][KEY] = input;
                let options = s.options(cfg);
                let mut run = composed(options);
                until(&mut run, f.started.cancelled()).await;
                f.cancel();
                bounded(run).await.expect("orderly shutdown");
                assert_eq!(
                    *f.factory_config.lock(),
                    [ConfigSeen {
                        key: KEY.to_owned(),
                        section: expected.clone(),
                        credential: Some("composition-secret".to_owned())
                    }]
                );
                assert_eq!(
                    *f.init_config.lock(),
                    [ConfigSeen {
                        key: KEY.to_owned(),
                        section: expected,
                        credential: Some("composition-secret".to_owned())
                    }]
                );
                assert_eq!(
                    *f.lifecycle.lock(),
                    Lifecycle {
                        events: vec!["factory", "init", "start", "stop", "cleanup"],
                        workers_started: 1,
                        workers_joined: 1,
                        cleanup_entered: 1,
                        cleanup_completed: 1,
                        cleanup_state: Some((true, false, true))
                    }
                );
            }
        },
    );
}

#[test]
fn composed_clients_reach_gear_init_before_start() {
    session(RenderedConfig::Unset, async {
        let s = Session::new(Case::Serve);
        let f = &s.fixture;
        let options = s.options(config());
        let mut run = composed(options);
        until(&mut run, f.started.cancelled()).await;
        f.cancel();
        bounded(run).await.expect("orderly shutdown");
        assert_eq!(
            *f.clients.lock(),
            [ClientsSeen {
                local: (17, true, false, true),
                remote: (29, false, false, true),
                directory: true,
                recorder: true
            }]
        );
        assert_eq!(
            *f.init_config.lock(),
            [ConfigSeen {
                key: KEY.to_owned(),
                section: json!({"config":{"vendor":"fixture"}}),
                credential: None
            }]
        );
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec!["factory", "init", "start", "stop", "cleanup"],
                workers_started: 1,
                workers_joined: 1,
                cleanup_entered: 1,
                cleanup_completed: 1,
                cleanup_state: Some((true, false, true))
            }
        );
    });
}

#[derive(Debug)]
struct RoleResponse {
    status: u16,
    body: String,
    factory_config: Vec<ConfigSeen>,
    init_config: Vec<ConfigSeen>,
}

#[derive(Debug)]
struct RoleStartupFailure {
    result: Result<(), OopCompositionRunError>,
    lifecycle: Lifecycle,
}

impl RoleStartupFailure {
    fn is_initial_bind_collision(&self) -> bool {
        let Err(OopCompositionRunError::Bootstrap { source }) = &self.result else {
            return false;
        };
        let entered_gear = self
            .lifecycle
            .events
            .iter()
            .any(|event| matches!(*event, "init" | "start"));
        !entered_gear
            && self.lifecycle.cleanup_completed == 1
            && std::iter::successors(
                Some(source.as_ref() as &(dyn std::error::Error + 'static)),
                |error| error.source(),
            )
            .filter_map(|error| error.downcast_ref::<io::Error>())
            .any(|error| error.kind() == ErrorKind::AddrInUse)
    }
}

async fn run_role_attempts<T, F>(
    mut run_attempt: impl FnMut(usize) -> F,
) -> Result<(T, Vec<RoleStartupFailure>), Vec<RoleStartupFailure>>
where
    F: Future<Output = Result<T, RoleStartupFailure>>,
{
    const MAX_ATTEMPTS: usize = 3;
    timeout(BOUND, async {
        let mut failures = Vec::new();
        loop {
            let attempt = failures.len() + 1;
            let failure = match run_attempt(attempt).await {
                Ok(response) => return Ok((response, failures)),
                Err(failure) => failure,
            };
            let retry = attempt < MAX_ATTEMPTS && failure.is_initial_bind_collision();
            failures.push(failure);
            if !retry {
                return Err(failures);
            }
            eprintln!(
                "role attempt {attempt}/{MAX_ATTEMPTS} hit an initial bind collision (AddrInUse)"
            );
        }
    })
    .await
    .expect("role attempts exceeded the shared deadline")
}

async fn run_role_attempt(addr: SocketAddr) -> Result<RoleResponse, RoleStartupFailure> {
    let s = Session::new(Case::Serve);
    let f = &s.fixture;
    let mut cfg = config();
    cfg["oop_http"]["listen_addr"] = json!(addr.to_string());
    let mut run = composed(s.options(cfg));
    tokio::select! {
        biased;
        result = run.as_mut() => return Err(RoleStartupFailure {
            result,
            lifecycle: std::mem::take(&mut *f.lifecycle.lock()),
        }),
        () = f.started.cancelled() => {},
    }
    let (status, body) = tokio::select! {
        // A completed bootstrap invalidates a response from the selected address.
        biased;
        result = run.as_mut() => panic!("bootstrap finished before the OpenAPI response: {result:?}"),
        response = request_openapi(addr) => response,
    };
    f.cancel();
    run.await.expect("orderly shutdown");
    Ok(RoleResponse {
        status,
        body,
        factory_config: std::mem::take(&mut *f.factory_config.lock()),
        init_config: std::mem::take(&mut *f.init_config.lock()),
    })
}

async fn request_openapi(addr: SocketAddr) -> (u16, String) {
    loop {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /.well-known/openapi.json HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u16>()
            .unwrap();
        // The listener serves 503 until bootstrap attaches the gear routes.
        if status != 503 {
            return (status, body.to_owned());
        }
    }
}

#[test]
fn composition_role_reaches_serving_without_changing_the_config_key() {
    session(RenderedConfig::Unset, async {
        let response = run_role_attempts(|_| {
            let addr = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap();
            run_role_attempt(addr)
        })
        .await;
        let (
            RoleResponse {
                status,
                body,
                factory_config,
                init_config,
            },
            _,
        ) = response.unwrap_or_else(|failures| {
            panic!(
                "role bootstrap failed on attempt {}: {failures:?}",
                failures.len()
            );
        });
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"openapi":"3.1.0","info":{"title":"composition-worker","version":"1.2.3"},"paths":{},"components":{"securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer","bearerFormat":"JWT"},"internalToken":{"type":"apiKey","in":"header","name":"x-toolkit-internal-token","description":"Platform-plane workload credential, validated by the receiving listener"}}},"x-toolkit-spec-scope":"minimum-conformance"})
        );
        assert_eq!(
            factory_config,
            [ConfigSeen {
                key: KEY.to_owned(),
                section: json!({"config":{"vendor":"fixture"}}),
                credential: None
            }]
        );
        assert_eq!(
            init_config,
            [ConfigSeen {
                key: KEY.to_owned(),
                section: json!({"config":{"vendor":"fixture"}}),
                credential: None
            }]
        );
    });
}

#[test]
fn role_attempts_retry_only_an_initial_bind_collision() {
    session(RenderedConfig::Unset, async {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let occupied_addr = occupied.local_addr().unwrap();
        let mut attempts = 0;
        let (
            RoleResponse {
                status,
                body,
                factory_config,
                init_config,
            },
            collisions,
        ) = run_role_attempts(|attempt| {
            attempts += 1;
            let addr = if attempt == 1 {
                occupied_addr
            } else {
                TcpListener::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap()
            };
            run_role_attempt(addr)
        })
        .await
        .unwrap_or_else(|failures| {
            panic!("role bootstrap failed after {attempts} attempts: {failures:?}");
        });
        assert!((1..=2).contains(&collisions.len()), "{collisions:?}");
        assert_eq!(attempts, collisions.len() + 1);
        for RoleStartupFailure { result, lifecycle } in collisions {
            let Err(OopCompositionRunError::Bootstrap { source }) = result else {
                panic!("expected bootstrap collision: {result:?}");
            };
            let error = source.downcast_ref::<io::Error>().expect("bind I/O error");
            assert_eq!(error.kind(), ErrorKind::AddrInUse);
            assert_eq!(
                lifecycle,
                Lifecycle {
                    events: vec!["factory", "cleanup"],
                    workers_started: 1,
                    workers_joined: 1,
                    cleanup_entered: 1,
                    cleanup_completed: 1,
                    cleanup_state: Some((true, false, false))
                }
            );
        }
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"openapi":"3.1.0","info":{"title":"composition-worker","version":"1.2.3"},"paths":{},"components":{"securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer","bearerFormat":"JWT"},"internalToken":{"type":"apiKey","in":"header","name":"x-toolkit-internal-token","description":"Platform-plane workload credential, validated by the receiving listener"}}},"x-toolkit-spec-scope":"minimum-conformance"})
        );
        assert_eq!(
            factory_config,
            [ConfigSeen {
                key: KEY.to_owned(),
                section: json!({"config":{"vendor":"fixture"}}),
                credential: None
            }]
        );
        assert_eq!(
            init_config,
            [ConfigSeen {
                key: KEY.to_owned(),
                section: json!({"config":{"vendor":"fixture"}}),
                credential: None
            }]
        );
        drop(occupied);

        for (kind, combined, events, cleanup_completed) in [
            (
                ErrorKind::PermissionDenied,
                false,
                &["factory", "cleanup"][..],
                1,
            ),
            (ErrorKind::AddrInUse, true, &["factory", "cleanup"][..], 1),
            (
                ErrorKind::AddrInUse,
                false,
                &["factory", "init", "cleanup"][..],
                1,
            ),
            (ErrorKind::AddrInUse, false, &["factory"][..], 0),
        ] {
            let mut attempts = 0;
            let failures = run_role_attempts(|_| {
                attempts += 1;
                let source = Box::new(io::Error::new(kind, "role startup control"));
                let result = if combined {
                    OopCompositionRunError::BootstrapAndCleanup {
                        source,
                        cleanup: ClientCleanupError::WorkerFailed,
                    }
                } else {
                    OopCompositionRunError::Bootstrap { source }
                };
                std::future::ready(Err::<(), _>(RoleStartupFailure {
                    result: Err(result),
                    lifecycle: Lifecycle {
                        events: events.to_vec(),
                        cleanup_completed,
                        ..Lifecycle::default()
                    },
                }))
            })
            .await
            .unwrap_err();
            assert_eq!(attempts, 1, "{kind:?}, {combined}, {events:?}");
            let [RoleStartupFailure { result, lifecycle }] = failures.as_slice() else {
                panic!("expected one terminal failure: {failures:?}");
            };
            let source = match result {
                Err(OopCompositionRunError::Bootstrap { source }) if !combined => source,
                Err(OopCompositionRunError::BootstrapAndCleanup {
                    source,
                    cleanup: ClientCleanupError::WorkerFailed,
                }) if combined => source,
                other => panic!("unexpected startup result: {other:?}"),
            };
            let error = source
                .downcast_ref::<io::Error>()
                .expect("control I/O error");
            assert_eq!(
                (error.kind(), error.to_string()),
                (kind, "role startup control".to_owned())
            );
            assert_eq!(
                *lifecycle,
                Lifecycle {
                    events: events.to_vec(),
                    cleanup_completed,
                    ..Lifecycle::default()
                }
            );
        }
    });
}

#[test]
fn role_attempts_preserve_collisions_until_a_successful_attempt() {
    session(RenderedConfig::Unset, async {
        for (collision_count, expected_attempts) in [(1, &[1, 2][..]), (2, &[1, 2, 3][..])] {
            let mut attempts = Vec::new();
            let (response, collisions) = run_role_attempts(|attempt| {
                attempts.push(attempt);
                std::future::ready(if attempt <= collision_count {
                    Err(RoleStartupFailure {
                        result: Err(OopCompositionRunError::Bootstrap {
                            source: Box::new(io::Error::new(
                                ErrorKind::AddrInUse,
                                "role retry control",
                            )),
                        }),
                        lifecycle: Lifecycle {
                            events: vec!["factory", "cleanup"],
                            workers_started: 1,
                            workers_joined: 1,
                            cleanup_entered: 1,
                            cleanup_completed: 1,
                            cleanup_state: Some((true, false, false)),
                        },
                    })
                } else {
                    Ok(73_u8)
                })
            })
            .await
            .unwrap_or_else(|failures| {
                panic!("attempts {attempts:?} failed: {failures:?}");
            });
            assert_eq!(attempts, expected_attempts);
            assert_eq!(response, 73);
            assert_eq!(collisions.len(), collision_count);
            for RoleStartupFailure { result, lifecycle } in collisions {
                let Err(OopCompositionRunError::Bootstrap { source }) = result else {
                    panic!("expected bootstrap collision: {result:?}");
                };
                let error = source
                    .downcast_ref::<io::Error>()
                    .expect("control I/O error");
                assert_eq!(
                    (error.kind(), error.to_string()),
                    (ErrorKind::AddrInUse, "role retry control".to_owned())
                );
                assert_eq!(
                    lifecycle,
                    Lifecycle {
                        events: vec!["factory", "cleanup"],
                        workers_started: 1,
                        workers_joined: 1,
                        cleanup_entered: 1,
                        cleanup_completed: 1,
                        cleanup_state: Some((true, false, false))
                    }
                );
            }
        }
    });
}

#[test]
fn role_attempts_stop_after_three_bind_collisions() {
    session(RenderedConfig::Unset, async {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = occupied.local_addr().unwrap();
        let mut attempts = 0;
        let failures = run_role_attempts(|_| {
            attempts += 1;
            run_role_attempt(addr)
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 3);
        assert_eq!(failures.len(), 3);
        for RoleStartupFailure { result, lifecycle } in failures {
            let Err(OopCompositionRunError::Bootstrap { source }) = result else {
                panic!("expected occupied address: {result:?}");
            };
            let error = source.downcast_ref::<io::Error>().expect("bind I/O error");
            assert_eq!(error.kind(), ErrorKind::AddrInUse);
            assert_eq!(
                lifecycle,
                Lifecycle {
                    events: vec!["factory", "cleanup"],
                    workers_started: 1,
                    workers_joined: 1,
                    cleanup_entered: 1,
                    cleanup_completed: 1,
                    cleanup_state: Some((true, false, false))
                }
            );
        }
    });
}

#[test]
fn printing_config_does_not_call_the_composition_factory() {
    const PRINT_MODE: &str = "TOOLKIT_TEST_OOP_COMPOSITION_PRINT_MODE";
    const TEST: &str = "printing_config_does_not_call_the_composition_factory";
    if let Ok(mode) = std::env::var(PRINT_MODE) {
        session(
            RenderedConfig::Set(r#"{"config":{"vendor":"master","priority":3}}"#),
            async {
                let s = Session::new(Case::Invalid);
                let mut cfg = config();
                cfg["gears"][KEY] = json!({"clients":{}});
                cfg["oop_http"]["internal_auth"] = json!({
                    "provider": "kube",
                    "verify": {"audiences": ["toolkit-internal"]},
                    "present": {"token_path": s.home.path().join("missing-token")}
                });
                let mut options = s.options(cfg);
                options.print_config = true;
                options.directory_endpoint.clear();
                match mode.as_str() {
                    "legacy" => bounded(Box::pin(run_oop_with_options(options)))
                        .await
                        .unwrap(),
                    "composed" => bounded(composed(options)).await.unwrap(),
                    other => panic!("unexpected print mode {other}"),
                }
                assert_eq!(*s.fixture.lifecycle.lock(), Lifecycle::default());
            },
        );
        return;
    }
    session(RenderedConfig::Unset, async {
        for (mode, expected) in [
            ("legacy", json!({ KEY: {"clients":{}} })),
            (
                "composed",
                json!({ KEY: {"clients":{}, "config":{"vendor":"master","priority":3}} }),
            ),
        ] {
            let output = run_child_test(TEST, PRINT_MODE, mode).await;
            let stdout = String::from_utf8(output.stdout).expect("print config stdout is UTF-8");
            let mut lines = stdout.lines().skip_while(|line| *line != "gears:");
            assert_eq!(lines.next(), Some("gears:"));
            let section = lines
                .take_while(|line| line.starts_with(' '))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(serde_saphyr::from_str::<Value>(&section).unwrap(), expected);
        }
    });
}

#[test]
fn composition_requires_http_before_calling_the_factory() {
    session(RenderedConfig::Unset, async {
        let s = Session::new(Case::Invalid);
        let f = &s.fixture;
        let options = s.options(json!({}));
        assert!(matches!(
            bounded(composed(options)).await,
            Err(OopCompositionRunError::Composition(
                OopCompositionError::InvalidConfig { field: "oop_http" }
            ))
        ));
        assert_eq!(*f.lifecycle.lock(), Lifecycle::default());
    });
}

#[test]
fn bootstrap_failures_do_not_call_the_factory() {
    for (input, expected) in [
        ("file", "config file does not exist"),
        ("rendered", "TOOLKIT_MODULE_CONFIG is set but invalid"),
        ("endpoint", "invalid URI"),
        (
            "missing-token",
            "cannot read projected service-account token at",
        ),
        (
            "empty-shared-secret",
            "internal_auth provider=shared_secret is unusable as an outbound credential: shared secret must not be empty",
        ),
    ] {
        let rendered = if input == "rendered" {
            RenderedConfig::Set("{")
        } else {
            RenderedConfig::Unset
        };
        session(rendered, async {
            let s = Session::new(Case::Invalid);
            let f = &s.fixture;
            let mut cfg = config();
            let missing_token = s.home.path().join("missing-token");
            match input {
                "missing-token" => {
                    cfg["oop_http"]["internal_auth"] = json!({
                        "provider": "kube",
                        "verify": {"audiences": ["toolkit-internal"]},
                        "present": {"token_path": missing_token}
                    });
                }
                "empty-shared-secret" => {
                    cfg["oop_http"]["internal_auth"] = json!({
                        "provider": "shared_secret",
                        "secret": "",
                        "peer_name": "composition-peer"
                    });
                }
                _ => {}
            }
            let mut options = s.options(cfg);
            let expected = match input {
                "file" => {
                    let path = s.home.path().join("missing.yaml");
                    let expected = format!("{expected}: {}", path.display());
                    options.config_path = Some(path);
                    expected
                }
                "endpoint" => {
                    options.directory_endpoint.clear();
                    expected.to_owned()
                }
                "missing-token" => {
                    let error = std::fs::read_to_string(&missing_token).unwrap_err();
                    assert_eq!(error.kind(), ErrorKind::NotFound);
                    format!("{expected} {}: {error}", missing_token.display())
                }
                _ => expected.to_owned(),
            };
            let Err(OopCompositionRunError::Bootstrap { source }) =
                bounded(composed(options)).await
            else {
                panic!("expected bootstrap failure for {input}")
            };
            assert_eq!(source.to_string(), expected, "{input}");
            assert_eq!(*f.lifecycle.lock(), Lifecycle::default());
        });
    }
}

#[test]
fn factory_errors_start_no_owned_workers() {
    session(RenderedConfig::Unset, async {
        for (case, expected) in [
            (
                Case::Invalid,
                OopCompositionError::InvalidConfig { field: "process" },
            ),
            (
                Case::Construction,
                OopCompositionError::ClientConstruction {
                    component: "fixture",
                },
            ),
        ] {
            let s = Session::new(case);
            let f = &s.fixture;
            let options = s.options(config());
            let Err(OopCompositionRunError::Composition(e)) = bounded(composed(options)).await
            else {
                panic!("expected factory error")
            };
            assert_eq!(e, expected);
            assert_eq!(
                *f.lifecycle.lock(),
                Lifecycle {
                    events: vec!["factory"],
                    ..Lifecycle::default()
                }
            );
        }
    });
}

#[test]
fn serve_option_failures_finish_owned_clients() {
    session(RenderedConfig::Unset, async {
        for (field, mut value, expected) in [
            (
                "listen_addr",
                json!("bad"),
                "invalid oop_http.listen_addr: bad",
            ),
            (
                "probe_bind_addr",
                json!("bad"),
                "invalid oop_http.probe_bind_addr: bad",
            ),
            (
                "advertise_uri",
                json!("ftp://example.com"),
                "invalid oop_http.advertise_uri: scheme must be http or https (got 'ftp')",
            ),
            (
                "labels",
                json!({"bad key":"value"}),
                "invalid oop_http.labels: label keys/values must be <=63 chars, <=64 entries, and use only ASCII alphanumerics plus '-', '_', '.' (starting and ending alphanumeric)",
            ),
            (
                "internal_auth",
                json!({"provider": "kube"}),
                "oop_http: internal_auth provider=kube has no `verify:` block, but this participant validates inbound platform-plane tokens; set `verify.audiences`",
            ),
        ] {
            let s = Session::new(Case::Serve);
            let f = &s.fixture;
            let mut cfg = config();
            if field == "internal_auth" {
                let token_path = s.home.path().join("projected-token");
                std::fs::write(&token_path, "composition-token").expect("write projected token");
                value["present"] = json!({"token_path": token_path});
            }
            cfg["oop_http"][field] = value;
            let options = s.options(cfg);
            let Err(OopCompositionRunError::Bootstrap { source }) =
                bounded(composed(options)).await
            else {
                panic!("expected {field} failure")
            };
            assert_eq!(source.to_string(), expected, "{field}");
            assert_eq!(
                *f.lifecycle.lock(),
                Lifecycle {
                    events: vec!["factory", "cleanup"],
                    workers_started: 1,
                    workers_joined: 1,
                    cleanup_entered: 1,
                    cleanup_completed: 1,
                    cleanup_state: Some((true, false, false))
                }
            );
            assert!(!f.abort.is_cancelled());
        }
    });
}

#[test]
fn a_bind_failure_finishes_owned_clients() {
    session(RenderedConfig::Unset, async {
        let s = Session::new(Case::Serve);
        let f = &s.fixture;
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cfg = config();
        cfg["oop_http"]["listen_addr"] = json!(occupied.local_addr().unwrap().to_string());
        let options = s.options(cfg);
        let Err(OopCompositionRunError::Bootstrap { source }) = bounded(composed(options)).await
        else {
            panic!("expected occupied address")
        };
        let err = std::iter::successors(
            Some(source.as_ref() as &(dyn std::error::Error + 'static)),
            |e| e.source(),
        )
        .find_map(|e| e.downcast_ref::<std::io::Error>())
        .expect("bind I/O error");
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec!["factory", "cleanup"],
                workers_started: 1,
                workers_joined: 1,
                cleanup_entered: 1,
                cleanup_completed: 1,
                cleanup_state: Some((true, false, false))
            }
        );
    });
}

#[test]
fn an_init_failure_finishes_owned_clients() {
    session(RenderedConfig::Unset, async {
        let s = Session::new(Case::FailInit);
        let f = &s.fixture;
        let options = s.options(config());
        let Err(OopCompositionRunError::Bootstrap { source }) = bounded(composed(options)).await
        else {
            panic!("expected init failure")
        };
        let chain = sources(source.as_ref());
        assert!(
            chain.iter().any(|m| m == "fixture init failure"),
            "{chain:?}"
        );
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec!["factory", "init", "cleanup"],
                workers_started: 1,
                workers_joined: 1,
                cleanup_entered: 1,
                cleanup_completed: 1,
                cleanup_state: Some((true, false, false))
            }
        );
    });
}

#[test]
fn host_stop_finishes_before_client_cleanup() {
    session(RenderedConfig::Unset, async {
        let s = Session::new(Case::HoldStopAndCleanup);
        let f = &s.fixture;
        let options = s.options(config());
        let mut run = composed(options);
        until(&mut run, async {
            tokio::select! {
                () = f.started.cancelled() => {},
                () = f.cleanup_entered.cancelled() => panic!("cleanup precedes start: {:?}", f.lifecycle.lock()),
            }
        }).await;
        until(&mut run, f.worker_started.cancelled()).await;
        f.cancel();
        until(&mut run, f.stop_entered.cancelled()).await;
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec!["factory", "init", "start", "stop-enter"],
                workers_started: 1,
                ..Lifecycle::default()
            }
        );
        assert!(!f.abort.is_cancelled());
        f.release_stop.cancel();
        until(&mut run, f.cleanup_entered.cancelled()).await;
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec![
                    "factory",
                    "init",
                    "start",
                    "stop-enter",
                    "stop-exit",
                    "cleanup-enter"
                ],
                workers_started: 1,
                cleanup_entered: 1,
                cleanup_state: Some((true, false, true)),
                ..Lifecycle::default()
            }
        );
        assert!(run.as_mut().now_or_never().is_none());
        f.release_cleanup.cancel();
        bounded(run).await.unwrap();
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec![
                    "factory",
                    "init",
                    "start",
                    "stop-enter",
                    "stop-exit",
                    "cleanup-enter",
                    "cleanup-exit"
                ],
                workers_started: 1,
                workers_joined: 1,
                cleanup_entered: 1,
                cleanup_completed: 1,
                cleanup_state: Some((true, false, true))
            }
        );
        assert!(!f.abort.is_cancelled());
    });
}

#[test]
fn bootstrap_and_cleanup_failures_are_both_returned() {
    session(RenderedConfig::Unset, async {
        for succeeds in [false, true] {
            let s = Session::new(Case::FailCleanup);
            let f = &s.fixture;
            let mut cfg = config();
            if !succeeds {
                cfg["oop_http"]["listen_addr"] = json!("bad");
            }
            let options = s.options(cfg);
            let mut run = composed(options);
            if succeeds {
                until(&mut run, f.started.cancelled()).await;
                f.cancel();
                assert!(matches!(
                    bounded(run).await,
                    Err(OopCompositionRunError::Cleanup(
                        ClientCleanupError::WorkerFailed
                    ))
                ));
                assert_eq!(
                    *f.lifecycle.lock(),
                    Lifecycle {
                        events: vec!["factory", "init", "start", "stop", "cleanup"],
                        workers_started: 1,
                        workers_joined: 1,
                        cleanup_entered: 1,
                        cleanup_completed: 1,
                        cleanup_state: Some((true, false, true))
                    }
                );
            } else {
                let Err(OopCompositionRunError::BootstrapAndCleanup {
                    source,
                    cleanup: ClientCleanupError::WorkerFailed,
                }) = bounded(run).await
                else {
                    panic!("expected runtime and cleanup errors")
                };
                assert_eq!(source.to_string(), "invalid oop_http.listen_addr: bad");
                assert_eq!(
                    *f.lifecycle.lock(),
                    Lifecycle {
                        events: vec!["factory", "cleanup"],
                        workers_started: 1,
                        workers_joined: 1,
                        cleanup_entered: 1,
                        cleanup_completed: 1,
                        cleanup_state: Some((true, false, false))
                    }
                );
            }
        }
    });
}

#[test]
fn a_cleanup_timeout_is_reported_after_host_stop() {
    session(RenderedConfig::Unset, async {
        let s = Session::new(Case::HoldCleanup);
        let f = &s.fixture;
        let mut cfg = config();
        cfg["server"]["shutdown_deadline_secs"] = json!(1);
        let options = s.options(cfg);
        let mut run = composed(options);
        until(&mut run, f.started.cancelled()).await;
        until(&mut run, f.worker_started.cancelled()).await;
        f.cancel();
        let result = timeout(BOUND + Duration::from_secs(1), run)
            .await
            .expect("configured cleanup deadline");
        assert!(matches!(
            result,
            Err(OopCompositionRunError::Cleanup(
                ClientCleanupError::DeadlineExceeded
            ))
        ));
        assert!(f.abort.is_cancelled());
        assert_eq!(
            *f.lifecycle.lock(),
            Lifecycle {
                events: vec!["factory", "init", "start", "stop", "cleanup"],
                workers_started: 1,
                cleanup_entered: 1,
                cleanup_state: Some((true, false, true)),
                ..Lifecycle::default()
            }
        );
        bounded(f.worker_aborted.cancelled()).await;
    });
}

#[test]
fn legacy_bootstrap_preserves_http_and_heartbeat_paths() {
    session(RenderedConfig::Unset, async {
        for cfg in [
            config(),
            json!({"gears":{KEY:{"config":{"vendor":"fixture"}}}}),
        ] {
            let s = Session::new(Case::Legacy);
            let f = &s.fixture;
            let mut run = Box::pin(run_oop_with_options(s.options(cfg)));
            until(&mut run, f.started.cancelled()).await;
            f.cancel();
            bounded(run).await.expect("orderly shutdown");
            assert_eq!(
                *f.lifecycle.lock(),
                Lifecycle {
                    events: vec!["init", "start", "stop"],
                    ..Lifecycle::default()
                }
            );
        }
        let s = Session::new(Case::Legacy);
        let f = &s.fixture;
        let mut options = s.options(config());
        options.print_config = true;
        options.directory_endpoint.clear();
        bounded(Box::pin(run_oop_with_options(options.clone())))
            .await
            .unwrap();
        let missing = s.home.path().join("missing.yaml");
        options.config_path = Some(missing.clone());
        assert_eq!(
            bounded(Box::pin(run_oop_with_options(options)))
                .await
                .unwrap_err()
                .to_string(),
            format!("config file does not exist: {}", missing.display())
        );
        assert_eq!(*f.lifecycle.lock(), Lifecycle::default());
    });
}

#[test]
fn dropping_bootstrap_aborts_owned_clients() {
    session(RenderedConfig::Unset, async {
        for case in [Case::HoldInit, Case::HoldCleanup] {
            let s = Session::new(case);
            let f = &s.fixture;
            let options = s.options(config());
            let mut run = composed(options);
            until(&mut run, f.worker_started.cancelled()).await;
            if case == Case::HoldInit {
                until(&mut run, f.init_entered.cancelled()).await;
            } else {
                until(&mut run, f.started.cancelled()).await;
                f.cancel();
                until(&mut run, f.cleanup_entered.cancelled()).await;
            }
            assert!(!f.abort.is_cancelled());
            drop(run);
            assert!(f.abort.is_cancelled());
            bounded(f.worker_aborted.cancelled()).await;
            if case == Case::HoldInit {
                assert_eq!(
                    *f.lifecycle.lock(),
                    Lifecycle {
                        events: vec!["factory", "init"],
                        workers_started: 1,
                        ..Lifecycle::default()
                    }
                );
            } else {
                assert_eq!(
                    *f.lifecycle.lock(),
                    Lifecycle {
                        events: vec!["factory", "init", "start", "stop", "cleanup"],
                        workers_started: 1,
                        cleanup_entered: 1,
                        cleanup_state: Some((true, false, true)),
                        ..Lifecycle::default()
                    }
                );
            }
        }
    });
}

#[cfg(unix)]
fn compose_and_signal(
    config: &AppConfig,
    ctx: &OopCompositionContext,
) -> Result<OopComposition, OopCompositionError> {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let composition = compose(config, ctx)?;
    eprintln!("composition factory sending SIGTERM to itself");
    kill(Pid::this(), Signal::SIGTERM).expect("signal this test child");
    Ok(composition)
}

fn child_stream(stream: &io::Result<Vec<u8>>) -> Cow<'_, str> {
    match stream {
        Ok(bytes) => String::from_utf8_lossy(bytes),
        Err(error) => format!("<read failed: {error}>").into(),
    }
}

async fn run_child_test(test: &str, mode_key: &str, mode: &str) -> Output {
    let context = format!("child {test} ({mode_key}={mode})");
    // The parent owns the fixture files even if the child dies before running destructors.
    let root =
        tempfile::tempdir().unwrap_or_else(|error| panic!("{context}: create home root: {error}"));
    let stdout = tempfile::NamedTempFile::new_in(root.path())
        .unwrap_or_else(|error| panic!("{context}: create stdout: {error}"));
    let stderr = tempfile::NamedTempFile::new_in(root.path())
        .unwrap_or_else(|error| panic!("{context}: create stderr: {error}"));
    let executable = std::env::current_exe()
        .unwrap_or_else(|error| panic!("{context}: locate test executable: {error}"));
    let mut command = tokio::process::Command::new(executable);
    #[cfg(unix)]
    command.env("TMPDIR", root.path());
    let mut child = command
        .args(["--exact", test, "--nocapture"])
        .env(mode_key, mode)
        .env(CHILD_TMPDIR, root.path())
        .stdout(
            stdout
                .reopen()
                .unwrap_or_else(|error| panic!("{context}: open stdout: {error}")),
        )
        .stderr(
            stderr
                .reopen()
                .unwrap_or_else(|error| panic!("{context}: open stderr: {error}")),
        )
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|error| panic!("{context}: spawn: {error}"));
    let wait = timeout(BOUND, child.wait()).await;
    let cleanup = match &wait {
        Ok(Ok(_)) => None,
        _ => Some(timeout(BOUND, child.kill()).await),
    };
    let stdout_bytes = std::fs::read(stdout.path());
    let stderr_bytes = std::fs::read(stderr.path());
    let streams = || {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            child_stream(&stdout_bytes),
            child_stream(&stderr_bytes)
        )
    };
    let status = match wait {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => panic!(
            "{context}: wait failed: {error}\nkill/reap: {cleanup:?}\n{}",
            streams()
        ),
        Err(elapsed) => panic!(
            "{context}: wait timed out after {BOUND:?}: {elapsed}\nkill/reap: {cleanup:?}\n{}",
            streams()
        ),
    };
    assert!(
        stdout_bytes.is_ok() && stderr_bytes.is_ok(),
        "{context} exited with {status}: read child streams\n{}",
        streams()
    );
    assert!(
        status.success() && child_stream(&stdout_bytes).contains("test result: ok. 1 passed;"),
        "{context} exited with {status}\n{}",
        streams()
    );
    Output {
        status,
        stdout: stdout_bytes.unwrap(),
        stderr: stderr_bytes.unwrap(),
    }
}

#[cfg(unix)]
#[test]
fn signals_are_armed_before_the_composition_factory() {
    const CHILD: &str = "TOOLKIT_TEST_OOP_COMPOSITION_SIGNAL_CHILD";
    const TEST: &str = "signals_are_armed_before_the_composition_factory";
    #[derive(Debug, PartialEq, Eq)]
    struct SignalObservation {
        result: Result<(), String>,
        process_cancelled_at_cleanup: bool,
        factory_calls: usize,
        workers_started: usize,
        workers_joined: usize,
        cleanup_entered: usize,
        cleanup_completed: usize,
        aborted: bool,
    }

    session(RenderedConfig::Unset, async {
        if std::env::var_os(CHILD).is_none() {
            run_child_test(TEST, CHILD, "1").await;
            return;
        }

        let s = Session::new(Case::Serve);
        let f = &s.fixture;
        let run = Box::pin(run_oop_with_composition(
            s.options(config()),
            compose_and_signal,
        ));
        let result = bounded(run).await;
        let log = f.lifecycle.lock();
        let (cancelled, _, _) = log.cleanup_state.expect("cleanup observed process token");
        assert_eq!(
            SignalObservation {
                result: result.map_err(|error| error.to_string()),
                process_cancelled_at_cleanup: cancelled,
                factory_calls: f.factory_config.lock().len(),
                workers_started: log.workers_started,
                workers_joined: log.workers_joined,
                cleanup_entered: log.cleanup_entered,
                cleanup_completed: log.cleanup_completed,
                aborted: f.abort.is_cancelled(),
            },
            SignalObservation {
                result: Ok(()),
                process_cancelled_at_cleanup: true,
                factory_calls: 1,
                workers_started: 1,
                workers_joined: 1,
                cleanup_entered: 1,
                cleanup_completed: 1,
                aborted: false,
            }
        );
    });
}
