// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Shared fixture of the Docker-backed integration tests: a `hashicorp/vault`
//! dev server per test, a KV v2 mount configured the way `docs/DESIGN.md`
//! prescribes, and a token holding only the minimal ACL policy of the docs.
//!
//! Every test that uses it is `#[ignore]`d and never runs in CI; see
//! `docs/TESTING.md`. Without a reachable Docker daemon the fixture prints a
//! notice and the test returns early (set `VAULT_CREDSTORE_REQUIRE_DOCKER=1`
//! to turn that into a failure).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "integration-test fixture: a setup failure IS the test failure, and each test binary uses a different subset"
)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, Image, ImageExt};
use tokio::sync::{Semaphore, SemaphorePermit};
use uuid::Uuid;
use vault_credstore_plugin_v2::config::{RetryConfig, VaultCredStorePluginConfig, VaultToken};
use vault_credstore_plugin_v2::domain::Service;
use vault_credstore_plugin_v2::infra::http::ReqwestTransport;

const ENV_REQUIRE_DOCKER: &str = "VAULT_CREDSTORE_REQUIRE_DOCKER";

/// The dev server's root token. Only the fixture's admin calls use it.
pub const ROOT_TOKEN: &str = "root";
/// KV v2 mount created by the fixture (the dev server's own `secret/` mount
/// is left alone).
pub const MOUNT: &str = "credstore-it";
const POLICY: &str = "credstore-it";

/// How many Vault containers may run at once: the conformance suite alone is
/// one test per check.
static CONTAINER_SLOTS: Semaphore = Semaphore::const_new(4);

/// The host port Docker mapped to the container's Vault port. Right after `start` (nothing is
/// waited for) the runtime may not have published the mapping yet, so it is asked again for a
/// few seconds before the setup fails.
async fn mapped_vault_port(container: &ContainerAsync<GenericImage>) -> u16 {
    let mut last = String::new();
    for _ in 0..100 {
        match container.get_host_port_ipv4(8200.tcp()).await {
            Ok(port) => return port,
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the container does not map the Vault port: {last}");
}

fn require_docker() -> bool {
    std::env::var_os(ENV_REQUIRE_DOCKER).is_some_and(|v| v != "0" && !v.is_empty())
}

/// The minimal ACL policy of `docs/DESIGN.md` (section 4.3), for `mount` and
/// `prefix`. The tests run the plugin with a token holding exactly this, so
/// the documented policy is the one that is verified.
pub fn minimal_policy(mount: &str, prefix: &str) -> String {
    format!(
        r#"
path "{mount}/data/{prefix}/*" {{
  capabilities = ["create", "update", "read"]
}}
path "{mount}/metadata/{prefix}/*" {{
  capabilities = ["create", "update", "read", "delete"]
}}
path "{mount}/destroy/{prefix}/*" {{
  capabilities = ["update"]
}}
"#
    )
}

/// A running Vault dev server plus the settings to reach it.
pub struct VaultFixture {
    _container: ContainerAsync<GenericImage>,
    _slot: SemaphorePermit<'static>,
    http: reqwest::Client,
    /// `http://127.0.0.1:<mapped port>`.
    pub address: String,
    /// Path prefix the plugin writes under (unique per fixture).
    pub prefix: String,
    /// A token holding only [`minimal_policy`].
    pub token: String,
}

impl VaultFixture {
    /// Starts a dev server, configures the mount, the policy and a token.
    /// `None` when Docker is not reachable (after printing why).
    pub async fn start() -> Option<Self> {
        let slot = CONTAINER_SLOTS
            .acquire()
            .await
            .expect("semaphore is never closed");
        // The image and its tag come from `libs/test-containers`; the tag can be
        // overridden with `GEARS_TEST_VAULT_TAG`.
        let image = test_containers::vault();
        let image_ref = format!("{}:{}", image.name(), image.tag());
        let request = image
            .with_exposed_port(8200.tcp())
            .with_wait_for(WaitFor::Nothing)
            .with_env_var("VAULT_DEV_ROOT_TOKEN_ID", ROOT_TOKEN)
            .with_env_var("VAULT_DEV_LISTEN_ADDRESS", "0.0.0.0:8200")
            .with_cap_add("IPC_LOCK")
            .with_cmd(["server", "-dev"]);
        let container = match request.start().await {
            Ok(container) => container,
            Err(e) => {
                assert!(
                    !require_docker(),
                    "{ENV_REQUIRE_DOCKER} is set but the {image_ref} container could not start: {e}"
                );
                eprintln!(
                    "SKIPPING: Docker is not reachable or {image_ref} could not be started ({e}); \
                     start Docker to run this test"
                );
                return None;
            }
        };
        let port = mapped_vault_port(&container).await;
        let mut fixture = Self {
            _container: container,
            _slot: slot,
            http: reqwest::Client::new(),
            address: format!("http://127.0.0.1:{port}"),
            prefix: format!("it-{}", Uuid::new_v4().simple()),
            token: String::new(),
        };
        fixture.wait_until_ready().await;
        fixture.configure().await;
        Some(fixture)
    }

    async fn wait_until_ready(&self) {
        for _ in 0..300 {
            if let Ok(r) = self
                .http
                .get(format!("{}/v1/sys/health", self.address))
                .send()
                .await
                && r.status().is_success()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("Vault did not become healthy within 60 s");
    }

    /// The mount, the policy and the plugin's token, as `docs/DESIGN.md`
    /// prescribes for production (apart from the mount's `max_versions`, which
    /// is not an obligation: the plugin sets 100 on every key it writes).
    async fn configure(&mut self) {
        let (status, body) = self
            .raw_as(
                ROOT_TOKEN,
                "POST",
                &format!("sys/mounts/{MOUNT}"),
                Some(json!({"type": "kv", "options": {"version": "2"}})),
            )
            .await;
        assert!(status < 300, "mount KV v2: {status} {body}");
        let (status, body) = self
            .raw(
                "POST",
                &format!("{MOUNT}/config"),
                Some(json!({"max_versions": 1000, "delete_version_after": "0s", "cas_required": false})),
            )
            .await;
        assert!(status < 300, "configure the mount: {status} {body}");
        let (status, body) = self
            .raw(
                "PUT",
                &format!("sys/policies/acl/{POLICY}"),
                Some(json!({"policy": minimal_policy(MOUNT, &self.prefix)})),
            )
            .await;
        assert!(status < 300, "write the policy: {status} {body}");
        self.token = self.create_token().await;
    }

    /// A new token holding only the minimal policy.
    pub async fn create_token(&self) -> String {
        let (status, body) = self
            .raw(
                "POST",
                "auth/token/create",
                Some(json!({"policies": [POLICY], "no_default_policy": true, "ttl": "1h"})),
            )
            .await;
        assert!(status < 300, "create a token: {status} {body}");
        body["auth"]["client_token"]
            .as_str()
            .expect("the response carries a client token")
            .to_owned()
    }

    /// Revokes `token` (and nothing else).
    pub async fn revoke_token(&self, token: &str) {
        let (status, body) = self
            .raw("POST", "auth/token/revoke", Some(json!({"token": token})))
            .await;
        assert!(status < 300, "revoke a token: {status} {body}");
    }

    /// A request to Vault with the root token; `path` is relative to `/v1/`.
    /// Returns the status and the JSON body (`Null` when empty).
    pub async fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.raw_as(ROOT_TOKEN, method, path, body).await
    }

    /// Like [`Self::raw`], with an explicit token.
    pub async fn raw_as(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = self
            .http
            .request(
                method.parse().expect("method"),
                format!("{}/v1/{path}", self.address),
            )
            .header("X-Vault-Token", token);
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await.expect("request to Vault");
        let status = response.status().as_u16();
        let text = response.text().await.expect("response body");
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Plugin configuration for this server with the minimal-policy token.
    pub fn config(&self) -> VaultCredStorePluginConfig {
        VaultCredStorePluginConfig {
            address: self.address.clone(),
            token: Some(VaultToken::from(self.token.as_str())),
            mount: MOUNT.to_owned(),
            path_prefix: self.prefix.clone(),
            timeout_secs: 10,
            retry: RetryConfig {
                max_attempts: 3,
                base_delay_ms: 50,
            },
            ..VaultCredStorePluginConfig::default()
        }
    }

    /// A plugin built from [`Self::config`].
    pub fn plugin(&self) -> Service {
        plugin_from(&self.config())
    }
}

/// A plugin built from `cfg`, the way the gear builds it.
pub fn plugin_from(cfg: &VaultCredStorePluginConfig) -> Service {
    cfg.validate().expect("a valid test configuration");
    let transport = ReqwestTransport::from_config(cfg).expect("transport builds");
    Service::new(Arc::new(transport), cfg)
}

/// The KV v2 path of a key's `kind` (`data`, `metadata`, `delete`, ...)
/// relative to `/v1/`.
pub fn key_path(fixture: &VaultFixture, kind: &str, key: &credstore_sdk::StoreKey) -> String {
    format!(
        "{MOUNT}/{kind}/{}/{}/{}",
        fixture.prefix, key.tenant_id.0, key.record_id
    )
}

/// A fresh key under a random tenant.
pub fn fresh_key() -> credstore_sdk::StoreKey {
    credstore_sdk::StoreKey::new(credstore_sdk::TenantId(Uuid::new_v4()), Uuid::new_v4())
}

/// Test-only predicates on the plugin error.
pub trait PluginErrorExt {
    fn is_unavailable(&self) -> bool;
    fn is_credentials_rejected(&self) -> bool;
}

impl PluginErrorExt for credstore_sdk::CredStorePluginError {
    fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
    fn is_credentials_rejected(&self) -> bool {
        matches!(self, Self::CredentialsRejected { .. })
    }
}
