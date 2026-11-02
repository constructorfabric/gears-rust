use uuid::Uuid;

use super::{BindingState, FailedBinding, PendingBinding, shard_of};

#[test]
fn a_shard_is_stable_and_in_range() {
    for n in 0..200_u128 {
        let fact = Uuid::from_u128(n.wrapping_mul(0x9e37_79b9_7f4a_7c15_f39c_c060_5ced_c834));
        let shard = shard_of(fact, 16);
        assert!((0..16).contains(&shard));
        assert_eq!(shard, shard_of(fact, 16));
    }
    assert_eq!(
        shard_of(Uuid::from_u128(u128::MAX), 0),
        0,
        "zero shards act as one"
    );
}

#[test]
fn a_binding_state_reads_back_from_its_columns() {
    for state in [
        BindingState::Bound,
        BindingState::Pending(PendingBinding::DerivedDeclarationUnavailable),
        BindingState::Failed(FailedBinding::DerivedDeclarationConflict),
    ] {
        assert_eq!(
            BindingState::parse(state.state(), state.reason()),
            Some(state)
        );
    }
    assert_eq!(
        BindingState::parse("bound", Some("derived_declaration_unavailable")),
        None
    );
    assert_eq!(BindingState::parse("pending", None), None);
}
