use super::{BulkCreated, BulkCreatedItem, BulkRecord};
use crate::models::{OperationType, QuotaId};

#[test]
fn a_bulk_record_round_trips_under_its_version() {
    let outcome = BulkCreated {
        items: vec![BulkCreatedItem {
            index: 0,
            idempotency_key: Some("seat-1".to_owned()),
            quota_id: QuotaId::generate(),
        }],
    };
    let blob = serde_json::to_value(BulkRecord::new(outcome.clone())).expect("serialize");
    assert_eq!(blob["__version"], 1);
    let back: BulkRecord<BulkCreated> = serde_json::from_value(blob).expect("deserialize");
    assert_eq!(back.outcome, outcome);
}

#[test]
fn every_operation_type_has_a_distinct_discriminator() {
    let names: std::collections::HashSet<&str> =
        OperationType::ALL.iter().map(|op| op.as_str()).collect();
    assert_eq!(names.len(), OperationType::ALL.len());
    assert!(names.contains("bulk_create_quotas"));
    assert!(names.contains("bulk_update_quotas"));
    assert!(names.contains("bulk_deactivate_quotas"));
}
