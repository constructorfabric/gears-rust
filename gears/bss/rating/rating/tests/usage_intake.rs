//! Usage capture and normalization (feature 04) against the contract fakes on SQLite: the
//! acceptance vectors this feature owns that do not need row locks or concurrent writers (those —
//! V01, V04, V68, V76 — also run on PostgreSQL in `postgres_usage_intake.rs`). V63, V69 and V70
//! need counting (feature 05) and corrections (feature 08).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bss_rating::domain::error::DomainError;
use bss_rating::domain::ports::Clock;
use bss_rating::domain::usage::feed::FeedError;
use bss_rating::domain::usage::intake::{RestartOutcome, RetryOutcome};
use bss_rating::domain::usage::loss::{LossKind, LossState};
use bss_rating::domain::usage::normalize::RejectReason;
use bss_rating::domain::usage::reader::{PauseReason, Step};
use bss_rating::domain::usage::source::SourceState;
use bss_rating::infra::feed_grant::{COLLECTOR_USAGE_RECORD, READ_FEED};
use bss_rating::infra::storage::repo::usage_repo;
use bss_rating::test_providers::MetricSample;
use bss_rating::test_providers::usage::{
    RecordSpec, invalidation_of, meter, qty, record_entry, sum_declaration,
};
use common::usage::{
    HOUR, T1, T2, checkpoint, committed, configure, fixture, hourly, id_of, identity, losses,
    open_exceptions, records, scope, source, step, stored, try_step,
};
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn v01_a_page_applied_twice_has_no_second_effect() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let (a, b) = (hourly(T1, "k1", "8"), hourly(T1, "k2", "3"));
    f.feed.push([a.clone(), b.clone()]);
    let first = committed(step(&f, &s).await);
    assert_eq!((first.captured, first.duplicates), (2, 0));
    let stored_a = stored(&f, &a).await.unwrap();
    // The collector delivers at least once: the same entries come again.
    f.feed.push([a.clone(), b.clone()]);
    let second = committed(step(&f, &s).await);
    assert_eq!((second.captured, second.duplicates), (0, 2));
    assert_eq!(f.contributions.interpreted(), vec![id_of(&a), id_of(&b)]);
    assert_eq!(stored(&f, &a).await.unwrap(), stored_a);
}

#[tokio::test]
async fn v03_a_rejected_entry_is_retried_from_its_stored_row_after_retention() {
    let f = fixture().await;
    // The cache holds a declaration that lacks the entry's metadata key.
    f.registry
        .declare(&meter("hours"), sum_declaration("h", &[]));
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let mut spec = RecordSpec::hourly(T1, &meter("hours"), "k1", HOUR).quantity(&qty("8"));
    spec.metadata.insert("region".to_owned(), "eu".to_owned());
    let entry = record_entry(&spec);
    f.feed.push([entry.clone()]);
    let report = committed(step(&f, &s).await);
    assert_eq!(report.rejected.get(&RejectReason::UsageMalformed), Some(&1));
    let row = stored(&f, &entry).await.unwrap();
    assert_eq!(row.contribution_state, "rejected");
    assert_eq!(row.reject_reason.as_deref(), Some("usage_malformed"));
    assert_eq!(
        open_exceptions(&f, T1).await,
        vec![(id_of(&entry).to_string(), "usage_malformed".to_owned())]
    );
    // The collector's retention removes the entry; the Rating-side fix lands.
    f.feed.expire_before(1);
    f.registry
        .declare(&meter("hours"), sum_declaration("h", &["region"]));
    let mut fresh = BTreeMap::new();
    fresh.insert(
        meter("hours"),
        f.declarations.resolve_fresh(&meter("hours")).await.unwrap(),
    );
    let reads = f.feed.reads();
    let outcome = f
        .intake
        .retry_rejected(f.db(), scope(&f, T1).await, identity(&entry), fresh.clone())
        .await
        .unwrap();
    assert_eq!(outcome, RetryOutcome::Interpreted);
    assert_eq!(f.feed.reads(), reads, "a retry never reads the collector");
    let row = stored(&f, &entry).await.unwrap();
    assert_eq!(row.contribution_state, "captured");
    assert_eq!(row.reject_reason, None);
    assert_eq!(row.gts_type.as_deref(), Some(meter("hours").as_str()));
    assert!(open_exceptions(&f, T1).await.is_empty());
    assert_eq!(f.contributions.interpreted(), vec![id_of(&entry)]);
    // Handed on once: a second retry finds nothing rejected.
    let again = f
        .intake
        .retry_rejected(f.db(), scope(&f, T1).await, identity(&entry), fresh)
        .await
        .unwrap();
    assert!(matches!(again, RetryOutcome::NotRejected(_)));
    assert_eq!(f.contributions.interpreted().len(), 1);
}

#[tokio::test]
async fn v04_a_page_that_fails_before_commit_leaves_nothing_and_is_read_again() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let (a, b) = (hourly(T1, "k1", "8"), hourly(T1, "k2", "3"));
    f.feed.push([a.clone(), b.clone()]);
    f.contributions.fail.store(true, Ordering::SeqCst);
    assert!(try_step(&f, &s).await.is_err());
    assert!(records(&f, T1).await.is_empty(), "nothing persisted");
    assert_eq!(checkpoint(&f, &s).await.cursor, None, "the cursor stays");
    f.contributions.fail.store(false, Ordering::SeqCst);
    let report = committed(step(&f, &s).await);
    assert_eq!(report.captured, 2);
    assert_eq!(records(&f, T1).await.len(), 2);
}

#[tokio::test]
async fn v07_a_changed_meter_set_reads_a_new_source_from_oldest() {
    let f = fixture().await;
    let old = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&old)).await;
    let (a, b) = (hourly(T1, "k1", "8"), hourly(T1, "k2", "3"));
    f.feed.push([a, b]);
    committed(step(&f, &old).await);
    let old_cursor = checkpoint(&f, &old).await.cursor;
    assert!(old_cursor.is_some());

    let new = source(&["hours", "gb"], &[T1]);
    let readable = configure(&f, std::slice::from_ref(&new)).await;
    assert_eq!(
        readable.len(),
        2,
        "the old source drains, the new one is read"
    );
    assert_eq!(checkpoint(&f, &old).await.state, SourceState::Draining);
    assert_eq!(
        checkpoint(&f, &new).await.cursor,
        None,
        "the new source starts at Oldest"
    );

    let gb = record_entry(&RecordSpec::hourly(T1, &meter("gb"), "g1", HOUR));
    f.feed.push([gb]);
    // The new source re-reads the overlap from Oldest; dedup absorbs it. Its own cursors only: a
    // cursor of the old subscription would be refused as FILTER_MISMATCH.
    let first = committed(step(&f, &new).await);
    assert_eq!((first.captured, first.duplicates), (1, 2));
    // The old source reads to its current end and retires.
    assert_eq!(step(&f, &old).await, Step::Drained);
    assert_eq!(checkpoint(&f, &old).await.state, SourceState::Retired);
    assert_eq!(step(&f, &old).await, Step::Retired);
}

#[tokio::test]
async fn v62_an_invalidation_before_its_target_withdraws_it_on_capture() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let target = hourly(T1, "k1", "8");
    let invalidation = invalidation_of(&target, "DUPLICATE_EMISSION");
    f.feed.push([invalidation.clone()]);
    let report = committed(step(&f, &s).await);
    assert_eq!(
        (report.invalidations_applied, report.targets_unknown),
        (1, 1)
    );
    let row = stored(&f, &invalidation).await.unwrap();
    assert_eq!(row.contribution_state, "applied");
    assert_eq!(row.entry_type.as_deref(), Some("invalidation"));
    assert_eq!(row.invalidates_id, Some(id_of(&target)));
    assert_eq!(
        open_exceptions(&f, T1).await,
        vec![(
            id_of(&target).to_string(),
            "invalidation_target_unknown".to_owned()
        )]
    );
    // The target arrives later (through a source whose meters overlap, say).
    f.feed.push([target.clone()]);
    let report = committed(step(&f, &s).await);
    assert_eq!(report.withdrawn_on_capture, 1);
    let row = stored(&f, &target).await.unwrap();
    assert_eq!(row.contribution_state, "withdrawn");
    assert_eq!(row.withdrawn_by, Some(id_of(&invalidation)));
    assert!(f.contributions.interpreted().is_empty(), "never counted");
    assert!(
        open_exceptions(&f, T1).await.is_empty(),
        "the exception closes"
    );
}

#[tokio::test]
async fn an_invalidation_after_its_target_withdraws_it_once() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let target = hourly(T1, "k1", "8");
    let invalidation = invalidation_of(&target, "DUPLICATE_EMISSION");
    f.feed.push([target.clone(), invalidation.clone()]);
    let report = committed(step(&f, &s).await);
    assert_eq!(
        (report.invalidations_applied, report.targets_unknown),
        (1, 0)
    );
    assert_eq!(
        stored(&f, &target).await.unwrap().contribution_state,
        "withdrawn"
    );
    // A replayed invalidation is absorbed by its id.
    f.feed.push([invalidation.clone()]);
    let report = committed(step(&f, &s).await);
    assert_eq!((report.duplicates, report.invalidations_applied), (1, 0));
    assert_eq!(
        stored(&f, &target).await.unwrap().withdrawn_by,
        Some(id_of(&invalidation))
    );
}

#[tokio::test]
async fn an_invalidation_withdraws_a_rejected_target_and_closes_its_exception() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let mut target = hourly(T1, "k1", "8");
    target["metadata"] = json!({"undeclared": "x"});
    let invalidation = invalidation_of(&target, "BAD_METADATA");
    f.feed.push([target.clone()]);
    committed(step(&f, &s).await);
    assert_eq!(open_exceptions(&f, T1).await.len(), 1);
    // The emitter invalidates the entry; the invalidation itself carries the same metadata, so it
    // is rejected too unless the meter declares it: declare it first.
    f.registry
        .declare(&meter("hours"), sum_declaration("h", &["undeclared"]));
    f.declarations.resolve_fresh(&meter("hours")).await.unwrap();
    f.feed.push([invalidation]);
    committed(step(&f, &s).await);
    let row = stored(&f, &target).await.unwrap();
    assert_eq!(row.contribution_state, "withdrawn");
    assert_eq!(row.reject_reason.as_deref(), Some("usage_malformed"));
    assert!(open_exceptions(&f, T1).await.is_empty());
}

#[tokio::test]
async fn v64_an_entry_reread_with_other_content_leaves_the_stored_row() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let original = hourly(T1, "k1", "8");
    f.feed.push([original.clone()]);
    committed(step(&f, &s).await);
    let before = stored(&f, &original).await.unwrap();
    let mut changed = original.clone();
    changed["quantity"] = json!("9");
    f.feed.push([changed]);
    let report = committed(step(&f, &s).await);
    assert_eq!((report.content_conflicts, report.captured), (1, 0));
    assert_eq!(stored(&f, &original).await.unwrap(), before);
    assert_eq!(
        f.contributions.interpreted().len(),
        1,
        "no normalization follows"
    );
    assert_eq!(
        open_exceptions(&f, T1).await,
        vec![(
            id_of(&original).to_string(),
            "usage_content_conflict".to_owned()
        )]
    );
}

#[tokio::test]
async fn v65_an_unidentifiable_entry_is_a_capture_reject_and_one_loss() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let broken = [
        json!({"id": "not-a-uuid", "gts_type_id": meter("hours").as_str()}),
        json!({"quantity": "1"}),
    ];
    let good = hourly(T1, "k1", "8");
    f.feed.push([broken[0].clone(), broken[1].clone(), good]);
    let report = committed(step(&f, &s).await);
    assert_eq!((report.unidentifiable, report.captured), (2, 1));
    let conn = f.db().conn().unwrap();
    let rejects = usage_repo::list_capture_rejects(&conn, s.source_id())
        .await
        .unwrap();
    assert_eq!(rejects.len(), 2);
    let losses = losses(&f, &s).await;
    assert_eq!(
        losses.len(),
        1,
        "one loss over the source scope for the page"
    );
    assert_eq!(losses[0].kind, LossKind::UnidentifiableEntry);
    assert_eq!(losses[0].state, LossState::Open);
    assert!(
        rejects
            .iter()
            .all(|reject| reject.loss_id == Some(losses[0].loss_id))
    );
    assert!(
        checkpoint(&f, &s).await.cursor.is_some(),
        "the cursor advances"
    );
    assert!(f.metrics.samples().contains(&MetricSample::SourceLosses(
        s.source_id(),
        LossKind::UnidentifiableEntry,
        1
    )));
}

#[tokio::test]
async fn v66_a_widened_scope_is_a_new_source_with_a_loss_for_the_new_tenant() {
    let f = fixture().await;
    let narrow = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&narrow)).await;
    let wide = source(&["hours"], &[T1, T2]);
    configure(&f, std::slice::from_ref(&wide)).await;
    assert_eq!(checkpoint(&f, &wide).await.cursor, None, "from Oldest");
    assert_eq!(checkpoint(&f, &narrow).await.state, SourceState::Draining);
    let losses = losses(&f, &wide).await;
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].kind, LossKind::ScopeWidenedBeyondRetention);
    assert_eq!(losses[0].tenant_id, T1, "owned by the scope's owner root");
    // Reconciling again changes nothing.
    configure(&f, std::slice::from_ref(&wide)).await;
    assert_eq!(self::losses(&f, &wide).await.len(), 1);
}

#[tokio::test]
async fn v67_backpressure_stops_fetching_and_resumes_below_eighty_percent() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let entries: Vec<Value> = (0..5).map(|i| hourly(T1, &format!("k{i}"), "1")).collect();
    f.feed.push(entries.clone());
    f.depth.set(101);
    let reads = f.feed.reads();
    assert_eq!(step(&f, &s).await, Step::Paused(PauseReason::Backpressure));
    f.depth.set(85);
    assert_eq!(step(&f, &s).await, Step::Paused(PauseReason::Backpressure));
    assert_eq!(f.feed.reads(), reads, "no page is fetched while paused");
    f.depth.set(79);
    assert_eq!(committed(step(&f, &s).await).captured, 3);
    assert_eq!(committed(step(&f, &s).await).captured, 2);
    assert_eq!(records(&f, T1).await.len(), 5, "every entry captured once");
    let paused: Vec<bool> = f
        .metrics
        .samples()
        .into_iter()
        .filter_map(|sample| match sample {
            MetricSample::FeedPaused(_, PauseReason::Backpressure, paused) => Some(paused),
            _ => None,
        })
        .collect();
    assert_eq!(paused, vec![true, false]);
}

#[tokio::test]
async fn v71_a_cursor_beyond_retention_opens_a_bounded_loss_and_restarts_from_oldest() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let entries: Vec<Value> = (0..5).map(|i| hourly(T1, &format!("k{i}"), "1")).collect();
    f.feed.push(entries);
    committed(step(&f, &s).await);
    let last_good = checkpoint(&f, &s).await;
    // The page after it is stuck for 130 days; retention removes what the reader never read.
    f.clock.advance(Duration::from_hours(130 * 24));
    f.feed.expire_before(4);
    assert_eq!(step(&f, &s).await, Step::Restarted);
    let losses = losses(&f, &s).await;
    assert_eq!(losses.len(), 1);
    let loss = &losses[0];
    assert_eq!(loss.kind, LossKind::CursorBeyondRetention);
    assert_eq!(loss.last_good_cursor, last_good.cursor);
    let range = loss.tainted.ranges()[&meter("hours")][0];
    let floor = time::Duration::days(125);
    assert_eq!(range.first(), (last_good.updated_at - floor).date());
    assert_eq!(range.last(), (f.clock.now() - floor).date());
    assert_eq!(checkpoint(&f, &s).await.cursor, None, "restarted at Oldest");
    // The recent days are re-read from Oldest; the loss stays open.
    let report = committed(step(&f, &s).await);
    assert_eq!(report.captured, 1);
    assert_eq!(self::losses(&f, &s).await[0].state, LossState::Open);
    assert!(
        f.metrics
            .samples()
            .contains(&MetricSample::FeedRestart(s.source_id()))
    );
}

#[tokio::test]
async fn v72_a_narrowed_grant_pauses_the_source_and_skips_nothing() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1, T2]);
    configure(&f, std::slice::from_ref(&s)).await;
    let entries = [hourly(T1, "k1", "1"), hourly(T2, "k2", "2")];
    f.feed.push(entries.clone());
    f.pdp.put_outside_grant(T2);
    assert_eq!(step(&f, &s).await, Step::Paused(PauseReason::GrantNarrowed));
    assert_eq!(checkpoint(&f, &s).await.cursor, None, "nothing committed");
    assert!(f.metrics.samples().contains(&MetricSample::FeedPaused(
        s.source_id(),
        PauseReason::GrantNarrowed,
        true
    )));
    f.pdp.bring_inside_grant(T2);
    let report = committed(step(&f, &s).await);
    assert_eq!(
        report.captured, 2,
        "the same page is read again with every entry"
    );
    assert_eq!(records(&f, T2).await.len(), 1);
}

#[tokio::test]
async fn a_denied_feed_read_pauses_the_source_too() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    f.feed.push([hourly(T1, "k1", "1")]);
    f.pdp.deny(COLLECTOR_USAGE_RECORD.name(), READ_FEED);
    let reads = f.feed.reads();
    assert_eq!(step(&f, &s).await, Step::Paused(PauseReason::GrantNarrowed));
    assert_eq!(f.feed.reads(), reads, "checked before the read");
}

#[tokio::test]
async fn v73_a_new_id_with_a_stored_natural_key_is_rejected_and_the_page_commits() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let original = hourly(T1, "k1", "8");
    let mut impostor = original.clone();
    impostor["id"] = json!(Uuid::from_u128(0xdead));
    f.feed.push([original.clone(), impostor.clone()]);
    let report = committed(step(&f, &s).await);
    assert_eq!(
        report.rejected.get(&RejectReason::NaturalKeyConflict),
        Some(&1)
    );
    let row = stored(&f, &impostor).await.unwrap();
    assert_eq!(row.contribution_state, "rejected");
    assert_eq!(
        row.reject_reason.as_deref(),
        Some("usage_natural_key_conflict")
    );
    assert_eq!(row.gts_type, None, "no normalized columns");
    assert_eq!(
        stored(&f, &original).await.unwrap().contribution_state,
        "captured"
    );
    assert!(checkpoint(&f, &s).await.cursor.is_some());
}

#[tokio::test]
async fn v74_an_optional_field_added_later_is_no_conflict() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let original = hourly(T1, "k1", "8");
    f.feed.push([original.clone()]);
    committed(step(&f, &s).await);
    let mut extended = original;
    extended["added_optional"] = json!({"since": "v2.1"});
    extended["metadata"] = json!({});
    f.feed.push([extended]);
    let report = committed(step(&f, &s).await);
    assert_eq!((report.duplicates, report.content_conflicts), (1, 0));
}

#[tokio::test]
async fn v75_an_unavailable_registry_rejects_nothing_and_keeps_the_cursor() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    f.feed.push([hourly(T1, "k1", "8")]);
    f.registry.set_unavailable(true);
    let error = try_step(&f, &s).await.unwrap_err();
    assert!(matches!(error, DomainError::DependencyUnavailable { .. }));
    assert!(records(&f, T1).await.is_empty());
    assert_eq!(checkpoint(&f, &s).await.cursor, None);
    f.registry.set_unavailable(false);
    assert_eq!(committed(step(&f, &s).await).captured, 1);
    assert_eq!(
        stored(&f, &hourly(T1, "k1", "8"))
            .await
            .unwrap()
            .contribution_state,
        "captured"
    );
}

#[tokio::test]
async fn v76_a_stale_reader_resets_no_newer_cursor_and_opens_no_loss() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    f.feed.push([hourly(T1, "k1", "8")]);
    committed(step(&f, &s).await);
    // A stale leader read before that page and got CursorBeyondRetention for the old cursor.
    let outcome = f
        .intake
        .restart_after_retention_loss(f.db(), &s, None, scope(&f, T1).await)
        .await
        .unwrap();
    assert_eq!(outcome, RestartOutcome::CursorMoved);
    assert!(losses(&f, &s).await.is_empty());
    assert!(checkpoint(&f, &s).await.cursor.is_some());
}

#[tokio::test]
async fn an_unavailable_feed_is_an_error_and_changes_nothing() {
    let f = fixture().await;
    let s = source(&["hours"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    f.feed
        .fail_next(FeedError::Unavailable("down".to_owned(), None));
    assert!(matches!(
        try_step(&f, &s).await,
        Err(DomainError::DependencyUnavailable { .. })
    ));
    assert_eq!(checkpoint(&f, &s).await.cursor, None);
}

#[tokio::test]
async fn a_meter_that_is_not_sum_is_captured_and_handed_on() {
    let f = fixture().await;
    let mut gauge = sum_declaration("GB", &[]);
    gauge.fold = bss_rating::domain::usage::declarations::Fold::Max;
    f.registry.declare(&meter("gb"), gauge);
    let s = source(&["gb"], &[T1]);
    configure(&f, std::slice::from_ref(&s)).await;
    let entry = record_entry(&RecordSpec::hourly(T1, &meter("gb"), "g1", HOUR).quantity(&qty("4")));
    f.feed.push([entry.clone()]);
    assert_eq!(committed(step(&f, &s).await).captured, 1);
    assert_eq!(f.contributions.interpreted(), vec![id_of(&entry)]);
}

mod leadership {
    //! The reader of a source runs only while this instance leads `usage-feed-{uuid}`.

    use bss_rating::domain::leadership::{LeaderWork, LeadershipName, LeaseProvider};
    use bss_rating::infra::cluster::{ClusterLeaseProvider, ElectionTiming, resolve_election};
    use bss_rating::test_providers::FakeClusterLeaders;
    use cluster_sdk::ElectionConfig;
    use tokio_util::sync::CancellationToken;
    use toolkit::client_hub::ClientHub;

    use super::*;

    #[tokio::test]
    async fn the_leader_of_a_source_reads_it_until_its_term_ends() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.feed.push([hourly(T1, "k1", "8")]);
        let leaders = FakeClusterLeaders::new();
        let hub = Arc::new(ClientHub::new());
        let wiring = common::LeadersWiring::bind(&leaders, &hub);
        let provider = ClusterLeaseProvider::new(
            resolve_election(&hub).await.unwrap(),
            ElectionTiming::new(
                ElectionConfig::new(Duration::from_secs(30), 2).unwrap(),
                Duration::from_secs(5),
            ),
        );
        let name = LeadershipName::UsageFeed(s.source_id().uuid());
        let reader = f.reader.clone();
        let source_id = s.source_id();
        let work: LeaderWork = Arc::new(move |term: CancellationToken| {
            let reader = reader.clone();
            Box::pin(async move {
                reader.run(source_id, term).await;
            })
        });
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { provider.run_while_leader(name, shutdown, work).await }
        });
        let scoped = name.scoped();
        assert_eq!(
            scoped,
            format!("bss-rating/usage-feed-{}", s.source_id().uuid())
        );
        leaders.wait_for_candidates(&scoped, 1).await;
        assert!(
            records(&f, T1).await.is_empty(),
            "no read without the leadership"
        );
        leaders.grant(&scoped, 0).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while records(&f, T1).await.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the leader captures the page");
        leaders.revoke(&scoped, 0).await;
        shutdown.cancel();
        task.await.unwrap().unwrap();
        wiring.stop().await;
    }
}

mod queue_depth {
    //! The depth backpressure reads, from the outbox tables of `rating.child_work`.

    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use bss_rating::config::RatingConfig;
    use bss_rating::domain::usage::reader::QueueDepth;
    use bss_rating::infra::queue_depth::OutboxQueueDepth;
    use bss_rating::infra::queues::{self, CHILD_WORK_QUEUE, QueueWorker, UnwiredQueue};
    use bss_rating::infra::storage::entity::outbox::incoming;
    use sea_orm::EntityTrait;
    use toolkit_db::Db;
    use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, Outbox, OutboxMessage, Record};
    use toolkit_db::secure::{AccessScope, SecureEntityExt};

    use super::*;

    /// A `rating.child_work` worker that processes every message it is handed.
    #[derive(Default)]
    struct Processing(AtomicUsize);

    #[async_trait]
    impl LeasedMessageHandler for Processing {
        async fn handle(&self, _msg: &OutboxMessage) -> MessageResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            MessageResult::Ok
        }
    }

    async fn enqueue(outbox: &Outbox, db: &Db, count: u32) {
        for partition in 0..count {
            let record = Record::to(CHILD_WORK_QUEUE, partition)
                .payload(b"child".to_vec(), "rating.child_work")
                .build()
                .unwrap();
            let conn = db.conn().unwrap();
            outbox.enqueue(&conn, record).await.unwrap().fire();
        }
    }

    async fn unsequenced(db: &Db) -> u64 {
        let conn = db.conn().unwrap();
        incoming::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .count(&conn)
            .await
            .unwrap()
    }

    /// Waits until `holds` is true, or fails the test after five seconds.
    async fn eventually<F, Fut>(what: &str, mut holds: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !holds().await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what} within five seconds"));
    }

    fn unwired(queue: &str) -> QueueWorker {
        Arc::new(UnwiredQueue::new(queue))
    }

    #[tokio::test]
    async fn messages_waiting_on_child_work_count_whether_sequenced_or_not() {
        let test_db = common::migrated_db().await;
        let db = test_db.db.clone();
        let layout = RatingConfig::default().queue_layout().unwrap();
        let depth = OutboxQueueDepth::new(db.clone());
        // With no sequencer running, every message stays unsequenced (`incoming`).
        let stopped = queues::start(db.clone(), &layout, unwired).await.unwrap();
        let writer = Arc::clone(stopped.outbox());
        stopped.stop().await;
        assert_eq!(depth.child_work_depth().await.unwrap(), 0);
        enqueue(&writer, &db, 3).await;
        assert_eq!(unsequenced(&db).await, 3);
        assert_eq!(depth.child_work_depth().await.unwrap(), 3);
        // Once the sequencer numbers them, they count by their partitions' sequences.
        let running = queues::start(db.clone(), &layout, unwired).await.unwrap();
        eventually("the sequencer numbers the messages", || async {
            unsequenced(&db).await == 0
        })
        .await;
        assert_eq!(depth.child_work_depth().await.unwrap(), 3);
        running.stop().await;
    }

    #[tokio::test]
    async fn a_processed_message_leaves_the_depth() {
        let test_db = common::migrated_db().await;
        let db = test_db.db.clone();
        let layout = RatingConfig::default().queue_layout().unwrap();
        let processing = Arc::new(Processing::default());
        let worker = Arc::clone(&processing);
        let outbox = queues::start(db.clone(), &layout, move |queue| {
            if queue == CHILD_WORK_QUEUE {
                Arc::clone(&worker) as QueueWorker
            } else {
                unwired(queue)
            }
        })
        .await
        .unwrap();
        let depth = OutboxQueueDepth::new(db.clone());
        enqueue(outbox.outbox(), &db, 3).await;
        eventually("the worker processes the messages", || async {
            processing.0.load(Ordering::SeqCst) == 3
        })
        .await;
        eventually("the processed messages leave the depth", || async {
            depth.child_work_depth().await.unwrap() == 0
        })
        .await;
        outbox.stop().await;
    }
}

/// A capture reject of `source`, as `(reason, loss_id)`.
async fn rejects_of(
    f: &common::usage::Fixture,
    s: &bss_rating::domain::usage::source::SourceDefinition,
) -> Vec<(String, Option<Uuid>)> {
    let conn = f.db().conn().unwrap();
    usage_repo::list_capture_rejects(&conn, s.source_id())
        .await
        .unwrap()
        .into_iter()
        .map(|reject| (reject.reason, reject.loss_id))
        .collect()
}

mod retry {
    //! A rejected entry retried from its stored row (feature 04 §4.1 `inst-st-retry`).

    use bss_rating::domain::usage::declarations::PageDeclarations;
    use bss_rating::domain::usage::intake::UsageIntake;
    use common::usage::ProbedRepo;

    use super::*;

    /// An hourly entry of `key` carrying the metadata key `region`, which the fixture's
    /// declarations do not declare.
    fn with_region(key: &str) -> Value {
        let mut spec = RecordSpec::hourly(T1, &meter("hours"), key, HOUR).quantity(&qty("8"));
        spec.metadata.insert("region".to_owned(), "eu".to_owned());
        record_entry(&spec)
    }

    fn declaring_region() -> PageDeclarations {
        let mut fresh = BTreeMap::new();
        fresh.insert(meter("hours"), Some(sum_declaration("h", &["region"])));
        fresh
    }

    #[tokio::test]
    async fn a_retry_rejected_again_for_another_reason_replaces_the_exception() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let entry = with_region("k1");
        // Another id holding the entry's natural key, as a nonconforming collector could send.
        let mut holder = hourly(T1, "k1", "8");
        holder["id"] = json!(Uuid::from_u128(0xbeef));
        f.feed.push([entry.clone(), holder]);
        let report = committed(step(&f, &s).await);
        assert_eq!(report.rejected.get(&RejectReason::UsageMalformed), Some(&1));
        assert_eq!(report.captured, 2);

        let outcome = f
            .intake
            .retry_rejected(
                f.db(),
                scope(&f, T1).await,
                identity(&entry),
                declaring_region(),
            )
            .await
            .unwrap();
        let RetryOutcome::RejectedAgain(rejection) = outcome else {
            panic!("rejected again, got {outcome:?}");
        };
        assert_eq!(rejection.reason, RejectReason::NaturalKeyConflict);
        let row = stored(&f, &entry).await.unwrap();
        assert_eq!(row.contribution_state, "rejected");
        assert_eq!(
            row.reject_reason.as_deref(),
            Some("usage_natural_key_conflict"),
            "the row stands rejected for the new reason"
        );
        assert_eq!(
            open_exceptions(&f, T1).await,
            vec![(
                id_of(&entry).to_string(),
                "usage_natural_key_conflict".to_owned()
            )],
            "the malformed exception is closed, the conflict is open"
        );
        assert!(
            f.contributions.interpreted().len() == 1,
            "only the holder was handed on"
        );
    }

    #[tokio::test]
    async fn a_retry_rejected_again_for_the_same_reason_keeps_one_exception() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let entry = with_region("k1");
        f.feed.push([entry.clone()]);
        committed(step(&f, &s).await);
        let stale = f
            .declarations
            .resolve_page([&meter("hours")])
            .await
            .unwrap();
        let outcome = f
            .intake
            .retry_rejected(f.db(), scope(&f, T1).await, identity(&entry), stale)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            RetryOutcome::RejectedAgain(ref rejection) if rejection.reason == RejectReason::UsageMalformed
        ));
        assert_eq!(
            open_exceptions(&f, T1).await,
            vec![(id_of(&entry).to_string(), "usage_malformed".to_owned())]
        );
        assert_eq!(
            stored(&f, &entry).await.unwrap().reject_reason.as_deref(),
            Some("usage_malformed")
        );
    }

    #[tokio::test]
    async fn a_retried_invalidation_is_applied_and_withdraws_its_target() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let target = hourly(T1, "k1", "8");
        // The invalidation carries a metadata key the declaration lacks, so it is rejected and
        // its target stays counted until the retry.
        let mut invalidation = invalidation_of(&target, "DUPLICATE_EMISSION");
        invalidation["metadata"] = json!({"region": "eu"});
        f.feed.push([target.clone(), invalidation.clone()]);
        let report = committed(step(&f, &s).await);
        assert_eq!(report.rejected.get(&RejectReason::UsageMalformed), Some(&1));
        assert_eq!(
            stored(&f, &target).await.unwrap().contribution_state,
            "captured"
        );

        let outcome = f
            .intake
            .retry_rejected(
                f.db(),
                scope(&f, T1).await,
                identity(&invalidation),
                declaring_region(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, RetryOutcome::Interpreted);
        let applied = stored(&f, &invalidation).await.unwrap();
        assert_eq!(applied.contribution_state, "applied");
        assert_eq!(applied.entry_type.as_deref(), Some("invalidation"));
        let withdrawn = stored(&f, &target).await.unwrap();
        assert_eq!(withdrawn.contribution_state, "withdrawn");
        assert_eq!(withdrawn.withdrawn_by, Some(id_of(&invalidation)));
        assert!(open_exceptions(&f, T1).await.is_empty());
        assert_eq!(
            f.contributions.interpreted(),
            vec![id_of(&target)],
            "an invalidation is applied, not handed to attribution"
        );
    }

    #[tokio::test]
    async fn a_withdrawal_a_concurrent_writer_won_aborts_the_page() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let target = hourly(T1, "k1", "8");
        f.feed.push([target.clone()]);
        committed(step(&f, &s).await);
        let probe = Arc::new(ProbedRepo::default());
        probe.lose_withdrawals.store(true, Ordering::SeqCst);
        let mut parts = f.intake_parts();
        parts.records = probe;
        let mut reader_parts = f.reader_parts();
        reader_parts.intake = UsageIntake::new(parts);
        let reader = bss_rating::domain::usage::reader::UsageFeedReader::new(reader_parts);
        let cursor = checkpoint(&f, &s).await.cursor;
        let invalidation = invalidation_of(&target, "DUPLICATE_EMISSION");
        f.feed.push([invalidation.clone()]);
        let error = reader.session(s.source_id()).step().await.unwrap_err();
        assert!(matches!(error, DomainError::Concurrent(_)), "{error:?}");
        assert_eq!(checkpoint(&f, &s).await.cursor, cursor, "nothing committed");
        assert!(stored(&f, &invalidation).await.is_none());
        assert_eq!(
            stored(&f, &target).await.unwrap().contribution_state,
            "captured"
        );
    }
}

mod target_unknown {
    //! The 15-minute safety net of `invalidation_target_unknown` (feature 04 §4.5).

    use bss_rating::domain::exception::{
        ExceptionKey, ExceptionReason, ExceptionRecord, ExceptionRetrier, SubjectKind, SubjectRef,
    };
    use bss_rating::domain::usage::entry::{self, CURRENT_DIGEST_VERSION};
    use bss_rating::domain::usage::intake::{TARGET_UNKNOWN, TargetUnknownRetrier};
    use bss_rating::domain::usage::normalize::normalize;
    use bss_rating::domain::usage::{CaptureOutcome, NewUsageRecord, UsageRecordRepo};
    use bss_rating::infra::storage::repo::usage_repo::OrmUsageRepo;

    use super::*;

    fn exception_on(subject: SubjectRef) -> ExceptionRecord {
        ExceptionRecord {
            exception_id: Uuid::now_v7(),
            key: ExceptionKey::new(
                T1,
                SubjectKind::UsageRecord,
                subject,
                ExceptionReason::parse(TARGET_UNKNOWN).unwrap(),
            ),
            detail: String::new(),
            first_seen_at: time::OffsetDateTime::UNIX_EPOCH,
            last_attempt_at: time::OffsetDateTime::UNIX_EPOCH,
            attempts: 1,
            resolution: None,
        }
    }

    #[tokio::test]
    async fn an_exception_whose_subject_is_no_record_id_is_a_corrupt_row() {
        let f = fixture().await;
        let retrier = TargetUnknownRetrier::new(f.intake.clone());
        let exception = exception_on(SubjectRef::parse("not-a-record").unwrap());
        let error = retrier
            .retry(f.db(), &scope(&f, T1).await, &exception)
            .await
            .unwrap_err();
        assert!(matches!(error, DomainError::CorruptRow(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_target_with_no_invalidation_naming_it_is_left_open() {
        let f = fixture().await;
        let retrier = TargetUnknownRetrier::new(f.intake.clone());
        let target = hourly(T1, "k1", "8");
        let exception = exception_on(SubjectRef::from(id_of(&target)));
        retrier
            .retry(f.db(), &scope(&f, T1).await, &exception)
            .await
            .unwrap();
        assert!(stored(&f, &target).await.is_none());
    }

    #[tokio::test]
    async fn a_target_stored_after_its_invalidation_is_withdrawn_and_the_exception_resolved() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let target = hourly(T1, "k1", "8");
        let invalidation = invalidation_of(&target, "DUPLICATE_EMISSION");
        f.feed.push([invalidation.clone()]);
        assert_eq!(committed(step(&f, &s).await).targets_unknown, 1);
        assert_eq!(
            open_exceptions(&f, T1).await,
            vec![(id_of(&target).to_string(), TARGET_UNKNOWN.to_owned())]
        );
        // The target arrives by a path that does not withdraw it on capture.
        let declarations = f
            .declarations
            .resolve_page([&meter("hours")])
            .await
            .unwrap();
        let normalized = normalize(&target, identity(&target), &declarations).unwrap();
        let record = NewUsageRecord {
            identity: identity(&target),
            source_id: s.source_id(),
            raw_entry: target.clone(),
            raw_sha256: entry::digest(&target, CURRENT_DIGEST_VERSION),
            raw_digest_version: CURRENT_DIGEST_VERSION,
            captured_at: f.clock.now(),
            outcome: CaptureOutcome::Interpreted(normalized),
        };
        let tenant_scope = scope(&f, T1).await;
        let stored_scope = tenant_scope.clone();
        f.db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { OrmUsageRepo.insert(tx, &stored_scope, &record).await })
            })
            .await
            .unwrap();
        let retrier = TargetUnknownRetrier::new(f.intake.clone());
        let exception = exception_on(SubjectRef::from(id_of(&target)));
        retrier
            .retry(f.db(), &tenant_scope, &exception)
            .await
            .unwrap();
        let withdrawn = stored(&f, &target).await.unwrap();
        assert_eq!(withdrawn.contribution_state, "withdrawn");
        assert_eq!(withdrawn.withdrawn_by, Some(id_of(&invalidation)));
        assert!(open_exceptions(&f, T1).await.is_empty());
    }
}

mod capture {
    //! What a page captures, sets aside and compares.

    use bss_rating::domain::usage::entry::MAX_RAW_ENTRY_BYTES;
    use bss_rating::domain::usage::intake::UsageIntake;
    use bss_rating::domain::usage::reader::UsageFeedReader;
    use common::usage::ProbedRepo;

    use super::*;

    #[tokio::test]
    async fn a_replayed_unidentifiable_entry_is_neither_counted_nor_lost_again() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let broken = json!({"id": "not-a-uuid", "gts_type_id": meter("hours").as_str()});
        f.feed.push([broken.clone()]);
        assert_eq!(committed(step(&f, &s).await).unidentifiable, 1);
        // The feed delivers at least once: the same entry again.
        f.feed.push([broken]);
        let replay = committed(step(&f, &s).await);
        assert_eq!((replay.unidentifiable, replay.losses_opened.len()), (0, 0));
        assert_eq!(rejects_of(&f, &s).await.len(), 1);
        assert_eq!(losses(&f, &s).await.len(), 1);
    }

    #[tokio::test]
    async fn a_reread_page_is_compared_without_being_interpreted_again() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let probe = Arc::new(ProbedRepo::default());
        let mut parts = f.intake_parts();
        parts.records = probe.clone();
        let mut reader_parts = f.reader_parts();
        reader_parts.intake = UsageIntake::new(parts);
        let mut session = UsageFeedReader::new(reader_parts).session(s.source_id());
        let entries = [
            hourly(T1, "k1", "8"),
            hourly(T1, "k2", "3"),
            hourly(T1, "k3", "1"),
        ];
        f.feed.push(entries.clone());
        committed(session.step().await.unwrap());
        let lookups = probe.key_lookups.load(Ordering::SeqCst);
        assert!(lookups > 0, "a new entry is interpreted");
        f.feed.push(entries);
        let again = match session.step().await.unwrap() {
            Step::Committed { report, .. } => report,
            other => panic!("expected a committed page, got {other:?}"),
        };
        assert_eq!(again.duplicates, 3);
        assert_eq!(
            probe.key_lookups.load(Ordering::SeqCst),
            lookups,
            "a stored entry is compared by its digest alone"
        );
    }

    #[tokio::test]
    async fn an_entry_larger_than_a_row_holds_is_rejected_with_a_stub() {
        let f = fixture().await;
        f.registry
            .declare(&meter("hours"), sum_declaration("h", &["note"]));
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let mut spec = RecordSpec::hourly(T1, &meter("hours"), "k1", HOUR);
        spec.metadata
            .insert("note".to_owned(), "x".repeat(MAX_RAW_ENTRY_BYTES));
        let big = record_entry(&spec);
        f.feed.push([big.clone()]);
        let report = committed(step(&f, &s).await);
        assert_eq!(report.rejected.get(&RejectReason::UsageMalformed), Some(&1));
        let row = stored(&f, &big).await.unwrap();
        assert_eq!(row.contribution_state, "rejected");
        assert_eq!(
            row.raw_entry["oversize"]["bytes"],
            json!(big.to_string().len())
        );
        assert!(row.raw_entry.to_string().len() < 2048);
        assert!(f.contributions.interpreted().is_empty());
        // Its digest covers the whole entry, so a re-read is a duplicate.
        f.feed.push([big]);
        assert_eq!(committed(step(&f, &s).await).duplicates, 1);
    }

    #[tokio::test]
    async fn an_entry_of_a_meter_the_source_does_not_read_is_set_aside() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.feed.set_unfiltered(true);
        let gb = record_entry(&RecordSpec::hourly(T1, &meter("gb"), "g1", HOUR));
        let hours = hourly(T1, "k1", "8");
        f.feed.push([gb.clone(), hours.clone()]);
        let report = committed(step(&f, &s).await);
        assert_eq!((report.captured, report.outside_subscription), (1, 1));
        assert!(
            stored(&f, &gb).await.is_none(),
            "written to no tenant's records"
        );
        assert!(stored(&f, &hours).await.is_some());
        assert_eq!(
            rejects_of(&f, &s).await,
            vec![("outside_subscription".to_owned(), None)]
        );
        assert!(
            losses(&f, &s).await.is_empty(),
            "nothing of the source is lost"
        );
        // A replay sets nothing aside again.
        f.feed.push([gb]);
        assert_eq!(committed(step(&f, &s).await).outside_subscription, 0);
        assert_eq!(rejects_of(&f, &s).await.len(), 1);
    }

    #[tokio::test]
    async fn an_entry_of_a_tenant_outside_the_configured_scope_is_set_aside() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let child = Uuid::from_u128(0x11);
        f.hierarchy.place(child, T1);
        let (inside, outside) = (hourly(child, "k1", "8"), hourly(T2, "k2", "3"));
        f.feed.push([inside.clone(), outside.clone()]);
        let report = committed(step(&f, &s).await);
        assert_eq!((report.captured, report.outside_subscription), (1, 1));
        assert!(
            stored(&f, &inside).await.is_some(),
            "a descendant of a root is read"
        );
        assert!(records(&f, T2).await.is_empty());
        assert!(f.metrics.samples().iter().any(|sample| matches!(
            sample,
            MetricSample::FeedPage(source, report) if *source == s.source_id() && report.outside_subscription == 1
        )));
    }

    #[tokio::test]
    async fn a_tenant_hierarchy_that_cannot_answer_commits_nothing() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.feed.push([hourly(Uuid::from_u128(0x11), "k1", "8")]);
        f.hierarchy.set_unavailable(true);
        let error = try_step(&f, &s).await.unwrap_err();
        assert!(
            matches!(error, DomainError::DependencyUnavailable { .. }),
            "{error:?}"
        );
        assert_eq!(checkpoint(&f, &s).await.cursor, None);
    }
}

mod reading {
    //! The reader's checks around a page: its size, its time, the grant and the source's state.

    use bss_rating::domain::usage::reader::ReaderSettings;
    use bss_rating::domain::usage::reader::{RunEnd, UsageFeedReader};
    use common::usage::{ProbedRepo, configure_at};
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn a_page_larger_than_the_limit_is_refused_and_nothing_commits() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.feed.set_overrun(2);
        f.feed
            .push((1..=5).map(|n| hourly(T1, &format!("k{n}"), "1")));
        let error = try_step(&f, &s).await.unwrap_err();
        assert!(
            matches!(error, DomainError::DependencyUnavailable { ref detail, .. } if detail.contains("5 entries")),
            "{error:?}"
        );
        assert!(records(&f, T1).await.is_empty());
        assert_eq!(checkpoint(&f, &s).await.cursor, None);
    }

    #[tokio::test]
    async fn a_feed_that_never_answers_is_given_up_after_the_call_timeout() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let mut parts = f.reader_parts();
        parts.settings = ReaderSettings::new(
            3,
            Duration::from_millis(10),
            100,
            Duration::from_millis(50),
            Duration::from_millis(50),
        )
        .unwrap();
        let reader = UsageFeedReader::new(parts);
        f.feed.set_stalled(true);
        let error =
            tokio::time::timeout(Duration::from_secs(5), reader.session(s.source_id()).step())
                .await
                .expect("the step returns within its call timeout")
                .unwrap_err();
        assert!(
            matches!(error, DomainError::DependencyUnavailable { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn the_grant_is_checked_again_after_the_read_and_before_the_commit() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.feed.push([hourly(T1, "k1", "8")]);
        // The check before the read passes; the one before the commit is refused.
        f.feed_grant.refuse_from_call(2);
        assert_eq!(step(&f, &s).await, Step::Paused(PauseReason::GrantNarrowed));
        assert_eq!(f.feed.reads(), 1, "the page was read");
        assert!(records(&f, T1).await.is_empty(), "and not committed");
        assert_eq!(checkpoint(&f, &s).await.cursor, None);
        f.feed_grant.follow_the_pdp();
        assert_eq!(committed(step(&f, &s).await).captured, 1);
        assert!(f.metrics.samples().contains(&MetricSample::FeedPaused(
            s.source_id(),
            PauseReason::GrantNarrowed,
            false
        )));
    }

    #[tokio::test]
    async fn a_tenant_of_the_scope_outside_the_grant_pauses_the_source() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        let child = Uuid::from_u128(0x11);
        f.hierarchy.place(child, T1);
        f.feed.push([hourly(child, "k1", "8")]);
        // The root stays covered; a tenant under it is carved out of the grant.
        f.pdp.put_outside_grant(child);
        assert_eq!(step(&f, &s).await, Step::Paused(PauseReason::GrantNarrowed));
        assert_eq!(checkpoint(&f, &s).await.cursor, None, "nothing skipped");
        f.pdp.bring_inside_grant(child);
        assert_eq!(committed(step(&f, &s).await).captured, 1);
    }

    #[tokio::test]
    async fn the_end_of_a_term_clears_the_pauses_it_set() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.depth.set(1_000);
        let mut session = f.reader.session(s.source_id());
        assert_eq!(
            session.step().await.unwrap(),
            Step::Paused(PauseReason::Backpressure)
        );
        session.end();
        let paused: Vec<bool> = f
            .metrics
            .samples()
            .into_iter()
            .filter_map(|sample| match sample {
                MetricSample::FeedPaused(_, PauseReason::Backpressure, paused) => Some(paused),
                _ => None,
            })
            .collect();
        assert_eq!(paused, [true, false]);
    }

    #[tokio::test]
    async fn a_failing_feed_is_retried_with_growing_pauses_not_a_hot_loop() {
        let f = fixture().await;
        let s = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&s)).await;
        f.feed.set_unavailable(true);
        let term = CancellationToken::new();
        let running = tokio::spawn({
            let (reader, term) = (f.reader.clone(), term.clone());
            async move { reader.run(s.source_id(), term).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let failures = f
            .metrics
            .samples()
            .iter()
            .filter(|sample| matches!(sample, MetricSample::DependencyError(..)))
            .count();
        // The fixture caps the backoff at 50 ms; jittered, each pause is 25-50 ms.
        assert!(
            (5..=25).contains(&failures),
            "{failures} failures in half a second"
        );
        f.feed.set_unavailable(false);
        f.feed.push([hourly(T1, "k1", "8")]);
        tokio::time::timeout(Duration::from_secs(5), async {
            while records(&f, T1).await.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the reader recovers");
        term.cancel();
        assert_eq!(running.await.unwrap(), RunEnd::TermEnded);
    }

    #[tokio::test]
    async fn a_drained_source_ends_its_run_so_its_leadership_is_given_up() {
        let f = fixture().await;
        let old = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&old)).await;
        configure(&f, &[source(&["gb"], &[T1])]).await;
        assert_eq!(checkpoint(&f, &old).await.state, SourceState::Draining);
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            f.reader.run(old.source_id(), CancellationToken::new()),
        )
        .await
        .expect("a retired source does not wait out its term");
        assert_eq!(ended, RunEnd::Retired);
        assert_eq!(checkpoint(&f, &old).await.state, SourceState::Retired);
        assert!(!f.reader.is_readable(old.source_id()).await.unwrap());
        // A reconciliation that names it again makes it readable.
        configure(&f, std::slice::from_ref(&old)).await;
        assert!(f.reader.is_readable(old.source_id()).await.unwrap());
    }

    #[tokio::test]
    async fn a_drained_source_made_active_meanwhile_is_read_on() {
        let f = fixture().await;
        let old = source(&["hours"], &[T1]);
        configure(&f, std::slice::from_ref(&old)).await;
        configure(&f, &[source(&["gb"], &[T1])]).await;
        let probe = Arc::new(ProbedRepo::default());
        probe.lose_retirements.store(true, Ordering::SeqCst);
        let mut parts = f.reader_parts();
        parts.checkpoints = probe;
        let step = UsageFeedReader::new(parts)
            .session(old.source_id())
            .step()
            .await
            .unwrap();
        assert!(
            matches!(
                step,
                Step::Committed {
                    caught_up: true,
                    ..
                }
            ),
            "not drained: {step:?}"
        );
    }

    #[tokio::test]
    async fn an_older_configuration_does_not_revert_a_newer_one() {
        let f = fixture().await;
        let (s1, s2) = (source(&["hours"], &[T1]), source(&["gb"], &[T1]));
        configure_at(&f, std::slice::from_ref(&s1), 1).await;
        configure_at(&f, std::slice::from_ref(&s2), 2).await;
        // An instance still on generation 1 restarts mid-rollout.
        let read = configure_at(&f, std::slice::from_ref(&s1), 1).await;
        assert_eq!(checkpoint(&f, &s1).await.state, SourceState::Draining);
        assert_eq!(checkpoint(&f, &s2).await.state, SourceState::Active);
        let mut ids: Vec<_> = read.iter().map(|c| c.source_id).collect();
        ids.sort();
        let mut expected = vec![s1.source_id(), s2.source_id()];
        expected.sort();
        assert_eq!(ids, expected, "it reads the stored sources");
        // The same generation again changes nothing; a newer one applies.
        configure_at(&f, std::slice::from_ref(&s2), 2).await;
        assert_eq!(checkpoint(&f, &s2).await.state, SourceState::Active);
        configure_at(&f, std::slice::from_ref(&s1), 3).await;
        assert_eq!(checkpoint(&f, &s1).await.state, SourceState::Active);
        assert_eq!(checkpoint(&f, &s2).await.state, SourceState::Draining);
        assert_eq!(checkpoint(&f, &s1).await.config_generation, 3);
    }
}
