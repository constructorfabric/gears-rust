//! `OutboxSetupBuilder::build`'s checks, the lazy enqueue path and the
//! `Debug` output of the outbox types, over an in-memory SQLite database and a
//! broker double that panics on every call: none of these paths may reach the
//! broker.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::json;
use toolkit_db::outbox::{Outbox, Partitions};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use ::gts::{GtsSchema, GtsTypeId};

use crate::api::{EventBrokerApi, ProducerMode};
use crate::broker_stub::BrokerStub;
use crate::error::EventBrokerError;
use crate::gts::EventV1;

use super::{OutboxRegistration, OutboxSetup};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.producer.unit_outbox_orders.v1");
const EVENT_TYPE: &str =
    gts_id!("cf.core.events.event.v1~example.sdk.producer.unit_outbox_order_created.v1~");
const SUBJECT_TYPE: &str =
    gts_id!("cf.core.events.subject.v1~example.sdk.producer.unit_outbox_order.v1~");
const TENANT: Uuid = Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_7e01);
const QUEUE: &str = "orders-events";

/// A broker every call of which panics, so a test that reaches it fails.
struct NoBroker;

impl BrokerStub for NoBroker {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
}

impl GtsSchema for OrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

/// The options one `build` case varies; [`Options::valid`] builds.
struct Options {
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    source: String,
    registration: OutboxRegistration,
    queue: String,
    /// Set on the builder only when present; the SDK default otherwise.
    client_agent: Option<String>,
}

impl Options {
    fn valid() -> Self {
        Self {
            topics: vec![TOPIC.to_owned()],
            event_type_patterns: vec![EVENT_TYPE.to_owned()],
            source: "order-service".to_owned(),
            registration: OutboxRegistration::Stateless,
            queue: QUEUE.to_owned(),
            client_agent: None,
        }
    }

    /// Builds a lazy setup with these options over a fresh database.
    async fn build(self) -> Result<OutboxSetup, EventBrokerError> {
        self.build_over(sqlite_db(&next_dsn()).await).await
    }

    /// As [`Options::build`], over `db`.
    async fn build_over(self, db: toolkit_db::Db) -> Result<OutboxSetup, EventBrokerError> {
        let builder = OutboxSetup::builder()
            .broker(no_broker())
            .db(db)
            .security_context(SecurityContext::anonymous())
            .source(self.source)
            .registration(self.registration)
            .topics(self.topics)
            .event_type_patterns(self.event_type_patterns)
            .queue(self.queue, Partitions::of(4))
            .lazy_schema_loading();
        match self.client_agent {
            Some(client_agent) => builder.client_agent(client_agent).build().await,
            None => builder.build().await,
        }
    }
}

/// Each check of `build` refuses its input with its own text, in order and
/// before any broker call; the accepted bounds build.
#[tokio::test]
async fn a_setup_build_refuses_each_invalid_option_before_any_call() {
    let refused = [
        (
            "empty topics",
            Options {
                topics: vec![],
                ..Options::valid()
            },
            "topics must contain at least one non-empty entry".to_owned(),
        ),
        (
            "a blank topic entry",
            Options {
                topics: vec![TOPIC.to_owned(), "  ".to_owned()],
                ..Options::valid()
            },
            "topics must contain at least one non-empty entry".to_owned(),
        ),
        (
            "empty patterns",
            Options {
                event_type_patterns: vec![],
                ..Options::valid()
            },
            "event_type_patterns must contain at least one non-empty entry".to_owned(),
        ),
        (
            "a blank source",
            Options {
                source: "   ".to_owned(),
                ..Options::valid()
            },
            "source is required".to_owned(),
        ),
        (
            "a non-ASCII source",
            Options {
                source: "order-\u{e9}".to_owned(),
                ..Options::valid()
            },
            "source must contain only printable ASCII (0x20-0x7E)".to_owned(),
        ),
        (
            "a 257-byte client agent",
            Options {
                client_agent: Some("a".repeat(257)),
                ..Options::valid()
            },
            "client_agent must be 1-256 bytes, got 257".to_owned(),
        ),
        (
            "a non-ASCII client agent",
            Options {
                client_agent: Some("agent-\u{e9}".to_owned()),
                ..Options::valid()
            },
            "client_agent must contain only printable ASCII (0x20-0x7E)".to_owned(),
        ),
        (
            "an empty client agent",
            Options {
                client_agent: Some(String::new()),
                ..Options::valid()
            },
            "client_agent must be 1-256 bytes, got 0".to_owned(),
        ),
        (
            "a stateless managed registration",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Stateless, "orders"),
                ..Options::valid()
            },
            "managed producer registration requires monotonic or chained mode".to_owned(),
        ),
        (
            "a blank key",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, "  "),
                ..Options::valid()
            },
            "managed producer registration key must not be empty".to_owned(),
        ),
        (
            "a key with a trailing space",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, "orders "),
                ..Options::valid()
            },
            "managed producer registration key must not have leading or trailing whitespace"
                .to_owned(),
        ),
        (
            "a key with a leading space",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, " orders"),
                ..Options::valid()
            },
            "managed producer registration key must not have leading or trailing whitespace"
                .to_owned(),
        ),
        (
            "a key with a trailing tab",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, "orders\t"),
                ..Options::valid()
            },
            "managed producer registration key must not have leading or trailing whitespace"
                .to_owned(),
        ),
        (
            "a 256-byte key",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, "k".repeat(256)),
                ..Options::valid()
            },
            "managed producer registration key must be at most 255 bytes".to_owned(),
        ),
        (
            "a too-long key with edge whitespace",
            Options {
                registration: OutboxRegistration::managed(
                    ProducerMode::Chained,
                    format!("{} ", "k".repeat(255)),
                ),
                ..Options::valid()
            },
            "managed producer registration key must not have leading or trailing whitespace"
                .to_owned(),
        ),
        (
            "a too-long blank queue",
            Options {
                queue: " ".repeat(256),
                ..Options::valid()
            },
            "producer outbox queue name must not be empty".to_owned(),
        ),
        (
            "a queue with a trailing tab",
            Options {
                queue: "orders\t".to_owned(),
                ..Options::valid()
            },
            "producer outbox queue name must not have leading or trailing whitespace".to_owned(),
        ),
        (
            "a queue with a trailing space",
            Options {
                queue: "orders ".to_owned(),
                ..Options::valid()
            },
            "producer outbox queue name must not have leading or trailing whitespace".to_owned(),
        ),
        (
            "a queue with a leading space",
            Options {
                queue: " orders".to_owned(),
                ..Options::valid()
            },
            "producer outbox queue name must not have leading or trailing whitespace".to_owned(),
        ),
        (
            "a 256-byte queue",
            Options {
                queue: "q".repeat(256),
                ..Options::valid()
            },
            "producer outbox queue name must be at most 255 bytes".to_owned(),
        ),
        (
            "a too-long queue with edge whitespace",
            Options {
                queue: format!("{} ", "q".repeat(255)),
                ..Options::valid()
            },
            "producer outbox queue name must not have leading or trailing whitespace".to_owned(),
        ),
        (
            "a queue with inner whitespace",
            Options {
                queue: "orders created".to_owned(),
                ..Options::valid()
            },
            "producer outbox queue name must contain only ASCII alphanumerics, '.', '_' and '-'"
                .to_owned(),
        ),
        (
            "a queue starting with a hyphen",
            Options {
                queue: "-orders".to_owned(),
                ..Options::valid()
            },
            "producer outbox queue name must start and end with an ASCII alphanumeric".to_owned(),
        ),
        (
            "a queue with a slash",
            Options {
                queue: "orders/created".to_owned(),
                ..Options::valid()
            },
            "producer outbox queue name must contain only ASCII alphanumerics, '.', '_' and '-'"
                .to_owned(),
        ),
    ];
    for (case, options, expected) in refused {
        let built = options.build().await;

        let Err(EventBrokerError::InvalidProducerOptions { detail }) = &built else {
            panic!("{case} must be refused, got {built:?}");
        };
        assert_eq!(detail, &expected, "{case}");
    }

    let accepted = [
        (
            "a 256-byte client agent",
            Options {
                client_agent: Some("a".repeat(256)),
                ..Options::valid()
            },
        ),
        (
            "a 255-byte key",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, "k".repeat(255)),
                ..Options::valid()
            },
        ),
        (
            "a key with inner whitespace",
            Options {
                registration: OutboxRegistration::managed(ProducerMode::Chained, "orders created"),
                ..Options::valid()
            },
        ),
        (
            "a 255-byte queue",
            Options {
                queue: "q".repeat(255),
                ..Options::valid()
            },
        ),
        (
            "a queue with each allowed punctuation",
            Options {
                queue: "orders.created_v1-x".to_owned(),
                ..Options::valid()
            },
        ),
    ];
    for (case, options) in accepted {
        let built = options.build().await;

        assert!(built.is_ok(), "{case} must build, got {built:?}");
    }
}

/// A lazy setup refuses to enqueue a type it has not prepared, inside the
/// caller's transaction, without preparing it there: no broker call, and the
/// transaction writes nothing.
#[tokio::test]
async fn a_lazy_setup_refuses_an_unprepared_type_without_a_broker_call() {
    let dsn = next_dsn();
    let db = sqlite_db(&dsn).await;
    let setup = Options::valid().build_over(db.clone()).await.unwrap();
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();
    let producer = handle.producer().clone();

    let enqueued = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move { producer.enqueue(tx, order()).await.map_err(TxError::Broker) })
        })
        .await;

    let Err(TxError::Broker(err @ EventBrokerError::SchemaNotPrepared { type_id, detail })) =
        &enqueued
    else {
        panic!("expected SchemaNotPrepared, got {enqueued:?}");
    };
    assert_eq!(
        (type_id.as_str(), detail.as_str(), err.to_string()),
        (
            EVENT_TYPE,
            "call prepare_event_type outside the business transaction first",
            format!(
                "schema not prepared for {EVENT_TYPE}; call `prepare_event_type` before opening \
                 the txn"
            )
        )
    );
    assert_eq!(outbox_bodies(&dsn).await, 0);
    handle.stop().await;
}

/// No outbox type prints its security context, broker, database or outbox;
/// the builder prints which of those are set and the context's subject id.
#[tokio::test]
async fn each_outbox_type_prints_no_context_or_connection() {
    let builder = OutboxSetup::builder()
        .broker(no_broker())
        .security_context(SecurityContext::anonymous())
        .source("order-service");
    let managed = Options {
        registration: OutboxRegistration::managed(ProducerMode::Chained, "orders"),
        ..Options::valid()
    };
    let db = sqlite_db(&next_dsn()).await;
    let registered = managed.build_over(db.clone()).await.unwrap();
    let setup_text = format!("{registered:?}");
    let (_queue_builder, binder) = registered.register(Outbox::builder(db.clone()));
    let started = Options {
        registration: OutboxRegistration::managed(ProducerMode::Chained, "orders"),
        ..Options::valid()
    }
    .build_over(db.clone())
    .await
    .unwrap()
    .start(Outbox::builder(db.clone()))
    .await
    .unwrap();

    assert_eq!(
        format!("{builder:?}"),
        format!(
            "OutboxSetupBuilder {{ broker: set, db: unset, security_context: Some({}), \
             source: Some(\"order-service\"), registration: None, topics: [], \
             event_type_patterns: [], queue: None, partitions: None, client_agent: None, \
             lazy_schema_loading: false }}",
            Uuid::nil()
        )
    );
    assert_eq!(
        setup_text,
        "OutboxSetup { queue: \"orders-events\", partitions: 4, mode: Chained, .. }"
    );
    assert_eq!(
        format!("{binder:?}"),
        "OutboxBinder { queue: \"orders-events\", partitions: 4, mode: Chained, .. }"
    );
    assert_eq!(
        format!("{started:?}"),
        "OutboxProducerHandle { queue: \"orders-events\", partitions: 4, mode: Chained, .. }"
    );
    assert_eq!(
        format!("{:?}", started.producer()),
        "OutboxProducer { queue: \"orders-events\", partitions: 4, mode: Chained, .. }"
    );
    started.stop().await;
}

/// Why the test transaction did not commit.
#[derive(Debug)]
enum TxError {
    Broker(EventBrokerError),
    #[expect(
        dead_code,
        reason = "read only through Debug, in a failed test's message"
    )]
    Db(toolkit_db::DbError),
}

impl From<toolkit_db::DbError> for TxError {
    fn from(err: toolkit_db::DbError) -> Self {
        Self::Db(err)
    }
}

fn no_broker() -> Arc<dyn EventBrokerApi> {
    Arc::new(NoBroker)
}

/// A built order event of the test tenant.
fn order() -> EventV1<OrderCreated> {
    EventV1::builder(OrderCreated {
        order_id: Uuid::from_u128(1),
    })
    .tenant_id(TENANT)
    .subject("order-1")
    .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
    .build()
    .expect("a valid order event")
}

/// A fresh shared-cache in-memory SQLite DSN, so a second connection of its
/// own (`outbox_bodies`) reads the same database.
fn next_dsn() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("sqlite:file:evbk_sdk_outbox_setup_{seq}?mode=memory&cache=shared")
}

/// The migrated in-memory SQLite database at `dsn`: the outbox's tables and
/// the producer registrations'.
async fn sqlite_db(dsn: &str) -> toolkit_db::Db {
    let db = toolkit_db::connect_db(
        dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("sqlite connects");
    let mut migrations = toolkit_db::outbox::outbox_migrations();
    migrations.extend(crate::producer_registration_migrations());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, migrations)
        .await
        .expect("migrations run");
    db
}

/// How many message bodies the outbox in the database at `dsn` holds, read
/// over a connection of its own.
async fn outbox_bodies(dsn: &str) -> i64 {
    use sea_orm::ConnectionTrait;
    use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};

    let conn = sea_orm::Database::connect(dsn).await.expect("connect");
    conn.query_one(
        Query::select()
            .expr_as(Expr::col(Alias::new("id")).count(), "n")
            .from(Alias::new("toolkit_outbox_body")),
    )
    .await
    .expect("read the outbox bodies")
    .expect("a count row")
    .try_get::<i64>("", "n")
    .expect("a count")
}
