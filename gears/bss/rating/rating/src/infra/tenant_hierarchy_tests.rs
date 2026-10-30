#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo,
    TenantResolverClient, TenantResolverError,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::ResolverTenantHierarchy;
use crate::domain::error::DomainError;
use crate::domain::ports::Dependency;
use crate::domain::usage::reader::TenantHierarchy;
use crate::domain::usage::source::TenantScope;

const ROOT: Uuid = Uuid::from_u128(0x1);
const OTHER_ROOT: Uuid = Uuid::from_u128(0x2);
const CHILD: Uuid = Uuid::from_u128(0x11);
const STRANGER: Uuid = Uuid::from_u128(0x99);

/// A directory of one `ROOT → CHILD` edge; `STRANGER` is unknown, and `fail` breaks every read.
#[derive(Default)]
struct Directory {
    fail: bool,
    asked: Mutex<Vec<(Uuid, Uuid, BarrierMode)>>,
}

fn not_exercised() -> TenantResolverError {
    TenantResolverError::Internal("not exercised by the hierarchy check".to_owned())
}

#[async_trait]
impl TenantResolverClient for Directory {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(not_exercised())
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(not_exercised())
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Err(not_exercised())
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        Err(not_exercised())
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        Err(not_exercised())
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        self.asked
            .lock()
            .unwrap()
            .push((ancestor_id.0, descendant_id.0, options.barrier_mode));
        if self.fail {
            return Err(TenantResolverError::ServiceUnavailable("down".to_owned()));
        }
        if descendant_id.0 == STRANGER {
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: descendant_id,
            });
        }
        Ok(ancestor_id.0 == ROOT && descendant_id.0 == CHILD)
    }
}

fn hierarchy(directory: &Arc<Directory>) -> ResolverTenantHierarchy {
    ResolverTenantHierarchy::new(Arc::clone(directory) as Arc<dyn TenantResolverClient>)
}

#[tokio::test]
async fn a_root_or_its_descendant_is_within_the_scope() {
    let directory = Arc::new(Directory::default());
    let scope = TenantScope::new([OTHER_ROOT, ROOT]).unwrap();
    assert!(hierarchy(&directory).within(&scope, ROOT).await.unwrap());
    assert!(
        directory.asked.lock().unwrap().is_empty(),
        "a root needs no question"
    );
    assert!(hierarchy(&directory).within(&scope, CHILD).await.unwrap());
    let asked = directory.asked.lock().unwrap().clone();
    assert!(
        asked
            .iter()
            .all(|(_, _, mode)| *mode == BarrierMode::Ignore),
        "a self-managed tenant still lies in its parent's subtree: {asked:?}"
    );
}

#[tokio::test]
async fn a_tenant_under_no_root_or_unknown_to_the_directory_is_outside() {
    let directory = Arc::new(Directory::default());
    let scope = TenantScope::new([OTHER_ROOT]).unwrap();
    assert!(!hierarchy(&directory).within(&scope, CHILD).await.unwrap());
    assert!(
        !hierarchy(&directory)
            .within(&scope, STRANGER)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_directory_that_cannot_answer_is_an_unavailable_dependency() {
    let directory = Arc::new(Directory {
        fail: true,
        ..Directory::default()
    });
    let scope = TenantScope::new([ROOT]).unwrap();
    let error = hierarchy(&directory)
        .within(&scope, CHILD)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            DomainError::DependencyUnavailable {
                dependency: Dependency::TenantHierarchy,
                ..
            }
        ),
        "{error:?}"
    );
}
