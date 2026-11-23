//! The quota catalog store (P-D-270, P-D-271, P-D-272, P-D-274): mapping lineages, definition
//! versions applicable up to a SKU version, the current availability as the highest revision, and
//! the list's availability in one grouped read per page.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::quota::{CandidateFilter, QuotaCandidate};
use crate::infra::storage::repo::quota_intake_repo::{
    self as intake, quota_intake_repo_tests::RepoFixture,
};
use crate::test_support::products_statements;
use serde_json::json;
use toolkit_odata::CursorV1;

fn v(n: u64) -> QuotaVersion {
    QuotaVersion::new(n).unwrap()
}

/// What one definition version names.
struct Shape {
    sku: Uuid,
    sku_version: u64,
    subject: Uuid,
    profile: Option<Uuid>,
    resource_types: Vec<Uuid>,
}

impl Shape {
    fn at(sku: Uuid, sku_version: u64) -> Self {
        Self {
            sku,
            sku_version,
            subject: Uuid::from_u128(0x5b),
            profile: None,
            resource_types: vec![Uuid::from_u128(0x3e)],
        }
    }
}

async fn candidate(f: &RepoFixture) -> QuotaCandidate {
    f.staged_candidate().await
}

fn mapping(f: &RepoFixture, c: &QuotaCandidate, mapping_id: Uuid, version: u64) -> MappingVersion {
    MappingVersion {
        mapping_id,
        version: v(version),
        target_id: c.target_id,
        capability_key: c.capability_key.clone(),
        plugin_instance_id: "plugin-instance".to_owned(),
        target_key: "pool".to_owned(),
        mapping_key: "tenant-vcpu-mapping-3".to_owned(),
        mapping_json: json!({"mapping_key": "tenant-vcpu-mapping-3", "version": version}),
        digest: format!("{version}").repeat(64),
        profile_id: Uuid::from_u128(0x9f),
        profile_version: v(version),
        profile_json: json!({"profile": "bounded_allocation"}),
        profile_digest: "f".repeat(64),
        created_by: f.actor,
        created_at: f.now,
    }
}

fn availability(f: &RepoFixture, definition: Uuid, revision: u64, state: &str) -> Availability {
    let (state, reason) = match state {
        "enabled" => (QuotaAvailabilityState::Enabled, None),
        "withdrawn" => (
            QuotaAvailabilityState::Withdrawn,
            Some("withdrawn".to_owned()),
        ),
        _ => (
            QuotaAvailabilityState::Disabled,
            Some("qualification_pending".to_owned()),
        ),
    };
    Availability {
        definition_id: definition,
        version: v(1),
        revision: v(revision),
        state,
        reason_code: reason,
        qualification: None,
        observed_at: f.now,
        decided_by: f.actor,
    }
}

/// A target, a candidate, a mapping, the definition version 1 and its availability revision 1,
/// disabled until qualification.
async fn definition_of_shape(f: &RepoFixture, shape: Shape) -> Uuid {
    stored_of_shape(f, shape).await.0.definition_id
}

/// [`definition_of_shape`], answering the stored definition version and its mapping.
async fn stored_of_shape(f: &RepoFixture, shape: Shape) -> (DefinitionVersion, MappingVersion) {
    let c = candidate(f).await;
    let m = mapping(f, &c, Uuid::new_v4(), 1);
    insert_mapping(&f.conn(), &f.scope, f.tenant, &m)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let definition = DefinitionVersion {
        definition_id: id,
        version: v(1),
        candidate_id: c.id,
        sku_id: shape.sku,
        sku_version: v(shape.sku_version),
        metric_uuid: Uuid::from_u128(0x3c),
        subject_type_uuid: shape.subject,
        profile_id: shape.profile.unwrap_or(m.profile_id),
        mapping_id: m.mapping_id,
        mapping_version: m.version,
        content_json: json!({"sku_version": shape.sku_version.to_string()}),
        digest: "0".repeat(64),
        created_by: f.actor,
        created_at: f.now,
    };
    insert_definition(
        &f.conn(),
        &f.scope,
        f.tenant,
        &definition,
        &shape.resource_types,
    )
    .await
    .unwrap();
    insert_availability(
        &f.conn(),
        &f.scope,
        f.tenant,
        &availability(f, id, 1, "disabled"),
    )
    .await
    .unwrap();
    (definition, m)
}

async fn definition(f: &RepoFixture, sku: Uuid, sku_version: u64) -> Uuid {
    definition_of_shape(f, Shape::at(sku, sku_version)).await
}

fn filter(sku: Uuid, sku_version: u64) -> DefinitionFilter {
    DefinitionFilter {
        sku_id: sku,
        sku_version: v(sku_version),
        subject_type_uuid: None,
        resource_type_uuid: None,
        profile_id: None,
        enabled_only: false,
    }
}

async fn listed(f: &RepoFixture, filter: &DefinitionFilter) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = list_definitions(
        &f.conn(),
        &f.scope,
        f.tenant,
        filter,
        &ODataQuery::default(),
    )
    .await
    .unwrap()
    .items
    .into_iter()
    .map(|(d, _)| d.definition_id)
    .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn the_definition_list_answers_versions_up_to_the_sku_version() {
    let f = RepoFixture::new().await;
    let at_two = definition(&f, f.sku, 2).await;
    let at_four = definition(&f, f.sku, 4).await;
    let _other_sku = definition(&f, Uuid::new_v4(), 1).await;
    assert_eq!(listed(&f, &filter(f.sku, 1)).await, Vec::<Uuid>::new());
    assert_eq!(listed(&f, &filter(f.sku, 3)).await, vec![at_two]);
    let mut both = vec![at_two, at_four];
    both.sort();
    assert_eq!(listed(&f, &filter(f.sku, 4)).await, both);
    assert_eq!(
        listed(&f, &filter(f.sku, u64::MAX)).await,
        both,
        "a SKU version beyond the stored range keeps every stored one"
    );
    let other = Uuid::new_v4();
    let page = list_definitions(
        &f.conn(),
        &AccessScope::for_tenant(other),
        other,
        &filter(f.sku, 4),
        &ODataQuery::default(),
    )
    .await
    .unwrap();
    assert!(page.items.is_empty(), "another tenant lists none");
}

#[tokio::test]
async fn the_definition_list_filters_by_resource_type() {
    let f = RepoFixture::new().await;
    let (vm, volume) = (Uuid::from_u128(0x1), Uuid::from_u128(0x2));
    let both = definition_of_shape(
        &f,
        Shape {
            resource_types: vec![vm, volume, vm],
            ..Shape::at(f.sku, 1)
        },
    )
    .await;
    let vm_only = definition_of_shape(
        &f,
        Shape {
            resource_types: vec![vm],
            ..Shape::at(f.sku, 1)
        },
    )
    .await;
    let narrowed = |resource_type| DefinitionFilter {
        resource_type_uuid: Some(resource_type),
        ..filter(f.sku, 1)
    };
    let mut vms = vec![both, vm_only];
    vms.sort();
    assert_eq!(listed(&f, &narrowed(vm)).await, vms);
    assert_eq!(listed(&f, &narrowed(volume)).await, vec![both]);
    assert_eq!(
        listed(&f, &narrowed(Uuid::from_u128(0x3))).await,
        Vec::<Uuid>::new()
    );
}

#[tokio::test]
async fn the_definition_list_filters_by_subject_type_and_profile() {
    let f = RepoFixture::new().await;
    let (tenant_subject, project_subject) = (Uuid::from_u128(0xa), Uuid::from_u128(0xb));
    let profile = Uuid::from_u128(0xc);
    let of_tenant = definition_of_shape(
        &f,
        Shape {
            subject: tenant_subject,
            profile: Some(profile),
            ..Shape::at(f.sku, 1)
        },
    )
    .await;
    let of_project = definition_of_shape(
        &f,
        Shape {
            subject: project_subject,
            ..Shape::at(f.sku, 1)
        },
    )
    .await;
    assert_eq!(
        listed(
            &f,
            &DefinitionFilter {
                subject_type_uuid: Some(project_subject),
                ..filter(f.sku, 1)
            }
        )
        .await,
        vec![of_project]
    );
    assert_eq!(
        listed(
            &f,
            &DefinitionFilter {
                profile_id: Some(profile),
                ..filter(f.sku, 1)
            }
        )
        .await,
        vec![of_tenant]
    );
}

#[tokio::test]
async fn the_current_availability_is_the_highest_revision() {
    let f = RepoFixture::new().await;
    let enabled = definition(&f, f.sku, 1).await;
    let pending = definition(&f, f.sku, 1).await;
    let withdrawn = definition(&f, f.sku, 1).await;
    for (id, revision, state) in [
        (enabled, 2, "enabled"),
        (withdrawn, 2, "enabled"),
        (withdrawn, 3, "withdrawn"),
    ] {
        insert_availability(
            &f.conn(),
            &f.scope,
            f.tenant,
            &availability(&f, id, revision, state),
        )
        .await
        .unwrap();
    }
    for (id, revision, state) in [
        (enabled, 2, QuotaAvailabilityState::Enabled),
        (pending, 1, QuotaAvailabilityState::Disabled),
        (withdrawn, 3, QuotaAvailabilityState::Withdrawn),
    ] {
        let (definition, current) = find_definition(&f.conn(), &f.scope, f.tenant, id, v(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(definition.definition_id, id);
        assert_eq!(
            (current.revision, current.state),
            (v(revision), state),
            "{id}"
        );
    }
    let page = list_definitions(
        &f.conn(),
        &f.scope,
        f.tenant,
        &filter(f.sku, 1),
        &ODataQuery::default(),
    )
    .await
    .unwrap();
    for (definition, current) in &page.items {
        let expected = if definition.definition_id == enabled {
            v(2)
        } else if definition.definition_id == pending {
            v(1)
        } else {
            v(3)
        };
        assert_eq!(current.revision, expected, "{}", definition.definition_id);
    }
    assert_eq!(page.items.len(), 3);
    assert_eq!(
        listed(
            &f,
            &DefinitionFilter {
                enabled_only: true,
                ..filter(f.sku, 1)
            }
        )
        .await,
        vec![enabled],
        "only a version whose current availability is enabled"
    );
    let again = insert_availability(
        &f.conn(),
        &f.scope,
        f.tenant,
        &availability(&f, enabled, 2, "disabled"),
    )
    .await;
    assert!(
        matches!(
            again,
            Err(RepoError::Refused(RepoRefusal::QuotaApprovalContended))
        ),
        "{again:?}"
    );
}

#[tokio::test]
async fn the_list_reads_availability_in_one_statement_per_page() {
    let mut runs = Vec::new();
    for n in [1, 3] {
        let (f, recorder) = RepoFixture::recorded().await;
        for _ in 0..n {
            definition(&f, f.sku, 1).await;
        }
        recorder.clear();
        let page = list_definitions(
            &f.conn(),
            &f.scope,
            f.tenant,
            &filter(f.sku, 1),
            &ODataQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), n);
        let statements = products_statements(&recorder);
        assert_eq!(statements.len(), 2, "{statements:#?}");
        assert!(
            statements[1].contains("products_quota_availability"),
            "{statements:#?}"
        );
        runs.push(statements);
    }
    assert_eq!(
        runs[0], runs[1],
        "the same statements for one row and for three"
    );
}

/// The digests of a page's approved candidates are one statement whatever the page holds, and it
/// reads the digest only, never the content; a version not stored is absent from the answer.
#[tokio::test]
async fn the_definition_digests_of_a_page_are_one_statement() {
    for n in [1, 3] {
        let (f, recorder) = RepoFixture::recorded().await;
        let mut keys = Vec::new();
        for _ in 0..n {
            keys.push((definition(&f, f.sku, 1).await, v(1)));
        }
        keys.push((Uuid::new_v4(), v(1)));
        recorder.clear();
        let digests = definition_digests(&f.conn(), &f.scope, f.tenant, &keys)
            .await
            .unwrap();
        assert_eq!(digests.len(), n, "{digests:?}");
        assert!(
            digests.values().all(|d| *d == "0".repeat(64)),
            "{digests:?}"
        );
        let statements = products_statements(&recorder);
        assert_eq!(statements.len(), 1, "{statements:#?}");
        assert!(!statements[0].contains("content_json"), "{statements:#?}");
    }
}

#[tokio::test]
async fn the_definition_list_pages_by_definition_then_version() {
    let f = RepoFixture::new().await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(definition(&f, f.sku, 1).await);
    }
    ids.sort();
    let first = list_definitions(
        &f.conn(),
        &f.scope,
        f.tenant,
        &filter(f.sku, 1),
        &ODataQuery::default().with_limit(2),
    )
    .await
    .unwrap();
    let next = first.page_info.next_cursor.clone().unwrap();
    let second = list_definitions(
        &f.conn(),
        &f.scope,
        f.tenant,
        &filter(f.sku, 1),
        &ODataQuery::default()
            .with_limit(2)
            .with_cursor(CursorV1::decode(&next).unwrap()),
    )
    .await
    .unwrap();
    let read: Vec<Uuid> = first
        .items
        .into_iter()
        .chain(second.items)
        .map(|(d, _)| d.definition_id)
        .collect();
    assert_eq!(read, ids);
}

#[tokio::test]
async fn a_definition_reads_as_stored_and_versions_count_up() {
    let f = RepoFixture::new().await;
    let id = definition(&f, f.sku, 2).await;
    assert_eq!(
        latest_definition_version(&f.conn(), &f.scope, f.tenant, id)
            .await
            .unwrap(),
        Some(v(1))
    );
    assert_eq!(
        latest_definition_version(&f.conn(), &f.scope, f.tenant, Uuid::new_v4())
            .await
            .unwrap(),
        None
    );
    let (stored, _) = find_definition(&f.conn(), &f.scope, f.tenant, id, v(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stored.sku_version,
            stored.content_json.clone(),
            stored.digest.as_str()
        ),
        (v(2), json!({"sku_version": "2"}), "0".repeat(64).as_str())
    );
    let taken = insert_definition(
        &f.conn(),
        &f.scope,
        f.tenant,
        &DefinitionVersion {
            candidate_id: candidate(&f).await.id,
            ..stored.clone()
        },
        &[],
    )
    .await;
    assert!(
        matches!(
            taken,
            Err(RepoError::Refused(RepoRefusal::QuotaApprovalContended))
        ),
        "{taken:?}"
    );
    assert_eq!(RepoRefusal::QuotaApprovalContended.code(), "CONTENDED");
    assert_eq!(
        find_definition(&f.conn(), &f.scope, f.tenant, id, v(2))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        find_definition(&f.conn(), &f.scope, f.tenant, id, v(u64::MAX))
            .await
            .unwrap(),
        None,
        "a version beyond the stored range names no row"
    );
    let other = Uuid::new_v4();
    assert_eq!(
        find_definition(&f.conn(), &AccessScope::for_tenant(other), other, id, v(1))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_definition_without_availability_is_corrupt_not_missing() {
    let f = RepoFixture::new().await;
    let c = candidate(&f).await;
    let m = mapping(&f, &c, Uuid::new_v4(), 1);
    insert_mapping(&f.conn(), &f.scope, f.tenant, &m)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    insert_definition(
        &f.conn(),
        &f.scope,
        f.tenant,
        &DefinitionVersion {
            definition_id: id,
            version: v(1),
            candidate_id: c.id,
            sku_id: f.sku,
            sku_version: v(1),
            metric_uuid: Uuid::from_u128(0x3c),
            subject_type_uuid: Uuid::from_u128(0x5b),
            profile_id: m.profile_id,
            mapping_id: m.mapping_id,
            mapping_version: m.version,
            content_json: json!({}),
            digest: "0".repeat(64),
            created_by: f.actor,
            created_at: f.now,
        },
        &[],
    )
    .await
    .unwrap();
    let error = find_definition(&f.conn(), &f.scope, f.tenant, id, v(1))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(matches!(error, RepoError::CorruptRow(_)), "{text}");
    assert!(
        text.contains(&id.to_string()) && text.contains(&f.tenant.to_string()),
        "{text}"
    );
    assert!(matches!(
        list_definitions(
            &f.conn(),
            &f.scope,
            f.tenant,
            &filter(f.sku, 1),
            &ODataQuery::default()
        )
        .await,
        Err(SkuListError::Repo(RepoError::CorruptRow(_)))
    ));
}

#[tokio::test]
async fn the_latest_mapping_of_each_lineage_reads_back() {
    let f = RepoFixture::new().await;
    let c = candidate(&f).await;
    let lineage = Uuid::new_v4();
    for version in [1, 2] {
        insert_mapping(
            &f.conn(),
            &f.scope,
            f.tenant,
            &mapping(&f, &c, lineage, version),
        )
        .await
        .unwrap();
    }
    let mut second_key = c.clone();
    second_key.capability_key = "tenant-ram".to_owned();
    let ram = mapping(&f, &second_key, Uuid::new_v4(), 1);
    insert_mapping(&f.conn(), &f.scope, f.tenant, &ram)
        .await
        .unwrap();
    let latest = latest_mapping(
        &f.conn(),
        &f.scope,
        f.tenant,
        c.target_id,
        &c.capability_key,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(latest, mapping(&f, &c, lineage, 2));
    assert_eq!(
        latest_mapping(&f.conn(), &f.scope, f.tenant, c.target_id, "absent")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        approved_mappings_of_target(&f.conn(), &f.scope, f.tenant, c.target_id)
            .await
            .unwrap(),
        vec![ram, latest],
        "the latest of each lineage, by capability key"
    );
    let again = insert_mapping(&f.conn(), &f.scope, f.tenant, &mapping(&f, &c, lineage, 2)).await;
    assert!(
        matches!(
            again,
            Err(RepoError::Refused(RepoRefusal::QuotaApprovalContended))
        ),
        "{again:?}"
    );
    // The candidate list is untouched by the catalog's writes.
    let staged = intake::list_candidates(
        &f.conn(),
        &f.scope,
        f.tenant,
        &CandidateFilter::default(),
        &ODataQuery::default(),
    )
    .await
    .unwrap();
    assert_eq!(staged.items.len(), 1);
}

/// A version above the `bigint` range is refused at the write, never wrapped into a negative
/// number, and nothing is stored.
#[tokio::test]
async fn a_version_beyond_the_stored_range_is_refused_not_wrapped() {
    let f = RepoFixture::new().await;
    let id = definition(&f, f.sku, 1).await;
    let beyond = u64::try_from(i64::MAX).unwrap() + 1;
    let refused = insert_availability(
        &f.conn(),
        &f.scope,
        f.tenant,
        &availability(&f, id, beyond, "enabled"),
    )
    .await;
    assert!(matches!(refused, Err(RepoError::Db(_))), "{refused:?}");
    let (_, current) = find_definition(&f.conn(), &f.scope, f.tenant, id, v(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.revision, v(1));
}

/// The definition versions of a set and their availability are two statements whatever the set
/// holds, and each version answers its own availability (P-D-275).
#[tokio::test]
async fn a_set_of_versions_reads_with_its_availability_in_two_statements() {
    for n in [1, 3] {
        let (f, recorder) = RepoFixture::recorded().await;
        let mut keys = Vec::new();
        for _ in 0..n {
            let (row, _) = stored_of_shape(&f, Shape::at(f.sku, 1)).await;
            keys.push((row.definition_id, row.version));
        }
        let enabled = keys[0].0;
        insert_availability(
            &f.conn(),
            &f.scope,
            f.tenant,
            &availability(&f, enabled, 2, "enabled"),
        )
        .await
        .unwrap();
        recorder.clear();
        let rows = find_definitions(&f.conn(), &f.scope, f.tenant, &keys)
            .await
            .unwrap();
        assert_eq!(rows.len(), n);
        for (definition, current) in &rows {
            assert_eq!(current.definition_id, definition.definition_id);
            let expected = if definition.definition_id == enabled {
                (v(2), QuotaAvailabilityState::Enabled)
            } else {
                (v(1), QuotaAvailabilityState::Disabled)
            };
            assert_eq!((current.revision, current.state), expected);
        }
        let statements = products_statements(&recorder);
        assert_eq!(statements.len(), 2, "{statements:#?}");
        assert!(
            statements[1].contains("products_quota_availability"),
            "{statements:#?}"
        );
    }
}

/// A version not stored, one beyond the stored range, or another tenant's is absent from the
/// answer; another tenant's scope reads none of the caller's.
#[tokio::test]
async fn a_version_not_stored_or_of_another_tenant_is_absent_from_the_set_read() {
    let mut f = RepoFixture::new().await;
    let (mine, _) = stored_of_shape(&f, Shape::at(f.sku, 1)).await;
    let (tenant, scope) = (f.tenant, f.scope.clone());
    let other = Uuid::new_v4();
    f.tenant = other;
    f.scope = AccessScope::for_tenant(other);
    let (theirs, _) = stored_of_shape(&f, Shape::at(f.sku, 1)).await;
    f.tenant = tenant;
    f.scope = scope;
    let keys = [
        (mine.definition_id, mine.version),
        (theirs.definition_id, theirs.version),
        (Uuid::new_v4(), v(1)),
        (mine.definition_id, v(u64::MAX)),
    ];
    let rows = find_definitions(&f.conn(), &f.scope, f.tenant, &keys)
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|(d, _)| d.definition_id)
            .collect::<Vec<_>>(),
        [mine.definition_id]
    );
    assert_eq!(rows[0].0, mine, "the version as stored");
    let foreign = find_definitions(&f.conn(), &AccessScope::for_tenant(other), f.tenant, &keys)
        .await
        .unwrap();
    assert!(foreign.is_empty(), "another tenant's scope reads none");
    assert!(
        find_definitions(&f.conn(), &f.scope, f.tenant, &[])
            .await
            .unwrap()
            .is_empty()
    );
}

/// The set read matches each id with its own version, pair by pair: with definition A at
/// versions 1 and 2 and B at version 1, asking for A at 2 and B at 1 answers exactly those two,
/// each with its own availability, never A at 1 or a cross product of the ids and versions.
#[tokio::test]
async fn a_set_read_matches_each_definition_with_its_own_version() {
    let f = RepoFixture::new().await;
    let (a, _) = stored_of_shape(&f, Shape::at(f.sku, 1)).await;
    let a2 = DefinitionVersion {
        version: v(2),
        candidate_id: candidate(&f).await.id,
        ..a.clone()
    };
    insert_definition(&f.conn(), &f.scope, f.tenant, &a2, &[Uuid::from_u128(0x3e)])
        .await
        .unwrap();
    insert_availability(
        &f.conn(),
        &f.scope,
        f.tenant,
        &Availability {
            version: v(2),
            ..availability(&f, a.definition_id, 1, "enabled")
        },
    )
    .await
    .unwrap();
    let (b, _) = stored_of_shape(&f, Shape::at(f.sku, 1)).await;
    let rows = find_definitions(
        &f.conn(),
        &f.scope,
        f.tenant,
        &[(a.definition_id, v(2)), (b.definition_id, v(1))],
    )
    .await
    .unwrap();
    let mut found: Vec<(Uuid, QuotaVersion, QuotaVersion, QuotaAvailabilityState)> = rows
        .iter()
        .map(|(d, current)| (d.definition_id, d.version, current.version, current.state))
        .collect();
    found.sort_by_key(|row| (row.0, row.1));
    let mut expected = vec![
        (a.definition_id, v(2), v(2), QuotaAvailabilityState::Enabled),
        (
            b.definition_id,
            v(1),
            v(1),
            QuotaAvailabilityState::Disabled,
        ),
    ];
    expected.sort_by_key(|row| (row.0, row.1));
    assert_eq!(found, expected);
}

/// The adapter target and the profile content of each mapping version a set names, in one
/// statement; a version not stored is absent.
#[tokio::test]
async fn the_mapping_facts_of_a_set_read_in_one_statement() {
    let (f, recorder) = RepoFixture::recorded().await;
    let (row, m) = stored_of_shape(&f, Shape::at(f.sku, 1)).await;
    recorder.clear();
    let facts = mapping_facts(
        &f.conn(),
        &f.scope,
        f.tenant,
        &[
            (row.mapping_id, row.mapping_version),
            (Uuid::new_v4(), v(1)),
            (row.mapping_id, v(u64::MAX)),
        ],
    )
    .await
    .unwrap();
    assert_eq!(facts.len(), 1, "{facts:?}");
    let fact = &facts[&(row.mapping_id, row.mapping_version)];
    assert_eq!(fact.target_id, m.target_id);
    assert_eq!(fact.profile["profile"], "bounded_allocation");
    assert_eq!(fact.profile, m.profile_json);
    let statements = products_statements(&recorder);
    assert_eq!(statements.len(), 1, "{statements:#?}");
    assert!(
        statements[0].contains("products_quota_mapping_version")
            && !statements[0].contains("mapping_json"),
        "{statements:#?}"
    );
    let other = Uuid::new_v4();
    let foreign = mapping_facts(
        &f.conn(),
        &AccessScope::for_tenant(other),
        other,
        &[(row.mapping_id, row.mapping_version)],
    )
    .await
    .unwrap();
    assert!(foreign.is_empty(), "another tenant reads none");
}
