#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use file_storage_sdk::{
        Backend, BackendCapability, BackendId, BackendKind, BackendTransport, FileMeta, UrlParams,
    };
    use uuid::Uuid;

    use crate::domain::error::DomainError;
    use crate::infra::backends::r#trait::{
        BackendDescriptor, BackendObjectKey, BackendReadResult, HeadResult, PresignedGetItem,
        PresignedGetOutcome, SharedBackend, StorageBackend,
    };
    use crate::infra::backends::smoke::run_smoke_tests;

    struct FakeBackend {
        descriptor: BackendDescriptor,
        fail_presign_put: bool,
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
            unimplemented!("not exercised in smoke tests")
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
            if self.fail_presign_put {
                Err(DomainError::backend("creds invalid"))
            } else {
                Ok("https://example/u".to_owned())
            }
        }
        async fn issue_presigned_gets(
            &self,
            _items: Vec<PresignedGetItem>,
        ) -> Result<Vec<PresignedGetOutcome>, DomainError> {
            Ok(vec![])
        }
        async fn head_object(&self, _key: &BackendObjectKey) -> Result<HeadResult, DomainError> {
            Ok(HeadResult {
                content_hash: "hash".to_owned(),
                size_bytes: 0,
            })
        }
    }

    fn descriptor_with_caps(id: BackendId, caps: Vec<BackendCapability>) -> BackendDescriptor {
        BackendDescriptor {
            sdk: Backend {
                id,
                kind: BackendKind::S3Compatible,
                default_public: false,
                default_private: true,
                transport: BackendTransport::Redirect,
                capabilities: caps,
                max_file_size_bytes: None,
            },
            max_signed_url_ttl_seconds_value: 3600,
            tenant_access: vec![],
        }
    }

    fn make_pair(caps: Vec<BackendCapability>, fail: bool) -> (BackendId, SharedBackend) {
        let id = Uuid::new_v4();
        let b: SharedBackend = Arc::new(FakeBackend {
            descriptor: descriptor_with_caps(id, caps),
            fail_presign_put: fail,
        });
        (id, b)
    }

    #[tokio::test]
    async fn smoke_runs_only_against_backends_declaring_conditional_put() {
        let pair = make_pair(vec![BackendCapability::PresignedUrls], false);
        let pairs: Vec<(BackendId, &SharedBackend)> = vec![(pair.0, &pair.1)];
        let result = run_smoke_tests(&pairs).await;
        assert!(result.is_ok(), "no-cap backends must be skipped: {result:?}");
    }

    #[tokio::test]
    async fn smoke_runs_against_conditional_put_backend_and_succeeds_on_happy_path() {
        let pair = make_pair(
            vec![
                BackendCapability::PresignedUrls,
                BackendCapability::PresignedConditionalPut,
            ],
            false,
        );
        let pairs: Vec<(BackendId, &SharedBackend)> = vec![(pair.0, &pair.1)];
        let result = run_smoke_tests(&pairs).await;
        assert!(result.is_ok(), "happy path must pass: {result:?}");
    }

    #[tokio::test]
    async fn smoke_fails_when_presign_put_returns_error() {
        let pair = make_pair(
            vec![
                BackendCapability::PresignedUrls,
                BackendCapability::PresignedConditionalPut,
            ],
            true,
        );
        let pairs: Vec<(BackendId, &SharedBackend)> = vec![(pair.0, &pair.1)];
        let result = run_smoke_tests(&pairs).await;
        match result {
            Err(crate::errors::InitError::SmokeTestFailed { step, .. }) => {
                assert_eq!(step, "step1-presign");
            }
            Err(other) => panic!("expected SmokeTestFailed, got: {other:?}"),
            Ok(()) => panic!("smoke must fail when backend rejects presign-put"),
        }
    }

    #[tokio::test]
    async fn smoke_succeeds_when_no_backends() {
        let pairs: Vec<(BackendId, &SharedBackend)> = vec![];
        assert!(run_smoke_tests(&pairs).await.is_ok());
    }
}
