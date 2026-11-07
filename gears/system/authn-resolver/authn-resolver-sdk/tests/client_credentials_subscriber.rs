//! This separate test binary controls when its first subscriber is installed.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsProvider,
    ClientCredentialsProviderError, ClientCredentialsRequest,
};
use parking_lot::Mutex;
use secrecy::SecretString;
use tokio::sync::{Notify, oneshot};
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber as _;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

#[derive(Default)]
struct CapturedEvents {
    events: Mutex<Vec<String>>,
    next_span: AtomicU64,
}

struct CaptureSubscriber(Arc<CapturedEvents>);

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(self.0.next_span.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut message = Message::default();
        event.record(&mut message);
        if !message.0.is_empty() {
            self.0.events.lock().push(message.0);
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

struct GatedResolver {
    entered: Notify,
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

#[async_trait]
impl AuthNResolverClient for GatedResolver {
    async fn authenticate(
        &self,
        _request: &authn_resolver_sdk::AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("unused bearer path".into()))
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let release = self.release.lock().take();
        self.entered.notify_one();
        if let Some(release) = release {
            release
                .await
                .map_err(|error| AuthNResolverError::Internal(error.to_string()))?;
        }
        Err(AuthNResolverError::ServiceUnavailable("offline".into()))
    }
}

fn failure_count(events: &CapturedEvents) -> usize {
    events
        .events
        .lock()
        .iter()
        .filter(|message| message.contains("S2S credential acquisition failed"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_global_logging_and_an_active_scoped_subscriber_both_work() {
    assert!(tracing::dispatcher::get_default(|dispatcher| {
        dispatcher.is::<tracing::subscriber::NoSubscriber>()
    }));
    let (release, wait) = oneshot::channel();
    let resolver = Arc::new(GatedResolver {
        entered: Notify::new(),
        release: Mutex::new(Some(wait)),
    });
    let build = || {
        ClientCredentialsProvider::builder(
            resolver.clone(),
            "service",
            SecretString::from("example-only-secret"),
        )
        .build()
    };
    let first = build().unwrap();
    let first_call = first.clone();
    let pending = tokio::spawn(async move { first_call.get().await });
    resolver.entered.notified().await;

    let global = Arc::new(CapturedEvents::default());
    tracing::subscriber::set_global_default(CaptureSubscriber(global.clone())).unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(ClientCredentialsProviderError::Resolver(
            AuthNResolverError::ServiceUnavailable(_)
        ))
    ));
    first.shutdown().await;
    let second = build().unwrap();
    assert!(second.get().await.is_err());
    second.shutdown().await;
    assert_eq!(failure_count(&global), 2);

    let scoped = Arc::new(CapturedEvents::default());
    let third = build().unwrap();
    assert!(
        third
            .get()
            .with_subscriber(CaptureSubscriber(scoped.clone()))
            .await
            .is_err()
    );
    third.shutdown().await;
    assert_eq!(failure_count(&scoped), 1);
    assert_eq!(failure_count(&global), 2);
}
