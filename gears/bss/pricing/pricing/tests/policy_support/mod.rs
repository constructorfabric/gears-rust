//! Explicit policy authoring input for usage-entry fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[toolkit::api::canonical_prelude::resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
struct MeterResource;
pub fn input() -> serde_json::Value {
    serde_json::json!({
        "rating_window":{"kind":"billing_cycle"},
        "aggregation_scope":"subscription_line","reset":"rating_window_start",
        "quantity_semantics":{"meter":{"usage_type_id":"vm-hours","version":"v1"},
            "unit":"VM\u{b7}hour","fold":"SUM","accrual_policy_version":"integrated-v1"},
        "partial_window":"actual_quantity_full_thresholds"
    })
}

/// Storage declaration used by the Products cross-gear retirement fixture.
pub fn storage_input() -> serde_json::Value {
    let mut policy = input();
    policy["quantity_semantics"]["meter"]["usage_type_id"] = serde_json::json!("storage");
    policy["quantity_semantics"]["unit"] = serde_json::json!("GB");
    policy
}

/// Contract-test declarations only: these names are not production registrations.
#[derive(Default)]
pub struct MeterProvider {
    pub failure: std::sync::atomic::AtomicU8,
    pub probe_db: Option<toolkit_db::DBProvider<toolkit_db::DbError>>,
    pub calls: std::sync::atomic::AtomicUsize,
    pub callers: std::sync::Mutex<Vec<uuid::Uuid>>,
}
#[async_trait::async_trait]
impl bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1 for MeterProvider {
    async fn resolve(
        &self,
        ctx: &toolkit_security::SecurityContext,
        meter: bss_pricing_sdk::terms::MeterRef,
    ) -> Result<
        bss_pricing_sdk::meter_semantics::MeterSemantics,
        toolkit_canonical_errors::CanonicalError,
    > {
        use std::sync::atomic::Ordering;
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.callers.lock().unwrap().push(ctx.subject_id());
        if let Some(db) = &self.probe_db {
            bss_pricing::infra::storage::repo::price_book_entry_repo::find(
                &db.conn().unwrap(),
                &toolkit_db::secure::AccessScope::for_tenant(ctx.subject_tenant_id()),
                ctx.subject_tenant_id(),
                uuid::Uuid::nil(),
            )
            .await
            .unwrap();
        }
        match self.failure.load(Ordering::SeqCst) {
            1 => {
                return Err(
                    toolkit_canonical_errors::CanonicalError::service_unavailable().create(),
                );
            }
            2 => {
                return Err(MeterResource::permission_denied()
                    .with_reason("METER_DENIED")
                    .create());
            }
            _ => {}
        }
        let unit = match (meter.usage_type_id.as_str(), meter.version.as_str()) {
            ("vm-hours", "v1") => "VM\u{b7}hour",
            ("cloudlet-hours", "v1") => "cloudlet\u{b7}hour",
            ("storage", "v1") => "GB",
            _ => {
                return Err(MeterResource::invalid_argument()
                    .with_field_violation(
                        "meter",
                        "unknown immutable version",
                        "METER_VERSION_UNKNOWN",
                    )
                    .create());
            }
        };
        let mut evidence = bss_pricing_sdk::meter_semantics::MeterSemantics {
            meter,
            canonical_unit: unit.into(),
            fold: bss_pricing_sdk::terms::Fold::Sum,
            accrual_policy_version: "integrated-v1".into(),
            source_integrated: true,
            digest: [7; 32],
        };
        match self.failure.load(Ordering::SeqCst) {
            3 => evidence.canonical_unit = "second".into(),
            4 => evidence.meter.version = "v2".into(),
            5 => evidence.accrual_policy_version = "raw-v1".into(),
            6 => evidence.source_integrated = false,
            7 => evidence.meter.usage_type_id = "other".into(),
            _ => {}
        }
        Ok(evidence)
    }
}
