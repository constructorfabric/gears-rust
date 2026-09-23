//! `cf-evctl` - a kafka/nats-style demo CLI over the SDK REST transport.
//!
//! `produce`/`consume` are one-shot; `demo-producer`/`demo-consumer` are the
//! demo loop (emit on an interval / print what arrives). Everything reaches the
//! broker only through `EventBrokerApi`, so the tool doubles as a manual end-to-
//! end check of the transport (both stream framings). `--debug` turns on wire
//! logging of every request and response.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use event_broker_sdk::api::{
    FrameStream, JoinRequest, Position, SeekPosition, SubscriptionInterest, WireFrame,
};
use event_broker_sdk::models::{CreateConsumerGroupRequest, Event};
use event_broker_sdk::rest::{RestBroker, StreamTransport};
use event_broker_sdk::{ConsumerGroupId, EventBrokerApi};
use futures_util::StreamExt;
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "cf-evctl",
    about = "Demo event-broker CLI over the SDK REST transport"
)]
struct Cli {
    /// Broker base URL, e.g. https://host:8080
    #[arg(long, global = true, default_value = "http://127.0.0.1:8080")]
    broker: String,
    /// Caller tenant id. Defaults to the standalone demo tenant.
    #[arg(
        long,
        global = true,
        default_value = "11111111-1111-1111-1111-111111111111"
    )]
    tenant: Uuid,
    /// Caller principal (subject) id. Defaults to a fresh uuid per run.
    #[arg(long, global = true)]
    principal: Option<Uuid>,
    /// Bearer token for the tenant plane.
    #[arg(long, global = true)]
    token: Option<String>,
    /// Log every HTTP request and response (method, url, headers, bodies). The
    /// bearer token is never printed.
    #[arg(long, global = true)]
    debug: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Copy, Clone, ValueEnum)]
enum Framing {
    Multipart,
    Sse,
}

impl From<Framing> for StreamTransport {
    fn from(f: Framing) -> Self {
        match f {
            Framing::Multipart => StreamTransport::Multipart,
            Framing::Sse => StreamTransport::Sse,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Publish one event.
    Produce(ProduceArgs),
    /// Join a group and stream its events to stdout (full frame dump).
    Consume(ConsumeArgs),
    /// Publish an event every `--interval` seconds (demo loop).
    DemoProducer {
        #[command(flatten)]
        event: ProduceArgs,
        /// Seconds between emits.
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Number of events to emit; 0 = forever.
        #[arg(long, default_value_t = 0)]
        count: u64,
    },
    /// Join a group and print a concise line per received event (demo loop).
    DemoConsumer(ConsumeArgs),
    /// Clear the shared group store (and delete the recorded group), so the next
    /// `demo-consumer` mints a fresh shared group.
    DemoReset {
        #[arg(long)]
        group_store: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct ProduceArgs {
    /// Event GTS type id (ends with `~`). Defaults to the seeded demo type.
    #[arg(
        long = "type",
        default_value = "gts.cf.core.events.event.v1~cf.core.demo.spread.v1~"
    )]
    type_id: String,
    #[arg(long, default_value = "hello")]
    subject: String,
    #[arg(long, default_value = "gts.cf.core.demo.subject.v1~")]
    subject_type: String,
    #[arg(long, default_value = "cf-evctl")]
    source: String,
    /// Event data as JSON: a literal, `@file`, or `-` for stdin. Ignored by
    /// `demo-producer`, which generates its own payload.
    #[arg(long, default_value = "-")]
    data: String,
}

#[derive(clap::Args)]
struct ConsumeArgs {
    /// Consumer group GTS instance id. Omit to share one via `--group-store`
    /// (get-or-create), so several consumers join the same group and fan out.
    #[arg(long)]
    group: Option<String>,
    /// Shared SQLite store holding the anonymous group id, so independently
    /// launched consumers coordinate on ONE group. Used only when `--group` is
    /// absent; the default path is shared, so two `demo-consumer`s fan out by
    /// default.
    #[arg(long)]
    group_store: Option<PathBuf>,
    /// Topic GTS instance id. Defaults to the seeded demo topic.
    #[arg(
        long,
        default_value = "gts.cf.core.events.topic.v1~cf.core.demo.topic.v1"
    )]
    topic: String,
    /// Event GTS type id (or pattern). Defaults to the seeded demo type.
    #[arg(
        long = "type",
        default_value = "gts.cf.core.events.event.v1~cf.core.demo.spread.v1~"
    )]
    type_id: String,
    #[arg(long, value_enum, default_value_t = Framing::Multipart)]
    framing: Framing,
}

fn read_data(arg: &str) -> Result<serde_json::Value, String> {
    let raw = if arg == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("reading stdin: {e}"))?;
        s
    } else if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?
    } else {
        arg.to_owned()
    };
    serde_json::from_str(&raw).map_err(|e| format!("data is not valid JSON: {e}"))
}

fn security_context(
    tenant: Uuid,
    principal: Option<Uuid>,
    token: Option<String>,
) -> Result<SecurityContext, String> {
    let mut builder = SecurityContext::builder()
        .subject_tenant_id(tenant)
        .subject_id(principal.unwrap_or_else(Uuid::new_v4));
    if let Some(token) = token {
        builder = builder.bearer_token(token);
    }
    builder
        .build()
        .map_err(|e| format!("building security context: {e}"))
}

fn make_event(tenant: Uuid, args: &ProduceArgs, data: serde_json::Value) -> Event {
    Event {
        id: Uuid::new_v4(),
        type_id: event_broker_sdk::GtsTypeId::new(&args.type_id),
        tenant_id: tenant,
        source: args.source.clone(),
        subject: args.subject.clone(),
        subject_type: event_broker_sdk::GtsTypeId::new(&args.subject_type),
        occurred_at: chrono::Utc::now(),
        trace_parent: None,
        data: Some(data),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    }
}

/// Default shared group store - a fixed path so two `demo-consumer` invocations
/// on the same machine coordinate on one group without extra flags.
fn default_group_store() -> PathBuf {
    std::env::temp_dir().join("cf-evctl-demo-group.db")
}

/// Reads the single stored group id, if the store has one yet.
fn read_stored_group(conn: &rusqlite::Connection) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT group_id FROM group_singleton WHERE k = 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| format!("reading group store: {e}"))
}

fn parse_stored_group(id: &str) -> Result<ConsumerGroupId, String> {
    ConsumerGroupId::try_from_gts(id)
        .ok_or_else(|| format!("stored group id is not an anonymous group: {id}"))
}

/// Get-or-create the shared anonymous group through the store. The first
/// consumer mints one (broker) and claims the singleton row; a consumer that
/// loses the claim (a PK conflict on the singleton row) re-reads until the
/// winner's id appears. A loser's freshly minted group is orphaned on the
/// broker - `demo-reset` clears the store and deletes the recorded group.
async fn resolve_shared_group(
    broker: &RestBroker,
    ctx: &SecurityContext,
    store: &std::path::Path,
) -> Result<ConsumerGroupId, String> {
    let conn = rusqlite::Connection::open(store)
        .map_err(|e| format!("open group store {}: {e}", store.display()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS group_singleton \
         (k INTEGER PRIMARY KEY CHECK(k = 1), group_id TEXT NOT NULL);",
    )
    .map_err(|e| format!("init group store: {e}"))?;

    if let Some(id) = read_stored_group(&conn)? {
        eprintln!("joined shared group {id} (from {})", store.display());
        return parse_stored_group(&id);
    }

    let created = broker
        .create_consumer_group(
            ctx,
            CreateConsumerGroupRequest {
                client_agent: "cf-evctl".to_owned(),
                description: None,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let gts = created.id.to_gts();
    match conn.execute(
        "INSERT INTO group_singleton (k, group_id) VALUES (1, ?1)",
        [&gts],
    ) {
        Ok(_) => {
            eprintln!("created shared group {gts} (stored at {})", store.display());
            Ok(created.id)
        }
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            for _ in 0..10 {
                if let Some(id) = read_stored_group(&conn)? {
                    eprintln!(
                        "joined shared group {id} (a peer won the claim; our {gts} is orphaned)"
                    );
                    return parse_stored_group(&id);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err("group store had a concurrent claim but no id appeared".to_owned())
        }
        Err(e) => Err(format!("claiming group store: {e}")),
    }
}

/// Clears the shared store and best-effort deletes the recorded group.
async fn reset_group_store(
    broker: &RestBroker,
    ctx: &SecurityContext,
    store: &std::path::Path,
) -> Result<(), String> {
    if !store.exists() {
        eprintln!(
            "group store {} does not exist; nothing to reset",
            store.display()
        );
        return Ok(());
    }
    let conn = rusqlite::Connection::open(store)
        .map_err(|e| format!("open group store {}: {e}", store.display()))?;
    if let Some(id) = read_stored_group(&conn).unwrap_or(None)
        && let Some(group) = ConsumerGroupId::try_from_gts(&id)
    {
        let _ = broker.delete_consumer_group(ctx, &group).await;
    }
    conn.execute("DELETE FROM group_singleton WHERE k = 1", [])
        .map_err(|e| format!("clearing group store: {e}"))?;
    eprintln!("group store {} reset", store.display());
    Ok(())
}

/// Joins the group, seeks every assigned partition to `earliest` (the broker
/// rejects an unseeded stream), and opens the stream. This is a demo path; the
/// `Consumer` runtime is the thing that persists and resumes real offsets.
async fn open_stream(
    broker: &RestBroker,
    ctx: &SecurityContext,
    tenant: Uuid,
    args: &ConsumeArgs,
) -> Result<FrameStream, String> {
    let group = match &args.group {
        Some(gts) => ConsumerGroupId::try_from_gts(gts).ok_or_else(|| {
            "group must be an anonymous consumer-group GTS id \
             (gts.cf.core.events.consumer_group.v1~<uuid>)"
                .to_owned()
        })?,
        None => {
            // Shared subscription: coordinate on ONE anonymous group through a
            // shared SQLite store so independently launched consumers fan out.
            let store = args.group_store.clone().unwrap_or_else(default_group_store);
            resolve_shared_group(broker, ctx, &store).await?
        }
    };
    let interest =
        SubscriptionInterest::builder()
            .topic(
                event_broker_sdk::GtsInstanceId::try_new(&args.topic).map_err(|e| e.to_string())?,
            )
            .tenant_id(tenant)
            .types([event_broker_sdk::GtsIdPattern::try_new(&args.type_id)
                .map_err(|e| e.to_string())?])
            .build()
            .map_err(|e| e.to_string())?;
    let assignment = broker
        .join(
            ctx,
            JoinRequest {
                group,
                client_agent: "cf-evctl".to_owned(),
                interests: vec![interest],
                session_timeout: None,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let positions: Vec<SeekPosition> = assignment
        .assigned
        .iter()
        .map(|p| SeekPosition {
            topic: p.topic.clone(),
            partition: p.partition,
            value: Position::Earliest,
        })
        .collect();
    if !positions.is_empty() {
        broker
            .seek(
                ctx,
                assignment.subscription_id,
                assignment.topology_version,
                &positions,
            )
            .await
            .map_err(|e| e.to_string())?;
    }
    broker
        .stream(ctx, assignment.subscription_id)
        .await
        .map_err(|e| e.to_string())
}

/// A one-line summary of a frame for the demo consumer.
fn summarize(frame: &WireFrame) -> String {
    match frame {
        WireFrame::Event(e) => format!(
            "event seq={} partition={} subject={} data={}",
            e.sequence.as_i64(),
            e.partition,
            e.subject,
            e.data
        ),
        WireFrame::Heartbeat { at } => format!("heartbeat at={at}"),
        WireFrame::Topology {
            topology_version,
            assigned,
        } => format!(
            "topology v={topology_version} partitions={}",
            assigned.len()
        ),
        WireFrame::Control { code, reason, .. } => format!("control {code:?} reason={reason:?}"),
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("cf-evctl: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let cli = Cli::parse();
    if cli.debug {
        // Wire logs go to stderr so they don't mix with the tool's stdout.
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "event_broker_sdk::rest=debug",
            ))
            .with_writer(std::io::stderr)
            .init();
    }
    let ctx = security_context(cli.tenant, cli.principal, cli.token)?;

    match cli.command {
        Command::Produce(args) => {
            let broker = broker(&cli.broker, StreamTransport::Multipart, cli.debug)?;
            let event = make_event(cli.tenant, &args, read_data(&args.data)?);
            let outcome = broker
                .publish(&ctx, &event)
                .await
                .map_err(|e| e.to_string())?;
            println!("{outcome:?}");
            Ok(())
        }
        Command::Consume(args) => {
            let broker = broker(&cli.broker, args.framing.into(), cli.debug)?;
            let mut stream = open_stream(&broker, &ctx, cli.tenant, &args).await?;
            while let Some(frame) = stream.next().await {
                match frame {
                    Ok(frame) => {
                        // Flush per frame: a streaming CLI's stdout is block-
                        // buffered when piped, so unflushed frames are lost.
                        println!("{frame:?}");
                        let _ = std::io::stdout().flush();
                    }
                    Err(err) => {
                        eprintln!("stream error: {err}");
                        break;
                    }
                }
            }
            Ok(())
        }
        Command::DemoProducer {
            event,
            interval,
            count,
        } => {
            let broker = broker(&cli.broker, StreamTransport::Multipart, cli.debug)?;
            let mut emitted = 0u64;
            loop {
                let data = serde_json::json!({
                    "seq": emitted,
                    "at": chrono::Utc::now().to_rfc3339(),
                    "msg": format!("demo event {emitted}"),
                });
                let e = make_event(cli.tenant, &event, data);
                let outcome = broker.publish(&ctx, &e).await.map_err(|e| e.to_string())?;
                println!("emit {emitted}: {outcome:?}");
                let _ = std::io::stdout().flush();
                emitted += 1;
                if count != 0 && emitted >= count {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
            Ok(())
        }
        Command::DemoConsumer(args) => {
            let broker = broker(&cli.broker, args.framing.into(), cli.debug)?;
            let mut stream = open_stream(&broker, &ctx, cli.tenant, &args).await?;
            while let Some(frame) = stream.next().await {
                match frame {
                    Ok(frame) => {
                        println!("{}", summarize(&frame));
                        let _ = std::io::stdout().flush();
                    }
                    Err(err) => {
                        eprintln!("stream error: {err}");
                        break;
                    }
                }
            }
            Ok(())
        }
        Command::DemoReset { group_store } => {
            let broker = broker(&cli.broker, StreamTransport::Multipart, cli.debug)?;
            let store = group_store.unwrap_or_else(default_group_store);
            reset_group_store(&broker, &ctx, &store).await
        }
    }
}

fn broker(url: &str, framing: StreamTransport, debug: bool) -> Result<RestBroker, String> {
    Ok(RestBroker::new(url.to_owned())
        .map_err(|e| e.to_string())?
        .with_stream_transport(framing)
        .with_debug(debug))
}
