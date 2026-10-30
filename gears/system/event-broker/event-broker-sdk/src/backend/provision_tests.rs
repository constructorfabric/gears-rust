use uuid::Uuid;

use super::{LogClaim, LogId, Replacing};

const L3: LogId = LogId::from_uuid(Uuid::from_u128(3));
const L7: LogId = LogId::from_uuid(Uuid::from_u128(7));
const L8: LogId = LogId::from_uuid(Uuid::from_u128(8));

#[test]
fn a_fresh_claim_is_accepted_over_nothing() {
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: L7,
    };
    assert!(claim.accepts(None));
}

#[test]
fn the_same_attempt_continued_is_accepted() {
    let claim = LogClaim {
        replacing: Replacing::Log(L7),
        new: L7,
    };
    assert!(claim.accepts(Some(L7)));
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: L7,
    };
    assert!(claim.accepts(Some(L7)));
}

#[test]
fn replacing_the_named_attempt_is_accepted() {
    let claim = LogClaim {
        replacing: Replacing::Log(L7),
        new: L8,
    };
    assert!(claim.accepts(Some(L7)));
}

#[test]
fn a_foreign_log_id_is_refused() {
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: L7,
    };
    assert!(!claim.accepts(Some(L3)));
    let claim = LogClaim {
        replacing: Replacing::Log(L7),
        new: L8,
    };
    assert!(!claim.accepts(Some(L3)));
}
