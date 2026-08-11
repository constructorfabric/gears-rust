use super::*;
use crate::domain::rms_registry::AdapterRecord;

/// Mock registry mapping one known subject → gts id; everything else is absent.
struct MockRegistry {
    known_subject: &'static str,
    gts_id: &'static str,
}

#[async_trait]
impl RmsAdapterRegistry for MockRegistry {
    async fn lookup(&self, _gts_id: &str) -> Result<Option<AdapterRecord>, DomainError> {
        Ok(None)
    }

    async fn gts_id_by_cert_subject(&self, subject: &str) -> Result<Option<String>, DomainError> {
        Ok((subject == self.known_subject).then(|| self.gts_id.to_owned()))
    }
}

fn resolver() -> RegistryPeerIdentityResolver {
    RegistryPeerIdentityResolver::new(Arc::new(MockRegistry {
        known_subject: "CN=adapter-s3,O=acme",
        gts_id: "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1",
    }))
}

#[tokio::test]
async fn resolves_known_cert_subject_to_gts_id() {
    let peer = PeerConnInfo {
        client_cert_subject: Some("CN=adapter-s3,O=acme".to_owned()),
    };
    let gts = resolver().resolve(&peer).await.unwrap();
    assert_eq!(gts, "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1");
}

#[tokio::test]
async fn fails_closed_without_certificate() {
    let peer = PeerConnInfo {
        client_cert_subject: None,
    };
    assert!(matches!(
        resolver().resolve(&peer).await,
        Err(DomainError::PeerUnverified)
    ));
}

#[tokio::test]
async fn unknown_subject_is_peer_unknown() {
    let peer = PeerConnInfo {
        client_cert_subject: Some("CN=stranger".to_owned()),
    };
    assert!(matches!(
        resolver().resolve(&peer).await,
        Err(DomainError::PeerUnknown)
    ));
}
