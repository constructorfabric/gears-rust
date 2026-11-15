//! Only the principal that created an anonymous consumer group, in its tenant,
//! may JOIN or DELETE it, in process (#5494). Another principal, another
//! subject of the creator's tenant or the creator's subject in another tenant,
//! is refused with `403 ConsumerGroupNotOwned`: on JOIN right after the group
//! lookup, before any PDP call; on DELETE after the tenant gate and before the
//! active-members check. Get keeps the tenant gate alone,
//! and a named group is shared by permission, not by creator. Each test records
//! every request the PDP sees after its setup, so the list holds exactly what
//! the call under test asks. The REST entry points are covered in
//! `api/rest/handlers/delivery/`, the in-process SDK client in
//! `local_broker_tests.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use authz_resolver_sdk::{
    AccessRequest, BarrierMode as PdpBarrierMode, EvaluationRequest, PolicyEnforcer, ResourceType,
    TenantMode,
};
use chrono::Utc;
use serde_json::Value;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::consumer_group_coordinator::{ConsumerGroupCoordinator, MemberState};
use crate::domain::delivery::JoinRequest;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{ConsumerGroup, ConsumerGroupCreateInput, ConsumerGroupKind};
use crate::domain::repo::ConsumerGroupRepo;
use crate::test_support::{DenyingAuthZ, EventBrokerHarness};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const CONSUMER_GROUP_RESOURCE: &str = "gts.cf.core.events.consumer_group.v1~";
const UNKNOWN_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d3600";
const NAMED_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d3604";

const CREATOR: Uuid = Uuid::from_u128(0x3601);
const TENANT: Uuid = Uuid::from_u128(0x3602);
const OTHER_SUBJECT: Uuid = Uuid::from_u128(0x3603);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x3605);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// The principal that creates the group: a subject of `TENANT`.
fn creator() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(CREATOR)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// Another subject of the creator's tenant.
fn other_principal() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(OTHER_SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// The creator's subject in another tenant: a principal other than the creator,
/// since an anonymous group is owned by its subject and tenant together.
fn creator_in_another_tenant() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(CREATOR)
        .subject_tenant_id(OTHER_TENANT)
        .build()
        .unwrap()
}

/// An anonymous group created by [`creator`], with the PDP list cleared after
/// the setup.
struct Created {
    harness: EventBrokerHarness,
    enforcer: PolicyEnforcer,
    pdp: Recorded,
    group: GtsInstanceId,
}

/// [`creator`] creates an anonymous group under a PDP that allows every
/// request and records each one.
async fn created() -> Created {
    let pdp = Recorded::default();
    let sink = Arc::clone(&pdp);
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingAuthZ {
        deny_if: move |req: &EvaluationRequest| {
            sink.lock()
                .unwrap()
                .push(serde_json::to_value(req).unwrap());
            false
        },
    }));
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(enforcer.clone())
        .build()
        .await;
    let group = harness
        .delivery()
        .create_consumer_group(
            &creator(),
            ConsumerGroupCreateInput {
                client_agent: Some("group-owner-test".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    pdp.lock().unwrap().clear();
    Created {
        harness,
        enforcer,
        pdp,
        group,
    }
}

/// A JOIN of `group` with no interests, so the only PDP call a JOIN that
/// passes the owner check makes is the group check.
fn join_request(group: &GtsInstanceId) -> JoinRequest {
    JoinRequest {
        consumer_group: group.clone(),
        client_agent: "group-owner-test".to_owned(),
        interests: Vec::new(),
        session_timeout: Duration::from_secs(30),
    }
}

/// [`creator`] JOINs `created`'s group; returns the subscription id. Clears
/// the PDP list afterwards.
async fn join_as_creator(created: &Created) -> Uuid {
    let id = created
        .harness
        .delivery()
        .join(&creator(), join_request(&created.group))
        .await
        .expect("the creator's JOIN is allowed")
        .subscription
        .id;
    created.pdp.lock().unwrap().clear();
    id
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The tenant gate's request for `ctx` and `action` on the group's tenant, as
/// `tenant_authorized` sends it: `TENANT` as the root of a `root_only` context
/// with barriers respected, and as `owner_tenant_id`.
fn gate_request(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &'static str,
) -> Vec<Value> {
    vec![
        serde_json::to_value(
            enforcer.build_request_with(
                ctx,
                &ResourceType::from_static(REQUEST_RESOURCE, &[pep_properties::OWNER_TENANT_ID]),
                action,
                None,
                false,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
                    .context_tenant_id(TENANT)
                    .tenant_mode(TenantMode::RootOnly)
                    .barrier_mode(PdpBarrierMode::Respect)
                    .require_constraints(false),
            ),
        )
        .unwrap(),
    ]
}

/// The group check for `ctx` on `group`: `consume` on the group family
/// resource, which declares `owner_tenant_id`, naming the group and its tenant
/// `TENANT`, with `TENANT` as the root of a `root_only` context with barriers
/// respected.
fn group_check(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    group: &GtsInstanceId,
) -> Vec<Value> {
    vec![
        serde_json::to_value(
            enforcer.build_request_with(
                ctx,
                &ResourceType::from_static(
                    CONSUMER_GROUP_RESOURCE,
                    &[pep_properties::OWNER_TENANT_ID],
                ),
                "consume",
                None,
                false,
                &AccessRequest::new()
                    .resource_property("consumer_group_id", group.as_ref())
                    .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
                    .context_tenant_id(TENANT)
                    .tenant_mode(TenantMode::RootOnly)
                    .barrier_mode(PdpBarrierMode::Respect)
                    .require_constraints(false),
            ),
        )
        .unwrap(),
    ]
}

/// The owner check's refusal of group `group`, as the domain returns it.
fn assert_not_owned<T>(result: Result<T, DomainError>, group: &GtsInstanceId) {
    let Err(error) = result else {
        panic!("expected Forbidden, got Ok");
    };
    let DomainError::Forbidden {
        code,
        message,
        resource,
    } = error
    else {
        panic!("expected Forbidden, got {error:?}");
    };
    assert_eq!(
        (code, message, resource),
        (
            ErrorCode::ConsumerGroupNotOwned,
            format!("consumer group '{group}' was created by another principal"),
            group.to_string(),
        )
    );
}

fn subscription_ids(created: &Created) -> Vec<Uuid> {
    created
        .harness
        .groups()
        .list()
        .iter()
        .map(|s| s.id)
        .collect()
}

async fn group_exists(created: &Created) -> bool {
    created
        .harness
        .repo()
        .find_consumer_group(&created.group)
        .await
        .expect("reading the group must not fail")
        .is_some()
}

/// The refused JOIN writes no routing marker either: the marker store is not
/// reachable from here (`Storage::cache` is private to the facade), but the
/// owner check returns before `join` reaches `mark_subscription`, and the empty
/// PDP list shows it returned before the group check, which comes earlier still.
#[tokio::test]
async fn a_join_by_another_principal_of_the_tenant_is_refused_before_any_pdp_call() {
    let created = created().await;

    let result = created
        .harness
        .delivery()
        .join(&other_principal(), join_request(&created.group))
        .await;

    assert_not_owned(result, &created.group);
    assert_eq!(recorded(&created.pdp), Vec::<Value>::new());
    assert_eq!(subscription_ids(&created), Vec::<Uuid>::new());
}

#[tokio::test]
async fn a_join_by_the_same_subject_from_another_tenant_is_refused_before_any_pdp_call() {
    let created = created().await;

    let result = created
        .harness
        .delivery()
        .join(&creator_in_another_tenant(), join_request(&created.group))
        .await;

    assert_not_owned(result, &created.group);
    assert_eq!(recorded(&created.pdp), Vec::<Value>::new());
    assert_eq!(subscription_ids(&created), Vec::<Uuid>::new());
}

/// A JOIN evicts the group's `Disconnected` members, so a JOIN by another
/// principal that got through would remove the creator's member.
#[tokio::test]
async fn a_join_by_another_principal_keeps_the_creators_disconnected_member() {
    let created = created().await;
    let member = join_as_creator(&created).await;
    let groups = created.harness.shared_groups();
    let (_assignment, handle) =
        ConsumerGroupCoordinator::subscribe(&groups, &created.group, member)
            .expect("the creator's member exists");
    drop(handle);
    assert_eq!(groups.state_of(member), Some(MemberState::Disconnected));

    let result = created
        .harness
        .delivery()
        .join(&other_principal(), join_request(&created.group))
        .await;

    assert_not_owned(result, &created.group);
    assert_eq!(recorded(&created.pdp), Vec::<Value>::new());
    assert_eq!(subscription_ids(&created), vec![member]);
    assert_eq!(groups.state_of(member), Some(MemberState::Disconnected));
}

#[tokio::test]
async fn a_delete_by_another_principal_of_the_tenant_is_refused_after_the_tenant_gate() {
    let created = created().await;

    let result = created
        .harness
        .delivery()
        .delete_consumer_group(&other_principal(), &created.group)
        .await;

    assert_not_owned(result, &created.group);
    assert_eq!(
        recorded(&created.pdp),
        gate_request(&created.enforcer, &other_principal(), "manage")
    );
    assert!(
        group_exists(&created).await,
        "the refused delete kept the group"
    );
}

#[tokio::test]
async fn a_delete_by_another_principal_of_a_group_with_members_is_refused_not_in_conflict() {
    let created = created().await;
    let member = join_as_creator(&created).await;

    let result = created
        .harness
        .delivery()
        .delete_consumer_group(&other_principal(), &created.group)
        .await;

    assert_not_owned(result, &created.group);
    assert_eq!(
        recorded(&created.pdp),
        gate_request(&created.enforcer, &other_principal(), "manage")
    );
    assert!(
        group_exists(&created).await,
        "the refused delete kept the group"
    );
    assert_eq!(subscription_ids(&created), vec![member]);
}

#[tokio::test]
async fn a_delete_of_an_unknown_group_by_another_principal_answers_not_found() {
    let created = created().await;
    let unknown = GtsInstanceId::try_new(UNKNOWN_GROUP).unwrap();

    let result = created
        .harness
        .delivery()
        .delete_consumer_group(&other_principal(), &unknown)
        .await;

    let Err(DomainError::NotFound {
        code,
        message,
        resource,
    }) = result
    else {
        panic!("expected NotFound, got {result:?}");
    };
    assert_eq!(
        (code, message, resource),
        (
            ErrorCode::ConsumerGroupNotFound,
            format!("consumer group '{UNKNOWN_GROUP}' is not registered"),
            UNKNOWN_GROUP.to_owned(),
        )
    );
    assert_eq!(recorded(&created.pdp), Vec::<Value>::new());
}

#[tokio::test]
async fn the_creator_joins_leaves_and_deletes_its_own_group() {
    let created = created().await;

    let member = created
        .harness
        .delivery()
        .join(&creator(), join_request(&created.group))
        .await
        .expect("the creator's JOIN is allowed")
        .subscription
        .id;
    assert_eq!(
        recorded(&created.pdp),
        group_check(&created.enforcer, &creator(), &created.group)
    );
    created
        .harness
        .delivery()
        .leave(&creator(), member)
        .await
        .expect("the creator's LEAVE is allowed");
    created.pdp.lock().unwrap().clear();
    created
        .harness
        .delivery()
        .delete_consumer_group(&creator(), &created.group)
        .await
        .expect("the creator's delete is allowed");

    assert_eq!(
        recorded(&created.pdp),
        gate_request(&created.enforcer, &creator(), "manage")
    );
    assert!(
        !group_exists(&created).await,
        "the creator's delete removed the group"
    );
}

#[tokio::test]
async fn a_get_by_another_principal_of_the_tenant_answers_the_group() {
    let created = created().await;

    let group = created
        .harness
        .delivery()
        .get_consumer_group(&other_principal(), &created.group)
        .await
        .expect("a read keeps the tenant gate alone");

    assert_eq!(
        (
            group.id,
            group.kind,
            group.tenant_id,
            group.owner_principal_id
        ),
        (
            created.group.clone(),
            ConsumerGroupKind::Anonymous,
            TENANT,
            CREATOR
        )
    );
    assert_eq!(
        recorded(&created.pdp),
        gate_request(&created.enforcer, &other_principal(), "consume")
    );
}

/// A named group is shared by permission: a principal other than the one its
/// row names goes to the named group's own permission check, not the owner
/// check. Its id ends in a UUID, so the JOIN can write its owner marker.
#[tokio::test]
async fn a_named_group_joined_by_another_principal_is_not_refused_by_the_owner_check() {
    let created = created().await;
    let named = GtsInstanceId::try_new(NAMED_GROUP).unwrap();
    created
        .harness
        .repo()
        .create_consumer_group(ConsumerGroup {
            id: named.clone(),
            kind: ConsumerGroupKind::Named,
            tenant_id: TENANT,
            owner_principal_id: CREATOR,
            description: None,
            created_at: Utc::now(),
        })
        .await
        .expect("seeding the named group must not fail");

    let member = created
        .harness
        .delivery()
        .join(&other_principal(), join_request(&named))
        .await
        .expect("a named group has no owner check")
        .subscription
        .id;

    assert_eq!(
        recorded(&created.pdp),
        vec![
            serde_json::to_value(
                created.enforcer.build_request_with(
                    &other_principal(),
                    &ResourceType::new(NAMED_GROUP.to_owned(), &[pep_properties::OWNER_TENANT_ID]),
                    "consume",
                    None,
                    false,
                    &AccessRequest::new()
                        .resource_property("consumer_group_id", NAMED_GROUP)
                        .context_tenant_id(TENANT)
                        .tenant_mode(TenantMode::RootOnly)
                        .barrier_mode(PdpBarrierMode::Respect)
                        .require_constraints(false),
                ),
            )
            .unwrap(),
        ]
    );
    assert_eq!(subscription_ids(&created), vec![member]);
}
