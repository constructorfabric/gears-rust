//! Management service tests against in-memory `SQLite` with every migration
//! applied, the real Rego backend, a fake type catalog, and a fake PDP.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use policy_engine_sdk::gts::permissions::actions;
use policy_engine_sdk::management::{
    self as sdk, BundlePatch, DocumentSpec, ManagementError, NewBundle, VersionContent, finding,
    reason,
};
use toolkit_db::secure::AccessScope;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::UNKNOWN_TYPE;
use crate::domain::management::test_support::governance::{
    ALICE, Harness, TENANT_A, TENANT_B, alice, bob, harness, harness_with, spec,
};
use crate::domain::management::test_support::{FakePdp, PdpMode};
use crate::domain::model::{VersionId, VersionState};
use crate::domain::repos::VersionRepository;
use crate::infra::storage::content_repo::OrmVersionRepository;

fn content(documents: Vec<DocumentSpec>) -> VersionContent {
    VersionContent { documents }
}

fn syntax_error(name: &str) -> DocumentSpec {
    DocumentSpec {
        content: format!("package {name}\n\ndeny if {{"),
        ..spec(name)
    }
}

fn code(err: &ManagementError) -> Option<String> {
    reason_of(err)
}

impl Harness {
    async fn stored_state(&self, id: Uuid) -> VersionState {
        let conn = self.db.conn().unwrap();
        OrmVersionRepository
            .get_with_content(&conn, &AccessScope::allow_all(), VersionId(id))
            .await
            .unwrap()
            .expect("version exists")
            .state
    }

    /// A draft of `bundle` (Alice's) with valid documents `names`.
    async fn draft_with(&self, bundle: &sdk::Bundle, names: &[&str]) -> sdk::BundleVersion {
        let draft = self
            .core
            .create_draft_version(&alice(), bundle.id, None)
            .await
            .unwrap();
        self.core
            .replace_draft_content(
                &alice(),
                bundle.id,
                draft.id,
                content(names.iter().map(|n| spec(n)).collect()),
            )
            .await
            .unwrap()
            .version
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one end-to-end lifecycle scenario
async fn lifecycle_create_draft_validate_activate_seed_supersede() {
    let h = harness().await;
    let a = alice();

    let bundle = h
        .core
        .create_bundle(
            &a,
            NewBundle::new("guardrails").with_description("vm rules"),
        )
        .await
        .unwrap();
    assert_eq!(bundle.owner_tenant_id, TENANT_A);
    assert_eq!(bundle.created_by, ALICE);
    assert_eq!(bundle.active_version_id, None);
    assert_eq!(h.core.get_bundle(&a, bundle.id).await.unwrap(), bundle);

    let v1 = h
        .core
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();
    assert_eq!(v1.ordinal, 1);
    assert_eq!(v1.state, sdk::VersionState::Draft);

    // Invalid content is saved, reported by validation, refused by
    // activation; nothing activates.
    h.core
        .replace_draft_content(
            &a,
            bundle.id,
            v1.id,
            content(vec![spec("deny_delete"), syntax_error("broken")]),
        )
        .await
        .unwrap();
    let report = h.core.validate_version(&a, bundle.id, v1.id).await.unwrap();
    assert!(!report.is_valid());
    assert!(report.findings.iter().any(|f| {
        f.document_name.as_deref() == Some("broken") && f.code == finding::SYNTAX_ERROR
    }));
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.document_name.as_deref() != Some("deny_delete"))
    );
    let err = h
        .core
        .activate_version(&a, bundle.id, v1.id)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert_eq!(code(&err).as_deref(), Some(reason::VALIDATION_FAILED));
    assert_eq!(h.stored_state(v1.id).await, VersionState::Draft);

    // Valid content activates; the bundle points at it.
    let valid = h
        .core
        .replace_draft_content(&a, bundle.id, v1.id, content(vec![spec("deny_delete")]))
        .await
        .unwrap();
    assert_eq!(
        h.core.get_version(&a, bundle.id, v1.id).await.unwrap(),
        valid
    );
    assert_eq!(valid.documents[0].spec, spec("deny_delete"));
    assert!(
        h.core
            .validate_version(&a, bundle.id, v1.id)
            .await
            .unwrap()
            .is_valid()
    );
    let active = h.core.activate_version(&a, bundle.id, v1.id).await.unwrap();
    assert_eq!(active.state, sdk::VersionState::Active);
    assert_eq!(active.activated_by, Some(ALICE));
    assert!(active.activated_at.is_some());
    assert_eq!(
        h.core
            .get_bundle(&a, bundle.id)
            .await
            .unwrap()
            .active_version_id,
        Some(v1.id)
    );

    // Seeding a new draft from the active version copies its content.
    let v2 = h
        .core
        .create_draft_version(&a, bundle.id, Some(v1.id))
        .await
        .unwrap();
    assert_eq!(v2.ordinal, 2);
    let seeded = h.core.get_version(&a, bundle.id, v2.id).await.unwrap();
    assert_eq!(seeded.documents[0].spec, spec("deny_delete"));
    assert_ne!(seeded.documents[0].id, valid.documents[0].id);

    // Activating it supersedes the previous active version.
    h.core
        .replace_draft_content(
            &a,
            bundle.id,
            v2.id,
            content(vec![spec("deny_delete"), spec("deny_gadget")]),
        )
        .await
        .unwrap();
    h.core.activate_version(&a, bundle.id, v2.id).await.unwrap();
    assert_eq!(h.stored_state(v1.id).await, VersionState::Superseded);
    let versions = h.core.list_versions(&a, bundle.id).await.unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|v| (v.ordinal, v.state))
            .collect::<Vec<_>>(),
        vec![
            (2, sdk::VersionState::Active),
            (1, sdk::VersionState::Superseded)
        ]
    );
}

#[tokio::test]
async fn a_bundle_has_one_open_draft_so_a_retried_create_conflicts() {
    let h = harness().await;
    let a = alice();
    let bundle = h.core.create_bundle(&a, NewBundle::new("b")).await.unwrap();
    let draft = h
        .core
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();

    let err = h
        .core
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 409);
    assert_eq!(code(&err).as_deref(), Some(reason::DRAFT_EXISTS));
    assert_eq!(h.core.list_versions(&a, bundle.id).await.unwrap().len(), 1);

    // Deleting the draft (cascading its documents) frees the slot.
    h.core
        .replace_draft_content(&a, bundle.id, draft.id, content(vec![spec("d")]))
        .await
        .unwrap();
    h.core
        .delete_draft_version(&a, bundle.id, draft.id)
        .await
        .unwrap();
    let err = h
        .core
        .get_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 404);
    assert!(
        h.core
            .create_draft_version(&a, bundle.id, None)
            .await
            .is_ok()
    );

    // An activated draft frees it too.
    let other = h.core.create_bundle(&a, NewBundle::new("o")).await.unwrap();
    let (_, v) = h.active(&a, &["deny_delete"]).await;
    assert_eq!(v.state, sdk::VersionState::Active);
    let first = h.draft_with(&other, &["deny_delete"]).await;
    h.core
        .activate_version(&a, other.id, first.id)
        .await
        .unwrap();
    assert!(
        h.core
            .create_draft_version(&a, other.id, None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn activating_the_active_version_is_a_no_op_and_a_superseded_one_is_refused() {
    let h = harness().await;
    let a = alice();
    let (bundle, active) = h.active(&a, &["deny_delete"]).await;

    let again = h
        .core
        .activate_version(&a, bundle.id, active.id)
        .await
        .unwrap();
    assert_eq!(again, active, "success, nothing changed");

    let next = h.draft_with(&bundle, &["deny_delete"]).await;
    h.core
        .activate_version(&a, bundle.id, next.id)
        .await
        .unwrap();
    let err = h
        .core
        .activate_version(&a, bundle.id, active.id)
        .await
        .unwrap_err();
    assert_eq!(code(&err).as_deref(), Some(reason::VERSION_NOT_DRAFT));
}

#[tokio::test]
async fn only_drafts_change() {
    let h = harness().await;
    let a = alice();
    let (bundle, active) = h.active(&a, &["deny_delete"]).await;
    let refusals = [
        h.core
            .replace_draft_content(&a, bundle.id, active.id, content(vec![spec("x")]))
            .await
            .unwrap_err(),
        h.core
            .delete_draft_version(&a, bundle.id, active.id)
            .await
            .unwrap_err(),
    ];
    for err in refusals {
        assert_eq!(err.status_code(), 409);
        assert_eq!(code(&err).as_deref(), Some(reason::VERSION_NOT_DRAFT));
    }
}

// ---------------------------------------------------------------------------
// Validation and write-time refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concrete_resource_types_must_exist_and_patterns_must_parse() {
    let h = harness().await;
    let a = alice();
    let bundle = h.core.create_bundle(&a, NewBundle::new("b")).await.unwrap();
    let draft = h
        .core
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();

    let mut unknown = spec("unknown_type");
    unknown.resource_types = vec![UNKNOWN_TYPE.to_owned()];
    let mut malformed = spec("bad_pattern");
    malformed.resource_types = vec!["not a pattern".to_owned()];
    let mut wildcard = spec("wildcard");
    wildcard.resource_types = vec!["gts.cf.core.nowhere.*".to_owned()];
    h.core
        .replace_draft_content(
            &a,
            bundle.id,
            draft.id,
            content(vec![unknown, malformed, wildcard]),
        )
        .await
        .unwrap();

    let report = h
        .core
        .validate_version(&a, bundle.id, draft.id)
        .await
        .unwrap();
    let codes: Vec<(&str, &str)> = report
        .findings
        .iter()
        .map(|f| (f.document_name.as_deref().unwrap(), f.code.as_str()))
        .collect();
    assert_eq!(
        codes,
        [
            ("bad_pattern", finding::INVALID_PATTERN),
            ("unknown_type", finding::RESOURCE_TYPE_UNKNOWN),
        ],
        "documents read back by name; a wildcard needs no registered type"
    );
    let err = h
        .core
        .activate_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(code(&err).as_deref(), Some(reason::VALIDATION_FAILED));
}

#[tokio::test]
async fn write_time_refusals_leave_the_draft_unchanged() {
    let h = harness().await;
    let a = alice();
    let bundle = h.core.create_bundle(&a, NewBundle::new("b")).await.unwrap();
    let draft = h.draft_with(&bundle, &["deny_delete"]).await;

    let too_many: Vec<DocumentSpec> = (0..9).map(|i| spec(&format!("d{i}"))).collect();
    let cases = vec![
        (too_many, reason::CONTENT_LIMIT_EXCEEDED),
        (
            vec![spec("same"), spec("same")],
            finding::DUPLICATE_DOCUMENT_NAME,
        ),
    ];
    for (documents, expected) in cases {
        let err = h
            .core
            .replace_draft_content(&a, bundle.id, draft.id, content(documents))
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(code(&err).as_deref(), Some(expected));
    }
    let detail = h.core.get_version(&a, bundle.id, draft.id).await.unwrap();
    assert_eq!(detail.documents.len(), 1);

    // A seed must be a retained version of the same bundle.
    let other = h
        .core
        .create_bundle(&a, NewBundle::new("other"))
        .await
        .unwrap();
    for seed in [draft.id, Uuid::new_v4()] {
        let err = h
            .core
            .create_draft_version(&a, other.id, Some(seed))
            .await
            .unwrap_err();
        assert_eq!(code(&err).as_deref(), Some(reason::SEED_NOT_IN_BUNDLE));
    }
}

#[tokio::test]
async fn bundles_update_and_names_stay_unique_per_tenant() {
    let h = harness().await;
    let a = alice();
    let bundle = h.core.create_bundle(&a, NewBundle::new("b")).await.unwrap();
    h.core
        .create_bundle(&a, NewBundle::new("taken"))
        .await
        .unwrap();

    let err = h
        .core
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 409);
    assert_eq!(code(&err).as_deref(), Some(reason::BUNDLE_NAME_TAKEN));

    let updated = h
        .core
        .update_bundle(
            &a,
            bundle.id,
            BundlePatch {
                name: Some("renamed".to_owned()),
                description: Some("new".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (updated.name.as_str(), updated.description.as_str()),
        ("renamed", "new")
    );
    let err = h
        .core
        .update_bundle(
            &a,
            bundle.id,
            BundlePatch {
                name: Some("taken".to_owned()),
                description: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(&err).as_deref(), Some(reason::BUNDLE_NAME_TAKEN));
    let page = h
        .core
        .list_bundles(&a, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}

// ---------------------------------------------------------------------------
// Authorisation and isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn another_tenants_content_is_indistinguishable_from_absent_content() {
    let h = harness().await;
    let (bundle, active) = h.active(&alice(), &["deny_delete"]).await;
    let draft = h.draft_with(&bundle, &["deny_delete"]).await;
    let b = bob();
    let missing = Uuid::new_v4();

    for (target, version) in [(bundle.id, active.id), (missing, missing)] {
        let refusals = vec![
            h.core.get_bundle(&b, target).await.unwrap_err(),
            h.core.get_version(&b, target, version).await.unwrap_err(),
            h.core.list_versions(&b, target).await.unwrap_err(),
            h.core
                .update_bundle(&b, target, BundlePatch::default())
                .await
                .unwrap_err(),
            h.core
                .create_draft_version(&b, target, None)
                .await
                .unwrap_err(),
            h.core
                .validate_version(&b, target, version)
                .await
                .unwrap_err(),
            h.core
                .replace_draft_content(&b, target, draft.id, content(vec![spec("x")]))
                .await
                .unwrap_err(),
            h.core
                .activate_version(&b, target, draft.id)
                .await
                .unwrap_err(),
            h.core
                .delete_draft_version(&b, target, draft.id)
                .await
                .unwrap_err(),
        ];
        for err in refusals {
            assert_eq!(err.status_code(), 404, "{err:?}");
        }
    }
    assert!(
        h.core
            .list_bundles(&b, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // A version of another bundle is absent too.
    let other = h
        .core
        .create_bundle(&alice(), NewBundle::new("other"))
        .await
        .unwrap();
    let err = h
        .core
        .get_version(&alice(), other.id, active.id)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 404);
    // Creating for a tenant the caller may not manage is absent as well.
    let err = h
        .core
        .create_bundle(&alice(), NewBundle::new("x").with_owner_tenant(TENANT_B))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 404);
}

#[tokio::test]
async fn each_operation_needs_its_own_capability() {
    let h = harness_with(FakePdp::allow_only(&[actions::READ])).await;
    let a = alice();
    assert!(
        h.core
            .list_bundles(&a, &ODataQuery::default())
            .await
            .is_ok()
    );
    let err = h
        .core
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 403);
    assert_eq!(code(&err).as_deref(), Some(reason::CAPABILITY_DENIED));

    // With author but not publish: drafts, but no activation.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::READ, actions::AUTHOR]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )));
    let bundle = h.core.create_bundle(&a, NewBundle::new("b")).await.unwrap();
    let draft = h.draft_with(&bundle, &["deny_delete"]).await;
    let err = h
        .core
        .activate_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(code(&err).as_deref(), Some(reason::CAPABILITY_DENIED));
    assert_eq!(h.stored_state(draft.id).await, VersionState::Draft);

    // Publish alone activates.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::READ, actions::PUBLISH]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )));
    assert!(
        h.core
            .activate_version(&a, bundle.id, draft.id)
            .await
            .is_ok()
    );

    // Denied outright: listings are capability-denied, reads absent.
    h.pdp.set_mode(PdpMode::Deny);
    let err = h
        .core
        .list_bundles(&a, &ODataQuery::default())
        .await
        .unwrap_err();
    assert_eq!(code(&err).as_deref(), Some(reason::CAPABILITY_DENIED));
    assert_eq!(
        h.core
            .get_bundle(&a, bundle.id)
            .await
            .unwrap_err()
            .status_code(),
        404
    );

    // An unreachable PDP is an outage, never a grant.
    h.pdp.set_mode(PdpMode::Fail);
    for err in [
        h.core.get_bundle(&a, bundle.id).await.unwrap_err(),
        h.core
            .create_bundle(&a, NewBundle::new("c"))
            .await
            .unwrap_err(),
        h.core
            .activate_version(&a, bundle.id, draft.id)
            .await
            .unwrap_err(),
    ] {
        assert_eq!(err.status_code(), 503);
    }
}
