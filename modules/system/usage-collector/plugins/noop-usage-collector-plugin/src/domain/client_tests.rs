use chrono::{TimeZone, Utc};
use modkit_security::AccessScope;
use usage_collector_sdk::UsageCollectorPluginClientV1;
use usage_collector_sdk::models::{
    AggregationFn, AggregationQuery, GroupByDimension, RawQuery, Subject, UsageKind, UsageRecord,
};
use uuid::Uuid;

use super::Service;

/// Build a representative non-degenerate `AccessScope` (single tenant grant) so
/// the noop plugin's input matches what the gateway would actually deliver
/// post-PDP. The noop semantics ignore the scope, but using a real scope here
/// guards against tests that only pass because they tickle a degenerate path.
fn tenant_scope() -> AccessScope {
    AccessScope::for_tenant(Uuid::nil())
}

fn make_record(tenant_id: Uuid) -> UsageRecord {
    UsageRecord {
        module: "test-module".to_owned(),
        tenant_id,
        metric: "test.metric".to_owned(),
        kind: UsageKind::Gauge,
        value: 1.0,
        resource_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        idempotency_key: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        metadata: None,
    }
}

#[tokio::test]
async fn create_usage_record_always_returns_ok() {
    // "always" is documented by driving representative variation across the
    // input axes the noop plugin would otherwise be tempted to branch on:
    // metric kind, presence of a subject, and value (including zero). A single
    // call would not distinguish "always returns Ok" from "this one input
    // happens to be Ok".
    let service = Service;
    let plugin: &dyn UsageCollectorPluginClientV1 = &service;

    let base_tenant = Uuid::new_v4();
    let cases: Vec<(UsageKind, Option<Subject>, f64)> = vec![
        (
            UsageKind::Gauge,
            Some(Subject::with_type(Uuid::nil(), "test.subject")),
            1.0,
        ),
        (UsageKind::Gauge, None, 0.0),
        (
            UsageKind::Counter,
            Some(Subject::with_type(Uuid::new_v4(), "test.subject")),
            42.0,
        ),
        (UsageKind::Counter, None, 0.0),
    ];

    for (kind, subject, value) in cases {
        let mut rec = make_record(base_tenant);
        rec.kind = kind;
        rec.subject = subject.clone();
        rec.value = value;
        let result = plugin.create_usage_record(rec).await;
        assert!(
            result.is_ok(),
            "noop plugin must accept every variant; failed for kind={kind:?}, subject={subject:?}, value={value}: {result:?}",
        );
    }
}

#[tokio::test]
async fn noop_query_aggregated_returns_empty_vec() {
    // Parameterized over the cross-product of `AggregationFn` × representative
    // `group_by` sets so the test distinguishes "noop ignores all inputs" from
    // "noop returns empty for this one input shape" (mirrors the
    // `noop_query_raw_returns_empty_paged_result_with_echoed_page_size` shape).
    let svc = Service;
    let from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();

    let functions = [
        AggregationFn::Sum,
        AggregationFn::Avg,
        AggregationFn::Min,
        AggregationFn::Max,
        AggregationFn::Count,
    ];
    let group_by_sets: [Vec<GroupByDimension>; 4] = [
        vec![],
        vec![GroupByDimension::UsageType],
        vec![GroupByDimension::UsageType, GroupByDimension::Resource],
        vec![GroupByDimension::Subject, GroupByDimension::Source],
    ];

    for function in functions {
        for group_by in &group_by_sets {
            let query = AggregationQuery {
                scope: tenant_scope(),
                time_range: (from, to),
                function,
                group_by: group_by.clone(),
                bucket_size: None,
                usage_type: None,
                resource_id: None,
                resource_type: None,
                subject_id: None,
                subject_type: None,
                source: None,
            };
            let rows = svc
                .query_aggregated(query)
                .await
                .expect("noop plugin must succeed");
            assert!(
                rows.is_empty(),
                "expected empty result for fn={function:?} group_by={group_by:?}; got {} rows",
                rows.len()
            );
        }
    }
}

#[tokio::test]
async fn noop_query_raw_returns_empty_paged_result_with_echoed_page_size() {
    // Parameterized over distinct page_size values to assert "noop echoes
    // whatever bound you pass" rather than "this one value happens to round
    // trip"; also pins the cursor fields to `None` (no pagination state is
    // synthesized by the stub).
    let svc = Service;
    let from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
    for page_size in [1_u32, 25, 100, 500] {
        let query = RawQuery {
            scope: tenant_scope(),
            time_range: (from, to),
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_type: None,
            subject_id: None,
            cursor: None,
            page_size,
        };
        let paged = svc.query_raw(query).await.expect("noop stub never errors");
        assert!(paged.items.is_empty());
        assert!(paged.page_info.next_cursor.is_none());
        assert!(paged.page_info.prev_cursor.is_none());
        assert_eq!(paged.page_info.limit, u64::from(page_size));
    }
}
