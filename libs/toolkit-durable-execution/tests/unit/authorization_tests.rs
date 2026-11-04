use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Records calls and answers with a fixed outcome, or never answers. Grants a
/// tenant-constrained scope unless `unconstrained` is set.
#[derive(Default)]
struct Recording {
    calls: AtomicUsize,
    outcome: Option<AuthorizationErrorKind>,
    stall: bool,
    unconstrained: bool,
}

#[async_trait]
impl Authorizer for Recording {
    async fn caller_scope(
        &self,
        _: &SecurityContext,
        _: CallerAction,
    ) -> Result<AccessScope, AuthorizationError> {
        self.answer().await
    }
    async fn service_scope(&self, _: ServiceAction) -> Result<AccessScope, AuthorizationError> {
        self.answer().await
    }
}

impl Recording {
    async fn answer(&self) -> Result<AccessScope, AuthorizationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall {
            std::future::pending::<()>().await;
        }
        match self.outcome {
            None if self.unconstrained => Ok(AccessScope::allow_all()),
            None => Ok(AccessScope::for_tenant(Uuid::from_u128(1))),
            Some(AuthorizationErrorKind::Denied) => Err(AuthorizationError::denied()),
            Some(AuthorizationErrorKind::Unavailable) => Err(AuthorizationError::unavailable()
                .with_source(std::io::Error::other("policy endpoint down"))),
        }
    }
}

fn caller(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

#[test]
fn action_names_resources_and_constraints_are_stable() {
    let caller_actions = [
        (CallerAction::Create, "create"),
        (CallerAction::Get, "get"),
        (CallerAction::Update, "update"),
        (CallerAction::Manage, "manage"),
        (CallerAction::Inspect, "inspect"),
        (CallerAction::List, "list"),
    ];
    for (action, name) in caller_actions {
        assert_eq!(action.as_str(), name);
        assert_eq!(action.to_string(), name);
        assert_eq!(action.resource(), Resource::Run);
        assert_eq!(action.constraints(), ConstraintPolicy::Required);
    }
    let service_actions = [
        (
            ServiceAction::Execute,
            "execute",
            Resource::Run,
            ConstraintPolicy::Required,
        ),
        (
            ServiceAction::Heartbeat,
            "heartbeat",
            Resource::Run,
            ConstraintPolicy::Required,
        ),
        (
            ServiceAction::Recover,
            "recover",
            Resource::Run,
            ConstraintPolicy::Required,
        ),
        (
            ServiceAction::Dispatch,
            "dispatch",
            Resource::Run,
            ConstraintPolicy::Required,
        ),
        (
            ServiceAction::CancelDefinition,
            "cancel_definition",
            Resource::Run,
            ConstraintPolicy::Optional,
        ),
        (
            ServiceAction::DefinitionRegister,
            "register",
            Resource::Definition,
            ConstraintPolicy::Optional,
        ),
        (
            ServiceAction::DefinitionGet,
            "get",
            Resource::Definition,
            ConstraintPolicy::Optional,
        ),
        (
            ServiceAction::DefinitionUnregister,
            "unregister",
            Resource::Definition,
            ConstraintPolicy::Optional,
        ),
        (
            ServiceAction::DefinitionActivate,
            "activate",
            Resource::Definition,
            ConstraintPolicy::Optional,
        ),
        (
            ServiceAction::DefinitionList,
            "list",
            Resource::Definition,
            ConstraintPolicy::Optional,
        ),
        (
            ServiceAction::DefinitionRelease,
            "release",
            Resource::Definition,
            ConstraintPolicy::Optional,
        ),
    ];
    for (action, name, resource, constraints) in service_actions {
        assert_eq!(action.as_str(), name);
        assert_eq!(action.resource(), resource, "{name}");
        assert_eq!(action.constraints(), constraints, "{name}");
    }
    assert_eq!(Resource::Run.name(), "durable_execution.run");
    assert_eq!(Resource::Definition.name(), "durable_execution.definition");
    assert_eq!(
        Resource::Run.properties(),
        &[
            pep_properties::OWNER_TENANT_ID,
            pep_properties::OWNER_ID,
            pep_properties::RESOURCE_ID
        ]
    );
    assert_eq!(
        Resource::Definition.properties(),
        &[pep_properties::RESOURCE_ID]
    );
}

#[tokio::test]
async fn callers_without_subject_or_tenant_are_denied_before_policy() {
    let authorizer = Arc::new(Recording::default());
    let authority = Authority::new(authorizer.clone(), Duration::from_secs(1));
    for ctx in [
        caller(Uuid::nil(), Uuid::new_v4()),
        caller(Uuid::new_v4(), Uuid::nil()),
    ] {
        let error = authority.caller(&ctx, CallerAction::Get).await.unwrap_err();
        assert_eq!(error.kind(), AuthorizationErrorKind::Denied);
    }
    assert_eq!(authorizer.calls.load(Ordering::SeqCst), 0);
    authority
        .caller(&caller(Uuid::new_v4(), Uuid::new_v4()), CallerAction::Get)
        .await
        .unwrap();
    assert_eq!(authorizer.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn stalled_authorization_times_out_as_unavailable() {
    let authority = Authority::new(
        Arc::new(Recording {
            stall: true,
            ..Recording::default()
        }),
        Duration::from_secs(5),
    );
    let caller_error = authority
        .caller(
            &caller(Uuid::new_v4(), Uuid::new_v4()),
            CallerAction::Create,
        )
        .await
        .unwrap_err();
    let service_error = authority.service(ServiceAction::Execute).await.unwrap_err();
    assert_eq!(caller_error.kind(), AuthorizationErrorKind::Unavailable);
    assert_eq!(service_error.kind(), AuthorizationErrorKind::Unavailable);
    // The timeout is distinguishable from an authorizer that answered unavailable.
    let cause = StdError::source(&service_error).map(ToString::to_string);
    assert!(cause.is_some_and(|cause| cause.contains("did not answer")));
}

#[tokio::test]
async fn authorizer_failures_keep_their_kind_and_cause() {
    for kind in [
        AuthorizationErrorKind::Denied,
        AuthorizationErrorKind::Unavailable,
    ] {
        let authority = Authority::new(
            Arc::new(Recording {
                outcome: Some(kind),
                ..Recording::default()
            }),
            Duration::from_secs(1),
        );
        let error = authority
            .service(ServiceAction::Dispatch)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(
            StdError::source(&error).is_some(),
            kind == AuthorizationErrorKind::Unavailable
        );
        assert!(!format!("{error:?}").contains("policy endpoint"));
    }
}

#[tokio::test]
async fn unconstrained_scopes_are_rejected_only_where_constraints_are_required() {
    let authority = Authority::new(
        Arc::new(Recording {
            unconstrained: true,
            ..Recording::default()
        }),
        Duration::from_secs(1),
    );
    let ctx = caller(Uuid::new_v4(), Uuid::new_v4());
    for action in [
        CallerAction::Create,
        CallerAction::Get,
        CallerAction::Update,
        CallerAction::Manage,
        CallerAction::Inspect,
        CallerAction::List,
    ] {
        let error = authority.caller(&ctx, action).await.unwrap_err();
        assert_eq!(error.kind(), AuthorizationErrorKind::Denied, "{action}");
    }
    for action in [
        ServiceAction::Execute,
        ServiceAction::Heartbeat,
        ServiceAction::Recover,
        ServiceAction::Dispatch,
    ] {
        let error = authority.service(action).await.unwrap_err();
        assert_eq!(error.kind(), AuthorizationErrorKind::Denied, "{action}");
    }
    for action in [
        ServiceAction::CancelDefinition,
        ServiceAction::DefinitionRegister,
        ServiceAction::DefinitionGet,
        ServiceAction::DefinitionUnregister,
        ServiceAction::DefinitionActivate,
        ServiceAction::DefinitionList,
        ServiceAction::DefinitionRelease,
    ] {
        assert!(
            authority.service(action).await.unwrap().is_unconstrained(),
            "{action}"
        );
    }
}

#[test]
fn with_source_accepts_anyhow_and_boxed_errors() {
    let from_anyhow = AuthorizationError::unavailable().with_source(anyhow::anyhow!("pdp down"));
    let boxed: Box<dyn StdError + Send + Sync> = "token expired".into();
    let from_box = AuthorizationError::denied().with_source(boxed);
    assert_eq!(
        StdError::source(&from_anyhow).map(ToString::to_string),
        Some("pdp down".to_owned())
    );
    assert_eq!(
        StdError::source(&from_box).map(ToString::to_string),
        Some("token expired".to_owned())
    );
}

mod logs {
    use super::*;
    use std::collections::BTreeMap;
    use tracing::instrument::WithSubscriber;

    type Fields = BTreeMap<String, String>;

    /// Captures the fields of every event.
    #[derive(Clone, Default)]
    struct Capture(Arc<parking_lot::Mutex<Vec<Fields>>>);
    struct Visitor(Fields);
    impl tracing::field::Visit for Visitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = Visitor(BTreeMap::new());
            event.record(&mut visitor);
            self.0.lock().push(visitor.0);
        }
    }

    impl Capture {
        fn events(&self) -> Vec<Fields> {
            self.0.lock().clone()
        }
    }

    fn keys(fields: &Fields) -> Vec<&str> {
        fields.keys().map(String::as_str).collect()
    }

    #[tokio::test]
    async fn caller_rejections_log_identity_and_reason_but_never_the_cause() {
        let capture = Capture::default();
        let subject = Uuid::from_u128(0xabc);
        let tenant = Uuid::from_u128(0xdef);
        let authority = Authority::new(
            Arc::new(Recording {
                outcome: Some(AuthorizationErrorKind::Unavailable),
                ..Recording::default()
            }),
            Duration::from_secs(1),
        );
        async {
            crate::test_support::keep_callsites_enabled();
            authority
                .caller(&caller(subject, tenant), CallerAction::Get)
                .await
                .unwrap_err();
            authority
                .caller(&caller(Uuid::nil(), tenant), CallerAction::List)
                .await
                .unwrap_err();
        }
        .with_subscriber(capture.clone())
        .await;
        let events = capture.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(
            keys(&events[0]),
            [
                "action",
                "message",
                "reason",
                "resource",
                "subject_id",
                "tenant_id"
            ]
        );
        assert_eq!(events[0]["action"], "get");
        assert_eq!(events[0]["reason"], "unavailable");
        assert_eq!(events[0]["resource"], "durable_execution.run");
        assert_eq!(events[0]["subject_id"], subject.to_string());
        assert_eq!(events[0]["tenant_id"], tenant.to_string());
        assert_eq!(events[1]["reason"], "missing_identity");
        assert!(
            events
                .iter()
                .flat_map(|fields| fields.values())
                .all(|value| !value.contains("policy endpoint"))
        );
    }

    #[tokio::test]
    async fn service_rejections_log_the_reason_without_identity() {
        let capture = Capture::default();
        let authority = Authority::new(
            Arc::new(Recording {
                unconstrained: true,
                ..Recording::default()
            }),
            Duration::from_secs(1),
        );
        async {
            crate::test_support::keep_callsites_enabled();
            authority.service(ServiceAction::Execute).await.unwrap_err();
        }
        .with_subscriber(capture.clone())
        .await;
        let events = capture.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(
            keys(&events[0]),
            ["action", "message", "reason", "resource"]
        );
        assert_eq!(events[0]["reason"], "unconstrained_scope");
    }
}
