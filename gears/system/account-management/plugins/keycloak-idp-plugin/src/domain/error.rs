//! Internal plugin error.
//!
//! Boundaries (`idp_impl.rs` in Phase 15) translate `PluginError` into the
//! typed SDK failure variants per DESIGN §5.5. Inside the plugin we deal in
//! `PluginError` exclusively so call sites don't sprinkle SDK-type construction
//! around the codebase.
//!
//! `detail` format per DESIGN §5.5:
//!   `kc:{METHOD} {path_template} -> {status}: {body_first_2KB}`
//! for KC REST failures; `internal:{component}: {short message}` otherwise.

use std::sync::OnceLock;

use thiserror::Error;
use toolkit_macros::domain_model;

/// DESIGN §8.2 defence-in-depth: regex-redact obvious credential leaks in any
/// `body_first_2KB` we emit, before the value crosses the AM boundary. AM owns
/// the authoritative redaction; this is a belt-and-braces layer.
///
/// Two compiled patterns with explicit role split (so they don't fight each
/// other on the same input):
///
/// 1. `BEARER_RE` — matches `Bearer <token>` anywhere (HTTP-header echo,
///    JSON-wrapped `"Bearer X"`, form-body fragments). Covers every
///    `Authorization`-header echo we've seen from KC.
/// 2. `KV_RE` — matches `key[:=]value` for `client_secret` and `password`
///    only. `authorization` is deliberately NOT here — `kv_re`'s `\S+`
///    value capture is too greedy for the `Authorization: Bearer X`
///    shape (it would consume `Bearer` while leaving the token), and
///    `bearer_re` already covers that case.
#[allow(clippy::expect_used)] // regex literals are compile-time constants; cannot fail at runtime
fn bearer_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)(bearer)\s+([A-Za-z0-9._~+/\-]+=*)").expect("valid regex")
    })
}

#[allow(clippy::expect_used)] // regex literals are compile-time constants; cannot fail at runtime
fn kv_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Value-bound: stop at JSON / form / shell delimiters so we
        // don't swallow trailing fields of a multi-field body (see the
        // regression test pinning the JSON-after-secret shape).
        //
        // Key set covers both snake_case (form bodies, our own
        // request/response shapes) and the camelCase variants Keycloak
        // emits on certain error / discovery responses
        // (`ClientRepresentation.secret`, `clientSecret`, `refreshToken`,
        // …). `(?i)` does NOT bridge snake_case↔camelCase, so each
        // shape is enumerated explicitly. `\bsecret\b` is anchored
        // with word boundaries so it doesn't false-positive on
        // `client_secrets` / `secretarial`.
        regex::Regex::new(
            r#"(?ix)
              ( client_secret
              | clientSecret
              | password
              | refresh_token
              | refreshToken
              | access_token
              | accessToken
              | id_token
              | idToken
              | \bsecret\b
              )
              ["'\s]*[:=]["'\s]*
              [^"',&\s}]+
            "#,
        )
        .expect("valid regex")
    })
}

/// Redact obvious credential patterns from a response body string.
///
/// Implements DESIGN §8.2 defence-in-depth before the value is stored in
/// `body_first_2kb` of a [`PluginError::KcRest`]:
///
/// - `Bearer <token>` → `Bearer <redacted>` (space separator, matching the
///   HTTP-header echo shape that the dedicated `BEARER_RE` pattern targets).
/// - `key[:=]value` for `client_secret` / `password` / `authorization`
///   → `key=<redacted>` (preserves the `=` form because that's how the
///   surrounding form-body or JSON serialises the original).
#[must_use]
pub fn redact_secrets(body: &str) -> String {
    let s = bearer_re().replace_all(body, "$1 <redacted>");
    kv_re().replace_all(&s, "$1=<redacted>").into_owned()
}

#[domain_model]
#[derive(Debug, Error)]
pub enum PluginError {
    /// Keycloak Admin REST failure. `path_template` keeps literal
    /// `{realm}` / `{tenant_id}` / `{client_uuid}` placeholders to stay
    /// grep-friendly; concrete IDs ride on adjacent structured fields.
    /// `body_first_2kb` is verbatim-truncated.
    #[error("kc:{method} {path_template} -> {status}: {body_first_2kb}")]
    KcRest {
        method: &'static str,
        path_template: String,
        status: KcStatusKind,
        body_first_2kb: String,
    },
    #[error("internal:CredStoreReader: {detail}")]
    CredStoreRead { detail: String },
    #[error("internal:MetadataCodec: {0}")]
    MetadataDecode(#[from] crate::domain::metadata_codec::DecodeError),
    #[error("internal:Config: {detail}")]
    Config { detail: String },
    /// Caller-supplied `provisioning_metadata` rejected by `parse_input`
    /// BEFORE any KC call (e.g. created+explicit-realm, shared with no
    /// parent metadata, bad mode/realm shape). Distinct from `Config` —
    /// which is also emitted post-KC — so it can map to a clean 400.
    #[error("provisioning input rejected: {detail}")]
    ProvisionInputRejected { detail: String },
    /// Bootstrap admin lacks `view-realm` / `query-groups` on the target realm.
    /// Phase 15 boundary translates this to `IdpProvisionFailure::UnsupportedOperation`
    /// per DESIGN §5.2 (line 432). The detail string is the runbook key the
    /// operator searches for.
    #[error("bootstrap admin lacks view-realm/query-groups in {realm} — see Phase 0 IaC checklist")]
    BootstrapPermsMissing { realm: String },

    /// `mode = created` requested but the target realm already exists in
    /// Keycloak. Validation rejection — NO IdP-side state was created.
    /// Phase 15 boundary maps this to
    /// [`IdpProvisionFailure::CleanFailure`] per DESIGN §5.2.
    #[error("mode = created but realm '{realm}' already exists")]
    CreatedRealmExists { realm: String },

    /// Ambiguous Created-mode failure — Keycloak (or `OpenBao`) side effects
    /// may or may not be in place. Phase 15 boundary maps this to
    /// [`IdpProvisionFailure::Ambiguous { detail }`] per DESIGN §4.2 / §5.5
    /// (line 482): `detail = format!("ambig:{stage}: {body}")`. `stage`
    /// pinpoints which step failed so the operator reconciliation runbook
    /// (§12 risk #9) can prioritise cleanup.
    #[error("{stage}: {detail}")]
    AmbiguousCreated {
        stage: AmbiguousStage,
        detail: String,
    },

    /// Deprovision target was already absent (vendor 404/410, or the
    /// missing-metadata branch — DESIGN §6.2). Phase 15 boundary maps to
    /// [`IdpDeprovisionFailure::NotFound`] which AM treats as a
    /// success-equivalent on the hard-delete pipeline.
    #[error("deprovision target not found in realm '{realm_name}'")]
    DeprovisionNotFound { realm_name: String },

    /// Transient failure during deprovision (5xx / transport / timeout).
    /// Phase 15 boundary maps to [`IdpDeprovisionFailure::Retryable`]; the
    /// row is deferred to the next reaper / retention tick.
    #[error("deprovision retryable: {detail}")]
    DeprovisionRetryable { detail: String },

    /// Operator-must-intervene deprovision failure — e.g. malformed
    /// persisted metadata or other irrecoverable state. Phase 15 boundary
    /// maps to [`IdpDeprovisionFailure::Terminal`].
    #[error("deprovision terminal: {detail}")]
    DeprovisionTerminal { detail: String },

    /// User-op payload was rejected by KC (400/422 that is not a
    /// recognised password-policy reject: invalid email format,
    /// user-profile attribute validation, malformed metadata). Phase
    /// 15 boundary maps this to [`IdpUserOperationFailure::Rejected`]
    /// per DESIGN §5.5 — AM surfaces a generic validation envelope.
    /// Uniqueness conflicts never land here: every KC 409 rides
    /// [`Self::UserOpDuplicate`] (VHP-2158).
    #[error("user op rejected: {detail}")]
    UserOpRejected { detail: String },

    /// User-op hit a KC uniqueness collision — ANY 409 from
    /// user-create (the status is the machine signal; on that endpoint
    /// it has no other cause). `field` is refined from KC's
    /// `errorMessage` when attributable, `UsernameOrEmail` otherwise
    /// (KC 26's combined constant, unparseable bodies). Boundary maps
    /// this to [`IdpUserOperationFailure::DuplicateUser`] so AM
    /// surfaces HTTP 409 `already_exists` (VHP-2158) — wording drift
    /// in KC can degrade the field token, never the status.
    #[error("user op duplicate {field:?}: {detail}")]
    UserOpDuplicate {
        field: account_management_sdk::IdpUserDuplicateField,
        detail: String,
    },

    /// User-op was rejected by KC's password policy (400/422 with a
    /// recognised policy `errorMessage`). Boundary maps this to
    /// [`IdpUserOperationFailure::PasswordPolicy`] so AM can surface
    /// the structured `password` / `PASSWORD_POLICY` field violation
    /// (VHP-2158). Previously this fell through the non-409 catch-all
    /// into `UserOpUnavailable` and mis-surfaced as a retryable 503.
    #[error("user op password policy: {detail}")]
    UserOpPasswordPolicy { detail: String },

    /// User-op failed at transport / 5xx / timeout. Phase 15 boundary
    /// maps to [`IdpUserOperationFailure::Unavailable`] per DESIGN §5.5
    /// — AM surfaces `idp_unavailable` and does NOT serve a fallback
    /// projection.
    #[error("user op unavailable: {detail}")]
    UserOpUnavailable { detail: String },

    /// User-op was explicitly declined by the provider (read-only /
    /// legacy profile). Phase 15 boundary maps to
    /// [`IdpUserOperationFailure::UnsupportedOperation`] per DESIGN §5.5.
    #[error("user op unsupported: {detail}")]
    UserOpUnsupported { detail: String },

    /// Service-principal request rejected before any KC call
    /// (name/scope/quota/conflict). Permanent client error.
    /// Boundary maps this to service-principal-sdk's `InvalidInput`.
    #[error("sp invalid input: {detail}")]
    SpInvalidInput {
        detail: String,
        field: Option<String>,
    },
    /// Service-principal client absent, or not owned by the addressed
    /// tenant (ownership-attribute mismatch is NOT revealed separately).
    /// Boundary maps this to service-principal-sdk's `NotFound`.
    #[error("sp not found: {detail}")]
    SpNotFound { detail: String },
}

/// Stable `failure_variant` label string for the `keycloak_idp_plugin_failure_total`
/// metric (DESIGN §9). Must stay 1:1 with [`PluginError`] variants so dashboards
/// and alerts don't drift when a new variant is added — every new variant adds
/// one branch here.
#[must_use]
pub fn failure_variant_label(e: &PluginError) -> &'static str {
    match e {
        PluginError::Config { .. } => "config",
        PluginError::ProvisionInputRejected { .. } => "provision_input_rejected",
        PluginError::CredStoreRead { .. } => "credstore_read",
        PluginError::MetadataDecode(_) => "metadata_decode",
        PluginError::KcRest { .. } => "kc_rest",
        PluginError::AmbiguousCreated { .. } => "ambiguous_created",
        PluginError::CreatedRealmExists { .. } => "created_realm_exists",
        PluginError::BootstrapPermsMissing { .. } => "bootstrap_perms_missing",
        PluginError::DeprovisionNotFound { .. } => "deprovision_not_found",
        PluginError::DeprovisionRetryable { .. } => "deprovision_retryable",
        PluginError::DeprovisionTerminal { .. } => "deprovision_terminal",
        PluginError::UserOpRejected { .. } => "user_op_rejected",
        PluginError::UserOpDuplicate { .. } => "user_op_duplicate",
        PluginError::UserOpPasswordPolicy { .. } => "user_op_password_policy",
        PluginError::UserOpUnavailable { .. } => "user_op_unavailable",
        PluginError::UserOpUnsupported { .. } => "user_op_unsupported",
        PluginError::SpInvalidInput { .. } => "sp_invalid_input",
        PluginError::SpNotFound { .. } => "sp_not_found",
    }
}

/// Wire prefix on every `IdpProvisionFailure::Ambiguous::detail` produced by
/// the plugin (DESIGN §4.2.1). Operator reconciliation tooling parses this
/// prefix to bucket failures by saga stage.
pub(crate) const AMBIG_PREFIX: &str = "ambig:";

/// Which Created-mode saga step produced an
/// [`PluginError::AmbiguousCreated`]. Stable strings — these are part of the
/// `detail` prefix emitted to AM (DESIGN §5.5: `ambig:kc_realm_create`,
/// `ambig:kc_client_create`, `ambig:kc_role_mapping`,
/// `ambig:kc_client_secret_read`, `ambig:openbao_put_after_kc_success`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbiguousStage {
    KcRealmCreate,
    KcClientCreate,
    KcRoleMapping,
    KcClientSecretRead,
    OpenBaoPutAfterKcSuccess,
    /// Saga exceeded `provision_timeout_ms`. Wire detail: `ambig:timeout`.
    Timeout,
    /// Shared-mode admin-user bind (GET-modify-PUT on `/users/{id}`)
    /// failed AFTER the tenant group was already created. Operator
    /// runbook: the tenant group exists in KC but the admin user is
    /// not bound — re-run provisioning is safe (idempotent), or
    /// operator can manually set `tenant_id` + `user_type` attributes
    /// on the admin user. Wire detail: `ambig:admin_user_bind`.
    AdminUserBind,
    /// SA-user attribute GET-modify-PUT failed after client create.
    /// Wire detail: `ambig:kc_sa_attr_set`.
    KcSaAttrSet,
    /// Client-scope attach failed after client create.
    /// Wire detail: `ambig:kc_client_scope_attach`.
    KcClientScopeAttach,
    /// Secret regeneration POST outcome unknown.
    /// Wire detail: `ambig:kc_client_secret_rotate`.
    KcClientSecretRotate,
    /// Client DELETE outcome unknown.
    /// Wire detail: `ambig:kc_client_delete`.
    KcClientDelete,
}

impl std::fmt::Display for AmbiguousStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::KcRealmCreate => "kc_realm_create",
            Self::KcClientCreate => "kc_client_create",
            Self::KcRoleMapping => "kc_role_mapping",
            Self::KcClientSecretRead => "kc_client_secret_read",
            Self::OpenBaoPutAfterKcSuccess => "openbao_put_after_kc_success",
            Self::Timeout => "timeout",
            Self::AdminUserBind => "admin_user_bind",
            Self::KcSaAttrSet => "kc_sa_attr_set",
            Self::KcClientScopeAttach => "kc_client_scope_attach",
            Self::KcClientSecretRotate => "kc_client_secret_rotate",
            Self::KcClientDelete => "kc_client_delete",
        };
        write!(f, "{AMBIG_PREFIX}{name}")
    }
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KcStatusKind {
    Http(u16),
    Transport,
    Timeout,
}

impl KcStatusKind {
    /// `true` when this outcome is worth retrying. Transport- and
    /// timeout-class failures are always transient; HTTP responses defer to
    /// [`is_retryable_status`] (5xx / 429 retry, the rest fast-bail).
    #[must_use]
    pub fn is_retryable(self) -> bool {
        match self {
            Self::Http(s) => is_retryable_status(s),
            Self::Transport | Self::Timeout => true,
        }
    }
}

/// HTTP-status retry policy shared between the low-level transport
/// (`infra::kc_http`) and bootstrap-init pre-warm (`module`). 5xx and 429
/// are treated as transient; everything else in 4xx is permanent so callers
/// fast-bail instead of burning their retry budget on guaranteed-permanent
/// failures (`invalid_client`, missing realm, missing role mapping).
#[must_use]
pub(crate) fn is_retryable_status(status: u16) -> bool {
    status >= 500 || status == 429
}

impl std::fmt::Display for KcStatusKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(s) => write!(f, "{s}"),
            Self::Transport => write!(f, "transport"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
