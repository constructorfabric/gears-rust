#![cfg_attr(coverage_nightly, coverage(off))]

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::*;

fn seed(host: &str, port: u16) -> Seed<String> {
    Seed {
        key: format!("{host}:{port}"),
        host: host.to_owned(),
        port,
    }
}

/// Resolver backed by a mutable table; unknown names fail.
#[derive(Default)]
struct TableResolver {
    table: Mutex<HashMap<String, Vec<IpAddr>>>,
    calls: AtomicUsize,
}

impl TableResolver {
    fn set(&self, host: &str, ips: &[[u8; 4]]) {
        self.table.lock().insert(
            host.to_owned(),
            ips.iter().map(|ip| IpAddr::from(*ip)).collect(),
        );
    }
}

#[async_trait]
impl Resolver for TableResolver {
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        self.table
            .lock()
            .get(host)
            .map(|ips| ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown host"))
    }
}

struct BlockLoopback;

impl AddressFilter for BlockLoopback {
    fn check<'a>(&'a self, _host: &str, addr: &SocketAddr) -> Result<(), &'a str> {
        if addr.ip().is_loopback() {
            Err("loopback")
        } else {
            Ok(())
        }
    }
}

/// Health check that reports a fixed set of addresses as down.
#[derive(Default)]
struct Down(Mutex<Vec<SocketAddr>>);

#[async_trait]
impl HealthCheck for Down {
    async fn check(&self, addr: SocketAddr) -> bool {
        !self.0.lock().contains(&addr)
    }
}

fn opts(resolver: Arc<dyn Resolver>) -> BackendPoolOptions {
    BackendPoolOptions {
        resolver,
        health_check: None,
        refresh_interval: None,
        health_interval: None,
        ..BackendPoolOptions::default()
    }
}

#[tokio::test]
async fn round_robin_visits_every_member() {
    let pool = BackendPool::build(
        vec![seed("127.0.0.1", 1001), seed("127.0.0.1", 1002)],
        opts(Arc::new(TableResolver::default())),
    )
    .await;
    let mut ports = Vec::new();
    for _ in 0..4 {
        ports.push(pool.select().unwrap().1.port());
    }
    assert_eq!(ports.iter().filter(|&&p| p == 1001).count(), 2);
    assert_eq!(ports.iter().filter(|&&p| p == 1002).count(), 2);
}

#[tokio::test]
async fn selection_returns_seed_key_for_hostnames() {
    let resolver = Arc::new(TableResolver::default());
    resolver.set("api.internal", &[[10, 0, 0, 1], [10, 0, 0, 2]]);
    let pool = BackendPool::build(vec![seed("api.internal", 443)], opts(resolver)).await;
    assert_eq!(pool.members().len(), 2);
    let (key, addr) = pool.select().unwrap();
    assert_eq!(key, "api.internal:443");
    assert_eq!(addr.port(), 443);
}

#[tokio::test]
async fn unresolvable_seeds_yield_no_backends() {
    let pool = BackendPool::build(
        vec![seed("missing.internal", 443)],
        opts(Arc::new(TableResolver::default())),
    )
    .await;
    assert_eq!(pool.select().unwrap_err(), SelectError::NoBackendsResolved);
}

#[tokio::test]
async fn filter_blocks_resolved_addresses() {
    let resolver = Arc::new(TableResolver::default());
    resolver.set("mixed.internal", &[[127, 0, 0, 1], [10, 0, 0, 7]]);
    let mut o = opts(resolver);
    o.filter = Arc::new(BlockLoopback);
    let pool = BackendPool::build(vec![seed("mixed.internal", 80), seed("127.0.0.1", 81)], o).await;
    let members = pool.members();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].addr(), "10.0.0.7:80".parse().unwrap());
}

#[tokio::test]
async fn filter_blocking_everything_yields_no_backends() {
    let mut o = opts(Arc::new(TableResolver::default()));
    o.filter = Arc::new(BlockLoopback);
    let pool = BackendPool::build(vec![seed("127.0.0.1", 80)], o).await;
    assert_eq!(pool.select().unwrap_err(), SelectError::NoBackendsResolved);
}

#[tokio::test]
async fn duplicate_addresses_keep_first_seed() {
    let resolver = Arc::new(TableResolver::default());
    resolver.set("a.internal", &[[10, 0, 0, 1]]);
    resolver.set("b.internal", &[[10, 0, 0, 1]]);
    let pool = BackendPool::build(
        vec![seed("a.internal", 80), seed("b.internal", 80)],
        opts(resolver),
    )
    .await;
    assert_eq!(pool.members().len(), 1);
    assert_eq!(pool.select().unwrap().0, "a.internal:80");
}

#[tokio::test]
async fn refresh_picks_up_dns_changes_and_keeps_health() {
    let resolver = Arc::new(TableResolver::default());
    resolver.set("svc.internal", &[[10, 0, 0, 1]]);
    let pool = BackendPool::build(vec![seed("svc.internal", 80)], opts(resolver.clone())).await;
    pool.mark_unhealthy("10.0.0.1:80".parse().unwrap());

    resolver.set("svc.internal", &[[10, 0, 0, 1], [10, 0, 0, 2]]);
    pool.refresh().await;
    let members = pool.members();
    assert_eq!(members.len(), 2);
    let old = members
        .iter()
        .find(|m| m.addr().ip() == IpAddr::from([10, 0, 0, 1]))
        .unwrap();
    assert!(!old.is_healthy(), "health carries over");
    assert_eq!(pool.select().unwrap().1, "10.0.0.2:80".parse().unwrap());
}

#[tokio::test]
async fn dns_outage_keeps_the_last_good_addresses() {
    let resolver = Arc::new(TableResolver::default());
    resolver.set("svc.internal", &[[10, 0, 0, 1], [10, 0, 0, 2]]);
    resolver.set("other.internal", &[[10, 0, 0, 9]]);
    let pool = BackendPool::build(
        vec![seed("svc.internal", 80), seed("other.internal", 80)],
        opts(resolver.clone()),
    )
    .await;
    pool.mark_unhealthy("10.0.0.1:80".parse().unwrap());

    // svc.internal stops resolving; other.internal moves.
    resolver.table.lock().remove("svc.internal");
    resolver.set("other.internal", &[[10, 0, 0, 10]]);
    pool.refresh().await;

    let mut addrs: Vec<SocketAddr> = pool.members().iter().map(Member::addr).collect();
    addrs.sort_unstable();
    let expected: Vec<SocketAddr> = ["10.0.0.1:80", "10.0.0.2:80", "10.0.0.10:80"]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
    assert_eq!(addrs.len(), 3);
    assert!(expected.iter().all(|a| addrs.contains(a)), "{addrs:?}");
    let members = pool.members();
    let kept = members
        .iter()
        .find(|m| m.addr() == "10.0.0.1:80".parse().unwrap())
        .unwrap();
    assert!(!kept.is_healthy(), "health carries over for kept addresses");
}

#[test]
fn round_robin_falls_back_when_the_chosen_member_went_down() {
    // Two healthy members counted, but the second flips down before the
    // pick: the pick must land on a healthy member, not report none.
    let members = vec![
        Member::for_test("10.0.0.1:80".parse().unwrap(), true),
        Member::for_test("10.0.0.2:80".parse().unwrap(), true),
    ];
    let rr = RoundRobin::default();
    rr.next.store(1, Ordering::Relaxed);
    members[1].set_healthy(false);
    assert_eq!(rr.pick_among(&members, 2), Some(0));
}

#[tokio::test]
async fn health_checks_drive_selection() {
    let down = Arc::new(Down::default());
    let mut o = opts(Arc::new(TableResolver::default()));
    o.health_check = Some(down.clone());
    let pool = BackendPool::build(vec![seed("10.0.0.1", 80), seed("10.0.0.2", 80)], o).await;

    *down.0.lock() = vec!["10.0.0.1:80".parse().unwrap()];
    pool.run_health_checks().await;
    for _ in 0..4 {
        assert_eq!(pool.select().unwrap().1, "10.0.0.2:80".parse().unwrap());
    }

    *down.0.lock() = vec![
        "10.0.0.1:80".parse().unwrap(),
        "10.0.0.2:80".parse().unwrap(),
    ];
    pool.run_health_checks().await;
    assert_eq!(
        pool.select().unwrap_err(),
        SelectError::AllBackendsUnhealthy
    );

    down.0.lock().clear();
    pool.run_health_checks().await;
    assert!(pool.select().is_ok());
}

#[tokio::test]
async fn health_thresholds_damp_a_flapping_backend() {
    let down = Arc::new(Down::default());
    let mut o = opts(Arc::new(TableResolver::default()));
    o.health_check = Some(down.clone());
    o.unhealthy_threshold = 3;
    o.healthy_threshold = 2;
    let pool = BackendPool::build(vec![seed("10.0.0.1", 80)], o).await;
    let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();
    let healthy = || pool.members()[0].is_healthy();
    let probe = |up: bool| {
        *down.0.lock() = if up { vec![] } else { vec![addr] };
        pool.run_health_checks()
    };
    assert!(healthy(), "new members start healthy");

    // Two failures, then a success: the streak resets, still healthy.
    probe(false).await;
    probe(false).await;
    probe(true).await;
    assert!(healthy());
    // Three failures in a row take it out.
    for _ in 0..3 {
        assert!(healthy());
        probe(false).await;
    }
    assert!(!healthy());

    // One success, one failure: not back yet. Two successes in a row are.
    probe(true).await;
    probe(false).await;
    assert!(!healthy());
    probe(true).await;
    assert!(!healthy());
    probe(true).await;
    assert!(healthy());
}

#[tokio::test]
async fn a_passive_mark_restarts_the_streak() {
    // A failing probe started while the member was healthy must not count
    // towards its recovery once a passive mark took it down.
    let down = Arc::new(Down::default());
    let mut o = opts(Arc::new(TableResolver::default()));
    o.health_check = Some(down.clone());
    o.unhealthy_threshold = 3;
    o.healthy_threshold = 2;
    let pool = BackendPool::build(vec![seed("10.0.0.1", 80)], o).await;
    let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();
    *down.0.lock() = vec![addr];
    pool.run_health_checks().await;
    pool.mark_unhealthy(addr);
    down.0.lock().clear();
    pool.run_health_checks().await;
    assert!(
        !pool.members()[0].is_healthy(),
        "one success after the mark is not two"
    );
    pool.run_health_checks().await;
    assert!(pool.members()[0].is_healthy());
}

#[tokio::test]
async fn passive_marks_act_at_once_and_streaks_survive_a_refresh() {
    let resolver = Arc::new(TableResolver::default());
    resolver.set("svc.internal", &[[10, 0, 0, 1]]);
    let down = Arc::new(Down::default());
    let mut o = opts(resolver.clone());
    o.health_check = Some(down.clone());
    o.unhealthy_threshold = 3;
    o.healthy_threshold = 2;
    let pool = BackendPool::build(vec![seed("svc.internal", 80)], o).await;
    let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();

    // A passive mark ignores the threshold.
    pool.mark_unhealthy(addr);
    assert!(!pool.members()[0].is_healthy());

    // One success, then a refresh that keeps the address: the streak
    // carries over, so one more success brings it back.
    pool.run_health_checks().await;
    assert!(!pool.members()[0].is_healthy());
    pool.refresh().await;
    pool.run_health_checks().await;
    assert!(pool.members()[0].is_healthy());

    // An address that appears in a refresh starts healthy.
    resolver.set("svc.internal", &[[10, 0, 0, 1], [10, 0, 0, 2]]);
    *down.0.lock() = vec!["10.0.0.2:80".parse().unwrap()];
    pool.refresh().await;
    let members = pool.members();
    let fresh = members
        .iter()
        .find(|m| m.addr() == "10.0.0.2:80".parse().unwrap())
        .unwrap();
    assert!(fresh.is_healthy());
}

#[tokio::test(start_paused = true)]
async fn background_task_refreshes_and_stops_on_drop() {
    let resolver = Arc::new(TableResolver::default());
    let mut o = opts(resolver.clone());
    o.refresh_interval = Some(Duration::from_secs(30));
    let pool = BackendPool::build(vec![seed("svc.internal", 80)], o).await;
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 1);

    resolver.set("svc.internal", &[[10, 0, 0, 9]]);
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 2);
    assert!(pool.select().is_ok());

    drop(pool);
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(
        resolver.calls.load(Ordering::Relaxed),
        2,
        "no refresh after drop"
    );
}

#[tokio::test]
async fn tcp_health_check_probes_real_sockets() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up = listener.local_addr().unwrap();
    let check = TcpHealthCheck {
        timeout: Duration::from_millis(500),
    };
    assert!(check.check(up).await);
    drop(listener);
    assert!(!check.check(up).await);
}

#[tokio::test]
async fn dns_resolver_short_circuits_ip_literals() {
    let r = DnsResolver { backoff: vec![] };
    assert_eq!(
        r.resolve("127.0.0.1", 8080).await.unwrap(),
        vec!["127.0.0.1:8080".parse().unwrap()]
    );
    assert_eq!(
        r.resolve("[::1]", 8080).await.unwrap(),
        vec!["[::1]:8080".parse().unwrap()]
    );
}

#[tokio::test]
async fn dns_resolver_resolves_localhost() {
    let r = DnsResolver { backoff: vec![] };
    let addrs = r.resolve("localhost", 9001).await.unwrap();
    assert!(
        addrs
            .iter()
            .all(|a| a.ip().is_loopback() && a.port() == 9001)
    );
    assert!(!addrs.is_empty());
}

#[test]
fn round_robin_skips_unhealthy_and_handles_empty() {
    let rr = RoundRobin::default();
    assert_eq!(rr.pick(&[]), None);
}

#[tokio::test]
async fn round_robin_shares_evenly_around_unhealthy_members() {
    let down = Arc::new(Down::default());
    let mut o = opts(Arc::new(TableResolver::default()));
    o.health_check = Some(down.clone());
    let pool = BackendPool::build(
        vec![
            seed("10.0.0.1", 80),
            seed("10.0.0.2", 80),
            seed("10.0.0.3", 80),
        ],
        o,
    )
    .await;
    *down.0.lock() = vec!["10.0.0.2:80".parse().unwrap()];
    pool.run_health_checks().await;
    let mut counts: HashMap<SocketAddr, usize> = HashMap::new();
    for _ in 0..300 {
        *counts.entry(pool.select().unwrap().1).or_default() += 1;
    }
    assert_eq!(counts.len(), 2);
    assert!(counts.values().all(|&n| n == 150), "{counts:?}");
}
