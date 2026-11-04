use uuid::Uuid;

use super::{BackendError, LogClaim, LogId, Replacing, Stored};

const L3: LogId = LogId::from_uuid(Uuid::from_u128(3));
const L7: LogId = LogId::from_uuid(Uuid::from_u128(7));
const L8: LogId = LogId::from_uuid(Uuid::from_u128(8));

/// `check`'s answer, with any error's variant and detail spelled out:
/// `BackendError` has no `PartialEq`.
fn checked(claim: LogClaim, stored: Stored) -> Result<(), String> {
    claim.check("provision", stored).map_err(|err| match err {
        BackendError::NotProvisioned { detail } => format!("NotProvisioned: {detail}"),
        BackendError::LogConflict { detail } => format!("LogConflict: {detail}"),
        other => format!("unexpected: {other:?}"),
    })
}

#[test]
fn a_fresh_claim_is_accepted_over_nothing() {
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: L7,
    };
    assert_eq!(checked(claim, Stored::Nothing), Ok(()));
}

#[test]
fn a_replacing_claim_is_accepted_over_nothing() {
    let claim = LogClaim {
        replacing: Replacing::Log(L3),
        new: L7,
    };
    assert_eq!(checked(claim, Stored::Nothing), Ok(()));
}

#[test]
fn a_fresh_claim_continued_is_accepted() {
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: L7,
    };
    assert_eq!(checked(claim, Stored::Log(L7)), Ok(()));
}

#[test]
fn a_replacing_claim_continued_is_accepted() {
    let claim = LogClaim {
        replacing: Replacing::Log(L3),
        new: L7,
    };
    assert_eq!(checked(claim, Stored::Log(L7)), Ok(()));
}

#[test]
fn a_held_claim_over_its_own_log_is_accepted() {
    let claim = LogClaim {
        replacing: Replacing::Held,
        new: L7,
    };
    assert_eq!(checked(claim, Stored::Log(L7)), Ok(()));
}

#[test]
fn replacing_the_named_attempt_is_accepted() {
    let claim = LogClaim {
        replacing: Replacing::Log(L7),
        new: L8,
    };
    assert_eq!(checked(claim, Stored::Log(L7)), Ok(()));
}

#[test]
fn a_held_claim_over_nothing_is_not_provisioned() {
    let claim = LogClaim {
        replacing: Replacing::Held,
        new: L7,
    };
    assert_eq!(
        checked(claim, Stored::Nothing),
        Err("NotProvisioned: provision: the topic has no log on this backend".to_owned())
    );
}

#[test]
fn a_fresh_claim_over_a_foreign_log_is_a_conflict() {
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: L7,
    };
    assert_eq!(
        checked(claim, Stored::Log(L3)),
        Err("LogConflict: provision: the backend holds another log id for this topic".to_owned())
    );
}

#[test]
fn a_replacing_claim_over_a_foreign_log_is_a_conflict() {
    let claim = LogClaim {
        replacing: Replacing::Log(L7),
        new: L8,
    };
    assert_eq!(
        checked(claim, Stored::Log(L3)),
        Err("LogConflict: provision: the backend holds another log id for this topic".to_owned())
    );
}

#[test]
fn a_held_claim_over_a_foreign_log_is_a_conflict() {
    let claim = LogClaim {
        replacing: Replacing::Held,
        new: L7,
    };
    assert_eq!(
        checked(claim, Stored::Log(L3)),
        Err("LogConflict: provision: the backend holds another log id for this topic".to_owned())
    );
}

#[test]
fn only_a_held_claim_forbids_creation() {
    assert_eq!(
        [Replacing::Nothing, Replacing::Log(L3), Replacing::Held].map(|replacing| LogClaim {
            replacing,
            new: L7
        }
        .allows_creation()),
        [true, true, false]
    );
}
