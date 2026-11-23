//! Shared process fixture for the standalone and consumer-group rebalance targets.

// Follow-up: rework `common` into a shared test-support library crate (where `pub`
// items are API and are never flagged), then drop `dead_code` here.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

mod cutover;

use cutover::{OWNER, Pair, Process};
pub use cutover::{OWNER_SUBJECT, OWNER_TENANT};

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};

const STARTUP: Duration = Duration::from_secs(30);
const STARTUP_RECHECK: Duration = Duration::from_millis(40);
const REQUEST_BOUND: Duration = Duration::from_secs(10);
const PUBLISH_RECHECK: Duration = Duration::from_millis(20);
const TOPIC: &str = "gts.cf.core.events.topic.v1~";
const EVENT: &str = "gts.cf.core.events.event.v1~";

/// Splits declarative fixtures into what each side of the process receives.
///
/// A test writes what it means - a topic with a partition count, an event type
/// with a topic and a payload contract - and this puts each half where the
/// running process expects it: the topic and event type become the documents
/// `types-registry` holds (an instance, and a derived type schema of the
/// abstract event base), while the partition count becomes `event-broker`
/// configuration, because a topic carries no count of its own.
///
/// An event-type fixture may name `partition_key` to point the type at a member
/// other than the tenant the base defaults to.
fn split_fixtures(entities: &[Value]) -> (Vec<Value>, Vec<(String, i64)>) {
    let mut documents = Vec::new();
    let mut partitions = Vec::new();

    for entity in entities {
        let id = entity["id"]
            .as_str()
            .expect("a fixture entity names its id");
        if id.starts_with(TOPIC) && !id.ends_with('~') {
            partitions.push((
                id.to_owned(),
                entity["partitions"]
                    .as_i64()
                    .expect("a topic fixture names its partition count"),
            ));
            documents.push(serde_json::json!({
                "id": id,
                "description": "a topic this integration test publishes to",
            }));
        } else if id.starts_with(EVENT) {
            let topic = entity["topic_id"]
                .as_str()
                .expect("an event-type fixture names its topic");
            let mut event = entity.clone();
            event["topic"] = serde_json::json!(topic);
            let fixtures = event_broker::test_support::StaticTypesRegistry::of(serde_json::json!([
                { "id": topic, "partitions": 1 },
                event,
            ]));
            let [schema] = fixtures.event_type_schemas() else {
                panic!("expected exactly one schema for {id}");
            };
            documents.push(schema.clone());
        } else {
            panic!("fixture '{id}' is neither a topic instance nor an event type");
        }
    }

    (documents, partitions)
}

/// Where the broker keeps its event log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLog {
    /// The log lives in the broker process.
    Memory,
    /// The log lives in `events.db` under the broker home and survives a restart.
    Sqlite,
}

impl EventLog {
    fn backend_entry(self, home: &Path) -> Value {
        match self {
            Self::Memory => {
                json!({"type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"})
            }
            Self::Sqlite => json!({
                "type": "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~",
                "path": home.join("events.db"),
            }),
        }
    }
}

/// The `streaming:` keys a test sets on the server it boots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "each field is named after the config key it sets, unit suffix included"
)]
pub struct Streaming {
    /// `heartbeat_interval_secs`: the idle cadence of an open stream.
    pub heartbeat_secs: u32,
    /// `authz_scope_ttl_secs`: how long a consume scope window serves.
    pub authz_scope_ttl_secs: u32,
    /// `authz_eval_deadline_secs`: the longest the broker waits on the PDP.
    pub authz_eval_deadline_secs: u32,
}

impl Default for Streaming {
    /// Mirrors the broker's default heartbeat, scope TTL and evaluation deadline.
    fn default() -> Self {
        Self {
            heartbeat_secs: 5,
            authz_scope_ttl_secs: 60,
            authz_eval_deadline_secs: 10,
        }
    }
}

/// Owns both children and the owner's broker HTTP connection pool.
pub struct TestServer {
    // Declaration order stops the broker before Core on unwinding.
    broker: Process,
    pair: Pair,
    listener: SocketAddr,
    client: Client,
    documents: Vec<Value>,
}

#[derive(Debug)]
pub struct Refused {
    pub status: ExitStatus,
    pub logs: Vec<String>,
}

impl TestServer {
    pub async fn start(entities: Vec<Value>) -> Self {
        Self::boot(entities, Streaming::default(), EventLog::Memory).await
    }

    pub async fn start_with_event_log(entities: Vec<Value>, event_log: EventLog) -> Self {
        Self::boot(entities, Streaming::default(), event_log).await
    }

    /// On a quiet stream, the heartbeat bounds how long it takes the broker
    /// to detect a dropped connection on a failed write.
    pub async fn start_with_heartbeat(entities: Vec<Value>, heartbeat_secs: u32) -> Self {
        let streaming = Streaming {
            heartbeat_secs,
            ..Streaming::default()
        };
        Self::boot(entities, streaming, EventLog::Memory).await
    }

    pub async fn start_with_streaming(entities: Vec<Value>, streaming: Streaming) -> Self {
        Self::boot(entities, streaming, EventLog::Memory).await
    }

    pub async fn start_refused(entities: Vec<Value>, streaming: Streaming) -> Refused {
        let (mut pair, _) = Self::configure(entities, streaming, EventLog::Memory)
            .await
            .expect("prepare refused split-process fixture");
        let mut broker = Self::launch(&pair, "broker-refused").expect("launch refused broker");
        let status = broker.exited().await.unwrap_or_else(|error| {
            panic!(
                "wait for refused broker: {error}\nCaptured output:
{}",
                broker.safe_log(&pair.workload).unwrap_or_default()
            )
        });
        let logs = broker
            .safe_log(&pair.workload)
            .expect("read refused broker output");
        drop(broker);
        pair.finish_selected()
            .await
            .expect("stop Core after broker refusal");
        Refused {
            status,
            logs: logs.lines().map(str::to_owned).collect(),
        }
    }

    async fn boot(entities: Vec<Value>, streaming: Streaming, event_log: EventLog) -> Self {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build owner HTTP client");
        let (pair, documents) = Self::configure(entities, streaming, event_log)
            .await
            .expect("prepare split-process fixture");
        let mut broker = Self::launch(&pair, "broker").expect("launch broker");
        pair.await_base_schemas(&mut broker)
            .await
            .expect("broker publishes its base schemas");
        pair.admit_documents(&mut broker, &documents)
            .await
            .expect("admit the fixture documents after broker publication");
        let listener = Self::ready(&pair, &mut broker)
            .await
            .expect("await broker readiness");
        pair.await_catalogue(&mut broker, &format!("http://{listener}"), &documents)
            .await
            .expect("broker serves the fixture catalogue");
        Self {
            broker,
            pair,
            listener,
            client,
            documents,
        }
    }

    async fn configure(
        entities: Vec<Value>,
        streaming: Streaming,
        event_log: EventLog,
    ) -> Result<(Pair, Vec<Value>)> {
        let (documents, partitions) = split_fixtures(&entities);
        let pair = Pair::start_selected(&documents).await?;
        println!(
            "Core {} serves registry at {} and directory at {}",
            pair.core_pid()?,
            pair.gateway,
            pair.grpc
        );
        let mut config = pair.broker_config()?;
        let home = config["server"]["home_dir"]
            .as_str()
            .context("broker home")?;
        let mut topics = serde_json::Map::from_iter([(TOPIC.to_owned(), json!({}))]);
        topics.extend(
            partitions
                .into_iter()
                .map(|(topic, count)| (topic, json!({"partitions":count}))),
        );
        config["gears"]["event-broker"]["config"] = json!({
            "mode": "standalone",
            "process": {"cluster":{"mode":"memory"}},
            "workers": {"specification_refresh_interval_secs": 1},
            "backends": {"main": event_log.backend_entry(Path::new(home))},
            "backend_routing": { (TOPIC): "main" },
            "streaming": {
                "heartbeat_interval_secs": streaming.heartbeat_secs,
                "read_batch_max_events": 256,
                "read_batch_max_bytes": 1_048_576,
                "progress_drift_threshold": 1000,
                "progress_min_interval_secs": 30,
                "authz_scope_ttl_secs": streaming.authz_scope_ttl_secs,
                "authz_eval_deadline_secs": streaming.authz_eval_deadline_secs,
            },
            "topics": topics,
        });
        pair.write_broker_config(&config)?;
        Ok((pair, documents))
    }

    fn launch(pair: &Pair, name: &str) -> Result<Process> {
        pair.core_pid()?;
        pair.broker(name, &["--e2e-flight-control", "run"], Some(&pair.grpc))
    }

    async fn ready(pair: &Pair, broker: &mut Process) -> Result<SocketAddr> {
        let started = Instant::now();
        let bound = timeout(STARTUP, async {
            loop {
                pair.core_pid()?;
                broker.require_owned()?;
                // Only the owned child's bind event supplies a probe address.
                if let Some(addr) = broker.bound_http(&pair.workload)? {
                    ensure!(
                        pair.gateway.port_or_known_default() != Some(addr.port()),
                        "Core and broker HTTP listeners must be distinct"
                    );
                    let probe = pair
                        .client
                        .get(format!("http://{addr}/readyz"))
                        .send()
                        .await;
                    if probe.is_ok_and(|response| response.status() == StatusCode::OK) {
                        broker.require_owned()?;
                        pair.core_pid()?;
                        println!(
                            "Broker {} ready at {addr} in {} ms",
                            broker.pid(),
                            started.elapsed().as_millis()
                        );
                        return Ok(addr);
                    }
                }
                sleep(STARTUP_RECHECK).await;
            }
        })
        .await;
        match bound {
            Ok(Ok(addr)) => Ok(addr),
            Ok(Err(error)) => {
                let output = broker.safe_log(&pair.workload)?;
                Err(error.context(format!("broker failed before readiness:\n{output}")))
            }
            Err(_) => {
                let output = broker.safe_log(&pair.workload)?;
                anyhow::bail!("broker readiness timed out after {STARTUP:?}:\n{output}")
            }
        }
    }

    /// Reap the broker, then start a replacement in its existing data home.
    pub async fn restart(&mut self) {
        let core_pid = self.pair.core_pid().expect("live Core before restart");
        let old_pid = self.broker.pid();
        self.broker.stop().expect("reap old broker");
        self.pair
            .registry_documents(&self.documents)
            .await
            .expect("Core retains its admission");
        let mut broker =
            Self::launch(&self.pair, "broker-restarted").expect("launch replacement broker");
        let listener = Self::ready(&self.pair, &mut broker)
            .await
            .expect("await replacement broker");
        self.pair
            .await_catalogue(&mut broker, &format!("http://{listener}"), &self.documents)
            .await
            .expect("replacement broker serves the fixture catalogue");
        assert_ne!(old_pid, broker.pid());
        assert_ne!(self.listener, listener);
        assert_eq!(
            self.pair.core_pid().expect("live Core after restart"),
            core_pid
        );
        self.broker = broker;
        self.listener = listener;
    }

    pub fn logs(&self) -> Vec<String> {
        self.broker
            .safe_log(&self.pair.workload)
            .expect("read broker output")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.pair
            .core_pid()
            .expect("Core must own the platform listener");
        self.broker
            .verify_identity()
            .expect("broker must own the tenant listener");
        self.client
            .request(method, format!("http://{}{path}", self.listener))
            .bearer_auth(OWNER)
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.request(Method::GET, path).timeout(REQUEST_BOUND)
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.request(Method::POST, path).timeout(REQUEST_BOUND)
    }

    pub fn delete(&self, path: &str) -> RequestBuilder {
        self.request(Method::DELETE, path).timeout(REQUEST_BOUND)
    }

    pub async fn open_stream(&self, path: &str) -> Response {
        let started = Instant::now();
        let response = timeout(REQUEST_BOUND, self.request(Method::GET, path).send())
            .await
            .expect("stream opens within the bound")
            .expect("open SSE stream");
        println!(
            "Broker {} SSE headers returned {} in {} ms",
            self.broker.pid(),
            response.status(),
            started.elapsed().as_millis()
        );
        response
    }

    /// Retry the same authenticated publish while ingest returns `503`.
    /// The caller distinguishes an accepted event from a deduped retry.
    pub async fn publish_retrying_while_unavailable(&self, body: &Value) -> Response {
        let deadline = Instant::now() + REQUEST_BOUND;
        let mut attempts = 0_u32;
        loop {
            let response = self
                .post("/event-broker/v1/events")
                .json(body)
                .send()
                .await
                .expect("publish request");
            if response.status() != StatusCode::SERVICE_UNAVAILABLE || Instant::now() >= deadline {
                return response;
            }
            attempts += 1;
            eprintln!("publish attempt {attempts} returned 503, retrying");
            sleep(PUBLISH_RECHECK).await;
        }
    }

    pub async fn finish(mut self) {
        self.broker.finish().await.expect("stop broker");
        self.broker
            .safe_log(&self.pair.workload)
            .expect("screen broker output");
        self.pair.finish_selected().await.expect("stop Core");
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if std::thread::panicking() {
            match self.broker.safe_log(&self.pair.workload) {
                Ok(output) => eprintln!("Captured broker output:\n{output}"),
                Err(error) => eprintln!("Broker output unavailable: {error}"),
            }
        }
    }
}

/// Reads SSE events (`event: <kind>\ndata: <json>\n\n`) off a real
/// `GET .../events:sse` response one at a time, buffering across however
/// `reqwest`'s `bytes_stream()` happens to chunk the underlying TCP socket -
/// unlike the in-crate router tests (`streaming_tests.rs`), a real socket
/// gives no guarantee that one `Frame` arrives as exactly one read, or that
/// two frames don't arrive in the same read.
pub struct SseFrameReader {
    stream:
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buf: String,
}

impl SseFrameReader {
    pub fn new(response: reqwest::Response) -> Self {
        Self {
            stream: Box::pin(response.bytes_stream()),
            buf: String::new(),
        }
    }

    pub async fn next_frame(&mut self, timeout: Duration) -> (String, Value) {
        use tokio_stream::StreamExt as _;

        tokio::time::timeout(timeout, async {
            loop {
                if let Some(end) = self.buf.find("\n\n") {
                    let raw = self.buf[..end].to_owned();
                    self.buf.drain(..=end + 1);
                    let event = raw
                        .strip_prefix("event: ")
                        .expect("SSE block must start with 'event: '");
                    let (kind, data) = event
                        .split_once("\ndata: ")
                        .expect("SSE block must contain a 'data: ' line");
                    let parsed: Value = serde_json::from_str(data).expect("SSE data must be JSON");
                    return (kind.to_owned(), parsed);
                }
                let chunk = self
                    .stream
                    .next()
                    .await
                    .expect("SSE stream must not end before a full frame arrives")
                    .expect("SSE chunk must not be a transport error");
                self.buf.push_str(&String::from_utf8_lossy(&chunk));
            }
        })
        .await
        .expect("must receive an SSE frame within the timeout")
    }

    /// Non-panicking variant of `next_frame`: `None` if no complete frame
    /// arrives within `timeout`, or the connection closes - used to assert
    /// the ABSENCE of a frame (e.g. proving a partition lost in a rebalance
    /// stops being delivered), where `next_frame`'s panic-on-timeout would
    /// be the wrong tool.
    pub async fn try_next_frame(&mut self, timeout: Duration) -> Option<(String, Value)> {
        use tokio_stream::StreamExt as _;

        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let raw = self.buf[..end].to_owned();
                self.buf.drain(..=end + 1);
                let event = raw
                    .strip_prefix("event: ")
                    .expect("SSE block must start with 'event: '");
                let (kind, data) = event
                    .split_once("\ndata: ")
                    .expect("SSE block must contain a 'data: ' line");
                let parsed: Value = serde_json::from_str(data).expect("SSE data must be JSON");
                return Some((kind.to_owned(), parsed));
            }
            match tokio::time::timeout(timeout, self.stream.next()).await {
                Ok(Some(Ok(chunk))) => self.buf.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(Some(Err(_))) => panic!("SSE chunk must not be a transport error"),
                Ok(None) | Err(_) => return None,
            }
        }
    }
}
