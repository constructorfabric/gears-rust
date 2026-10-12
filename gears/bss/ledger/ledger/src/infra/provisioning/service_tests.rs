//! The provisioning transaction keeps business rejections across its boundary.

use super::repo_to_db;
use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::infra::posting::error_transport::decode_business_error;

fn round_trip(error: RepoError) -> DomainError {
    decode_business_error(&repo_to_db(error))
}

#[test]
fn a_scale_lock_and_a_concurrent_registration_keep_their_meaning() {
    assert!(matches!(
        round_trip(RepoError::CurrencyScaleLocked("XTS".into())),
        DomainError::CurrencyScaleLocked(_)
    ));
    assert!(matches!(
        round_trip(RepoError::Conflict("concurrent grain insertion".into())),
        DomainError::ConcurrentModification(_)
    ));
}

#[test]
fn scale_out_of_range_keeps_its_flow_contract_and_the_rest_is_internal() {
    match round_trip(RepoError::ScaleOutOfRange("XTS".into())) {
        DomainError::ScaleOutOfRange(detail) => {
            assert!(detail.contains("XTS"), "the refusal names the currency");
        }
        _ => panic!("expected ScaleOutOfRange"),
    }
    assert!(matches!(
        round_trip(RepoError::Db("connection reset".into())),
        DomainError::Internal(_)
    ));
}
