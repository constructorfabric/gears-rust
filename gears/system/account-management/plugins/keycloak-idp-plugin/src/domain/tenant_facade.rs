//! `TenantFacade` — provision/deprovision tenants against Keycloak per DESIGN §4.2.
//!
//! Handles all three [`RealmBinding`] modes (Shared / Adopted / Created),
//! including the Created-mode saga that creates a per-tenant realm,
//! realm-admin client, role mappings, and OpenBao-stored realm-admin secret.
//!
//! See DESIGN §4.1 (Adopted row — narrow emptiness scope), §4.2
//! (component design), §5.2 (input contract — `mode = adopted` requires
//! empty parent tenant group), §5.4 ("Group emptiness check" — absent
//! parent is success-equivalent), and ADDENDUM §3.4 (the
//! `admin_secret_ref = None` rule for default-shared-realm tenants).
//!
//! Suppressed module-wide: every private struct below is a wire shape — the
//! `provisioning_metadata` blob AM forwards verbatim, and the field subsets of
//! Keycloak's Admin REST representations this facade reads back.
#![allow(unknown_lints, de0101_no_serde_in_contract)]

use std::sync::{Arc, OnceLock};

use account_management_sdk::idp::{
    IdpDeprovisionTenantRequest, IdpProvisionTarget, IdpProvisionTenantRequest,
};
use credstore_sdk::{SecretRef, SecretValue, SharingMode};
use regex::Regex;
use serde::Deserialize;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{RealmAdminConfig, TenantFacadeConfig};
use crate::domain::audit;
use crate::domain::credstore::CredStoreWriter;
use crate::domain::error::redact_secrets;
use crate::domain::error::{AmbiguousStage, KcStatusKind, PluginError};
use crate::domain::kc::KcAdminClient;
use crate::domain::kc::factory::{KeycloakAdminClientFactory, SecretSource};
use crate::domain::kc::transport::truncate_2kb;
use crate::domain::metadata_codec::{MetadataCodec, RealmBinding, TenantIdpMetadataV1};
use crate::domain::ports::metrics::{
    CredstoreMetricsPort, CredstoreOp, CredstoreOutcome, FailureMetricsPort, FailureVariant,
    MetadataCodecMetricsPort, PluginOp, TenantLifecycleMetricsPort, VersionObserved,
};
use crate::domain::service_principal_facade::ServicePrincipalFacade;

/// Per-realm `realm-management` roles granted to the Created-mode realm-admin
/// client's service account (DESIGN §8.1 least-privilege table for the
/// per-realm admin). Matches the capability matrix:
///   view-realm + view-clients      → read realm metadata
///   query-groups + manage-realm    → manage tenant groups (group CRUD)
///   manage-users + query-users     → manage users
///   manage-clients                 → manage client scopes / protocol mappers
const REALM_ADMIN_REQUIRED_ROLES: &[&str] = &[
    "view-realm",
    "view-clients",
    "query-groups",
    "manage-realm",
    "manage-users",
    "query-users",
    "manage-clients",
];

/// `clientId` of the Keycloak built-in `realm-management` client. Each realm
/// hosts one such client; its roles (`view-realm`, `manage-users`, …) are the
/// canonical per-realm RBAC anchors granted to the plugin's realm-admin
/// service account.
const REALM_MANAGEMENT_CLIENT_ID: &str = "realm-management";

/// Realm attribute key stamping the AM tenant that owns a Created-mode
/// realm. Written into the `RealmRepresentation.attributes` map at
/// realm-create time so that (create-path idempotency):
///   * a retried / re-entrant realm-create recognises a realm as its own —
///     the unique-name 409 from a duplicate POST reconciles to idempotent
///     success instead of a false `AmbiguousCreated` → AM 500, and
///   * the orphan reaper can GC dedicated realms by owning tenant.
pub(crate) const PROVISIONING_TENANT_ID_ATTR: &str = "cf.provisioning.tenant_id";

/// Outcome of probing whether a Created-mode realm already exists and who
/// owns it, per the [`PROVISIONING_TENANT_ID_ATTR`] marker.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealmProbe {
    /// `GET /admin/realms/{realm}` → 404. Realm absent; safe to create.
    Absent,
    /// Realm present and its ownership marker matches the requesting tenant.
    OwnedByUs,
    /// Realm present but the marker is missing or names a different tenant.
    Foreign,
}

/// Parse the owning tenant id out of a `RealmRepresentation` body by reading
/// the [`PROVISIONING_TENANT_ID_ATTR`] attribute. Tolerates both the scalar
/// (`"id"`) and single-element-array (`["id"]`) attribute encodings Keycloak
/// versions have used. Returns `None` when the body is unparsable or the
/// marker is absent / malformed.
fn realm_owner_tenant_id(body: &str) -> Option<Uuid> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let attr = value.get("attributes")?.get(PROVISIONING_TENANT_ID_ATTR)?;
    let raw = match attr {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(items) => items.first()?.as_str()?,
        _ => return None,
    };
    Uuid::parse_str(raw).ok()
}

/// Decoded `provisioning_metadata` input (DESIGN §5.2 wire shape).
#[domain_model]
#[derive(Debug)]
struct ParsedInput {
    realm_binding: RealmBinding,
    realm_name: String,
    /// Optional Keycloak user UUID for admin-bind bootstrap.
    /// When present and `realm_binding = Shared`, `provision_shared` will
    /// PATCH that user's attributes with `tenant_id` and `user_type = "user"`.
    /// v1: bind only in Shared mode (DESIGN bump required to expand).
    admin_user_id: Option<Uuid>,
}

/// Wire shape of `provisioning_metadata` (DESIGN §5.2 — `mode`/`realm_name`).
///
/// `deny_unknown_fields` makes future schema drift (e.g. a v2 plugin emitting
/// extra keys) loud rather than silent: a v1 plugin reading a v2 input fails
/// the request (`ProvisionInputRejected` → 400) instead of stripping the extras
/// and acting on a partial view.
#[domain_model]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMetadata {
    /// Realm-binding mode. Optional now: absent ⇒ `shared`. The
    /// effective realm is derived from `target` + mode in `parse_input`, so
    /// `mode` and `realm_name` are no longer mandatory on the wire.
    #[serde(default)]
    mode: Option<String>,
    /// Explicit realm name. Required only for Root and `adopted`; ignored for
    /// `shared` (inherited from the parent) and rejected for `created`.
    #[serde(default)]
    realm_name: Option<String>,
    /// Optional Keycloak user UUID for admin-bind bootstrap.
    /// Missing field deserialises as `None` — backward compatible with
    /// existing call sites that omit the field.
    ///
    /// `""` (empty string) also deserialises as `None`. Reason: production
    /// `config/server.yaml` always carries `admin_user_id: "${VP_..._USER_ID:-}"`
    /// so the bind step is default-on; in dev / non-IaC environments the env
    /// var is unset → POSIX default-empty expansion leaves an empty string,
    /// which must decode as "no bind" rather than fail the saga.
    #[serde(default, deserialize_with = "deserialize_optional_uuid_or_empty")]
    admin_user_id: Option<Uuid>,
}

/// Custom deserializer treating both missing and empty-string inputs as `None`.
/// See `WireMetadata::admin_user_id` doc comment for rationale.
fn deserialize_optional_uuid_or_empty<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => Uuid::parse_str(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Subset of Keycloak's `GroupRepresentation` the facade needs (the `id`
/// returned by `POST .../groups` / `POST .../children`, plus the `name` used
/// to match the parent in a `search=&exact=true` listing).
#[domain_model]
#[derive(Deserialize)]
struct GroupRep {
    id: Uuid,
    #[serde(default)]
    name: String,
}

/// Subset of Keycloak's `ClientRepresentation` the facade needs when looking
/// up the freshly-created realm-admin client (and the built-in
/// `realm-management` client) by `clientId` via
/// `GET /admin/realms/{realm}/clients?clientId=...`.
#[domain_model]
#[derive(Deserialize)]
struct ClientRep {
    id: Uuid,
}

/// Subset of Keycloak's `UserRepresentation` for the service-account-user
/// endpoint (`GET .../clients/{uuid}/service-account-user`).
#[domain_model]
#[derive(Deserialize)]
struct UserRep {
    id: Uuid,
}

/// Subset of Keycloak's `RoleRepresentation` for role-mapping calls. Both
/// `id` and `name` MUST be echoed back when `POST`-ing to
/// `/role-mappings/clients/{rm_uuid}`.
#[domain_model]
#[derive(Deserialize, serde::Serialize, Clone)]
struct RoleRep {
    id: Uuid,
    name: String,
}

/// Subset of Keycloak's `CredentialRepresentation` for the
/// `GET .../clients/{uuid}/client-secret` endpoint.
#[domain_model]
#[derive(Deserialize)]
struct CredentialRep {
    value: String,
}

/// Compiled form of the DESIGN §5.2 realm-name regex. Initialised once via
/// [`OnceLock`]; panics only if the literal pattern is malformed (compile-time
/// invariant — the pattern is a constant).
#[allow(clippy::expect_used)] // regex literal is a compile-time constant; cannot fail at runtime
fn realm_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]{1,200}$").expect("valid regex"))
}

/// Re-tag a transport-level [`PluginError::KcRest`] surfaced by
/// [`KcAdminClient::raw_request`] as an [`PluginError::AmbiguousCreated`] so the
/// caller's stage attribution is preserved. Created-mode mid-saga transport
/// failures are inherently ambiguous (the request may have landed; we just
/// never got the response), so this collapse is correct.
pub(crate) fn ambiguous_from_kc_error(stage: AmbiguousStage, e: PluginError) -> PluginError {
    match e {
        PluginError::KcRest {
            method,
            path_template,
            status,
            body_first_2kb,
        } => PluginError::AmbiguousCreated {
            stage,
            detail: format!("kc:{method} {path_template} -> {status}: {body_first_2kb}"),
        },
        other => other,
    }
}

// `failure_variant_label` lives in `crate::domain::error` (single source of truth).

/// Re-tag any [`PluginError::KcRest`] surfaced on the deprovision saga as
/// [`PluginError::DeprovisionRetryable`] per DESIGN §6.2 — transport /
/// timeout / non-2xx upstream failures all defer to the next retention
/// tick. Non-[`PluginError::KcRest`] variants (e.g.
/// [`PluginError::CredStoreRead`],
/// [`PluginError::BootstrapPermsMissing`]) pass through unchanged so the
/// Phase 15 boundary can translate them.
fn deprovision_translate_kc(e: PluginError) -> PluginError {
    match e {
        PluginError::KcRest {
            method,
            path_template,
            status,
            body_first_2kb,
        } => PluginError::DeprovisionRetryable {
            detail: format!("kc:{method} {path_template} -> {status}: {body_first_2kb}"),
        },
        other => other,
    }
}

/// Classify failures from the service-principal purge step when it runs as
/// part of tenant deprovisioning. Retryable KC statuses keep the retention row
/// moving; permanent 4xx/config-shape failures park as Terminal so operators
/// get one actionable row instead of an endless retention loop.
fn deprovision_translate_sp_purge(e: PluginError) -> PluginError {
    match e {
        PluginError::KcRest {
            method,
            path_template,
            status,
            body_first_2kb,
        } if status.is_retryable() => PluginError::DeprovisionRetryable {
            detail: format!(
                "service-principal purge: kc:{method} {path_template} -> {status}: {body_first_2kb}"
            ),
        },
        PluginError::KcRest {
            method,
            path_template,
            status,
            body_first_2kb,
        } => PluginError::DeprovisionTerminal {
            detail: format!(
                "service-principal purge: kc:{method} {path_template} -> {status}: {body_first_2kb}"
            ),
        },
        other @ PluginError::AmbiguousCreated { .. } => PluginError::DeprovisionRetryable {
            detail: format!("service-principal purge ambiguous: {other}"),
        },
        // No explicit `SpNotFound` arm: `purge_tenant_principals` swallows
        // per-client 404s internally, so it never surfaces here. The
        // catch-all maps any hypothetical future `SpNotFound` to
        // `DeprovisionTerminal` — NOT `DeprovisionNotFound`, which would be
        // read as success-equivalent and skip realm/tenant teardown.
        other => PluginError::DeprovisionTerminal {
            detail: format!("service-principal purge: {other}"),
        },
    }
}

#[domain_model]
pub struct TenantFacade {
    cfg: TenantFacadeConfig,
    realm_admin_cfg: RealmAdminConfig,
    kc_factory: Arc<KeycloakAdminClientFactory>,
    /// Used by `deprovision_tenant_inner` to decode the persisted blob; the
    /// encode-on-success path lives at the `idp_impl` boundary on the plugin
    /// root struct.
    codec: Arc<MetadataCodec>,
    /// Used by Created mode (DESIGN §4.7 / §8.1) to persist the per-realm
    /// admin secret under the templated `SecretRef`. Shared / Adopted never
    /// write — those realms' admin secrets are operator-provisioned.
    cs_writer: CredStoreWriter,
    // Segregated metric ports (DESIGN §9). One `Arc` per subdomain so a
    // future test fixture can mock just the surface it asserts on.
    tenant_metrics: Arc<dyn TenantLifecycleMetricsPort>,
    credstore_metrics: Arc<dyn CredstoreMetricsPort>,
    metadata_metrics: Arc<dyn MetadataCodecMetricsPort>,
    failure_metrics: Arc<dyn FailureMetricsPort>,
    /// Optional cascade hook installed by module wiring after both facades are
    /// constructed. Tests that focus only on tenant lifecycle leave it unset.
    sp_purge_facade: OnceLock<Arc<ServicePrincipalFacade>>,
}

impl TenantFacade {
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "DI surface: four typed metric-port views are part of the segregated-ports refactor; a builder would just trade arg count for indirection"
    )]
    pub fn new(
        cfg: TenantFacadeConfig,
        realm_admin_cfg: RealmAdminConfig,
        kc_factory: Arc<KeycloakAdminClientFactory>,
        codec: Arc<MetadataCodec>,
        cs_writer: CredStoreWriter,
        tenant_metrics: Arc<dyn TenantLifecycleMetricsPort>,
        credstore_metrics: Arc<dyn CredstoreMetricsPort>,
        metadata_metrics: Arc<dyn MetadataCodecMetricsPort>,
        failure_metrics: Arc<dyn FailureMetricsPort>,
    ) -> Self {
        Self {
            cfg,
            realm_admin_cfg,
            kc_factory,
            codec,
            cs_writer,
            tenant_metrics,
            credstore_metrics,
            metadata_metrics,
            failure_metrics,
            sp_purge_facade: OnceLock::new(),
        }
    }

    /// Install the tenant-deprovision service-principal purge hook.
    ///
    /// The facade is created after `TenantFacade` in module wiring, so this is
    /// a one-shot setter instead of a constructor argument. Repeated calls are
    /// ignored to keep test fixtures simple and avoid panics in init code.
    pub fn set_service_principal_purge_facade(&self, facade: Arc<ServicePrincipalFacade>) {
        let _ = self.sp_purge_facade.set(facade);
    }

    /// Internal facade method: returns the plugin's [`TenantIdpMetadataV1`] on
    /// success, [`PluginError`] on failure. The boundary at `idp_impl.rs`
    /// (Phase 15) translates this into the typed SDK
    /// `IdpProvisionFailure` variants.
    ///
    /// Wraps the saga body in `tokio::time::timeout` keyed on
    /// `cfg.provision_timeout_ms` (DESIGN §4.2). A timeout returns
    /// [`PluginError::AmbiguousCreated`] with stage
    /// [`AmbiguousStage::Timeout`] because the saga may have partially
    /// applied side-effects inside Keycloak or `OpenBao`.
    ///
    /// # Errors
    ///
    /// * [`PluginError::Config`] for malformed `provisioning_metadata` input.
    /// * [`PluginError::KcRest`] for transport / non-2xx KC responses.
    /// * [`PluginError::CredStoreRead`] if the realm-admin credential cannot
    ///   be resolved from `OpenBao` (Adopted / Created modes).
    /// * [`PluginError::AmbiguousCreated`] with stage [`AmbiguousStage::Timeout`]
    ///   when the saga exceeds `provision_timeout_ms`.
    pub async fn provision_tenant_inner(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
    ) -> Result<TenantIdpMetadataV1, PluginError> {
        let timeout = std::time::Duration::from_millis(self.cfg.provision_timeout_ms);
        match tokio::time::timeout(timeout, self.provision_tenant_saga(ctx, req)).await {
            Ok(result) => result,
            Err(_elapsed) => {
                // Saga was cancelled — its in-body `failure_metrics` /
                // audit emissions never ran. Emit the failure counter
                // here so dashboards see the timeout in
                // `keycloak_idp_plugin_failure_total{op=provision_tenant,
                // failure_variant=ambiguous_created}` alongside the
                // structured error returned to AM.
                let err = PluginError::AmbiguousCreated {
                    stage: AmbiguousStage::Timeout,
                    detail: format!(
                        "provision_tenant timed out after {}ms (DESIGN §4.2 provision_timeout_ms)",
                        self.cfg.provision_timeout_ms
                    ),
                };
                self.failure_metrics
                    .failure(PluginOp::ProvisionTenant, FailureVariant::from(&err));
                Err(err)
            }
        }
    }

    /// Saga body for [`Self::provision_tenant_inner`]. Separated so the outer
    /// wrapper can apply `provision_timeout_ms` (DESIGN §4.2).
    async fn provision_tenant_saga(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
    ) -> Result<TenantIdpMetadataV1, PluginError> {
        let started = std::time::Instant::now();
        // Decode the parsed metadata first so we can label the latency
        // histogram by `realm_binding` regardless of outcome. Malformed
        // input → bare `Config` failure with no histogram sample (the call
        // never made it to a binding-specific path).
        //
        // Order rationale: parse-then-probe, NOT probe-then-parse.
        // A malformed-input fast-fail must not pay the cost of a KC
        // round-trip — an operator with a typo'd `provisioning_metadata`
        // gets an immediate clear `Config` rather than racing the
        // probe's transport timeout. Once parse succeeds we know the
        // bind is shaped legitimately and only then is it worth
        // confirming KC is reachable.
        let parsed = match Self::parse_input(req) {
            Ok(p) => p,
            Err(e) => {
                self.failure_metrics
                    .failure(PluginOp::ProvisionTenant, FailureVariant::from(&e));
                return Err(e);
            }
        };
        // Pre-saga `health_probe` (DESIGN §4.4) — a flat-out KC outage
        // surfaces as `CleanFailure` here so the binding-specific saga
        // never starts. We re-tag the underlying `KcRest{Transport|Timeout|
        // Http(5xx)}` (which Phase 15 would map to `Ambiguous`) as
        // `Config` because the probe is the FIRST KC touch; nothing has
        // been mutated yet, so "Ambiguous" would understate our
        // knowledge and produce unnecessary AM-reaper work.
        if let Err(e) = self.kc_factory.health_probe().await {
            let err = PluginError::Config {
                detail: format!("pre-saga KC health probe failed: {e}; refusing to start saga"),
            };
            self.failure_metrics
                .failure(PluginOp::ProvisionTenant, FailureVariant::from(&err));
            return Err(err);
        }
        let result = match parsed.realm_binding {
            RealmBinding::Shared => self.provision_shared(ctx, req, &parsed).await,
            RealmBinding::Adopted => self.provision_adopted(req, &parsed).await,
            RealmBinding::Created => self.provision_created(req, &parsed).await,
        };
        let elapsed = started.elapsed().as_secs_f64();
        self.tenant_metrics
            .provision_tenant_duration(parsed.realm_binding, elapsed);
        match &result {
            Ok(meta) => {
                // Cardinality watchdog lives inside the adapter — the call
                // site just passes the realm name as `&str` and the adapter
                // decides whether to attach the label.
                self.tenant_metrics
                    .realm_bound(meta.realm_binding, &meta.realm_name);
                audit::emit_tenant_bound(
                    ctx,
                    req.tenant_id,
                    &meta.realm_name,
                    meta.realm_binding,
                    meta.admin_secret_ref.as_deref(),
                );
                if meta.realm_binding == RealmBinding::Created {
                    audit::emit_realm_created(ctx, req.tenant_id, &meta.realm_name);
                }
            }
            Err(e) => {
                // The decode-failure mirror that lived here was
                // unreachable: provision goes through `parse_input`
                // (which yields `PluginError::Config`, never
                // `PluginError::MetadataDecode`), so the dedicated
                // `metadata_decode_failure` counter only fires from
                // the deprovision path. Removed to stop a future
                // refactor from being misled into thinking provision
                // emits the counter. The invariant is now load-bearing
                // — assert it in debug builds so a refactor that
                // legitimately surfaces a decode error here trips a
                // loud failure instead of silently bypassing the
                // dedicated counter.
                debug_assert!(
                    !matches!(e, PluginError::MetadataDecode(_)),
                    "provision_tenant_saga is not expected to surface MetadataDecode; \
                     if this changes, wire metadata_metrics.metadata_decode_failure here \
                     symmetric with the deprovision path"
                );
                self.failure_metrics
                    .failure(PluginOp::ProvisionTenant, FailureVariant::from(e));
            }
        }
        result
    }

    /// Internal facade method for tenant deprovisioning. Decodes the
    /// persisted [`TenantIdpMetadataV1`] from `req.tenant_context.metadata`,
    /// deletes the per-tenant group, and for [`RealmBinding::Created`]
    /// additionally tears down the realm + realm-admin secret when the
    /// parent tenant group becomes empty (DESIGN §4.2, §5.5, §6.2).
    ///
    /// **Missing-metadata branch** (DESIGN §4.2 / §6.2 / §9): when
    /// `req.tenant_context.metadata == None`, we cannot self-classify
    /// «orphan from prior Ambiguous» vs «normal already-cleaned cascade»
    /// — return [`PluginError::DeprovisionNotFound`] (success-equivalent
    /// at the AM boundary), and on `am.system` ctx increments the
    /// `keycloak_idp_plugin_deprovision_missing_metadata_total` counter
    /// (DESIGN §9 trigger metric; wired to `OTel`).
    ///
    /// # Errors
    ///
    /// * [`PluginError::DeprovisionNotFound`] — vendor 404/410 or
    ///   `metadata = None` (success-equivalent in AM's pipeline).
    /// * [`PluginError::DeprovisionRetryable`] — transient 5xx / transport /
    ///   timeout failures on any saga step.
    /// * [`PluginError::DeprovisionTerminal`] — malformed persisted metadata
    ///   or other operator-must-intervene state.
    /// * [`PluginError::BootstrapPermsMissing`] — propagated unchanged so
    ///   the Phase 15 boundary can translate it.
    pub async fn deprovision_tenant_inner(
        &self,
        ctx: &SecurityContext,
        req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), PluginError> {
        // Wrap the saga in the same per-call timeout as provision. A hung
        // KC call (token endpoint, group DELETE, descendant count) would
        // otherwise hang the AM caller indefinitely; per DESIGN §4.2 the
        // upper bound is the operator-tunable `provision_timeout_ms`
        // (used here for symmetry — deprovision has the same saga
        // surface area as provision).
        let timeout = std::time::Duration::from_millis(self.cfg.provision_timeout_ms);
        match tokio::time::timeout(timeout, self.deprovision_tenant_saga(ctx, req)).await {
            Ok(result) => result,
            Err(_elapsed) => {
                // Saga cancelled — its in-body `failure_metrics`/audit
                // emissions never ran. Emit the failure counter here so
                // dashboards see the timeout symmetric with the
                // provision-timeout arm above.
                let err = PluginError::DeprovisionRetryable {
                    detail: format!(
                        "deprovision_tenant timed out after {}ms (DESIGN §4.2 provision_timeout_ms)",
                        self.cfg.provision_timeout_ms
                    ),
                };
                self.failure_metrics
                    .failure(PluginOp::DeprovisionTenant, FailureVariant::from(&err));
                Err(err)
            }
        }
    }

    /// Saga body for [`Self::deprovision_tenant_inner`]. Separated so the
    /// outer wrapper can apply the per-call timeout (DESIGN §4.2).
    async fn deprovision_tenant_saga(
        &self,
        ctx: &SecurityContext,
        req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), PluginError> {
        // Order rationale: missing-metadata + decode branches BEFORE
        // `health_probe`. Both pre-decode paths return without touching
        // KC at all (success-equivalent `DeprovisionNotFound` or
        // operator-intervention `DeprovisionTerminal`), so probing KC
        // first would (a) waste a round-trip on inputs the plugin can
        // resolve locally and (b) mask a malformed-metadata `Terminal`
        // behind a `Retryable` when KC also happens to be down. The
        // probe runs once we know the saga will actually need KC.
        // ---- Missing-metadata branch (DESIGN §4.2 / §6.2). ----
        let Some(blob) = req.tenant_context.metadata.as_ref() else {
            if ctx.subject_type() == Some("am.system") {
                // Increment the DESIGN §9 trigger metric; the audit event
                // below fires on every missing-metadata branch regardless of
                // caller's subject_type so operators see the actor + tenant id.
                self.tenant_metrics.deprovision_missing_metadata();
            }
            // Symmetric with the `Ok(None)` decode branch + the
            // `DeprovisionNotFound` post-decode arm: every path that
            // returns DeprovisionNotFound increments the generic failure
            // counter so dashboards don't under-count the missing-metadata
            // case.
            self.failure_metrics.failure(
                PluginOp::DeprovisionTenant,
                FailureVariant::DEPROVISION_NOT_FOUND,
            );
            audit::emit_tenant_unbound(
                ctx,
                req.tenant_context.tenant_id,
                "",
                None,
                /* already_absent = */ true,
                /* removed_realm = */ false,
            );
            return Err(PluginError::DeprovisionNotFound {
                realm_name: "unknown".into(),
            });
        };

        // ---- Decode persisted metadata. ----
        let metadata = match self.codec.decode(Some(blob.clone())) {
            Ok(Some(m)) => m,
            // Decoded `None` is unreachable when input was `Some(_)`; treat
            // as missing metadata for safety (no `tracing::warn!` here — we
            // already know the input WAS present, just decoded empty).
            Ok(None) => {
                self.failure_metrics.failure(
                    PluginOp::DeprovisionTenant,
                    FailureVariant::DEPROVISION_NOT_FOUND,
                );
                return Err(PluginError::DeprovisionNotFound {
                    realm_name: "unknown".into(),
                });
            }
            Err(e) => {
                self.metadata_metrics
                    .metadata_decode_failure(VersionObserved::from(&e));
                self.failure_metrics.failure(
                    PluginOp::DeprovisionTenant,
                    FailureVariant::DEPROVISION_TERMINAL,
                );
                return Err(PluginError::DeprovisionTerminal {
                    detail: format!("metadata decode failed: {e}"),
                });
            }
        };

        // Pre-saga `health_probe` (DESIGN §4.4) — runs AFTER decode so
        // local-only resolutions (missing-metadata, decode-terminal) get
        // their own clear failure type instead of being masked as
        // `Retryable`. Beyond this point the saga is guaranteed to need
        // KC, so a probe failure is unambiguously the right time to
        // bail. The downstream `DeprovisionRetryable` carrier maps to
        // `IdpDeprovisionFailure::Retryable` at the Phase 15 boundary.
        if let Err(e) = self.kc_factory.health_probe().await {
            let err = PluginError::DeprovisionRetryable {
                detail: format!("pre-saga KC health probe failed: {e}; retry once KC is back"),
            };
            self.failure_metrics
                .failure(PluginOp::DeprovisionTenant, FailureVariant::from(&err));
            return Err(err);
        }

        if let Some(sp_facade) = self.sp_purge_facade.get() {
            match sp_facade
                .purge_tenant_principals(req.tenant_context.tenant_id)
                .await
            {
                Ok(deleted_count) => {
                    if deleted_count > 0 {
                        audit::emit_service_principals_purged(
                            ctx,
                            req.tenant_context.tenant_id,
                            deleted_count,
                        );
                    }
                }
                Err(e) => {
                    let err = deprovision_translate_sp_purge(e);
                    self.failure_metrics
                        .failure(PluginOp::DeprovisionTenant, FailureVariant::from(&err));
                    return Err(err);
                }
            }
        }

        // Delegate the post-decode saga to a helper that returns whether
        // the realm itself was torn down (Created last-tenant), so the
        // outer wrapper can attribute audit + metric increments
        // consistently regardless of which step failed.
        match self.deprovision_after_decode(&metadata).await {
            Ok(removed_realm) => {
                audit::emit_tenant_unbound(
                    ctx,
                    req.tenant_context.tenant_id,
                    &metadata.realm_name,
                    Some(metadata.realm_binding),
                    /* already_absent = */ false,
                    removed_realm,
                );
                // Decrement the `realms_bound` UpDownCounter on EVERY
                // successful deprovision, symmetric with the `+1` that
                // fires on every successful provision (see
                // `provision_tenant_saga` Ok arm). The previous
                // implementation only decremented on Created last-tenant
                // teardown, which monotonically drifted the gauge upward
                // for Shared/Adopted lifecycles. Cardinality watchdog
                // lives inside the adapter.
                self.tenant_metrics
                    .realm_unbound(metadata.realm_binding, &metadata.realm_name);
                if removed_realm {
                    audit::emit_realm_removed(
                        ctx,
                        req.tenant_context.tenant_id,
                        &metadata.realm_name,
                    );
                }
                Ok(())
            }
            Err(e) => {
                if matches!(e, PluginError::DeprovisionNotFound { .. }) {
                    // 404/410 is success-equivalent at the AM boundary; emit
                    // an `already_absent` audit event so the operator log
                    // mirrors the Phase 15 boundary's success translation.
                    audit::emit_tenant_unbound(
                        ctx,
                        req.tenant_context.tenant_id,
                        &metadata.realm_name,
                        Some(metadata.realm_binding),
                        /* already_absent = */ true,
                        /* removed_realm = */ false,
                    );
                }
                self.failure_metrics
                    .failure(PluginOp::DeprovisionTenant, FailureVariant::from(&e));
                Err(e)
            }
        }
    }

    /// Post-decode portion of [`Self::deprovision_tenant_inner`]: resolves
    /// the secret source, deletes the tenant group, and for
    /// [`RealmBinding::Created`] additionally tears down the realm + the
    /// `OpenBao`-backed realm-admin secret when the parent tenant group
    /// becomes empty. Returns `Ok(removed_realm)` where `removed_realm`
    /// is `true` iff this call `DELETE`d the realm (Created last-tenant
    /// teardown).
    async fn deprovision_after_decode(
        &self,
        metadata: &TenantIdpMetadataV1,
    ) -> Result<bool, PluginError> {
        // ---- Resolve realm-admin secret source. ----
        // Shared blobs are persisted with `admin_secret_ref = None`
        // (ADDENDUM §3.4 — Shared uses the env-backed inline secret
        // resolved at module init via toolkit's `ExpandVars`); Adopted /
        // Created always carry a templated OpenBao `SecretRef` on the
        // metadata blob.
        let secret_source = if let Some(ref_str) = metadata.admin_secret_ref.as_ref() {
            let secret_ref =
                SecretRef::new(ref_str).map_err(|e| PluginError::DeprovisionTerminal {
                    detail: format!("admin_secret_ref invalid: {e}"),
                })?;
            SecretSource::OpenBaoRef(secret_ref)
        } else {
            SecretSource::Inline(
                self.realm_admin_cfg
                    .default_shared_realm_secret
                    .clone_into_secret_string(),
            )
        };

        // ---- DELETE tenant group via realm-admin (reactive-401). ----
        //
        // The wrapper acquires the realm-admin client, runs the DELETE,
        // and retries on a 401 from KC's auth layer so an operator-
        // rotated realm-admin secret converges without restart
        // (DESIGN §8.1 rotation lifecycle). `deprovision_translate_kc`
        // applied at the outer `.await` translates any `KcRest` that
        // leaks from acquire-stage failures OR a second-401 escape
        // into `DeprovisionRetryable`; closure-returned variants
        // (`DeprovisionNotFound`, `DeprovisionRetryable`) pass through
        // unchanged.
        let group_url = format!(
            "{}admin/realms/{}/groups/{}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(&metadata.realm_name),
            metadata.tenant_group_id,
        );
        self.kc_factory
            .with_reactive_401(
                &metadata.realm_name,
                &metadata.admin_client_id,
                &secret_source,
                |realm_admin| {
                    // Closure must be `Fn`-callable across the
                    // wrapper's retry, so owned String captures clone
                    // on each invocation instead of being moved once.
                    let group_url = group_url.clone();
                    async move {
                        let (status, body) = Self::kc_delete_status(
                            &realm_admin,
                            &group_url,
                            "/admin/realms/{realm}/groups/{tenant_group_id}",
                        )
                        .await?;
                        match status {
                            200..=299 => Ok(()),
                            // Surface 401 as KcRest so the wrapper triggers
                            // its single retry. `kc_delete_status` returns
                            // 401 as `Ok((401, body))` — we re-tag here.
                            401 => Err(PluginError::KcRest {
                                method: "DELETE",
                                path_template:
                                    "/admin/realms/{realm}/groups/{tenant_group_id}".into(),
                                status: KcStatusKind::Http(401),
                                body_first_2kb: body,
                            }),
                            404 | 410 => Err(PluginError::DeprovisionNotFound {
                                realm_name: metadata.realm_name.clone(),
                            }),
                            other => Err(PluginError::DeprovisionRetryable {
                                detail: format!(
                                    "kc:DELETE /admin/realms/{{realm}}/groups/{{tenant_group_id}} -> {other}: {body}",
                                ),
                            }),
                        }
                    }
                },
            )
            .await
            .map_err(deprovision_translate_kc)?;

        // ---- Created-only: parent emptiness check + realm/secret cleanup. ----
        if metadata.realm_binding != RealmBinding::Created {
            return Ok(false);
        }

        let parent_name = self
            .cfg
            .tenant_group_root
            .trim_start_matches('/')
            .to_owned();

        // Bootstrap-side probe (find_top_group + has_subgroup) —
        // both reads, both idempotent, so wrapping the pair under a
        // single reactive-401 closure is safe under exactly-once
        // retry. `BootstrapPermsMissing` propagates so Phase 15
        // translates verbatim (DESIGN §5.2 line 432); other KcRest
        // hits get the `deprovision_translate_kc` mapping.
        let has_remaining_tenants = match self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let parent_name = parent_name.clone();
                async move {
                    match self
                        .find_top_group(&bootstrap, &metadata.realm_name, &parent_name)
                        .await?
                    {
                        Some(parent) => {
                            self.has_subgroup(&bootstrap, &metadata.realm_name, parent.id)
                                .await
                        }
                        None => Ok(false),
                    }
                }
            })
            .await
        {
            Ok(b) => b,
            Err(PluginError::BootstrapPermsMissing { realm }) => {
                return Err(PluginError::BootstrapPermsMissing { realm });
            }
            Err(other) => return Err(deprovision_translate_kc(other)),
        };

        if has_remaining_tenants {
            // Realm still has live tenants — keep it (DESIGN §4.2 / §5.4).
            return Ok(false);
        }

        // ---- Last tenant under a Created realm — tear down. ----
        // Wrap the realm DELETE in reactive-401 too: bootstrap-admin
        // secret rotation could fire 401 here. The DELETE is a
        // one-shot non-idempotent op, but per the reactive-401
        // contract a 401 means "auth rejected, no side effect" → one
        // retry is safe.
        let realm_url = format!(
            "{}admin/realms/{}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(&metadata.realm_name),
        );
        self.kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let realm_url = realm_url.clone();
                async move {
                    let (status, body) =
                        Self::kc_delete_status(&bootstrap, &realm_url, "/admin/realms/{realm}")
                            .await?;
                    match status {
                        200..=299 | 404 | 410 => Ok(()),
                        401 => Err(PluginError::KcRest {
                            method: "DELETE",
                            path_template: "/admin/realms/{realm}".into(),
                            status: KcStatusKind::Http(401),
                            body_first_2kb: body,
                        }),
                        other => Err(PluginError::DeprovisionRetryable {
                            detail: format!("kc:DELETE /admin/realms/{{realm}} -> {other}: {body}"),
                        }),
                    }
                }
            })
            .await
            .map_err(deprovision_translate_kc)?;

        // Delete the OpenBao-backed realm-admin secret. Created binding
        // always carries `admin_secret_ref` (the non-default-shared branch
        // above already validated this — but we re-check here instead of
        // using `expect()` so a corrupt blob downgrades to a Terminal
        // failure rather than panicking the worker).
        let secret_ref_str =
            metadata
                .admin_secret_ref
                .as_ref()
                .ok_or_else(|| PluginError::DeprovisionTerminal {
                    detail: "metadata.admin_secret_ref is None on Created binding".into(),
                })?;
        let secret_ref =
            SecretRef::new(secret_ref_str).map_err(|e| PluginError::DeprovisionTerminal {
                detail: format!("admin_secret_ref invalid: {e}"),
            })?;
        let delete_result = self.cs_writer.delete(&secret_ref).await;
        match &delete_result {
            Ok(()) => {
                self.credstore_metrics
                    .credstore_write(CredstoreOp::Delete, CredstoreOutcome::Ok);
            }
            Err(_) => {
                self.credstore_metrics
                    .credstore_write(CredstoreOp::Delete, CredstoreOutcome::Error);
            }
        }
        delete_result.map_err(|e| PluginError::DeprovisionRetryable {
            detail: format!("openbao delete failed: {e}"),
        })?;

        Ok(true)
    }

    /// DELETE-with-status helper. Returns `(status, body_first_2kb)` on
    /// success so the caller can both branch on the status code and embed
    /// the body in any [`PluginError::DeprovisionRetryable`] detail
    /// (operator runbooks key on the body text — DESIGN §6.2).
    /// Transport / timeout failures collapse to
    /// [`PluginError::DeprovisionRetryable`] via [`deprovision_translate_kc`].
    /// On 2xx/4xx/5xx the body is read non-fatally (errors yield `""`).
    async fn kc_delete_status(
        client: &KcAdminClient,
        url: &str,
        path_template: &str,
    ) -> Result<(u16, String), PluginError> {
        let (status, body) = client
            .raw_request("DELETE", url, None, path_template)
            .await
            .map_err(deprovision_translate_kc)?;
        Ok((status, redact_secrets(&truncate_2kb(&body))))
    }

    /// Decode `provisioning_metadata` and derive the EFFECTIVE realm from the
    /// provisioning `target` + mode. Pre-KC caller-input rejections
    /// surface as [`PluginError::ProvisionInputRejected`] (mapped to a clean
    /// 400) rather than the broad [`PluginError::Config`].
    ///
    /// Effective-realm matrix:
    /// * Root                     → explicit `realm_name`, required (unchanged).
    /// * Child + shared/no mode   → parent realm decoded from
    ///   `parent_context.metadata`; explicit `realm_name` ignored; missing /
    ///   undecodable parent metadata → reject.
    /// * Child + created          → `realm-<tenant_id>`; explicit `realm_name`
    ///   → reject.
    /// * Child + adopted          → explicit `realm_name`, required (unchanged).
    fn parse_input(req: &IdpProvisionTenantRequest) -> Result<ParsedInput, PluginError> {
        let raw = req.metadata.as_ref();
        // Wire shape (optional now). Absent metadata ⇒ empty wire (mode None).
        let wire: WireMetadata = match raw {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
                PluginError::ProvisionInputRejected {
                    detail: format!("provisioning_metadata malformed: {e}"),
                }
            })?,
            None => WireMetadata {
                mode: None,
                realm_name: None,
                admin_user_id: None,
            },
        };
        // Mode: default shared.
        let mode = wire.mode.as_deref().unwrap_or("shared");
        let realm_binding = match mode {
            "shared" => RealmBinding::Shared,
            "adopted" => RealmBinding::Adopted,
            "created" => RealmBinding::Created,
            other => {
                return Err(PluginError::ProvisionInputRejected {
                    detail: format!("provisioning_metadata.mode not allowed: {other}"),
                });
            }
        };
        // admin_user_id is only honoured in Shared mode (DESIGN-bumpable).
        // Silently dropping it in Adopted / Created would mask an operator
        // misconfiguration — reject so AM surfaces a clean 400 instead of
        // provisioning a half-bound tenant.
        if wire.admin_user_id.is_some() && realm_binding != RealmBinding::Shared {
            return Err(PluginError::ProvisionInputRejected {
                detail: format!(
                    "provisioning_metadata.admin_user_id is only supported in mode=shared (got mode={mode})"
                ),
            });
        }
        // Effective realm by target + mode.
        let realm_name = match (&req.target, realm_binding) {
            // Root: explicit realm_name required (unchanged).
            (IdpProvisionTarget::Root, _) => {
                wire.realm_name
                    .clone()
                    .ok_or_else(|| PluginError::ProvisionInputRejected {
                        detail: "root provisioning requires provisioning_metadata.realm_name"
                            .into(),
                    })?
            }
            // Child + shared (or mode absent): inherit parent realm; explicit ignored.
            (IdpProvisionTarget::Child { .. }, RealmBinding::Shared) => {
                let parent_blob = req.parent_context.as_ref().and_then(|c| c.metadata.clone());
                let decoded = MetadataCodec.decode(parent_blob).map_err(|e| {
                    PluginError::ProvisionInputRejected {
                        detail: format!("cannot decode parent idp_metadata: {e}"),
                    }
                })?;
                decoded.map(|m| m.realm_name).ok_or_else(|| {
                    PluginError::ProvisionInputRejected {
                        detail:
                            "shared mode but parent has no idp_metadata to inherit a realm from"
                                .into(),
                    }
                })?
            }
            // Child + created: generate; explicit realm_name is a mistake.
            (IdpProvisionTarget::Child { .. }, RealmBinding::Created) => {
                if wire.realm_name.is_some() {
                    return Err(PluginError::ProvisionInputRejected {
                        detail: "mode=created generates its own realm; do not supply realm_name"
                            .into(),
                    });
                }
                format!("realm-{}", req.tenant_id)
            }
            // Child + adopted: explicit realm_name required (unchanged).
            (IdpProvisionTarget::Child { .. }, RealmBinding::Adopted) => wire
                .realm_name
                .clone()
                .ok_or_else(|| PluginError::ProvisionInputRejected {
                    detail: "mode=adopted requires provisioning_metadata.realm_name".into(),
                })?,
            // `IdpProvisionTarget` is `#[non_exhaustive]`; an unknown topology
            // is a contract we don't yet support — reject as caller-input.
            (_, _) => {
                return Err(PluginError::ProvisionInputRejected {
                    detail: "unsupported provisioning target topology".into(),
                });
            }
        };
        // Defence-in-depth: validate the effective realm name shape uniformly
        // (DESIGN §5.2 — applies to inherited / generated / explicit alike).
        if !realm_name_re().is_match(&realm_name) {
            return Err(PluginError::ProvisionInputRejected {
                detail: format!(
                    "realm_name rejected: must match ^[A-Za-z0-9_-]{{1,200}}$ (got {}-char value)",
                    realm_name.chars().count()
                ),
            });
        }
        Ok(ParsedInput {
            realm_binding,
            realm_name,
            admin_user_id: wire.admin_user_id,
        })
    }

    async fn provision_shared(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
        parsed: &ParsedInput,
    ) -> Result<TenantIdpMetadataV1, PluginError> {
        tracing::debug!(
            target: "keycloak_idp.provision",
            tenant_id = %req.tenant_id,
            realm = %parsed.realm_name,
            "provision_shared: step 1 — bootstrap_client + assert_realm_exists (reactive-401)"
        );
        // Step 1: bootstrap admin verifies the realm exists. Wrapped in
        // reactive-401 so a rotated `bootstrap_admin.client_secret`
        // converges without operator-driven plugin restart (DESIGN §8.1).
        self.kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.assert_realm_exists(&bootstrap, &parsed.realm_name)
                    .await
            })
            .await?;

        tracing::debug!(
            target: "keycloak_idp.provision",
            tenant_id = %req.tenant_id,
            "provision_shared: steps 2-4 — realm_client + ensure_tenant_group [+ bind_admin_user_to_tenant] (reactive-401)"
        );
        // Step 2: realm-admin always uses the env-backed inline secret in
        // Shared mode (ADDENDUM §3.4 / §4.1 — Shared is the single
        // env-backed path; Adopted / Created go through OpenBao).
        let realm_admin_source = SecretSource::Inline(
            self.realm_admin_cfg
                .default_shared_realm_secret
                .clone_into_secret_string(),
        );
        // Steps 3 (+ optional 4) — wrapped together so a 401 mid-block
        // re-runs the idempotent `ensure_tenant_group` + (if requested)
        // `bind_admin_user_to_tenant`. Both are safe under
        // exactly-once retry: `ensure_tenant_group` is find-or-create,
        // and `bind_admin_user_to_tenant` is GET-then-PUT against a
        // user UUID whose pre-bind state already mismatched on the
        // first attempt (otherwise we would have succeeded). The audit
        // event fires AFTER the wrapper returns so it never doubles up
        // on a 401-then-success path.
        let tenant_group_id = self
            .kc_factory
            .with_reactive_401(
                &parsed.realm_name,
                &self.realm_admin_cfg.client_id_default,
                &realm_admin_source,
                |realm_admin| async move {
                    let gid = self
                        .ensure_tenant_group(&realm_admin, &parsed.realm_name, req.tenant_id)
                        .await?;
                    if let Some(admin_user_id) = parsed.admin_user_id {
                        self.bind_admin_user_to_tenant(
                            &realm_admin,
                            &parsed.realm_name,
                            admin_user_id,
                            req.tenant_id,
                        )
                        .await?;
                    }
                    Ok(gid)
                },
            )
            .await?;

        if let Some(admin_user_id) = parsed.admin_user_id {
            audit::emit_admin_user_bound(ctx, req.tenant_id, &parsed.realm_name, admin_user_id);
        }

        // Step 5: assemble metadata. Shared binding always persists
        // `admin_secret_ref = None` (ADDENDUM §3.4 — the env-backed admin
        // is plugin-config-resolved, not OpenBao-resolved).
        Ok(TenantIdpMetadataV1 {
            version: "v1".into(),
            realm_name: parsed.realm_name.clone(),
            realm_binding: RealmBinding::Shared,
            tenant_group_id,
            admin_secret_ref: None,
            admin_client_id: self.realm_admin_cfg.client_id_default.clone(),
        })
    }

    /// Adopted-mode provisioning (DESIGN §4.1 Adopted row + §5.2 validation +
    /// §5.4 "Group emptiness check"). The caller supplies an arbitrary realm
    /// name (NOT the default shared realm). The bootstrap admin probes that
    /// (a) the realm exists, (b) the parent tenant group is empty — meaning
    /// either absent, or present with zero subgroups (per §5.4: "absent
    /// parent is success-equivalent"). Per-realm admin creds live in `OpenBao`
    /// under the configured `secret_ref_template` ⊕ `realm_name`, so
    /// `admin_secret_ref` is always `Some(<resolved template>)`.
    async fn provision_adopted(
        &self,
        req: &IdpProvisionTenantRequest,
        parsed: &ParsedInput,
    ) -> Result<TenantIdpMetadataV1, PluginError> {
        // Steps 1 + 2: bootstrap admin verifies the realm exists AND
        // probes parent tenant group emptiness (DESIGN §5.4 / §8.1
        // Phase 0 — bootstrap has view-realm + query-groups granted
        // per-target-realm for adopted flows). Absent parent group is
        // success-equivalent. Both ops share the bootstrap client, so
        // they share the reactive-401 wrapper — a single 401 retries
        // the assertion and the group probe together.
        let parent_name = self
            .cfg
            .tenant_group_root
            .trim_start_matches('/')
            .to_owned();
        self.kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let parent_name = parent_name.clone();
                async move {
                    self.assert_realm_exists(&bootstrap, &parsed.realm_name)
                        .await?;
                    if let Some(parent) = self
                        .find_top_group(&bootstrap, &parsed.realm_name, &parent_name)
                        .await?
                        && self
                            .has_subgroup(&bootstrap, &parsed.realm_name, parent.id)
                            .await?
                    {
                        {
                            // Non-empty parent → CleanFailure-equivalent.
                            // Synthesise `Http(409)` so the Phase 15
                            // boundary's 4xx-→-CleanFailure translation
                            // table catches it without a new variant.
                            return Err(PluginError::KcRest {
                                method: "GET",
                                path_template:
                                    "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                                status: KcStatusKind::Http(409),
                                body_first_2kb: format!(
                                    "adopted realm '{}' is not empty: parent group '{}' has at least one subgroup",
                                    parsed.realm_name, parent_name,
                                ),
                            });
                        }
                    }
                    Ok(())
                }
            })
            .await?;

        // Step 3: realm-admin (OpenBao-backed for Adopted — operator
        // pre-provisions the per-realm secret at `secret_ref_template` ⊕
        // `realm_name`).
        let secret_ref_str = self
            .realm_admin_cfg
            .secret_ref_template
            .replace("{realm_name}", &parsed.realm_name);
        let secret_ref = SecretRef::new(&secret_ref_str).map_err(|e| PluginError::Config {
            detail: format!("templated SecretRef invalid: {e}"),
        })?;
        let realm_admin_source = SecretSource::OpenBaoRef(secret_ref);

        // Step 4: ensure parent + child group (idempotent; `ensure_tenant_group`
        // creates the parent if missing, then the per-tenant child).
        // Wrapped in reactive-401 — an operator who rotated the
        // OpenBao-backed realm-admin secret converges without restart.
        let tenant_group_id = self
            .kc_factory
            .with_reactive_401(
                &parsed.realm_name,
                &self.realm_admin_cfg.client_id_default,
                &realm_admin_source,
                |realm_admin| async move {
                    self.ensure_tenant_group(&realm_admin, &parsed.realm_name, req.tenant_id)
                        .await
                },
            )
            .await?;

        // NOTE: `parsed.admin_user_id` is intentionally not acted upon in
        // Adopted mode. v1: bind only in Shared mode (DESIGN bump required
        // to expand — see §A scope note).
        Ok(TenantIdpMetadataV1 {
            version: "v1".into(),
            realm_name: parsed.realm_name.clone(),
            realm_binding: RealmBinding::Adopted,
            tenant_group_id,
            admin_secret_ref: Some(secret_ref_str),
            admin_client_id: self.realm_admin_cfg.client_id_default.clone(),
        })
    }

    /// Created-mode provisioning (DESIGN §4.2 Created row, §5.4 KC admin ops,
    /// §8.1 Phase 2 saga). The plugin owns the realm lifecycle — it
    /// (1) probes that the realm DOES NOT already exist (validation; if it
    /// does, that is a [`PluginError::CreatedRealmExists`] which Phase 15
    /// maps to `CleanFailure`), (2) creates the realm via the bootstrap
    /// admin, (3) creates the per-realm admin client + grants its service
    /// account the minimum per-realm `realm-management` roles, (4) reads
    /// back the generated `client_secret`, (5) persists it to `OpenBao` under
    /// the templated `SecretRef`, (6) finally uses the realm-admin to
    /// ensure the per-tenant group.
    ///
    /// **Ambiguous failure tagging** (DESIGN §5.5 line 482): every step
    /// after the validation probe is tagged with the corresponding
    /// [`AmbiguousStage`] so the operator reconciliation runbook (§12 risk
    /// #9) can prioritise cleanup. Step 5 is the saga's "point of no return"
    /// — KC has irreversible state, `OpenBao` does not, so a failure there
    /// surfaces specifically as
    /// [`AmbiguousStage::OpenBaoPutAfterKcSuccess`].
    async fn provision_created(
        &self,
        req: &IdpProvisionTenantRequest,
        parsed: &ParsedInput,
    ) -> Result<TenantIdpMetadataV1, PluginError> {
        // Steps 3-5 are wrapped INDIVIDUALLY in `with_reactive_401_bootstrap`
        // (NOT as a single block) because they form a non-idempotent
        // destructive saga: create-client → grant-roles → read-secret. A
        // 401 mid-block under a block-level wrapper would re-run earlier
        // steps with the fresh token. Per-call wrapping keeps each step's
        // retry narrowly scoped to the call whose token was rejected.
        //
        // Steps 1-2: idempotent realm ensure (probe → create → reconcile).
        // DESIGN §5.2, hardened for create-path idempotency: a
        // `mode = created` against an existing realm that is OURS (same
        // tenant marker) is adopted idempotently; a foreign realm is a
        // CleanFailure; and a 409 / lost-response on our own create
        // reconciles to success instead of a false `AmbiguousCreated`. The
        // probe / create / re-probe calls are each `with_reactive_401_bootstrap`
        // wrapped inside `ensure_realm`.
        self.ensure_realm(req, parsed).await?;

        // Force-refresh the bootstrap token before any per-new-realm work.
        // Keycloak auto-grants the creator `realm-admin` on the freshly-created
        // realm, but access tokens carry a point-in-time snapshot of role
        // claims — the bootstrap token in flight was minted BEFORE the realm
        // existed and does not include the new mapping. Without this
        // invalidate, step 3's `POST /admin/realms/{realm}/clients` returns
        // 403, which the reactive-401 wrapper does not refresh on, surfacing
        // as `AmbiguousCreated { stage: KcClientCreate }` and bricking the
        // saga. See `KeycloakAdminClientFactory::invalidate_bootstrap` for
        // the full rationale.
        self.kc_factory.invalidate_bootstrap();

        // Step 3: bootstrap admin creates the realm-admin client inside the
        // freshly-created realm.
        let admin_client_uuid = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.create_realm_admin_client(&bootstrap, &parsed.realm_name)
                    .await
            })
            .await?;

        // Step 4: grant the realm-admin client's service account the minimum
        // per-realm `realm-management` roles (DESIGN §8.1 least-privilege
        // table for the per-realm admin).
        self.kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.grant_realm_admin_roles(&bootstrap, &parsed.realm_name, admin_client_uuid)
                    .await
            })
            .await?;

        // Step 5: read back the generated `client_secret` so it can be
        // persisted to OpenBao for subsequent realm-admin token exchanges.
        let client_secret = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.read_realm_admin_client_secret(
                    &bootstrap,
                    &parsed.realm_name,
                    admin_client_uuid,
                )
                .await
            })
            .await?;

        // Step 6: persist the secret under the templated SecretRef. Anything
        // after this is "KC fully succeeded but OpenBao path may or may not
        // have landed" — see AmbiguousStage::OpenBaoPutAfterKcSuccess for
        // the reconciliation contract (§12 risk #9).
        let secret_ref_str = self
            .realm_admin_cfg
            .secret_ref_template
            .replace("{realm_name}", &parsed.realm_name);
        let secret_ref = SecretRef::new(&secret_ref_str).map_err(|e| PluginError::Config {
            detail: format!("templated SecretRef invalid: {e}"),
        })?;
        let put_result = self
            .cs_writer
            .put(
                &secret_ref,
                SecretValue::from(client_secret.as_str()),
                SharingMode::Tenant,
            )
            .await;
        match &put_result {
            Ok(()) => {
                self.credstore_metrics
                    .credstore_write(CredstoreOp::Put, CredstoreOutcome::Ok);
            }
            Err(_) => {
                self.credstore_metrics
                    .credstore_write(CredstoreOp::Put, CredstoreOutcome::Error);
            }
        }
        put_result.map_err(|e| PluginError::AmbiguousCreated {
            stage: AmbiguousStage::OpenBaoPutAfterKcSuccess,
            detail: format!("credstore: {e}"),
        })?;

        // Step 7: switch to the realm-admin (now that its secret is in
        // OpenBao) and ensure the per-tenant group structure exists.
        // Wrapped in reactive-401 — the OpenBao-backed secret may
        // have been rotated by an operator since the most recent
        // realm-admin token was cached.
        let realm_admin_source = SecretSource::OpenBaoRef(secret_ref);
        let tenant_group_id = self
            .kc_factory
            .with_reactive_401(
                &parsed.realm_name,
                &self.realm_admin_cfg.client_id_default,
                &realm_admin_source,
                |realm_admin| async move {
                    self.ensure_tenant_group(&realm_admin, &parsed.realm_name, req.tenant_id)
                        .await
                },
            )
            .await?;

        // NOTE: `parsed.admin_user_id` is intentionally not acted upon in
        // Created mode. v1: bind only in Shared mode (DESIGN bump required
        // to expand — see §A scope note).
        Ok(TenantIdpMetadataV1 {
            version: "v1".into(),
            realm_name: parsed.realm_name.clone(),
            realm_binding: RealmBinding::Created,
            tenant_group_id,
            admin_secret_ref: Some(secret_ref_str),
            admin_client_id: self.realm_admin_cfg.client_id_default.clone(),
        })
    }

    /// Idempotent realm-create step (DESIGN §5.2, hardened per / run
    /// am-functional-22). Folds the existence probe and the create into one
    /// reconciling step so a retried or re-entrant create is safe:
    ///
    /// * probe `OwnedByUs` → adopt (a prior attempt for THIS tenant already
    ///   created the realm) → `Ok`;
    /// * probe `Foreign` (or unmarked) → [`PluginError::CreatedRealmExists`]
    ///   (clean failure — never touch a realm we don't own);
    /// * probe `Absent` → `POST /admin/realms`. On success → `Ok`. On ANY
    ///   failure (409 unique-name from a duplicate POST, 5xx, transport /
    ///   timeout) re-probe: if the realm now exists and is ours the create
    ///   landed despite the error → idempotent `Ok`; if foreign →
    ///   `CreatedRealmExists`; if still absent / unknowable → preserve the
    ///   original `AmbiguousCreated { stage: KcRealmCreate }` so the operator
    ///   reconciliation runbook (§12 risk #9) still fires.
    ///
    /// Each KC call is wrapped in `with_reactive_401_bootstrap` so an
    /// operator-rotated bootstrap secret is picked up on a single 401.
    async fn ensure_realm(
        &self,
        req: &IdpProvisionTenantRequest,
        parsed: &ParsedInput,
    ) -> Result<(), PluginError> {
        let owner = req.tenant_id;
        let realm_name = parsed.realm_name.as_str();

        // Step 1: existence + ownership probe.
        let probe = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.probe_realm(&bootstrap, realm_name, owner).await
            })
            .await?;
        match probe {
            RealmProbe::OwnedByUs => return Ok(()),
            RealmProbe::Foreign => {
                return Err(PluginError::CreatedRealmExists {
                    realm: realm_name.to_owned(),
                });
            }
            RealmProbe::Absent => {}
        }

        // Step 2: create. On success we're done.
        let create = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.create_realm(&bootstrap, realm_name, req).await
            })
            .await;
        let Err(create_err) = create else {
            return Ok(());
        };

        // Reconcile: a duplicate POST (409) or a lost create response may
        // have landed the realm anyway. Re-probe to decide rather than
        // surfacing a false Ambiguous.
        match self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.probe_realm(&bootstrap, realm_name, owner).await
            })
            .await
        {
            Ok(RealmProbe::OwnedByUs) => Ok(()),
            Ok(RealmProbe::Foreign) => Err(PluginError::CreatedRealmExists {
                realm: realm_name.to_owned(),
            }),
            // Genuinely absent, or we couldn't confirm — keep the original
            // ambiguous classification (its detail carries the POST
            // status/body for the reconciliation runbook).
            Ok(RealmProbe::Absent) | Err(_) => Err(create_err),
        }
    }

    /// Probe whether `realm_name` exists and who owns it, per the
    /// [`PROVISIONING_TENANT_ID_ATTR`] marker. `GET /admin/realms/{realm}`:
    /// 404 → [`RealmProbe::Absent`]; 2xx → [`RealmProbe::OwnedByUs`] when the
    /// marker matches `owner_tenant_id`, else [`RealmProbe::Foreign`]; 403 →
    /// [`PluginError::BootstrapPermsMissing`]; any other non-2xx → a bare
    /// [`PluginError::KcRest`] (transient infra failure, NO side effect yet,
    /// so NOT tagged Ambiguous).
    async fn probe_realm(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        owner_tenant_id: Uuid,
    ) -> Result<RealmProbe, PluginError> {
        let probe_url = format!(
            "{}admin/realms/{}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
        );
        let (status, body) = bootstrap
            .raw_request("GET", &probe_url, None, "/admin/realms/{realm}")
            .await?;
        if status == 404 {
            Ok(RealmProbe::Absent)
        } else if (200..=299).contains(&status) {
            match realm_owner_tenant_id(&body) {
                Some(owner) if owner == owner_tenant_id => Ok(RealmProbe::OwnedByUs),
                _ => Ok(RealmProbe::Foreign),
            }
        } else if status == 403 {
            Err(PluginError::BootstrapPermsMissing {
                realm: realm_name.into(),
            })
        } else {
            Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            })
        }
    }

    /// Bootstrap admin creates the realm via
    /// `POST /admin/realms` with the canonical payload merged on top of
    /// `cfg.realm_defaults` (any operator-supplied keys win unless they
    /// collide with `realm`/`enabled`/`displayName`, which the plugin sets
    /// last to keep semantics deterministic).
    ///
    /// **Allowlist**: operator-supplied `realm_defaults` may carry only a
    /// narrow set of cosmetic / locale keys. The KC `RealmRepresentation`
    /// schema has ~150 top-level fields and most are security-sensitive
    /// (auth flows, federation, credential routing, security baseline
    /// knobs). A denylist would force us to chase every new KC release
    /// for fields we didn't know about; the operator-tunable surface at
    /// realm-create time is genuinely small, so we fail-closed instead.
    ///
    /// Currently allowed:
    /// - `displayNameHtml` — branding HTML
    /// - `defaultLocale` / `supportedLocales` /
    ///   `internationalizationEnabled` — i18n
    ///
    /// The plugin always sets `realm`, `enabled`, `displayName` itself —
    /// these are not operator-tunable. Anything else requires a
    /// security-reviewed PR to extend the allowlist; if you find
    /// yourself needing more, document the use case in the PR
    /// description and apply security review per
    /// `guidelines/SECURITY.md`.
    ///
    /// Any non-2xx → ambiguous failure tagged
    /// [`AmbiguousStage::KcRealmCreate`].
    async fn create_realm(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        req: &IdpProvisionTenantRequest,
    ) -> Result<(), PluginError> {
        const ALLOWLIST: &[&str] = &[
            "displayNameHtml",
            "defaultLocale",
            "supportedLocales",
            "internationalizationEnabled",
        ];

        let url = format!("{}admin/realms", self.kc_factory.base_url_str());
        // Start with the operator-supplied defaults (typed as
        // `serde_json::Value`; we accept any JSON shape and the canonical
        // keys below override on collision).
        let mut body = self.cfg.realm_defaults.clone();
        if !body.is_object() {
            body = serde_json::Value::Object(serde_json::Map::new());
        }
        if let Some(map) = body.as_object_mut() {
            // Allowlist enforcement: reject any operator-supplied key
            // not on the explicit allowlist BEFORE the plugin overrides
            // the canonical ones, so a misconfiguration surfaces as a
            // clean Config error at provision time rather than
            // silently widening the realm's security surface.
            if let Some(bad_key) = map.keys().find(|k| !ALLOWLIST.contains(&k.as_str())) {
                return Err(PluginError::Config {
                    detail: format!(
                        "tenant_facade.realm_defaults contains non-allowlisted key `{bad_key}` (allowed: {ALLOWLIST:?}); operator-controlled realm-creation state is rejected fail-closed — extend the allowlist via security-reviewed PR if you need a new key"
                    ),
                });
            }
            map.insert("realm".into(), serde_json::Value::String(realm_name.into()));
            map.insert("enabled".into(), serde_json::Value::Bool(true));
            map.insert(
                "displayName".into(),
                serde_json::Value::String(req.name.clone()),
            );
            // Ownership marker: stamp the AM tenant that owns this
            // realm so a retried / re-entrant create recognises it as ours
            // (`ensure_realm` reconcile) and the reaper can GC orphans by
            // owner. Plugin-set, after the allowlist gate — not operator-tunable.
            let mut attributes = serde_json::Map::new();
            attributes.insert(
                PROVISIONING_TENANT_ID_ATTR.into(),
                serde_json::Value::String(req.tenant_id.to_string()),
            );
            map.insert("attributes".into(), serde_json::Value::Object(attributes));
        }
        let (status, body_text) = bootstrap
            .raw_request("POST", &url, Some(&body), "/admin/realms")
            .await
            .map_err(|e| {
                // raw_request only fails on transport — re-tag as Ambiguous so
                // operators see WHICH stage was in flight when the call broke.
                ambiguous_from_kc_error(AmbiguousStage::KcRealmCreate, e)
            })?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcRealmCreate,
                detail: format!(
                    "kc:POST /admin/realms -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        Ok(())
    }

    /// Create the per-realm admin client (`keycloak-idp-plugin-realm-admin`) as
    /// a confidential `client_credentials`-only client with a service
    /// account. Returns the client's Keycloak-assigned UUID, discovered via
    /// a follow-up `GET .../clients?clientId=...` (Keycloak returns no body
    /// on the create — only a `Location` header; the list call is more
    /// robust across KC versions and easier to mock in tests).
    async fn create_realm_admin_client(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
    ) -> Result<Uuid, PluginError> {
        let client_id = &self.realm_admin_cfg.client_id_default;
        let create_url = format!(
            "{}admin/realms/{}/clients",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
        );
        let payload = serde_json::json!({
            "clientId": client_id,
            "protocol": "openid-connect",
            "publicClient": false,
            "serviceAccountsEnabled": true,
            "standardFlowEnabled": false,
            "directAccessGrantsEnabled": false,
            "implicitFlowEnabled": false,
            "authorizationServicesEnabled": false,
            "clientAuthenticatorType": "client-secret",
        });
        let (status, body_text) = bootstrap
            .raw_request(
                "POST",
                &create_url,
                Some(&payload),
                "/admin/realms/{realm}/clients",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientCreate, e))?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientCreate,
                detail: format!(
                    "kc:POST /admin/realms/{{realm}}/clients -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        self.lookup_client_uuid(
            bootstrap,
            realm_name,
            client_id,
            AmbiguousStage::KcClientCreate,
        )
        .await
    }

    /// Find the realm-management client's UUID + the realm-admin SA user's
    /// UUID + the role objects to map, then POST the role-mapping. Any
    /// failure along this chain is tagged
    /// [`AmbiguousStage::KcRoleMapping`].
    async fn grant_realm_admin_roles(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        admin_client_uuid: Uuid,
    ) -> Result<(), PluginError> {
        // The two lookups are independent — fire them concurrently to save
        // one network round-trip on the Created-mode saga.
        let (sa_user_uuid, rm_client_uuid) = tokio::try_join!(
            self.get_service_account_user(bootstrap, realm_name, admin_client_uuid),
            self.lookup_client_uuid(
                bootstrap,
                realm_name,
                REALM_MANAGEMENT_CLIENT_ID,
                AmbiguousStage::KcRoleMapping,
            ),
        )?;
        let available_roles = self
            .list_available_realm_management_roles(
                bootstrap,
                realm_name,
                sa_user_uuid,
                rm_client_uuid,
            )
            .await?;
        // Select the subset matching `REALM_ADMIN_REQUIRED_ROLES`. If any
        // expected role is absent the realm-management client is malformed
        // (KC built-in shape changed?) — tag as kc_role_mapping so the
        // operator sees the discrepancy.
        let mut to_map: Vec<RoleRep> = Vec::with_capacity(REALM_ADMIN_REQUIRED_ROLES.len());
        for required in REALM_ADMIN_REQUIRED_ROLES {
            match available_roles.iter().find(|r| r.name == *required) {
                Some(r) => to_map.push(r.clone()),
                None => {
                    return Err(PluginError::AmbiguousCreated {
                        stage: AmbiguousStage::KcRoleMapping,
                        detail: format!(
                            "kc:GET /admin/realms/{{realm}}/users/{{user_uuid}}/role-mappings/clients/{{rm_uuid}}/available -> 200: required realm-management role '{required}' not in available set",
                        ),
                    });
                }
            }
        }
        let post_url = format!(
            "{}admin/realms/{}/users/{}/role-mappings/clients/{}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            sa_user_uuid,
            rm_client_uuid,
        );
        let body = serde_json::to_value(&to_map).map_err(|e| PluginError::AmbiguousCreated {
            stage: AmbiguousStage::KcRoleMapping,
            detail: format!("serialise role-mapping body: {e}"),
        })?;
        let (status, body_text) = bootstrap
            .raw_request(
                "POST",
                &post_url,
                Some(&body),
                "/admin/realms/{realm}/users/{user_uuid}/role-mappings/clients/{rm_uuid}",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcRoleMapping, e))?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcRoleMapping,
                detail: format!(
                    "kc:POST /admin/realms/{{realm}}/users/{{user_uuid}}/role-mappings/clients/{{rm_uuid}} -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        Ok(())
    }

    /// Fetch a client's Keycloak UUID by its `clientId` via
    /// `GET /admin/realms/{realm}/clients?clientId={clientId}`. The caller
    /// supplies the [`AmbiguousStage`] so failures attribute to whichever
    /// saga step triggered the lookup.
    async fn lookup_client_uuid(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        client_id: &str,
        stage: AmbiguousStage,
    ) -> Result<Uuid, PluginError> {
        let url = format!(
            "{}admin/realms/{}/clients?clientId={}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            urlencoding::encode(client_id),
        );
        let (status, body_text) = bootstrap
            .raw_request("GET", &url, None, "/admin/realms/{realm}/clients")
            .await
            .map_err(|e| ambiguous_from_kc_error(stage, e))?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/clients -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        let clients: Vec<ClientRep> =
            serde_json::from_str(&body_text).map_err(|e| PluginError::AmbiguousCreated {
                stage,
                detail: format!("kc:GET /admin/realms/{{realm}}/clients json decode: {e}"),
            })?;
        clients
            .into_iter()
            .next()
            .map(|c| c.id)
            .ok_or(PluginError::AmbiguousCreated {
                stage,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/clients -> 200: clientId '{client_id}' not found after create",
                ),
            })
    }

    /// `GET /admin/realms/{realm}/clients/{uuid}/service-account-user` →
    /// the service-account user's UUID. Failures are tagged
    /// [`AmbiguousStage::KcRoleMapping`] because they block the role-mapping
    /// step.
    async fn get_service_account_user(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        client_uuid: Uuid,
    ) -> Result<Uuid, PluginError> {
        let url = format!(
            "{}admin/realms/{}/clients/{}/service-account-user",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            client_uuid,
        );
        let (status, body_text) = bootstrap
            .raw_request(
                "GET",
                &url,
                None,
                "/admin/realms/{realm}/clients/{client_uuid}/service-account-user",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcRoleMapping, e))?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcRoleMapping,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/clients/{{client_uuid}}/service-account-user -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        let user: UserRep =
            serde_json::from_str(&body_text).map_err(|e| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcRoleMapping,
                detail: format!("service-account-user json decode: {e}"),
            })?;
        Ok(user.id)
    }

    /// Fetch the list of available `realm-management` roles for a user
    /// via `GET .../users/{u}/role-mappings/clients/{rm}/available`.
    async fn list_available_realm_management_roles(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        sa_user_uuid: Uuid,
        rm_client_uuid: Uuid,
    ) -> Result<Vec<RoleRep>, PluginError> {
        let url = format!(
            "{}admin/realms/{}/users/{}/role-mappings/clients/{}/available",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            sa_user_uuid,
            rm_client_uuid,
        );
        let (status, body_text) = bootstrap
            .raw_request(
                "GET",
                &url,
                None,
                "/admin/realms/{realm}/users/{user_uuid}/role-mappings/clients/{rm_uuid}/available",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcRoleMapping, e))?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcRoleMapping,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/users/{{user_uuid}}/role-mappings/clients/{{rm_uuid}}/available -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        serde_json::from_str::<Vec<RoleRep>>(&body_text).map_err(|e| {
            PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcRoleMapping,
                detail: format!("available roles json decode: {e}"),
            }
        })
    }

    /// `GET /admin/realms/{realm}/clients/{uuid}/client-secret` → the
    /// generated `client_secret` plaintext. Failures are tagged
    /// [`AmbiguousStage::KcClientSecretRead`] — these straddle the
    /// "KC-side success vs OpenBao-side write" boundary; from the operator
    /// runbook's point of view they look indistinguishable from a
    /// `KcRoleMapping` partial: KC has full state but no secret in `OpenBao`.
    async fn read_realm_admin_client_secret(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
        client_uuid: Uuid,
    ) -> Result<String, PluginError> {
        let url = format!(
            "{}admin/realms/{}/clients/{}/client-secret",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            client_uuid,
        );
        let (status, body_text) = bootstrap
            .raw_request(
                "GET",
                &url,
                None,
                "/admin/realms/{realm}/clients/{client_uuid}/client-secret",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientSecretRead, e))?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientSecretRead,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/clients/{{client_uuid}}/client-secret -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }
        let cred: CredentialRep =
            serde_json::from_str(&body_text).map_err(|e| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientSecretRead,
                detail: format!("client-secret json decode: {e}"),
            })?;
        Ok(cred.value)
    }

    /// Probe `/admin/realms/{realm}` with the bootstrap admin token; non-2xx
    /// surfaces as [`PluginError::KcRest`] (Phase 15 boundary maps this to
    /// `CleanFailure` per DESIGN §5.2 «`mode = shared` but realm does not
    /// exist»).
    ///
    /// **403 special case (DESIGN §5.2 line 432):** this helper is ALWAYS
    /// invoked with a bootstrap-admin client (Shared and Adopted both call it
    /// that way; the reactive-401 wrapper around its call sites doesn't
    /// change this — 403 is a permission issue, not a rotation signal). When
    /// the bootstrap admin is missing `view-realm` on the target realm, KC returns
    /// 403 on the probe; we surface a dedicated
    /// [`PluginError::BootstrapPermsMissing`] so Phase 15 can translate it to
    /// `IdpProvisionFailure::UnsupportedOperation` (rather than the default
    /// 4xx→`CleanFailure` mapping).
    async fn assert_realm_exists(
        &self,
        bootstrap: &KcAdminClient,
        realm_name: &str,
    ) -> Result<(), PluginError> {
        let probe_url = format!(
            "{}admin/realms/{}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
        );
        let (status, body) = bootstrap
            .raw_request("GET", &probe_url, None, "/admin/realms/{realm}")
            .await?;
        if (200..=299).contains(&status) {
            Ok(())
        } else if status == 403 {
            Err(PluginError::BootstrapPermsMissing {
                realm: realm_name.into(),
            })
        } else {
            Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            })
        }
    }

    /// Bind a pre-provisioned Keycloak admin user to the tenant by setting
    /// `tenant_id` and `user_type` attributes on the user.
    ///
    /// Implements GET-modify-PUT against Keycloak Admin REST. The mutation
    /// surface is narrow — only the two managed attribute KEYS
    /// (`attributes.tenant_id` and `attributes.user_type`) are touched.
    /// Every other field of the GET'd `UserRepresentation` is round-tripped
    /// into the PUT body verbatim.
    ///
    /// **Race window** (full-document last-writer-wins): KC's `/users/{id}`
    /// PUT has no optimistic concurrency (no `ETag`, no `version` enforcement).
    /// The PUT body is the FULL `UserRepresentation` we snapshotted at the
    /// GET step with only those two attribute keys mutated, so ANY concurrent
    /// edit to top-level fields (`firstName`, `email`, `enabled`, …) or to
    /// other attribute keys between our GET and our PUT is clobbered by the
    /// stale snapshot. v1 accepts this — the bind only runs once during
    /// tenant bootstrap, well before any operator-level concurrent edits to
    /// the admin user are plausible. The "narrowness" of the merge is about
    /// what THIS call deliberately mutates, not about what survives.
    ///
    /// # Errors
    ///
    /// ALL failure branches translate to [`PluginError::AmbiguousCreated`]
    /// tagged [`AmbiguousStage::AdminUserBind`]. The reason is the saga
    /// ordering: this method is `provision_shared` Step 4, called AFTER
    /// `ensure_tenant_group` (Step 3) has already created the per-tenant
    /// Keycloak group. Any error here therefore leaves a real side-effect
    /// (the tenant group) in KC — surfacing the failure as `CleanFailure`
    /// at the Phase-15 boundary would lie to AM ("nothing happened, safe
    /// to retry clean") and let the orphaned group grow unbounded on
    /// every retry. The Ambiguous tag triggers AM's reconciliation path,
    /// which can complete or unwind the half-built tenant. This explicitly
    /// includes the 404-on-GET case (admin user missing) and the
    /// hijack-guard case (`tenant_id` attribute mismatch) — both fire
    /// AFTER Step 3's side-effect has landed.
    async fn bind_admin_user_to_tenant(
        &self,
        realm_admin: &KcAdminClient,
        realm_name: &str,
        admin_user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), PluginError> {
        let user_url = format!(
            "{}admin/realms/{}/users/{}",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            admin_user_id,
        );

        // Step 1: GET current user representation.
        let (get_status, get_body) = realm_admin
            .raw_request(
                "GET",
                &user_url,
                None,
                "/admin/realms/{realm}/users/{user_uuid}",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::AdminUserBind, e))?;
        // Surface a 401 as bare `KcRest{Http(401)}` so the enclosing
        // `with_reactive_401` wrapper sees the rotation signal and
        // retries with a refreshed bearer. The 401 GET has no side
        // effect (auth-rejected before any KC mutation), so collapsing
        // it into `AmbiguousCreated` here would lock out the rotation
        // recovery path. Non-401 non-2xx responses STILL tag as
        // `AmbiguousCreated` per the saga-ordering invariant below.
        if get_status == 401 {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/users/{user_uuid}".into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&get_body)),
            });
        }
        if !(200..=299).contains(&get_status) {
            // ALL non-2xx non-401 GET responses (including 404) tag as
            // AmbiguousCreated: `ensure_tenant_group` (Step 3) already
            // created the per-tenant KC group, so even "user genuinely
            // missing" leaves a real side-effect that AM must reconcile.
            // Marking the 404 case as `CleanFailure` would let the
            // orphaned group accumulate one per retry indefinitely.
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::AdminUserBind,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/users/{{user_uuid}} -> {get_status}: {}",
                    redact_secrets(&truncate_2kb(&get_body)),
                ),
            });
        }

        // Step 2: parse + targeted attribute merge.
        // Modify ONLY the two managed keys; every other field of the
        // GET'd snapshot rides along (see the race-window note in the
        // doc-comment).
        //
        // Body-parse failures here are POST-side-effect: the tenant
        // group is already created at this point, so tagging them as
        // `Config` (which the Phase 15 boundary maps to `CleanFailure`
        // = "nothing happened, safe to retry clean") would lie to AM.
        // All three parse branches tag as `AmbiguousCreated{AdminUserBind}`
        // for symmetry with the 5xx/403-on-GET path above.
        let mut user: serde_json::Value =
            serde_json::from_str(&get_body).map_err(|e| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::AdminUserBind,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/users/{{user_uuid}} body parse failed: {e}"
                ),
            })?;
        let user_obj = user
            .as_object_mut()
            .ok_or_else(|| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::AdminUserBind,
                detail: "user representation is not a JSON object".into(),
            })?;
        let attrs = user_obj
            .entry("attributes".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let attrs_obj = attrs
            .as_object_mut()
            .ok_or_else(|| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::AdminUserBind,
                detail: "user.attributes is not a JSON object".into(),
            })?;

        // Hijack guard: refuse to overwrite an existing tenant_id whose
        // shape isn't already a singleton matching this tenant. Two
        // failure modes we close here:
        //
        //   (a) Foreign single-tenant bind — `attributes.tenant_id = ["X"]`
        //       (or bare `"X"` from a hand-edited record) with X != the
        //       requested tenant. An operator authorised to provision
        //       tenants who supplied a foreign user's UUID would
        //       silently transfer that user's attribute-based authz to
        //       the newly-created tenant.
        //   (b) Multi-valued overwrite — `attributes.tenant_id = ["A", "B"]`
        //       (the user is admin of A and B). The previous version
        //       passed the guard whenever the requested tenant_id was
        //       ANY element of the existing array, then unconditionally
        //       wrote `[tenant_id]`, silently dropping the user's other
        //       bindings. v1 has a single-tenant-per-user model (see
        //       `user_facade::provision_user`, design §nonGoals
        //       `move_user_tenant`), so a multi-valued attribute is by
        //       definition unexpected operator state — the plugin
        //       refuses rather than guess which bindings are safe to
        //       drop.
        //
        // Allowed shapes (idempotent re-bind during bootstrap re-run):
        //   * no `tenant_id` attribute at all (legitimate bootstrap), OR
        //   * `tenant_id == "T"` bare string AND T == requested, OR
        //   * `tenant_id == ["T"]` singleton array AND T == requested.
        //
        // Mismatched bind tags as `AmbiguousCreated{AdminUserBind}` —
        // NOT `Config` (CleanFailure) — because Step 3 already created
        // the tenant group. A `Config` rejection would tell AM "nothing
        // happened, safe to retry clean" and leak an orphaned KC group
        // per attempt. Ambiguous routes through AM's reconciliation path.
        if let Some(existing) = attrs_obj.get("tenant_id") {
            let target = tenant_id.to_string();
            let is_idempotent_rebind = match existing {
                serde_json::Value::String(s) => s.as_str() == target.as_str(),
                serde_json::Value::Array(values) => {
                    values.len() == 1 && values[0].as_str() == Some(target.as_str())
                }
                _ => false,
            };
            if !is_idempotent_rebind {
                // Distinguish hijack vs multi-valued displacement in the
                // detail so an operator triaging the AM-reconciliation
                // path can tell "I pasted the wrong UUID" apart from
                // "this user is unexpectedly cross-tenant" without
                // re-reading Keycloak. Substrings `hijack` and
                // `multi-valued` are stable for log-grep / tests.
                let classification = match existing {
                    serde_json::Value::Array(values) if values.len() > 1 => "multi-valued",
                    _ => "different tenant",
                };
                return Err(PluginError::AmbiguousCreated {
                    stage: AmbiguousStage::AdminUserBind,
                    detail: format!(
                        "admin_user_id {admin_user_id} is already bound to a {classification} (attributes.tenant_id = {existing}); refusing to overwrite — supplying a foreign user UUID would silently hijack that user's attribute-based authz, and writing [{target}] over a multi-valued attribute would silently drop the user's other tenant bindings"
                    ),
                });
            }
        }

        attrs_obj.insert(
            "tenant_id".to_string(),
            serde_json::json!([tenant_id.to_string()]),
        );
        attrs_obj.insert("user_type".to_string(), serde_json::json!(["user"]));

        // Step 3: PUT updated user representation back.
        let (put_status, put_body) = realm_admin
            .raw_request(
                "PUT",
                &user_url,
                Some(&user),
                "/admin/realms/{realm}/users/{user_uuid}",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::AdminUserBind, e))?;
        // Mirror the GET arm: surface 401 as `KcRest{Http(401)}` so the
        // outer reactive-401 wrapper can re-acquire and retry the
        // GET-modify-PUT cycle. KC's auth layer rejects the PUT before
        // applying the mutation, so this is a no-side-effect failure
        // that fits the wrapper's exactly-once-retry contract.
        if put_status == 401 {
            return Err(PluginError::KcRest {
                method: "PUT",
                path_template: "/admin/realms/{realm}/users/{user_uuid}".into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&put_body)),
            });
        }
        if !(200..=299).contains(&put_status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::AdminUserBind,
                detail: format!(
                    "kc:PUT /admin/realms/{{realm}}/users/{{user_uuid}} -> {put_status}: {}",
                    redact_secrets(&truncate_2kb(&put_body)),
                ),
            });
        }
        Ok(())
    }

    /// Resolve-or-create the parent tenant group and create the per-tenant
    /// child subgroup. Returns the new child group UUID (the value persisted
    /// as `TenantIdpMetadataV1.tenant_group_id`).
    async fn ensure_tenant_group(
        &self,
        realm_admin: &KcAdminClient,
        realm_name: &str,
        tenant_id: Uuid,
    ) -> Result<Uuid, PluginError> {
        // Strip any leading `/` so a config value like `/tenants` and `tenants`
        // both yield the same Keycloak group name.
        let parent_name = self.cfg.tenant_group_root.trim_start_matches('/');
        let parent_id = self
            .find_or_create_top_group(realm_admin, realm_name, parent_name)
            .await?;

        let create_url = format!(
            "{}admin/realms/{}/groups/{}/children",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            parent_id
        );
        let body = serde_json::json!({ "name": tenant_id.to_string() });
        let (status, body_text) = realm_admin
            .raw_request(
                "POST",
                &create_url,
                Some(&body),
                "/admin/realms/{realm}/groups/{parent_uuid}/children",
            )
            .await?;

        // Happy path: 2xx → parse the created GroupRepresentation and return its id.
        if (200..=299).contains(&status) {
            let g: GroupRep =
                serde_json::from_str(&body_text).map_err(|e| PluginError::KcRest {
                    method: "POST",
                    path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                    status: KcStatusKind::Http(status),
                    body_first_2kb: format!("group rep parse: {e}"),
                })?;
            return Ok(g.id);
        }

        // Idempotent path: a previous saga run (or any other actor) already
        // created the subgroup → Keycloak returns 409 with "Sibling group
        // named '<tenant_id>' already exists." Look the existing group up by
        // listing parent's children with the same `search=&exact=true` filter
        // used for top-level groups, and return its id. Without this branch
        // the saga loops forever on bootstrap restart after a crash between
        // group creation and AM-side persistence.
        if status == 409 {
            let lookup_url = format!(
                "{}admin/realms/{}/groups/{}/children?search={}&exact=true",
                self.kc_factory.base_url_str(),
                urlencoding::encode(realm_name),
                parent_id,
                urlencoding::encode(&tenant_id.to_string()),
            );
            let (lookup_status, lookup_body) = realm_admin
                .raw_request(
                    "GET",
                    &lookup_url,
                    None,
                    "/admin/realms/{realm}/groups/{parent_uuid}/children",
                )
                .await?;
            if !(200..=299).contains(&lookup_status) {
                return Err(PluginError::KcRest {
                    method: "GET",
                    path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                    status: KcStatusKind::Http(lookup_status),
                    body_first_2kb: redact_secrets(&truncate_2kb(&lookup_body)),
                });
            }
            let groups: Vec<GroupRep> =
                serde_json::from_str(&lookup_body).map_err(|e| PluginError::KcRest {
                    method: "GET",
                    path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                    status: KcStatusKind::Http(lookup_status),
                    body_first_2kb: format!("groups array parse: {e}"),
                })?;
            let expected = tenant_id.to_string();
            if let Some(existing) = groups.into_iter().find(|g| g.name == expected) {
                tracing::info!(
                    target: "keycloak_idp.provision",
                    tenant_id = %tenant_id,
                    group_id = %existing.id,
                    "ensure_tenant_group: subgroup already exists (409 idempotent path)"
                );
                return Ok(existing.id);
            }
            // 409 returned but no matching child found — surface as KcRest
            // with the original 409 body so operators see Keycloak's exact
            // message in metrics / logs.
            return Err(PluginError::KcRest {
                method: "POST",
                path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: format!(
                    "409 conflict but subgroup not found in parent listing: {}",
                    redact_secrets(&truncate_2kb(&body_text)),
                ),
            });
        }

        Err(PluginError::KcRest {
            method: "POST",
            path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
            status: KcStatusKind::Http(status),
            body_first_2kb: redact_secrets(&truncate_2kb(&body_text)),
        })
    }

    async fn find_or_create_top_group(
        &self,
        realm_admin: &KcAdminClient,
        realm_name: &str,
        group_name: &str,
    ) -> Result<Uuid, PluginError> {
        let search_url = format!(
            "{}admin/realms/{}/groups?search={}&exact=true",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            urlencoding::encode(group_name),
        );
        let (status, body_text) = realm_admin
            .raw_request("GET", &search_url, None, "/admin/realms/{realm}/groups")
            .await?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: redact_secrets(&truncate_2kb(&body_text)),
            });
        }

        let groups: Vec<GroupRep> =
            serde_json::from_str(&body_text).map_err(|e| PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: format!("groups list parse: {e}"),
            })?;
        if let Some(g) = groups.iter().find(|g| g.name == group_name) {
            return Ok(g.id);
        }

        // Parent group missing — create top-level group.
        //
        // Keycloak returns 201 with EMPTY body on `POST /admin/realms/{realm}/groups`
        // (the canonical group id lives in the `Location` header, not in the
        // response body — confirmed against KC 26.x). The realm-admin
        // `KcAdminClient::raw_request` surface does not expose response
        // headers, so we cannot read the Location here. Re-issue the same
        // exact-match search we ran at the top of this function to recover
        // the id deterministically; a 2xx on POST guarantees the group now
        // exists, so this re-search must succeed.
        //
        // Earlier implementations parsed `GroupRepresentation` from the POST
        // response body. That happened to work against KC versions that
        // returned a populated body, but produced an opaque
        // `EOF while parsing a value at line 1 column 0` failure against
        // KC 26.x (empty-body 201). The post-create re-search is
        // version-agnostic — it does not depend on the response body shape.
        let create_url = format!(
            "{}admin/realms/{}/groups",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
        );
        let body = serde_json::json!({ "name": group_name });
        let (status, body_text) = realm_admin
            .raw_request(
                "POST",
                &create_url,
                Some(&body),
                "/admin/realms/{realm}/groups",
            )
            .await?;
        if !(200..=299).contains(&status) {
            return Err(PluginError::KcRest {
                method: "POST",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: redact_secrets(&truncate_2kb(&body_text)),
            });
        }

        // Post-create re-search: the just-created group MUST be findable by
        // the same exact-match query that returned empty at the top of this
        // function. Failure here means KC returned 2xx on POST but the group
        // is not subsequently visible — operationally indistinguishable from
        // a KC bug; surface as a structured error so the operator sees it.
        let (lookup_status, lookup_body) = realm_admin
            .raw_request("GET", &search_url, None, "/admin/realms/{realm}/groups")
            .await?;
        if !(200..=299).contains(&lookup_status) {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(lookup_status),
                body_first_2kb: redact_secrets(&truncate_2kb(&lookup_body)),
            });
        }
        let post_create: Vec<GroupRep> =
            serde_json::from_str(&lookup_body).map_err(|e| PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(lookup_status),
                body_first_2kb: format!("groups list parse: {e}"),
            })?;
        post_create
            .into_iter()
            .find(|g| g.name == group_name)
            .map(|g| g.id)
            .ok_or_else(|| PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(lookup_status),
                body_first_2kb: format!(
                    "post-create lookup returned no entry named '{group_name}' \
                     after KC reported {status} on create"
                ),
            })
    }

    /// Find-only counterpart to [`Self::find_or_create_top_group`]: probes
    /// `GET /admin/realms/{realm}/groups?search={name}&exact=true` and
    /// returns `Some({id, ..})` if the parent group exists, `None` if
    /// the search returned `[]`. Used by Adopted-mode emptiness probe —
    /// "parent absent" is treated as success-equivalent (DESIGN §5.4).
    ///
    /// **403 special case (DESIGN §5.2 line 432):** the Adopted emptiness
    /// probe runs with the bootstrap admin, so a 403 here means the bootstrap
    /// admin lacks `query-groups` on the target realm. Surface
    /// [`PluginError::BootstrapPermsMissing`] so Phase 15 maps it to
    /// `IdpProvisionFailure::UnsupportedOperation` rather than the default
    /// 4xx→`CleanFailure` translation.
    async fn find_top_group(
        &self,
        admin: &KcAdminClient,
        realm_name: &str,
        group_name: &str,
    ) -> Result<Option<TopGroupInfo>, PluginError> {
        let search_url = format!(
            "{}admin/realms/{}/groups?search={}&exact=true",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            urlencoding::encode(group_name),
        );
        let (status, body_text) = admin
            .raw_request("GET", &search_url, None, "/admin/realms/{realm}/groups")
            .await?;
        if status == 403 {
            return Err(PluginError::BootstrapPermsMissing {
                realm: realm_name.into(),
            });
        }
        if !(200..=299).contains(&status) {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: redact_secrets(&truncate_2kb(&body_text)),
            });
        }
        let groups: Vec<GroupRep> =
            serde_json::from_str(&body_text).map_err(|e| PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: format!("groups list parse: {e}"),
            })?;
        Ok(groups
            .into_iter()
            .find(|g| g.name == group_name)
            .map(|g| TopGroupInfo { id: g.id }))
    }

    /// Probe whether a parent group has ANY subgroups via
    /// `GET /admin/realms/{realm}/groups/{parent_id}/children?first=0&max=1`.
    /// All callers only need a "zero vs non-zero" answer for the Adopted
    /// emptiness check / Created last-tenant teardown, so we ask Keycloak
    /// for at most one child and return a bool — `max=1` also keeps the
    /// response small and stays compatible across Keycloak 24/25/26 (the
    /// `subGroupCount` field on the parent's `GroupRepresentation` only
    /// landed in 26).
    ///
    /// **403 special case (DESIGN §5.2 line 432):** only the Adopted-mode
    /// emptiness probe calls this helper, and only with the bootstrap admin.
    /// A 403 therefore means the bootstrap admin lacks `query-groups` on the
    /// target realm; surface [`PluginError::BootstrapPermsMissing`] so Phase
    /// 15 translates to `IdpProvisionFailure::UnsupportedOperation` rather
    /// than the default 4xx→`CleanFailure` mapping.
    async fn has_subgroup(
        &self,
        admin: &KcAdminClient,
        realm_name: &str,
        parent_id: Uuid,
    ) -> Result<bool, PluginError> {
        let url = format!(
            "{}admin/realms/{}/groups/{}/children?first=0&max=1",
            self.kc_factory.base_url_str(),
            urlencoding::encode(realm_name),
            parent_id,
        );
        let (status, body_text) = admin
            .raw_request(
                "GET",
                &url,
                None,
                "/admin/realms/{realm}/groups/{parent_uuid}/children",
            )
            .await?;
        if status == 403 {
            return Err(PluginError::BootstrapPermsMissing {
                realm: realm_name.into(),
            });
        }
        if !(200..=299).contains(&status) {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: redact_secrets(&truncate_2kb(&body_text)),
            });
        }
        let children: Vec<GroupRep> =
            serde_json::from_str(&body_text).map_err(|e| PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/groups/{parent_uuid}/children".into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: format!("children list parse: {e}"),
            })?;
        Ok(!children.is_empty())
    }
}

/// Result of [`TenantFacade::find_top_group`] — the parent group's `id`. Kept
/// as a struct (rather than bare `Uuid`) so future Adopted-mode work can
/// extend it (e.g. with `subGroupCount` when KC 26+ is the baseline) without
/// rippling through call sites.
#[domain_model]
struct TopGroupInfo {
    id: Uuid,
}

#[cfg(test)]
#[path = "tenant_facade_tests.rs"]
mod tenant_facade_tests;
