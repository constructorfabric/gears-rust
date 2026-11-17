use super::*;

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use cf_system_sdks::directory::{LabelSelector, RegisterAck, ServiceEndpoint, ServiceInstanceInfo};

use crate::runtime::DependencyChecker;

/// Records every mutation and models directory membership so a heartbeat for an
/// unknown record returns `NOT_FOUND` exactly as the real directory does.
#[derive(Default)]
struct Calls {
    registers: Vec<(String, String, Option<ReportedState>)>,
    /// Full registration payloads, so a test can assert which halves (gRPC /
    /// REST) a re-registration carried.
    register_infos: Vec<RegisterInstanceInfo>,
    heartbeats: Vec<(String, String, Option<ReportedState>)>,
    deregisters: Vec<(String, String)>,
    known: HashSet<(String, String)>,
    lease_ttl_ms: u64,
}

struct MockDir {
    calls: Mutex<Calls>,
    /// When set, `heartbeat` signals `heartbeat_entered` and then waits on
    /// `heartbeat_release` before returning, so a test can act (untrack,
    /// re-register) while a heartbeat is deterministically in flight.
    hold_heartbeat: AtomicBool,
    heartbeat_entered: tokio::sync::Notify,
    heartbeat_release: tokio::sync::Notify,
    /// One-shot gate: hold the next registration before applying its payload.
    hold_register: AtomicBool,
    register_release: tokio::sync::Notify,
}

impl MockDir {
    fn new(lease_ttl_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Calls {
                lease_ttl_ms,
                ..Calls::default()
            }),
            hold_heartbeat: AtomicBool::new(false),
            heartbeat_entered: tokio::sync::Notify::new(),
            heartbeat_release: tokio::sync::Notify::new(),
            hold_register: AtomicBool::new(false),
            register_release: tokio::sync::Notify::new(),
        })
    }

    /// Hold the next registration before applying its payload until released.
    fn hold_registers(&self) {
        self.hold_register.store(true, Ordering::SeqCst);
    }

    /// Release the held registration; subsequent calls do not use the gate.
    fn release_registers(&self) {
        self.register_release.notify_one();
    }

    /// Forget a record so its next heartbeat returns `NOT_FOUND`, emulating a
    /// directory restart or an eviction.
    fn forget(&self, gear: &str, id: &str) {
        self.calls
            .lock()
            .unwrap()
            .known
            .remove(&(gear.to_owned(), id.to_owned()));
    }

    /// Make the next `heartbeat` block (after computing its result) until
    /// [`release_heartbeats`](Self::release_heartbeats).
    fn hold_heartbeats(&self) {
        self.hold_heartbeat.store(true, Ordering::SeqCst);
    }

    /// Wait until a held heartbeat has entered and is blocked.
    async fn wait_heartbeat_entered(&self) {
        self.heartbeat_entered.notified().await;
    }

    /// Release a held heartbeat and stop holding subsequent ones.
    fn release_heartbeats(&self) {
        self.hold_heartbeat.store(false, Ordering::SeqCst);
        self.heartbeat_release.notify_one();
    }
}

#[async_trait]
impl DirectoryClient for MockDir {
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
        Err(DirectoryNotFound::new(service_name).into())
    }
    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint> {
        Err(DirectoryNotFound::new(gear_name).into())
    }
    async fn get_openapi_spec(&self, gear_name: &str) -> Result<String> {
        Err(DirectoryNotFound::new(gear_name).into())
    }
    async fn list_instances(&self, _gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn resolve_by_labels(
        &self,
        _gear: &str,
        _selector: &LabelSelector,
    ) -> Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        self.register_with_lease(info).await.map(|_| ())
    }
    async fn register_with_lease(&self, info: RegisterInstanceInfo) -> Result<RegisterAck> {
        if self.hold_register.swap(false, Ordering::SeqCst) {
            self.register_release.notified().await;
        }
        let ttl = {
            let mut calls = self.calls.lock().unwrap();
            calls.registers.push((
                info.gear.clone(),
                info.instance_id.clone(),
                info.reported_state,
            ));
            calls.register_infos.push(info.clone());
            calls.known.insert((info.gear, info.instance_id));
            (calls.lease_ttl_ms > 0).then(|| Duration::from_millis(calls.lease_ttl_ms))
        };
        Ok(RegisterAck { lease_ttl: ttl })
    }
    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
        let mut calls = self.calls.lock().unwrap();
        calls
            .deregisters
            .push((gear.to_owned(), instance_id.to_owned()));
        calls
            .known
            .remove(&(gear.to_owned(), instance_id.to_owned()));
        Ok(())
    }
    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()> {
        self.heartbeat(gear, instance_id, None).await
    }
    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: &str,
        reported: Option<ReportedState>,
    ) -> Result<()> {
        // Compute the result under the lock, then release it before any await.
        let result = {
            let mut calls = self.calls.lock().unwrap();
            calls
                .heartbeats
                .push((gear.to_owned(), instance_id.to_owned(), reported));
            if calls
                .known
                .contains(&(gear.to_owned(), instance_id.to_owned()))
            {
                Ok(())
            } else {
                Err(DirectoryNotFound::new(format!("instance {gear}/{instance_id}")).into())
            }
        };
        if self.hold_heartbeat.load(Ordering::SeqCst) {
            self.heartbeat_entered.notify_one();
            self.heartbeat_release.notified().await;
        }
        result
    }
}

fn info(gear: &str, id: &str) -> RegisterInstanceInfo {
    RegisterInstanceInfo::new(gear, id).with_rest_endpoint(ServiceEndpoint::http(gear, 8080))
}

#[tokio::test]
async fn register_learns_lease_ttl_and_paces_renewal_at_a_third() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(5),
    );

    pm.register(info("billing", "i1")).await.unwrap();

    // 30s lease -> renew every 10s, not the 5s fallback.
    assert_eq!(pm.renewal_interval(), Duration::from_secs(10));
    // The registration carried the process reported state (ready -> Serving).
    let calls = dir.calls.lock().unwrap();
    assert_eq!(
        calls.registers,
        vec![(
            "billing".to_owned(),
            "i1".to_owned(),
            Some(ReportedState::Serving),
        )]
    );
}

#[tokio::test]
async fn renewal_interval_falls_back_without_a_lease_ttl() {
    let dir = MockDir::new(0); // directory returns no TTL
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir,
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(7),
    );
    pm.register(info("billing", "i1")).await.unwrap();
    assert_eq!(pm.renewal_interval(), Duration::from_secs(7));
}

#[tokio::test]
async fn renew_all_heartbeats_reported_state_from_dependency_checker() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    deps.register_dep("other"); // unresolved -> not ready
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps.clone()),
        Duration::from_secs(5),
    );
    pm.register(info("billing", "i1")).await.unwrap();

    // Not ready yet -> Starting.
    pm.renew_all().await;
    // Dependencies resolve -> Serving.
    deps.mark_resolved("other");
    pm.renew_all().await;
    // Drain -> Draining.
    deps.set_draining(true);
    pm.renew_all().await;

    let hb = &dir.calls.lock().unwrap().heartbeats;
    assert_eq!(hb[0].2, Some(ReportedState::Starting));
    assert_eq!(hb[1].2, Some(ReportedState::Serving));
    assert_eq!(hb[2].2, Some(ReportedState::Draining));
}

#[tokio::test]
async fn not_found_heartbeat_triggers_reregistration() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(5),
    );
    pm.register(info("billing", "i1")).await.unwrap();

    // The directory forgets the record (restart / eviction).
    dir.forget("billing", "i1");

    pm.renew_all().await;

    let calls = dir.calls.lock().unwrap();
    // One heartbeat (which got NOT_FOUND) and a re-registration.
    assert_eq!(calls.heartbeats.len(), 1);
    assert_eq!(
        calls.registers.len(),
        2,
        "the initial register plus the re-register after NOT_FOUND"
    );
    assert!(
        calls
            .known
            .contains(&("billing".to_owned(), "i1".to_owned()))
    );
}

#[tokio::test]
async fn register_merges_halves_so_recovery_keeps_both() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(5),
    );

    // grpc-hub registers the gRPC half (no REST endpoint).
    pm.register(
        RegisterInstanceInfo::new("billing", "i1").with_grpc_services(vec![(
            "billing.Svc".to_owned(),
            ServiceEndpoint::new("http://grpc"),
        )]),
    )
    .await
    .unwrap();

    // The REST advertiser later registers the REST half (no gRPC services).
    pm.register(
        RegisterInstanceInfo::new("billing", "i1")
            .with_rest_endpoint(ServiceEndpoint::http("billing", 8080)),
    )
    .await
    .unwrap();

    // The directory forgets the record; recovery must re-register BOTH halves,
    // not just the last registrar's.
    dir.forget("billing", "i1");
    pm.renew_all().await;

    let last = dir
        .calls
        .lock()
        .unwrap()
        .register_infos
        .last()
        .cloned()
        .expect("a re-registration must have happened");
    assert!(
        !last.grpc_services.is_empty(),
        "recovery kept the gRPC half registered by grpc-hub"
    );
    assert!(
        last.rest_endpoint.is_some(),
        "recovery kept the REST half registered by the advertiser"
    );
}

#[tokio::test]
async fn untrack_during_an_inflight_heartbeat_prevents_reregistration() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(5),
    );
    pm.register(info("billing", "i1")).await.unwrap();

    // The directory forgets the record, and the next heartbeat is held in
    // flight so the owner can untrack it mid-recovery (e.g. deregistering on
    // shutdown). The untrack must win: recovery must not resurrect a record
    // removed on purpose.
    dir.forget("billing", "i1");
    dir.hold_heartbeats();

    let pm2 = Arc::clone(&pm);
    let task = tokio::spawn(async move { pm2.renew_all().await });

    dir.wait_heartbeat_entered().await;
    pm.untrack("billing", "i1");
    dir.release_heartbeats();
    task.await.unwrap();

    let calls = dir.calls.lock().unwrap();
    assert_eq!(
        calls.registers.len(),
        1,
        "an untracked record must not be re-registered after a NOT_FOUND heartbeat"
    );
    assert!(
        !calls
            .known
            .contains(&("billing".to_owned(), "i1".to_owned())),
        "the record stays absent from the directory"
    );
}

#[tokio::test]
async fn recovery_reregisters_the_current_record_not_a_stale_snapshot() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(5),
    );

    // Initial registration carries only the REST half.
    pm.register(
        RegisterInstanceInfo::new("billing", "i1")
            .with_rest_endpoint(ServiceEndpoint::http("billing", 8080)),
    )
    .await
    .unwrap();

    // The directory forgets it; the next heartbeat (held in flight) will
    // NOT_FOUND and trigger recovery.
    dir.forget("billing", "i1");
    dir.hold_heartbeats();

    let pm2 = Arc::clone(&pm);
    let task = tokio::spawn(async move { pm2.renew_all().await });

    // While the heartbeat is blocked, a second registrar merges in the gRPC
    // half. Recovery must re-register this *current* merged record, not the
    // REST-only record the renewal pass started with.
    dir.wait_heartbeat_entered().await;
    pm.register(
        RegisterInstanceInfo::new("billing", "i1").with_grpc_services(vec![(
            "billing.Svc".to_owned(),
            ServiceEndpoint::new("http://grpc"),
        )]),
    )
    .await
    .unwrap();
    dir.release_heartbeats();
    task.await.unwrap();

    let last = dir
        .calls
        .lock()
        .unwrap()
        .register_infos
        .last()
        .cloned()
        .expect("recovery must have re-registered");
    assert!(
        last.rest_endpoint.is_some(),
        "recovery kept the REST half from the first registrar"
    );
    assert!(
        !last.grpc_services.is_empty(),
        "recovery carried the gRPC half merged in while the heartbeat was in flight"
    );
}

#[test]
fn an_inflight_recovery_does_not_clobber_a_newer_registration() {
    use std::future::Future;
    use std::pin::{Pin, pin};
    use std::task::{Context, Poll, Waker};

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps),
        Duration::from_secs(5),
    );

    // Initial registration advertises endpoint v1.
    let mut initial = pin!(
        pm.register(
            RegisterInstanceInfo::new("billing", "i1")
                .with_rest_endpoint(ServiceEndpoint::new("http://v1")),
        )
    );
    assert!(matches!(poll_once(initial.as_mut()), Poll::Ready(Ok(()))));

    // The directory forgets the record; hold recovery before it applies v1.
    dir.forget("billing", "i1");
    dir.hold_registers();
    let mut recover = pin!(pm.renew_all());
    assert!(poll_once(recover.as_mut()).is_pending());
    assert!(
        !dir.hold_register.load(Ordering::SeqCst),
        "recovery must reach and consume the one-shot registration gate"
    );
    assert_eq!(dir.calls.lock().unwrap().register_infos.len(), 1);

    // Poll v2 while recovery is blocked: it must wait without writing metadata.
    let mut update = pin!(
        pm.register(
            RegisterInstanceInfo::new("billing", "i1")
                .with_rest_endpoint(ServiceEndpoint::new("http://v2")),
        )
    );
    assert!(
        poll_once(update.as_mut()).is_pending(),
        "a newer registration must wait for the in-flight recovery write"
    );
    assert_eq!(
        dir.calls.lock().unwrap().register_infos.len(),
        1,
        "neither recovery nor the queued update may apply before release"
    );

    dir.release_registers();
    assert!(
        poll_once(recover.as_mut()).is_ready(),
        "recovery must finish once its registration is released"
    );
    assert!(
        matches!(poll_once(update.as_mut()), Poll::Ready(Ok(()))),
        "the queued registration must finish after recovery releases the write lock"
    );

    // The directory's final record is the newer v2, not the recovered v1.
    let endpoint_uri = |dir: &MockDir| {
        dir.calls
            .lock()
            .unwrap()
            .register_infos
            .last()
            .and_then(|i| i.rest_endpoint.as_ref().map(|e| e.uri.clone()))
    };
    assert_eq!(
        endpoint_uri(&dir).as_deref(),
        Some("http://v2"),
        "the newer registration must win at the directory, not the in-flight recovery"
    );

    // The cache agrees: a second recovery re-registers v2, so cache and
    // directory stay consistent (no drift back to v1).
    dir.forget("billing", "i1");
    let mut recover_again = pin!(pm.renew_all());
    assert!(poll_once(recover_again.as_mut()).is_ready());
    assert_eq!(
        endpoint_uri(&dir).as_deref(),
        Some("http://v2"),
        "recovery re-registers the current (v2) record"
    );
}

#[tokio::test]
async fn begin_drain_reports_draining_then_deregister_all_removes_records() {
    let dir = MockDir::new(30_000);
    let deps = Arc::new(DependencyChecker::new());
    let pm = PresenceManager::new(
        dir.clone(),
        ReportedStateSource::Dependencies(deps.clone()),
        Duration::from_secs(5),
    );
    pm.register(info("billing", "i1")).await.unwrap();

    // Drain sequence: flip readiness, push an immediate Draining heartbeat.
    deps.set_draining(true);
    pm.begin_drain();
    pm.renew_all().await; // stands in for the loop's wake-driven pass
    assert_eq!(
        dir.calls.lock().unwrap().heartbeats.last().unwrap().2,
        Some(ReportedState::Draining)
    );

    // End of drain: deregister and stop tracking.
    pm.deregister_all().await;
    assert_eq!(
        dir.calls.lock().unwrap().deregisters,
        vec![("billing".to_owned(), "i1".to_owned())]
    );
}
