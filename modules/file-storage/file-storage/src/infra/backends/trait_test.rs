#[cfg(test)]
mod tests {
    use super::super::r#trait::BackendDescriptor;
    use file_storage_sdk::{Backend, BackendCapability, BackendKind, BackendTransport};
    use uuid::Uuid;

    fn descriptor_with_access(tenant_access: Vec<Uuid>) -> BackendDescriptor {
        let id = Uuid::new_v4();
        BackendDescriptor {
            sdk: Backend {
                id,
                kind: BackendKind::S3Compatible,
                default_public: false,
                default_private: true,
                transport: BackendTransport::Redirect,
                capabilities: vec![
                    BackendCapability::PresignedUrls,
                    BackendCapability::PublicReadUrls,
                ],
                max_file_size_bytes: Some(2048),
            },
            max_signed_url_ttl_seconds_value: 1234,
            tenant_access,
        }
    }

    #[test]
    fn empty_tenant_access_visible_to_every_tenant() {
        let d = descriptor_with_access(vec![]);
        assert!(d.is_visible_to(Uuid::new_v4()));
        assert!(d.is_visible_to(Uuid::nil()));
    }

    #[test]
    fn nonempty_tenant_access_visible_only_to_listed_tenants() {
        let allowed = Uuid::new_v4();
        let other = Uuid::new_v4();
        let d = descriptor_with_access(vec![allowed]);
        assert!(d.is_visible_to(allowed));
        assert!(!d.is_visible_to(other), "non-listed tenant must NOT see backend");
    }

    #[test]
    fn descriptor_accessor_methods() {
        let d = descriptor_with_access(vec![]);
        assert_eq!(d.id(), d.sdk.id);
        assert_eq!(d.max_signed_url_ttl_seconds(), 1234);
        assert_eq!(d.max_file_size_bytes(), Some(2048));
        assert_eq!(d.capabilities().len(), 2);
        assert!(
            d.capabilities()
                .contains(&BackendCapability::PresignedUrls)
        );
    }
}
