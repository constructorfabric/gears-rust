use std::sync::Mutex;
use std::time::Duration;

use toolkit_resilience::circuit_breaker::{Lifecycle, Outcome, State, StateChange};
use uuid::Uuid;

use super::*;
use crate::domain::model::{Endpoint, ManagedBy, Scheme, Server, SharingMode, Upstream};

const INSTANCE: &str = "/oagw/v1/proxy/api.example.com/v1";

fn cb(
    failure_threshold: u32,
    timeout_seconds: u32,
    success_threshold: u32,
) -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        sharing: SharingMode::Private,
        enabled: true,
        failure_threshold,
        timeout_seconds,
        success_threshold,
        owner_id: None,
    }
}

fn upstream(id: Uuid, circuit_breaker: Option<CircuitBreakerConfig>) -> Upstream {
    Upstream {
        id,
        tenant_id: Uuid::new_v4(),
        alias: "api.example.com".into(),
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.example.com".into(),
                port: 443,
            }],
        },
        protocol: "http".into(),
        enabled: true,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        circuit_breaker,
        cors: None,
        tags: vec![],
        managed_by: ManagedBy::Api,
    }
}

fn fail(breakers: &CircuitBreakers, u: &Upstream, times: u32) {
    for _ in 0..times {
        breakers
            .admit(u, INSTANCE)
            .unwrap()
            .answered(http::StatusCode::SERVICE_UNAVAILABLE);
    }
}

fn retry_after(err: DomainError) -> Duration {
    match err {
        DomainError::CircuitBreakerOpen {
            retry_after,
            instance,
            ..
        } => {
            assert_eq!(instance, INSTANCE);
            retry_after
        }
        other => panic!("expected CircuitBreakerOpen, got {other:?}"),
    }
}

#[test]
fn config_maps_to_consecutive_failures_open_for_and_probes() {
    let config = breaker_config(&cb(4, 7, 2));
    assert_eq!(config.consecutive_failures(), Some(4));
    assert_eq!(config.open_for(), Duration::from_secs(7));
    assert_eq!(config.half_open_calls(), 2);
    assert!(config.enabled());
    assert_eq!(config.validate(), Ok(()));
}

#[tokio::test]
async fn no_breaker_without_an_enabled_config() {
    let breakers = CircuitBreakers::new();
    let absent = upstream(Uuid::new_v4(), None);
    let disabled = upstream(
        Uuid::new_v4(),
        Some(CircuitBreakerConfig {
            enabled: false,
            ..cb(1, 1, 1)
        }),
    );
    for u in [&absent, &disabled] {
        fail(&breakers, u, 5);
        assert!(breakers.admit(u, INSTANCE).is_ok());
    }
    assert_eq!(breakers.len(), 0);
}

#[tokio::test(start_paused = true)]
async fn opens_after_consecutive_failures_then_closes_on_probes() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(3, 10, 2)));

    // A success in between resets the count.
    fail(&breakers, &u, 2);
    breakers
        .admit(&u, INSTANCE)
        .unwrap()
        .answered(http::StatusCode::OK);
    fail(&breakers, &u, 2);
    assert!(breakers.admit(&u, INSTANCE).is_ok());

    fail(&breakers, &u, 1);
    let err = breakers.admit(&u, INSTANCE).unwrap_err();
    assert_eq!(retry_after(err), Duration::from_secs(10));

    tokio::time::advance(Duration::from_secs(4)).await;
    let err = breakers.admit(&u, INSTANCE).unwrap_err();
    assert_eq!(retry_after(err), Duration::from_secs(6));

    // Half-open: `success_threshold` probes at once, then busy.
    tokio::time::advance(Duration::from_secs(6)).await;
    let first = breakers.admit(&u, INSTANCE).unwrap();
    let second = breakers.admit(&u, INSTANCE).unwrap();
    assert!(breakers.admit(&u, INSTANCE).is_err());
    first.answered(http::StatusCode::OK);
    second.answered(http::StatusCode::NOT_FOUND);

    // Closed again: a failure short of the threshold does not open it.
    fail(&breakers, &u, 2);
    assert!(breakers.admit(&u, INSTANCE).is_ok());
}

#[tokio::test(start_paused = true)]
async fn a_failed_probe_opens_the_circuit_again() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 5, 1)));
    fail(&breakers, &u, 1);
    tokio::time::advance(Duration::from_secs(5)).await;
    breakers
        .admit(&u, INSTANCE)
        .unwrap()
        .failed(&DomainError::DownstreamError {
            detail: "refused".into(),
            instance: INSTANCE.into(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        });
    let err = breakers.admit(&u, INSTANCE).unwrap_err();
    assert_eq!(retry_after(err), Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn a_dropped_permit_records_nothing_and_frees_its_probe_slot() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 1, 1)));
    fail(&breakers, &u, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    // The only probe slot: dropped, it is free for the next request, whose
    // success closes the circuit.
    drop(breakers.admit(&u, INSTANCE).unwrap());
    breakers
        .admit(&u, INSTANCE)
        .unwrap()
        .answered(http::StatusCode::OK);
    let a = breakers.admit(&u, INSTANCE).unwrap();
    let b = breakers.admit(&u, INSTANCE).unwrap();
    drop((a, b));
}

/// Closed, a dropped permit neither resets the count (as a success would)
/// nor adds to it (as a failure would).
#[tokio::test]
async fn a_dropped_permit_leaves_the_failure_count_alone() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(2, 60, 1)));
    fail(&breakers, &u, 1);
    drop(breakers.admit(&u, INSTANCE).unwrap());
    assert!(
        breakers.admit(&u, INSTANCE).is_ok(),
        "not opened by the drop"
    );
    fail(&breakers, &u, 1);
    assert!(
        breakers.admit(&u, INSTANCE).is_err(),
        "not reset by the drop"
    );
}

/// `check` rejects while open and keeps no probe slot once the timeout has
/// passed: the request's own permit later takes the only one.
#[tokio::test(start_paused = true)]
async fn check_fails_fast_without_taking_a_probe_slot() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 5, 1)));
    fail(&breakers, &u, 1);
    let err = breakers.check(&u, INSTANCE).unwrap_err();
    assert_eq!(retry_after(err), Duration::from_secs(5));

    tokio::time::advance(Duration::from_secs(5)).await;
    breakers.check(&u, INSTANCE).unwrap();
    breakers.check(&u, INSTANCE).unwrap();
    let probe = breakers.admit(&u, INSTANCE).unwrap();
    assert!(
        breakers.check(&u, INSTANCE).is_err(),
        "the probe is running"
    );
    probe.answered(http::StatusCode::OK);
    breakers.check(&u, INSTANCE).unwrap();
    assert!(breakers.admit(&u, INSTANCE).is_ok(), "closed");
}

/// At the open → half-open edge the check rejects nothing until a probe
/// holds the slot: requests that pass it before then are refused only by
/// their own `admit`, after the gateway's steps in between.
#[tokio::test(start_paused = true)]
async fn check_passes_at_the_half_open_edge_until_a_probe_holds_the_slot() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 5, 1)));
    fail(&breakers, &u, 1);
    tokio::time::advance(Duration::from_secs(5)).await;

    for _ in 0..3 {
        breakers.check(&u, INSTANCE).unwrap();
    }
    let probe = breakers.admit(&u, INSTANCE).unwrap();
    assert!(breakers.check(&u, INSTANCE).is_err(), "the slot is held");
    assert!(breakers.admit(&u, INSTANCE).is_err(), "the slot is held");

    drop(probe);
    breakers.check(&u, INSTANCE).unwrap();
    assert!(
        breakers.admit(&u, INSTANCE).is_ok(),
        "the slot is free again"
    );
}

/// A deadline hit counts only when target resolution and the exchange
/// started with all but a tenth of the request timeout left.
#[tokio::test]
async fn a_deadline_hit_counts_only_with_the_full_budget() {
    let timeout = Duration::from_secs(30);
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    let permit = |left| {
        breakers
            .admit(&u, INSTANCE)
            .unwrap()
            .with_budget(left, timeout)
    };

    // The CredStore read took 4 s of 30: the upstream had 26 s.
    permit(Duration::from_secs(26)).timed_out();
    permit(Duration::from_millis(26_999)).timed_out();
    assert!(breakers.admit(&u, INSTANCE).is_ok(), "not counted");

    permit(Duration::from_secs(27)).timed_out();
    assert!(breakers.admit(&u, INSTANCE).is_err(), "counted");

    let u = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    breakers.admit(&u, INSTANCE).unwrap().timed_out();
    assert!(
        breakers.admit(&u, INSTANCE).is_err(),
        "without a budget, the full one"
    );
}

/// An SSRF refusal is recorded as nothing.
#[tokio::test]
async fn an_ignored_request_records_nothing() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    breakers.admit(&u, INSTANCE).unwrap().ignored();
    assert!(breakers.admit(&u, INSTANCE).is_ok());
}

/// A request pinned to one endpoint of several records nothing, whatever
/// it ends with; unpinned, the same failure opens the circuit.
#[tokio::test]
async fn a_pinned_request_records_nothing() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    let pinned = || {
        breakers
            .admit(&u, INSTANCE)
            .unwrap()
            .pinned_to_one_endpoint(true)
    };
    pinned().answered(http::StatusCode::SERVICE_UNAVAILABLE);
    pinned().timed_out();
    pinned().failed(&DomainError::LinkUnavailable {
        detail: "x".into(),
        instance: INSTANCE.into(),
        dispatch: crate::domain::error::Dispatch::NotSent,
    });
    assert!(breakers.admit(&u, INSTANCE).is_ok(), "not counted");

    breakers
        .admit(&u, INSTANCE)
        .unwrap()
        .pinned_to_one_endpoint(false)
        .answered(http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(breakers.admit(&u, INSTANCE).is_err(), "counted");
}

/// A stored config the toolkit rejects (written around validation) fails
/// open: no breaker, every request through.
#[tokio::test]
async fn an_invalid_stored_config_fails_open() {
    let breakers = CircuitBreakers::new();
    for config in [cb(0, 60, 1), cb(1, 60, 0)] {
        let u = upstream(Uuid::new_v4(), Some(config));
        for _ in 0..3 {
            breakers
                .admit(&u, INSTANCE)
                .unwrap()
                .answered(http::StatusCode::SERVICE_UNAVAILABLE);
        }
        breakers.check(&u, INSTANCE).unwrap();
    }
    assert_eq!(breakers.len(), 0);
}

#[tokio::test]
async fn a_changed_config_rebuilds_the_breaker_closed() {
    let breakers = CircuitBreakers::new();
    let id = Uuid::new_v4();
    let u = upstream(id, Some(cb(1, 60, 1)));
    fail(&breakers, &u, 1);
    assert!(breakers.admit(&u, INSTANCE).is_err());

    // As another replica's write arrives: a new config, a closed breaker.
    let changed = upstream(id, Some(cb(2, 60, 1)));
    assert!(breakers.admit(&changed, INSTANCE).is_ok());
    assert_eq!(breakers.len(), 1);

    // The same config keeps the breaker and its counts.
    fail(&breakers, &changed, 1);
    assert!(breakers.admit(&changed, INSTANCE).is_ok());
    fail(&breakers, &changed, 1);
    assert!(breakers.admit(&changed, INSTANCE).is_err());

    // So does a renamed upstream (its metrics name follows the alias).
    let renamed = Upstream {
        alias: "renamed".into(),
        ..changed
    };
    assert!(breakers.admit(&renamed, INSTANCE).is_ok());
}

/// New endpoints rebuild the breaker on the next request, on any replica:
/// it counted failures of other hosts.
#[tokio::test]
async fn changed_endpoints_rebuild_the_breaker_closed() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    fail(&breakers, &u, 1);
    assert!(breakers.admit(&u, INSTANCE).is_err());

    let mut moved = u.clone();
    moved.server.endpoints[0].host = "api2.example.com".into();
    assert!(breakers.admit(&moved, INSTANCE).is_ok());
    assert_eq!(breakers.len(), 1);
}

/// Disabling a breaker removes it, so no metric reports it open any more.
#[tokio::test]
async fn a_disabled_breaker_is_removed() {
    let breakers = CircuitBreakers::new();
    let u = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    fail(&breakers, &u, 1);
    assert_eq!(breakers.len(), 1);
    let mut disabled = u.clone();
    if let Some(cb) = disabled.circuit_breaker.as_mut() {
        cb.enabled = false;
    }
    assert!(breakers.admit(&disabled, INSTANCE).is_ok());
    assert_eq!(breakers.len(), 0);
    assert!(
        breakers.admit(&u, INSTANCE).is_ok(),
        "enabled again: closed"
    );
}

/// Idle breakers go once `timeout_seconds` plus the grace has passed since
/// their last request, unless a request still holds them.
#[tokio::test(start_paused = true)]
async fn evict_idle_removes_unused_breakers() {
    let breakers = CircuitBreakers::new();
    let idle = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    let held = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    let used = upstream(Uuid::new_v4(), Some(cb(1, 60, 1)));
    fail(&breakers, &idle, 1);
    let permit = breakers.admit(&held, INSTANCE).unwrap();
    breakers
        .admit(&used, INSTANCE)
        .unwrap()
        .answered(http::StatusCode::OK);

    tokio::time::advance(Duration::from_secs(60) + IDLE_GRACE - Duration::from_secs(1)).await;
    assert_eq!(breakers.evict_idle(), 0);
    breakers
        .admit(&used, INSTANCE)
        .unwrap()
        .answered(http::StatusCode::OK);

    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(breakers.evict_idle(), 1, "only the idle one");
    assert!(breakers.admit(&idle, INSTANCE).is_ok(), "rebuilt closed");
    permit.answered(http::StatusCode::OK);
    assert_eq!(breakers.len(), 3);
}

/// What the metrics observer sees of one breaker.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn push(&self, event: String) {
        self.0.lock().unwrap().push(event);
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

fn name_of(name: Option<&Arc<str>>) -> &str {
    name.map_or("", |n| &**n)
}

impl CircuitBreakerObserver for Recorder {
    fn on_attach(&self, e: &Lifecycle<'_>) {
        self.push(format!("attach {}", name_of(e.name)));
    }

    fn on_detach(&self, e: &Lifecycle<'_>) {
        self.push(format!("detach {}", name_of(e.name)));
    }

    fn on_state_change(&self, e: &StateChange<'_>) {
        let state = |s| match s {
            State::Closed => "closed",
            State::Open => "open",
            State::HalfOpen => "half-open",
        };
        self.push(format!(
            "{} {} -> {}",
            name_of(e.name),
            state(e.from),
            state(e.to)
        ));
    }
}

/// Every breaker, the rebuilt ones included, reports to the observer under
/// `{alias}/{upstream}`, with the config owner when it is another upstream.
/// A rebuilt breaker attaches before the one it replaces detaches.
#[tokio::test]
async fn the_observer_sees_every_breaker_under_its_name() {
    let recorder = Arc::new(Recorder::default());
    let breakers = CircuitBreakers::new().with_observer(recorder.clone());
    let id = Uuid::new_v4();
    let u = upstream(id, Some(cb(1, 60, 1)));
    let name = format!("api.example.com/{id}");

    fail(&breakers, &u, 1);
    assert_eq!(
        recorder.take(),
        [format!("attach {name}"), format!("{name} closed -> open")]
    );

    let changed = upstream(id, Some(cb(2, 60, 1)));
    fail(&breakers, &changed, 2);
    assert_eq!(
        recorder.take(),
        [
            format!("attach {name}"),
            format!("detach {name}"),
            format!("{name} closed -> open")
        ]
    );

    let renamed = Upstream {
        alias: "renamed".into(),
        ..changed
    };
    fail(&breakers, &renamed, 2);
    let new_name = format!("renamed/{id}");
    assert_eq!(
        recorder.take(),
        [
            format!("attach {new_name}"),
            format!("detach {name}"),
            format!("{new_name} closed -> open")
        ]
    );

    let owner = Uuid::new_v4();
    let inherited = upstream(
        id,
        Some(CircuitBreakerConfig {
            owner_id: Some(owner),
            ..cb(1, 60, 1)
        }),
    );
    fail(&breakers, &inherited, 1);
    let owned_name = format!("api.example.com/{id}/{owner}");
    assert_eq!(
        recorder.take(),
        [
            format!("attach {owned_name}"),
            format!("{owned_name} closed -> open")
        ]
    );
}

#[tokio::test]
async fn each_config_owner_gets_its_own_breaker() {
    let breakers = CircuitBreakers::new();
    let id = Uuid::new_v4();
    let ancestor = Uuid::new_v4();
    let own = upstream(
        id,
        Some(CircuitBreakerConfig {
            owner_id: Some(id),
            ..cb(1, 60, 1)
        }),
    );
    let inherited = upstream(
        id,
        Some(CircuitBreakerConfig {
            owner_id: Some(ancestor),
            ..cb(2, 60, 1)
        }),
    );
    fail(&breakers, &own, 1);
    assert!(breakers.admit(&own, INSTANCE).is_err());
    // Alternating between the two does not rebuild either.
    assert!(breakers.admit(&inherited, INSTANCE).is_ok());
    assert!(breakers.admit(&own, INSTANCE).is_err());
    assert_eq!(breakers.len(), 2);
}

#[tokio::test]
async fn remove_for_upstream_drops_its_breakers_and_those_it_configures() {
    let breakers = CircuitBreakers::new();
    let (a, b, owner) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let inherited = |id| {
        upstream(
            id,
            Some(CircuitBreakerConfig {
                owner_id: Some(owner),
                ..cb(1, 60, 1)
            }),
        )
    };
    let own_b = upstream(b, Some(cb(1, 60, 1)));
    for u in [&inherited(a), &inherited(b), &own_b] {
        fail(&breakers, u, 1);
    }
    assert_eq!(breakers.len(), 3);

    breakers.remove_for_upstream(owner);
    assert_eq!(breakers.len(), 1);
    assert!(breakers.admit(&inherited(a), INSTANCE).is_ok());
    assert!(breakers.admit(&own_b, INSTANCE).is_err());

    breakers.remove_for_upstream(b);
    assert!(breakers.admit(&own_b, INSTANCE).is_ok());
}

#[test]
fn statuses_502_to_504_are_failures() {
    for (status, outcome) in [
        (200, Outcome::Success),
        (101, Outcome::Success),
        (404, Outcome::Success),
        (429, Outcome::Success),
        (500, Outcome::Success),
        (501, Outcome::Success),
        (502, Outcome::Failure),
        (503, Outcome::Failure),
        (504, Outcome::Failure),
        (505, Outcome::Success),
    ] {
        assert_eq!(
            outcome_of_status(http::StatusCode::from_u16(status).unwrap()),
            outcome,
            "{status}"
        );
    }
}

#[test]
fn transport_errors_are_failures_and_gateway_errors_say_nothing() {
    let d = || "x".to_owned();
    let i = || INSTANCE.to_owned();
    let failures = [
        DomainError::ConnectionTimeout {
            detail: d(),
            instance: i(),
            dispatch: crate::domain::error::Dispatch::NotSent,
        },
        DomainError::RequestTimeout {
            detail: d(),
            instance: i(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        },
        DomainError::LinkUnavailable {
            detail: d(),
            instance: i(),
            dispatch: crate::domain::error::Dispatch::NotSent,
        },
        DomainError::DownstreamError {
            detail: d(),
            instance: i(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        },
        DomainError::ProtocolError {
            detail: d(),
            instance: i(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        },
        DomainError::StreamAborted {
            detail: d(),
            instance: i(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        },
    ];
    for err in &failures {
        assert_eq!(outcome_of_error(err), Outcome::Failure, "{err:?}");
    }
    let ignored = [
        DomainError::validation("bad request"),
        DomainError::Internal { message: d() },
        DomainError::PayloadTooLarge {
            detail: d(),
            instance: i(),
        },
        DomainError::RateLimitExceeded {
            detail: d(),
            instance: i(),
            retry_after_secs: None,
            limit: None,
            reset_epoch: None,
            remaining: None,
        },
        DomainError::AuthenticationFailed {
            detail: d(),
            instance: i(),
            reason: oagw_sdk::reason::auth::PLUGIN_INTERNAL,
        },
        DomainError::CredentialStoreUnavailable {
            detail: d(),
            retry_after: None,
            instance: i(),
        },
        // The identity provider, not the upstream.
        DomainError::IdpUnavailable {
            detail: d(),
            instance: i(),
        },
        DomainError::IdpInvalidResponse {
            detail: d(),
            instance: i(),
        },
        DomainError::GuardRejected {
            status: 403,
            error_code: d(),
            detail: d(),
            instance: i(),
            resource_id: None,
        },
        DomainError::CircuitBreakerOpen {
            detail: d(),
            instance: i(),
            retry_after: Duration::from_secs(1),
        },
    ];
    for err in &ignored {
        assert_eq!(outcome_of_error(err), Outcome::Ignore, "{err:?}");
    }
}
