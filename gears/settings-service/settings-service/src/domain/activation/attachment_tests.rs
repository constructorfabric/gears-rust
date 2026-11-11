//! Report visibility and grouping over the in-memory tenant tree.

use time::OffsetDateTime;
use uuid::Uuid;

use super::attach;
use crate::audit::{ActorClassification, AuditOperation, AuditOutcome, StoredAuditRecord};
use crate::test_support::Tree;

fn report(id: u128, tenant: Uuid, change: Uuid, at: i64) -> StoredAuditRecord {
    StoredAuditRecord {
        id: Uuid::from_u128(id),
        declaration_key: "k".to_owned(),
        tenant_id: Some(tenant),
        operation: AuditOperation::Activation,
        actor: "gear".to_owned(),
        actor_classification: ActorClassification::Public,
        actor_tenant_id: None,
        pre_image: None,
        post_image: None,
        outcome: AuditOutcome::Success,
        request_id: "r".to_owned(),
        change_set_id: None,
        occurred_at: OffsetDateTime::from_unix_timestamp(1_800_000_000 + at).expect("time"),
        retain_until: None,
        change_ref: Some(change),
        detail: None,
        instance_id: None,
        instance_label: None,
    }
}

#[tokio::test]
async fn the_chain_of_a_tenant_is_asked_once_however_many_reports_it_made() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    let change = Uuid::from_u128(1);
    let reports = vec![
        report(10, tree.b, change, 1),
        report(11, tree.b, change, 2),
        report(12, tree.b, change, 3),
        report(13, tree.a, change, 4),
    ];
    let kept = attach(&hierarchy, tree.a, tree.a, reports)
        .await
        .expect("attached");
    assert_eq!(kept.of(change).len(), 4);
    assert_eq!(hierarchy.chain_calls(), 1, "`b` once; `a` is the target");
}

#[tokio::test]
async fn reports_outside_the_callers_subtree_or_below_a_standalone_tenant_are_dropped() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    let below = Uuid::new_v4();
    hierarchy.add_tenant(below, tree.s);
    let unknown = Uuid::new_v4();
    let change = Uuid::from_u128(1);
    let reports = vec![
        report(10, tree.c, change, 1),
        report(11, tree.s, change, 2),
        report(12, below, change, 3),
        report(13, tree.root, change, 4),
        report(14, unknown, change, 5),
        report(15, tree.b, change, 6),
    ];
    let kept = attach(&hierarchy, tree.a, tree.a, reports)
        .await
        .expect("attached");
    let ids: Vec<u128> = kept.of(change).iter().map(|r| r.id.as_u128()).collect();
    assert_eq!(ids, [15], "only `b`, a plain descendant");
}

#[tokio::test]
async fn a_tenant_the_resolver_forgets_between_its_two_questions_drops_the_report() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    // `b` is on a chain the resolver answered, then is unknown to the
    // standalone question: dropped like an unknown chain, never an error.
    hierarchy.forget_for_standalone(tree.b);
    let change = Uuid::from_u128(1);
    let kept = attach(
        &hierarchy,
        tree.a,
        tree.a,
        vec![report(10, tree.b, change, 1), report(11, tree.a, change, 2)],
    )
    .await
    .expect("attached");
    let ids: Vec<u128> = kept.of(change).iter().map(|r| r.id.as_u128()).collect();
    assert_eq!(ids, [11]);
}

#[tokio::test]
async fn a_resolver_failing_for_one_tenants_chain_fails_the_attachment_only_for_it() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    hierarchy.fail_chain_for(tree.b);
    let change = Uuid::from_u128(1);
    let refused = attach(
        &hierarchy,
        tree.a,
        tree.a,
        vec![report(10, tree.b, change, 1)],
    )
    .await;
    assert!(
        matches!(
            refused,
            Err(crate::domain::error::DomainError::Unavailable { .. })
        ),
        "{refused:?}"
    );
    assert_eq!(hierarchy.chain_calls(), 1, "the chain of `b`, asked once");
    let at_target = attach(
        &hierarchy,
        tree.a,
        tree.a,
        vec![report(11, tree.a, change, 2)],
    )
    .await
    .expect("the target's own report asks nothing");
    assert_eq!(at_target.of(change).len(), 1);
}

#[tokio::test]
async fn a_resolver_that_cannot_answer_fails_the_attachment() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    hierarchy.set_unavailable(true);
    let refused = attach(
        &hierarchy,
        tree.a,
        tree.a,
        vec![report(10, tree.b, Uuid::from_u128(1), 1)],
    )
    .await;
    assert!(
        matches!(
            refused,
            Err(crate::domain::error::DomainError::Unavailable { .. })
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn each_group_is_newest_first_with_the_id_breaking_a_tie() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    let (one, two) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let kept = attach(
        &hierarchy,
        tree.a,
        tree.a,
        vec![
            report(10, tree.a, one, 5),
            report(11, tree.a, one, 9),
            report(12, tree.a, one, 5),
            report(13, tree.a, two, 1),
        ],
    )
    .await
    .expect("attached");
    let order: Vec<u128> = kept.of(one).iter().map(|r| r.id.as_u128()).collect();
    assert_eq!(order, [11, 12, 10]);
    assert_eq!(kept.of(two).len(), 1);
    assert!(kept.of(Uuid::from_u128(3)).is_empty());
}
