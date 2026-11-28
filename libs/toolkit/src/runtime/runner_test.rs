use super::*;

trait TestClient: Send + Sync {
    fn value(&self) -> u32;
}

struct FixedClient(u32);

impl TestClient for FixedClient {
    fn value(&self) -> u32 {
        self.0
    }
}

#[test]
fn a_remote_registration_is_visible_without_becoming_local() {
    let hub = ClientHub::new();
    let remote: Arc<dyn TestClient> = Arc::new(FixedClient(11));
    crate::runtime::ClientRegistration::remote(Arc::clone(&remote)).apply(&hub);
    let orders = hub.for_gear("orders");
    let billing = hub.for_gear("billing");

    let found = hub.get::<dyn TestClient>().unwrap();
    assert_eq!(
        (
            found.value(),
            hub.try_get_local::<dyn TestClient>().is_some(),
            hub.has_remote_proxy::<dyn TestClient>(),
            Arc::ptr_eq(&found, &remote),
        ),
        (11, false, true, true)
    );
    let found = orders.get::<dyn TestClient>().unwrap();
    assert_eq!(
        (
            found.value(),
            orders.try_get_local::<dyn TestClient>().is_some(),
            orders.has_remote_proxy::<dyn TestClient>(),
            Arc::ptr_eq(&found, &remote),
        ),
        (11, false, false, true)
    );
    let found = billing.get::<dyn TestClient>().unwrap();
    assert_eq!(
        (
            found.value(),
            billing.try_get_local::<dyn TestClient>().is_some(),
            billing.has_remote_proxy::<dyn TestClient>(),
            Arc::ptr_eq(&found, &remote),
        ),
        (11, false, false, true)
    );
}

#[test]
fn a_local_registration_stays_local() {
    let hub = ClientHub::new();
    let local: Arc<dyn TestClient> = Arc::new(FixedClient(22));
    crate::runtime::ClientRegistration::new(Arc::clone(&local)).apply(&hub);
    let orders = hub.for_gear("orders");

    let found = hub.get::<dyn TestClient>().unwrap();
    assert_eq!(
        (
            found.value(),
            hub.try_get_local::<dyn TestClient>().is_some(),
            hub.has_remote_proxy::<dyn TestClient>(),
            Arc::ptr_eq(&found, &local),
        ),
        (22, true, false, true)
    );
    let found = orders.get::<dyn TestClient>().unwrap();
    assert_eq!(
        (
            found.value(),
            orders.try_get_local::<dyn TestClient>().is_some(),
            orders.has_remote_proxy::<dyn TestClient>(),
            Arc::ptr_eq(&found, &local),
        ),
        (22, true, false, true)
    );
}

#[test]
fn a_local_registration_wins_in_either_registration_order() {
    let remote: Arc<dyn TestClient> = Arc::new(FixedClient(11));
    let local: Arc<dyn TestClient> = Arc::new(FixedClient(22));
    for registrations in [
        [
            crate::runtime::ClientRegistration::remote(Arc::clone(&remote)),
            crate::runtime::ClientRegistration::new(Arc::clone(&local)),
        ],
        [
            crate::runtime::ClientRegistration::new(Arc::clone(&local)),
            crate::runtime::ClientRegistration::remote(Arc::clone(&remote)),
        ],
    ] {
        let hub = ClientHub::new();
        for registration in registrations {
            registration.apply(&hub);
        }
        let orders = hub.for_gear("orders");

        let found = hub.get::<dyn TestClient>().unwrap();
        assert_eq!(
            (
                found.value(),
                hub.try_get_local::<dyn TestClient>().is_some(),
                hub.has_remote_proxy::<dyn TestClient>(),
                Arc::ptr_eq(&found, &local),
            ),
            (22, true, true, true)
        );
        let found = orders.get::<dyn TestClient>().unwrap();
        assert_eq!(
            (
                found.value(),
                orders.try_get_local::<dyn TestClient>().is_some(),
                orders.has_remote_proxy::<dyn TestClient>(),
                Arc::ptr_eq(&found, &local),
            ),
            (22, true, false, true)
        );
    }
}

struct EmptyConfigProvider;
impl ConfigProvider for EmptyConfigProvider {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn opts() -> RunOptions {
    RunOptions::new(
        Arc::new(EmptyConfigProvider),
        DbOptions::None,
        ShutdownOptions::Token(CancellationToken::new()),
        Uuid::new_v4(),
    )
}

#[test]
fn build_host_runtime_applies_the_init_deadline() {
    // `Some` must reach `HostRuntime::with_init_deadline`; `None` leaves the
    // `DEFAULT_INIT_DEADLINE` in place.
    let deadline = std::time::Duration::from_secs(7);
    let host = build_host_runtime(opts().with_init_deadline(Some(deadline))).unwrap();
    assert_eq!(host.init_deadline(), deadline);

    let host = build_host_runtime(opts()).unwrap();
    assert_eq!(host.init_deadline(), crate::runtime::DEFAULT_INIT_DEADLINE);
}
