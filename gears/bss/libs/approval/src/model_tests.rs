use super::{UnitState, Verdict};

/// Every stored state and verdict name reads back as itself; an unknown name and a name in
/// another case read as `None`. The doors parse a request's `state` filter with the same function.
#[test]
fn every_state_and_verdict_round_trips_through_its_stored_name() {
    let states = [
        UnitState::Pending,
        UnitState::Approved,
        UnitState::Rejected,
        UnitState::Withdrawn,
    ];
    assert_eq!(
        states.map(UnitState::as_str),
        ["pending", "approved", "rejected", "withdrawn"]
    );
    for state in states {
        assert_eq!(UnitState::parse(state.as_str()), Some(state));
    }
    for unknown in ["", "bogus", "Pending", "PENDING", " pending"] {
        assert_eq!(UnitState::parse(unknown), None, "{unknown:?}");
    }
    let verdicts = [Verdict::Approve, Verdict::Reject];
    assert_eq!(verdicts.map(Verdict::as_str), ["approve", "reject"]);
    for verdict in verdicts {
        assert_eq!(Verdict::parse(verdict.as_str()), Some(verdict));
    }
    for unknown in ["", "maybe", "Approve", "REJECT"] {
        assert_eq!(Verdict::parse(unknown), None, "{unknown:?}");
    }
}
