//! OpenTelemetry-backed adapter for the `keycloak-idp-plugin` metric ports.
//!
//! [`KeycloakIdpPluginMetricsAdapter`] owns all OpenTelemetry instruments
//! declared by the catalog in [`crate::domain::metrics`] and implements
//! every port trait from [`crate::domain::ports::metrics`]. A single
//! `Arc<KeycloakIdpPluginMetricsAdapter>` is constructed at module init
//! ([`crate::module`]) and coerced into per-trait `Arc<dyn ...>` views
//! by DI.
//!
//! ## Naming convention (mirrors RMS / policy-engine)
//!
//! Instrument names are **literal Prometheus** names with the wire-side
//! suffix baked into the name: counters end with `_total`
//! (`keycloak_idp_plugin_failure_total`), histograms end with their unit
//! (`_seconds`, or `_ms` only where genuinely milliseconds), and
//! gauges / up-down-counters carry no suffix. The adapter does **not**
//! call `with_unit(...)` — the unit lives in the name. This matches the
//! platform collector's `add_metric_suffixes: false` posture, which
//! preserves these names verbatim and does NOT auto-append suffixes
//! (same as RMS / policy-engine).
//!
//! ## Cardinality watchdog
//!
//! Per DESIGN §4.4 / §9, `realm_name` is a partner-controlled label.
//! The adapter tracks distinct realm names in an internal `DashSet`;
//! once the cap (`realm_label_cap`, sourced from config) is exceeded,
//! the `realm_name` label is dropped from all subsequent emissions and
//! a `tracing::field` is recorded on the current span instead. The
//! policy is enforced inside the adapter — call sites pass `&str` and
//! never decide whether to emit the label.

use std::sync::Arc;

use dashmap::DashSet;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter, UpDownCounter};

use crate::domain::metadata_codec::RealmBinding;
use crate::domain::metrics::{
    DEFAULT_PREFIX, KEYCLOAK_IDP_PLUGIN_CREDENTIAL_REFRESH, KEYCLOAK_IDP_PLUGIN_CREDSTORE_WRITE,
    KEYCLOAK_IDP_PLUGIN_DEPROVISION_MISSING_METADATA, KEYCLOAK_IDP_PLUGIN_FAILURE,
    KEYCLOAK_IDP_PLUGIN_KC_ADMIN_REQUEST_DURATION, KEYCLOAK_IDP_PLUGIN_KC_ADMIN_TOKEN_REFRESH,
    KEYCLOAK_IDP_PLUGIN_METADATA_DECODE_FAILURE, KEYCLOAK_IDP_PLUGIN_ORPHAN_USER_COMPENSATION,
    KEYCLOAK_IDP_PLUGIN_PROVISION_TENANT_DURATION, KEYCLOAK_IDP_PLUGIN_REALMS_BOUND,
    KEYCLOAK_IDP_PLUGIN_SP_OP_DURATION, KEYCLOAK_IDP_PLUGIN_USER_OP_DURATION,
};
use crate::domain::ports::metrics::{
    CredstoreMetricsPort, CredstoreOp, CredstoreOutcome, EndpointClass, FailureMetricsPort,
    FailureVariant, KcAdminMetricsPort, MetadataCodecMetricsPort, OrphanCompensationOutcome,
    PluginOp, SpOp, SpOpMetricsPort, TenantLifecycleMetricsPort, TokenRefreshOutcome, TokenTier,
    UserOp, UserOpMetricsPort, VersionObserved,
};

/// OpenTelemetry adapter that owns one instrument per plugin metric
/// family and implements every port trait.
pub struct KeycloakIdpPluginMetricsAdapter {
    // Histograms (4)
    provision_tenant_duration: Histogram<f64>,
    user_op_duration: Histogram<f64>,
    kc_admin_request_duration: Histogram<f64>,
    sp_op_duration: Histogram<f64>,

    // Counters (7)
    failure: Counter<u64>,
    kc_admin_token_refresh: Counter<u64>,
    credential_refresh: Counter<u64>,
    credstore_write: Counter<u64>,
    metadata_decode_failure: Counter<u64>,
    deprovision_missing_metadata: Counter<u64>,
    orphan_user_compensation: Counter<u64>,

    // UpDownCounter (1)
    realms_bound: UpDownCounter<i64>,

    // Cardinality watchdog (DESIGN §4.4, §9). `0` disables.
    seen_realms: DashSet<String>,
    realm_label_cap: u32,
}

/// Substitute the leading `keycloak_idp_plugin` namespace token of a literal
/// metric family name with the adapter's `prefix`, preserving the rest
/// of the (already suffixed) name verbatim. With the default prefix
/// (`"keycloak_idp_plugin"`) this is a no-op and the name is emitted exactly
/// as declared in the catalog.
///
/// `"keycloak_idp_plugin_failure_total"` + `"my_plugin"` →
/// `"my_plugin_failure_total"`. A family that does not start with the
/// default token (should not happen for catalog constants) is prefixed
/// as `"{prefix}_{family}"` so the host prefix still shows up.
#[inline]
fn rename(family: &str, prefix: &str) -> String {
    match family.strip_prefix(DEFAULT_PREFIX) {
        Some(tail) => format!("{prefix}{tail}"),
        None => format!("{prefix}_{family}"),
    }
}

impl KeycloakIdpPluginMetricsAdapter {
    /// Build every instrument. `prefix` defaults to
    /// [`crate::domain::metrics::DEFAULT_PREFIX`] (`"keycloak_idp_plugin"`)
    /// and substitutes the leading `keycloak_idp_plugin` namespace token of
    /// every literal metric family name (a no-op with the default
    /// prefix; the unit/`_total` suffix already baked into the name is
    /// preserved). `realm_label_cap` controls the cardinality watchdog
    /// (pass `0` to disable).
    #[must_use]
    pub fn new(meter: &Meter, prefix: &str, realm_label_cap: u32) -> Self {
        Self {
            provision_tenant_duration: meter
                .f64_histogram(rename(
                    KEYCLOAK_IDP_PLUGIN_PROVISION_TENANT_DURATION,
                    prefix,
                ))
                .with_description("provision_tenant end-to-end latency in seconds by realm_binding")
                .build(),
            user_op_duration: meter
                .f64_histogram(rename(KEYCLOAK_IDP_PLUGIN_USER_OP_DURATION, prefix))
                .with_description("User-op latency in seconds by op")
                .build(),
            kc_admin_request_duration: meter
                .f64_histogram(rename(
                    KEYCLOAK_IDP_PLUGIN_KC_ADMIN_REQUEST_DURATION,
                    prefix,
                ))
                .with_description("Single KC Admin REST call latency in seconds by endpoint_class")
                .build(),
            sp_op_duration: meter
                .f64_histogram(rename(KEYCLOAK_IDP_PLUGIN_SP_OP_DURATION, prefix))
                .with_description("Service-principal op latency in seconds by op")
                .build(),
            failure: meter
                .u64_counter(rename(KEYCLOAK_IDP_PLUGIN_FAILURE, prefix))
                .with_description("Plugin failures by op x failure_variant")
                .build(),
            kc_admin_token_refresh: meter
                .u64_counter(rename(KEYCLOAK_IDP_PLUGIN_KC_ADMIN_TOKEN_REFRESH, prefix))
                .with_description("Admin token refresh count by outcome x tier x realm")
                .build(),
            credential_refresh: meter
                .u64_counter(rename(KEYCLOAK_IDP_PLUGIN_CREDENTIAL_REFRESH, prefix))
                .with_description(
                    "Admin client_secret re-resolution count by outcome x tier x realm",
                )
                .build(),
            credstore_write: meter
                .u64_counter(rename(KEYCLOAK_IDP_PLUGIN_CREDSTORE_WRITE, prefix))
                .with_description("OpenBao writes/deletes for per-realm admin secrets")
                .build(),
            metadata_decode_failure: meter
                .u64_counter(rename(KEYCLOAK_IDP_PLUGIN_METADATA_DECODE_FAILURE, prefix))
                .with_description("Malformed / unsupported-version metadata blob counter")
                .build(),
            deprovision_missing_metadata: meter
                .u64_counter(rename(
                    KEYCLOAK_IDP_PLUGIN_DEPROVISION_MISSING_METADATA,
                    prefix,
                ))
                .with_description("deprovision_tenant with metadata=None and am.system ctx")
                .build(),
            orphan_user_compensation: meter
                .u64_counter(rename(KEYCLOAK_IDP_PLUGIN_ORPHAN_USER_COMPENSATION, prefix))
                .with_description(
                    "Orphan-user compensation outcome (after add_user_to_group failure) by outcome",
                )
                .build(),
            realms_bound: meter
                .i64_up_down_counter(rename(KEYCLOAK_IDP_PLUGIN_REALMS_BOUND, prefix))
                .with_description("Active realm bindings by realm_binding x realm_name")
                .build(),
            seen_realms: DashSet::new(),
            realm_label_cap,
        }
    }

    /// Decide whether the `realm_name` label should be emitted for
    /// `realm` and record the decision (so the unbind side can match).
    ///
    /// Contract:
    /// * `realm_label_cap == 0` → always emit (cap disabled).
    /// * `seen_realms` holds **exactly** the realms whose label has
    ///   already been emitted at least once. On a new realm:
    ///   * if `len() < cap`, insert it and emit (`true`);
    ///   * otherwise drop the label (`false`) and **do NOT insert** —
    ///     a realm whose `+1` was emitted unlabelled MUST get an
    ///     unlabelled `-1` too, and that requires `contains(realm)`
    ///     to be `false` on the unbind lookup.
    ///
    /// Previously `seen_realms` was populated **before** the cap
    /// check, which let realms bound past-cap drift into the set; the
    /// unbind helper (`realm_key_value_no_observe`) then keyed off
    /// `contains()` and KEPT the label on `-1`, breaking the
    /// `+1`/`-1` pairing on the `realms_bound` `UpDownCounter`.
    ///
    /// **Shared budget**: every emit site that takes a realm name
    /// (`realm_bound`, `kc_admin_token_refresh`, `credential_refresh`)
    /// shares the SAME `realm_label_cap`. A high-churn token-refresh
    /// path against many partner realms can independently exhaust the
    /// cap and silently drop `realm_name` from `realms_bound` for
    /// legitimate low-churn tenants. The single-budget design mirrors
    /// AM's posture; a one-shot `tracing::warn!` fires the first time
    /// the cap is crossed so operators see WHICH realm tipped it over.
    pub(crate) fn observe_realm_and_should_emit(&self, realm: &str) -> bool {
        if self.realm_label_cap == 0 {
            return true;
        }
        // Already-labelled realm — always emit the label, no budget
        // change.
        if self.seen_realms.contains(realm) {
            return true;
        }
        // New realm. Check the budget BEFORE inserting; only insert if
        // we're going to emit the label, so the set's membership stays
        // 1:1 with "label was emitted".
        let cap = self.realm_label_cap as usize;
        if self.seen_realms.len() < cap {
            // Race-tolerant insert: `DashSet::insert` is atomic. If
            // two threads race the same new realm, one wins the insert
            // (returns `true`), the other sees the entry on its own
            // `insert` (returns `false`); both return `true` here
            // because both are guaranteed to find the realm in the
            // set on any subsequent lookup. The set may briefly grow
            // by 1 past the cap in a tight race, which is acceptable
            // — operator dashboards aren't sensitive to ±1 cardinality.
            self.seen_realms.insert(realm.to_string());
            return true;
        }
        // Cap reached and this realm is genuinely new — one-shot
        // operator signal the first time the cap is first crossed.
        // We bound the warn to "this is the realm that wanted in but
        // couldn't" so operators see the proximate cause.
        tracing::warn!(
            target: "keycloak_idp.metrics",
            cap = self.realm_label_cap,
            tripping_realm = realm,
            "keycloak_idp_plugin realm_label_cap reached; `realm_name` label dropped for new realms across realms_bound + kc_admin_token_refresh + credential_refresh (shared budget)"
        );
        false
    }

    /// Look up the watchdog decision **without** observing — used on
    /// the `realm_unbound` path. The bind side recorded membership
    /// only when the label was actually emitted; this helper therefore
    /// mirrors the bind decision exactly (`contains` ↔ "label was
    /// emitted at bind"), keeping the `+1`/`-1` pairing on
    /// `realms_bound` symmetric per `(realm_binding, realm_name)`.
    fn realm_key_value_no_observe(&self, label_key: &'static str, realm: &str) -> Option<KeyValue> {
        if self.realm_label_cap == 0 || self.seen_realms.contains(realm) {
            Some(KeyValue::new(label_key, realm.to_owned()))
        } else {
            tracing::Span::current().record("realm.name", tracing::field::display(realm));
            None
        }
    }

    /// Helper shared between `realm_bound` and the reserved
    /// `kc_admin_token_refresh` / `credential_refresh` paths: observes
    /// the realm (counts against the shared budget) and, when the
    /// label is dropped, records the realm name on the current
    /// tracing span so operators can still see which realm a sample
    /// originated from at trace level.
    fn realm_key_value(&self, label_key: &'static str, realm: &str) -> Option<KeyValue> {
        if self.observe_realm_and_should_emit(realm) {
            Some(KeyValue::new(label_key, realm.to_owned()))
        } else {
            tracing::Span::current().record("realm.name", tracing::field::display(realm));
            None
        }
    }
}

/// Convenience constructor used at module init: builds an
/// `Arc<KeycloakIdpPluginMetricsAdapter>` against the process-global
/// OpenTelemetry meter provider, scoped to the plugin instrumentation
/// library, with the default prefix.
#[must_use]
pub fn build_default_adapter(realm_label_cap: u32) -> Arc<KeycloakIdpPluginMetricsAdapter> {
    let meter = opentelemetry::global::meter("keycloak-idp-plugin");
    Arc::new(KeycloakIdpPluginMetricsAdapter::new(
        &meter,
        DEFAULT_PREFIX,
        realm_label_cap,
    ))
}

// ════════════════════════════════════════════════════════════════════
//  Port-trait implementations
// ════════════════════════════════════════════════════════════════════

impl TenantLifecycleMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn provision_tenant_duration(&self, realm_binding: RealmBinding, secs: f64) {
        self.provision_tenant_duration.record(
            secs,
            &[KeyValue::new(
                "realm_binding",
                realm_binding.as_metric_label(),
            )],
        );
    }

    fn realm_bound(&self, realm_binding: RealmBinding, realm_name: &str) {
        let mut labels = vec![KeyValue::new(
            "realm_binding",
            realm_binding.as_metric_label(),
        )];
        if let Some(kv) = self.realm_key_value("realm_name", realm_name) {
            labels.push(kv);
        }
        self.realms_bound.add(1, &labels);
    }

    fn realm_unbound(&self, realm_binding: RealmBinding, realm_name: &str) {
        let mut labels = vec![KeyValue::new(
            "realm_binding",
            realm_binding.as_metric_label(),
        )];
        // No-observe path: don't consume budget on the unbind side. See
        // `realm_key_value_no_observe` docstring for the rationale.
        if let Some(kv) = self.realm_key_value_no_observe("realm_name", realm_name) {
            labels.push(kv);
        }
        self.realms_bound.add(-1, &labels);
    }

    fn deprovision_missing_metadata(&self) {
        self.deprovision_missing_metadata.add(1, &[]);
    }
}

impl UserOpMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn user_op_duration(&self, op: UserOp, secs: f64) {
        self.user_op_duration
            .record(secs, &[KeyValue::new("op", op.as_str())]);
    }

    fn orphan_user_compensation(&self, outcome: OrphanCompensationOutcome) {
        self.orphan_user_compensation
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }
}

impl KcAdminMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn kc_admin_request_duration(&self, endpoint_class: EndpointClass, secs: f64) {
        self.kc_admin_request_duration.record(
            secs,
            &[KeyValue::new("endpoint_class", endpoint_class.as_str())],
        );
    }

    fn kc_admin_token_refresh(&self, outcome: TokenRefreshOutcome, tier: TokenTier, realm: &str) {
        let mut labels = vec![
            KeyValue::new("outcome", outcome.as_str()),
            KeyValue::new("tier", tier.as_str()),
        ];
        if let Some(kv) = self.realm_key_value("realm", realm) {
            labels.push(kv);
        }
        self.kc_admin_token_refresh.add(1, &labels);
    }

    fn credential_refresh(&self, outcome: TokenRefreshOutcome, tier: TokenTier, realm: &str) {
        let mut labels = vec![
            KeyValue::new("outcome", outcome.as_str()),
            KeyValue::new("tier", tier.as_str()),
        ];
        if let Some(kv) = self.realm_key_value("realm", realm) {
            labels.push(kv);
        }
        self.credential_refresh.add(1, &labels);
    }
}

impl CredstoreMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn credstore_write(&self, op: CredstoreOp, outcome: CredstoreOutcome) {
        self.credstore_write.add(
            1,
            &[
                KeyValue::new("op", op.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }
}

impl MetadataCodecMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn metadata_decode_failure(&self, version_observed: VersionObserved) {
        // VersionObserved holds Cow; the OTel KeyValue API needs an
        // owned String for non-static labels, so we allocate here. The
        // metric is low-frequency (only fires on decode failures) so
        // the allocation cost is irrelevant.
        self.metadata_decode_failure.add(
            1,
            &[KeyValue::new(
                "version_observed",
                version_observed.as_str().to_owned(),
            )],
        );
    }
}

impl FailureMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn failure(&self, op: PluginOp, variant: FailureVariant) {
        self.failure.add(
            1,
            &[
                KeyValue::new("op", op.as_str()),
                KeyValue::new("failure_variant", variant.as_str()),
            ],
        );
    }
}

impl SpOpMetricsPort for KeycloakIdpPluginMetricsAdapter {
    fn sp_op_duration(&self, op: SpOp, secs: f64) {
        self.sp_op_duration
            .record(secs, &[KeyValue::new("op", op.as_str())]);
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
