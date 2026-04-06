use super::*;

#[test]
fn mock_guard_generates_unique_prefix() {
    let guard1 = MockGuard::new();
    let guard2 = MockGuard::new();

    assert!(guard1.prefix().starts_with("/t-"));
    assert!(guard2.prefix().starts_with("/t-"));
    assert_ne!(guard1.prefix(), guard2.prefix());
}

#[test]
fn mock_guard_path_helper_prepends_prefix() {
    let guard = MockGuard::new();
    let full_path = guard.path("/v1/chat/completions");

    assert!(full_path.starts_with(guard.prefix()));
    assert!(full_path.ends_with("/v1/chat/completions"));
}

#[test]
fn mock_guard_registers_and_cleans_up_routes() {
    // Create a guard and track its specific routes
    let key1;
    let key2;

    {
        let mut guard = MockGuard::new();
        guard.mock(
            "POST",
            "/test",
            MockResponse {
                status: 200,
                headers: vec![],
                body: MockBody::Json(json!({"ok": true})),
            },
        );
        guard.mock(
            "GET",
            "/test2",
            MockResponse {
                status: 201,
                headers: vec![],
                body: MockBody::Json(json!({"created": true})),
            },
        );

        key1 = RouteKey {
            method: "POST".into(),
            path: guard.path("/test"),
        };
        key2 = RouteKey {
            method: "GET".into(),
            path: guard.path("/test2"),
        };

        // Routes should be registered
        assert!(guard.state.dynamic_routes.contains_key(&key1));
        assert!(guard.state.dynamic_routes.contains_key(&key2));
    }

    // After guard is dropped, routes should be cleaned up
    let state = shared_mock().shared_state();
    assert!(!state.dynamic_routes.contains_key(&key1));
    assert!(!state.dynamic_routes.contains_key(&key2));
}
