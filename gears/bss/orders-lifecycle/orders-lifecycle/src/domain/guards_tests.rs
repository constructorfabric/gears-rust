#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn registry() -> (StateTable, GuardRegistry) {
    let table = StateTable::registered().unwrap();
    let registry = GuardRegistry::registered(&table).unwrap();
    (table, registry)
}
fn names(registry: &GuardRegistry, row: u8) -> Vec<&'static str> {
    registry
        .row_guards(RowId::of(row))
        .iter()
        .map(|g| g.name)
        .collect()
}

/// The registration (precedence) order of every row, as the feature contracts state it.
#[test]
fn every_row_has_its_contract_guard_order() {
    let (_, r) = registry();
    let structural = [
        "capture.category-admitted",
        "capture.currency-consistent",
        "capture.line-cap",
    ];
    let gate_tail = [
        "gate.binding-admission",
        "capture.date-basis",
        GATE_COMPOSITE,
    ];
    let expect: Vec<(u8, Vec<&str>)> = vec![
        (1, vec!["capture.category-admitted"]),
        (
            2,
            vec![
                "capture.draft-line-membership",
                "capture.mixed-field-classes",
                "capture.seller-fixed",
                "capture.category-admitted",
                "capture.currency-consistent",
                "capture.line-cap",
            ],
        ),
        (
            3,
            vec![
                "versioning.admin-line-membership",
                "versioning.admin-field-classification",
                "versioning.admin-changed",
            ],
        ),
        (
            4,
            [
                vec!["gate.has-lines"],
                structural.to_vec(),
                gate_tail.to_vec(),
            ]
            .concat(),
        ),
        (5, vec!["cancel.reason-required"]),
        (6, vec!["expiry.candidate-fresh", "expiry.due"]),
        (7, vec!["seam.verdict-authority"]),
        (8, vec!["seam.verdict-authority"]),
        (9, vec!["seam.verdict-authority"]),
        (10, vec!["seam.verdict-authority", "seam.denial-reason"]),
        (
            11,
            vec![
                "preconditions.acceptance-recorded",
                "preconditions.payment-authorization",
                "gate.binding-deadline",
            ],
        ),
        (12, vec!["seam.spawn-signal-null"]),
        (
            13,
            vec![
                "seam.acknowledgement-lines",
                "seam.acknowledgement-subscriptions",
            ],
        ),
        (
            14,
            vec!["seam.failure-reason", "seam.compensation-evidence"],
        ),
        (15, vec!["cancel.reason-required", "seam.cancel-window"]),
        (
            16,
            vec![
                "cancel.reason-required",
                "seam.compensation-evidence",
                "seam.cancel-window",
            ],
        ),
        (17, vec!["cancel.reason-required"]),
        (21, vec![]),
        (22, vec!["hold.resume-cap"]),
        (23, vec!["cancel.reason-required", "seam.cancel-window"]),
        (
            24,
            vec![
                "expiry.candidate-fresh",
                "expiry.due",
                "expiry.prehold-exempt",
            ],
        ),
        (
            25,
            vec![
                "preconditions.acceptance-not-recorded",
                "preconditions.recording-party",
            ],
        ),
        (
            26,
            vec![
                "seam.failure-reason",
                "seam.compensation-evidence",
                "seam.acknowledge-prehold",
            ],
        ),
        (
            27,
            vec![
                "cancel.reason-required",
                "seam.workflow-cancel-prehold",
                "seam.compensation-evidence",
                "seam.cancel-window",
            ],
        ),
        (
            28,
            vec![
                "force.reason-required",
                "force.spawn-recorded",
                "force.overdue-window",
                "force.second-approver",
            ],
        ),
        (
            29,
            vec![
                "force.reason-required",
                "force.prehold",
                "force.spawn-recorded",
                "force.overdue-window",
                "force.second-approver",
            ],
        ),
    ];
    let amendment = [
        vec![
            "versioning.amendment-cap",
            "versioning.amendment-nonempty",
            "versioning.amendment-reason",
            "versioning.no-administrative-field",
            "versioning.no-frozen-field",
            "versioning.payer-within-seller",
        ],
        structural.to_vec(),
        gate_tail.to_vec(),
    ]
    .concat();
    for (row, expected) in expect {
        assert_eq!(names(&r, row), expected, "row {row}");
    }
    for row in [18, 19, 20] {
        assert_eq!(names(&r, row), amendment, "row {row}");
    }
}

#[test]
fn every_reason_has_one_owner_and_inputs_are_declared_per_row() {
    let (table, r) = registry();
    // Every row's inputs come only from its guards; the gate carries the commercial ports.
    assert!(r.row_inputs(RowId::of(1)).is_empty());
    assert!(r.row_inputs(RowId::of(2)).is_empty());
    assert_eq!(r.row_inputs(RowId::of(4)).len(), 8);
    assert_eq!(
        r.row_inputs(RowId::of(11)),
        [InputPort::AcceptanceRequirement].into_iter().collect()
    );
    // Guards never return engine reasons.
    for row in table.rows() {
        for guard in r.row_guards(row.id) {
            assert!(guard.reasons.iter().all(|x| !ENGINE_REASONS.contains(x)));
        }
    }
}

fn compile_with(f: impl FnOnce(&mut Vec<GuardDecl>)) -> Result<GuardRegistry, GuardRegistryError> {
    let table = StateTable::registered().unwrap();
    let mut decls = GUARDS.to_vec();
    f(&mut decls);
    GuardRegistry::compile(&table, &decls)
}

#[test]
fn invalid_registrations_fail_startup() {
    assert_eq!(
        compile_with(|d| d[0].rows = &[30]).unwrap_err(),
        GuardRegistryError::UnknownRow("capture.draft-line-membership", 30)
    );
    assert_eq!(
        compile_with(|d| d[0].reasons = &["line-gone"]).unwrap_err(),
        GuardRegistryError::UnknownReason("capture.draft-line-membership", "line-gone")
    );
    assert_eq!(
        compile_with(|d| d[0].reasons = &["version-conflict"]).unwrap_err(),
        GuardRegistryError::EngineReason("capture.draft-line-membership", Reason::VersionConflict)
    );
    assert_eq!(
        compile_with(|d| d[0].reasons = &[]).unwrap_err(),
        GuardRegistryError::NoReason("capture.draft-line-membership")
    );
    assert_eq!(
        compile_with(|d| d[1].name = d[0].name).unwrap_err(),
        GuardRegistryError::DuplicateName("capture.draft-line-membership")
    );
    assert_eq!(
        compile_with(|d| d[0].rows = &[2, 2]).unwrap_err(),
        GuardRegistryError::DuplicateRow("capture.draft-line-membership", 2)
    );
    assert_eq!(
        compile_with(|d| d[0].rows = &[1, 2]).unwrap_err(),
        GuardRegistryError::CreateGuard("capture.draft-line-membership")
    );
    // A reason no registration owns: drop the only owner of `no-lines`.
    assert_eq!(
        compile_with(|d| d.retain(|g| g.name != "gate.has-lines")).unwrap_err(),
        GuardRegistryError::UnownedReason(Reason::NoLines)
    );
    // The composite gate must stay the last guard of its rows.
    assert_eq!(
        compile_with(|d| {
            let gate = d.iter().position(|g| g.name == GATE_COMPOSITE).unwrap();
            let gate = d.remove(gate);
            d.insert(0, gate);
        })
        .unwrap_err(),
        GuardRegistryError::GateNotLast(4)
    );
}
