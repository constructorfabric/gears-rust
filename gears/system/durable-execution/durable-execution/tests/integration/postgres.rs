//! Pinned workspace PostgreSQL, shared while fixtures live; isolated schemas per test.
use std::{
    ops::Deref,
    sync::{Arc, Weak},
};
use testcontainers::{ContainerAsync, ImageExt, core::Mount, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

struct Server {
    url: String,
    _container: ContainerAsync<Postgres>,
}
static SERVER: tokio::sync::Mutex<Weak<Server>> = tokio::sync::Mutex::const_new(Weak::new());

pub struct Database {
    pub url: String,
    _server: Option<Arc<Server>>,
}
impl Deref for Database {
    type Target = str;
    fn deref(&self) -> &str {
        &self.url
    }
}
#[expect(
    clippy::expect_used,
    reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
)]
pub async fn database() -> Database {
    // Child processes inherit the parent's owned fixture, never start another server.
    if let Ok(url) = std::env::var("DURABLE_TEST_PG_URL") {
        assert!(
            url.starts_with("postgres:") || url.starts_with("postgresql:"),
            "DURABLE_TEST_PG_URL must be a PostgreSQL URL"
        );
        return Database { url, _server: None };
    }
    let mut cached = SERVER.lock().await;
    let server = if let Some(server) = cached.upgrade() {
        server
    } else {
        let container = test_containers::postgres()
            .with_mount(
                Mount::tmpfs_mount("/var/lib/postgresql").with_size_bytes(512 * 1024 * 1024),
            )
            .start()
            .await
            .expect("integration tests require Docker and pinned PostgreSQL");
        let host = container.get_host().await.expect("PostgreSQL host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("PostgreSQL port");
        let server = Arc::new(Server {
            url: format!("postgres://postgres:postgres@{host}:{port}/postgres"),
            _container: container,
        });
        *cached = Arc::downgrade(&server);
        server
    };
    Database {
        url: server.url.clone(),
        _server: Some(server),
    }
}
