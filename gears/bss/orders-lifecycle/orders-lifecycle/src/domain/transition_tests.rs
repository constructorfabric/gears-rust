#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::contributions::tests::facts;
use crate::domain::guards::{GATE_COMPOSITE, GuardRegistry};
use bss_orders_lifecycle_sdk::catalog::OrderState as S;

fn env() -> (StateTable, GuardRegistry) {
    let table = StateTable::registered().unwrap();
    let registry = GuardRegistry::registered(&table).unwrap();
    (table, registry)
}
fn t() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_100).unwrap()
}
fn pass(_: &GuardSubject<'_>) -> GuardVerdict {
    GuardVerdict::Pass
}
/// Bind every guard of `row` to pass, except `fail` which refuses with its first reason.
fn bindings(registry: &GuardRegistry, row: u8, fail: &[&'static str]) -> GuardBindings {
    let mut b = GuardBindings::new();
    for g in registry.row_guards(crate::domain::state_table::RowId::of(row)) {
        if fail.contains(&g.name) {
            let reason = g.reasons[0];
            b = b.bind(g.name, move |_: &GuardSubject<'_>| {
                GuardVerdict::Fail(reason)
            });
        } else {
            b = b.bind(g.name, pass);
        }
    }
    b
}
fn resolved(registry: &GuardRegistry, row: u8) -> Inputs {
    registry
        .row_inputs(crate::domain::state_table::RowId::of(row))
        .into_iter()
        .fold(Inputs::new(), |i, p| i.with(p, InputState::Resolved))
}

#[test]
fn workflow_class_checks_version_before_admissibility() {
    let (table, _) = env();
    // A superseded reflection on an order an amendment moved to `submitted`... at another
    // version: version-conflict, never not-admissible (D-110).
    let mut f = facts(S::Approved);
    f.current_version = 5;
    let r = admit(
        &table,
        Trigger::ReflectApprovalGranted,
        &f,
        4,
        DraftRevisions::NotApplicable,
    )
    .unwrap();
    assert_eq!(
        r.unwrap_err(),
        EngineRefusal::VersionConflict {
            current_version: 5,
            draft_revision: None
        }
    );
    // Same version, wrong state: not-admissible.
    let r = admit(
        &table,
        Trigger::ReflectApprovalGranted,
        &f,
        5,
        DraftRevisions::NotApplicable,
    )
    .unwrap();
    assert_eq!(
        r.unwrap_err(),
        EngineRefusal::NotAdmissible {
            state: S::Approved,
            trigger: Trigger::ReflectApprovalGranted
        }
    );
}

#[test]
fn ordinary_triggers_check_admissibility_before_version() {
    let (table, _) = env();
    let mut f = facts(S::Cancelled);
    f.current_version = 5;
    for trigger in [
        Trigger::Hold,
        Trigger::ForceFailUnreconciled,
        Trigger::Cancel,
    ] {
        let r = admit(&table, trigger, &f, 4, DraftRevisions::NotApplicable).unwrap();
        assert_eq!(
            r.unwrap_err(),
            EngineRefusal::NotAdmissible {
                state: S::Cancelled,
                trigger
            },
            "{trigger:?}"
        );
    }
    let f = facts(S::Submitted);
    let r = admit(&table, Trigger::Hold, &f, 1, DraftRevisions::NotApplicable).unwrap();
    assert_eq!(
        r.unwrap_err(),
        EngineRefusal::VersionConflict {
            current_version: 2,
            draft_revision: None
        }
    );
}

#[test]
fn draft_revisions_are_compared_after_admissibility() {
    let (table, _) = env();
    let draft = facts(S::Draft);
    let ok = |client, prepared| DraftRevisions::Draft { client, prepared };
    // In draft: absent client revision is a conflict naming the current revision (D-147).
    for revisions in [ok(None, 4), ok(Some(3), 4), ok(Some(4), 3)] {
        let r = admit(&table, Trigger::DraftMutate, &draft, 2, revisions).unwrap();
        assert_eq!(
            r.unwrap_err(),
            EngineRefusal::VersionConflict {
                current_version: 2,
                draft_revision: Some(4)
            }
        );
    }
    assert!(
        admit(&table, Trigger::DraftMutate, &draft, 2, ok(Some(4), 4))
            .unwrap()
            .is_ok()
    );
    // Submit with a stale prepared snapshot settles version-conflict, never a stale result.
    let r = admit(&table, Trigger::Submit, &draft, 2, ok(Some(4), 3)).unwrap();
    assert!(matches!(r, Err(EngineRefusal::VersionConflict { .. })));
    // Outside draft, an absent revision is not-admissible first (D-145).
    let submitted = facts(S::Submitted);
    let r = admit(&table, Trigger::DraftMutate, &submitted, 2, ok(None, 4)).unwrap();
    assert!(matches!(r, Err(EngineRefusal::NotAdmissible { .. })));
    // Revisions are carried exactly by draft-mutate and submit.
    assert_eq!(
        admit(&table, Trigger::Cancel, &draft, 2, ok(Some(4), 4)).unwrap_err(),
        DecisionError::DraftRevisions
    );
    assert_eq!(
        admit(
            &table,
            Trigger::Submit,
            &draft,
            2,
            DraftRevisions::NotApplicable
        )
        .unwrap_err(),
        DecisionError::DraftRevisions
    );
}

#[test]
fn guards_run_in_registration_order_and_the_first_failure_wins() {
    let (table, registry) = env();
    let mut held = facts(S::OnHold);
    held.pre_hold_state = Some(S::InFulfillment);
    // Row 27: cancel reason, pre-hold, evidence, cancel window.
    let d = decide(
        &table,
        &registry,
        Trigger::CancelWorkflowMediated,
        &held,
        2,
        DraftRevisions::NotApplicable,
        &bindings(
            &registry,
            27,
            &["seam.compensation-evidence", "seam.cancel-window"],
        ),
        &Inputs::new(),
        t(),
    )
    .unwrap();
    assert_eq!(
        d,
        Decision::Guard {
            guard: "seam.compensation-evidence",
            reason: Reason::CompensationEvidenceMissing,
            assessment: false
        }
    );
    let d = decide(
        &table,
        &registry,
        Trigger::CancelWorkflowMediated,
        &held,
        2,
        DraftRevisions::NotApplicable,
        &bindings(
            &registry,
            27,
            &["cancel.reason-required", "seam.cancel-window"],
        ),
        &Inputs::new(),
        t(),
    )
    .unwrap();
    assert!(matches!(
        d,
        Decision::Guard {
            reason: Reason::CancelReasonRequired,
            ..
        }
    ));
}

#[test]
fn unresolvable_inputs_never_mask_earlier_refusals_and_precluded_is_not_unresolvable() {
    let (table, registry) = env();
    let mut amend = facts(S::Submitted);
    amend.version_allocation_high_water = 3;
    let identity_down = resolved(&registry, 18).with(
        InputPort::Identity,
        InputState::Unresolvable(Reason::IdentityPartyUnavailable),
    );
    // The amendment cap fails on its resolved inputs: it outranks the identity outage.
    let d = decide(
        &table,
        &registry,
        Trigger::Amendment,
        &amend,
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 18, &["versioning.amendment-cap"]),
        &identity_down,
        t(),
    )
    .unwrap();
    assert!(matches!(
        d,
        Decision::Guard {
            reason: Reason::AmendmentCapExhausted,
            ..
        }
    ));
    // Nothing earlier fails: the unevaluable input is the outcome, at the payer guard.
    let d = decide(
        &table,
        &registry,
        Trigger::Amendment,
        &amend,
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 18, &[]),
        &identity_down,
        t(),
    )
    .unwrap();
    assert_eq!(
        d,
        Decision::Unevaluable {
            guard: "versioning.payer-within-seller",
            reason: Reason::IdentityPartyUnavailable,
            assessment: false
        }
    );
    // Engine refusals outrank the unevaluable input too (early-branch precedence).
    let d = decide(
        &table,
        &registry,
        Trigger::Amendment,
        &amend,
        1,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 18, &[]),
        &identity_down,
        t(),
    )
    .unwrap();
    assert!(matches!(
        d,
        Decision::Engine(EngineRefusal::VersionConflict { .. })
    ));
    // Precluded: the cap fails, the gate inputs were skipped: the cap reason, never a 503.
    let precluded = registry
        .row_inputs(crate::domain::state_table::RowId::of(18))
        .into_iter()
        .fold(Inputs::new(), |i, p| i.with(p, InputState::Precluded));
    let d = decide(
        &table,
        &registry,
        Trigger::Amendment,
        &amend,
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 18, &["versioning.amendment-cap"]),
        &precluded,
        t(),
    )
    .unwrap();
    assert!(matches!(
        d,
        Decision::Guard {
            reason: Reason::AmendmentCapExhausted,
            ..
        }
    ));
    // A precluded input reached by evaluation is a contract defect (fail closed).
    assert_eq!(
        decide(
            &table,
            &registry,
            Trigger::Amendment,
            &amend,
            2,
            DraftRevisions::NotApplicable,
            &bindings(&registry, 18, &[]),
            &precluded,
            t(),
        )
        .unwrap_err(),
        DecisionError::PrecludedReached("versioning.payer-within-seller")
    );
    // The gate composite's unevaluable input reaches the assessment.
    let pricing_down = resolved(&registry, 4).with(
        InputPort::PricingRevision,
        InputState::Unresolvable(Reason::PricingRevisionUnavailable),
    );
    let d = decide(
        &table,
        &registry,
        Trigger::Submit,
        &facts(S::Draft),
        2,
        DraftRevisions::Draft {
            client: Some(4),
            prepared: 4,
        },
        &bindings(&registry, 4, &[]),
        &pricing_down,
        t(),
    )
    .unwrap();
    assert_eq!(
        d,
        Decision::Unevaluable {
            guard: GATE_COMPOSITE,
            reason: Reason::PricingRevisionUnavailable,
            assessment: true
        }
    );
}

#[test]
fn bindings_and_inputs_must_match_the_registration_exactly() {
    let (table, registry) = env();
    let f = facts(S::Draft);
    let draft = DraftRevisions::Draft {
        client: Some(4),
        prepared: 4,
    };
    let run = |b: &GuardBindings, i: &Inputs| {
        decide(
            &table,
            &registry,
            Trigger::DraftMutate,
            &f,
            2,
            draft,
            b,
            i,
            t(),
        )
    };
    let complete = bindings(&registry, 2, &[]);
    assert!(matches!(
        run(&complete, &Inputs::new()),
        Ok(Decision::Admitted(_))
    ));
    let mut missing = GuardBindings::new();
    for g in registry
        .row_guards(crate::domain::state_table::RowId::of(2))
        .iter()
        .skip(1)
    {
        missing = missing.bind(g.name, pass);
    }
    assert_eq!(
        run(&missing, &Inputs::new()).unwrap_err(),
        DecisionError::Unbound("capture.draft-line-membership")
    );
    let extra = complete.clone().bind("hold.resume-cap", pass);
    assert_eq!(
        run(&extra, &Inputs::new()).unwrap_err(),
        DecisionError::Unregistered("hold.resume-cap")
    );
    let undeclared = Inputs::new().with(InputPort::Identity, InputState::Resolved);
    assert_eq!(
        run(&complete, &undeclared).unwrap_err(),
        DecisionError::UndeclaredInput(InputPort::Identity)
    );
    // A guard may only return one of its registered reasons.
    let rogue = bindings(&registry, 2, &[]).bind("capture.line-cap", |_: &GuardSubject<'_>| {
        GuardVerdict::Fail(Reason::NoLines)
    });
    assert_eq!(
        run(&rogue, &Inputs::new()).unwrap_err(),
        DecisionError::GuardReason("capture.line-cap")
    );
    // A declared input must be resolved, unresolvable with its own reason, or precluded.
    let d = decide(
        &table,
        &registry,
        Trigger::BeginFulfillment,
        &facts(S::Approved),
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 11, &[]),
        &Inputs::new(),
        t(),
    );
    assert_eq!(
        d.unwrap_err(),
        DecisionError::MissingInput(InputPort::AcceptanceRequirement)
    );
    let d = decide(
        &table,
        &registry,
        Trigger::BeginFulfillment,
        &facts(S::Approved),
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 11, &[]),
        &Inputs::new().with(
            InputPort::AcceptanceRequirement,
            InputState::Unresolvable(Reason::EvaluationUnavailable),
        ),
        t(),
    );
    assert_eq!(
        d.unwrap_err(),
        DecisionError::InputReason(InputPort::AcceptanceRequirement)
    );
}

#[test]
fn resume_cap_precedes_the_stored_target_check() {
    let (table, registry) = env();
    let held = facts(S::OnHold); // no stored pre-hold state
    let d = decide(
        &table,
        &registry,
        Trigger::Resume,
        &held,
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 22, &["hold.resume-cap"]),
        &Inputs::new(),
        t(),
    )
    .unwrap();
    assert!(matches!(
        d,
        Decision::Guard {
            reason: Reason::ResumeCapExhausted,
            ..
        }
    ));
    let d = decide(
        &table,
        &registry,
        Trigger::Resume,
        &held,
        2,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 22, &[]),
        &Inputs::new(),
        t(),
    )
    .unwrap();
    assert_eq!(d, Decision::Engine(EngineRefusal::ResumeTargetMissing));
}

/// Every expanded key: admitted with passing guards; every registered guard can refuse it.
#[test]
fn every_expanded_key_admits_and_every_guard_refuses() {
    let (table, registry) = env();
    for row in table.rows().iter().filter(|r| r.id.number() != 1) {
        for from in row.from.iter().flatten() {
            let mut f = facts(*from);
            if *from == S::OnHold {
                f.pre_hold_state = Some(S::Approved);
            }
            let revisions = if uses_draft_revisions(row.trigger) {
                DraftRevisions::Draft {
                    client: Some(4),
                    prepared: 4,
                }
            } else {
                DraftRevisions::NotApplicable
            };
            let n = row.id.number();
            let run = |b: &GuardBindings| {
                decide(
                    &table,
                    &registry,
                    row.trigger,
                    &f,
                    2,
                    revisions,
                    b,
                    &resolved(&registry, n),
                    t(),
                )
                .unwrap()
            };
            assert_eq!(run(&bindings(&registry, n, &[])), Decision::Admitted(row));
            for guard in registry.row_guards(row.id) {
                match run(&bindings(&registry, n, &[guard.name])) {
                    Decision::Guard {
                        guard: g, reason, ..
                    } => {
                        assert_eq!(g, guard.name);
                        assert_eq!(reason, guard.reasons[0]);
                    }
                    other => panic!("{} {}: {other:?}", row.id, guard.name),
                }
            }
        }
    }
}

#[test]
fn administrative_changes_reach_only_the_administrative_edit_row() {
    use crate::domain::audit::{AdminAttribute, AdminChange, AdminField};
    let (table, registry) = env();
    let change = [AdminChange {
        field: AdminField::Order(AdminAttribute::ExternalReference),
        prior: None,
        new: Some("PO".into()),
    }];
    let facts = facts(S::Approved);
    let err = decide_with(
        &table,
        &registry,
        Trigger::Hold,
        &facts,
        facts.current_version,
        DraftRevisions::NotApplicable,
        &bindings(&registry, 21, &[]),
        &resolved(&registry, 21),
        t(),
        &change,
    )
    .unwrap_err();
    assert_eq!(err, DecisionError::Administrative);
    // Row 3's change guard sees exactly the reconciled changes.
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
    let probe = std::sync::Arc::clone(&seen);
    let b = bindings(&registry, 3, &[]).bind(
        "versioning.admin-changed",
        move |s: &GuardSubject<'_>| {
            probe.store(s.administrative.len(), std::sync::atomic::Ordering::SeqCst);
            GuardVerdict::Pass
        },
    );
    let d = decide_with(
        &table,
        &registry,
        Trigger::AdministrativeEdit,
        &facts,
        facts.current_version,
        DraftRevisions::NotApplicable,
        &b,
        &resolved(&registry, 3),
        t(),
        &change,
    )
    .unwrap();
    assert!(matches!(d, Decision::Admitted(_)));
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn only_draft_edits_and_amendments_propose_axis_changes() {
    use bss_orders_lifecycle_sdk::catalog::Trigger as T;
    assert!(axis_change_admitted(T::DraftMutate, true, false));
    assert!(axis_change_admitted(T::DraftMutate, false, true));
    assert!(axis_change_admitted(T::DraftMutate, true, true));
    assert!(axis_change_admitted(T::Amendment, false, true));
    // The resource axis is frozen from submit; an empty delta is not a proposal.
    assert!(!axis_change_admitted(T::Amendment, true, true));
    assert!(!axis_change_admitted(T::Amendment, true, false));
    assert!(!axis_change_admitted(T::DraftMutate, false, false));
    for trigger in [
        T::Create,
        T::AdministrativeEdit,
        T::Submit,
        T::Hold,
        T::Resume,
        T::Cancel,
    ] {
        assert!(!axis_change_admitted(trigger, false, true), "{trigger:?}");
    }
}
