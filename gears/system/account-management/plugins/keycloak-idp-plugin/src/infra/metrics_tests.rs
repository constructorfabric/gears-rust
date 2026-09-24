use std::sync::Arc;

use super::*;
use crate::domain::error::PluginError;
use crate::domain::metadata_codec::{DecodeError, RealmBinding};
use crate::domain::ports::metrics::{
    CredstoreMetricsPort, CredstoreOp, CredstoreOutcome, EndpointClass, FailureMetricsPort,
    FailureVariant, KcAdminMetricsPort, MetadataCodecMetricsPort, PluginOp, SaOp, SaOpMetricsPort,
    TenantLifecycleMetricsPort, TokenRefreshOutcome, TokenTier, UserOp, UserOpMetricsPort,
    VersionObserved,
};

// ---- adapter construction smoke ----

#[test]
fn adapter_builds_against_global_meter() {
    // Must not panic; instruments construct against NoopMeterProvider
    // when the global meter provider has not been overridden.
    let _adapter = build_default_adapter(/*realm_label_cap=*/ 0);
}

#[test]
fn rename_helper_substitutes_namespace_token() {
    // Leading `keycloak_idp_plugin` token is swapped for the prefix; the
    // already-suffixed remainder (`_failure_total`) is preserved.
    assert_eq!(
        rename("keycloak_idp_plugin_failure_total", "my_plugin"),
        "my_plugin_failure_total"
    );
    assert_eq!(
        rename("keycloak_idp_plugin_provision_tenant_duration_seconds", "x"),
        "x_provision_tenant_duration_seconds"
    );
    // With the default prefix the substitution is a no-op.
    assert_eq!(
        rename("keycloak_idp_plugin_realms_bound", DEFAULT_PREFIX),
        "keycloak_idp_plugin_realms_bound"
    );
    // A family that does not start with the default token falls into the
    // "{prefix}_{family}" arm.
    assert_eq!(rename("nodot", "x"), "x_nodot");
}

// ---- cardinality watchdog ----

#[test]
fn realm_label_drops_once_cap_is_exceeded() {
    let adapter = build_default_adapter(/*cap=*/ 3);
    adapter.realm_bound(RealmBinding::Created, "realm-a");
    adapter.realm_bound(RealmBinding::Created, "realm-b");
    adapter.realm_bound(RealmBinding::Created, "realm-c");
    assert!(
        adapter.observe_realm_and_should_emit("realm-c"),
        "3rd distinct realm == cap, must still emit",
    );
    adapter.realm_bound(RealmBinding::Created, "realm-d");
    assert!(
        !adapter.observe_realm_and_should_emit("realm-d"),
        "cap exceeded at the 4th distinct realm, label must drop",
    );
}

#[test]
fn realm_label_cap_zero_never_drops() {
    let adapter = build_default_adapter(/*cap=*/ 0);
    for i in 0..1000 {
        adapter.realm_bound(RealmBinding::Created, &format!("realm-{i}"));
    }
    assert!(
        adapter.observe_realm_and_should_emit("realm-999"),
        "cap=0 disables the watchdog; must always emit",
    );
}

#[test]
fn realm_bound_unbound_label_symmetry_holds_past_cap() {
    // Regression guard for the round-2 watchdog refactor: a realm
    // bound *past* the cap (label dropped on `+1`) must also have its
    // `-1` emitted WITHOUT the label, so the `(realm_binding,
    // realm_name=Y)` series and the unlabelled `(realm_binding)`
    // series stay symmetric on the `realms_bound` UpDownCounter.
    //
    // The earlier implementation inserted into `seen_realms` BEFORE
    // the cap check, so the unbind `contains()` lookup kept the
    // label on `-1` even though `+1` went out unlabelled.
    let adapter = build_default_adapter(/*cap=*/ 2);

    // Fill the budget with two realms — both labelled.
    assert!(adapter.observe_realm_and_should_emit("realm-a"));
    assert!(adapter.observe_realm_and_should_emit("realm-b"));

    // A third realm is bound past-cap — the bind side returns false
    // (label dropped) and MUST NOT enter `seen_realms`. The unbind
    // side then mirrors that decision via `contains()`.
    assert!(
        !adapter.observe_realm_and_should_emit("realm-c"),
        "past-cap bind must NOT emit the realm_name label",
    );
    let unbind_kv = adapter.realm_key_value_no_observe("realm_name", "realm-c");
    assert!(
        unbind_kv.is_none(),
        "past-cap unbind must NOT emit the realm_name label (symmetric with bind); got {unbind_kv:?}"
    );

    // Sanity: a labelled realm bound within the cap still emits its
    // label on the unbind side.
    let labelled_kv = adapter.realm_key_value_no_observe("realm_name", "realm-a");
    assert!(
        labelled_kv.is_some(),
        "within-cap unbind must keep the realm_name label",
    );
}

#[test]
fn realm_label_repeated_observation_does_not_inflate_cardinality() {
    let adapter = build_default_adapter(/*cap=*/ 2);
    adapter.realm_bound(RealmBinding::Created, "realm-a");
    adapter.realm_bound(RealmBinding::Created, "realm-a"); // duplicate
    adapter.realm_bound(RealmBinding::Created, "realm-b");
    assert!(
        adapter.observe_realm_and_should_emit("realm-b"),
        "duplicates must not count against the cap",
    );
    adapter.realm_bound(RealmBinding::Created, "realm-c");
    assert!(
        !adapter.observe_realm_and_should_emit("realm-c"),
        "cap hits at the 3rd distinct realm",
    );
}

// ---- adapter implements every port ----

#[test]
fn adapter_implements_every_port() {
    let adapter = build_default_adapter(0);
    let _t: Arc<dyn TenantLifecycleMetricsPort> = Arc::clone(&adapter) as _;
    let _u: Arc<dyn UserOpMetricsPort> = Arc::clone(&adapter) as _;
    let _k: Arc<dyn KcAdminMetricsPort> = Arc::clone(&adapter) as _;
    let _c: Arc<dyn CredstoreMetricsPort> = Arc::clone(&adapter) as _;
    let _m: Arc<dyn MetadataCodecMetricsPort> = Arc::clone(&adapter) as _;
    let _f: Arc<dyn FailureMetricsPort> = Arc::clone(&adapter) as _;
    let _s: Arc<dyn SaOpMetricsPort> = Arc::clone(&adapter) as _;
}

// ---- end-to-end no-panic smoke covering every port method ----

#[test]
fn every_port_method_runs_against_noop_meter_without_panic() {
    let adapter = build_default_adapter(/*cap=*/ 100);

    adapter.provision_tenant_duration(RealmBinding::Shared, 0.42);
    adapter.realm_bound(RealmBinding::Created, "alpha");
    adapter.realm_unbound(RealmBinding::Created, "alpha");
    adapter.deprovision_missing_metadata();

    adapter.user_op_duration(UserOp::ProvisionUser, 0.1);
    adapter.user_op_duration(UserOp::DeprovisionUser, 0.2);
    adapter.user_op_duration(UserOp::ListUsers, 0.05);

    adapter.kc_admin_request_duration(EndpointClass::UNKNOWN, 0.01);
    adapter.kc_admin_token_refresh(TokenRefreshOutcome::Success, TokenTier::StaticEnv, "shared");
    adapter.credential_refresh(TokenRefreshOutcome::Error, TokenTier::OpenBao, "tenant-r");

    adapter.credstore_write(CredstoreOp::Put, CredstoreOutcome::Ok);
    adapter.credstore_write(CredstoreOp::Delete, CredstoreOutcome::Error);

    adapter.metadata_decode_failure(VersionObserved::MISSING);
    adapter.metadata_decode_failure(VersionObserved::from(&DecodeError::UnsupportedVersion {
        observed: "v9".into(),
    }));

    adapter.failure(PluginOp::ProvisionTenant, FailureVariant::CONFIG);
    adapter.failure(
        PluginOp::DeprovisionUser,
        FailureVariant::from(&PluginError::DeprovisionRetryable { detail: "x".into() }),
    );

    adapter.sa_op_duration(SaOp::Create, 0.1);
    adapter.sa_op_duration(SaOp::RotateSecret, 0.2);
    adapter.sa_op_duration(SaOp::Revoke, 0.05);
    adapter.sa_op_duration(SaOp::List, 0.03);
}

// ---- histogram bucket boundaries ----

#[test]
fn duration_histograms_use_second_scale_bucket_boundaries() {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use crate::domain::metrics::{
        KEYCLOAK_IDP_PLUGIN_KC_ADMIN_REQUEST_DURATION,
        KEYCLOAK_IDP_PLUGIN_PROVISION_TENANT_DURATION, KEYCLOAK_IDP_PLUGIN_SA_OP_DURATION,
        KEYCLOAK_IDP_PLUGIN_USER_OP_DURATION,
    };

    // Expected boundaries are spelled out as literals rather than read back from
    // the constants under test. Asserting a constant against itself only proves
    // `with_boundaries` was called at all, and would still pass if the values
    // regressed to the millisecond-scale SDK defaults this test exists to catch.
    let expected_op_bounds = vec![
        0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
    ];
    let expected_provision_bounds =
        vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 7.5, 10.0, 15.0, 30.0];

    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let adapter = KeycloakIdpPluginMetricsAdapter::new(
        &provider.meter("keycloak-idp-plugin"),
        DEFAULT_PREFIX,
        /*realm_label_cap=*/ 0,
    );

    // Record a sample so each histogram exports a data point.
    adapter.provision_tenant_duration(RealmBinding::Shared, 0.42);
    adapter.user_op_duration(UserOp::ProvisionUser, 0.01);
    adapter.kc_admin_request_duration(EndpointClass::UNKNOWN, 0.01);
    adapter.sa_op_duration(SaOp::Create, 0.01);
    provider.force_flush().expect("provider should flush");

    // One snapshot for every assertion below: `get_finished_metrics` deep-clones
    // the whole accumulated batch on each call, and the periodic reader can
    // append another batch between two reads.
    let snapshot = exporter
        .get_finished_metrics()
        .expect("in-memory exporter should be readable");

    let bounds = |name: &str| -> Vec<f64> {
        for resource_metrics in &snapshot {
            for scope_metrics in resource_metrics.scope_metrics() {
                for metric in scope_metrics.metrics() {
                    if metric.name() == name
                        && let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data()
                        && let Some(dp) = hist.data_points().next()
                    {
                        return dp.bounds().collect();
                    }
                }
            }
        }
        panic!("{name} should export a histogram data point");
    };

    for name in [
        KEYCLOAK_IDP_PLUGIN_USER_OP_DURATION,
        KEYCLOAK_IDP_PLUGIN_KC_ADMIN_REQUEST_DURATION,
        KEYCLOAK_IDP_PLUGIN_SA_OP_DURATION,
    ] {
        assert_eq!(
            bounds(name),
            expected_op_bounds,
            "{name} records seconds and must use second-scale bucket boundaries"
        );
    }

    assert_eq!(
        bounds(KEYCLOAK_IDP_PLUGIN_PROVISION_TENANT_DURATION),
        expected_provision_bounds,
        "provision_tenant cascades many KC Admin calls and needs the taller set"
    );
}
