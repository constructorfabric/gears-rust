//! Exercises limits and route policy on the running worker and its gateway.
use super::cutover::{OWNER, Pair};
use anyhow::{Context as _, Result, ensure};
use bytes::Bytes;
use reqwest::{Body, StatusCode};
use serde_json::{Value, json};
use std::{fs, io, path::Path, time::Duration};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::time::{Instant, timeout};

const CAP: usize = 16_777_216;
const TRACE: &str = "0123456789abcdef0123456789abcdef";
const PARENT: &str = "00-0123456789abcdef0123456789abcdef-1111111111111111-01";
const GROUPS: &str = "/event-broker/v1/consumer-groups";
const SUBS: &str = "/event-broker/v1/subscriptions";
const LIMIT: Duration = Duration::from_secs(30);
const WATCHDOG: Duration = Duration::from_secs(42);

fn configure(pair: &Pair) -> Result<()> {
    let mut cfg = pair.broker_config()?;
    let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../config/event-broker-standalone.yaml");
    let yaml = fs::read_to_string(sample)?;
    let options = serde_saphyr::options! {
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
    };
    let mut loaded: Value = serde_saphyr::from_str_with_options(&yaml, options)
        .map_err(|_| anyhow::anyhow!("failed to parse standalone worker sample"))?;
    let mut http = loaded
        .get_mut("oop_http")
        .map(Value::take)
        .context("standalone worker sample is missing oop_http")?;
    ensure!(
        http.get("request_limits").is_some(),
        "standalone worker sample is missing oop_http.request_limits"
    );
    http["listen_addr"] = cfg["oop_http"]["listen_addr"].take();
    http["advertise_uri"] = cfg["oop_http"]["advertise_uri"].take();
    *http
        .get_mut("internal_auth")
        .and_then(|auth| auth.get_mut("secret"))
        .context("standalone worker sample is missing its workload credential field")? =
        json!(pair.workload);
    cfg["oop_http"] = http;
    pair.write_broker_config(&cfg)
}

#[tokio::test]
async fn the_broker_enforces_its_request_limits_and_keeps_streams_alive_after_headers() -> Result<()>
{
    let pair = Pair::start().await?;
    configure(&pair)?;
    let mut broker = pair.broker("limits", &["--e2e-flight-control", "run"], Some(&pair.grpc))?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), 200);
    for chunked in [false, true] {
        for size in [CAP, CAP + 1] {
            let mut bytes = br#"{"client_agent":"agent/"}"#.to_vec();
            bytes.resize(size, b' ');
            let body = if chunked {
                Body::wrap_stream(tokio_stream::iter([Ok::<_, io::Error>(Bytes::from(bytes))]))
            } else {
                Body::from(bytes)
            };
            let response = pair
                .client
                .post(format!("{base}{GROUPS}"))
                .bearer_auth(OWNER)
                .header("traceparent", PARENT)
                .header("content-type", "application/json")
                .body(body)
                .timeout(WATCHDOG)
                .send()
                .await?;
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let status = response.status();
            let problem = response.json::<Value>().await?;
            if size == CAP {
                assert_eq!(status, 400);
                assert_eq!(
                    problem,
                    json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~","title":"Invalid Argument","status":400,"detail":"Request validation failed","instance":GROUPS,"trace_id":TRACE,"context":{"field_violations":[{"field":"client_agent","description":"must follow the RFC 9110 User-Agent grammar: product *( SP ( product / comment ) )","reason":"user_agent_grammar"}],"resource_type":"gts.cf.core.events.request.v1~"}})
                );
            } else {
                assert_eq!(status, 413);
                assert_eq!(
                    problem,
                    json!({"type":"about:blank","title":"Payload Too Large","status":413,"detail":"Payload Too Large","instance":GROUPS,"trace_id":TRACE,"context":{}})
                );
            }
        }
    }
    let url = pair.subscription(&base).await?;
    let mut stream = pair
        .client
        .get(url)
        .bearer_auth(OWNER)
        .timeout(WATCHDOG)
        .send()
        .await?;
    assert_eq!(stream.status(), 200);
    assert_eq!(stream.headers()["content-type"], "text/event-stream");
    let first = Instant::now();
    let mut buffer = Vec::new();
    let mut topology = false;
    timeout(WATCHDOG, async {
        loop {
            let bytes = stream.chunk().await?.context("owned stream closed")?;
            buffer.extend_from_slice(&bytes);
            while let Some(end) = buffer.windows(2).position(|p| p == b"\n\n") {
                let frame: Vec<_> = buffer.drain(..end + 2).collect();
                let frame = std::str::from_utf8(&frame)?;
                if let Some(data) = frame.strip_prefix("event: topology\ndata: ") {
                    assert_eq!(
                        serde_json::from_str::<Value>(data.trim())?,
                        json!({"kind":"topology","topology_version":1,"assigned":[]})
                    );
                    topology = true;
                }
                if let Some(data) = frame.strip_prefix("event: heartbeat\ndata: ") {
                    let body = serde_json::from_str::<Value>(data.trim())?;
                    let object = body.as_object().context("heartbeat object")?;
                    let keys = object.keys().map(String::as_str).collect::<Vec<_>>();
                    assert_eq!(keys, ["at", "kind"]);
                    assert_eq!(body["kind"], "heartbeat");
                    chrono::DateTime::parse_from_rfc3339(
                        body["at"].as_str().context("heartbeat timestamp")?,
                    )?;
                    if first.elapsed() > LIMIT {
                        ensure!(topology, "missing topology frame");
                        return Ok::<_, anyhow::Error>(());
                    }
                }
            }
        }
    })
    .await??;
    drop(stream);
    broker.terminate()?;
    assert_eq!(broker.exited().await?.code(), Some(0));
    pair.finish()
}

#[tokio::test]
async fn a_delayed_policy_call_reaches_the_request_deadline_and_the_draining_probes_stay_available()
-> Result<()> {
    let pair = Pair::start().await?;
    configure(&pair)?;
    let policy = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let mut cfg = pair.broker_config()?;
    cfg["gears"]["event-broker"]["clients"]["consumes"]["authz-resolver"]["endpoint"] =
        json!(format!("http://{}", policy.local_addr()?));
    pair.write_broker_config(&cfg)?;
    let mut broker = pair.broker(
        "delayed",
        &["--e2e-flight-control", "run"],
        Some(&pair.grpc),
    )?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), 200);
    let request = || {
        pair.client
            .post(format!("{base}{GROUPS}"))
            .bearer_auth(OWNER)
            .header("traceparent", PARENT)
            .timeout(WATCHDOG)
            .send()
    };
    let observe = || async {
        let (socket, _) = timeout(WATCHDOG, policy.accept()).await??;
        let mut peer = BufReader::new(socket);
        let mut request = String::new();
        timeout(WATCHDOG, peer.read_line(&mut request)).await??;
        ensure!(
            request.starts_with("POST /authz-resolver/v1/evaluate "),
            "expected PDP route"
        );
        Ok::<_, anyhow::Error>(peer)
    };
    let started = Instant::now();
    let (body, stream) = tokio::sync::mpsc::channel::<Result<Bytes, io::Error>>(2);
    let upload = Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(stream));
    let client = tokio::spawn(
        pair.client
            .post(format!("{base}{GROUPS}"))
            .bearer_auth(OWNER)
            .header("traceparent", PARENT)
            .body(upload)
            .timeout(WATCHDOG)
            .send(),
    );
    body.send(Ok(Bytes::from_static(b"{"))).await?;
    // The PDP client has its own shorter deadline. Finish the upload with three seconds left
    // on the outer timer, then observe the real handler's outbound authorization call.
    tokio::time::sleep(Duration::from_secs(27)).await;
    body.send(Ok(Bytes::from_static(br#""client_agent":"cutover"}"#)))
        .await?;
    drop(body);
    let held = observe().await?;
    let response = timeout(WATCHDOG, client).await???;
    assert!(started.elapsed() >= LIMIT);
    assert_eq!(response.status(), 504);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    assert_eq!(
        response.json::<Value>().await?,
        json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~","title":"Deadline Exceeded","status":504,"detail":"Request exceeded 30s timeout","instance":GROUPS,"trace_id":TRACE,"context":{"resource_type":"gts.cf.core.http.request.v1~"}})
    );
    drop(held);
    let client = tokio::spawn(request());
    let held = observe().await?;
    broker.terminate()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ready = pair.client.get(format!("{base}/readyz")).send().await?;
        if ready.status() == StatusCode::SERVICE_UNAVAILABLE {
            assert_eq!(
                ready.json::<Value>().await?,
                json!({"state":"draining","ready":false})
            );
            break;
        }
        ensure!(Instant::now() < deadline, "drain readiness");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let health = pair.client.get(format!("{base}/healthz")).send().await?;
    assert_eq!(health.status(), 200);
    assert_eq!(health.text().await?, "ok");
    drop(held);
    let _disconnected = timeout(WATCHDOG, client).await??;
    assert_eq!(broker.exited().await?.code(), Some(0));
    pair.finish()
}

#[tokio::test]
async fn the_gateway_and_worker_apply_the_route_specific_mime_and_credential_policy() -> Result<()>
{
    let pair = Pair::start().await?;
    configure(&pair)?;
    let mut broker = pair.broker("mime", &["--e2e-flight-control", "run"], Some(&pair.grpc))?;
    let (base, page) = pair.authenticated_page(&mut broker).await?;
    assert_eq!(page.status(), 200);
    let (mut legacy, gateway) = pair.legacy_gateway().await?;
    for root in [&base, &gateway] {
        for mime in [
            None,
            Some("text/plain"),
            Some("application/json"),
            Some("application/json; charset=utf-8"),
        ] {
            let mut req = pair
                .client
                .post(format!("{root}{SUBS}"))
                .bearer_auth(OWNER)
                .header("traceparent", PARENT)
                .body("!");
            if let Some(mime) = mime {
                req = req.header("content-type", mime);
            }
            let res = req.send().await?;
            let json_mime = mime.is_some_and(|value| value.starts_with("application/json"));
            let (status, title) = if json_mime {
                (400, "Bad Request")
            } else {
                (415, "Unsupported Media Type")
            };
            assert_eq!(res.status().as_u16(), status);
            assert_eq!(res.headers()["content-type"], "application/problem+json");
            assert_eq!(
                res.json::<Value>().await?,
                json!({"type":"about:blank","title":title,"status":status,"detail":title,"instance":SUBS,"trace_id":TRACE,"context":{}})
            );
        }
        let collection = || {
            pair.client
                .get(format!("{root}{GROUPS}"))
                .bearer_auth(OWNER)
        };
        let before = collection().send().await?;
        assert_eq!(before.status(), StatusCode::OK);
        assert_eq!(
            before.json::<Value>().await?,
            json!({"items":[],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
        );
        let created = pair
            .client
            .post(format!("{root}{GROUPS}"))
            .bearer_auth(OWNER)
            .header("traceparent", PARENT)
            .send()
            .await?;
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()["content-type"], "application/json");
        let location = created.headers()["location"].to_str()?.to_owned();
        let body = created.json::<Value>().await?;
        let persisted = collection().send().await?;
        assert_eq!(persisted.status(), StatusCode::OK);
        let page = persisted.json::<Value>().await?;
        let [row] = page["items"]
            .as_array()
            .context("group collection")?
            .as_slice()
        else {
            anyhow::bail!("expected one persisted anonymous group");
        };
        let id = row["id"].as_str().context("persisted group id")?;
        let suffix = id
            .strip_prefix("gts.cf.core.events.consumer_group.v1~")
            .context("consumer group GTS prefix")?;
        let uuid = uuid::Uuid::parse_str(suffix)?;
        assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
        assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
        assert_eq!(uuid.to_string(), suffix);
        let created_at = row["created_at"]
            .as_str()
            .context("persisted creation timestamp")?;
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(created_at)?
                .offset()
                .local_minus_utc(),
            0
        );
        let expected = json!({"id":id,"created_at":created_at,"kind":"anonymous","description":null,
            "owner_principal":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","tenant":"00000000-0000-0000-0000-000000000002"});
        assert_eq!(
            page,
            json!({"items":[expected],"page_info":{"limit":25,"prev_cursor":null,"next_cursor":null}})
        );
        assert_eq!(body, expected);
        assert_eq!(location, format!("{GROUPS}/{id}"));
        let workload_only = pair
            .client
            .get(format!("{root}{GROUPS}"))
            .header("x-toolkit-internal-token", &pair.workload)
            .header("traceparent", PARENT)
            .send()
            .await?;
        assert_eq!(workload_only.status(), 401);
        assert_eq!(
            workload_only.headers()["content-type"],
            "application/problem+json"
        );
        assert_eq!(
            workload_only.json::<Value>().await?,
            json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~","title":"Unauthenticated","status":401,"detail":"Authentication required","instance":GROUPS,"trace_id":TRACE,"context":{"reason":"MISSING_BEARER"}})
        );
        let mixed = pair
            .client
            .get(format!("{root}{GROUPS}"))
            .bearer_auth(OWNER)
            .header("x-toolkit-internal-token", &pair.workload)
            .header("traceparent", PARENT)
            .send()
            .await?;
        assert_eq!(mixed.status(), 401);
        assert_eq!(mixed.headers()["content-type"], "application/problem+json");
        assert_eq!(
            mixed.json::<Value>().await?,
            json!({"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~","title":"Unauthenticated","status":401,"detail":"Authentication required","instance":GROUPS,"trace_id":TRACE,"context":{"reason":"MIXED_CREDENTIALS"}})
        );
    }
    legacy.terminate()?;
    assert_eq!(legacy.exited().await?.code(), Some(0));
    broker.terminate()?;
    assert_eq!(broker.exited().await?.code(), Some(0));
    pair.finish()
}
