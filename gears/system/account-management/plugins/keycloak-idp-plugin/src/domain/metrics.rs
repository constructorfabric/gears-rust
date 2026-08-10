//! Stable metric-family name constants for `keycloak-idp-plugin`.
//!
//! Names are **literal Prometheus** instrument names: underscore-separated,
//! lowercase, with the wire-side suffix baked directly into the name —
//! `_total` for monotonic counters, a unit suffix (`_seconds`, and `_ms`
//! only where the instrument is genuinely milliseconds) for histograms,
//! and **no** suffix for gauges / up-down-counters. The adapter does
//! **not** call `with_unit(...)`; the unit lives in the name. This matches
//! the sibling modules (RMS, policy-engine, openbao) and the platform
//! collector's `add_metric_suffixes: false` posture, which preserves
//! these names verbatim on the wire and does NOT auto-append suffixes.
//!
//! The infra adapter (see [`crate::infra::metrics`]) builds one `OTel`
//! instrument per constant below, optionally substituting the leading
//! `keycloak_idp_plugin` namespace token for its `prefix` argument so a host
//! embedding the plugin under a different name can rebrand all emissions
//! in one place.
//!
//! ## Cardinality watchdog (DESIGN §4.4, §9)
//!
//! `realm_name` is a partner-controlled label whose cardinality grows
//! with every Created-mode realm. The watchdog now lives **inside the
//! adapter** (see `KeycloakIdpPluginMetricsAdapter::observe_realm_and_should_emit`),
//! not on the domain struct — call sites simply pass `&str` and the
//! adapter decides whether to attach the label.

// ════════════════════════════════════════════════════════════════════
//  Metric family catalog — literal Prometheus name constants
// ════════════════════════════════════════════════════════════════════

/// `provision_tenant` end-to-end latency. Histogram, unit seconds.
pub const KEYCLOAK_IDP_PLUGIN_PROVISION_TENANT_DURATION: &str =
    "keycloak_idp_plugin_provision_tenant_duration_seconds";

/// User-op latency. Histogram, unit seconds.
pub const KEYCLOAK_IDP_PLUGIN_USER_OP_DURATION: &str =
    "keycloak_idp_plugin_user_op_duration_seconds";

/// KC Admin REST single-call latency. Histogram, unit seconds. Reserved —
/// no live emitters today; declared so the follow-up PR wiring the KC
/// HTTP layer does not need to reshape DI.
pub const KEYCLOAK_IDP_PLUGIN_KC_ADMIN_REQUEST_DURATION: &str =
    "keycloak_idp_plugin_kc_admin_request_duration_seconds";

/// Plugin failures. Counter, labelled by `op` × `failure_variant`.
pub const KEYCLOAK_IDP_PLUGIN_FAILURE: &str = "keycloak_idp_plugin_failure_total";

/// KC Admin token refresh. Counter. Reserved — declared without live
/// emitters (see [`KEYCLOAK_IDP_PLUGIN_KC_ADMIN_REQUEST_DURATION`]).
pub const KEYCLOAK_IDP_PLUGIN_KC_ADMIN_TOKEN_REFRESH: &str =
    "keycloak_idp_plugin_kc_admin_token_refresh_total";

/// Admin `client_secret` re-resolution. Counter. Reserved — declared
/// without live emitters.
pub const KEYCLOAK_IDP_PLUGIN_CREDENTIAL_REFRESH: &str =
    "keycloak_idp_plugin_credential_refresh_total";

/// `OpenBao` writes/deletes for per-realm admin secrets. Counter.
pub const KEYCLOAK_IDP_PLUGIN_CREDSTORE_WRITE: &str = "keycloak_idp_plugin_credstore_write_total";

/// Malformed / unsupported-version metadata blob counter.
pub const KEYCLOAK_IDP_PLUGIN_METADATA_DECODE_FAILURE: &str =
    "keycloak_idp_plugin_metadata_decode_failure_total";

/// `deprovision_tenant` arrived with `metadata = None` under an
/// `am.system` security context. Counter.
pub const KEYCLOAK_IDP_PLUGIN_DEPROVISION_MISSING_METADATA: &str =
    "keycloak_idp_plugin_deprovision_missing_metadata_total";

/// Active realm bindings. `UpDownCounter`, labelled by `realm_binding`
/// × `realm_name` (subject to the adapter's cardinality watchdog). No
/// suffix — up-down-counters carry no `_total`.
pub const KEYCLOAK_IDP_PLUGIN_REALMS_BOUND: &str = "keycloak_idp_plugin_realms_bound";

/// Orphan-user compensation outcomes. Counter, labelled by `outcome`
/// ∈ {`ok`, `failed`}. Emitted by `UserFacade::provision_user_impl` when
/// `add_user_to_group` fails after `create_user` already landed — the
/// compensation path (`delete_user`) is best-effort and operators need
/// an actionable signal distinct from the primary group-add failure.
pub const KEYCLOAK_IDP_PLUGIN_ORPHAN_USER_COMPENSATION: &str =
    "keycloak_idp_plugin_orphan_user_compensation_total";

/// Service-principal op latency. Histogram, unit seconds.
pub const KEYCLOAK_IDP_PLUGIN_SP_OP_DURATION: &str = "keycloak_idp_plugin_sp_op_duration_seconds";

/// Default instrument-name prefix token. The adapter substitutes this
/// for the leading `"keycloak_idp_plugin"` token of the constants above so a
/// host embedding the plugin under a different name can rebrand all
/// emissions in one place. With the default value the substitution is a
/// no-op and names are emitted verbatim.
pub const DEFAULT_PREFIX: &str = "keycloak_idp_plugin";
