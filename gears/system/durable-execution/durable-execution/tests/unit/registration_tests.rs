use super::*;
use proptest::prelude::*;
#[test]
fn commands_check_revision_and_state_before_mutation() {
    let contract = crate::infra::storage::repository::tests::definition().contract();
    let mut state = Definition::new(contract);
    assert!(state.unregister(1, UnregisterMode::Retain).is_err());
    assert_eq!(state.view().state, RegistrationState::Active);
    assert!(state.release().is_err());
    state
        .unregister(0, UnregisterMode::CancelAndRelease)
        .unwrap();
    assert!(state.binding_generation().is_err());
    assert!(state.unregister(1, UnregisterMode::Retain).is_err());
    state.release().unwrap();
    assert!(!state.permits(0));
    let generation = state.binding_generation().unwrap();
    assert_eq!(generation, 1);
    state
        .unregister(state.revision, UnregisterMode::CancelAndRelease)
        .unwrap();
    assert_eq!(state.state, RegistrationState::Released);
    state.activate(state.revision).unwrap();
    assert!(state.permits(1));
    assert!(!state.permits(0));
}
proptest! {
    #[test]
    fn arbitrary_commands_never_reopen_a_revoked_generation(commands in prop::collection::vec(0u8..4,0..80)) {
        let mut state=Definition::new(crate::infra::storage::repository::tests::definition().contract());
        let mut revoked=None;
        for command in commands {
            let before=state.revision;
            let result=match command {
                0=>state.unregister(before,UnregisterMode::Retain),
                1=>state.unregister(before,UnregisterMode::CancelAndRelease),
                2=>state.release(),
                _=>state.activate(before),
            };
            prop_assert_eq!(state.revision,before+u64::from(result.is_ok()));
            if let Some(last)=revoked {
                prop_assert!(state.revoked_through.is_some_and(|next|next>=last));
                prop_assert!(!state.permits(last));
            }
            revoked=state.revoked_through;
            let decoded:Definition=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
            prop_assert_eq!(decoded.view(),state.view());
        }
    }
}
