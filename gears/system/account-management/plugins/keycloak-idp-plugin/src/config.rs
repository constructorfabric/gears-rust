//! Plugin configuration.
//!
//! Mirrors the worked example at
//! `docs/design/DESIGN-vp-idp-plugin-202605192055/vp-idp-plugin.config.example.yaml`.
//! Per the ADDENDUM, static admin creds (`bootstrap_admin.client_secret`,
//! `realm_admin.default_shared_realm_secret`) are templated `${VAR}` strings
//! resolved at config-load time via toolkit's `ExpandVars` (symmetric with the
//! AM DB DSN's `${PGPASSWORD}` substitution). Operators may also inline a
//! literal value. Per-realm dynamic secrets for `Adopted` / `Created` use the
//! `realm_admin.secret_ref_template`.

use std::time::Duration;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use toolkit::var_expand::{ExpandVars as ExpandVarsTrait, ExpandVarsError};
use toolkit_gts::gts_id;
use toolkit_macros::ExpandVars;
use url::Url;
use uuid::Uuid;

/// Wrapper around a config string whose final value is a secret.
///
/// Holds a plain `String` for `Deserialize` + `ExpandVars` compatibility
/// (toolkit's macro derives env-var substitution on `String` fields and
/// teaching it about `secrecy::SecretString` is out of scope), but
/// suppresses every accidental leak surface around it:
///
/// * No `Display` impl — `format!("{}", secret)` won't compile.
/// * `Debug` emits `<redacted>`, so `anyhow!("{:?}", config)` /
///   panic-formatter dumps don't print the resolved value.
/// * No `Serialize`, no `PartialEq` — secret bytes never leak through a
///   `to_yaml()` / config-snapshot path or an assertion message.
/// * The only read accessor is [`Self::expose`] (deliberately verbose
///   so every leak site is grep-able) plus
///   [`Self::clone_into_secret_string`] which moves the value behind
///   the `secrecy` opaque-debug guarantee at the factory boundary.
///
/// The newtype implements [`ExpandVarsTrait`] manually so the
/// `#[expand_vars]` derive on its parent struct keeps working —
/// substitution happens on the inner `String` before any consumer
/// sees the value.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct SecretFromEnv(String);

impl SecretFromEnv {
    /// Read the resolved secret bytes. Use **only** at boundaries that
    /// hand the value to a `secrecy`-protected sink — typically
    /// [`Self::clone_into_secret_string`] is what you want.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Clone the resolved secret into a [`SecretString`] for downstream
    /// `secrecy`-tracked usage (factory caches, token POST bodies,
    /// realm-admin source enum). The intermediate `String` lives only
    /// on this function's stack.
    #[must_use]
    pub fn clone_into_secret_string(&self) -> SecretString {
        SecretString::from(self.0.clone())
    }

    /// Construct from an unexpanded literal. Crate-private and named to
    /// stand out at call sites — production code MUST go through
    /// `Deserialize` + `ExpandVars` so `${VAR}` placeholders are
    /// substituted before any consumer sees the value. The only legitimate
    /// callers today are unit-test fixtures + `domain::test_support`.
    ///
    /// The `#[cfg_attr(not(test), allow(dead_code))]` keeps the function
    /// callable from `#[cfg(test)]` modules (where it's the test-fixture
    /// entry point) without tripping `dead_code` in production builds.
    /// The function itself is NOT `#[cfg(test)]`-gated so DE1101
    /// (tests-in-separate-files) doesn't see it as inline test code
    /// alongside the companion `config_tests.rs`.
    #[doc(hidden)]
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(crate) fn from_literal_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl std::fmt::Debug for SecretFromEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl ExpandVarsTrait for SecretFromEnv {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        self.0.expand_vars()
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackoffKind {
    #[default]
    Exponential,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JitterKind {
    #[default]
    Full,
    None,
}

/// HTTP retry policy for Keycloak Admin REST. Applied to 5xx / 429 / transport
/// errors only — 4xx (other than 429) and 2xx return immediately. Per DESIGN §4.4.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRetryPolicy {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub backoff: BackoffKind,
    #[serde(default)]
    pub jitter: JitterKind,
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    #[serde(default = "default_max_backoff_ms")]
    pub max_backoff_ms: u64,
}

fn default_max_retries() -> u32 {
    3
}
fn default_initial_backoff_ms() -> u64 {
    100
}
fn default_max_backoff_ms() -> u64 {
    5_000
}

impl Default for HttpRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            backoff: BackoffKind::default(),
            jitter: JitterKind::default(),
            initial_backoff_ms: default_initial_backoff_ms(),
            max_backoff_ms: default_max_backoff_ms(),
        }
    }
}

/// Lightweight probe struct used to read the `enabled` flag without requiring
/// all mandatory Keycloak fields to be present. Deserialization of
/// [`VpIdpPluginConfig`] is only attempted when the flag is `true` (default).
///
/// This mirrors the pattern used by `temporal-workflow-executor-plugin`: read
/// `enabled` via `ctx.config_or_default::<VpIdpPluginEnabledProbe>()`, then
/// proceed with the full config parse only when enabled.
#[derive(Debug, Default, Deserialize)]
pub struct VpIdpPluginEnabledProbe {
    /// When `false` the module skips full init. Defaults to `true`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct VpIdpPluginConfig {
    pub security_context: SecurityContextConfig,
    #[expand_vars]
    pub keycloak: KeycloakConfig,
    pub tenant_facade: TenantFacadeConfig,
    pub user_facade: UserFacadeConfig,
    /// Machine-identity lifecycle configuration (confidential OIDC clients).
    #[serde(default)]
    pub service_principal: ServicePrincipalConfig,

    /// Vendor name advertised on the `PluginV1<IdpPluginSpecV1>`
    /// instance this module publishes to types-registry, and the key
    /// AM's `choose_plugin_instance` filter matches against
    /// `cfg.idp.vendor` to pick a candidate. Defaults to
    /// `"virtuozzo"` — deploys that want this plugin to win
    /// selection MUST set `account-management.idp.vendor = "virtuozzo"`
    /// to align, otherwise AM falls back per `idp.required`.
    #[serde(default = "default_vendor")]
    pub vendor: String,

    /// Plugin selection priority — lower wins on tie-breaks within
    /// the same vendor. Defaults to `50` (lower than
    /// `static-idp-plugin`'s `100`) so an environment that publishes
    /// both under `vendor = "virtuozzo"` (e.g. a transitional canary
    /// with two vp-idp-plugin versions) deterministically picks the
    /// newer one if it pins a lower number, while leaving headroom
    /// for emergency "force this plugin to win" overrides via
    /// `i16::MIN`.
    #[serde(default = "default_priority")]
    pub priority: i16,
}

fn default_vendor() -> String {
    "virtuozzo".to_owned()
}

const fn default_priority() -> i16 {
    50
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityContextConfig {
    /// Tenant under which the plugin owns its infrastructure secrets.
    ///
    /// Defaults to the platform **root** tenant (`…0001`, AM bootstrap's
    /// "Platform" tenant). This MUST be a REAL tenant: credstore's own-tenant
    /// gate (`scope_includes_tenant`) checks that the secret's owning tenant
    /// falls inside the plugin's RBAC-granted scope. The nil UUID is not a real
    /// tenant — it is in no grant's materialized subtree — so it fails the gate
    /// (`AccessDenied` on every dedicated-realm secret write). The root tenant
    /// is the correct platform-scope home and is covered by the plugin's
    /// Credstore Secret Operator grant.
    #[serde(default = "default_platform_tenant_id")]
    pub platform_tenant_id: Uuid,
}

/// Platform root tenant id — AM bootstrap's stable root ("Platform") tenant.
/// The plugin's infrastructure secrets are owned here so the credstore
/// own-tenant gate passes under the plugin's Credstore Secret Operator grant.
fn default_platform_tenant_id() -> Uuid {
    Uuid::from_u128(1)
}

#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct KeycloakConfig {
    pub base_url: Url,
    #[expand_vars]
    pub bootstrap_admin: BootstrapAdminConfig,
    #[expand_vars]
    pub realm_admin: RealmAdminConfig,
    #[serde(default)]
    pub tls_ca_bundle_ref: Option<String>,
    #[serde(default = "default_token_safety_ms")]
    pub admin_token_lifetime_safety_ms: u64,
    #[serde(default = "default_http_timeout_ms")]
    pub http_request_timeout_ms: u64,
    /// Humantime string (e.g. `"30m"`) or `null` to disable. Reactive-401 is always on.
    #[serde(default, with = "humantime_serde")]
    pub credential_refresh_interval: Option<Duration>,
    /// HTTP retry policy applied to 5xx / 429 / transport errors (DESIGN §4.4).
    #[serde(default)]
    pub http_retry_policy: HttpRetryPolicy,
    /// Metrics cardinality controls (DESIGN §4.4, §9).
    #[serde(default)]
    pub metrics: KeycloakMetricsConfig,
    /// Init-time bootstrap-admin pre-warm retry budget (DESIGN §4.0 init
    /// lifecycle / §4.4 line 197). Operators with slow-starting KC pods
    /// can extend this without a code change; the defaults match the
    /// `~12s of backoff + per-attempt timeout` budget the runbook documents.
    #[serde(default)]
    pub bootstrap_pre_warm: BootstrapPreWarmConfig,
}

/// Bootstrap-admin pre-warm retry budget. `MAX_ATTEMPTS - 1` backoffs
/// apply BETWEEN attempts — the last attempt's failure is propagated
/// without an extra sleep.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapPreWarmConfig {
    /// Max attempts before init bails. Default `5`.
    #[serde(default = "default_pre_warm_max_attempts")]
    pub max_attempts: u32,
    /// Linear backoff between attempts. Humantime string (e.g. `"3s"`).
    /// Default `"3s"`.
    #[serde(default = "default_pre_warm_backoff", with = "humantime_serde")]
    pub backoff: Duration,
}

fn default_pre_warm_max_attempts() -> u32 {
    5
}
fn default_pre_warm_backoff() -> Duration {
    Duration::from_secs(3)
}

impl Default for BootstrapPreWarmConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_pre_warm_max_attempts(),
            backoff: default_pre_warm_backoff(),
        }
    }
}

/// Metrics-specific configuration nested under `keycloak:` (DESIGN §4.4, §9).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeycloakMetricsConfig {
    /// Maximum number of distinct `realm_name` values that may appear as a
    /// metric label before the plugin drops the label and falls back to a
    /// tracing-span attribute. Default `500`. Set to `0` to disable (never
    /// drop).
    #[serde(default = "default_realm_label_cap")]
    pub drop_realm_label_at_cardinality: u32,
}

fn default_realm_label_cap() -> u32 {
    500
}

impl Default for KeycloakMetricsConfig {
    fn default() -> Self {
        Self {
            drop_realm_label_at_cardinality: default_realm_label_cap(),
        }
    }
}

fn default_token_safety_ms() -> u64 {
    30_000
}
fn default_http_timeout_ms() -> u64 {
    5_000
}

#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct BootstrapAdminConfig {
    #[serde(default = "default_master_realm")]
    pub realm: String,
    #[serde(default = "default_bootstrap_client_id")]
    pub client_id: String,
    /// Bootstrap admin client secret. Supports `${VAR}` templating expanded at
    /// config-load time via toolkit's `ExpandVars` (ADDENDUM §3.1) — symmetric
    /// with how the AM DB DSN handles `${PGPASSWORD}`. Operators may also
    /// inline a literal value.
    ///
    /// Wrapped in [`SecretFromEnv`] so `{:?}` / `Display` / `Serialize`
    /// surfaces don't leak the resolved value. The wrapper implements
    /// the workspace `ExpandVarsTrait` manually, so the `#[expand_vars]`
    /// derive on this struct keeps substituting `${VAR}` placeholders.
    #[expand_vars]
    pub client_secret: SecretFromEnv,
}

fn default_master_realm() -> String {
    "master".into()
}
fn default_bootstrap_client_id() -> String {
    "vp-idp-plugin-bootstrap".into()
}

#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct RealmAdminConfig {
    #[serde(default = "default_realm_admin_client_id")]
    pub client_id_default: String,
    /// Default-shared-realm admin client secret. Supports `${VAR}` templating
    /// expanded at config-load time via toolkit's `ExpandVars` (ADDENDUM §3.2).
    /// Operators may also inline a literal value.
    ///
    /// Wrapped in [`SecretFromEnv`] — see [`BootstrapAdminConfig::client_secret`]
    /// for the rationale (Debug-redacted, no Display / Serialize surface).
    #[expand_vars]
    pub default_shared_realm_secret: SecretFromEnv,
    /// `SecretRef` ID template for non-default-shared realms (Adopted / Created).
    /// Must yield a valid `SecretRef` (≤255 chars, `[A-Za-z0-9_-]+`).
    #[serde(default = "default_secret_ref_template")]
    pub secret_ref_template: String,
}

fn default_realm_admin_client_id() -> String {
    "vp-idp-plugin-realm-admin".into()
}
fn default_secret_ref_template() -> String {
    "vp-idp-realm-admin-{realm_name}-secret".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantFacadeConfig {
    /// Pass-through into Keycloak `RealmRepresentation` at `Created` provision time.
    #[serde(default)]
    pub realm_defaults: serde_json::Value,
    #[serde(default = "default_tenant_group_root")]
    pub tenant_group_root: String,
    /// Per-call upper bound on Keycloak Admin REST work (transport + retries) before
    /// returning `Ambiguous`. Per DESIGN §4.2.
    #[serde(default = "default_provision_timeout_ms")]
    pub provision_timeout_ms: u64,
}

fn default_tenant_group_root() -> String {
    "/tenants".into()
}

fn default_provision_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserFacadeConfig {
    #[serde(default = "default_user_required_actions")]
    pub default_user_required_actions: Vec<String>,
    #[serde(default = "default_list_limit")]
    pub list_users_page_limit_default: u32,
    #[serde(default = "default_list_limit_max")]
    pub list_users_page_limit_max: u32,
    #[serde(default = "default_session_revoke")]
    pub session_revoke_on_deprovision: bool,
}

fn default_user_required_actions() -> Vec<String> {
    vec!["VERIFY_EMAIL".into()]
}
fn default_list_limit() -> u32 {
    50
}
fn default_list_limit_max() -> u32 {
    200
}
fn default_session_revoke() -> bool {
    true
}

/// `service_principal` — tenant-scoped machine-identity lifecycle
/// (confidential `client_credentials` clients in the platform realm).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServicePrincipalConfig {
    /// Realm hosting service-principal clients. MUST name the shared
    /// realm whose issuer tenants trust (the same realm configured at
    /// AM bootstrap as `root_tenant_metadata.realm_name`) — the two
    /// keys have no cross-validation, keep them in sync.
    pub realm: String,
    /// Client scopes a caller may request. Empty = none attachable.
    pub scope_allowlist: Vec<String>,
    /// Max service-principal clients per tenant. Best-effort cap (no
    /// cross-replica serialization): concurrent creates may briefly
    /// overshoot; not a security boundary.
    pub per_tenant_quota: u32,
    /// `user_type` attribute value for service-account users. Must
    /// contain `subject_service` (RBAC principal-resolver pattern).
    pub subject_type: String,
}

impl Default for ServicePrincipalConfig {
    fn default() -> Self {
        Self {
            realm: "platform".into(),
            scope_allowlist: Vec::new(),
            per_tenant_quota: 10,
            subject_type: gts_id!("cf.core.security.subject_service_principal.v1~").into(),
        }
    }
}

impl ServicePrincipalConfig {
    /// Init-time validation; called from module init.
    ///
    /// `realm` cannot be cross-validated against AM's shared realm here — that
    /// value lives in account-management's bootstrap config, not this plugin's.
    /// The field defaults to `platform` when the section is omitted (see the
    /// `#[serde(default)]` on `VpIdpPluginConfig::service_principal`), so a
    /// deployment on a non-`platform` shared realm MUST set `realm` explicitly
    /// or principals are created/purged against the wrong realm. The check
    /// below only rejects an explicitly blank value.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `realm` is blank, or if `subject_type` does not
    /// contain `"subject_service"`.
    pub fn validate(&self) -> Result<(), String> {
        if self.realm.trim().is_empty() {
            return Err(
                "service_principal.realm must be a non-empty realm name (the shared realm \
                 whose issuer tenants trust — keep in sync with account-management \
                 bootstrap.root_tenant_metadata.realm_name)"
                    .to_string(),
            );
        }
        if !self.subject_type.contains("subject_service") {
            return Err(format!(
                "service_principal.subject_type '{}' must contain 'subject_service' \
                 (RBAC principal-resolver classification pattern)",
                self.subject_type,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
