#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The conformance suite against the in-memory fake.
//!
//! This lane needs no database, so it runs everywhere the workspace builds —
//! which is the point: if an obligation only the `PostgreSQL` store can satisfy
//! sneaks into the contract, it fails here first.

mod conformance;

use graph_storage::infra::fake_store::FakeGraphStore;
use graph_storage_sdk::models::ProjectionRequest;
use uuid::Uuid;

fn store() -> FakeGraphStore {
    FakeGraphStore::new()
}

#[tokio::test]
async fn a_failed_batch_commits_nothing() {
    conformance::batch_atomicity(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn source_generations_are_fenced_monotonically() {
    conformance::generation_fencing(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_node_never_outlives_its_incident_edges() {
    conformance::no_orphan_edges(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_fresh_tenant_reports_a_usable_revision() {
    conformance::a_fresh_tenant_reports_a_usable_revision(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn materializing_a_phantom_revalidates_its_edges() {
    conformance::materializing_a_phantom_revalidates_its_edges(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn an_edge_type_refuses_an_endpoint_it_does_not_admit() {
    conformance::endpoint_constraints_are_enforced(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_recorded_idempotency_key_replays() {
    conformance::idempotency(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn an_identical_batch_converges_without_moving_the_revision() {
    conformance::convergent_replay(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn an_unchanged_re_ingest_embeds_nothing() {
    conformance::an_unchanged_re_ingest_embeds_nothing(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn colliding_node_keys_stay_inside_their_tenants() {
    conformance::tenant_isolation(&store(), Uuid::now_v7(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn tombstoned_rows_are_absent_from_every_read_path() {
    conformance::tombstones_are_invisible(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_denied_row_reads_exactly_like_an_absent_one() {
    conformance::denied_is_indistinguishable_from_absent(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_denying_scope_ranks_nothing() {
    conformance::search_is_scoped(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_document_is_retrieved_by_its_own_text() {
    conformance::a_document_is_retrieved_by_its_own_text(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_declared_path_reaches_the_vector() {
    conformance::a_declared_path_reaches_the_vector(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_skipped_re_ingest_preserves_the_vector() {
    conformance::a_skipped_re_ingest_preserves_the_vector(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_stale_vector_stops_ranking_but_the_node_stays() {
    conformance::a_stale_vector_stops_ranking_but_the_node_stays(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn only_the_active_epoch_ranks() {
    conformance::only_the_active_epoch_ranks(&store(), Uuid::now_v7()).await;
}

/// Obligation 5, which only the fake can currently satisfy: two arms of one
/// read and a hydration after it observe one graph state.
///
/// The built-in `PostgreSQL` store declares this capability **absent** — a true
/// repeatable-read snapshot needs a transaction held across calls, which the
/// sealed runner cannot express (see `dev/DEVIATIONS.md`). Keeping the case
/// here, against the implementation that does honour it, is what stops the
/// obligation from quietly disappearing from the contract.
#[tokio::test]
async fn one_snapshot_spans_every_arm_of_one_read() {
    use graph_storage_sdk::plugin_api::GraphStoreV1;
    use toolkit_security::AccessScope;

    let store = store();
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);

    store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("ontology registers");
    conformance::ingest_batch(
        &store,
        &ctx,
        conformance::batch(vec![conformance::node("snap-1", "before")], Vec::new()),
    )
    .await
    .expect("the batch commits");

    let snapshot = store.begin_read(&ctx).await.expect("snapshot opens");

    // A concurrent commit lands between the arms of the compound read.
    conformance::ingest_batch(
        &store,
        &ctx,
        conformance::batch(vec![conformance::node("snap-2", "after")], Vec::new()),
    )
    .await
    .expect("the concurrent batch commits");

    let under = conformance::ctx(tenant, &scope, Some(&snapshot));
    let page = store
        .project_table(&under, ProjectionRequest::default())
        .await
        .expect("projection succeeds");
    assert!(
        page.items.iter().all(|row| row.node_key != "snap-2"),
        "the snapshot must not see a row committed after it opened: {:?}",
        page.items
    );
    // The platform `Page` has no revision slot, so the compound read's
    // revision is asserted through the arms that do carry it (see
    // `dev/DEVIATIONS.md` D-005).

    let resolved = store
        .resolve_node_ids(&under, &["snap-2".to_owned()])
        .await
        .expect("resolution succeeds");
    assert!(
        resolved.is_empty(),
        "a second arm of the same read observes the same state"
    );

    store.end_read(snapshot).await.expect("snapshot closes");
}

#[tokio::test]
async fn the_envelope_records_the_subject_of_each_verb() {
    conformance::the_envelope_records_the_subject_of_each_verb(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_projection_row_carries_the_envelope() {
    conformance::a_projection_row_carries_the_envelope(&store(), Uuid::now_v7()).await;
}

// --- payload projection (DEVIATIONS D-104) ----------------------------------

#[tokio::test]
async fn a_declared_payload_path_filters_and_orders_the_projection() {
    conformance::a_declared_payload_path_filters_and_orders_the_projection(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn an_undeclared_payload_path_is_refused_naming_the_alternatives() {
    conformance::an_undeclared_payload_path_is_refused_naming_the_alternatives(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn an_index_path_onto_a_non_scalar_is_refused_at_registration() {
    conformance::an_index_path_onto_a_non_scalar_is_refused_at_registration(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn a_deeper_chain_registers_and_its_ancestor_admits_the_leaf() {
    conformance::a_deeper_chain_registers_and_its_ancestor_admits_the_leaf(
        &store().with_max_chain_depth(8),
        Uuid::now_v7(),
    )
    .await;
}

// --- type evolution (registering a changed schema in place) -----------------

#[tokio::test]
async fn a_backward_compatible_change_updates_the_type_in_place() {
    conformance::a_backward_compatible_change_updates_the_type_in_place(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn an_incompatible_change_is_refused_with_its_location() {
    conformance::an_incompatible_change_is_refused_with_its_location(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn a_changed_schema_is_still_a_conflict_by_default() {
    conformance::a_changed_schema_is_still_a_conflict_by_default(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_dry_run_reports_every_verdict_and_writes_nothing() {
    conformance::a_dry_run_reports_every_verdict_and_writes_nothing(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn a_change_the_schemas_cannot_prove_is_admitted_when_the_rows_fit() {
    conformance::a_change_the_schemas_cannot_prove_is_admitted_when_the_rows_fit(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn a_change_the_stored_rows_contradict_is_refused_naming_them() {
    conformance::a_change_the_stored_rows_contradict_is_refused_naming_them(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn an_accepted_type_update_advances_the_graph_revision() {
    conformance::an_accepted_type_update_advances_the_graph_revision(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn a_new_index_path_becomes_filterable_without_recreating_the_type() {
    conformance::a_new_index_path_becomes_filterable_without_recreating_the_type(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}
