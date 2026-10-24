//! Management service tests against in-memory `SQLite` with every migration
//! applied, the real Rego backend, a fake type catalog, and a fake PDP.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use policy_engine_sdk::gts::permissions::actions;
use policy_engine_sdk::management::{
    self as sdk, BundlePatch, DocumentSpec, LimitKind, ManagementError, NewBundle, VersionContent,
    finding, limits,
};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::UNKNOWN_TYPE;
use crate::domain::management::test_support::governance::{
    ALICE, Harness, TENANT_A, TENANT_B, alice, bob, harness, harness_with, spec,
};
use crate::domain::management::test_support::{FakePdp, PdpMode};
use crate::domain::model::{BundleId, VersionId, VersionState};

/// The findings a `VALIDATION_FAILED` error carries.
fn findings_of(err: ManagementError) -> Vec<sdk::ValidationFinding> {
    match err {
        ManagementError::ValidationFailed { findings } => findings,
        other => panic!("expected VALIDATION_FAILED, got {other:?}"),
    }
}

fn content(documents: Vec<DocumentSpec>) -> VersionContent {
    VersionContent { documents }
}

fn syntax_error(name: &str) -> DocumentSpec {
    DocumentSpec {
        content: format!("package {name}\n\ndeny if {{"),
        ..spec(name)
    }
}

impl Harness {
    /// A draft of `bundle` (Alice's) with valid documents `names`.
    async fn draft_with(&self, bundle: &sdk::Bundle, names: &[&str]) -> sdk::BundleVersion {
        let draft = self
            .service
            .create_draft_version(&alice(), bundle.id, None)
            .await
            .unwrap();
        self.service
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
        .service
        .create_bundle(
            &a,
            NewBundle::new("guardrails").with_description("vm rules"),
        )
        .await
        .unwrap();
    assert_eq!(bundle.owner_tenant_id, TENANT_A);
    assert_eq!(bundle.created_by, ALICE);
    assert_eq!(bundle.active_version_id, None);
    assert_eq!(h.service.get_bundle(&a, bundle.id).await.unwrap(), bundle);

    let v1 = h
        .service
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();
    assert_eq!(v1.ordinal, 1);
    assert_eq!(v1.state, sdk::VersionState::Draft);

    // Invalid content is saved, reported by validation, refused by
    // activation; nothing activates.
    h.service
        .replace_draft_content(
            &a,
            bundle.id,
            v1.id,
            content(vec![spec("deny_delete"), syntax_error("broken")]),
        )
        .await
        .unwrap();
    let invalid = h
        .service
        .validate_version(&a, bundle.id, v1.id)
        .await
        .unwrap_err();
    let findings = findings_of(invalid.clone());
    assert!(findings.iter().any(|f| {
        f.document_name.as_deref() == Some("broken") && f.code == finding::SYNTAX_ERROR
    }));
    assert!(
        findings
            .iter()
            .all(|f| f.document_name.as_deref() != Some("deny_delete"))
    );
    let err = h
        .service
        .activate_version(&a, bundle.id, v1.id)
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 400);
    assert_eq!(reason_of(&err).as_deref(), Some("VALIDATION_FAILED"));
    assert_eq!(err, invalid, "validation answers what activation would");
    assert_eq!(h.stored_state(v1.id).await, VersionState::Draft);

    // Valid content activates; the bundle points at it.
    let valid = h
        .service
        .replace_draft_content(&a, bundle.id, v1.id, content(vec![spec("deny_delete")]))
        .await
        .unwrap();
    assert_eq!(
        h.service.get_version(&a, bundle.id, v1.id).await.unwrap(),
        valid
    );
    assert_eq!(valid.documents[0].spec, spec("deny_delete"));
    h.service
        .validate_version(&a, bundle.id, v1.id)
        .await
        .unwrap();
    let active = h
        .service
        .activate_version(&a, bundle.id, v1.id)
        .await
        .unwrap();
    assert_eq!(active.state, sdk::VersionState::Active);
    assert_eq!(active.activated_by, Some(ALICE));
    assert!(active.activated_at.is_some());
    assert_eq!(
        h.service
            .get_bundle(&a, bundle.id)
            .await
            .unwrap()
            .active_version_id,
        Some(v1.id)
    );

    // Seeding a new draft from the active version copies its content.
    let v2 = h
        .service
        .create_draft_version(&a, bundle.id, Some(v1.id))
        .await
        .unwrap();
    assert_eq!(v2.ordinal, 2);
    let seeded = h.service.get_version(&a, bundle.id, v2.id).await.unwrap();
    assert_eq!(seeded.documents[0].spec, spec("deny_delete"));
    assert_ne!(seeded.documents[0].id, valid.documents[0].id);

    // Activating it supersedes the previous active version.
    h.service
        .replace_draft_content(
            &a,
            bundle.id,
            v2.id,
            content(vec![spec("deny_delete"), spec("deny_gadget")]),
        )
        .await
        .unwrap();
    h.service
        .activate_version(&a, bundle.id, v2.id)
        .await
        .unwrap();
    assert_eq!(h.stored_state(v1.id).await, VersionState::Superseded);
    let versions = h
        .service
        .list_versions(&a, bundle.id, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(
        versions
            .items
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
    let bundle = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap();
    let draft = h
        .service
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();

    let err = h
        .service
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 409);
    assert_eq!(reason_of(&err).as_deref(), Some("DRAFT_EXISTS"));
    assert_eq!(
        h.service
            .list_versions(&a, bundle.id, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );

    // Deleting the draft (cascading its documents) frees the slot.
    h.service
        .replace_draft_content(&a, bundle.id, draft.id, content(vec![spec("d")]))
        .await
        .unwrap();
    h.service
        .delete_draft_version(&a, bundle.id, draft.id)
        .await
        .unwrap();
    let err = h
        .service
        .get_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 404);
    assert!(
        h.service
            .create_draft_version(&a, bundle.id, None)
            .await
            .is_ok()
    );

    // An activated draft frees it too.
    let other = h
        .service
        .create_bundle(&a, NewBundle::new("o"))
        .await
        .unwrap();
    let (_, v) = h.active(&a, &["deny_delete"]).await;
    assert_eq!(v.state, sdk::VersionState::Active);
    let first = h.draft_with(&other, &["deny_delete"]).await;
    h.service
        .activate_version(&a, other.id, first.id)
        .await
        .unwrap();
    assert!(
        h.service
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
        .service
        .activate_version(&a, bundle.id, active.id)
        .await
        .unwrap();
    assert_eq!(again, active, "success, nothing changed");

    let next = h.draft_with(&bundle, &["deny_delete"]).await;
    h.service
        .activate_version(&a, bundle.id, next.id)
        .await
        .unwrap();
    let err = h
        .service
        .activate_version(&a, bundle.id, active.id)
        .await
        .unwrap_err();
    assert_eq!(reason_of(&err).as_deref(), Some("VERSION_NOT_DRAFT"));
}

#[tokio::test]
async fn only_drafts_change() {
    let h = harness().await;
    let a = alice();
    let (bundle, active) = h.active(&a, &["deny_delete"]).await;
    let refusals = [
        h.service
            .replace_draft_content(&a, bundle.id, active.id, content(vec![spec("x")]))
            .await
            .unwrap_err(),
        h.service
            .delete_draft_version(&a, bundle.id, active.id)
            .await
            .unwrap_err(),
    ];
    for err in refusals {
        assert_eq!(err.category().http_status(), 409);
        assert_eq!(reason_of(&err).as_deref(), Some("VERSION_NOT_DRAFT"));
    }
}

// ---------------------------------------------------------------------------
// Validation and write-time refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concrete_resource_types_must_exist_and_patterns_must_parse() {
    let h = harness().await;
    let a = alice();
    let bundle = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap();
    let draft = h
        .service
        .create_draft_version(&a, bundle.id, None)
        .await
        .unwrap();

    let mut unknown = spec("unknown_type");
    unknown.resource_types = vec![UNKNOWN_TYPE.to_owned()];
    let mut malformed = spec("bad_pattern");
    malformed.resource_types = vec!["not a pattern".to_owned()];
    let mut wildcard = spec("wildcard");
    wildcard.resource_types = vec!["gts.cf.core.nowhere.*".to_owned()];
    h.service
        .replace_draft_content(
            &a,
            bundle.id,
            draft.id,
            content(vec![unknown, malformed, wildcard]),
        )
        .await
        .unwrap();

    let findings = findings_of(
        h.service
            .validate_version(&a, bundle.id, draft.id)
            .await
            .unwrap_err(),
    );
    let codes: Vec<(&str, &str)> = findings
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
        .service
        .activate_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(reason_of(&err).as_deref(), Some("VALIDATION_FAILED"));
}

#[tokio::test]
async fn write_time_refusals_leave_the_draft_unchanged() {
    let h = harness().await;
    let a = alice();
    let bundle = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap();
    let draft = h.draft_with(&bundle, &["deny_delete"]).await;

    let too_many: Vec<DocumentSpec> = (0..9).map(|i| spec(&format!("d{i}"))).collect();
    let cases = vec![
        (too_many, "CONTENT_LIMIT_EXCEEDED"),
        (
            vec![spec("same"), spec("same")],
            finding::DUPLICATE_DOCUMENT_NAME,
        ),
    ];
    for (documents, expected) in cases {
        let err = h
            .service
            .replace_draft_content(&a, bundle.id, draft.id, content(documents))
            .await
            .unwrap_err();
        assert_eq!(err.category().http_status(), 400);
        assert_eq!(reason_of(&err).as_deref(), Some(expected));
    }
    let detail = h
        .service
        .get_version(&a, bundle.id, draft.id)
        .await
        .unwrap();
    assert_eq!(detail.documents.len(), 1);

    // A seed must be a retained version of the same bundle.
    let other = h
        .service
        .create_bundle(&a, NewBundle::new("other"))
        .await
        .unwrap();
    for seed in [draft.id, VersionId(Uuid::new_v4())] {
        let err = h
            .service
            .create_draft_version(&a, other.id, Some(seed))
            .await
            .unwrap_err();
        assert_eq!(reason_of(&err).as_deref(), Some("SEED_NOT_IN_BUNDLE"));
    }
}

#[tokio::test]
async fn bundles_update_and_names_stay_unique_per_tenant() {
    let h = harness().await;
    let a = alice();
    let bundle = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap();
    h.service
        .create_bundle(&a, NewBundle::new("taken"))
        .await
        .unwrap();

    let err = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 409);
    assert_eq!(reason_of(&err).as_deref(), Some("BUNDLE_NAME_TAKEN"));

    let updated = h
        .service
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
        .service
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
    assert_eq!(reason_of(&err).as_deref(), Some("BUNDLE_NAME_TAKEN"));
    let page = h
        .service
        .list_bundles(&a, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}

#[tokio::test]
async fn a_bundle_is_found_by_its_name() {
    let h = harness().await;
    let a = alice();
    let wanted = h
        .service
        .create_bundle(&a, NewBundle::new("baseline"))
        .await
        .unwrap();
    h.service
        .create_bundle(&a, NewBundle::new("baseline-strict"))
        .await
        .unwrap();
    // Bob's namesake is outside Alice's scope.
    h.service
        .create_bundle(&bob(), NewBundle::new("baseline"))
        .await
        .unwrap();

    let by_name = ODataQuery::default().with_filter(
        toolkit_odata::parse_filter_string("name eq 'baseline'")
            .unwrap()
            .into_expr(),
    );
    let page = h.service.list_bundles(&a, &by_name).await.unwrap();
    assert_eq!(page.items, vec![wanted]);
}

#[tokio::test]
async fn a_name_and_an_owning_tenant_name_exactly_one_bundle() {
    // A grant over two tenants, each with a bundle of the same name.
    let h = harness_with(FakePdp::new(PdpMode::Administers(vec![TENANT_A, TENANT_B]))).await;
    let in_a = h
        .service
        .create_bundle(&alice(), NewBundle::new("baseline"))
        .await
        .unwrap();
    let in_b = h
        .service
        .create_bundle(&bob(), NewBundle::new("baseline"))
        .await
        .unwrap();
    let query = |filter: String| {
        ODataQuery::default().with_filter(
            toolkit_odata::parse_filter_string(&filter)
                .unwrap()
                .into_expr(),
        )
    };

    let mut both: Vec<_> = h
        .service
        .list_bundles(&alice(), &query("name eq 'baseline'".to_owned()))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|b| b.id)
        .collect();
    both.sort_by_key(|id| id.0);
    let mut expected = vec![in_a.id, in_b.id];
    expected.sort_by_key(|id| id.0);
    assert_eq!(both, expected, "one namesake per tenant");

    let one = h
        .service
        .list_bundles(
            &alice(),
            &query(format!(
                "name eq 'baseline' and owner_tenant_id eq {TENANT_B}"
            )),
        )
        .await
        .unwrap();
    assert_eq!(one.items, vec![in_b]);
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

    for (target, version) in [
        (bundle.id, active.id),
        (BundleId(missing), VersionId(missing)),
    ] {
        let refusals = vec![
            h.service.get_bundle(&b, target).await.unwrap_err(),
            h.service
                .get_version(&b, target, version)
                .await
                .unwrap_err(),
            h.service
                .list_versions(&b, target, &ODataQuery::default())
                .await
                .unwrap_err(),
            h.service
                .update_bundle(&b, target, BundlePatch::default())
                .await
                .unwrap_err(),
            h.service
                .create_draft_version(&b, target, None)
                .await
                .unwrap_err(),
            h.service
                .validate_version(&b, target, version)
                .await
                .unwrap_err(),
            h.service
                .replace_draft_content(&b, target, draft.id, content(vec![spec("x")]))
                .await
                .unwrap_err(),
            h.service
                .activate_version(&b, target, draft.id)
                .await
                .unwrap_err(),
            h.service
                .delete_draft_version(&b, target, draft.id)
                .await
                .unwrap_err(),
        ];
        for err in refusals {
            assert_eq!(err.category().http_status(), 404, "{err:?}");
        }
    }
    assert!(
        h.service
            .list_bundles(&b, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // A version of another bundle is absent too.
    let other = h
        .service
        .create_bundle(&alice(), NewBundle::new("other"))
        .await
        .unwrap();
    let err = h
        .service
        .get_version(&alice(), other.id, active.id)
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 404);
    // Creating for a tenant the caller may not manage is absent as well.
    let err = h
        .service
        .create_bundle(&alice(), NewBundle::new("x").with_owner_tenant(TENANT_B))
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 404);
}

#[tokio::test]
async fn each_operation_needs_its_own_capability() {
    let h = harness_with(FakePdp::allow_only(&[actions::READ])).await;
    let a = alice();
    assert!(
        h.service
            .list_bundles(&a, &ODataQuery::default())
            .await
            .is_ok()
    );
    let err = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 403);
    assert_eq!(reason_of(&err).as_deref(), Some("CAPABILITY_DENIED"));

    // With author but not publish: drafts, but no activation.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::READ, actions::AUTHOR]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )));
    let bundle = h
        .service
        .create_bundle(&a, NewBundle::new("b"))
        .await
        .unwrap();
    let draft = h.draft_with(&bundle, &["deny_delete"]).await;
    let err = h
        .service
        .activate_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(reason_of(&err).as_deref(), Some("CAPABILITY_DENIED"));
    assert_eq!(h.stored_state(draft.id).await, VersionState::Draft);

    // Publish alone activates.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::READ, actions::PUBLISH]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )));
    assert!(
        h.service
            .activate_version(&a, bundle.id, draft.id)
            .await
            .is_ok()
    );

    // Denied outright: listings are capability-denied, reads absent.
    h.pdp.set_mode(PdpMode::Deny);
    let err = h
        .service
        .list_bundles(&a, &ODataQuery::default())
        .await
        .unwrap_err();
    assert_eq!(reason_of(&err).as_deref(), Some("CAPABILITY_DENIED"));
    assert_eq!(
        h.service
            .get_bundle(&a, bundle.id)
            .await
            .unwrap_err()
            .category()
            .http_status(),
        404
    );

    // An unreachable PDP is an outage, never a grant.
    h.pdp.set_mode(PdpMode::Fail);
    for err in [
        h.service.get_bundle(&a, bundle.id).await.unwrap_err(),
        h.service
            .create_bundle(&a, NewBundle::new("c"))
            .await
            .unwrap_err(),
        h.service
            .activate_version(&a, bundle.id, draft.id)
            .await
            .unwrap_err(),
    ] {
        assert_eq!(err.category().http_status(), 503);
    }
}

#[tokio::test]
async fn bundle_names_and_descriptions_are_bounded() {
    let h = harness().await;
    let a = alice();
    let too_long = "n".repeat(limits::MAX_BUNDLE_NAME_BYTES + 1);
    for (bundle, field) in [
        (NewBundle::new("  "), "name"),
        (NewBundle::new("a\nb"), "name"),
        (NewBundle::new(too_long.clone()), "name"),
        (
            NewBundle::new("ok")
                .with_description("d".repeat(limits::MAX_BUNDLE_DESCRIPTION_BYTES + 1)),
            "description",
        ),
    ] {
        let err = h.service.create_bundle(&a, bundle).await.unwrap_err();
        assert!(
            matches!(&err, ManagementError::InvalidBundle { field: f, .. } if f == field),
            "{err:?}"
        );
    }

    let bundle = h
        .service
        .create_bundle(
            &a,
            NewBundle::new("n".repeat(limits::MAX_BUNDLE_NAME_BYTES)),
        )
        .await
        .unwrap();
    let rename = BundlePatch {
        name: Some(too_long),
        description: None,
    };
    let err = h
        .service
        .update_bundle(&a, bundle.id, rename)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ManagementError::InvalidBundle { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn document_names_resource_types_and_actions_are_bounded_on_write() {
    let h = harness().await;
    let a = alice();
    let (bundle, draft) = h.draft(&a, &[]).await;
    let write = |documents: Vec<DocumentSpec>| {
        let service = std::sync::Arc::clone(&h.service);
        let a = a.clone();
        async move {
            service
                .replace_draft_content(&a, bundle.id, draft.id, content(documents))
                .await
        }
    };

    let err = write(vec![DocumentSpec {
        name: "line\nbreak".to_owned(),
        ..spec("x")
    }])
    .await
    .unwrap_err();
    assert_eq!(
        err,
        ManagementError::InvalidDocumentName {
            name: "line\nbreak".to_owned()
        }
    );

    let long_name = "d".repeat(admission_control_sdk::MAX_DOCUMENT_NAME_LEN + 1);
    let too_many_actions = (0..=limits::MAX_ACTIONS_PER_DOCUMENT)
        .map(|n| format!("a{n}"))
        .collect();
    let err = write(vec![
        DocumentSpec {
            name: long_name,
            ..spec("x")
        },
        DocumentSpec {
            actions: too_many_actions,
            ..spec("y")
        },
    ])
    .await
    .unwrap_err();
    let ManagementError::ContentLimitExceeded { violations } = err else {
        panic!("expected a content limit violation: {err:?}");
    };
    let kinds: Vec<_> = violations.iter().map(|v| v.kind).collect();
    assert_eq!(
        kinds,
        [LimitKind::DocumentNameBytes, LimitKind::ActionsPerDocument]
    );
}

#[tokio::test]
async fn a_version_scoped_grant_validates_the_whole_version_before_activating() {
    let h = harness().await;
    let a = alice();
    let (bundle, draft) = h.draft(&a, &["main"]).await;
    h.service
        .replace_draft_content(&a, bundle.id, draft.id, content(vec![syntax_error("main")]))
        .await
        .unwrap();

    // The PDP grants publish on this version only: a resource constraint
    // that names the version, none of its documents.
    h.pdp.set_mode(PdpMode::AllowResources(vec![draft.id.0]));
    let refused = h
        .service
        .activate_version(&a, bundle.id, draft.id)
        .await
        .unwrap_err();
    assert_eq!(reason_of(&refused).as_deref(), Some("VALIDATION_FAILED"));
    assert_eq!(h.stored_state(draft.id).await, VersionState::Draft);

    // The same grant activates the version once its content is valid.
    h.pdp.set_mode(PdpMode::Allow(None));
    h.service
        .replace_draft_content(&a, bundle.id, draft.id, content(vec![spec("main")]))
        .await
        .unwrap();
    h.pdp.set_mode(PdpMode::AllowResources(vec![draft.id.0]));
    let active = h
        .service
        .activate_version(&a, bundle.id, draft.id)
        .await
        .unwrap();
    assert_eq!(active.state, sdk::VersionState::Active);
}

#[tokio::test]
async fn versions_are_listed_a_page_at_a_time_newest_first() {
    let h = harness().await;
    let a = alice();
    let (bundle, mut draft) = h.draft(&a, &["main"]).await;
    for _ in 0..2 {
        h.service
            .activate_version(&a, bundle.id, draft.id)
            .await
            .unwrap();
        draft = h
            .service
            .create_draft_version(&a, bundle.id, Some(draft.id))
            .await
            .unwrap();
    }

    let mut ordinals = Vec::new();
    let mut query = ODataQuery::default().with_limit(2);
    loop {
        let page = h
            .service
            .list_versions(&a, bundle.id, &query)
            .await
            .unwrap();
        assert!(page.items.len() <= 2);
        ordinals.extend(page.items.iter().map(|v| v.ordinal));
        match page.page_info.next_cursor {
            Some(cursor) => {
                query = ODataQuery::default()
                    .with_limit(2)
                    .with_cursor(toolkit_odata::CursorV1::decode(&cursor).unwrap());
            }
            None => break,
        }
    }
    assert_eq!(ordinals, [3, 2, 1]);
}
