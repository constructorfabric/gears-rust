//! Service-level operational-metrics emission tests (Phase 1: PDP-helper +
//! plugin-host instruments).
//!
//! Each test wires a `Service` with a real `UcMetricsMeter` bound to an
//! in-memory exporter (via `test_support::service_with_metrics`), drives one
//! service method, `force_flush()`es, and asserts the exported instrument
//! series. This proves the shared PDP wrapper in `domain/authz.rs` and the
//! plugin-SPI dispatch wrapper in `Service` emit per DESIGN §3.11.5.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rust_decimal::Decimal;
use time::OffsetDateTime;
use toolkit_gts::gts_id;
use toolkit_odata::{CursorV1, ODataQuery, Page as ODataPage, PageInfo, SortDir};
use usage_collector_sdk::{
    AggregationBucket, AggregationOp, AggregationResult, AggregationSpec, IdempotencyKey,
    MetadataKey, ResourceRef, UsageCollectorError, UsageKind, UsageRecord, UsageRecordStatus,
    UsageType, UsageTypeGtsId,
};
use uuid::Uuid;

use toolkit_security::pep_properties;

use super::{
    classify_deactivation_plugin_error, classify_query_result, classify_record_error,
    classify_usage_type_result,
};
use crate::domain::authz::usage_record;
use crate::domain::ports::metrics::{
    DeactivationErrorCategory, QueryErrorCategory, RecordErrorCategory, RequestOutcome,
    UsageTypeErrorCategory,
};
use crate::domain::test_support::{
    CountingAllowAllResolver, CountingPermitResolver, CountingTenantPermitResolver,
    DenyAllResolver, HappyPathPlugin, UnreachableResolver, authenticated_ctx,
    counter_sum_with_label, gauge_last, histogram_count, histogram_count_with_label, histogram_sum,
    histogram_sum_with_label, service_with_metrics, service_with_metrics_unready_plugin,
};
use usage_collector_sdk::UsageCollectorPluginError;

const SAMPLE_GTS_ID: &str = gts_id!("cf.core.uc.usage_record.v1~example.usage._.bytes_in.v1");

fn sample_record() -> UsageRecord {
    UsageRecord {
        uuid: Uuid::from_u128(0x1234),
        gts_id: UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid gts_id"),
        tenant_id: Uuid::from_u128(2),
        resource_ref: ResourceRef::new("rsc-1", "compute.vm").expect("valid resource ref"),
        subject_ref: None,
        metadata: BTreeMap::new(),
        value: Decimal::from(1),
        idempotency_key: IdempotencyKey::new("idem-1").expect("valid idempotency key"),
        corrects_id: None,
        status: UsageRecordStatus::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn sample_usage_type() -> UsageType {
    UsageType {
        // A `UsageTypeGtsId` derives from the reserved usage_record base.
        gts_id: UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid usage-type gts_id"),
        kind: UsageKind::Counter,
        metadata_fields: BTreeSet::new(),
    }
}

/// A valid base64url-encoded keyset cursor over `gts_id`, so the gauge
/// refresh's `CursorV1::decode` succeeds and the pagination loop advances.
fn encoded_cursor() -> String {
    CursorV1 {
        k: vec![gts_id!("cf.core.uc.usage_record.v1~example.usage._.sample.v1").to_owned()],
        o: SortDir::Asc,
        s: "gts_id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    }
    .encode()
    .expect("cursor encodes")
}

/// A page of `n` sample usage types carrying the given `next_cursor`.
fn type_page(n: usize, next_cursor: Option<String>) -> ODataPage<UsageType> {
    ODataPage {
        items: (0..n).map(|_| sample_usage_type()).collect(),
        page_info: PageInfo {
            next_cursor,
            prev_cursor: None,
            limit: 1000,
        },
    }
}

/// A page of `n` sample usage records (for a raw-query result-rows assertion).
fn record_page(n: usize) -> ODataPage<UsageRecord> {
    ODataPage {
        items: (0..n).map(|_| sample_record()).collect(),
        page_info: PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit: 1000,
        },
    }
}

/// An `ODataQuery` whose `$filter` pins a bounded `created_at` window — the
/// minimum a raw/aggregated query needs to clear `require_bounded_time_window`.
fn bounded_query() -> ODataQuery {
    let expr = toolkit_odata::parse_filter_string(
        "created_at ge 2026-01-01T00:00:00Z and created_at lt 2026-02-01T00:00:00Z",
    )
    .expect("test filter parses")
    .into_expr();
    ODataQuery::from(Some(expr))
}

/// A PDP permit scoped to `sample_record()`'s tenant (`Uuid::from_u128(2)`),
/// which projects to a `tenant_id` `OData` filter — the shape a successful raw /
/// aggregated LIST needs (a tenant-less scope fails closed in projection).
fn tenant_scoped_permit() -> Arc<CountingPermitResolver> {
    CountingPermitResolver::new(
        pep_properties::OWNER_TENANT_ID,
        Uuid::from_u128(2).to_string(),
    )
}

/// A single-bucket aggregated result (the no-grouping case), for the
/// aggregated result-rows assertion.
fn single_bucket_aggregation() -> AggregationResult {
    AggregationResult {
        buckets: vec![AggregationBucket {
            key: Vec::new(),
            value: Some(Decimal::from(42)),
        }],
    }
}

/// A usage type whose closed `metadata_fields` admits exactly `key`.
fn usage_type_with_metadata_field(key: &str) -> UsageType {
    UsageType {
        gts_id: UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid usage-type gts_id"),
        kind: UsageKind::Counter,
        metadata_fields: [MetadataKey::new(key).expect("valid metadata key")]
            .into_iter()
            .collect(),
    }
}

/// A sample record carrying a single `key=value` metadata entry.
fn record_with_metadata(key: &str, value: &str) -> UsageRecord {
    let mut record = sample_record();
    record.metadata.insert(
        MetadataKey::new(key).expect("valid metadata key"),
        value.to_owned(),
    );
    record
}

// ── UsageType catalog ────────────────────────────────────────────────

#[tokio::test]
async fn usage_type_list_deny_records_denied_authz() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.ut.listdeny.v1",
        Arc::new(DenyAllResolver),
    );

    let _outcome = service
        .list_usage_types(&authenticated_ctx(), &ODataQuery::default())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "operation",
            "list"
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "outcome",
            "denied"
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "error_category",
            "authz",
        ),
        1,
    );
    // Lifecycle mutations no longer touch the gauge (it is refreshed only by
    // the periodic serve loop), so a single denied attempt leaves it unset.
    assert_eq!(gauge_last(&exporter, "uc_usage_types"), None);
}

#[tokio::test]
async fn usage_type_create_success_records_request() {
    let plugin = HappyPathPlugin::new();
    plugin.set_create_usage_type(sample_usage_type());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.create.v1",
        CountingAllowAllResolver::new(),
    );

    let result = service
        .create_usage_type(&authenticated_ctx(), sample_usage_type())
        .await;
    assert!(result.is_ok(), "create should succeed: {result:?}");
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "operation",
            "create",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "outcome",
            "success"
        ),
        1,
    );
}

#[tokio::test]
async fn refresh_usage_types_gauge_sums_across_pages() {
    // The undercount fix: a two-page catalog must be summed, not truncated to
    // the first page. Page 1 (2 items) carries a next_cursor; page 2 (1 item)
    // terminates. Expected total = 3.
    let plugin = HappyPathPlugin::new();
    let probe = plugin.clone();
    plugin.set_list_usage_types_pages(vec![
        type_page(2, Some(encoded_cursor())),
        type_page(1, None),
    ]);

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.paginate.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await;
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), Some(3));

    // Pagination threaded the cursor: two SPI calls, the second carrying the
    // decoded cursor from page 1's next_cursor.
    let inputs = probe.list_usage_types_inputs();
    assert_eq!(inputs.len(), 2, "expected two paginated list calls");
    assert!(inputs[0].cursor.is_none(), "first call has no cursor");
    assert!(
        inputs[1].cursor.is_some(),
        "second call carries the page cursor"
    );
}

#[tokio::test]
async fn refresh_usage_types_gauge_single_page_sets_true_count() {
    let plugin = HappyPathPlugin::new();
    plugin.set_list_usage_types(type_page(4, None));

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.singlepage.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await;
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), Some(4));
}

#[tokio::test]
async fn refresh_usage_types_gauge_spi_error_leaves_gauge_unset() {
    // `list_usage_types` unprogrammed → SPI error → best-effort no-op; the
    // gauge is never set.
    let plugin = HappyPathPlugin::new();

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.spierr.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await;
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), None);
}

#[tokio::test]
async fn refresh_usage_types_gauge_error_leaves_prior_value() {
    // First refresh sets a known value from a one-shot queued page; the queue
    // is then empty, so the second refresh errors (unprogrammed single
    // response) and MUST leave the prior value intact.
    let plugin = HappyPathPlugin::new();
    plugin.set_list_usage_types_pages(vec![type_page(2, None)]);

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.priorval.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await; // -> Some(2)
    service.refresh_usage_types_gauge().await; // queue empty -> error -> no-op
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), Some(2));
}

#[tokio::test(start_paused = true)]
async fn refresh_usage_types_gauge_timeout_leaves_prior_value() {
    // Seed a known value, then make the SPI hang; the bounded refresh timeout
    // fires (auto-advanced under start_paused) and leaves the prior value.
    let plugin = HappyPathPlugin::new();
    plugin.set_list_usage_types_pages(vec![type_page(5, None)]);

    let (service, provider, exporter) = service_with_metrics(
        plugin.clone(),
        "test.metrics.ut.pgtimeout.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await; // -> Some(5)
    plugin.set_list_usage_types_hang();
    service.refresh_usage_types_gauge().await; // hangs -> timeout -> no-op
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), Some(5));
}

#[tokio::test]
async fn refresh_usage_types_gauge_undecodable_cursor_is_noop() {
    // A page advertising an undecodable next_cursor: the partial count MUST NOT
    // be published (a failed pagination is a no-op, not a partial count).
    let plugin = HappyPathPlugin::new();
    plugin.set_list_usage_types_pages(vec![type_page(1, Some("not-a-valid-cursor".to_owned()))]);

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.badcursor.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await;
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), None);
}

#[tokio::test]
async fn refresh_usage_types_gauge_page_cap_leaves_prior_value() {
    // A prior refresh seeds a known value. Then the plugin advertises a
    // never-terminating cursor chain — every page carries a `next_cursor`, so
    // the walk would run forever were it not for the `USAGE_TYPES_GAUGE_MAX_PAGES`
    // safety cap. A capped walk is a partial count and MUST NOT be published, so
    // the gauge keeps its prior value.
    let plugin = HappyPathPlugin::new();
    plugin.set_list_usage_types_pages(vec![type_page(6, None)]);

    let (service, provider, exporter) = service_with_metrics(
        plugin.clone(),
        "test.metrics.ut.pagecap.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await; // -> Some(6)

    // Queue one page more than the cap, each carrying a next_cursor so the walk
    // never terminates on its own — the cap is what stops it.
    plugin.set_list_usage_types_pages(
        (0..=super::USAGE_TYPES_GAUGE_MAX_PAGES)
            .map(|_| type_page(1, Some(encoded_cursor())))
            .collect(),
    );

    service.refresh_usage_types_gauge().await; // page cap hit -> no-op
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), Some(6));
}

#[tokio::test]
async fn refresh_usage_types_gauge_unbound_plugin_is_noop() {
    // No bound plugin (lazy binding not yet resolved) → early return, gauge
    // untouched, no panic. `service_with_metrics_unready_plugin` wires a
    // structurally-unready binding (registry advertises an instance, no
    // scoped client registered), so there is no plugin instance to pass in.
    let (service, provider, exporter) = service_with_metrics_unready_plugin(
        "test.metrics.ut.unbound.v1",
        CountingAllowAllResolver::new(),
    );

    service.refresh_usage_types_gauge().await;
    provider.force_flush().unwrap();

    assert_eq!(gauge_last(&exporter, "uc_usage_types"), None);
}

// ── Deactivation handler ─────────────────────────────────────────────

#[tokio::test]
async fn deactivation_pdp_deny_records_true_denied_authz_despite_notfound_response() {
    // Prefetch succeeds; PDP denies → the response is existence-oracle
    // collapsed to `NotFound`, but the metric records the TRUE `(denied, authz)`.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_record(sample_record());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.deact.deny.v1",
        Arc::new(DenyAllResolver),
    );

    let result = service
        .deactivate_usage_record(&authenticated_ctx(), Uuid::from_u128(0x1234))
        .await;
    assert!(
        matches!(result, Err(UsageCollectorError::NotFound { .. })),
        "PDP deny must collapse to NotFound on the caller surface: {result:?}",
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_deactivation_requests_total",
            "outcome",
            "denied"
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_deactivation_requests_total",
            "error_category",
            "authz",
        ),
        1,
    );
    assert_eq!(
        histogram_count(&exporter, "uc_deactivation_duration_seconds"),
        1,
    );
}

// ── Query gateway ────────────────────────────────────────────────────

#[tokio::test]
async fn query_raw_deny_records_denied_authz_and_inflight_net_zero() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.query.deny.v1",
        Arc::new(DenyAllResolver),
    );

    let _outcome = service
        .list_usage_records(
            &authenticated_ctx(),
            UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid gts_id"),
            &ODataQuery::default(),
            &[],
        )
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_query_requests_total", "query_kind", "raw"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_query_requests_total", "outcome", "denied"),
        1,
    );
    assert_eq!(histogram_count(&exporter, "uc_query_duration_seconds"), 1);
    // Deny occurs before the inflight guard is entered → the gauge must not
    // leak a positive value (0 or never-emitted).
    assert!(matches!(
        gauge_last(&exporter, "uc_query_inflight"),
        None | Some(0)
    ));
}

// ── Ingestion gateway ────────────────────────────────────────────────

#[tokio::test]
async fn ingestion_single_deny_records_rejected_authz_and_duration_no_request_counter() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.ingest.single.v1",
        Arc::new(DenyAllResolver),
    );

    let _outcome = service
        .create_usage_record(&authenticated_ctx(), sample_record())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_records_total",
            "outcome",
            "rejected"
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_records_total",
            "record_kind",
            "usage"
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_records_total",
            "error_category",
            "authz",
        ),
        1,
    );
    assert_eq!(
        histogram_count(&exporter, "uc_ingestion_duration_seconds"),
        1
    );
    // Single-emit does NOT increment the batch-only request counter.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "outcome",
            "accepted"
        ),
        0,
    );
}

#[tokio::test]
async fn ingestion_batch_all_denied_observes_batch_size_and_partial_request() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.ingest.batch.v1",
        Arc::new(DenyAllResolver),
    );

    let result = service
        .create_usage_records(&authenticated_ctx(), vec![sample_record(), sample_record()])
        .await;
    assert!(
        result.is_ok(),
        "batch returns per-record outcomes, not an outer Err"
    );
    provider.force_flush().unwrap();

    assert_eq!(histogram_count(&exporter, "uc_ingestion_batch_size"), 1);
    // Two records, both PDP-denied → two rejected/authz per-record increments.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_records_total",
            "outcome",
            "rejected"
        ),
        2,
    );
    // Any per-record rejection → the request is HTTP 207 → outcome="partial".
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "outcome",
            "partial"
        ),
        1,
    );
    assert_eq!(
        histogram_count(&exporter, "uc_ingestion_duration_seconds"),
        1
    );
}

// ── PDP-helper instruments (uc_authz_decisions_total / uc_pdp_failures_total /
//    uc_pdp_duration_seconds) ──────────────────────────────────────────────

#[tokio::test]
async fn pdp_permit_emits_permit_decision_and_duration() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.pdp.permit.v1",
        CountingAllowAllResolver::new(),
    );

    // Catalog LIST runs under require_constraints(false); an allow_all permit
    // is the legitimate happy path. The downstream plugin call may error, but
    // the PDP decision is recorded regardless.
    let _outcome = service
        .list_usage_types(&authenticated_ctx(), &ODataQuery::default())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "permit"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_authz_decisions_total",
            "operation",
            "usage_type_list",
        ),
        1,
    );
    assert_eq!(histogram_count(&exporter, "uc_pdp_duration_seconds"), 1);
}

#[tokio::test]
async fn pdp_deny_emits_deny_decision_not_failure() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.pdp.deny.v1",
        Arc::new(DenyAllResolver),
    );

    let _outcome = service
        .list_usage_types(&authenticated_ctx(), &ODataQuery::default())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "deny"),
        1,
    );
    // A deny is a decision, not a failure.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_pdp_failures_total",
            "operation",
            "usage_type_list",
        ),
        0,
    );
}

#[tokio::test]
async fn pdp_unreachable_emits_failure_not_decision() {
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.pdp.unreachable.v1",
        Arc::new(UnreachableResolver),
    );

    let _outcome = service
        .list_usage_types(&authenticated_ctx(), &ODataQuery::default())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_pdp_failures_total", "cause", "unreachable"),
        1,
    );
    // A transport failure is not a decision.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_authz_decisions_total",
            "operation",
            "usage_type_list",
        ),
        0,
    );
    // Failure completions still observe duration.
    assert_eq!(histogram_count(&exporter, "uc_pdp_duration_seconds"), 1);
}

// ── Two-stage authorization: the decision counter records the EFFECTIVE gear
//    decision, not the raw `access_scope_with` return ──────────────────────────
//
// The per-record ingestion and LIST paths authorize in two stages: the PDP
// returns a permit-with-constraints (`Ok(scope)`), then a gear-side gate
// (`scope_admits_attribution_tuple` per-record, `scope_to_odata_filter` for
// LIST) can turn that constrained permit into a deny. `uc_authz_decisions_total`
// must reflect the decision the gear returns — otherwise the deny-anomaly alert
// (DESIGN §3.11.6) never sees the cross-tenant reconnaissance signal.

#[tokio::test]
async fn pdp_permit_with_foreign_tenant_gate_denial_records_deny_not_permit() {
    // The PDP permits, but scopes the grant to ONE tenant (`granted`). The
    // record names a DIFFERENT tenant, so the per-record attribution gate
    // (`scope_admits_attribution_tuple`) turns the constrained permit into a
    // deny. That cross-tenant attempt is exactly the reconnaissance signal the
    // deny-anomaly alert keys off, so the decision counter MUST record `deny` —
    // and MUST NOT record the premature `permit` from the raw PDP return.
    let granted = Uuid::from_u128(0x5001);
    // sample_record() names tenant Uuid::from_u128(2), outside `granted`.
    let resolver =
        CountingPermitResolver::new(pep_properties::OWNER_TENANT_ID, granted.to_string());

    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.pdp.gatedeny.v1",
        resolver,
    );

    let outcome = service
        .create_usage_record(&authenticated_ctx(), sample_record())
        .await;
    assert!(
        outcome.is_err(),
        "a record attributed to a tenant outside the granted scope must be denied",
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "deny"),
        1,
        "the attribution-gate denial is the effective decision and must record `deny`",
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "permit"),
        0,
        "the premature permit from the raw PDP return must be suppressed",
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "operation", "ingest"),
        1,
        "exactly one decision sample for the one ingest authorization (no double-count)",
    );
    assert_eq!(histogram_count(&exporter, "uc_pdp_duration_seconds"), 1);
}

#[tokio::test]
async fn list_projection_denial_records_deny_not_permit() {
    // The PDP permits, but with a constraint that narrows ONLY by `resource_type`
    // — no `OWNER_TENANT_ID` pin. `scope_to_odata_filter` fails that closed (a
    // tenant-less constraint would AND into the query as a cross-tenant
    // predicate). The effective LIST decision is therefore a deny, so the
    // decision counter must record `deny`, not the raw PDP `permit`.
    let resolver =
        CountingPermitResolver::new(usage_record::PROP_RESOURCE_TYPE, "compute.vm".to_owned());

    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.list.projdeny.v1",
        resolver,
    );

    let outcome = service
        .list_usage_records(
            &authenticated_ctx(),
            UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid gts_id"),
            &ODataQuery::default(),
            &[],
        )
        .await;
    assert!(
        outcome.is_err(),
        "a tenant-less PDP scope must fail closed on the LIST path",
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "deny"),
        1,
        "a scope that fails projection is a fail-closed deny, not a permit",
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "permit"),
        0,
        "the premature permit from the raw PDP return must be suppressed",
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_authz_decisions_total",
            "operation",
            "query_raw"
        ),
        1,
    );
}

#[tokio::test]
async fn per_record_permit_records_exactly_one_permit_no_double_count() {
    // Guards the no-double-count invariant of the two-stage per-record
    // authorize: a clean permit (the PDP grants the record's own tenant AND the
    // gate admits) must emit exactly ONE `permit` decision and ONE duration
    // sample — never `permit` + `deny` for the same call, which would corrupt
    // both sides of the deny-anomaly ratio.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(sample_usage_type());
    plugin.set_create_record(sample_record());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.perrecord.permit.v1",
        CountingTenantPermitResolver::new(),
    );

    service
        .create_usage_record(&authenticated_ctx(), sample_record())
        .await
        .expect("permitted single emit persists");
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "permit"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "decision", "deny"),
        0,
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_authz_decisions_total", "operation", "ingest"),
        1,
    );
    assert_eq!(histogram_count(&exporter, "uc_pdp_duration_seconds"), 1);
}

// ── Plugin-host instruments (uc_plugin_call_duration_seconds /
//    uc_plugin_accept_errors_total / uc_plugin_ready) ─────────────────────

#[tokio::test]
async fn plugin_backend_error_records_duration_counter_and_ready() {
    // AllowAll permits; the unprogrammed HappyPathPlugin returns
    // `Internal` for `list_usage_types` → a backend-classified fault.
    let (service, provider, exporter) = service_with_metrics(
        HappyPathPlugin::new(),
        "test.metrics.plugin.backend.v1",
        CountingAllowAllResolver::new(),
    );

    let _outcome = service
        .list_usage_types(&authenticated_ctx(), &ODataQuery::default())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "uc_plugin_call_duration_seconds"),
        1,
        "error completions are still dispatch completions",
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "error_category",
            "backend_error",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "operation",
            "list_usage_types",
        ),
        1,
    );
    // A successful structural binding sets the readiness gauge to 1.
    assert_eq!(gauge_last(&exporter, "uc_plugin_ready"), Some(1));
}

#[tokio::test]
async fn plugin_domain_typed_error_does_not_increment_accept_counter() {
    // A domain-typed variant (UsageTypeNotFound) is a caller-visible outcome,
    // NOT a plugin fault — its duration is still observed, but it MUST NOT
    // increment uc_plugin_accept_errors_total.
    let plugin = HappyPathPlugin::new();
    let gts_id = UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid gts_id");
    plugin.set_get_usage_type_not_found(gts_id.clone());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.plugin.domain.v1",
        CountingAllowAllResolver::new(),
    );

    let _outcome = service.get_usage_type(&authenticated_ctx(), gts_id).await;
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "uc_plugin_call_duration_seconds"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "operation",
            "get_usage_type",
        ),
        0,
        "domain-typed plugin errors must not count as accept errors",
    );
}

#[tokio::test]
async fn plugin_unready_increments_unready_counter_and_zeroes_ready() {
    let (service, provider, exporter) = service_with_metrics_unready_plugin(
        "test.metrics.plugin.unready.v1",
        CountingAllowAllResolver::new(),
    );

    let _outcome = service
        .list_usage_types(&authenticated_ctx(), &ODataQuery::default())
        .await;
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "error_category",
            "unready",
        ),
        1,
    );
    assert_eq!(gauge_last(&exporter, "uc_plugin_ready"), Some(0));
    // No SPI invocation occurred, so no duration sample is recorded.
    assert_eq!(
        histogram_count(&exporter, "uc_plugin_call_duration_seconds"),
        0,
    );
}

// ── Emit-path plugin-host instrument coverage ───────────────────────────────
//
// The ingestion/emit SPI dispatches (`get_usage_type`, `get_usage_record`,
// `create_usage_record` / `create_usage_records`) route through the same
// `instrument_spi` / `resolve_plugin_for` wrappers as the read paths, so a
// permitted emit MUST land on `uc_plugin_call_duration_seconds` (and, on a
// backend fault, `uc_plugin_accept_errors_total`) under the emit `operation`
// labels. The deny-path emit tests above short-circuit at PDP and never reach
// the SPI, so they cannot guard this wiring; these tests drive the dispatch.

#[tokio::test]
async fn ingestion_single_success_dispatch_records_plugin_call_duration_per_op() {
    // Permit + catalog hit + ordinary counter semantics + persist echo. The
    // catalog `get_usage_type` and the persist `create_usage_record` are BOTH
    // instrumented dispatches, each contributing one duration sample under its
    // own `operation` label.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(sample_usage_type());
    plugin.set_create_record(sample_record());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ingestok.single.v1",
        CountingTenantPermitResolver::new(),
    );

    service
        .create_usage_record(&authenticated_ctx(), sample_record())
        .await
        .expect("permitted single emit persists");
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_plugin_call_duration_seconds",
            "operation",
            "create_usage_record",
        ),
        1,
        "the persist SPI dispatch MUST contribute exactly one duration sample",
    );
    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_plugin_call_duration_seconds",
            "operation",
            "get_usage_type",
        ),
        1,
        "the catalog lookup on the emit path is instrumented too",
    );
    // A clean persist raises no backend fault.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "operation",
            "create_usage_record",
        ),
        0,
    );
}

#[tokio::test]
async fn ingestion_batch_success_dispatch_records_plugin_call_duration_per_op() {
    // Two records sharing one gts_id: the catalog pre-pass dedups to a single
    // `get_usage_type` dispatch, and the eligible records persist through one
    // `create_usage_records` dispatch — each an instrumented completion.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(sample_usage_type());
    plugin.set_create_records(vec![Ok(sample_record()), Ok(sample_record())]);

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ingestok.batch.v1",
        CountingTenantPermitResolver::new(),
    );

    let per_record = service
        .create_usage_records(&authenticated_ctx(), vec![sample_record(), sample_record()])
        .await
        .expect("batch returns per-record outcomes, not an outer Err");
    assert!(
        per_record.iter().all(Result::is_ok),
        "both permitted records persist",
    );
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_plugin_call_duration_seconds",
            "operation",
            "create_usage_records",
        ),
        1,
        "the batch persist SPI dispatch MUST contribute exactly one duration sample",
    );
    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_plugin_call_duration_seconds",
            "operation",
            "get_usage_type",
        ),
        1,
        "the two records dedup to a single catalog dispatch",
    );
    // Every record persisted → the batch request completes as `accepted` (not
    // `partial`, which needs at least one per-record rejection).
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "outcome",
            "accepted",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "error_category",
            "none",
        ),
        1,
    );
    // An all-success request must NOT record `partial`.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "outcome",
            "partial",
        ),
        0,
    );
}

#[tokio::test]
async fn ingestion_single_backend_error_increments_accept_errors_per_op() {
    // Catalog hit, then the persist SPI faults with `Internal` (backend). The
    // failed dispatch is still a completed dispatch (one duration sample) AND a
    // backend-classified accept error under `operation="create_usage_record"`.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(sample_usage_type());
    plugin.set_create_record_err(UsageCollectorPluginError::internal(
        "usage-collector test fake: simulated persist backend fault",
    ));

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ingesterr.single.v1",
        CountingTenantPermitResolver::new(),
    );

    let outcome = service
        .create_usage_record(&authenticated_ctx(), sample_record())
        .await;
    assert!(outcome.is_err(), "a persist backend fault surfaces as Err");
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "operation",
            "create_usage_record",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "error_category",
            "backend_error",
        ),
        1,
    );
    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_plugin_call_duration_seconds",
            "operation",
            "create_usage_record",
        ),
        1,
        "an error completion is still a dispatch completion",
    );
}

#[tokio::test]
async fn ingestion_batch_backend_error_increments_accept_errors_per_op() {
    // `get_usage_type` succeeds so the record is eligible and the batch reaches
    // the persist SPI; `create_usage_records` is left unprogrammed, so the stub
    // returns an outer `Internal` transport fault (backend-classified) which
    // surfaces as the batch-level outer `Err`.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(sample_usage_type());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ingesterr.batch.v1",
        CountingTenantPermitResolver::new(),
    );

    let outcome = service
        .create_usage_records(&authenticated_ctx(), vec![sample_record()])
        .await;
    assert!(
        outcome.is_err(),
        "a batch-level persist transport fault surfaces as an outer Err",
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "operation",
            "create_usage_records",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "error_category",
            "backend_error",
        ),
        1,
    );
    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_plugin_call_duration_seconds",
            "operation",
            "create_usage_records",
        ),
        1,
    );
}

#[tokio::test]
async fn ingestion_batch_unready_plugin_increments_unready_counter() {
    // The batch path resolves the plugin (via `resolve_plugin_for`) BEFORE the
    // PDP fan-out; a structurally-unready binding short-circuits there and MUST
    // still emit the `unready` accept error. The pre-completion emit path called
    // `get_plugin()` directly and skipped this counter — this guards the switch
    // to `resolve_plugin_for`.
    let (service, provider, exporter) = service_with_metrics_unready_plugin(
        "test.metrics.ingestunready.batch.v1",
        CountingTenantPermitResolver::new(),
    );

    let outcome = service
        .create_usage_records(&authenticated_ctx(), vec![sample_record()])
        .await;
    assert!(
        outcome.is_err(),
        "an unready plugin binding short-circuits the batch with an outer Err",
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_plugin_accept_errors_total",
            "error_category",
            "unready",
        ),
        1,
    );
    assert_eq!(gauge_last(&exporter, "uc_plugin_ready"), Some(0));
    // Resolution failed before any SPI dispatch, so no duration sample lands.
    assert_eq!(
        histogram_count(&exporter, "uc_plugin_call_duration_seconds"),
        0,
    );
    // A whole-request (outer `Err`) failure is a single `rejected` batch
    // request classified as `plugin_error` — distinct from the per-record
    // `partial`/`accepted` arms.
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "outcome",
            "rejected",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_ingestion_requests_total",
            "error_category",
            "plugin_error",
        ),
        1,
    );
}

// ── Metric-label classifiers: exhaustive arm coverage (pure fns) ────────────
//
// `classify_record_error`, `classify_query_result`, `classify_usage_type_result`
// and `classify_deactivation_plugin_error` decide the `error_category` /
// `outcome` labels operators alert on. The end-to-end tests above exercise only
// the authz / not-found arms; these table-driven unit tests pin EVERY arm of
// the closed §3.11.5 vocabularies, so a misrouted variant is caught here rather
// than as a silently-wrong dashboard series.

/// The canonical sample `gts_id` as a typed id, for classifier fixtures.
fn gts() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_GTS_ID).expect("valid gts_id")
}

/// `(input result, expected (outcome, error_category))` row for the query
/// classifier table.
type QueryClassifierCase = (
    Result<(), UsageCollectorError>,
    (RequestOutcome, QueryErrorCategory),
);

/// `(input result, expected (outcome, error_category))` row for the usage-type
/// classifier table.
type UsageTypeClassifierCase = (
    Result<(), UsageCollectorError>,
    (RequestOutcome, UsageTypeErrorCategory),
);

#[test]
fn classify_record_error_maps_each_arm() {
    let cases: Vec<(UsageCollectorError, RecordErrorCategory)> = vec![
        (
            UsageCollectorError::permission_denied("pdp"),
            RecordErrorCategory::Authz,
        ),
        // Catalog-absent UsageType (usage-type resource) → unknown_usage_type.
        (
            UsageCollectorError::usage_type_not_found(&gts()),
            RecordErrorCategory::UnknownUsageType,
        ),
        // A record-resource NotFound (an L1 `corrects_id` reference) stays with
        // the semantics family — NOT folded into catalog absence.
        (
            UsageCollectorError::usage_record_not_found(Uuid::from_u128(7)),
            RecordErrorCategory::SemanticsViolation,
        ),
        (
            UsageCollectorError::corrects_id_not_found(Uuid::from_u128(8)),
            RecordErrorCategory::SemanticsViolation,
        ),
        // The two metadata reasons are the ONLY InvalidArgument arms that map to
        // metadata_size; any other validation reason is semantics_violation.
        (
            UsageCollectorError::metadata_size_exceeded(9000, 8192),
            RecordErrorCategory::MetadataSize,
        ),
        (
            UsageCollectorError::unknown_metadata_key(&gts(), "region"),
            RecordErrorCategory::MetadataSize,
        ),
        (
            UsageCollectorError::negative_counter_value(Decimal::from(-1)),
            RecordErrorCategory::SemanticsViolation,
        ),
        // Conflict: idempotency is its own category; any other conflict reason
        // is semantics_violation.
        (
            UsageCollectorError::idempotency_conflict("k", Uuid::from_u128(9)),
            RecordErrorCategory::IdempotencyConflict,
        ),
        (
            UsageCollectorError::corrects_id_inactive(Uuid::from_u128(10)),
            RecordErrorCategory::SemanticsViolation,
        ),
        // Anything unclassified is a plugin_error.
        (
            UsageCollectorError::internal("boom"),
            RecordErrorCategory::PluginError,
        ),
        (
            UsageCollectorError::service_unavailable("down", None),
            RecordErrorCategory::PluginError,
        ),
    ];
    for (err, expected) in cases {
        assert_eq!(
            classify_record_error(&err),
            expected,
            "misclassified {err:?}"
        );
    }
}

#[test]
fn classify_query_result_maps_each_arm() {
    let cases: Vec<QueryClassifierCase> = vec![
        (Ok(()), (RequestOutcome::Success, QueryErrorCategory::None)),
        (
            Err(UsageCollectorError::permission_denied("pdp")),
            (RequestOutcome::Denied, QueryErrorCategory::Authz),
        ),
        (
            Err(UsageCollectorError::usage_type_not_found(&gts())),
            (RequestOutcome::Error, QueryErrorCategory::UnknownUsageType),
        ),
        // The only service-level InvalidArgument on the query path is the
        // mandatory bounded-window guard.
        (
            Err(UsageCollectorError::missing_time_window()),
            (RequestOutcome::Error, QueryErrorCategory::QueryBudget),
        ),
        (
            Err(UsageCollectorError::internal("boom")),
            (RequestOutcome::Error, QueryErrorCategory::PluginError),
        ),
        (
            Err(UsageCollectorError::service_unavailable("down", None)),
            (RequestOutcome::Error, QueryErrorCategory::PluginError),
        ),
    ];
    for (result, expected) in cases {
        assert_eq!(
            classify_query_result(&result),
            expected,
            "misclassified {result:?}",
        );
    }
}

#[test]
fn classify_usage_type_result_maps_each_arm() {
    let cases: Vec<UsageTypeClassifierCase> = vec![
        (
            Ok(()),
            (RequestOutcome::Success, UsageTypeErrorCategory::None),
        ),
        (
            Err(UsageCollectorError::permission_denied("pdp")),
            (RequestOutcome::Denied, UsageTypeErrorCategory::Authz),
        ),
        (
            Err(UsageCollectorError::usage_type_already_exists(&gts())),
            (RequestOutcome::Error, UsageTypeErrorCategory::Conflict),
        ),
        (
            Err(UsageCollectorError::usage_type_not_found(&gts())),
            (RequestOutcome::Error, UsageTypeErrorCategory::NotFound),
        ),
        (
            Err(UsageCollectorError::usage_type_referenced(&gts(), 3)),
            (RequestOutcome::Error, UsageTypeErrorCategory::Referenced),
        ),
        (
            Err(UsageCollectorError::invalid_usage_kind("bogus")),
            (RequestOutcome::Error, UsageTypeErrorCategory::Validation),
        ),
        // A Conflict whose reason is NOT UsageTypeReferenced is not a named
        // lifecycle arm — it falls through to the catch-all plugin_error.
        (
            Err(UsageCollectorError::already_inactive(Uuid::from_u128(11))),
            (RequestOutcome::Error, UsageTypeErrorCategory::PluginError),
        ),
        (
            Err(UsageCollectorError::internal("boom")),
            (RequestOutcome::Error, UsageTypeErrorCategory::PluginError),
        ),
    ];
    for (result, expected) in cases {
        assert_eq!(
            classify_usage_type_result(&result),
            expected,
            "misclassified {result:?}",
        );
    }
}

#[test]
fn classify_deactivation_plugin_error_maps_each_arm() {
    let already = UsageCollectorPluginError::UsageRecordAlreadyInactive {
        id: Uuid::from_u128(1),
    };
    let not_found = UsageCollectorPluginError::UsageRecordNotFound {
        id: Uuid::from_u128(2),
    };
    let transient = UsageCollectorPluginError::transient("backend blip");
    let internal = UsageCollectorPluginError::internal("boom");

    assert_eq!(
        classify_deactivation_plugin_error(&already),
        DeactivationErrorCategory::AlreadyInactive,
    );
    assert_eq!(
        classify_deactivation_plugin_error(&not_found),
        DeactivationErrorCategory::NotFound,
    );
    // Every other plugin fault (retryable or not) is a plugin_error.
    assert_eq!(
        classify_deactivation_plugin_error(&transient),
        DeactivationErrorCategory::PluginError,
    );
    assert_eq!(
        classify_deactivation_plugin_error(&internal),
        DeactivationErrorCategory::PluginError,
    );
}

// ── Query gateway: success + aggregated coverage ────────────────────────────
//
// The deny test above exits at PDP before the inflight guard / dispatch; these
// drive a permitted raw list and a permitted aggregation to completion, pinning
// the `success` outcome, the result-rows observation (off page size for raw /
// bucket count for aggregated), and the duration sample the deny path can't
// reach.

#[tokio::test]
async fn query_raw_success_records_success_rows_and_duration() {
    let plugin = HappyPathPlugin::new();
    plugin.set_list_usage_records_response(record_page(3));

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.query.rawok.v1",
        tenant_scoped_permit(),
    );

    let page = service
        .list_usage_records(&authenticated_ctx(), gts(), &bounded_query(), &[])
        .await
        .expect("a permitted, bounded raw query succeeds");
    assert_eq!(page.items.len(), 3);
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_query_requests_total", "outcome", "success"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(&exporter, "uc_query_requests_total", "query_kind", "raw"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_query_requests_total",
            "error_category",
            "none"
        ),
        1,
    );
    // Result rows observed exactly once, off the returned page size (3).
    assert_eq!(
        histogram_count_with_label(&exporter, "uc_query_result_rows", "query_kind", "raw"),
        1,
    );
    assert!(
        (histogram_sum_with_label(&exporter, "uc_query_result_rows", "query_kind", "raw") - 3.0)
            .abs()
            < f64::EPSILON,
        "the raw result-rows observation must carry the page size (3)",
    );
    assert_eq!(
        histogram_count_with_label(&exporter, "uc_query_duration_seconds", "query_kind", "raw"),
        1,
    );
}

#[tokio::test]
async fn query_aggregated_success_records_success_rows_and_duration() {
    let plugin = HappyPathPlugin::new();
    plugin.set_query_aggregated_usage_records_response(single_bucket_aggregation());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.query.aggok.v1",
        tenant_scoped_permit(),
    );

    let result = service
        .query_aggregated_usage_records(
            &authenticated_ctx(),
            gts(),
            &bounded_query(),
            &[],
            AggregationSpec {
                op: AggregationOp::Sum,
                group_by: Vec::new(),
            },
        )
        .await
        .expect("a permitted, bounded aggregation succeeds");
    assert_eq!(result.buckets.len(), 1);
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(&exporter, "uc_query_requests_total", "outcome", "success"),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_query_requests_total",
            "query_kind",
            "aggregated",
        ),
        1,
    );
    // Aggregated result rows are observed off the bucket count (1), NOT a page
    // size — this is the branch the raw success test cannot cover.
    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_query_result_rows",
            "query_kind",
            "aggregated",
        ),
        1,
    );
    assert!(
        (histogram_sum_with_label(
            &exporter,
            "uc_query_result_rows",
            "query_kind",
            "aggregated",
        ) - 1.0)
            .abs()
            < f64::EPSILON,
        "the aggregated result-rows observation must carry the bucket count (1)",
    );
    assert_eq!(
        histogram_count_with_label(
            &exporter,
            "uc_query_duration_seconds",
            "query_kind",
            "aggregated",
        ),
        1,
    );
}

// ── Emission: uc_record_metadata_bytes observe / skip ───────────────────────

#[tokio::test]
async fn ingestion_single_with_metadata_observes_record_metadata_bytes() {
    // A permitted single emit whose usage type declares the key it carries
    // reaches `observe_metadata_bytes` and persists.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(usage_type_with_metadata_field("region"));
    plugin.set_create_record(sample_record());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ingest.meta.v1",
        CountingTenantPermitResolver::new(),
    );

    service
        .create_usage_record(
            &authenticated_ctx(),
            record_with_metadata("region", "us-east-1"),
        )
        .await
        .expect("a permitted record with declared metadata persists");
    provider.force_flush().unwrap();

    // A record carrying metadata contributes exactly one observation, whose
    // magnitude is the serialized JSON byte size (> 0).
    assert_eq!(histogram_count(&exporter, "uc_record_metadata_bytes"), 1);
    assert!(
        histogram_sum(&exporter, "uc_record_metadata_bytes") > 0.0,
        "the serialized metadata size must be a positive byte count",
    );
}

#[tokio::test]
async fn ingestion_single_empty_metadata_skips_record_metadata_bytes() {
    // `sample_record()` carries no metadata → `observe_metadata_bytes` returns
    // before recording, so the instrument stays empty even on a clean persist.
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type(sample_usage_type());
    plugin.set_create_record(sample_record());

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ingest.nometa.v1",
        CountingTenantPermitResolver::new(),
    );

    service
        .create_usage_record(&authenticated_ctx(), sample_record())
        .await
        .expect("a permitted record with no metadata persists");
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "uc_record_metadata_bytes"),
        0,
        "an empty-metadata record must record nothing on uc_record_metadata_bytes",
    );
}

// ── UsageType gauge: delete-path refresh + refresh-failure isolation ─────────

#[tokio::test]
async fn usage_type_delete_success_records_request() {
    let plugin = HappyPathPlugin::new();
    plugin.set_delete_usage_type_ok();

    let (service, provider, exporter) = service_with_metrics(
        plugin,
        "test.metrics.ut.delete.v1",
        CountingAllowAllResolver::new(),
    );

    service
        .delete_usage_type(&authenticated_ctx(), gts())
        .await
        .expect("delete should succeed");
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "operation",
            "delete",
        ),
        1,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_usage_type_requests_total",
            "outcome",
            "success",
        ),
        1,
    );
}
