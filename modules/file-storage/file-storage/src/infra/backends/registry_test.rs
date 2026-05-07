#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use file_storage_sdk::{
        Backend, BackendCapability, BackendId, BackendKind, BackendTransport, FileMeta, UrlParams,
    };
    use uuid::Uuid;

    use crate::domain::error::DomainError;
    use crate::infra::backends::registry::BackendRegistry;
    use crate::infra::backends::r#trait::{
        BackendDescriptor, BackendObjectKey, BackendReadResult, HeadResult, PresignedGetItem,
        PresignedGetOutcome, SharedBackend, StorageBackend,
    };

    struct FakeBackend {
        descriptor: BackendDescriptor,
    }

    #[async_trait]
    impl StorageBackend for FakeBackend {
        fn descriptor(&self) -> &BackendDescriptor {
            &self.descriptor
        }
        async fn open_read(
            &self,
            _key: &BackendObjectKey,
        ) -> Result<BackendReadResult, DomainError> {
            unimplemented!("fake: open_read not exercised by registry tests")
        }
        async fn delete_object(&self, _key: &BackendObjectKey) -> Result<(), DomainError> {
            Ok(())
        }
        async fn issue_presigned_put(
            &self,
            _key: &BackendObjectKey,
            _meta: &FileMeta,
            _params: &UrlParams,
            _expected_etag: &str,
            _ttl: u64,
        ) -> Result<String, DomainError> {
            Ok("https://example/u".to_owned())
        }
        async fn issue_presigned_gets(
            &self,
            items: Vec<PresignedGetItem>,
        ) -> Result<Vec<PresignedGetOutcome>, DomainError> {
            Ok(items
                .into_iter()
                .map(|i| PresignedGetOutcome {
                    key: i.key,
                    result: Err(DomainError::internal("fake: not exercised here")),
                })
                .collect())
        }
        async fn head_object(&self, _key: &BackendObjectKey) -> Result<HeadResult, DomainError> {
            Ok(HeadResult {
                content_hash: "hash".to_owned(),
                size_bytes: 0,
            })
        }
    }

    fn make_descriptor(id: BackendId, default_private: bool, tenant_access: Vec<Uuid>) -> BackendDescriptor {
        BackendDescriptor {
            sdk: Backend {
                id,
                kind: BackendKind::S3Compatible,
                default_public: false,
                default_private,
                transport: BackendTransport::Redirect,
                capabilities: vec![BackendCapability::PresignedUrls],
                max_file_size_bytes: None,
            },
            max_signed_url_ttl_seconds_value: 3600,
            tenant_access,
        }
    }

    fn make_registry(entries: Vec<(BackendId, bool, Vec<Uuid>)>) -> BackendRegistry {
        let mut map: HashMap<BackendId, SharedBackend> = HashMap::new();
        for (id, private, access) in entries {
            let backend = Arc::new(FakeBackend {
                descriptor: make_descriptor(id, private, access),
            });
            map.insert(id, backend);
        }
        BackendRegistry::new(map)
    }

    #[test]
    fn resolve_visible_returns_backend_when_id_matches_and_tenant_allowed() {
        let tenant = Uuid::new_v4();
        let id = Uuid::new_v4();
        let reg = make_registry(vec![(id, true, vec![tenant])]);
        let result = reg.resolve_visible(id, tenant);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_visible_returns_not_found_for_unknown_id() {
        let tenant = Uuid::new_v4();
        let known = Uuid::new_v4();
        let unknown = Uuid::new_v4();
        let reg = make_registry(vec![(known, true, vec![])]);
        match reg.resolve_visible(unknown, tenant) {
            Err(DomainError::NotFound) => {}
            Err(other) => panic!("expected NotFound, got: {other:?}"),
            Ok(_) => panic!("expected NotFound, got Ok"),
        }
    }

    #[test]
    fn resolve_visible_returns_not_found_when_tenant_not_in_access_list() {
        let allowed = Uuid::new_v4();
        let other = Uuid::new_v4();
        let id = Uuid::new_v4();
        let reg = make_registry(vec![(id, true, vec![allowed])]);
        match reg.resolve_visible(id, other) {
            Err(DomainError::NotFound) => {}
            Err(other) => panic!(
                "expected NotFound (no enumeration oracle), got: {other:?}"
            ),
            Ok(_) => panic!("hidden backend must NOT be visible"),
        }
    }

    #[test]
    fn resolve_visible_works_with_empty_tenant_access_list() {
        let id = Uuid::new_v4();
        let reg = make_registry(vec![(id, true, vec![])]);
        assert!(reg.resolve_visible(id, Uuid::new_v4()).is_ok());
    }

    #[test]
    fn list_visible_to_tenant_filters_by_access_list() {
        let allowed = Uuid::new_v4();
        let other = Uuid::new_v4();
        let private_id = Uuid::new_v4();
        let scoped_id = Uuid::new_v4();
        let alt_id = Uuid::new_v4();
        let reg = make_registry(vec![
            (private_id, false, vec![]),
            (scoped_id, false, vec![allowed]),
            (alt_id, false, vec![other]),
        ]);
        let visible_to_allowed = reg.list_visible_to_tenant(allowed);
        let ids: Vec<BackendId> = visible_to_allowed.iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&private_id));
        assert!(ids.contains(&scoped_id));
    }

    #[test]
    fn list_visible_to_tenant_includes_all_when_all_have_empty_access() {
        let reg = make_registry(vec![
            (Uuid::new_v4(), false, vec![]),
            (Uuid::new_v4(), false, vec![]),
        ]);
        let v = reg.list_visible_to_tenant(Uuid::new_v4());
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn list_visible_to_tenant_excludes_hidden_backends() {
        let allowed = Uuid::new_v4();
        let other = Uuid::new_v4();
        let id = Uuid::new_v4();
        let reg = make_registry(vec![(id, false, vec![allowed])]);
        let v = reg.list_visible_to_tenant(other);
        assert!(v.is_empty(), "hidden backend must not appear in list");
    }

    #[test]
    fn len_reports_total_registered_backends() {
        let reg = make_registry(vec![
            (Uuid::new_v4(), false, vec![]),
            (Uuid::new_v4(), false, vec![]),
            (Uuid::new_v4(), false, vec![]),
        ]);
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn iter_yields_every_backend_pair() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let reg = make_registry(vec![(id_a, false, vec![]), (id_b, false, vec![])]);
        let ids: Vec<BackendId> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id_a));
        assert!(ids.contains(&id_b));
    }
}
