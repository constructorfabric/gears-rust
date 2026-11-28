//! Observes private validation between the broker listener and the admitted fixture.
use super::cutover::{
    OWNER, Pair, UNKNOWN,
    auth_wire::{
        AuthenticateRequest, AuthenticateResponse, Failure, Identity,
        authenticate_response::Outcome, bearer_validation_client::BearerValidationClient,
    },
};
use anyhow::{Context as _, Result, ensure};
use reqwest::{Response as HttpResponse, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::{Barrier, oneshot};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request, Response, Status,
    codegen::{BoxFuture, Service, http},
};

const TRACE: &str = "0123456789abcdef0123456789abcdef";
const PARENT: &str = "00-0123456789abcdef0123456789abcdef-1111111111111111-01";
const CORE_WITNESS: &str = "cutover-pdp-request-witness";
const BROKER_WITNESS: &str = "cutover-broker-log-witness";
const GROUPS: &str = "/event-broker/v1/consumer-groups";
const CALL_WATCHDOG: Duration = Duration::from_secs(8);
const PEER_HOLD: Duration = Duration::from_secs(14);

#[derive(Clone, Copy)]
enum Reply {
    Relay,
    WrongCredential,
    Unavailable,
    Internal,
    Missing,
    Malformed,
    NonOk,
    Hold,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation {
    metadata: BTreeMap<String, Vec<String>>,
    workload_matches: bool,
    known_bearer: bool,
}

#[derive(Clone)]
struct Peer {
    core: String,
    workload: String,
    observations: Arc<Mutex<Vec<Observation>>>,
    reply: Arc<Mutex<Reply>>,
    overlap: Arc<Barrier>,
    received: Arc<tokio::sync::Notify>,
}

impl tonic::server::NamedService for Peer {
    const NAME: &'static str = "test_flight_control.v1.BearerValidation";
}

impl Service<http::Request<tonic::body::Body>> for Peer {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        assert_eq!(
            request.uri().path(),
            "/test_flight_control.v1.BearerValidation/Authenticate"
        );
        let peer = self.clone();
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::default());
            Ok(grpc.unary(peer, request).await)
        })
    }
}

impl tonic::server::UnaryService<AuthenticateRequest> for Peer {
    type Response = AuthenticateResponse;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, mut request: Request<AuthenticateRequest>) -> Self::Future {
        let mut metadata = BTreeMap::<String, Vec<String>>::new();
        let headers = request.metadata();
        let workload_matches = headers.get_all("x-toolkit-internal-token").iter().count() == 1
            && headers
                .get("x-toolkit-internal-token")
                .is_some_and(|v| v.as_bytes() == self.workload.as_bytes());
        for field in headers.iter() {
            use tonic::metadata::KeyAndValueRef::{Ascii, Binary};
            let (name, value) = match field {
                Ascii(key, value) => (key.as_str(), value.as_bytes()),
                Binary(key, _) => (key.as_str(), &[][..]),
            };
            match name {
                "te"
                | "content-type"
                | "user-agent"
                | "grpc-timeout"
                | "x-toolkit-internal-token" => {}
                "traceparent" | "x-request-id" => {
                    metadata
                        .entry(name.to_owned())
                        .or_default()
                        .push(String::from_utf8_lossy(value).into_owned());
                }
                other => {
                    metadata.entry(other.to_owned()).or_default();
                }
            }
        }
        let concurrent = metadata
            .get("x-request-id")
            .is_some_and(|values| values.iter().any(|id| id.starts_with("paired-")));
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Observation {
                metadata,
                workload_matches,
                known_bearer: request.get_ref().bearer_token == OWNER,
            });
        self.received.notify_one();
        let reply = *self
            .reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let core = self.core.clone();
        let overlap = self.overlap.clone();
        Box::pin(async move {
            if concurrent {
                tokio::time::timeout(CALL_WATCHDOG, overlap.wait())
                    .await
                    .map_err(|_| Status::deadline_exceeded("pair did not overlap"))?;
            }
            let outcome = match reply {
                Reply::Relay | Reply::WrongCredential => {
                    if matches!(reply, Reply::WrongCredential) {
                        request.metadata_mut().insert(
                            "x-toolkit-internal-token",
                            tonic::metadata::MetadataValue::from_static("fixture-wrong-private"),
                        );
                    }
                    let mut client = BearerValidationClient::connect(core)
                        .await
                        .map_err(|_| Status::unavailable("fixture disconnected"))?;
                    return client.authenticate(request).await;
                }
                Reply::Unavailable => Some(Outcome::Failure(Failure::Unavailable.into())),
                Reply::Internal => Some(Outcome::Failure(Failure::Internal.into())),
                Reply::Missing => None,
                Reply::Malformed => Some(Outcome::Identity(Identity::default())),
                Reply::NonOk => return Err(Status::unavailable("synthetic transport failure")),
                Reply::Hold => {
                    tokio::time::sleep(PEER_HOLD).await;
                    return Err(Status::deadline_exceeded("peer hold elapsed"));
                }
            };
            Ok(Response::new(AuthenticateResponse { outcome }))
        })
    }
}

struct Monitor {
    peer: Peer,
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Monitor {
    async fn start(pair: &Pair) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let peer = Peer {
            core: pair.grpc.clone(),
            workload: pair.workload.clone(),
            observations: Arc::default(),
            reply: Arc::new(Mutex::new(Reply::Relay)),
            overlap: Arc::new(Barrier::new(2)),
            received: Arc::default(),
        };
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(peer.clone())
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _shutdown = stop.await;
                }),
        );
        Ok(Self {
            peer,
            addr,
            shutdown,
            task,
        })
    }
    fn set_reply(&self, reply: Reply) {
        *self
            .peer
            .reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = reply;
    }

    fn count(&self) -> usize {
        self.peer
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
    fn observations_since(&self, start: usize) -> Vec<Observation> {
        self.peer
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[start..]
            .to_vec()
    }
    async fn finish(mut self) -> Result<()> {
        let _receiver = self.shutdown.send(());
        if let Ok(result) = tokio::time::timeout(CALL_WATCHDOG, &mut self.task).await {
            result??;
        } else {
            self.task.abort();
            ensure!(
                tokio::time::timeout(CALL_WATCHDOG, self.task)
                    .await?
                    .is_err_and(|e| e.is_cancelled()),
                "peer reap"
            );
            anyhow::bail!("peer exceeded shutdown deadline");
        }
        Ok(())
    }
}

fn groups(pair: &Pair, base: &str, parent: &str, id: &str) -> reqwest::RequestBuilder {
    pair.client
        .get(format!("{base}{GROUPS}"))
        .bearer_auth(OWNER)
        .header("traceparent", parent)
        .header("x-request-id", id)
        .timeout(CALL_WATCHDOG)
}

#[tokio::test]
async fn the_broker_recovers_authentication_after_flight_control_restarts() -> Result<()> {
    use super::cutover::Process;

    const RECOVERY_WINDOW: Duration = Duration::from_secs(30);
    const RETRY_AFTER: Duration = Duration::from_secs(5);
    const REQUEST_ID: &str = "auth-recovery";
    const JSON: &str = "application/json";
    const PROBLEM: &str = "application/problem+json";

    let mut pair = Pair::start_selected(&[])
        .await
        .context("original Core admission")?;
    let original_core = pair.core_pid()?;
    let listeners = format!("grpc={} gateway={}", pair.grpc, pair.gateway);
    let mut broker = pair.broker(
        REQUEST_ID,
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let broker_pid = broker.pid();
    broker.require_owned().context("initial broker ownership")?;
    let (base, ready) = pair.authenticated_page(&mut broker).await?;
    let broker_addr = broker
        .bound_http(&pair.workload)?
        .context("owned broker HTTP listener")?;
    assert_eq!(base, format!("http://{broker_addr}"));
    println!("original Core pid={original_core} {listeners} broker pid={broker_pid} http={base}");

    let owned = |broker: &mut Process, pair: &Pair, phase: &str| -> Result<()> {
        broker
            .require_owned()
            .with_context(|| format!("{phase} broker pid={broker_pid} http={base}"))?;
        assert_eq!(broker.pid(), broker_pid, "{phase} broker PID");
        assert_eq!(
            broker.bound_http(&pair.workload)?,
            Some(broker_addr),
            "{phase} broker listener"
        );
        Ok(())
    };
    let inspect = async |pair: &Pair, response: HttpResponse| -> Result<_> {
        let [mime, retry] = ["content-type", "retry-after"].map(|name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        });
        let status = response.status();
        let body = response.json::<Value>().await.context("public JSON body")?;
        let summary = format!(
            "status={} mime={mime} retry-after={retry:?} body={body}",
            status.as_u16()
        );
        let secrets = [OWNER, UNKNOWN, &pair.workload];
        ensure!(
            secrets.into_iter().all(|secret| !summary.contains(secret)),
            "credential present in public response"
        );
        let summary = summary.chars().take(512).collect::<String>();
        Ok((status, mime, retry, body, summary))
    };
    let receipt = |phase: &str, started: Instant, summary: &str| {
        println!(
            "{phase} {summary} call_us={} broker pid={broker_pid} http={base}",
            started.elapsed().as_micros()
        );
    };
    let owner_request = |pair: &Pair| groups(pair, &base, PARENT, REQUEST_ID);
    let probe = async |broker: &mut Process, pair: &Pair, phase: &str| {
        owned(broker, pair, phase)?;
        let health = pair.client.get(format!("{base}/healthz")).send().await?;
        assert_eq!(health.status(), StatusCode::OK, "{phase} liveness");
        assert!(health.text().await? == "ok", "{phase} liveness body");
        println!("{phase} health status=200 body=ok");
        let started = Instant::now();
        let (status, mime, retry, body, summary) =
            inspect(pair, owner_request(pair).send().await?).await?;
        owned(broker, pair, phase)?;
        receipt(phase, started, &summary);
        Ok::<_, anyhow::Error>((status, mime, retry, body, summary))
    };
    let owner_page =
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}});
    let owner_ok = (StatusCode::OK, JSON, &owner_page);
    let unavailable_status = StatusCode::SERVICE_UNAVAILABLE;
    let unavailable = json!({
        "type":"gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
        "title":"Service Unavailable","status":503,"detail":"Service temporarily unavailable",
        "instance":GROUPS,"trace_id":TRACE,"context":{"retry_after_seconds":5}
    });

    owned(&mut broker, &pair, "ready")?;
    let (status, mime, _, body, summary) = inspect(&pair, ready).await?;
    assert!(
        (status, mime.as_str(), &body) == owner_ok,
        "ready {summary}"
    );
    let (status, mime, _, body, summary) = probe(&mut broker, &pair, "before")
        .await
        .context("before owner probe")?;
    assert!(
        (status, mime.as_str(), &body) == owner_ok,
        "before {summary}"
    );

    pair.stop_core()
        .context("reap original Core before outage probe")?;
    let (status, mime, retry, body, outage_summary) = probe(&mut broker, &pair, "outage")
        .await
        .context("outage owner probe")?;
    assert_eq!(status, unavailable_status, "outage {outage_summary}");
    assert!(
        (mime.as_str(), retry.as_str(), &body) == (PROBLEM, "5", &unavailable),
        "outage {outage_summary}"
    );

    pair.restart_core().await.with_context(|| {
        format!("replacement Core admission {listeners}, last public response {outage_summary}")
    })?;
    let recovery_started = tokio::time::Instant::now();
    let deadline = recovery_started + RECOVERY_WINDOW;
    let in_window = || recovery_started.elapsed() < RECOVERY_WINDOW;
    let replacement_core = pair.core_pid().context("replacement Core ownership")?;
    assert_ne!(replacement_core, original_core, "replacement Core PID");
    println!("replacement Core pid={replacement_core} {listeners}");

    let mut last_response = outage_summary;
    for attempt in 1.. {
        let deadline_error = |last: &str| {
            format!(
                "recovery deadline attempt={attempt} elapsed={:?} original Core pid={original_core} replacement Core pid={replacement_core} broker pid={broker_pid} http={base} last public response {last}",
                recovery_started.elapsed(),
            )
        };
        ensure!(in_window(), "{}", deadline_error(&last_response));
        owned(&mut broker, &pair, "recovery request")?;
        assert_eq!(pair.core_pid()?, replacement_core);
        let started = Instant::now();
        let receive = async {
            let response = owner_request(&pair)
                .send()
                .await
                .context("recovery owner GET")?;
            last_response = format!("status={} body pending", response.status().as_u16());
            inspect(&pair, response).await
        };
        let (status, mime, retry, body, summary) = tokio::time::timeout_at(deadline, receive)
            .await
            .with_context(|| deadline_error(&last_response))?
            .with_context(|| format!("recovery attempt={attempt} {last_response}"))?;
        owned(&mut broker, &pair, "recovery response")?;
        assert_eq!(pair.core_pid()?, replacement_core);
        ensure!(in_window(), "{}", deadline_error(&summary));
        let recovery_us = recovery_started.elapsed().as_micros();
        receipt(
            &format!("recovery attempt={attempt} recovery_us={recovery_us}"),
            started,
            &summary,
        );
        if status == StatusCode::OK {
            assert!(
                (status, mime.as_str(), &body) == owner_ok,
                "recovery {summary}"
            );
            break;
        }
        assert_eq!(status, unavailable_status, "recovery {summary}");
        assert!(
            (mime.as_str(), retry.as_str(), &body) == (PROBLEM, "5", &unavailable),
            "recovery {summary}"
        );
        last_response = summary;
        tokio::time::timeout_at(deadline, tokio::time::sleep(RETRY_AFTER))
            .await
            .with_context(|| deadline_error(&last_response))?;
    }

    let started = Instant::now();
    let response = pair
        .client
        .get(format!("{base}{GROUPS}"))
        .bearer_auth(UNKNOWN)
        .header("traceparent", PARENT)
        .header("x-request-id", REQUEST_ID)
        .timeout(CALL_WATCHDOG)
        .send()
        .await
        .context("rejected credential GET")?;
    let (status, mime, _, body, summary) = inspect(&pair, response)
        .await
        .context("rejected credential problem")?;
    owned(&mut broker, &pair, "rejected credential")?;
    receipt("rejected", started, &summary);
    let unauthenticated = json!({
        "type":"gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
        "title":"Unauthenticated","status":401,"detail":"Authentication required",
        "instance":GROUPS,"trace_id":TRACE,"context":{"reason":"AUTHN_FAILED"}
    });
    assert!(
        (status, mime.as_str(), body) == (StatusCode::UNAUTHORIZED, PROBLEM, unauthenticated),
        "rejected credential {summary}"
    );
    broker.stop()?;
    pair.finish()
}

#[tokio::test]
async fn the_broker_reports_private_authentication_faults_at_the_tenant_route() -> Result<()> {
    let mut pair = Pair::start().await?;
    let monitor = Monitor::start(&pair).await?;
    let mut broker = pair.broker(
        "faults",
        &["--e2e-flight-control", "run"],
        Some(&format!("http://{}", monitor.addr)),
    )?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(
        page.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    let expected_attempt = Observation {
        metadata: BTreeMap::from([
            ("traceparent".into(), vec![PARENT.into()]),
            ("x-request-id".into(), vec!["fault-oracle".into()]),
        ]),
        workload_matches: true,
        known_bearer: true,
    };
    let fault_request = |pair: &Pair| groups(pair, &base, PARENT, "fault-oracle");
    let before = monitor.count();
    let initial = fault_request(&pair).send().await?;
    assert_eq!(
        monitor.observations_since(before),
        std::slice::from_ref(&expected_attempt)
    );
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(
        initial.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    pair.stop_core()?;
    let before = monitor.count();
    let unavailable = fault_request(&pair).send().await?;
    let retry = json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~","title":"Service Unavailable","status":503,"detail":"Service temporarily unavailable","instance":GROUPS,"trace_id":TRACE,"context":{"retry_after_seconds":5}});
    let internal = json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~","title":"Internal","status":500,"detail":"An internal error occurred. Please retry later.","instance":GROUPS,"trace_id":TRACE,"context":{}});
    assert_eq!(
        monitor.observations_since(before),
        std::slice::from_ref(&expected_attempt)
    );
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        unavailable.headers()["content-type"],
        "application/problem+json"
    );
    assert_eq!(
        unavailable
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("5")
    );
    assert_eq!(unavailable.json::<Value>().await?, retry);
    pair.restart_core().await?;
    let before = monitor.count();
    let live = fault_request(&pair).send().await?;
    assert_eq!(
        monitor.observations_since(before),
        std::slice::from_ref(&expected_attempt)
    );
    assert_eq!(live.status(), StatusCode::OK);
    assert_eq!(
        live.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    for (reply, status) in [
        (Reply::WrongCredential, 503),
        (Reply::Unavailable, 503),
        (Reply::Internal, 500),
        (Reply::Missing, 500),
        (Reply::Malformed, 500),
        (Reply::NonOk, 503),
        (Reply::Hold, 503),
    ] {
        monitor.set_reply(reply);
        let before = monitor.count();
        let start = Instant::now();
        let r = groups(&pair, &base, PARENT, "fault-oracle").send().await?;
        if matches!(reply, Reply::Hold) {
            assert!(start.elapsed() >= Duration::from_secs(4) && start.elapsed() < CALL_WATCHDOG);
        }
        assert_eq!(monitor.count(), before + 1);
        assert_eq!(r.status().as_u16(), status);
        assert_eq!(r.headers()["content-type"], "application/problem+json");
        assert_eq!(
            r.headers().get("retry-after").and_then(|v| v.to_str().ok()),
            (status == 503).then_some("5")
        );
        assert_eq!(
            r.json::<Value>().await?,
            if status == 503 { &retry } else { &internal }.clone()
        );
    }
    monitor.set_reply(Reply::Hold);
    let before = monitor.count();
    let pending = tokio::spawn(groups(&pair, &base, PARENT, "cancelled-caller").send());
    tokio::time::timeout(CALL_WATCHDOG, async {
        while monitor.count() == before {
            monitor.peer.received.notified().await;
        }
    })
    .await?;
    pending.abort();
    assert!(
        tokio::time::timeout(CALL_WATCHDOG, pending)
            .await?
            .is_err_and(|error| error.is_cancelled())
    );
    assert_eq!(monitor.count(), before + 1);
    monitor.set_reply(Reply::Relay);
    let recovered = groups(&pair, &base, PARENT, "after-caller-cancel")
        .send()
        .await?;
    assert_eq!(recovered.status(), 200);
    assert_eq!(
        recovered.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    assert_eq!(monitor.count(), before + 2);
    let create = |id| {
        pair.client
            .post(format!("{base}{GROUPS}"))
            .bearer_auth(OWNER)
            .header("traceparent", PARENT)
            .header("x-request-id", id)
            .timeout(CALL_WATCHDOG)
    };
    monitor.set_reply(Reply::Unavailable);
    let denied = create("denied-create").send().await?;
    assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(denied.headers()["retry-after"], "5");
    assert_eq!(denied.json::<Value>().await?, retry);
    monitor.set_reply(Reply::Relay);
    let unchanged = groups(&pair, &base, PARENT, "after-denied-create")
        .send()
        .await?;
    assert_eq!(unchanged.status(), StatusCode::OK);
    assert_eq!(
        unchanged.json::<Value>().await?,
        json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    let allowed = create("allowed-create").send().await?;
    assert_eq!(allowed.status(), StatusCode::CREATED);
    let group = allowed.json::<Value>().await?;
    let id = group["id"].as_str().context("created group id")?;
    let created_at = group["created_at"]
        .as_str()
        .context("created group timestamp")?;
    let expected = json!({"id":id,"created_at":created_at,"kind":"anonymous","description":null,
        "owner_principal":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","tenant":"00000000-0000-0000-0000-000000000002"});
    assert_eq!(group, expected);
    let changed = groups(&pair, &base, PARENT, "after-allowed-create")
        .send()
        .await?;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_eq!(
        changed.json::<Value>().await?,
        json!({"items":[expected],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
    );
    let core_log = pair.core_log()?;
    let completed = core_log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event["target"] == "access_log"
                && matches!(
                    event["fields"]["request_id"].as_str(),
                    Some("denied-create" | "allowed-create")
                )
        })
        .map(|event| {
            (
                event["fields"]["request_id"].clone(),
                event["fields"]["method"].clone(),
                event["fields"]["uri"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        completed,
        [(
            json!("allowed-create"),
            json!("POST"),
            json!("/authz-resolver/v1/evaluate")
        )]
    );
    broker.stop()?;
    monitor.finish().await?;
    pair.finish()
}

async fn first_frame(stream: &mut HttpResponse) -> Result<Vec<u8>> {
    tokio::time::timeout(CALL_WATCHDOG, async {
        let mut bytes = Vec::new();
        while !bytes.windows(2).any(|p| p == b"\n\n") {
            ensure!(bytes.len() < 16_384, "oversized topology frame");
            bytes.extend_from_slice(
                &stream
                    .chunk()
                    .await?
                    .context("stream closed before topology")?,
            );
        }
        Ok::<_, anyhow::Error>(bytes)
    })
    .await?
}

#[tokio::test]
async fn the_broker_forwards_only_request_scoped_correlation_to_the_private_service() -> Result<()>
{
    let pair = Pair::start().await?;
    let monitor = Monitor::start(&pair).await?;
    let mut broker = pair.broker(
        "correlation",
        &["--e2e-flight-control", "run"],
        Some(&format!("http://{}", monitor.addr)),
    )?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), StatusCode::OK);
    let before = monitor.count();
    let second = "00-1123456789abcdef0123456789abcdef-2222222222222222-01";
    let (first, last) = tokio::try_join!(
        groups(&pair, &base, PARENT, "paired-a")
            .header("x-caller-extra", "discard-me")
            .send(),
        groups(&pair, &base, second, "paired-b").send()
    )?;
    for r in [first, last] {
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.json::<Value>().await?,
            json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
        );
    }
    {
        let records = monitor
            .peer
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut two = records[before..].iter().collect::<Vec<_>>();
        two.sort_by_key(|o| o.metadata.get("x-request-id"));
        assert_eq!(two.len(), 2);
        for (record, (trace, id)) in two
            .into_iter()
            .zip([(PARENT, "paired-a"), (second, "paired-b")])
        {
            assert_eq!(
                record,
                &Observation {
                    metadata: BTreeMap::from([
                        ("traceparent".into(), vec![trace.into()]),
                        ("x-request-id".into(), vec![id.into()])
                    ]),
                    workload_matches: true,
                    known_bearer: true
                }
            );
        }
    }
    for (parents, ids, expected_ids) in [
        (vec![], vec![], vec![]),
        (vec!["invalid"], vec!["bad id"], vec![]),
        (
            vec![PARENT, second],
            vec!["first-id", "second-id"],
            vec!["first-id"],
        ),
    ] {
        let mut request = pair
            .client
            .get(format!("{base}{GROUPS}"))
            .bearer_auth(OWNER);
        for parent in parents {
            request = request.header("traceparent", parent);
        }
        for id in ids {
            request = request.header("x-request-id", id);
        }
        let before = monitor.count();
        assert_eq!(request.send().await?.status(), StatusCode::OK);
        let expected = if expected_ids.is_empty() {
            BTreeMap::new()
        } else {
            BTreeMap::from([(
                "x-request-id".into(),
                expected_ids.into_iter().map(str::to_owned).collect(),
            )])
        };
        let records = monitor
            .peer
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            &records[before..],
            [Observation {
                metadata: expected,
                workload_matches: true,
                known_bearer: true
            }]
        );
    }
    let before = monitor.count();
    let mut durations = Vec::new();
    for index in 0..100 {
        let start = Instant::now();
        let page = groups(&pair, &base, PARENT, &format!("get-{index}"))
            .send()
            .await?;
        assert_eq!(page.status(), StatusCode::OK);
        assert_eq!(
            page.json::<Value>().await?,
            json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
        );
        durations.push(start.elapsed());
    }
    assert_eq!(monitor.count() - before, 100);
    assert_eq!(
        monitor.observations_since(before),
        (0..100)
            .map(|index| Observation {
                metadata: BTreeMap::from([
                    ("traceparent".into(), vec![PARENT.into()]),
                    ("x-request-id".into(), vec![format!("get-{index}")])
                ]),
                workload_matches: true,
                known_bearer: true,
            })
            .collect::<Vec<_>>()
    );
    durations.sort();
    println!(
        "GET groups 100 responses, 100 private calls. p50 {} us, p95 {} us",
        durations[49].as_micros(),
        durations[94].as_micros()
    );
    let mut urls = Vec::with_capacity(100);
    for _ in 0..100 {
        urls.push(pair.subscription(&base).await?);
    }
    let before = monitor.count();
    let mut streams = Vec::with_capacity(100);
    let mut timings = Vec::with_capacity(100);
    for (index, url) in urls.into_iter().enumerate() {
        let start = Instant::now();
        let stream = pair
            .client
            .get(url)
            .header("x-request-id", format!("open-{index}"))
            .header("traceparent", PARENT)
            .bearer_auth(OWNER)
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        assert_eq!(stream.status(), StatusCode::OK);
        assert_eq!(stream.headers()["content-type"], "text/event-stream");
        timings.push(start.elapsed());
        streams.push(stream);
    }
    assert_eq!(monitor.count() - before, 100);
    assert_eq!(
        monitor.observations_since(before),
        (0..100)
            .map(|index| Observation {
                metadata: BTreeMap::from([
                    ("traceparent".into(), vec![PARENT.into()]),
                    ("x-request-id".into(), vec![format!("open-{index}")])
                ]),
                workload_matches: true,
                known_bearer: true,
            })
            .collect::<Vec<_>>()
    );
    let before_frames = monitor.count();
    for mut stream in streams {
        let frame = first_frame(&mut stream).await?;
        let text = std::str::from_utf8(&frame)?;
        let topology = text.split("\n\n").next().context("first SSE frame")?;
        let mut lines = topology.lines();
        assert_eq!(lines.next(), Some("event: topology"));
        let payload = lines
            .next()
            .and_then(|s| s.strip_prefix("data: "))
            .context("SSE data")?;
        assert_eq!(
            serde_json::from_str::<Value>(payload)?,
            json!({"kind":"topology","topology_version":1,"assigned":[]})
        );
        assert_eq!(lines.next(), None);
    }
    assert_eq!(monitor.count() - before, 100);
    assert_eq!(monitor.count(), before_frames);
    timings.sort();
    println!(
        "100 SSE opens, 100 private calls, 100 topology frames, 0 additional private calls. p50 {} us, p95 {} us",
        timings[49].as_micros(),
        timings[94].as_micros()
    );
    broker.stop()?;
    monitor.finish().await?;
    pair.finish()
}

#[tokio::test]
async fn the_private_fixture_keeps_test_credentials_out_of_broker_responses_and_logs() -> Result<()>
{
    let pair = Pair::start().await?;
    let mut broker = pair.broker(
        "confidentiality",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), StatusCode::OK);
    let screen = |bytes: &[u8]| -> Result<()> {
        for secret in [OWNER, UNKNOWN, &pair.workload] {
            ensure!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "credential present in collected stream"
            );
        }
        Ok(())
    };
    let created = pair
        .client
        .post(format!("{base}{GROUPS}"))
        .bearer_auth(OWNER)
        .header("traceparent", PARENT)
        .header("x-request-id", CORE_WITNESS)
        .send()
        .await?;
    assert_eq!(created.status(), 201);
    let denied: HttpResponse = pair
        .client
        .get(format!("{base}{GROUPS}"))
        .bearer_auth(UNKNOWN)
        .header("traceparent", PARENT)
        .header("x-request-id", BROKER_WITNESS)
        .send()
        .await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    for header in denied.headers().values() {
        screen(header.as_bytes())?;
    }
    assert_eq!(denied.headers()["content-type"], "application/problem+json");
    let body = denied
        .bytes()
        .await
        .map_err(|_| anyhow::anyhow!("failed to read public Problem"))?;
    screen(&body)?;
    let body = std::str::from_utf8(&body).context("public Problem encoding")?;
    let problem: Value =
        serde_json::from_str(body).map_err(|_| anyhow::anyhow!("invalid public Problem"))?;
    screen(problem.to_string().as_bytes())?;
    assert_eq!(
        problem,
        json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~","title":"Unauthenticated","status":401,"detail":"Authentication required","instance":GROUPS,"trace_id":TRACE,"context":{"reason":"AUTHN_FAILED"}})
    );
    let mut private = BearerValidationClient::connect(pair.grpc.clone()).await?;
    let client_debug = format!("{private:?}");
    screen(client_debug.as_bytes())?;
    let mut request = Request::new(AuthenticateRequest {
        bearer_token: OWNER.into(),
    });
    let redacted = format!("{:?}", request.get_ref());
    screen(redacted.as_bytes())?;
    assert_eq!(redacted, "AuthenticateRequest { .. }");
    request
        .metadata_mut()
        .insert("x-toolkit-internal-token", pair.workload.parse()?);
    let private = private
        .authenticate(request)
        .await
        .map_err(|_| anyhow::anyhow!("private validation failed"))?
        .into_inner();
    let decoded = format!("{private:?}");
    screen(decoded.as_bytes())?;
    assert_eq!(
        private,
        AuthenticateResponse {
            outcome: Some(Outcome::Identity(Identity {
                subject_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
                subject_type: Some("user".into()),
                subject_tenant_id: "00000000-0000-0000-0000-000000000002".into(),
                token_scopes: vec![]
            }))
        }
    );
    broker.stop()?;
    let broker_log = broker.safe_log(&pair.workload)?;
    let core_log = pair.core_log()?;
    let completed = core_log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event["target"] == "access_log" && event["fields"]["request_id"] == CORE_WITNESS
        })
        .map(|event| {
            (
                event["fields"]["method"].clone(),
                event["fields"]["uri"].clone(),
            )
        })
        .collect::<Vec<_>>();
    ensure!(
        completed == [(json!("POST"), json!("/authz-resolver/v1/evaluate"))],
        "unexpected completed PDP requests"
    );
    for (collected, benign) in [
        (broker_log.as_str(), BROKER_WITNESS),
        (&core_log, CORE_WITNESS),
        (body, "Unauthenticated"),
        (&client_debug, "BearerValidationClient"),
        (&redacted, "AuthenticateRequest"),
        (&decoded, "00000000-0000-0000-0000-000000000002"),
    ] {
        ensure!(collected.contains(benign), "missing collection control");
    }
    println!("6 collected streams, 6 positive controls, 18 credential scans, 0 matches");
    pair.finish()
}
