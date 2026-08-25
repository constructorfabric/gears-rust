//! The retirement's registry request id.
//!
//! The registry is a cross-tenant service whose idempotency is keyed on exactly
//! this string, so what the id owes is two things at once: the **same** retirement
//! must render the **same** id (or a retry strands a second assignment), and a
//! *different act* over the same subject must render a **different** one. The
//! second half is what review finding Z9-10 is about — this was the one id of the
//! five whose act discriminator was borrowed from a string built in another module.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use uuid::Uuid;

use super::retirement_request_id;
use crate::domain::scope_key::PlanId;
use crate::infra::approval::retirement_unit_ref;
use crate::infra::storage::repo::audit_repo;

const TENANT: Uuid = Uuid::from_u128(0x_7e_11_a2_71);
const PLAN: Uuid = Uuid::from_u128(0x_b1_a2_c3);

/// The real composition, spelled out once so the id is asserted as a **value** and
/// not through the function that builds it.
///
/// `retirement_unit_ref` renders `{plan}/retirement/{revision}`, so the whole id is
/// `retirement/{tenant}/{plan}/retirement/{revision}`. The leading segment is this
/// module's and the inner one is the approval plane's; both are here because the
/// point of the change is that the outer one no longer depends on the inner.
#[test]
fn the_id_leads_with_the_act_and_carries_the_whole_coordinate() {
    let plan = PlanId::new(PLAN);
    let id = retirement_request_id(TENANT, &retirement_unit_ref(plan, 4));
    assert_eq!(
        id,
        format!("retirement/{TENANT}/{PLAN}/retirement/4"),
        "the registry's key is this exact string"
    );
}

/// **The property, armed where the subject cannot supply it.**
///
/// A subject ref that names no act is not hypothetical: `audit_repo`'s
/// `plan_revision_ref` is the plane's other spelling of "this plan at this
/// revision", and it is what a publish, a supersession or a cutover of the same
/// revision is named by. Handed one of those, the old id was
/// `{tenant}/{plan}/{revision}` — a string any of those acts could equally have
/// produced, on a service that treats one id as one request.
#[test]
fn the_act_is_named_by_the_id_itself_and_not_by_the_subject_it_was_handed() {
    let subject = audit_repo::plan_revision_ref(PlanId::new(PLAN), 4);
    assert!(
        !subject.contains("retirement"),
        "the premise of this case: this subject names no act"
    );

    let id = retirement_request_id(TENANT, &subject);
    assert!(
        id.starts_with("retirement/"),
        "the act has to be in the id whatever the subject says: {id}"
    );
    assert_ne!(
        id,
        format!("{TENANT}/{subject}"),
        "and it must not be the act-less string every sibling act over this subject would \
         also render"
    );
}

/// The positive control: the id is **derived**, so a retry re-requests rather than
/// stranding a second assignment.
///
/// Without this, an id that mixed in a fresh `Uuid` would satisfy the case above
/// and break the property the function exists for.
#[test]
fn one_retirement_renders_one_id_however_often_it_is_asked() {
    let plan = PlanId::new(PLAN);
    let first = retirement_request_id(TENANT, &retirement_unit_ref(plan, 4));
    let second = retirement_request_id(TENANT, &retirement_unit_ref(plan, 4));
    assert_eq!(first, second, "a retry of one retirement is one request");
    assert_ne!(
        first,
        retirement_request_id(TENANT, &retirement_unit_ref(plan, 5)),
        "and two revisions are two"
    );
    assert_ne!(
        first,
        retirement_request_id(
            Uuid::from_u128(0x_7e_11_a2_72),
            &retirement_unit_ref(plan, 4)
        ),
        "and two tenants are two, on a cross-tenant service"
    );
}
