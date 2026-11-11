#![cfg(feature = "integration")]
// The rig, the holds and the polls are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The per-instance account of consumer activation (ADR-004), on a real
//! `PostgreSQL`: the Level 2 rows of DESIGN-activation §7 that need real rows,
//! real transactions and, for the concurrent ones, separate committed
//! connections with `pg_stat_database.deadlocks` asserted unchanged.
//!
//! The replica is not served: the test drives the delivery loop's pass itself
//! through a notifier over the replica's database (`tests/support/instances.rs`),
//! so `K` is the number of ticks that publish and a call of silence or a
//! forgetting happens at the tick the test runs. An interval passes by making
//! the stamped states due again, and a day by ageing a subscription's
//! `silent_since`: nothing here waits for a timer. The races are ordered with
//! the holds of `infra::storage::activation_hold` — a statement of the
//! repository parked until the test releases it — and, for a write, the
//! coordinator's own pause point; every outcome is asserted on the tables.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_activation_instances_test
//! ```

// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-instance-tests:p1

mod support;

use std::sync::Arc;
use std::time::Duration;

use cf_gears_settings_service::domain::activation::{
    ActivationRecorder, BackResponse, NotifyMetrics, ReportResult, Tick,
};
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::infra::storage::activation_hold::{Step, hold};
use cf_gears_settings_service::infra::storage::activation_repo::ActivationRepo;
use cf_gears_settings_service::infra::storage::audit_store::AuditStore;
use sea_orm::ConnectionTrait as _;
use serde_json::json;
use support::instances::{
    Ident, K, LockBench, Rig, WritePause, finished, gear_context, ident, instance, reached,
    still_waiting, write_task,
};
use support::platform::{CHILD, ROOT, Replica};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Setups
// ---------------------------------------------------------------------------

/// `A` and `B` registered, a change `X` written: `A` confirms it, `B` stays
/// silent until the timeout calls it so. Returns `(A, B, X)`.
async fn silent_pair(r: &Rig) -> (Uuid, Uuid, String) {
    let (a, b) = (instance(1), instance(2));
    r.register(a, ROOT).await;
    r.register(b, ROOT).await;
    r.write(ROOT, "one").await;
    let x = r.change(ROOT).await;
    r.deliveries(1).await;
    assert_eq!(r.confirm(a, ROOT).await, ReportResult::Applied);
    r.deliveries(1).await;
    r.tick_due().await;
    assert_eq!(r.status(b, ROOT).await, "silent");
    assert!(r.silent(b, ROOT).await);
    (a, b, x)
}

/// `A` and `B` registered, a change `X` written that neither answers: both are
/// called silent. Returns `(A, B, X)`.
async fn both_silent(r: &Rig) -> (Uuid, Uuid, String) {
    let (a, b) = (instance(1), instance(2));
    r.register(a, ROOT).await;
    r.register(b, ROOT).await;
    r.write(ROOT, "one").await;
    let x = r.change(ROOT).await;
    r.run_to_silence().await;
    for who in [a, b] {
        assert_eq!(r.status(who, ROOT).await, "silent");
        assert!(r.silent(who, ROOT).await);
    }
    (a, b, x)
}

/// One instance whose row of a change sits at `K` accepted deliveries, due:
/// the next tick calls it silent. Returns the instance.
async fn due_for_silence(r: &Rig) -> Uuid {
    let a = instance(1);
    r.register(a, ROOT).await;
    r.write(ROOT, "one").await;
    r.deliveries(K).await;
    let row = r.row(a, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", K));
    r.elapse().await;
    a
}

// ---------------------------------------------------------------------------
// Two instances, one silent
// ---------------------------------------------------------------------------

/// *Two instances both confirm*: a row each, both `succeeded`, one `activation`
/// record per instance under the change.
#[tokio::test]
async fn two_instances_both_confirm() {
    let Some(r) = Rig::new("both-confirm").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b) = (instance(1), instance(2));
    r.register(a, ROOT).await;
    r.register(b, ROOT).await;
    r.write(ROOT, "one").await;
    let change = r.change(ROOT).await;

    assert_eq!(r.holders(ROOT).await, [a, b], "a row per expected instance");
    for who in [a, b] {
        let row = r.row(who, ROOT).await.unwrap();
        assert_eq!(row.status, "awaiting");
        assert_eq!(row.change_ref, change);
    }
    assert_eq!(r.confirm(a, ROOT).await, ReportResult::Applied);
    assert_eq!(r.confirm(b, ROOT).await, ReportResult::Applied);

    for who in [a, b] {
        assert_eq!(r.status(who, ROOT).await, "succeeded");
        assert_eq!(
            r.records(who).await,
            [("success".to_owned(), change.clone(), ROOT.to_string())],
            "one record per instance under the change"
        );
    }
    assert_eq!(r.all_records().await, 2);
    assert!(matches!(
        r.tick_due().await,
        Tick::Complete { published: 0 }
    ));
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *One instance silent → `incomplete`* and *Report timeout bounds the wait*:
/// after exactly `K` accepted deliveries the silent row is `silent` — not
/// before, and never published again — with one `silent` record under the
/// change, the subscription marked, and the next change expecting the
/// confirming instance only.
#[tokio::test]
async fn one_silent_instance_is_called_after_exactly_k_deliveries_and_the_next_change_leaves_it_out()
 {
    let Some(r) = Rig::new("one-silent").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b) = (instance(1), instance(2));
    r.register(a, ROOT).await;
    r.register(b, ROOT).await;
    r.write(ROOT, "one").await;
    let x = r.change(ROOT).await;

    // The bound of the wait: `K` publications, each counted, no row called
    // silent before the `K`th has been accepted.
    r.deliveries(1).await;
    assert_eq!(r.confirm(a, ROOT).await, ReportResult::Applied);
    for delivered in 2..=K {
        assert_eq!(r.row(b, ROOT).await.unwrap().deliveries, delivered - 1);
        assert_eq!(r.status(b, ROOT).await, "awaiting");
        r.deliveries(1).await;
    }
    let row = r.row(b, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", K));
    assert!(!r.silent(b, ROOT).await);
    let published = r.notified();

    assert!(matches!(
        r.tick_due().await,
        Tick::Complete { published: 0 }
    ));
    let row = r.row(b, ROOT).await.unwrap();
    assert_eq!(row.status, "silent");
    assert_eq!(row.deliveries, K, "the call counts no delivery");
    assert!(row.silent_since);
    assert!(r.silent(b, ROOT).await, "the subscription is marked silent");
    assert!(!r.silent(a, ROOT).await);
    assert_eq!(r.status(a, ROOT).await, "succeeded");
    assert_eq!(
        r.records(b).await,
        [("silent".to_owned(), x.clone(), ROOT.to_string())],
        "one `silent` record under the change"
    );
    assert_eq!(r.notified(), published, "a silent row is not published");
    r.tick_due().await;
    assert_eq!(r.notified(), published, "nor at a later tick");

    // The next change writes a row for the confirming instance only.
    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a]);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Silence recorded under its change*, the record half: `B`'s `silent` record
/// under `X` stays after a change `Y` supersedes `X` and after `B` is forgotten,
/// and `Y`'s account writes none for `B`.
#[tokio::test]
async fn a_silent_record_stays_under_its_change_after_the_next_change_and_the_forgetting() {
    let Some(r) = Rig::new("silence-record").await else {
        return;
    };
    let (a, b, x) = silent_pair(&r).await;
    let silent = [("silent".to_owned(), x, ROOT.to_string())];
    assert_eq!(r.records(b).await, silent);

    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a], "Y's account has none for B");
    assert_eq!(r.records(b).await, silent, "X's record outlives X");

    r.silent_for(b, ROOT, 25).await;
    r.tick_due().await;
    assert!(r.sub(b, ROOT).await.is_none(), "B is forgotten");
    assert_eq!(r.records(b).await, silent, "and so is its record kept");
    assert_eq!(r.holders(ROOT).await, [a]);
}

/// *Failure wins, delivery goes on*: a `failed` report settles its row at once,
/// the other instance's row is still published and, after `K` deliveries,
/// called silent; its later confirmation settles its own row and leaves the
/// failed one failed.
#[tokio::test]
async fn a_failure_settles_at_once_and_delivery_goes_on_for_the_other_instance() {
    let Some(r) = Rig::new("failure-wins").await else {
        return;
    };
    let (a, b) = (instance(1), instance(2));
    r.register(a, ROOT).await;
    r.register(b, ROOT).await;
    r.write(ROOT, "one").await;
    let x = r.change(ROOT).await;
    r.deliveries(1).await;

    let reported = r.report(a, ROOT, &x, "failed", None).await.unwrap();
    assert_eq!(reported, ReportResult::Applied);
    assert_eq!(r.status(a, ROOT).await, "failed", "failed at once");
    let before = r.notified();
    r.deliveries(1).await;
    assert_eq!(r.notified(), before + 1, "B's row is still published");
    r.tick_due().await;
    assert_eq!(
        r.status(b, ROOT).await,
        "silent",
        "and, after K, called silent"
    );

    assert_eq!(r.confirm(b, ROOT).await, ReportResult::Applied);
    assert_eq!(r.status(b, ROOT).await, "succeeded");
    assert_eq!(r.status(a, ROOT).await, "failed", "the failure stands");
    assert_eq!(r.outcomes(a).await, ["failure"]);
    assert_eq!(r.outcomes(b).await, ["silent", "success"]);
}

/// *Late report completes*: a `silent` row moves to `succeeded` on the
/// expected value, or to `failed` on a failure, each with one record, and the
/// subscription's `silent_since` is cleared.
#[tokio::test]
async fn a_late_report_settles_a_silent_row_and_ends_the_silence() {
    let Some(r) = Rig::new("late-report").await else {
        return;
    };
    let (confirmer, quiet, failing) = (instance(1), instance(2), instance(3));
    for who in [confirmer, quiet, failing] {
        r.register(who, ROOT).await;
    }
    r.write(ROOT, "one").await;
    let change = r.change(ROOT).await;
    r.deliveries(1).await;
    assert_eq!(r.confirm(confirmer, ROOT).await, ReportResult::Applied);
    r.deliveries(1).await;
    r.tick_due().await;
    for who in [quiet, failing] {
        assert_eq!(r.status(who, ROOT).await, "silent");
        assert!(r.silent(who, ROOT).await);
    }

    assert_eq!(r.confirm(quiet, ROOT).await, ReportResult::Applied);
    assert_eq!(r.status(quiet, ROOT).await, "succeeded");
    assert!(!r.silent(quiet, ROOT).await, "the silence ended");
    assert_eq!(r.outcomes(quiet).await, ["silent", "success"]);

    let failed = r
        .report(failing, ROOT, &change, "failed", None)
        .await
        .unwrap();
    assert_eq!(failed, ReportResult::Applied);
    assert_eq!(
        r.status(failing, ROOT).await,
        "failed",
        "a late failure is failed"
    );
    assert!(!r.silent(failing, ROOT).await);
    assert_eq!(r.outcomes(failing).await, ["silent", "failure"]);
}

/// *Report Timeout, Silence and Forgetting*: a delivery counts only as a
/// publication the broker accepted while the state was due. The end of a
/// request (no scan lock, the exact interval) and a tick (its tick age) read the
/// one due row before either stamps; both publications are accepted and their
/// stamps run at once. The second stamp waits for the first's row lock and then
/// finds the row no longer as it read it due: one delivery, not two.
#[tokio::test]
async fn two_publishers_stamping_one_due_read_at_once_count_one_delivery() {
    use cf_gears_settings_service::domain::activation::notifier::tick_age;
    use cf_gears_settings_service::domain::activation::{NotificationStore, SubscriberTenant};
    use cf_gears_settings_service::infra::storage::notification_store::DbNotificationStore;

    let Some(r) = Rig::new("double-stamp").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = instance(1);
    r.register(a, ROOT).await;
    r.write(ROOT, "one").await;
    let store = DbNotificationStore::new(r.replica.gear.db().expect("initialized"));
    let pair = SubscriberTenant {
        subscriber: r.subscriber(),
        tenant: ROOT.to_string(),
    };
    let interval = Duration::from_secs(support::instances::INTERVAL_SECONDS);
    let by_request = store.due_of(&pair, interval).await.unwrap();
    let mut by_tick = store.due(tick_age(interval)).await.unwrap();
    by_tick.retain(|row| row.change.key == r.key.as_str());
    assert_eq!(
        (by_request.len(), by_tick.len()),
        (1, 1),
        "both read it due"
    );

    let (first, second) = tokio::join!(store.stamp_due(&by_request), store.stamp_due(&by_tick));
    assert_eq!(first.unwrap() + second.unwrap(), 1, "one stamp counted");
    let row = r.row(a, ROOT).await.unwrap();
    assert_eq!((row.deliveries, row.notified), (1, true), "one delivery");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

// ---------------------------------------------------------------------------
// Report against the call of silence
// ---------------------------------------------------------------------------

/// *Report against the call of silence*, the report commits first: the call
/// held before its guarded statement finds the row settled — an accounted
/// report leaves it `succeeded`, writes no `silent` record and leaves the
/// subscription unmarked.
#[tokio::test]
async fn a_report_committing_before_the_call_of_silence_leaves_the_row_settled() {
    let Some(r) = Rig::new("report-first").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = due_for_silence(&r).await;

    let call = hold(Step::CallSilent, r.key.as_str(), 1);
    let tick = r.tick_task();
    reached(&call).await;
    assert_eq!(r.confirm(a, ROOT).await, ReportResult::Applied);
    call.release();
    assert!(matches!(finished(tick).await, Tick::Complete { .. }));

    assert_eq!(r.status(a, ROOT).await, "succeeded");
    assert_eq!(r.outcomes(a).await, ["success"], "no `silent` record");
    assert!(!r.silent(a, ROOT).await, "the subscription is not marked");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// The `late` label of every response latency a Recorder observed; nothing
/// else is kept.
#[derive(Default)]
struct LateLabels(std::sync::Mutex<Vec<bool>>);

impl NotifyMetrics for LateLabels {
    fn publish_to_deliver(&self, _event: &'static str, _elapsed: Duration) {}
    fn response_latency(&self, _elapsed: Duration, late: bool) {
        self.0.lock().unwrap().push(late);
    }
    fn redelivery(&self) {}
    fn awaiting(&self, _count: u64) {}
    fn subscriptions(&self, _count: u64) {}
    fn silent(&self, _count: u64) {}
    fn silent_subscriptions(&self, _count: u64) {}
    fn silenced(&self) {}
    fn incomplete(&self) {}
    fn forgotten(&self) {}
    fn reaction(&self, _outcome: &'static str) {}
    fn report(&self, _result: &'static str) {}
    fn eviction(&self, _result: &'static str) {}
}

/// *Report against the call of silence*, the call commits first: one `silent`
/// record and the subscription marked, then an accounted report lands as a late
/// one — it settles the row, writes its record and clears `silent_since`. The
/// report read the row `awaiting` before the call; its latency is labelled
/// `late` all the same, from the row its guarded update settled.
#[tokio::test]
async fn an_accounted_report_landing_after_the_call_of_silence_is_a_late_one() {
    let Some(r) = Rig::new("call-first-accounted").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = due_for_silence(&r).await;
    let row = r.row(a, ROOT).await.unwrap();
    assert_eq!(row.status, "awaiting", "the report reads the row awaiting");

    let labels = Arc::new(LateLabels::default());
    let recorder = ActivationRecorder::new(
        r.replica.gear.db().expect("initialized"),
        ActivationRepo,
        AuditStore,
        Arc::clone(&labels) as Arc<dyn NotifyMetrics>,
    );
    let held = hold(Step::Report, r.key.as_str(), 1);
    let (who, key, change_ref, expected) = (
        r.gear.clone(),
        r.key.as_str().to_owned(),
        row.change_ref.clone(),
        row.expected_value.clone(),
    );
    let report = tokio::spawn(async move {
        recorder
            .receive_response(
                &who,
                BackResponse {
                    instance_id: Some(a),
                    change_ref,
                    tenant: ROOT.to_string(),
                    key,
                    status: "success".to_owned(),
                    applied_value: Some(expected),
                    detail: None,
                },
            )
            .await
    });
    reached(&held).await;
    assert!(matches!(r.tick().await, Tick::Complete { .. }));
    assert_eq!(r.status(a, ROOT).await, "silent", "the call committed");
    assert!(r.silent(a, ROOT).await);
    assert_eq!(r.outcomes(a).await, ["silent"]);

    held.release();
    assert_eq!(finished(report).await.unwrap(), ReportResult::Applied);
    assert_eq!(
        r.status(a, ROOT).await,
        "succeeded",
        "a late report settles"
    );
    assert_eq!(r.outcomes(a).await, ["silent", "success"]);
    assert!(!r.silent(a, ROOT).await, "and clears silent_since");
    assert_eq!(
        *labels.0.lock().unwrap(),
        [true],
        "the settled row was silent: late=\"true\""
    );
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report against the call of silence*, the call commits first and the report
/// is stale: it re-arms the row to `awaiting` and clears `silent_since`; the
/// call's record stays.
#[tokio::test]
async fn a_stale_report_landing_after_the_call_of_silence_re_arms_the_row() {
    let Some(r) = Rig::new("call-first-stale").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = due_for_silence(&r).await;

    let held = hold(Step::Rearm, r.key.as_str(), 1);
    let report = r.report_task(
        a,
        ROOT,
        Uuid::new_v4().to_string(),
        "success",
        Some("\"another\"".to_owned()),
    );
    reached(&held).await;
    assert!(matches!(r.tick().await, Tick::Complete { .. }));
    assert_eq!(r.status(a, ROOT).await, "silent");

    held.release();
    assert_eq!(finished(report).await.unwrap(), ReportResult::Ignored);
    let row = r.row(a, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", 0));
    assert!(!row.silent_since && !row.notified, "re-armed");
    assert!(!r.silent(a, ROOT).await);
    assert_eq!(r.outcomes(a).await, ["silent"], "the earlier record stays");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

// ---------------------------------------------------------------------------
// Report paused before its clear
// ---------------------------------------------------------------------------

/// *Report paused before its clear*, a stale report: the call commits while the
/// report is held between its row statement and its clear; the guarded clear,
/// run after that commit, finds the `silent` row and does not run — the end
/// state is the serial order report-then-call — and the instance's next report
/// re-arms it.
#[tokio::test]
async fn a_stale_report_paused_before_its_clear_ends_as_report_then_call() {
    let Some(r) = Rig::new("paused-stale").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = due_for_silence(&r).await;

    let held = hold(Step::ClearSilent, r.key.as_str(), 1);
    let report = r.report_task(
        a,
        ROOT,
        Uuid::new_v4().to_string(),
        "success",
        Some("\"another\"".to_owned()),
    );
    reached(&held).await;
    assert!(matches!(r.tick().await, Tick::Complete { .. }));
    assert_eq!(r.status(a, ROOT).await, "silent", "the call committed");
    assert!(r.silent(a, ROOT).await);

    held.release();
    assert_eq!(finished(report).await.unwrap(), ReportResult::Ignored);
    assert_eq!(r.status(a, ROOT).await, "silent");
    assert!(
        r.silent(a, ROOT).await,
        "the clear found the silent row and did not run"
    );
    assert_eq!(r.outcomes(a).await, ["silent"], "one record");

    assert_eq!(r.stale(a, ROOT).await, ReportResult::Ignored);
    assert_eq!(
        r.status(a, ROOT).await,
        "awaiting",
        "the next report re-arms"
    );
    assert!(!r.silent(a, ROOT).await);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report paused before its clear*, a registration paused at its refresh ends
/// the same way: the subscription stays silent beside its `silent` row.
#[tokio::test]
async fn a_registration_paused_at_its_refresh_ends_as_registration_then_call() {
    let Some(r) = Rig::new("paused-registration").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = due_for_silence(&r).await;

    let held = hold(Step::Register, r.key.as_str(), 1);
    let registration = r.register_task(a, ROOT);
    reached(&held).await;
    assert!(matches!(r.tick().await, Tick::Complete { .. }));
    assert_eq!(r.status(a, ROOT).await, "silent", "the call committed");

    held.release();
    finished(registration).await.unwrap();
    assert_eq!(r.status(a, ROOT).await, "silent");
    assert!(r.silent(a, ROOT).await, "the guarded clear did not run");
    assert_eq!(r.outcomes(a).await, ["silent"]);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report paused before its clear*, an accounted report: held after its row
/// statement, it keeps the row locked, so the call's row update waits for the
/// report's commit and then matches nothing — no `silent` record,
/// `silent_since` unset.
#[tokio::test]
async fn an_accounted_report_paused_before_its_clear_makes_the_call_match_nothing() {
    let Some(r) = Rig::new("paused-accounted").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let a = due_for_silence(&r).await;
    let row = r.row(a, ROOT).await.unwrap();

    let held = hold(Step::ClearSilent, r.key.as_str(), 1);
    let report = r.report_task(a, ROOT, row.change_ref, "success", Some(row.expected_value));
    reached(&held).await;
    let mut tick = r.tick_task();
    still_waiting(&mut tick).await;

    held.release();
    assert_eq!(finished(report).await.unwrap(), ReportResult::Applied);
    assert!(matches!(finished(tick).await, Tick::Complete { .. }));
    assert_eq!(r.status(a, ROOT).await, "succeeded");
    assert_eq!(r.outcomes(a).await, ["success"], "no `silent` record");
    assert!(!r.silent(a, ROOT).await);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report paused before its clear*, the call of silence not yet committed: the
/// call has made the row `silent` and found the subscription already silent —
/// every instance of the subscriber was silent, so the change expected them all
/// again — and is held before its record. A stale report, and separately a
/// registration, of the instance runs its clear meanwhile. The clear must not
/// read the row as it was before the call and end the silence the call is about
/// to commit: it waits for the call and then finds the `silent` row, so the end
/// state is the serial order report-then-call — the row `silent`, the
/// subscription silent.
#[tokio::test]
async fn a_clear_racing_an_uncommitted_call_of_silence_keeps_the_subscription_silent() {
    for by_registration in [false, true] {
        let name = if by_registration {
            "uncommitted-call-registration"
        } else {
            "uncommitted-call-report"
        };
        let Some(r) = Rig::new(name).await else {
            return;
        };
        let deadlocks = r.pg.deadlocks().await;
        let (a, b, _) = both_silent(&r).await;
        r.write(ROOT, "two").await;
        assert_eq!(r.holders(ROOT).await, [a, b], "all silent: both expected");
        assert!(r.silent(a, ROOT).await, "the subscription stays silent");
        r.deliveries(K).await;
        r.elapse().await;

        // The first call of the tick, `a`'s in instance order, is held with its
        // row `silent` and uncommitted.
        let call = hold(Step::SilentRecord, r.key.as_str(), 1);
        let tick = r.tick_task();
        reached(&call).await;
        let mut clearing = if by_registration {
            r.register_task(a, ROOT)
        } else {
            let report = r.report_task(
                a,
                ROOT,
                Uuid::new_v4().to_string(),
                "success",
                Some("\"another\"".to_owned()),
            );
            tokio::spawn(async move { finished(report).await.map(|_| ()) })
        };
        let early = tokio::time::timeout(support::instances::STILL_WAITING, &mut clearing).await;
        let waited = early.is_err();
        call.release();
        assert!(matches!(finished(tick).await, Tick::Complete { .. }));
        match early {
            Ok(done) => done.expect("the task did not panic").unwrap(),
            Err(_) => finished(clearing).await.unwrap(),
        }

        assert_eq!(r.status(a, ROOT).await, "silent", "the call committed");
        assert!(
            r.silent(a, ROOT).await,
            "the clear did not end the silence the call committed"
        );
        assert!(waited, "the clear waited for the call's commit");
        assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
    }
}

// ---------------------------------------------------------------------------
// Against the forgetting
// ---------------------------------------------------------------------------

/// *Report or registration against the forgetting*, clearing first: a report —
/// and, separately, a registration — of the silent instance commits while the
/// forgetting is held before its first statement; the forgetting then finds a
/// subscription no longer silent and keeps it and its rows.
#[tokio::test]
async fn a_report_or_a_registration_committing_before_the_forgetting_keeps_the_subscription() {
    for by_registration in [false, true] {
        let name = if by_registration {
            "forget-after-registration"
        } else {
            "forget-after-report"
        };
        let Some(r) = Rig::new(name).await else {
            return;
        };
        let deadlocks = r.pg.deadlocks().await;
        let (_, b, _) = silent_pair(&r).await;
        r.silent_for(b, ROOT, 25).await;

        let held = hold(Step::Forget, r.key.as_str(), 1);
        let tick = r.tick_task();
        reached(&held).await;
        if by_registration {
            r.register(b, ROOT).await;
        } else {
            assert_eq!(r.stale(b, ROOT).await, ReportResult::Ignored);
        }
        held.release();
        assert!(matches!(finished(tick).await, Tick::Complete { .. }));

        assert!(r.sub(b, ROOT).await.is_some(), "the subscription is kept");
        assert!(!r.silent(b, ROOT).await);
        let row = r.row(b, ROOT).await.expect("and so is its row");
        assert_eq!(
            row.status, "awaiting",
            "re-armed by the report or registration"
        );
        assert_eq!(r.outcomes(b).await, ["silent"], "no record altered");
        assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
    }
}

/// *Report or registration against the forgetting*, forgetting first: the
/// subscription and its rows go in one transaction; a report that read the
/// subscription before is answered `ignored` and its next one `not_subscribed`;
/// a registration of the same `instance_id` inserts a new subscription with no
/// row and no old row comes back.
#[tokio::test]
async fn a_forgetting_committing_before_the_report_answers_ignored_then_not_subscribed() {
    let Some(r) = Rig::new("forget-first").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (_, b, _) = silent_pair(&r).await;
    r.silent_for(b, ROOT, 25).await;
    let row = r.row(b, ROOT).await.unwrap();

    let held = hold(Step::Report, r.key.as_str(), 1);
    let report = r.report_task(
        b,
        ROOT,
        row.change_ref.clone(),
        "success",
        Some(row.expected_value.clone()),
    );
    reached(&held).await;
    assert!(matches!(r.tick().await, Tick::Complete { .. }));
    assert!(r.sub(b, ROOT).await.is_none(), "forgotten with its rows");
    assert!(r.row(b, ROOT).await.is_none());

    held.release();
    assert_eq!(
        finished(report).await.unwrap(),
        ReportResult::Ignored,
        "it read the subscription before the forgetting committed"
    );
    assert!(r.row(b, ROOT).await.is_none(), "no old row comes back");
    assert_eq!(r.outcomes(b).await, ["silent"], "no record altered");
    let next = r
        .report(
            b,
            ROOT,
            &row.change_ref,
            "success",
            Some(row.expected_value.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(next, ReportResult::NotSubscribed, "its next report");

    r.register(b, ROOT).await;
    let sub = r.sub(b, ROOT).await.expect("a new subscription");
    assert!(!sub.silent);
    assert!(r.row(b, ROOT).await.is_none(), "with no row");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report or registration against the forgetting*, forgetting first, the
/// registration between its lookup and its refresh: the registration found the
/// silent subscription, the forgetting — which takes no key lock — then deletes
/// it and commits, and the registration's refresh matches no row. It must not
/// answer registered with nothing stored: a registration inserts a new
/// subscription, with no row and not silent.
#[tokio::test]
async fn a_registration_whose_subscription_is_forgotten_after_its_lookup_inserts_a_new_one() {
    let Some(r) = Rig::new("forget-under-refresh").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b, _) = silent_pair(&r).await;
    // The next change leaves the silent `b` out: it holds no row, so the
    // registration re-arms nothing and holds no lock the forgetting meets.
    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a]);
    r.silent_for(b, ROOT, 25).await;

    let held = hold(Step::RegisterRefresh, r.key.as_str(), 1);
    let registration = r.register_task(b, ROOT);
    reached(&held).await;
    assert!(matches!(r.tick().await, Tick::Complete { .. }));
    assert!(r.sub(b, ROOT).await.is_none(), "the forgetting committed");

    held.release();
    finished(registration)
        .await
        .expect("the registration answers");
    let sub = r
        .sub(b, ROOT)
        .await
        .expect("the registration stored a subscription");
    assert!(!sub.silent, "a new subscription, not silent");
    assert!(r.row(b, ROOT).await.is_none(), "with no row");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report or registration against the forgetting*, a clear between the
/// forgetting's row delete and its subscription delete: the subscription is
/// kept, without its row. Every instance of the subscriber is silent, so the
/// next change gives each a row while their subscriptions stay silent; the
/// forgetting of one deletes its row and is held before the subscription's
/// delete, a stale report of that instance clears the subscription, and the
/// forgetting's guarded delete then matches nothing.
#[tokio::test]
async fn a_clear_between_the_forgettings_two_deletes_keeps_the_subscription_without_its_rows() {
    let Some(r) = Rig::new("forget-between").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b, _) = both_silent(&r).await;
    r.write(ROOT, "two").await;
    assert_eq!(
        r.holders(ROOT).await,
        [a, b],
        "all silent: every one expected"
    );
    r.silent_for(b, ROOT, 25).await;

    let clear = hold(Step::ClearSilent, r.key.as_str(), 1);
    let report = r.report_task(
        b,
        ROOT,
        Uuid::new_v4().to_string(),
        "success",
        Some("\"another\"".to_owned()),
    );
    reached(&clear).await;
    let between = hold(Step::ForgetSubscription, r.key.as_str(), 1);
    let tick = r.tick_task();
    reached(&between).await;
    assert!(
        r.row(b, ROOT).await.is_some(),
        "its delete is not committed"
    );

    clear.release();
    assert_eq!(finished(report).await.unwrap(), ReportResult::Ignored);
    between.release();
    assert!(matches!(finished(tick).await, Tick::Complete { .. }));

    let sub = r.sub(b, ROOT).await.expect("the subscription is kept");
    assert!(!sub.silent, "cleared by the report");
    assert!(r.row(b, ROOT).await.is_none(), "without its row");
    assert_eq!(
        r.holders(ROOT).await,
        [a],
        "the other instance is untouched"
    );
    assert_eq!(r.outcomes(b).await, ["silent"], "no record altered");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Forgetting after a day*, and the state half of *Read before and after
/// forgetting*: a subscription silent for longer than a day goes with its rows
/// and the confirmed instance's row stays; the records it left — the report's
/// and the call of silence's — are unchanged, and a later report of it is
/// answered `not_subscribed`.
#[tokio::test]
async fn a_subscription_silent_for_a_day_is_forgotten_with_its_rows_and_its_records_stay() {
    let Some(r) = Rig::new("forget-day").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b, x) = silent_pair(&r).await;
    let records = r.all_records().await;
    assert_eq!(r.holders(ROOT).await, [a, b], "before: B is in the account");

    r.silent_for(b, ROOT, 23).await;
    r.tick_due().await;
    assert!(r.sub(b, ROOT).await.is_some(), "not yet a day");

    r.silent_for(b, ROOT, 25).await;
    r.tick_due().await;
    assert!(r.sub(b, ROOT).await.is_none(), "forgotten");
    assert_eq!(
        r.holders(ROOT).await,
        [a],
        "after: the account is A's alone"
    );
    assert_eq!(r.status(a, ROOT).await, "succeeded");
    assert_eq!(
        r.all_records().await,
        records,
        "no record written or altered"
    );
    assert_eq!(
        r.records(b).await,
        [("silent".to_owned(), x.clone(), ROOT.to_string())]
    );
    let later = r.report(b, ROOT, &x, "success", Some("\"one\"")).await;
    assert_eq!(later.unwrap(), ReportResult::NotSubscribed);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Forgetting after a day*, the second case: a subscription called silent
/// again on a later change within the day is forgotten a day after it was first
/// called silent, not after the later call.
#[tokio::test]
async fn a_later_call_of_silence_does_not_move_the_forgetting_day() {
    let Some(r) = Rig::new("forget-first-day").await else {
        return;
    };
    let (a, c, _) = both_silent(&r).await;
    r.silent_for(c, ROOT, 23).await;

    // Every instance is silent: the next change gives each a row, and each is
    // called silent again.
    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a, c]);
    r.run_to_silence().await;
    for who in [a, c] {
        assert_eq!(r.status(who, ROOT).await, "silent");
        assert_eq!(r.outcomes(who).await, ["silent", "silent"]);
    }
    assert_eq!(
        r.silent_hours(c, ROOT).await,
        23,
        "the day still counts from the first call"
    );

    r.silent_for(c, ROOT, 25).await;
    r.tick_due().await;
    assert!(
        r.sub(c, ROOT).await.is_none(),
        "forgotten a day after the first"
    );
    assert!(r.row(c, ROOT).await.is_none());
    assert!(
        r.sub(a, ROOT).await.is_some(),
        "the other's day is not over"
    );
    assert!(r.row(a, ROOT).await.is_some());
}

/// *Day count unaffected by an outage*: the broker is down — a replica with no
/// broker in its hub — from before `T + SILENT_FORGET_SECONDS` to after it; the
/// pass forgets the subscription at the first tick after, during the outage,
/// and its rows go in the same transaction. The outage neither pauses nor
/// extends the day.
#[tokio::test]
async fn the_forgetting_day_runs_through_a_broker_outage() {
    let Some(r) = Rig::new("forget-outage").await else {
        return;
    };
    let (a, b, x) = silent_pair(&r).await;
    let (_down, no_broker) = down_replica(&r).await;

    r.silent_for(b, ROOT, 23).await;
    assert_eq!(no_broker.tick(&r.cancel).await, Tick::Idle);
    assert!(r.sub(b, ROOT).await.is_some(), "before the day is over");
    assert!(r.row(b, ROOT).await.is_some());

    r.silent_for(b, ROOT, 25).await;
    assert_eq!(
        no_broker.tick(&r.cancel).await,
        Tick::Idle,
        "no broker resolved"
    );
    assert!(
        r.sub(b, ROOT).await.is_none(),
        "forgotten during the outage"
    );
    assert!(r.row(b, ROOT).await.is_none(), "in the same transaction");
    assert!(r.row(a, ROOT).await.is_some());
    assert_eq!(r.outcomes(b).await, ["silent"]);
    assert_eq!(r.records(b).await[0].1, x);
}

/// A second replica over the same database whose hub holds no Event Broker,
/// and a notifier over it: the pass of a deployment whose broker is down.
async fn down_replica(
    r: &Rig,
) -> (
    Replica,
    std::sync::Arc<cf_gears_settings_service::domain::activation::ActivationNotifier>,
) {
    use cf_gears_settings_service::domain::activation::{ActivationNotifier, NotificationGateway};
    use cf_gears_settings_service::infra::activation_metrics::OtelNotifyMetrics;
    use cf_gears_settings_service::infra::mutation_lock::DeliveryScanLock;
    use cf_gears_settings_service::infra::storage::notification_store::DbNotificationStore;
    use std::sync::Arc;
    use std::time::Duration;
    use support::instances::{INTERVAL_SECONDS, REPORT_TIMEOUT_SECONDS};

    let replica = Replica::init_without_broker(&r.pg, &r.platform, json!({})).await;
    let db = replica.gear.db().expect("initialized");
    let notifier = Arc::new(
        ActivationNotifier::new(
            Arc::new(DbNotificationStore::new(Arc::clone(&db))),
            replica.gear.event_broker().expect("initialized") as Arc<dyn NotificationGateway>,
            Arc::new(DeliveryScanLock::new(db)),
            Arc::new(OtelNotifyMetrics::new()),
            Duration::from_secs(INTERVAL_SECONDS),
        )
        .with_report_timeout(Duration::from_secs(REPORT_TIMEOUT_SECONDS)),
    );
    (replica, notifier)
}

/// *Broker outage does not count*: while the broker refuses every publish — for
/// longer than the timeout — no row is called silent and no delivery counted;
/// with it accepting again, silence comes `K` accepted deliveries later. A
/// pass with no broker resolved calls nothing silent either, a row at `K`
/// included.
#[tokio::test]
async fn a_broker_outage_counts_no_delivery_and_calls_nothing_silent() {
    let Some(r) = Rig::new("outage").await else {
        return;
    };
    let a = instance(1);
    r.register(a, ROOT).await;
    r.write(ROOT, "one").await;

    r.platform.broker.refuse_notifications(true);
    for _ in 0..=K + 1 {
        r.tick_due().await;
    }
    let row = r.row(a, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", 0));
    assert!(!row.notified, "no accepted publication stamped it");
    assert!(!r.silent(a, ROOT).await);

    r.platform.broker.refuse_notifications(false);
    r.deliveries(K).await;
    let row = r.row(a, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", K));
    r.tick_due().await;
    assert_eq!(
        r.status(a, ROOT).await,
        "silent",
        "K accepted deliveries after the outage"
    );

    // A row at `K`, due, with no broker resolved: nothing is called silent.
    r.stale(a, ROOT).await;
    let (_down, no_broker) = down_replica(&r).await;
    r.deliveries(K).await;
    r.elapse().await;
    assert_eq!(no_broker.tick(&r.cancel).await, Tick::Idle);
    assert_eq!(r.status(a, ROOT).await, "awaiting");
}

// ---------------------------------------------------------------------------
// Re-entry
// ---------------------------------------------------------------------------

/// *Re-entry re-arms the current change*: `B`'s row of the current change is
/// `silent`; `B` reports an earlier `change_ref` with another value — the row
/// is `awaiting` again with no deliveries, `silent_since` cleared, the next
/// tick publishes the key, and `B`'s confirmation settles it with one more
/// record; the earlier `silent` record stays.
#[tokio::test]
async fn a_stale_report_re_arms_a_silent_row_of_the_current_change() {
    let Some(r) = Rig::new("re-arm").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (_, b, x) = silent_pair(&r).await;

    assert_eq!(r.stale(b, ROOT).await, ReportResult::Ignored);
    let row = r.row(b, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", 0));
    assert_eq!(row.change_ref, x, "still the current change");
    assert!(!row.silent_since && !row.notified);
    assert!(!r.silent(b, ROOT).await);

    let before = r.notified();
    r.deliveries(1).await;
    assert_eq!(r.notified(), before + 1, "the next scan publishes the key");
    assert_eq!(r.confirm(b, ROOT).await, ReportResult::Applied);
    assert_eq!(r.status(b, ROOT).await, "succeeded");
    assert_eq!(r.outcomes(b).await, ["silent", "success"]);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Re-entry by registration*: `B` silent and excluded from the current change
/// registers again — `silent_since` cleared, no row created, outside the
/// current account and expected by the next change; separately, `B`'s row of
/// the current change `silent`, it registers again — the row is re-armed and
/// `B` is notified again.
#[tokio::test]
async fn a_registration_ends_the_silence_and_re_arms_a_silent_row() {
    let Some(r) = Rig::new("re-entry-registration").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b, _) = silent_pair(&r).await;

    // First case: B is excluded from the current change.
    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a]);
    r.register(b, ROOT).await;
    assert!(!r.silent(b, ROOT).await, "silent_since cleared");
    assert_eq!(
        r.holders(ROOT).await,
        [a],
        "no row created, outside the account"
    );
    r.write(ROOT, "three").await;
    assert_eq!(r.holders(ROOT).await, [a, b], "expected by the next change");

    // Second case: B's row of the current change is silent.
    r.deliveries(1).await;
    assert_eq!(r.confirm(a, ROOT).await, ReportResult::Applied);
    r.deliveries(1).await;
    r.tick_due().await;
    assert_eq!(r.status(b, ROOT).await, "silent");
    r.register(b, ROOT).await;
    let row = r.row(b, ROOT).await.unwrap();
    assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", 0));
    assert!(!row.notified && !r.silent(b, ROOT).await);
    let before = r.notified();
    r.deliveries(1).await;
    assert_eq!(r.notified(), before + 1, "B is notified again");
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Re-entry by a stale report ends silence, counts from the next change*: a
/// silent instance the current change left out reports an earlier `change_ref`
/// — answered `ignored`, `silent_since` cleared, no row of the current change
/// created — and the next change expects it.
#[tokio::test]
async fn a_stale_report_of_an_excluded_instance_ends_its_silence_and_counts_from_the_next_change() {
    let Some(r) = Rig::new("re-entry-stale").await else {
        return;
    };
    let (a, b, _) = silent_pair(&r).await;
    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a], "B is left out");
    let before = r.row(a, ROOT).await.unwrap();

    assert_eq!(r.stale(b, ROOT).await, ReportResult::Ignored);
    assert!(!r.silent(b, ROOT).await, "silent_since cleared");
    assert!(
        r.row(b, ROOT).await.is_none(),
        "no row of the current change"
    );
    assert_eq!(
        r.row(a, ROOT).await.unwrap(),
        before,
        "the account is unchanged"
    );

    r.write(ROOT, "three").await;
    assert_eq!(r.holders(ROOT).await, [a, b], "the next change expects it");
}

/// *Every instance silent*: both instances of a subscriber silent at `(key, T)`
/// and a new change — both get rows and are notified, and if both stay silent
/// they are called silent again.
#[tokio::test]
async fn a_change_with_every_instance_silent_expects_them_all_again() {
    let Some(r) = Rig::new("all-silent").await else {
        return;
    };
    let (a, b, _) = both_silent(&r).await;

    r.write(ROOT, "two").await;
    let y = r.change(ROOT).await;
    assert_eq!(r.holders(ROOT).await, [a, b], "both get rows");
    for who in [a, b] {
        let row = r.row(who, ROOT).await.unwrap();
        assert_eq!((row.status.as_str(), row.deliveries), ("awaiting", 0));
        assert_eq!(row.change_ref, y);
    }
    let before = r.notified();
    r.deliveries(1).await;
    assert_eq!(r.notified(), before + 1, "and are notified");
    r.deliveries(K - 1).await;
    r.tick_due().await;
    for who in [a, b] {
        assert_eq!(r.status(who, ROOT).await, "silent", "silent again");
        assert_eq!(r.outcomes(who).await, ["silent", "silent"]);
    }
}

/// *Registered after the change*: an instance that registers after a change
/// committed has no row for it, its fresh read returns the committed value, and
/// the next change writes its row.
#[tokio::test]
async fn an_instance_registered_after_the_change_has_no_row_and_reads_the_committed_value() {
    let Some(r) = Rig::new("registered-after").await else {
        return;
    };
    let (a, c) = (instance(1), instance(3));
    r.register(a, ROOT).await;
    r.write(ROOT, "one").await;
    r.register(c, ROOT).await;

    assert_eq!(r.holders(ROOT).await, [a], "no row for the newcomer");
    assert_eq!(r.replica.read(&r.key, ROOT).await, "one", "its fresh read");
    assert!(r.records(c).await.is_empty());
    assert_eq!(r.confirm(a, ROOT).await, ReportResult::Applied);
    assert_eq!(
        r.status(a, ROOT).await,
        "succeeded",
        "outcome derived without C"
    );

    r.write(ROOT, "two").await;
    assert_eq!(
        r.holders(ROOT).await,
        [a, c],
        "the next change writes its row"
    );
}

// ---------------------------------------------------------------------------
// Races with a change
// ---------------------------------------------------------------------------

/// *Report against the all-silent decision*, the report first: both instances
/// silent, a change held between its candidate step and its commit, one
/// instance's stale report commits meanwhile — the change's statement then
/// writes a row for the reporting instance only and the other is excluded.
#[tokio::test]
async fn a_report_before_the_change_statement_leaves_the_other_silent_instance_excluded() {
    let Some(r) = Rig::new("all-silent-report-first").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b, _) = both_silent(&r).await;
    let tag = r.pg.row_tag(&r.key, ROOT).await;

    let pause = WritePause::install(&r);
    let write = write_task(&r, ROOT, "two", tag);
    pause.reached().await;
    assert_eq!(r.stale(b, ROOT).await, ReportResult::Ignored);
    assert!(!r.silent(b, ROOT).await, "B re-entered");
    pause.release();
    finished(write).await;
    r.platform.broker.refuse_notifications(false);

    assert_eq!(
        r.holders(ROOT).await,
        [b],
        "a row for the reporting instance only"
    );
    assert!(r.silent(a, ROOT).await);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Report against the all-silent decision*, the change first: both instances
/// get rows, and the report's clearing leaves the new expected set alone — no
/// ordering leaves the subscriber with zero expected instances.
#[tokio::test]
async fn a_report_after_the_change_statement_leaves_the_new_expected_set_alone() {
    let Some(r) = Rig::new("all-silent-change-first").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let (a, b, _) = both_silent(&r).await;

    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [a, b], "both get rows");
    assert_eq!(r.stale(b, ROOT).await, ReportResult::Ignored);
    assert!(!r.silent(b, ROOT).await, "B's silence ended");
    assert_eq!(
        r.holders(ROOT).await,
        [a, b],
        "the expected set is unchanged"
    );
    for who in [a, b] {
        assert_eq!(r.status(who, ROOT).await, "awaiting");
    }
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

// ---------------------------------------------------------------------------
// Several tenants, several gears, the cap
// ---------------------------------------------------------------------------

/// *Cascading accounts per tenant*: a `cascading` change at the root reaches
/// the subscribers at the root and at a descendant — separate rows, and a
/// silent instance at the descendant marks only the descendant's account.
#[tokio::test]
async fn a_cascading_change_keeps_one_account_per_tenant() {
    let Some(r) = Rig::new("cascading").await else {
        return;
    };
    let (a, b) = (instance(1), instance(2));
    for who in [a, b] {
        r.register(who, ROOT).await;
        r.register(who, CHILD).await;
    }
    r.write(ROOT, "one").await;
    let x = r.change(ROOT).await;
    assert_eq!(r.holders(ROOT).await, [a, b]);
    assert_eq!(r.holders(CHILD).await, [a, b], "a row per tenant");

    r.deliveries(1).await;
    for (who, tenant) in [(a, ROOT), (a, CHILD), (b, ROOT)] {
        assert_eq!(r.confirm(who, tenant).await, ReportResult::Applied);
    }
    r.deliveries(1).await;
    r.tick_due().await;

    assert_eq!(r.status(b, CHILD).await, "silent");
    assert_eq!(
        r.status(b, ROOT).await,
        "succeeded",
        "the root's account is whole"
    );
    assert!(r.silent(b, CHILD).await);
    assert!(
        !r.silent(b, ROOT).await,
        "only the descendant's account is marked"
    );
    for tenant in [ROOT, CHILD] {
        assert_eq!(r.status(a, tenant).await, "succeeded");
    }
    assert_eq!(
        r.records(b).await,
        [
            ("success".to_owned(), x.clone(), ROOT.to_string()),
            ("silent".to_owned(), x, CHILD.to_string())
        ]
    );
}

/// *First change after never-asked ghosts*: one live instance and ten ghost
/// registrations no change has asked — the change writes eleven rows, after `K`
/// deliveries the ghosts are `silent` with ten records, the next change expects
/// the live instance only, and the ghosts are forgotten a day later.
#[tokio::test]
async fn the_first_change_after_ghost_registrations_calls_them_silent_and_the_next_leaves_them_out()
{
    let Some(r) = Rig::new("ghosts").await else {
        return;
    };
    let live = instance(1);
    let ghosts: Vec<Uuid> = (10..20).map(instance).collect();
    r.register(live, ROOT).await;
    for ghost in &ghosts {
        r.register(*ghost, ROOT).await;
    }
    r.write(ROOT, "one").await;
    assert_eq!(r.rows().await, 11, "eleven rows");

    r.deliveries(1).await;
    assert_eq!(r.confirm(live, ROOT).await, ReportResult::Applied);
    r.deliveries(K - 1).await;
    r.tick_due().await;
    for ghost in &ghosts {
        assert_eq!(r.status(*ghost, ROOT).await, "silent");
        assert_eq!(r.outcomes(*ghost).await, ["silent"]);
    }
    assert_eq!(r.status(live, ROOT).await, "succeeded");
    assert_eq!(
        r.all_records().await,
        11,
        "ten silent records and the report"
    );

    r.write(ROOT, "two").await;
    assert_eq!(r.holders(ROOT).await, [live], "the live instance only");

    for ghost in &ghosts {
        r.silent_for(*ghost, ROOT, 25).await;
    }
    r.tick_due().await;
    assert_eq!(r.subs().await, 1, "the ghosts are forgotten");
    assert_eq!(r.all_records().await, 11, "their records stay");
}

/// *Eviction at the cap*: with 64 registrations of one `(subscriber, key,
/// tenant)` all expected by the current change and none silent, a 65th is
/// refused; once the oldest is silent it is evicted with its rows in the
/// registering transaction, the new registration commits, and the evicted
/// instance's next report answers `not_subscribed`.
#[tokio::test]
async fn the_registration_cap_refuses_while_all_are_expected_and_evicts_a_silent_one() {
    let Some(r) = Rig::new("cap").await else {
        return;
    };
    let deadlocks = r.pg.deadlocks().await;
    let all: Vec<Uuid> = (100..164).map(instance).collect();
    for who in &all {
        r.register(*who, ROOT).await;
    }
    assert_eq!(r.subs().await, 64);

    // Evictable at once: no instance holds a state, the oldest goes.
    let newcomer = instance(1000);
    r.register(newcomer, ROOT).await;
    assert_eq!(r.subs().await, 64, "the cap holds");
    assert!(r.sub(all[0], ROOT).await.is_none(), "the oldest is evicted");
    assert!(r.sub(newcomer, ROOT).await.is_some());
    let next = r
        .report(
            all[0],
            ROOT,
            &Uuid::new_v4().to_string(),
            "success",
            Some("\"v\""),
        )
        .await
        .unwrap();
    assert_eq!(
        next,
        ReportResult::NotSubscribed,
        "the evicted one's next report"
    );

    // Every registration expected and none silent: nothing is evictable.
    r.write(ROOT, "one").await;
    assert_eq!(r.rows().await, 64);
    let refused = r.register_as(&r.gear, instance(2000), ROOT, None).await;
    assert!(
        matches!(refused, Err(DomainError::Unavailable { .. })),
        "refused: {refused:?}"
    );
    assert_eq!(r.subs().await, 64, "nothing was written");
    assert_eq!(r.rows().await, 64);

    // The oldest is silent: evicted with its rows.
    let oldest = all[1];
    r.pg.exec(
        "UPDATE activation_state SET status = 'silent', silent_since = now()
          WHERE key = $1 AND instance_id = $2",
        vec![r.key.as_str().into(), oldest.into()],
    )
    .await;
    r.pg.exec(
        "UPDATE subscriptions SET silent_since = now() WHERE key = $1 AND instance_id = $2",
        vec![r.key.as_str().into(), oldest.into()],
    )
    .await;
    r.register(instance(2000), ROOT).await;
    assert!(r.sub(oldest, ROOT).await.is_none(), "evicted");
    assert!(r.row(oldest, ROOT).await.is_none(), "with its rows");
    assert_eq!(r.subs().await, 64);
    assert_eq!(r.rows().await, 63);
    assert_eq!(r.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Cross-gear report and unsubscribe*: another gear naming the first gear's
/// instance id for the same key and tenant changes nothing — its report answers
/// `not_subscribed`, its unsubscribe `404`; the first gear's subscription, row
/// and history are untouched.
#[tokio::test]
async fn another_gear_naming_an_instance_changes_nothing_of_the_first_gears() {
    let Some(r) = Rig::new("cross-gear").await else {
        return;
    };
    let i = instance(1);
    r.register(i, ROOT).await;
    r.write(ROOT, "one").await;
    let row = r.row(i, ROOT).await.unwrap();
    let other = gear_context();

    let report = Rig::report_with(
        &r.replica,
        &other,
        &r.key,
        i,
        ROOT,
        &row.change_ref,
        "success",
        Some(row.expected_value.as_str()),
    )
    .await
    .unwrap();
    assert_eq!(report, ReportResult::NotSubscribed);
    let unsubscribed = r.unsubscribe(&other, i, ROOT).await;
    assert!(
        matches!(unsubscribed, Err(DomainError::NotFound { .. })),
        "{unsubscribed:?}"
    );

    assert!(
        r.sub(i, ROOT).await.is_some(),
        "the subscription is unchanged"
    );
    assert_eq!(r.row(i, ROOT).await.unwrap(), row, "so is its row");
    assert_eq!(r.all_records().await, 0, "and the history");
    assert_eq!(r.confirm(i, ROOT).await, ReportResult::Applied);
}

/// *A person is no subscriber*: a caller whose subject is of an interactive
/// subject type — here the very subject id of the subscribed gear — is refused
/// its registration, its report and its unsubscribe; the gear's subscription,
/// its row and the history are untouched, and the gear's own report still
/// applies.
#[tokio::test]
async fn a_person_is_refused_every_consumer_call_and_changes_nothing() {
    let Some(r) = Rig::new("person-refused").await else {
        return;
    };
    let i = instance(1);
    r.register(i, ROOT).await;
    r.write(ROOT, "one").await;
    let row = r.row(i, ROOT).await.unwrap();
    let person = toolkit_security::SecurityContext::builder()
        .subject_id(r.gear.subject_id())
        .subject_tenant_id(ROOT)
        .subject_type(cf_gears_settings_service::domain::stepup::USER_SUBJECT_TYPE)
        .build()
        .expect("a context");

    let registered = r.register_as(&person, instance(2), ROOT, None).await;
    assert!(
        matches!(registered, Err(DomainError::Unauthorized { .. })),
        "{registered:?}"
    );
    let report = Rig::report_with(
        &r.replica,
        &person,
        &r.key,
        i,
        ROOT,
        &row.change_ref,
        "success",
        Some(row.expected_value.as_str()),
    )
    .await;
    assert!(
        matches!(report, Err(DomainError::Unauthorized { .. })),
        "{report:?}"
    );
    let unsubscribed = r.unsubscribe(&person, i, ROOT).await;
    assert!(
        matches!(unsubscribed, Err(DomainError::Unauthorized { .. })),
        "{unsubscribed:?}"
    );

    assert_eq!(r.subs().await, 1, "nothing registered");
    assert!(
        r.sub(i, ROOT).await.is_some(),
        "the subscription is unchanged"
    );
    assert_eq!(r.row(i, ROOT).await.unwrap(), row, "so is its row");
    assert_eq!(r.all_records().await, 0, "and the history");
    assert_eq!(r.confirm(i, ROOT).await, ReportResult::Applied);
}

// ---------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------

/// *Report idempotency*, amended: a duplicate report of each instance makes one
/// transition and one record, and `uq_activation_state` keeps one row per
/// `(subscriber, instance_id, key, tenant)` — a second row for one instance is
/// refused while two instances hold two.
#[tokio::test]
async fn a_duplicate_report_is_one_transition_per_instance_and_the_row_is_unique_per_instance() {
    let Some(r) = Rig::new("report-idempotent").await else {
        return;
    };
    let (a, b) = (instance(1), instance(2));
    r.register(a, ROOT).await;
    r.register(b, ROOT).await;
    r.write(ROOT, "one").await;
    let x = r.change(ROOT).await;

    for who in [a, b] {
        let row = r.row(who, ROOT).await.unwrap();
        for expected in [ReportResult::Applied, ReportResult::Ignored] {
            let again = r
                .report(who, ROOT, &x, "success", Some(row.expected_value.as_str()))
                .await
                .unwrap();
            assert_eq!(again, expected);
        }
        assert_eq!(r.outcomes(who).await, ["success"], "one record");
    }
    assert_eq!(r.rows().await, 2, "two rows for two instances");

    let duplicate =
        r.pg.conn
            .execute_unprepared(&format!(
                "INSERT INTO activation_state
               (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value)
             VALUES ('{}', '{a}', '{}', '{ROOT}', '{}', now(), 'x')",
                r.subscriber(),
                r.key.as_str(),
                Uuid::new_v4()
            ))
            .await;
    assert!(duplicate.is_err(), "`uq_activation_state` is per instance");
}

/// *Subscription idempotency*, amended: registering the same `(subscriber,
/// instance_id, key, tenant)` again keeps one row —
/// `uq_sub_subscriber_instance_key_tenant` refuses a second — and clears
/// `silent_since`; two instances of one subscriber hold two.
#[tokio::test]
async fn registering_an_instance_again_keeps_one_subscription_and_clears_its_silence() {
    let Some(r) = Rig::new("subscription-idempotent").await else {
        return;
    };
    let (a, b, _) = silent_pair(&r).await;
    r.write(ROOT, "two").await;
    assert!(r.silent(b, ROOT).await);

    let again = r
        .register_as(&r.gear, b, ROOT, Some("host-b"))
        .await
        .unwrap();
    assert!(again.silent_since.is_none(), "the answer says so");
    let sub = r.sub(b, ROOT).await.unwrap();
    assert!(!sub.silent, "silent_since cleared");
    assert_eq!(sub.label.as_deref(), Some("host-b"));
    assert_eq!(r.subs().await, 2, "two instances hold two");
    assert!(r.sub(a, ROOT).await.is_some());

    let insert = |instance: Uuid| {
        format!(
            "INSERT INTO subscriptions (id, subscriber, instance_id, key, tenant)
             VALUES ('{}', '{}', '{instance}', '{}', '{ROOT}')",
            Uuid::new_v4(),
            r.subscriber(),
            r.key.as_str()
        )
    };
    r.pg.conn
        .execute_unprepared(&insert(instance(9)))
        .await
        .expect("a third instance holds a third subscription");
    let duplicate = r.pg.conn.execute_unprepared(&insert(b)).await;
    assert!(
        duplicate.is_err(),
        "`uq_sub_subscriber_instance_key_tenant` is per instance"
    );
}

// ---------------------------------------------------------------------------
// Row-lock order of the multi-row statements
// ---------------------------------------------------------------------------

/// The subscriber of the lock bench's rows.
const BENCH_SUBSCRIBER: &str = "gear-lock-order";

/// `rows`, given in the canonical order `(subscriber, instance_id, key,
/// tenant)`, inserted in the reverse order, so that an index walk over rows
/// of equal index keys — the heap order — visits them last to first. Every
/// row gets a state of `change`; a subscription too when `subscribed`.
/// Returns the subscription ids in the canonical order.
async fn reversed(bench: &LockBench, rows: &[Ident], change: Uuid, subscribed: bool) -> Vec<Uuid> {
    let mut ids = Vec::new();
    if subscribed {
        for row in rows.iter().rev() {
            ids.push(bench.subscribe(row).await);
        }
        ids.reverse();
    }
    for row in rows.iter().rev() {
        bench.state(row, change).await;
    }
    ids
}

/// The multi-row statement run by `task` takes its row locks in the
/// canonical order: with `rows[mid]` held by another writer, the statement
/// waits on it holding every row before it and none after it — whatever
/// index or join its plan walks. Then the holder commits and the statement
/// finishes; its result is returned.
async fn locks_in_canonical_order<T: Send + 'static>(
    bench: &LockBench,
    rows: &[Ident],
    mid: usize,
    holder: sea_orm::DatabaseTransaction,
    task: tokio::task::JoinHandle<T>,
) -> T {
    bench.blocked(1).await;
    for (n, row) in rows.iter().enumerate() {
        if n != mid {
            assert_eq!(
                bench.locked(row).await,
                n < mid,
                "row {n} of {} is locked exactly when it comes before the held row {mid}",
                rows.len()
            );
        }
    }
    holder.commit().await.expect("the holder commits");
    finished(task).await
}

/// *Concurrency Testing*, the canonical row-lock order: a stamp of a pair's
/// due states across two keys and a change's overwrite of one of those keys,
/// both while a third writer holds the pair's state of the other key. Left to
/// their plans the two would lock the pair's rows in opposite orders — the
/// stamp along `idx_activation_awaiting` in heap order, the overwrite along
/// the read set's subscriptions — and, once the holder commits, each would
/// wait for a row the other holds: `deadlock detected`, the change's write
/// possibly the victim. In the canonical order the overwrite waits behind the
/// stamp and both commit; `pg_stat_database.deadlocks` is unchanged.
#[tokio::test]
async fn a_stamp_and_a_changes_overwrite_meeting_on_one_pair_commit_without_a_deadlock() {
    use cf_gears_settings_service::domain::activation::ActivationRepository as _;
    use cf_gears_settings_service::domain::activation::publisher::{StateRow, StateWrite};

    let Some(bench) = LockBench::new("lock-order-overwrite").await else {
        return;
    };
    let deadlocks = bench.pg.deadlocks().await;
    // Canonical order: r1 < q < r2.
    let r1 = ident(BENCH_SUBSCRIBER, 1, "lock.a");
    let q = ident(BENCH_SUBSCRIBER, 1, "lock.b");
    let r2 = ident(BENCH_SUBSCRIBER, 2, "lock.a");
    let first = Uuid::from_u128(0xc1);
    let (sub1, sub2) = (bench.subscribe(&r1).await, bench.subscribe(&r2).await);
    bench.subscribe(&q).await;
    // The heap holds r2, q, r1: the order the stamp's index walk visits them,
    // while the overwrite visits the subscriptions' order, r1 then r2.
    for row in [&r2, &q, &r1] {
        bench.state(row, first).await;
    }

    let holder = bench.hold(&q).await;
    let db = bench.db.clone();
    let due: Vec<_> = [&r1, &r2, &q]
        .into_iter()
        .map(|row| LockBench::due(row, first))
        .collect();
    let stamp = tokio::spawn(async move {
        ActivationRepo
            .stamp_due(&db.conn().unwrap(), &due, time::OffsetDateTime::now_utc())
            .await
    });
    bench.blocked(1).await;

    let second = Uuid::from_u128(0xc2);
    let write = StateWrite {
        key: r1.key.clone(),
        change_ref: second,
        rows: [sub1, sub2]
            .into_iter()
            .map(|subscription_id| StateRow {
                subscription_id,
                tenant: ROOT.to_string(),
                expected_value: "\"w\"".to_owned(),
            })
            .collect(),
    };
    let db = bench.db.clone();
    let overwrite = tokio::spawn(async move {
        db.transaction(|tx| {
            Box::pin(async move { Ok(ActivationRepo.write_states(tx, &write).await) })
        })
        .await
        .expect("the change's transaction ends")
    });
    bench.blocked(2).await;

    holder.commit().await.expect("the holder commits");
    let stamped = finished(stamp).await;
    let written = finished(overwrite).await;
    assert_eq!(
        bench.pg.deadlocks().await,
        deadlocks,
        "no deadlock: stamp {stamped:?}, overwrite {written:?}"
    );
    assert_eq!(
        stamped.expect("the stamp commits"),
        3,
        "every due row stamped"
    );
    written.expect("the change's overwrite commits");
    for row in [&r1, &r2] {
        assert_eq!(
            bench.stamp_of(row).await,
            (second, 0),
            "the overwrite, after the stamp, leaves the new change unstamped"
        );
    }
    assert_eq!(
        bench.stamp_of(&q).await,
        (first, 1),
        "the other key stamped once"
    );
}

/// The canonical row-lock order of the stamp of a pair's due states.
#[tokio::test]
async fn the_stamp_locks_its_rows_in_the_canonical_order() {
    let Some(bench) = LockBench::new("lock-order-stamp").await else {
        return;
    };
    let deadlocks = bench.pg.deadlocks().await;
    let rows: Vec<_> = (1..=5)
        .map(|n| {
            ident(
                BENCH_SUBSCRIBER,
                n,
                if n % 2 == 0 { "lock.a" } else { "lock.b" },
            )
        })
        .collect();
    let change = Uuid::from_u128(0xc1);
    reversed(&bench, &rows, change, true).await;
    let due: Vec<_> = rows.iter().map(|row| LockBench::due(row, change)).collect();

    let holder = bench.hold(&rows[2]).await;
    let db = bench.db.clone();
    let task = tokio::spawn(async move {
        ActivationRepo
            .stamp_due(&db.conn().unwrap(), &due, time::OffsetDateTime::now_utc())
            .await
    });
    let stamped = locks_in_canonical_order(&bench, &rows, 2, holder, task).await;
    assert_eq!(stamped.expect("the stamp commits"), 5);
    assert_eq!(bench.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// The canonical row-lock order of a change's overwrite of its affected
/// subscriptions' states.
#[tokio::test]
async fn the_overwrite_locks_its_rows_in_the_canonical_order() {
    use cf_gears_settings_service::domain::activation::ActivationRepository as _;
    use cf_gears_settings_service::domain::activation::publisher::{StateRow, StateWrite};

    let Some(bench) = LockBench::new("lock-order-write").await else {
        return;
    };
    let deadlocks = bench.pg.deadlocks().await;
    let rows: Vec<_> = (1..=5)
        .map(|n| ident(BENCH_SUBSCRIBER, n, "lock.a"))
        .collect();
    let ids = reversed(&bench, &rows, Uuid::from_u128(0xc1), true).await;
    let second = Uuid::from_u128(0xc2);
    let write = StateWrite {
        key: "lock.a".to_owned(),
        change_ref: second,
        rows: ids
            .into_iter()
            .map(|subscription_id| StateRow {
                subscription_id,
                tenant: ROOT.to_string(),
                expected_value: "\"w\"".to_owned(),
            })
            .collect(),
    };

    let holder = bench.hold(&rows[2]).await;
    let db = bench.db.clone();
    let task = tokio::spawn(async move {
        db.transaction(|tx| {
            Box::pin(async move { Ok(ActivationRepo.write_states(tx, &write).await) })
        })
        .await
        .expect("the change's transaction ends")
    });
    locks_in_canonical_order(&bench, &rows, 2, holder, task)
        .await
        .expect("the overwrite commits");
    for row in &rows {
        assert_eq!(
            bench.stamp_of(row).await,
            (second, 0),
            "every state replaced"
        );
    }
    assert_eq!(bench.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// The canonical row-lock order of a purge's delete of its key's states.
#[tokio::test]
async fn the_purge_locks_its_keys_rows_in_the_canonical_order() {
    use cf_gears_settings_service::domain::activation::ActivationRepository as _;

    let Some(bench) = LockBench::new("lock-order-purge").await else {
        return;
    };
    let deadlocks = bench.pg.deadlocks().await;
    let rows: Vec<_> = (1..=5)
        .map(|n| ident(BENCH_SUBSCRIBER, n, "lock.a"))
        .collect();
    reversed(&bench, &rows, Uuid::from_u128(0xc1), true).await;

    let holder = bench.hold(&rows[2]).await;
    let db = bench.db.clone();
    let task = tokio::spawn(async move {
        db.transaction(|tx| {
            Box::pin(async move { Ok(ActivationRepo.delete_key(tx, "lock.a").await) })
        })
        .await
        .expect("the purge's transaction ends")
    });
    let deleted = locks_in_canonical_order(&bench, &rows, 2, holder, task)
        .await
        .expect("the purge commits");
    assert_eq!(deleted, (5, 5), "every subscription and state of the key");
    assert_eq!(bench.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// The canonical row-lock order of the sweep of orphaned states, across
/// keys whose order runs against the instances'.
#[tokio::test]
async fn the_sweep_locks_the_orphans_in_the_canonical_order() {
    let Some(bench) = LockBench::new("lock-order-sweep").await else {
        return;
    };
    let deadlocks = bench.pg.deadlocks().await;
    let keys = ["lock.e", "lock.d", "lock.c", "lock.b", "lock.a"];
    let rows: Vec<_> = (1..=5)
        .zip(keys)
        .map(|(n, key)| ident(BENCH_SUBSCRIBER, n, key))
        .collect();
    reversed(&bench, &rows, Uuid::from_u128(0xc1), false).await;

    let holder = bench.hold(&rows[2]).await;
    let db = bench.db.clone();
    let task = tokio::spawn(async move { ActivationRepo.sweep_orphans(&db.conn().unwrap()).await });
    let swept = locks_in_canonical_order(&bench, &rows, 2, holder, task)
        .await
        .expect("the sweep commits");
    assert_eq!(swept, 5, "every orphan swept");
    for row in &rows {
        assert!(!bench.exists(row).await);
    }
    assert_eq!(bench.pg.deadlocks().await, deadlocks, "no deadlock");
}
