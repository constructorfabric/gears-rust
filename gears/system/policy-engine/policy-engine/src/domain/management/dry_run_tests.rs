#![allow(clippy::unwrap_used, clippy::expect_used)]

//! A dry run answers what its operation answers on the same state - success,
//! the error and its masking alike - and writes nothing. The checks
//! themselves are covered through the operations (`assignments_tests`,
//! `service_tests`); these tests cover the dispatch, the parity and the
//! absence of writes.

use policy_engine_sdk::gts::permissions::actions;
use policy_engine_sdk::management::{
    self as sdk, AssignmentSpec, DocumentSpec, ManagementError, PublishOperation, VersionContent,
};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

use super::*;
use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::governance::{
    Harness, TENANT_A, TENANT_A1, TENANT_S, TENANT_S1, alice, bob, harness, sam, spec,
};
use crate::domain::management::test_support::{PdpMode, ctx};
use crate::domain::model::{AssignmentId, VersionState};
use crate::domain::repos::AssignmentRepository;
use crate::infra::storage::content_repo::OrmAssignmentRepository;

/// Runs `operation` for real, its answer reduced to success or the error.
async fn run(
    h: &Harness,
    caller: &SecurityContext,
    operation: PublishOperation,
) -> Result<(), ManagementError> {
    match operation {
        PublishOperation::Activate {
            bundle_id,
            version_id,
        } => h
            .service
            .activate_version(caller, bundle_id, version_id)
            .await
            .map(drop),
        PublishOperation::Assign(spec) => h.service.assign(caller, spec).await.map(drop),
        PublishOperation::UpdateAssignment { assignment_id } => h
            .service
            .update_assignment(caller, assignment_id, false)
            .await
            .map(drop),
        PublishOperation::Unassign { assignment_id } => {
            h.service.unassign(caller, assignment_id).await
        }
    }
}

fn activate(bundle: &sdk::Bundle, version: &sdk::BundleVersion) -> PublishOperation {
    PublishOperation::Activate {
        bundle_id: bundle.id,
        version_id: version.id,
    }
}

/// One scenario per kind of outcome, built on a fresh harness.
#[derive(Debug, Clone, Copy)]
enum Case {
    ActivateDraft,
    ActivateInvalid,
    ActivateActive,
    ActivateSuperseded,
    ActivateOutOfSight,
    ActivateWithoutPublish,
    ActivatePdpDown,
    AssignReachable,
    AssignBehindBarrier,
    AssignExisting,
    AssignUnderGrantOnTheBundleAlone,
    AssignExistingUnderGrantOnTheBundleAlone,
    AssignBundleOutsideThePublishGrant,
    UpdateBehindBarrier,
    UnassignAbsent,
    UnassignOwnerBehindBarrier,
    UnassignDormantByTargetAdmin,
    UnassignDormantUncovered,
}

async fn scenario(case: Case) -> (Harness, SecurityContext, PublishOperation) {
    let h = harness().await;
    let (caller, operation) = match case {
        Case::ActivateDraft
        | Case::ActivateInvalid
        | Case::ActivateActive
        | Case::ActivateSuperseded
        | Case::ActivateOutOfSight
        | Case::ActivateWithoutPublish
        | Case::ActivatePdpDown => activation(&h, case).await,
        _ => assignment(&h, case).await,
    };
    (h, caller, operation)
}

async fn activation(h: &Harness, case: Case) -> (SecurityContext, PublishOperation) {
    let a = alice();
    match case {
        Case::ActivateDraft => {
            let (bundle, draft) = h.draft(&a, &["deny_delete"]).await;
            (a, activate(&bundle, &draft))
        }
        Case::ActivateInvalid => {
            let (bundle, draft) = h.draft(&a, &["deny_delete"]).await;
            let broken = DocumentSpec {
                content: "package p\n\ndeny if {".to_owned(),
                ..spec("broken")
            };
            let content = VersionContent {
                documents: vec![broken],
            };
            h.service
                .replace_draft_content(&a, bundle.id, draft.id, content)
                .await
                .unwrap();
            (a, activate(&bundle, &draft))
        }
        Case::ActivateActive => {
            let (bundle, active) = h.active(&a, &["deny_delete"]).await;
            (a, activate(&bundle, &active))
        }
        Case::ActivateSuperseded => {
            let (bundle, first) = h.active(&a, &["deny_delete"]).await;
            let second = h
                .service
                .create_draft_version(&a, bundle.id, Some(first.id))
                .await
                .unwrap();
            h.service
                .activate_version(&a, bundle.id, second.id)
                .await
                .unwrap();
            (a, activate(&bundle, &first))
        }
        Case::ActivateOutOfSight => {
            let (bundle, draft) = h.draft(&a, &["deny_delete"]).await;
            (bob(), activate(&bundle, &draft))
        }
        Case::ActivateWithoutPublish => {
            let (bundle, draft) = h.draft(&a, &["deny_delete"]).await;
            h.pdp.set_mode(PdpMode::Allow(Some(
                [actions::READ.to_owned(), actions::AUTHOR.to_owned()]
                    .into_iter()
                    .collect(),
            )));
            (a, activate(&bundle, &draft))
        }
        Case::ActivatePdpDown => {
            let (bundle, draft) = h.draft(&a, &["deny_delete"]).await;
            h.pdp.set_mode(PdpMode::Fail);
            (a, activate(&bundle, &draft))
        }
        _ => unreachable!("not an activation case"),
    }
}

async fn assignment(h: &Harness, case: Case) -> (SecurityContext, PublishOperation) {
    let a = alice();
    match case {
        Case::AssignReachable => {
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            let spec = AssignmentSpec::new(bundle.id, TENANT_A1);
            (a, PublishOperation::Assign(spec))
        }
        Case::AssignBehindBarrier => {
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            let spec = AssignmentSpec::new(bundle.id, TENANT_S);
            (a, PublishOperation::Assign(spec))
        }
        Case::AssignExisting => {
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            let spec = AssignmentSpec::new(bundle.id, TENANT_A1);
            h.service.assign(&a, spec.clone()).await.unwrap();
            (a, PublishOperation::Assign(spec.shadow()))
        }
        Case::AssignUnderGrantOnTheBundleAlone | Case::AssignExistingUnderGrantOnTheBundleAlone => {
            // A grant scoped to the bundle's id admits the bundle, never the
            // new assignment row (a fresh id).
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            if matches!(case, Case::AssignExistingUnderGrantOnTheBundleAlone) {
                h.stored_by(bundle.id, TENANT_A, TENANT_A1).await;
            }
            h.pdp.set_mode(PdpMode::AllowResources(vec![bundle.id.0]));
            let spec = AssignmentSpec::new(bundle.id, TENANT_A1);
            (a, PublishOperation::Assign(spec))
        }
        Case::AssignBundleOutsideThePublishGrant => {
            // Alice reads A1's bundle, but publishes on A's content only.
            let a1_admin = ctx(Uuid::from_u128(0xA1A), TENANT_A1);
            let (bundle, _) = h.active(&a1_admin, &["deny_delete"]).await;
            h.pdp
                .set_mode(PdpMode::Administers(vec![TENANT_A, TENANT_A1]));
            let spec = AssignmentSpec::new(bundle.id, TENANT_A1);
            (a, PublishOperation::Assign(spec))
        }
        Case::UpdateBehindBarrier => {
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            let behind = h.stored_by(bundle.id, TENANT_A, TENANT_S).await;
            let update = PublishOperation::UpdateAssignment {
                assignment_id: behind.id,
            };
            (a, update)
        }
        Case::UnassignAbsent => {
            let unassign = PublishOperation::Unassign {
                assignment_id: AssignmentId(Uuid::new_v4()),
            };
            (a, unassign)
        }
        Case::UnassignOwnerBehindBarrier | Case::UnassignDormantByTargetAdmin => {
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            let dormant = h.stored_by(bundle.id, TENANT_A, TENANT_S).await;
            let caller = match case {
                Case::UnassignOwnerBehindBarrier => a,
                _ => sam(),
            };
            let unassign = PublishOperation::Unassign {
                assignment_id: dormant.id,
            };
            (caller, unassign)
        }
        Case::UnassignDormantUncovered => {
            // S's administrator reaches S1 but holds publish on S only.
            let (bundle, _) = h.active(&a, &["deny_delete"]).await;
            let dormant = h.stored_by(bundle.id, TENANT_A, TENANT_S1).await;
            let unassign = PublishOperation::Unassign {
                assignment_id: dormant.id,
            };
            (sam(), unassign)
        }
        _ => unreachable!("not an assignment case"),
    }
}

#[tokio::test]
async fn dry_runs_answer_what_the_operations_answer() {
    let cases = [
        (Case::ActivateDraft, None),
        (Case::ActivateInvalid, Some("VALIDATION_FAILED")),
        (Case::ActivateActive, None),
        (Case::ActivateSuperseded, Some("VERSION_NOT_DRAFT")),
        (Case::ActivateOutOfSight, Some("NOT_FOUND")),
        (Case::ActivateWithoutPublish, Some("CAPABILITY_DENIED")),
        (Case::ActivatePdpDown, Some("UNAVAILABLE")),
        (Case::AssignReachable, None),
        (Case::AssignBehindBarrier, Some("TENANT_BOUNDARY")),
        (Case::AssignExisting, Some("ASSIGNMENT_EXISTS")),
        (
            Case::AssignUnderGrantOnTheBundleAlone,
            Some("CAPABILITY_DENIED"),
        ),
        (
            Case::AssignExistingUnderGrantOnTheBundleAlone,
            Some("CAPABILITY_DENIED"),
        ),
        (
            Case::AssignBundleOutsideThePublishGrant,
            Some("CAPABILITY_DENIED"),
        ),
        (Case::UpdateBehindBarrier, Some("TENANT_BOUNDARY")),
        (Case::UnassignAbsent, None),
        (Case::UnassignOwnerBehindBarrier, Some("TENANT_BOUNDARY")),
        (Case::UnassignDormantByTargetAdmin, None),
        (Case::UnassignDormantUncovered, Some("CAPABILITY_DENIED")),
    ];
    for (case, expected) in cases {
        let (h, caller, operation) = scenario(case).await;
        let dry = h.service.check_publish(&caller, operation.clone()).await;
        let real = run(&h, &caller, operation).await;
        assert_eq!(
            dry, real,
            "{case:?}: the dry run answers what the operation does"
        );
        assert_eq!(
            dry.as_ref().err().and_then(reason_of).as_deref(),
            expected,
            "{case:?}: {dry:?}"
        );
    }
}

#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let h = harness().await;
    let a = alice();
    let (drafted, draft) = h.draft(&a, &["deny_delete"]).await;
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let assigned = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();
    let dormant = h.stored_by(bundle.id, TENANT_A, TENANT_S).await;
    let assignments = || async {
        let conn = h.db.conn().unwrap();
        OrmAssignmentRepository
            .list_for_bundle(&conn, &AccessScope::allow_all(), bundle.id)
            .await
            .unwrap()
    };
    let before = assignments().await;

    // Each would succeed.
    for (caller, operation) in [
        (alice(), activate(&drafted, &draft)),
        (
            alice(),
            PublishOperation::Assign(AssignmentSpec::new(bundle.id, TENANT_A)),
        ),
        (
            alice(),
            PublishOperation::UpdateAssignment {
                assignment_id: assigned.id,
            },
        ),
        (
            alice(),
            PublishOperation::Unassign {
                assignment_id: assigned.id,
            },
        ),
        (
            sam(),
            PublishOperation::Unassign {
                assignment_id: dormant.id,
            },
        ),
    ] {
        h.service.check_publish(&caller, operation).await.unwrap();
    }

    assert_eq!(h.stored_state(draft.id).await, VersionState::Draft);
    assert_eq!(
        h.service
            .get_bundle(&a, drafted.id)
            .await
            .unwrap()
            .active_version_id,
        None
    );
    assert_eq!(
        assignments().await,
        before,
        "no row added, changed or removed"
    );
}

#[tokio::test]
async fn an_assignment_withdrawn_concurrently_is_withdrawn_not_refused() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let race = || {
        let assigned = h.stored_by(bundle.id, TENANT_A, TENANT_A1);
        async {
            let assigned = assigned.await;
            // Another caller withdraws it while this one is being authorised.
            let db = h.db.clone();
            let id = assigned.id;
            *h.pdp.before_next.lock() = Some(Box::pin(async move {
                let conn = db.conn().unwrap();
                OrmAssignmentRepository
                    .delete(&conn, &AccessScope::allow_all(), id)
                    .await
                    .unwrap();
            }));
            id
        }
    };

    let id = race().await;
    h.service.unassign(&a, id).await.unwrap();
    let id = race().await;
    let unassign = PublishOperation::Unassign { assignment_id: id };
    h.service.check_publish(&a, unassign).await.unwrap();
}

#[tokio::test]
async fn a_passing_dry_run_reserves_nothing() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let spec = AssignmentSpec::new(bundle.id, TENANT_A1);
    h.service
        .check_publish(&a, PublishOperation::Assign(spec.clone()))
        .await
        .unwrap();

    // Another administrator assigns the pair first: the operation still
    // checks, and refuses.
    h.stored_by(bundle.id, TENANT_A, TENANT_A1).await;
    assert_eq!(
        h.service.assign(&a, spec).await.unwrap_err(),
        ManagementError::AssignmentExists {
            bundle_id: bundle.id,
            tenant_id: TENANT_A1,
        }
    );
}
