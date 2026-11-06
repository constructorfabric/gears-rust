use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::Notify;
use toolkit_contract::runtime::config::{CredentialState, InternalTokenProvider};
use toolkit_contract::wiring::ClientTuning;

use super::{GtsDeploymentOutcome, GtsDeploymentReport, publish};
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::discovery::{EndpointResolver, WireOutcome};
use crate::gts_publication::{
    DeclarationSource, GtsPublicationBackend, GtsPublicationDescriptor, GtsPublicationExpired,
    GtsPublicationRequest,
};

const ENDPOINT: &str = "https://registry.example:8443";
const BASE: &str = "gts.cf.test.publication.base.v1~";
const DERIVED: &str = "gts.cf.test.publication.base.v1~cf.test.publication.derived.v1~";

/// The remote registry the wire fn binds: it records the endpoint and the
/// credential it got, and admits `DERIVED` only once `BASE` is admitted.
#[derive(Default)]
struct RemoteRegistry {
    deadlines: Mutex<Vec<Option<tokio::time::Instant>>>,
    endpoint: Mutex<Option<String>>,
    token: Mutex<Option<String>>,
    admitted: Mutex<Vec<String>>,
    base_admitted: Notify,
}

static REGISTRY: std::sync::LazyLock<Mutex<Option<Arc<RemoteRegistry>>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

#[allow(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "must match the WireFn signature the gear macro emits"
)]
fn wire(
    hub: &ClientHub,
    resolver: Arc<dyn EndpointResolver>,
    tuning: ClientTuning,
) -> anyhow::Result<WireOutcome> {
    let registry = REGISTRY
        .lock()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no test registry"))?;
    if let Some(provider) = &tuning.internal_token_provider
        && let CredentialState::Available(token) = provider.current()
    {
        use secrecy::ExposeSecret as _;
        *registry.token.lock() = Some(token.expose_secret().to_owned());
    }
    let endpoint = futures_util::FutureExt::now_or_never(resolver.resolve_endpoint("registry"))
        .and_then(Result::ok)
        .flatten();
    *registry.endpoint.lock() = endpoint;
    hub.register::<RemoteRegistry>(registry);
    Ok(WireOutcome::Remote)
}

fn backend(hub: &ClientHub) -> anyhow::Result<GtsPublicationBackend> {
    let registry = hub.get::<RemoteRegistry>()?;
    Ok(GtsPublicationBackend::new(
        move |request: GtsPublicationRequest| {
            let registry = Arc::clone(&registry);
            async move {
                registry.deadlines.lock().push(request.deadline());
                for (id, document) in request.gts_manifest().documents() {
                    if document.get("refuse").is_some() {
                        anyhow::bail!("{id}: rejected");
                    }
                    if document.get("expire").is_some() {
                        // The gear's own budget, shorter than the job's deadline.
                        return Err(anyhow::anyhow!("{id}: deadline reached")
                            .context(GtsPublicationExpired));
                    }
                    if id.id() == DERIVED {
                        loop {
                            let base_admitted = registry.base_admitted.notified();
                            if registry.admitted.lock().iter().any(|a| a == BASE) {
                                break;
                            }
                            base_admitted.await;
                        }
                    }
                    if document.get("hang").is_some() {
                        std::future::pending::<()>().await;
                    }
                    registry.admitted.lock().push(id.to_string());
                    if id.id() == BASE {
                        registry.base_admitted.notify_waiters();
                    }
                }
                Ok(())
            }
        },
    ))
}

fn derived() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "t",
        vec![(DERIVED.to_owned(), "{}".to_owned())],
    )]
}

fn base() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "t",
        vec![(BASE.to_owned(), "{}".to_owned())],
    )]
}

fn refused() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "t",
        vec![(
            "gts.cf.test.publication.refused.v1~".to_owned(),
            json!({"refuse": true}).to_string(),
        )],
    )]
}

fn expiring() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "t",
        vec![(
            "gts.cf.test.publication.expiring.v1~".to_owned(),
            json!({"expire": true}).to_string(),
        )],
    )]
}

fn hanging() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "t",
        vec![(
            "gts.cf.test.publication.hanging.v1~".to_owned(),
            json!({"hang": true}).to_string(),
        )],
    )]
}

macro_rules! descriptor {
    ($name:ident, $gear:literal, $docs:path) => {
        static $name: GtsPublicationDescriptor = GtsPublicationDescriptor {
            gear: $gear,
            publisher_version: "1.0.0",
            declarations: $docs,
            entities: None,
            backend,
            provider: "registry",
            wire,
        };
    };
}

descriptor!(DERIVED_PUB, "derived-publisher", derived);
descriptor!(BASE_PUB, "base-publisher", base);
descriptor!(REFUSED_PUB, "refused-publisher", refused);
descriptor!(HANGING_PUB, "hanging-publisher", hanging);
descriptor!(EXPIRING_PUB, "expiring-publisher", expiring);

struct NoConfig;

impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn tuning() -> ClientTuning {
    ClientTuning::default().with_internal_token_provider(Some(InternalTokenProvider::from_token(
        secrecy::SecretString::from("platform-token".to_owned()),
    )))
}

/// The tests share the static registry slot.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn run(
    descriptors: &[&'static GtsPublicationDescriptor],
    deadline: Duration,
) -> (anyhow::Result<GtsDeploymentReport>, Arc<RemoteRegistry>) {
    let registry = Arc::new(RemoteRegistry::default());
    *REGISTRY.lock() = Some(Arc::clone(&registry));
    let report = publish(
        descriptors,
        &NoConfig,
        ENDPOINT,
        &tuning(),
        deadline,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    (report, registry)
}

/// A derived-type publisher listed first does not delay its base-type
/// publisher: both succeed, through the explicit endpoint with the credential.
#[tokio::test]
async fn publishers_run_concurrently_against_the_explicit_registry() {
    let _serial = SERIAL.lock().await;
    let (report, registry) = run(&[&DERIVED_PUB, &BASE_PUB], Duration::from_secs(10)).await;
    let report = report.unwrap();

    assert!(report.succeeded(), "{report}");
    let admitted_seen = { registry.admitted.lock().clone() };
    assert_eq!(admitted_seen, [BASE, DERIVED]);
    let endpoint_seen = { registry.endpoint.lock().clone() };
    assert_eq!(endpoint_seen.as_deref(), Some(ENDPOINT));
    let token_seen = { registry.token.lock().clone() };
    assert_eq!(token_seen.as_deref(), Some("platform-token"));
}

/// A refusal is reported per publisher, with its GTS id; the others still run.
#[tokio::test]
async fn a_refusal_fails_the_job_and_names_the_publisher() {
    let _serial = SERIAL.lock().await;
    let (report, _) = run(&[&REFUSED_PUB, &BASE_PUB], Duration::from_secs(10)).await;
    let report = report.unwrap();

    assert!(!report.succeeded());
    assert_eq!(
        report.publishers[1],
        (
            "base-publisher",
            GtsDeploymentOutcome::Published { documents: 1 }
        )
    );
    let (gear, GtsDeploymentOutcome::Refused(message)) = &report.publishers[0] else {
        panic!("{report}");
    };
    assert_eq!(*gear, "refused-publisher");
    assert!(
        message.contains("gts.cf.test.publication.refused.v1~"),
        "{message}"
    );
}

/// The deadline ends what is still pending and names its GTS ids.
#[tokio::test(start_paused = true)]
async fn the_deadline_ends_pending_publishers() {
    let _serial = SERIAL.lock().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (report, registry) = run(&[&HANGING_PUB, &BASE_PUB], Duration::from_secs(30)).await;
    let report = report.unwrap();

    let deadlines_seen = { registry.deadlines.lock().clone() };
    assert_eq!(deadlines_seen, [Some(deadline), Some(deadline)]);

    assert!(!report.succeeded());
    assert_eq!(
        report.publishers,
        [
            (
                "hanging-publisher",
                GtsDeploymentOutcome::DeadlineExceeded {
                    unconfirmed: vec!["gts.cf.test.publication.hanging.v1~".to_owned()]
                }
            ),
            (
                "base-publisher",
                GtsDeploymentOutcome::Published { documents: 1 }
            ),
        ]
    );
}

/// A publisher whose own budget runs out before the job's deadline is reported
/// as not confirmed in time, with its GTS ids, not as refused.
#[tokio::test]
async fn a_gear_budget_expiry_is_a_deadline_not_a_refusal() {
    let _serial = SERIAL.lock().await;
    let (report, _) = run(&[&EXPIRING_PUB, &BASE_PUB], Duration::from_secs(10)).await;
    let report = report.unwrap();

    assert!(!report.succeeded());
    assert_eq!(
        report.publishers,
        [
            (
                "expiring-publisher",
                GtsDeploymentOutcome::DeadlineExceeded {
                    unconfirmed: vec!["gts.cf.test.publication.expiring.v1~".to_owned()]
                }
            ),
            (
                "base-publisher",
                GtsDeploymentOutcome::Published { documents: 1 }
            ),
        ]
    );
}

/// The platform credential never goes to an endpoint that is not an absolute
/// http(s) URI: the job fails before any wiring.
#[tokio::test]
async fn a_relative_or_foreign_registry_endpoint_fails_before_wiring() {
    let _serial = SERIAL.lock().await;
    for endpoint in [
        "registry:8443",
        "/registry",
        "ftp://registry",
        "not a uri",
        "https://:8443",
    ] {
        let registry = Arc::new(RemoteRegistry::default());
        *REGISTRY.lock() = Some(Arc::clone(&registry));
        let err = publish(
            &[&BASE_PUB],
            &NoConfig,
            endpoint,
            &tuning(),
            Duration::from_secs(10),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("registry endpoint"),
            "{endpoint}: {err:#}"
        );
        let token = { registry.token.lock().clone() };
        assert_eq!(token, None, "{endpoint}: no wiring, so no credential");
    }
}

/// Two publishers of one GTS id fail the job before any request.
#[tokio::test]
async fn an_ownership_conflict_fails_before_publication() {
    let _serial = SERIAL.lock().await;
    descriptor!(BASE_AGAIN, "base-again", base);
    let (report, registry) = run(&[&BASE_PUB, &BASE_AGAIN], Duration::from_secs(10)).await;

    let err = report.unwrap_err();
    assert!(
        err.to_string().contains("base-again and base-publisher"),
        "{err:#}"
    );
    assert!(registry.admitted.lock().is_empty());
}

/// The caller's cancellation ends pending publishers as cancelled, not as past
/// the deadline.
#[tokio::test]
async fn caller_cancellation_is_not_a_deadline() {
    let _serial = SERIAL.lock().await;
    let registry = Arc::new(RemoteRegistry::default());
    *REGISTRY.lock() = Some(Arc::clone(&registry));
    let cancel = tokio_util::sync::CancellationToken::new();
    let canceller = {
        let cancel = cancel.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        }
    };
    let descriptors = [&HANGING_PUB];
    let tuning = tuning();
    let (report, ()) = tokio::join!(
        publish(
            &descriptors,
            &NoConfig,
            ENDPOINT,
            &tuning,
            Duration::from_secs(3600),
            &cancel
        ),
        canceller
    );

    assert_eq!(
        report.unwrap().publishers,
        [(
            "hanging-publisher",
            GtsDeploymentOutcome::Cancelled {
                unconfirmed: vec!["gts.cf.test.publication.hanging.v1~".to_owned()]
            }
        )]
    );
}
