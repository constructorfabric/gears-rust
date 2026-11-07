//! Construction through [`ClientCredentialsProviderBuilder`].

use secrecy::{ExposeSecret as _, SecretString};

use super::*;

const SECRET: &str = "s3cr3t-value";

/// One exchange as the resolver received it.
#[derive(Debug, Clone, PartialEq)]
struct Exchange {
    client_id: String,
    client_secret: String,
    scopes: Option<ScopeSet>,
    cache_control: RequestCacheControl,
}

/// Resolver stub recording the exchanged requests.
#[derive(Default)]
struct RecordingClient {
    exchanges: Mutex<Vec<Exchange>>,
}

impl RecordingClient {
    fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.lock().clone()
    }
}

#[async_trait]
impl AuthNResolverClient for RecordingClient {
    async fn authenticate(&self, _request: &crate::AuthenticationRequest) -> Outcome {
        unreachable!("not used by the provider")
    }

    async fn exchange_client_credentials(&self, request: &ClientCredentialsRequest) -> Outcome {
        self.exchanges.lock().push(Exchange {
            client_id: request.client_id.clone(),
            client_secret: request.client_secret.expose_secret().to_owned(),
            scopes: request.scopes.clone(),
            cache_control: request.cache_control.clone(),
        });
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(7))
            .build()
            .unwrap();
        Ok(
            AuthenticationResult::new(ctx).with_cache_control(CacheControl::until(
                OffsetDateTime::now_utc() + Duration::from_secs(3600),
            )),
        )
    }
}

fn builder(client: &Arc<RecordingClient>) -> ClientCredentialsProviderBuilder {
    ClientCredentialsProvider::builder(client.clone(), "svc", SecretString::from(SECRET))
}

fn set(scopes: &[&str]) -> ScopeSet {
    ScopeSet::new(scopes).unwrap()
}

fn exchange(scopes: &[&str], cache_control: RequestCacheControl) -> Exchange {
    Exchange {
        client_id: "svc".to_owned(),
        client_secret: SECRET.to_owned(),
        scopes: ScopeSet::optional(scopes).unwrap(),
        cache_control,
    }
}

#[tokio::test]
async fn defaults_request_idp_default_scopes_with_the_default_config() {
    let client = Arc::new(RecordingClient::default());
    let provider = builder(&client).build().unwrap();

    assert_eq!(
        provider.handle.shared.core.config,
        ClientCredentialsProviderConfig::default()
    );
    provider.get().await.unwrap();
    provider.get().await.unwrap();

    assert_eq!(
        client.exchanges(),
        [exchange(&[], RequestCacheControl::AllowCached)],
        "credentials are sent with every exchange; the entry is reused"
    );
}

#[tokio::test]
async fn default_scopes_are_sent_canonically_and_shared_with_an_equal_explicit_set() {
    let client = Arc::new(RecordingClient::default());
    let provider = builder(&client)
        .with_default_scopes(set(&["write", "read", "write"]))
        .build()
        .unwrap();

    let default = provider.get().await.unwrap();
    let explicit = provider
        .get_with_scopes(&set(&["read", "write"]))
        .await
        .unwrap();
    provider.get_with_scopes(&set(&["z", "a"])).await.unwrap();

    assert_eq!(default.origin(), explicit.origin());
    assert_eq!(
        client.exchanges(),
        [
            exchange(&["read", "write"], RequestCacheControl::AllowCached),
            exchange(&["a", "z"], refresh_if(None)),
        ]
    );
}

#[test]
fn the_config_is_validated_at_build() {
    let client = Arc::new(RecordingClient::default());
    let build = |config: ClientCredentialsProviderConfig| {
        builder(&client).with_config(config).build().map(drop)
    };

    for invalid in [0, MAX_ENTRIES + 1] {
        assert_eq!(
            build(ClientCredentialsProviderConfig::default().with_max_entries(invalid)),
            Err(ClientCredentialsProviderConfigError::MaxEntries)
        );
    }
    assert_eq!(
        build(ClientCredentialsProviderConfig::default().with_acquisition_timeout(Duration::ZERO)),
        Err(ClientCredentialsProviderConfigError::AcquisitionTimeout)
    );
    for valid in [1, MAX_ENTRIES] {
        assert_eq!(
            build(ClientCredentialsProviderConfig::default().with_max_entries(valid)),
            Ok(())
        );
    }
    assert_eq!(ClientCredentialsProviderConfig::default().max_entries, 17);
}

#[test]
fn the_built_config_is_the_one_supplied() {
    let client = Arc::new(RecordingClient::default());
    let config = ClientCredentialsProviderConfig::default().with_max_entries(3);
    let provider = builder(&client)
        .with_config(config.clone())
        .build()
        .unwrap();

    assert_eq!(provider.handle.shared.core.config, config);
}

/// A plain test without a runtime: building must neither spawn nor exchange.
#[test]
fn building_does_not_contact_the_resolver() {
    let client = Arc::new(RecordingClient::default());
    let provider = builder(&client)
        .with_default_scopes(set(&["read"]))
        .build()
        .unwrap();

    assert!(client.exchanges().is_empty());
    assert_eq!(*provider.handle.shared.running.borrow(), 0);
}

#[test]
fn debug_output_never_contains_the_client_secret() {
    let client = Arc::new(RecordingClient::default());
    let builder = builder(&client).with_default_scopes(set(&["read"]));

    let rendered = format!("{builder:?}");
    assert!(rendered.contains("svc"), "{rendered}");
    assert!(rendered.contains("read"), "{rendered}");
    assert!(!rendered.contains(SECRET), "{rendered}");

    let provider = builder.build().unwrap();
    let rendered = format!("{provider:?}");
    assert!(!rendered.contains(SECRET), "{rendered}");
}
