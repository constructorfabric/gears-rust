//! One engine of the gear-DB x storage-backend matrix: where the gear's own
//! database lives, and the storage backend beside it. On `PostgreSQL` and
//! `MySQL` both are on the same server, each in a schema or database of its
//! own; on `SQLite` the gear database is a temp file and the backend is the
//! memory backend, the default lane's pair.

use std::sync::Arc;
use std::time::Duration;

use event_broker::test_support::GearDb;
use event_broker_sdk::Position;
use event_broker_sdk::backend::{
    Backend, BackendError, BuildContext, Partition, Provider, Role, TopicPartition,
};
use gts::GtsInstanceId;
use memory_event_broker_plugin::MemoryBackend;
use mysql_event_broker_plugin::MysqlBackendProvider;
use postgres_event_broker_plugin::PostgresBackendProvider;

use super::containers::{self, Server};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Sqlite,
    Postgres,
    Mysql,
}

/// The engine's server, kept alive with this value.
pub struct Matrix {
    engine: Engine,
    server: Option<Server>,
}

impl Matrix {
    /// `None` when the engine needs a container that cannot start and Docker
    /// is not required (the reason is printed): the caller returns, skipped.
    pub async fn start(engine: Engine, purpose: &str) -> Option<Self> {
        let server = match engine {
            Engine::Sqlite => None,
            Engine::Postgres => Some(containers::postgres(purpose).await?),
            Engine::Mysql => Some(containers::mysql(purpose).await?),
        };
        Some(Self { engine, server })
    }

    fn url(&self) -> &str {
        &self
            .server
            .as_ref()
            .expect("a server-based engine has a server")
            .url
    }

    /// A gear database no other test uses.
    pub fn gear_db(&self) -> GearDb {
        match self.engine {
            Engine::Sqlite => GearDb::Sqlite,
            Engine::Postgres => GearDb::Postgres {
                url: self.url().to_owned(),
                schema: containers::fresh_name("gear"),
            },
            Engine::Mysql => GearDb::Mysql {
                url: self.url().to_owned(),
                database: containers::fresh_name("gear"),
            },
        }
    }

    /// A started storage backend no other test uses.
    pub async fn backend(&self) -> Arc<dyn Backend> {
        let settings = match self.engine {
            Engine::Sqlite => return Arc::new(MemoryBackend::new()),
            Engine::Postgres => serde_json::json!({
                "url": self.url(),
                "schema": containers::fresh_name("log"),
                "pools": { "main": 4 },
            }),
            Engine::Mysql => serde_json::json!({
                "url": self.url(),
                "database": containers::fresh_name("log"),
                "pools": { "main": 4, "background": 2 },
            }),
        };
        let serde_json::Value::Object(settings) = settings else {
            unreachable!("an object literal");
        };
        let context = BuildContext::new(&settings, Role::Standalone);
        let backend = match self.engine {
            Engine::Sqlite => unreachable!("returned above"),
            Engine::Postgres => PostgresBackendProvider::default().build(&context).await,
            Engine::Mysql => MysqlBackendProvider::default().build(&context).await,
        }
        .expect("build the storage backend");
        backend.start().await.expect("start the storage backend");
        answering(backend.as_ref()).await;
        backend
    }
}

/// Waits until `backend` has finished its startup and answers something other
/// than `Unavailable`.
async fn answering(backend: &dyn Backend) {
    let probe = TopicPartition::new(
        GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.matrix.probe.v1").unwrap(),
        Partition::new(0),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    while let Err(BackendError::Unavailable { .. }) =
        backend.resolve(&probe, Position::Latest).await
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the storage backend did not start within a minute"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
