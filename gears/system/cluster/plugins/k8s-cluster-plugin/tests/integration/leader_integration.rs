//! Layer 3 — leader-election integration scenarios (docs/TESTING.md §4.3),
//! asserting on the actual `Lease` objects (holder, renewTime, leaseDurationSeconds,
//! leaseTransitions, labels, name annotation).

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::large_futures,
    reason = "integration tests: a setup failure IS the test failure"
)]

use crate::common;

use std::time::Duration;

use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{ElectionConfig, LeaderStatus, LeaderWatch, LeaderWatchEvent};
use k8s_cluster_plugin::K8sLeaderElectionPlugin;
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use kube::ResourceExt;
use kube::api::{ObjectMeta, PostParams};
use serde_json::json;

const LABEL_MANAGED_BY: &str = "cluster.cf-gears.io/managed-by";
const LABEL_PRIMITIVE: &str = "cluster.cf-gears.io/primitive";
const ANNOTATION_NAME: &str = "cluster.cf-gears.io/name";

/// Polls a leader watch until it delivers a definitive `Status`, or panics on
/// timeout / a terminal `Closed`.
async fn first_status(watch: &mut LeaderWatch) -> LeaderStatus {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, watch.changed()).await else {
            panic!("no leader status within the deadline");
        };
        match event {
            LeaderWatchEvent::Status(status) => return status,
            LeaderWatchEvent::Closed(err) => panic!("watch closed before a status: {err:?}"),
            _ => {}
        }
    }
}

/// Waits until the watch reports the wanted status (via its snapshot), bounded.
async fn wait_for_status(watch: &LeaderWatch, want: LeaderStatus, timeout: Duration) -> bool {
    common::wait_until(timeout, Duration::from_millis(50), || async {
        watch.status() == want
    })
    .await
}

/// The single `Lease` whose name annotation is `election`, if present.
async fn lease_for(ns: &common::NamespaceGuard, election: &str) -> Option<Lease> {
    ns.list_leases()
        .await
        .into_iter()
        .find(|l| l.annotations().get(ANNOTATION_NAME).map(String::as_str) == Some(election))
}

/// `K8S-LEAD-001`: a single candidate becomes `Leader`; the Lease exists with our
/// holder, a 30s duration, non-null acquire/renew times, the labels, and the name
/// annotation carrying the original election name.
#[tokio::test]
async fn k8s_lead_001_elect_acquires_and_reports_leader() {
    let ns = common::fresh_namespace("lead-001").await;
    let handle = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("leader starts");
    let leader = handle.leader_election();

    let mut watch = leader.elect("svc").await.expect("elect");
    assert_eq!(first_status(&mut watch).await, LeaderStatus::Leader);

    let lease = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async { lease_for(&ns, "svc").await.is_some() },
    )
    .await;
    assert!(lease, "the Lease object exists");
    let lease = lease_for(&ns, "svc").await.expect("lease");
    let spec = lease.spec.as_ref().expect("spec");
    assert!(
        spec.holder_identity
            .as_deref()
            .is_some_and(|h| !h.is_empty()),
        "has a holder"
    );
    assert_eq!(
        spec.lease_duration_seconds,
        Some(30),
        "K8S-LEAD-001: 30s duration"
    );
    assert!(spec.acquire_time.is_some(), "acquireTime set");
    assert!(spec.renew_time.is_some(), "renewTime set");
    let labels = lease.labels();
    assert_eq!(
        labels.get(LABEL_MANAGED_BY).map(String::as_str),
        Some("cf-gears-cluster")
    );
    assert_eq!(
        labels.get(LABEL_PRIMITIVE).map(String::as_str),
        Some("election")
    );

    watch.resign().await.expect("resign");
    handle.stop().await;
}

/// `K8S-LEAD-002`: ten independent candidates on one name; exactly one `Leader`,
/// nine `Follower`, and exactly one Lease whose holder is the winner.
#[tokio::test]
async fn k8s_lead_002_ten_candidates_exactly_one_leader() {
    let ns = common::fresh_namespace("lead-002").await;
    let mut handles = Vec::new();
    let mut watches = Vec::new();
    for _ in 0..10 {
        let handle = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
            .with_client(ns.client.clone())
            .build_and_start()
            .await
            .expect("candidate starts");
        let watch = handle.leader_election().elect("svc").await.expect("elect");
        handles.push(handle);
        watches.push(watch);
    }

    let mut leaders = 0;
    let mut followers = 0;
    let mut lost = 0;
    for watch in &mut watches {
        match first_status(watch).await {
            LeaderStatus::Leader => leaders += 1,
            LeaderStatus::Follower => followers += 1,
            // A candidate that fails to settle stays in the transient `Lost` state;
            // counting it separately catches that rather than passing it off as a
            // follower.
            LeaderStatus::Lost => lost += 1,
        }
    }
    assert_eq!(leaders, 1, "K8S-LEAD-002: exactly one leader");
    assert_eq!(followers, 9, "K8S-LEAD-002: nine settled followers");
    assert_eq!(lost, 0, "K8S-LEAD-002: no candidate is stuck in Lost");

    let leases = ns.list_leases().await;
    assert_eq!(leases.len(), 1, "K8S-LEAD-002: exactly one Lease");

    for handle in handles {
        handle.stop().await;
    }
}

/// `K8S-LEAD-003`: renewal keeps leadership past the TTL — after several renewal
/// intervals the holder still leads and `renewTime` advanced.
#[tokio::test]
async fn k8s_lead_003_renewal_keeps_leadership() {
    let ns = common::fresh_namespace("lead-003").await;
    let handle = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("leader starts");
    let leader = handle.leader_election();

    // A 6s TTL (2s renewal) renews ~twice across the 4s window below — enough to
    // prove the renewal loop runs and advances `renewTime` — with a comfortable
    // margin against a missed renewal under CI load (a shorter TTL could lapse and
    // fail the "still leader" assertion spuriously).
    let mut watch = leader
        .elect_with_config(
            "svc",
            ElectionConfig::new(Duration::from_secs(6), 2).unwrap(),
        )
        .await
        .expect("elect");
    assert_eq!(first_status(&mut watch).await, LeaderStatus::Leader);

    let renew0 = lease_for(&ns, "svc")
        .await
        .expect("lease")
        .spec
        .unwrap()
        .renew_time;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        watch.status(),
        LeaderStatus::Leader,
        "K8S-LEAD-003: still leader after 3s"
    );
    let renew1 = lease_for(&ns, "svc")
        .await
        .expect("lease")
        .spec
        .unwrap()
        .renew_time;
    assert_ne!(renew0, renew1, "K8S-LEAD-003: renewTime advanced");

    watch.resign().await.expect("resign");
    handle.stop().await;
}

/// A `Lease` object with no holder — the shape a candidate claims via a guarded
/// *replace* (not a create), so pre-seeding one forces the 409-guarded-replace path.
fn free_lease(name: &str, namespace: &str) -> Lease {
    Lease {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(namespace.to_owned()),
            ..ObjectMeta::default()
        },
        spec: Some(LeaseSpec::default()), // holderIdentity unset → free
    }
}

/// `K8S-LEAD-004`: acquisition is `resourceVersion`-guarded — the 409 path. Two
/// candidates observe the *same free* Lease and both attempt the guarded replace:
/// exactly one wins, the other becomes a follower, and only guarded PUTs land on the
/// object. This is the test that fails if the guarded `replace` is ever turned into an
/// unconditional `patch` (DESIGN §2.6) — under an unconditional write both candidates
/// would report `Leader` and a `PATCH` (not a `PUT`) would land.
///
/// The `cluster_k8s_conflicts_total{primitive="election"}` counter the original spec
/// asserted on (docs/TESTING.md §4.3) is documented in DESIGN §9/§6.1 but never emitted
/// by the crate, so this asserts against server state and the request tally instead.
#[tokio::test]
async fn k8s_lead_004_acquisition_is_resource_version_guarded() {
    let ns = common::fresh_namespace("lead-004").await;
    let (client, counts) = common::counted_client().await;

    // Pre-create the Lease *free* under the name the plugin maps "svc" to. Pinning it
    // via `election_lease_names` avoids depending on the hash mapping and guarantees
    // both candidates take the guarded-replace branch rather than racing a create.
    // Seeded through the admin client, so it never touches the counted tally.
    let pinned = "pinned-004";
    ns.leases()
        .create(&PostParams::default(), &free_lease(pinned, &ns.namespace))
        .await
        .expect("seed a free lease");

    // Distinct, explicit identities so the winner can be identified on the object. A
    // 30s TTL keeps any renewal out of the measurement window: the only guarded writes
    // counted are the claim attempts themselves.
    let (id_a, id_b) = ("cand-a-004", "cand-b-004");
    let cfg = |identity: &str| {
        json!({
            "election_lease_names": { "svc": pinned },
            "min_election_ttl_ms": 500,
            "identity": identity,
        })
    };
    let ttl = ElectionConfig::new(Duration::from_secs(30), 2).unwrap();

    let a = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg(id_a)))
        .with_client(client.clone())
        .build_and_start()
        .await
        .expect("A starts");
    let b = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg(id_b)))
        .with_client(client.clone())
        .build_and_start()
        .await
        .expect("B starts");

    // Snapshot the tally *after* both preflights (each issues SSAR POSTs and a version
    // GET on this same client) but before the elects, so the deltas below are exactly
    // the election's own requests.
    let updates_before = counts.updates();
    let mutating_before = counts.mutating();
    let patches_before = counts.patches();

    let la = a.leader_election();
    let lb = b.leader_election();
    let (ra, rb) = tokio::join!(
        la.elect_with_config("svc", ttl),
        lb.elect_with_config("svc", ttl)
    );
    let mut a_watch = ra.expect("A elect");
    let mut b_watch = rb.expect("B elect");
    let (sa, sb) = tokio::join!(first_status(&mut a_watch), first_status(&mut b_watch));

    let leaders = [sa, sb]
        .iter()
        .filter(|s| **s == LeaderStatus::Leader)
        .count();
    let followers = [sa, sb]
        .iter()
        .filter(|s| **s == LeaderStatus::Follower)
        .count();
    assert_eq!(
        leaders, 1,
        "K8S-LEAD-004: exactly one candidate wins the guarded replace"
    );
    assert_eq!(
        followers, 1,
        "K8S-LEAD-004: the loser's 409 makes it a follower"
    );

    // Exactly one Lease, held by one of the two candidates — the guarded replace did not
    // fork the object (it claims the pinned pre-existing Lease in place) or leave it free.
    let leases = ns.list_leases().await;
    assert_eq!(
        leases.len(),
        1,
        "K8S-LEAD-004: still exactly one Lease object"
    );
    let holder = leases[0]
        .spec
        .as_ref()
        .and_then(|s| s.holder_identity.clone())
        .expect("the winning candidate holds the Lease");
    // The election path's holder is the bare candidate identity (§3.6) — the fenced
    // `<owner>#<fence>` token is the store-owned-lease *join* path, not `elect`.
    assert!(
        holder == id_a || holder == id_b,
        "K8S-LEAD-004: the holder is one of the two candidates, got `{holder}`"
    );

    // Every mutation the election made was a guarded PUT: no create (the object
    // pre-existed), no delete, and — the regression this test exists for — no PATCH.
    // An unconditional-`patch` conversion would show up as `patches > 0` (and, worse,
    // as two leaders above).
    let updates = counts.updates() - updates_before;
    let mutating = counts.mutating() - mutating_before;
    let patches = counts.patches() - patches_before;
    assert!(
        updates >= 1,
        "K8S-LEAD-004: at least the winning guarded replace landed"
    );
    assert_eq!(
        patches, 0,
        "K8S-LEAD-004: acquisition is a guarded PUT, never an unconditional patch"
    );
    assert_eq!(
        mutating, updates,
        "K8S-LEAD-004: every mutating request was a guarded PUT (no create/delete/patch)"
    );

    a.stop().await;
    b.stop().await;
}

/// `K8S-LEAD-005`: failover on holder death — the leader's plugin is `stop()`ed
/// without resigning; a follower takes over after ~one lease duration and
/// `leaseTransitions` increments. The Lease is still present with the dead holder's
/// identity immediately after the stop.
#[tokio::test]
async fn k8s_lead_005_failover_on_holder_death() {
    let ns = common::fresh_namespace("lead-005").await;
    let cfg = json!({ "min_election_ttl_ms": 500 });
    let ttl = ElectionConfig::new(Duration::from_secs(2), 2).unwrap();

    let a = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg.clone()))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("A starts");
    let mut a_watch = a
        .leader_election()
        .elect_with_config("svc", ttl)
        .await
        .expect("A elect");
    assert_eq!(first_status(&mut a_watch).await, LeaderStatus::Leader);
    let a_identity = lease_for(&ns, "svc")
        .await
        .unwrap()
        .spec
        .unwrap()
        .holder_identity
        .unwrap();

    let b = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("B starts");
    let b_watch = b
        .leader_election()
        .elect_with_config("svc", ttl)
        .await
        .expect("B elect");

    // A dies without resigning: the claim lapses, the object does not.
    a.stop().await;
    let after = lease_for(&ns, "svc")
        .await
        .expect("lease present after stop");
    assert_eq!(
        after.spec.unwrap().holder_identity.as_deref(),
        Some(a_identity.as_str()),
        "K8S-LEAD-005: the Lease persists with the dead holder's identity"
    );

    assert!(
        wait_for_status(&b_watch, LeaderStatus::Leader, Duration::from_secs(8)).await,
        "K8S-LEAD-005: a follower takes over after ~one lease duration"
    );
    let final_lease = lease_for(&ns, "svc").await.expect("lease");
    let spec = final_lease.spec.unwrap();
    assert_ne!(
        spec.holder_identity.as_deref(),
        Some(a_identity.as_str()),
        "new holder"
    );
    assert!(
        spec.lease_transitions.unwrap_or(0) >= 1,
        "K8S-LEAD-005: leaseTransitions incremented"
    );

    b.stop().await;
}

/// `K8S-LEAD-006`: `resign` hands over within a round-trip — the Lease's holder is
/// null right after, a follower acquires well inside one lease duration, and the
/// resigner observes `Lost`.
#[tokio::test]
async fn k8s_lead_006_resign_hands_over() {
    let ns = common::fresh_namespace("lead-006").await;
    let cfg = json!({ "min_election_ttl_ms": 500 });
    let ttl = ElectionConfig::new(Duration::from_secs(10), 2).unwrap();

    let a = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg.clone()))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("A starts");
    let a_watch = a
        .leader_election()
        .elect_with_config("svc", ttl)
        .await
        .expect("A elect");
    assert!(wait_for_status(&a_watch, LeaderStatus::Leader, Duration::from_secs(5)).await);

    let b = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("B starts");
    let b_watch = b
        .leader_election()
        .elect_with_config("svc", ttl)
        .await
        .expect("B elect");

    a_watch.resign().await.expect("resign hands over");

    // A follower acquires well inside the 10s TTL — far faster than a TTL-lapse.
    assert!(
        wait_for_status(&b_watch, LeaderStatus::Leader, Duration::from_secs(3)).await,
        "K8S-LEAD-006: a follower acquires within a round-trip of the resign"
    );

    a.stop().await;
    b.stop().await;
}

/// `K8S-LEAD-007`: a follower issues no writes — while a leader renews, a follower's
/// request counter shows zero mutating verbs and at least one watch.
#[tokio::test]
async fn k8s_lead_007_follower_issues_no_writes() {
    let ns = common::fresh_namespace("lead-007").await;
    // A 6s TTL keeps the leader stable under load: a shorter TTL could lapse, and a
    // follower that then acquired would issue the very writes this asserts it never
    // makes. The leader still renews (~every 2s) within the 3s observation window.
    let cfg = json!({});
    let ttl = ElectionConfig::new(Duration::from_secs(6), 2).unwrap();

    let leader = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg.clone()))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("leader starts");
    let l_watch = leader
        .leader_election()
        .elect_with_config("svc", ttl)
        .await
        .expect("elect");
    assert!(wait_for_status(&l_watch, LeaderStatus::Leader, Duration::from_secs(5)).await);

    let (client, counts) = common::counted_client().await;
    let follower = K8sLeaderElectionPlugin::builder(ns.leader_config_with(cfg))
        .with_client(client)
        .build_and_start()
        .await
        .expect("follower starts");
    let f_watch = follower
        .leader_election()
        .elect_with_config("svc", ttl)
        .await
        .expect("elect");
    assert!(wait_for_status(&f_watch, LeaderStatus::Follower, Duration::from_secs(5)).await);

    // Let the leader renew several times while the follower watches.
    let mutating_before = counts.mutating();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        counts.mutating() - mutating_before,
        0,
        "K8S-LEAD-007: a steady follower issues no mutating requests"
    );
    assert!(
        counts.watches() >= 1,
        "K8S-LEAD-007: the follower holds a watch"
    );

    leader.stop().await;
    follower.stop().await;
}

/// `K8S-LEAD-008`: transient loss re-enrols with no consumer code — the leader's
/// Lease is overwritten out-of-band; it observes `Lost`, then a subsequent
/// `Leader`/`Follower`, and never a terminal `Closed`.
#[tokio::test]
async fn k8s_lead_008_transient_loss_reenrolls() {
    let ns = common::fresh_namespace("lead-008").await;
    let handle = K8sLeaderElectionPlugin::builder(
        ns.leader_config_with(json!({ "min_election_ttl_ms": 500 })),
    )
    .with_client(ns.client.clone())
    .build_and_start()
    .await
    .expect("leader starts");
    let mut watch = handle
        .leader_election()
        .elect_with_config(
            "svc",
            ElectionConfig::new(Duration::from_secs(2), 2).unwrap(),
        )
        .await
        .expect("elect");
    assert_eq!(first_status(&mut watch).await, LeaderStatus::Leader);

    // Overwrite the Lease out-of-band: a third party takes the holder identity.
    let mut lease = lease_for(&ns, "svc").await.expect("lease");
    if let Some(spec) = lease.spec.as_mut() {
        spec.holder_identity = Some("intruder".to_owned());
    }
    ns.leases()
        .replace(&lease.name_any(), &PostParams::default(), &lease)
        .await
        .expect("out-of-band overwrite");

    // The leader observes Lost, then re-enrolls to a definitive status, never Closed.
    let mut saw_lost = false;
    let mut reenrolled = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !(saw_lost && reenrolled) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, watch.changed()).await {
            Ok(LeaderWatchEvent::Status(LeaderStatus::Lost)) => saw_lost = true,
            Ok(LeaderWatchEvent::Status(_)) if saw_lost => reenrolled = true,
            Ok(LeaderWatchEvent::Closed(err)) => {
                panic!("K8S-LEAD-008: loss must be transient, not Closed: {err:?}")
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(saw_lost, "K8S-LEAD-008: observes Lost after the overwrite");
    assert!(
        reenrolled,
        "K8S-LEAD-008: re-enrols to a definitive status with no consumer code"
    );

    handle.stop().await;
}

/// `K8S-LEAD-009`: a 409 on renewal loses leadership immediately — a third party
/// takes the Lease between renewals; the next renewal 409s and `Lost` is emitted on
/// that attempt.
#[tokio::test]
async fn k8s_lead_009_conflict_on_renewal_loses_immediately() {
    let ns = common::fresh_namespace("lead-009").await;
    // A long TTL so a natural TTL lapse cannot explain the Lost — only the 409 can.
    let handle = K8sLeaderElectionPlugin::builder(
        ns.leader_config_with(json!({ "min_election_ttl_ms": 500 })),
    )
    .with_client(ns.client.clone())
    .build_and_start()
    .await
    .expect("leader starts");
    let mut watch = handle
        .leader_election()
        .elect_with_config(
            "svc",
            ElectionConfig::new(Duration::from_secs(30), 5).unwrap(),
        )
        .await
        .expect("elect");
    assert_eq!(first_status(&mut watch).await, LeaderStatus::Leader);

    // A third party takes the Lease (a guarded replace with the current rv).
    let mut lease = lease_for(&ns, "svc").await.expect("lease");
    if let Some(spec) = lease.spec.as_mut() {
        spec.holder_identity = Some("intruder".to_owned());
    }
    ns.leases()
        .replace(&lease.name_any(), &PostParams::default(), &lease)
        .await
        .expect("steal the lease");

    // The next renewal 409s and Lost is emitted well inside the 30s TTL / 5 misses.
    let lost = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match watch.changed().await {
                LeaderWatchEvent::Status(LeaderStatus::Lost) => return true,
                LeaderWatchEvent::Closed(_) => return false,
                _ => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        lost,
        "K8S-LEAD-009: a 409 on renewal emits Lost immediately, not after N misses"
    );

    handle.stop().await;
}

/// `K8S-LEAD-010`: a sub-`min_election_ttl` config is rejected at the call, naming
/// the derived renewal rate; with the floor lowered, the same call succeeds.
#[tokio::test]
async fn k8s_lead_010_sub_floor_ttl_rejected() {
    let ns = common::fresh_namespace("lead-010").await;

    let strict = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("leader starts");
    let err = strict
        .leader_election()
        .elect_with_config(
            "svc",
            ElectionConfig::new(Duration::from_secs(1), 2).unwrap(),
        )
        .await
        .expect_err("a 1s TTL is below the 5s floor");
    match err {
        ClusterError::InvalidConfig { reason } => {
            assert!(
                reason.contains("min_election_ttl"),
                "names the floor: {reason}"
            );
        }
        other => panic!("K8S-LEAD-010: expected InvalidConfig, got {other:?}"),
    }
    strict.stop().await;

    let relaxed = K8sLeaderElectionPlugin::builder(
        ns.leader_config_with(json!({ "min_election_ttl_ms": 500 })),
    )
    .with_client(ns.client.clone())
    .build_and_start()
    .await
    .expect("leader starts");
    let mut watch = relaxed
        .leader_election()
        .elect_with_config(
            "svc",
            ElectionConfig::new(Duration::from_secs(1), 2).unwrap(),
        )
        .await
        .expect("K8S-LEAD-010: with the floor lowered, the 1s TTL is accepted");
    assert_eq!(first_status(&mut watch).await, LeaderStatus::Leader);
    relaxed.stop().await;
}

/// `K8S-LEAD-011`: `election_lease_names` pins a pre-existing Lease name — the plugin
/// contends on the literal configured name and creates nothing under the mapped one.
#[tokio::test]
async fn k8s_lead_011_election_lease_names_pins_object() {
    let ns = common::fresh_namespace("lead-011").await;
    let handle = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({
        "election_lease_names": { "svc": "my-pinned-lease" }
    })))
    .with_client(ns.client.clone())
    .build_and_start()
    .await
    .expect("leader starts");

    let mut watch = handle.leader_election().elect("svc").await.expect("elect");
    assert_eq!(first_status(&mut watch).await, LeaderStatus::Leader);

    let present = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            ns.leases()
                .get_opt("my-pinned-lease")
                .await
                .expect("get")
                .is_some()
        },
    )
    .await;
    assert!(
        present,
        "K8S-LEAD-011: the Lease lands on the literal configured name"
    );
    let leases = ns.list_leases().await;
    assert_eq!(
        leases.len(),
        1,
        "K8S-LEAD-011: nothing created under the mapped name"
    );
    assert_eq!(leases[0].name_any(), "my-pinned-lease");

    watch.resign().await.expect("resign");
    handle.stop().await;
}

/// The `<owner>#<fence>` holder of a Lease, if present.
fn token_holder(lease: &Lease) -> Option<String> {
    lease.spec.as_ref().and_then(|s| s.holder_identity.clone())
}

/// `K8S-LEAD-012` (mirrors redis's leader token path): the store-owned-leases half
/// of leader election served statelessly. A sole candidate `join`s and wins, getting
/// a `LeaseToken` whose Lease holder is `<owner>#<fence>`; a second candidate joining
/// against the live incumbent follows (`None`, not an error); the winner `renew`s
/// against the token; `resign` clears the holder and the successor's join wins the
/// now-free lease; the resigned token is fenced out of both `renew` and `resign`.
#[tokio::test]
async fn k8s_lead_012_join_renew_resign_token_path() {
    let ns = common::fresh_namespace("lead-012").await;
    let handle = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("leader starts");
    let leader = handle.leader_election();
    let ttl = ElectionConfig::default().ttl();

    // A sole candidate joins and wins.
    let token = leader
        .join("svc", "cand-a", ElectionConfig::default())
        .await
        .expect("join dispatches")
        .expect("a sole candidate takes the claim");
    assert_eq!(token.name, "svc");
    assert_eq!(token.owner, "cand-a");
    let want = format!("cand-a#{}", token.fence);
    let lease = lease_for(&ns, "svc").await.expect("lease exists");
    assert_eq!(
        token_holder(&lease).as_deref(),
        Some(want.as_str()),
        "K8S-LEAD-012: the Lease holder is <owner>#<fence>"
    );

    // A second candidate against the live incumbent follows, not an error.
    let follower = leader
        .join("svc", "cand-b", ElectionConfig::default())
        .await
        .expect("join dispatches");
    assert!(
        follower.is_none(),
        "K8S-LEAD-012: a live incumbent yields a follower (None), got {follower:?}"
    );

    // The winner renews against its token.
    leader
        .renew(&token, ttl)
        .await
        .expect("K8S-LEAD-012: renew holds leadership");

    // Resign clears the holder; the successor then wins the now-free lease.
    leader.resign(&token).await.expect("K8S-LEAD-012: resign");
    let after = lease_for(&ns, "svc")
        .await
        .expect("K8S-LEAD-012: the object persists after resign");
    assert!(
        token_holder(&after).is_none(),
        "K8S-LEAD-012: resign clears the holder, not the object"
    );
    let b = leader
        .join("svc", "cand-b", ElectionConfig::default())
        .await
        .expect("join dispatches")
        .expect("K8S-LEAD-012: cand-b wins the freed election");
    assert_eq!(b.owner, "cand-b");

    // The resigned token is fenced out of both renew and resign.
    let stale_renew = leader.renew(&token, ttl).await;
    assert!(
        matches!(stale_renew, Err(ClusterError::LockExpired { .. })),
        "K8S-LEAD-012: a fenced-out token cannot renew, got {stale_renew:?}"
    );
    leader
        .resign(&token)
        .await
        .expect("K8S-LEAD-012: a stale resign is idempotent by absence");

    handle.stop().await;
}

/// `K8S-LEAD-013`: pod-agnostic leader token portability (invariant I7). A candidate
/// `join`s through one backend instance and the resulting `LeaseToken` is `renew`ed
/// **and** `resign`ed through a second, independent instance that never ran the
/// election — the store-owned-leases half a gear serving `Join`/`Renew`/`Resign` RPCs
/// across replicas depends on. The token's `owner`+`fence` alone reconstruct the
/// stored `holderIdentity` the second instance fences on.
#[tokio::test]
async fn k8s_lead_013_a_token_renews_and_resigns_from_another_instance() {
    let ns = common::fresh_namespace("lead-013").await;
    let handle_a = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("instance A starts");
    let handle_b = K8sLeaderElectionPlugin::builder(ns.leader_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("instance B starts");
    let leader_a = handle_a.leader_election();
    let leader_b = handle_b.leader_election();
    let ttl = ElectionConfig::default().ttl();

    // A wins the election via join; B never saw this call.
    let token = leader_a
        .join("svc", "cand-a", ElectionConfig::default())
        .await
        .expect("join dispatches")
        .expect("A takes the claim");

    // B renews A's claim from the token alone (I7).
    leader_b.renew(&token, ttl).await.expect(
        "K8S-LEAD-013: B renews A's leadership via the token, never having run the election",
    );
    let want = format!("cand-a#{}", token.fence);
    assert_eq!(
        lease_for(&ns, "svc")
            .await
            .and_then(|l| token_holder(&l))
            .as_deref(),
        Some(want.as_str()),
        "K8S-LEAD-013: the claim is still held under the same holder after B's renew"
    );
    // A second candidate joining through either instance still follows.
    assert!(
        leader_b
            .join("svc", "cand-b", ElectionConfig::default())
            .await
            .expect("join dispatches")
            .is_none(),
        "K8S-LEAD-013: the live claim yields a follower from any instance"
    );

    // B resigns A's claim from the token; the election is then free.
    leader_b
        .resign(&token)
        .await
        .expect("K8S-LEAD-013: B resigns A's claim via the token");
    assert!(
        lease_for(&ns, "svc")
            .await
            .and_then(|l| token_holder(&l))
            .is_none(),
        "K8S-LEAD-013: B's cross-instance resign cleared the holder cluster-wide"
    );

    handle_a.stop().await;
    handle_b.stop().await;
}
