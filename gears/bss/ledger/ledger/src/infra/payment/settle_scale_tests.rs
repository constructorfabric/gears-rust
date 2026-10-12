//! The settle FX hook classifies a functional-currency scale failure through the
//! shared [`scale_error_to_domain`] table, the same as every other resolve site.

use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::domain::money::ScaleError;
use crate::infra::posting::error_transport::scale_error_to_domain;

#[test]
fn an_unregistered_functional_currency_is_a_request_error_not_a_500() {
    assert!(matches!(
        scale_error_to_domain(ScaleError::UnknownCurrencyScale("XTS".into())),
        DomainError::InvalidRequest(d) if d.contains("XTS")
    ));
    assert!(matches!(
        scale_error_to_domain(ScaleError::Repo(RepoError::Db("down".into()))),
        DomainError::Internal(_)
    ));
    assert!(matches!(
        scale_error_to_domain(ScaleError::CorruptStoredScale {
            currency: "XTS".into(),
            currency_scale: 40,
        }),
        DomainError::Internal(_)
    ));
}

/// Registry contention is retryable `ConcurrentModification`, as on the posting
/// and approval paths, not an `Internal` fault.
#[test]
fn registry_contention_is_a_concurrent_modification() {
    assert!(matches!(
        scale_error_to_domain(ScaleError::Repo(RepoError::Conflict("busy".into()))),
        DomainError::ConcurrentModification(_)
    ));
}
