#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Round trip of every [`PolicyManagementClientV1`] method through the local
//! client, against in-memory `SQLite` with the fake tree and PDP.

use std::sync::Arc;

use policy_engine_sdk::management::{
    AssignmentSpec, BundlePatch, NewBundle, PolicyManagementClientV1, VersionContent, VersionState,
};
use toolkit_odata::ODataQuery;

use super::*;
use crate::domain::management::test_support::governance::{TENANT_A1, alice, harness, spec};

#[tokio::test]
async fn every_method_round_trips_through_the_local_client() {
    let h = harness().await;
    let client: Arc<dyn PolicyManagementClientV1> =
        Arc::new(PolicyManagementLocalClient::new(Arc::clone(&h.service)));
    let a = alice();

    // Bundles.
    let bundle = client
        .create_bundle(&a, NewBundle::new("guardrails"))
        .await
        .unwrap();
    assert_eq!(
        client.get_bundle(&a, bundle.id).await.unwrap().name,
        "guardrails"
    );
    assert_eq!(
        client
            .list_bundles(&a, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let bundle = client
        .update_bundle(
            &a,
            bundle.id,
            BundlePatch {
                description: Some("tenant guardrails".to_owned()),
                ..BundlePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bundle.description, "tenant guardrails");

    // Versions.
    let draft = client
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();
    let version = client
        .replace_draft_content(
            &a,
            bundle.id,
            draft.id,
            VersionContent {
                documents: vec![spec("alpha")],
            },
        )
        .await
        .unwrap()
        .version;
    assert_eq!(
        client
            .get_version(&a, bundle.id, version.id)
            .await
            .unwrap()
            .documents
            .len(),
        1
    );
    assert_eq!(client.list_versions(&a, bundle.id).await.unwrap().len(), 1);
    assert!(
        client
            .validate_version(&a, bundle.id, version.id)
            .await
            .unwrap()
            .is_valid()
    );
    let activated = client
        .activate_version(&a, bundle.id, version.id)
        .await
        .unwrap();
    assert_eq!(activated.state, VersionState::Active);
    let seeded = client
        .create_draft_version(&a, bundle.id, Some(version.id))
        .await
        .unwrap();
    client
        .delete_draft_version(&a, bundle.id, seeded.id)
        .await
        .unwrap();

    // Assignments.
    let assigned = client
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();
    assert_eq!(
        client.get_assignment(&a, assigned.id).await.unwrap(),
        assigned
    );
    let updated = client
        .update_assignment(&a, assigned.id, false)
        .await
        .unwrap();
    assert!(!updated.enforce);
    client.unassign(&a, updated.id).await.unwrap();

    let concrete = PolicyManagementLocalClient::new(Arc::clone(&h.service));
    assert!(format!("{concrete:?}").starts_with("PolicyManagementLocalClient"));
}
