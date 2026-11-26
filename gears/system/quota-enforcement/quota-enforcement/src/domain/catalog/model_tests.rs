#![allow(clippy::expect_used)]

use gts::GtsTypeId;
use quota_enforcement_sdk::MetricKind;
use serde_json::{Value, json};

use super::{CatalogEntry, MetricMode, build_entry, derives_from};
use crate::domain::ports::metrics::ValidationReason;
use crate::domain::ports::type_registry::{RegisteredType, TypeRead, TypeRegistry};
use crate::infra::types_registry::PlatformTypeRegistry;
use crate::test_support::{
    DEVICE_SUBJECT_TYPE, METRIC_TOKENS, TENANT_SUBJECT_TYPE, USER_SUBJECT_TYPE, platform_registry,
};

async fn registered(id: &str) -> RegisteredType {
    let id = GtsTypeId::new(id);
    let mut answers =
        PlatformTypeRegistry::new(platform_registry(), std::time::Duration::from_secs(5))
            .read_types(vec![(id.clone(), None)])
            .await
            .expect("read");
    match answers.remove(&id) {
        Some(TypeRead::Found(registered)) => *registered,
        other => panic!("expected a registered type, got {other:?}"),
    }
}

fn reason(entry: &CatalogEntry) -> Option<ValidationReason> {
    match entry {
        CatalogEntry::Served(_) => None,
        CatalogEntry::Unavailable { reason } => Some(*reason),
    }
}

fn with(mut registered: RegisteredType, edit: impl FnOnce(&mut RegisteredType)) -> RegisteredType {
    edit(&mut registered);
    registered
}

#[tokio::test]
async fn a_valid_metric_type_is_served_with_its_traits() {
    let entry = build_entry(registered(METRIC_TOKENS).await);
    let CatalogEntry::Served(served) = entry else {
        panic!("served expected");
    };
    assert_eq!(served.kind(), MetricKind::Counter);
    assert_eq!(served.mode(), MetricMode::QuotaGated);
    assert_eq!(served.id().as_str(), METRIC_TOKENS);
}

#[tokio::test]
async fn an_abstract_type_is_unavailable() {
    let entry = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.is_abstract = true;
    }));
    assert_eq!(reason(&entry), Some(ValidationReason::Abstract));
}

#[tokio::test]
async fn a_type_outside_the_metric_base_is_unavailable() {
    let entry = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.id = GtsTypeId::new(USER_SUBJECT_TYPE);
    }));
    assert_eq!(reason(&entry), Some(ValidationReason::NotDerived));
}

#[tokio::test]
async fn an_own_version_of_major_zero_is_unavailable() {
    let entry = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.id = GtsTypeId::new("gts.cf.core.quota.metric.v1~cf.qetest.metric.draft.v0~");
    }));
    assert_eq!(reason(&entry), Some(ValidationReason::MajorVersionZero));
}

#[tokio::test]
async fn a_wrong_trait_makes_the_metric_unavailable() {
    for traits in [
        json!({ "admitted_subjects": [] }),
        json!({ "kind": "histogram" }),
        json!({ "kind": "counter", "enforcement": "sometimes" }),
        json!({ "kind": "counter", "admitted_subjects": ["gts.cf.core.qe.other.v1~"] }),
        json!({ "kind": "counter", "admitted_subjects": ["not a type id"] }),
        json!({ "kind": "counter", "extra": true }),
        Value::Null,
    ] {
        let entry = build_entry(with(registered(METRIC_TOKENS).await, |t| {
            t.effective_traits = traits.clone();
        }));
        assert_eq!(
            reason(&entry),
            Some(ValidationReason::TraitInvalid),
            "{traits}"
        );
    }
}

#[tokio::test]
async fn a_schema_that_does_not_compile_makes_the_metric_unavailable() {
    let entry = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.payload = Err("does not compile".to_owned());
    }));
    assert_eq!(reason(&entry), Some(ValidationReason::SchemaInvalid));
}

#[tokio::test]
async fn enforcement_defaults_to_quota_gated_and_direct_is_read() {
    let direct = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.effective_traits = json!({ "kind": "gauge", "enforcement": "direct" });
    }));
    let CatalogEntry::Served(served) = direct else {
        panic!("served expected");
    };
    assert_eq!(served.mode(), MetricMode::Direct);
    assert_eq!(served.kind(), MetricKind::Gauge);
}

#[tokio::test]
async fn admission_takes_an_exact_or_derived_type_and_always_the_tenant() {
    let CatalogEntry::Served(served) = build_entry(registered(METRIC_TOKENS).await) else {
        panic!("served expected");
    };
    assert!(served.admits(&GtsTypeId::new(USER_SUBJECT_TYPE)));
    assert!(served.admits(&GtsTypeId::new(
        "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~acme.core.quota.admin.v1~"
    )));
    assert!(served.admits(&GtsTypeId::new(TENANT_SUBJECT_TYPE)));
    assert!(!served.admits(&GtsTypeId::new(DEVICE_SUBJECT_TYPE)));
}

#[tokio::test]
async fn admitting_the_subject_base_admits_every_subject_type() {
    let CatalogEntry::Served(served) = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.effective_traits = json!({
            "kind": "counter",
            "admitted_subjects": [quota_resolver_sdk::QUOTA_SUBJECT_TYPE_ID],
        });
    })) else {
        panic!("served expected");
    };
    assert!(served.admits(&GtsTypeId::new(USER_SUBJECT_TYPE)));
    assert!(served.admits(&GtsTypeId::new(DEVICE_SUBJECT_TYPE)));
}

#[tokio::test]
async fn the_empty_default_admits_tenant_quotas_only() {
    let CatalogEntry::Served(served) = build_entry(with(registered(METRIC_TOKENS).await, |t| {
        t.effective_traits = json!({ "kind": "counter" });
    })) else {
        panic!("served expected");
    };
    assert!(served.admits(&GtsTypeId::new(TENANT_SUBJECT_TYPE)));
    assert!(!served.admits(&GtsTypeId::new(USER_SUBJECT_TYPE)));
}

#[test]
fn derivation_is_a_strict_prefix() {
    let base = "gts.cf.core.quota.metric.v1~";
    assert!(derives_from(&GtsTypeId::new(METRIC_TOKENS), base));
    assert!(!derives_from(&GtsTypeId::new(base), base));
    assert!(!derives_from(&GtsTypeId::new(USER_SUBJECT_TYPE), base));
}
