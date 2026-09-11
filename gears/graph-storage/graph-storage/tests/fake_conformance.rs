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
/// sealed runner cannot express (DESIGN § 3.3). Keeping the case
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
    // DESIGN § Read Consistency Contract).

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

// --- payload projection (ADR-0003) ----------------------------------

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
    conformance::a_backward_compatible_change_updates_the_type_in_place(&store(), Uuid::now_v7())
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
    conformance::a_dry_run_reports_every_verdict_and_writes_nothing(&store(), Uuid::now_v7()).await;
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

// --- payload migrations ------------------------------------------------------

#[tokio::test]
async fn a_migration_moves_the_data_with_the_type() {
    conformance::a_migration_moves_the_data_with_the_type(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_migration_that_leaves_rows_invalid_is_refused_naming_them() {
    conformance::a_migration_that_leaves_rows_invalid_is_refused_naming_them(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn a_migration_without_a_schema_change_is_refused() {
    conformance::a_migration_without_a_schema_change_is_refused(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_migration_stamps_its_writer_and_moves_the_version() {
    conformance::a_migration_stamps_its_writer_and_moves_the_version(&store(), Uuid::now_v7())
        .await;
}

// --- source-namespace ownership ----------------------------------------------

#[tokio::test]
async fn a_source_namespace_is_claimed_by_its_first_writer() {
    conformance::a_source_namespace_is_claimed_by_its_first_writer(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn writing_under_another_producers_namespace_is_forbidden() {
    conformance::writing_under_another_producers_namespace_is_forbidden(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn a_transfer_moves_the_namespace_and_records_who_moved_it() {
    conformance::a_transfer_moves_the_namespace_and_records_who_moved_it(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn an_owned_nodes_source_field_claims_no_namespace() {
    conformance::an_owned_nodes_source_field_claims_no_namespace(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn readiness_reports_every_capability_and_only_some_block_service() {
    conformance::readiness_reports_every_capability_and_only_some_block_service(&store()).await;
}

// --- scope replacement --------------------------------------------------------

#[tokio::test]
async fn scope_replacement_removes_what_the_batch_no_longer_names() {
    conformance::scope_replacement_removes_what_the_batch_no_longer_names(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn scope_replacement_preserves_analysis_edges_and_their_endpoints() {
    conformance::scope_replacement_preserves_analysis_edges_and_their_endpoints(
        &store(),
        Uuid::now_v7(),
    )
    .await;
}

/// Multi-threaded on purpose: on the default single-threaded runtime the two
/// futures only interleave at await points, which is not the race the
/// obligation is about.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_replacements_of_one_scope_serialize() {
    conformance::two_replacements_of_one_scope_serialize(&store(), Uuid::now_v7()).await;
}

// --- both families, and the edge read -----------------------------------------

#[tokio::test]
async fn both_node_families_and_both_edge_families_round_trip() {
    conformance::both_node_families_and_both_edge_families_round_trip(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn an_edge_read_carries_the_envelope() {
    conformance::an_edge_read_carries_the_envelope(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn an_edge_whose_endpoint_is_hidden_is_not_readable() {
    conformance::an_edge_whose_endpoint_is_hidden_is_not_readable(
        &store(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn no_read_surface_answers_with_another_tenants_rows() {
    conformance::no_read_surface_answers_with_another_tenants_rows(
        &store(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test]
async fn an_edge_type_evolves_over_its_own_rows() {
    conformance::an_edge_type_evolves_over_its_own_rows(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_same_key_ingest_may_not_change_the_type() {
    conformance::a_same_key_ingest_may_not_change_the_type(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn per_item_outcomes_follow_the_batch_order() {
    conformance::per_item_outcomes_follow_the_batch_order(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn a_scope_and_an_idempotency_key_belong_to_their_producer() {
    conformance::a_scope_and_an_idempotency_key_belong_to_their_producer(&store(), Uuid::now_v7())
        .await;
}

#[tokio::test]
async fn a_type_pattern_narrows_search_and_a_hop() {
    conformance::a_type_pattern_narrows_search_and_a_hop(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn hybrid_search_fuses_both_arms() {
    conformance::hybrid_search_fuses_both_arms(&store(), Uuid::now_v7()).await;
}

#[tokio::test]
async fn deleting_an_already_tombstoned_row_is_a_no_op() {
    conformance::deleting_an_already_tombstoned_row_is_a_no_op(&store(), Uuid::now_v7()).await;
}
