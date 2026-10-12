//! Usage-type and meter checks.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::meter_pair_complete;
use crate::domain::error::DomainError;

#[test]
fn the_meter_pair_travels_together_or_not_at_all() {
    meter_pair_complete(None, None).expect("no declaration is a complete non-declaration");
    meter_pair_complete(Some("gib_month"), Some("usage:storage")).expect("the whole pair");
    let missing_usage =
        meter_pair_complete(Some("gib_month"), None).expect_err("half a declaration is refused");
    assert_eq!(missing_usage.code(), "METER_DECLARATION_INCOMPLETE");
    assert!(
        matches!(missing_usage, DomainError::MeterDeclarationIncomplete(ref d) if d.contains("without usage_type_ref")),
        "got {missing_usage}"
    );
    let missing_unit = meter_pair_complete(None, Some("usage:storage"))
        .expect_err("the other half is refused the same way");
    assert!(
        matches!(missing_unit, DomainError::MeterDeclarationIncomplete(ref d) if d.contains("without metering_unit")),
        "got {missing_unit}"
    );
}
