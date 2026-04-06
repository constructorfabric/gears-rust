use super::*;
use tenant_resolver_sdk::TenantStatus;
use uuid::Uuid;

fn ctx_for_tenant(tenant_id: TenantId) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id.0)
        .build()
        .unwrap()
}

const TENANT_A: &str = "11111111-1111-1111-1111-111111111111";
const TENANT_B: &str = "22222222-2222-2222-2222-222222222222";

// ==================== get_tenant tests ====================

#[tokio::test]
async fn get_tenant_returns_info_for_matching_id() {
    let service = Service;
    let tenant_id = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let ctx = ctx_for_tenant(tenant_id);

    let result = service.get_tenant(&ctx, tenant_id).await;

    assert!(result.is_ok());
    let info = result.unwrap();
    assert_eq!(info.id, tenant_id);
    assert_eq!(info.name, TENANT_NAME);
    assert_eq!(info.status, TenantStatus::Active);
    assert!(info.tenant_type.is_none());
    assert!(info.parent_id.is_none());
    assert!(!info.self_managed);
}

#[tokio::test]
async fn get_tenant_returns_error_for_different_id() {
    let service = Service;
    let ctx_tenant = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let query_tenant = TenantId(Uuid::parse_str(TENANT_B).unwrap());
    let ctx = ctx_for_tenant(ctx_tenant);

    let result = service.get_tenant(&ctx, query_tenant).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        TenantResolverError::TenantNotFound { tenant_id } => {
            assert_eq!(tenant_id, query_tenant);
        }
        other => panic!("Expected TenantNotFound, got: {other:?}"),
    }
}

#[tokio::test]
async fn get_tenant_rejects_nil_uuid() {
    let service = Service;
    let nil_id = TenantId::nil();
    let ctx = ctx_for_tenant(nil_id);

    // Even if id matches ctx.subject_tenant_id().unwrap_or_default(), nil UUID is rejected
    let result = service.get_tenant(&ctx, nil_id).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        TenantResolverError::TenantNotFound { tenant_id } => {
            assert_eq!(tenant_id, nil_id);
        }
        other => panic!("Expected TenantNotFound, got: {other:?}"),
    }
}

#[tokio::test]
async fn get_tenants_rejects_nil_uuid() {
    let service = Service;
    let nil_id = TenantId::nil();
    let ctx = ctx_for_tenant(nil_id);

    let result = service
        .get_tenants(&ctx, &[nil_id], &GetTenantsOptions::default())
        .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn get_ancestors_rejects_nil_uuid() {
    let service = Service;
    let nil_id = TenantId::nil();
    let ctx = ctx_for_tenant(nil_id);

    let result = service
        .get_ancestors(&ctx, nil_id, &GetAncestorsOptions::default())
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        TenantResolverError::TenantNotFound { .. }
    ));
}

#[tokio::test]
async fn get_descendants_rejects_nil_uuid() {
    let service = Service;
    let nil_id = TenantId::nil();
    let ctx = ctx_for_tenant(nil_id);

    let result = service
        .get_descendants(&ctx, nil_id, &GetDescendantsOptions::default())
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        TenantResolverError::TenantNotFound { .. }
    ));
}

#[tokio::test]
async fn is_ancestor_rejects_nil_uuid() {
    let service = Service;
    let nil_id = TenantId::nil();
    let ctx = ctx_for_tenant(nil_id);

    let result = service
        .is_ancestor(&ctx, nil_id, nil_id, &IsAncestorOptions::default())
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        TenantResolverError::TenantNotFound { .. }
    ));
}

// ==================== get_tenants tests ====================

#[tokio::test]
async fn get_tenants_returns_self() {
    let service = Service;
    let tenant_id = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let ctx = ctx_for_tenant(tenant_id);

    let result = service
        .get_tenants(&ctx, &[tenant_id], &GetTenantsOptions::default())
        .await;

    assert!(result.is_ok());
    let tenants = result.unwrap();
    assert_eq!(tenants.len(), 1);
    assert_eq!(tenants[0].id, tenant_id);
}

#[tokio::test]
async fn get_tenants_skips_nonexistent() {
    let service = Service;
    let ctx_tenant = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let other_tenant = TenantId(Uuid::parse_str(TENANT_B).unwrap());
    let ctx = ctx_for_tenant(ctx_tenant);

    // Request both the context tenant and a nonexistent one
    let result = service
        .get_tenants(
            &ctx,
            &[ctx_tenant, other_tenant],
            &GetTenantsOptions::default(),
        )
        .await;

    assert!(result.is_ok());
    let tenants = result.unwrap();
    // Only the context tenant is returned
    assert_eq!(tenants.len(), 1);
    assert_eq!(tenants[0].id, ctx_tenant);
}

#[tokio::test]
async fn get_tenants_with_filter() {
    let service = Service;
    let tenant_id = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let ctx = ctx_for_tenant(tenant_id);

    // Filter for suspended status (our tenant is Active)
    let opts = GetTenantsOptions {
        status: vec![TenantStatus::Suspended],
    };
    let result = service.get_tenants(&ctx, &[tenant_id], &opts).await;

    assert!(result.is_ok());
    // Filtered out because status doesn't match
    assert!(result.unwrap().is_empty());
}

// ==================== get_ancestors tests ====================

#[tokio::test]
async fn get_ancestors_returns_empty_for_self() {
    let service = Service;
    let tenant_id = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let ctx = ctx_for_tenant(tenant_id);

    let result = service
        .get_ancestors(&ctx, tenant_id, &GetAncestorsOptions::default())
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(response.tenant.id, tenant_id);
    assert!(response.ancestors.is_empty());
}

#[tokio::test]
async fn get_ancestors_error_for_different_id() {
    let service = Service;
    let ctx_tenant = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let other_tenant = TenantId(Uuid::parse_str(TENANT_B).unwrap());
    let ctx = ctx_for_tenant(ctx_tenant);

    let result = service
        .get_ancestors(&ctx, other_tenant, &GetAncestorsOptions::default())
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        TenantResolverError::TenantNotFound { tenant_id } => {
            assert_eq!(tenant_id, other_tenant);
        }
        other => panic!("Expected TenantNotFound, got: {other:?}"),
    }
}

// ==================== get_descendants tests ====================

#[tokio::test]
async fn get_descendants_returns_empty_for_self() {
    let service = Service;
    let tenant_id = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let ctx = ctx_for_tenant(tenant_id);

    let result = service
        .get_descendants(&ctx, tenant_id, &GetDescendantsOptions::default())
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(response.tenant.id, tenant_id);
    assert!(response.descendants.is_empty());
}

#[tokio::test]
async fn get_descendants_error_for_different_id() {
    let service = Service;
    let ctx_tenant = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let other_tenant = TenantId(Uuid::parse_str(TENANT_B).unwrap());
    let ctx = ctx_for_tenant(ctx_tenant);

    let result = service
        .get_descendants(&ctx, other_tenant, &GetDescendantsOptions::default())
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        TenantResolverError::TenantNotFound { tenant_id } => {
            assert_eq!(tenant_id, other_tenant);
        }
        other => panic!("Expected TenantNotFound, got: {other:?}"),
    }
}

// ==================== is_ancestor tests ====================

#[tokio::test]
async fn is_ancestor_self_returns_false() {
    let service = Service;
    let tenant_id = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let ctx = ctx_for_tenant(tenant_id);

    let result = service
        .is_ancestor(&ctx, tenant_id, tenant_id, &IsAncestorOptions::default())
        .await;

    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[tokio::test]
async fn is_ancestor_error_for_different_ancestor() {
    let service = Service;
    let ctx_tenant = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let other_tenant = TenantId(Uuid::parse_str(TENANT_B).unwrap());
    let ctx = ctx_for_tenant(ctx_tenant);

    let result = service
        .is_ancestor(
            &ctx,
            other_tenant,
            ctx_tenant,
            &IsAncestorOptions::default(),
        )
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        TenantResolverError::TenantNotFound { tenant_id } => {
            assert_eq!(tenant_id, other_tenant);
        }
        other => panic!("Expected TenantNotFound, got: {other:?}"),
    }
}

#[tokio::test]
async fn is_ancestor_error_for_different_descendant() {
    let service = Service;
    let ctx_tenant = TenantId(Uuid::parse_str(TENANT_A).unwrap());
    let other_tenant = TenantId(Uuid::parse_str(TENANT_B).unwrap());
    let ctx = ctx_for_tenant(ctx_tenant);

    let result = service
        .is_ancestor(
            &ctx,
            ctx_tenant,
            other_tenant,
            &IsAncestorOptions::default(),
        )
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        TenantResolverError::TenantNotFound { tenant_id } => {
            assert_eq!(tenant_id, other_tenant);
        }
        other => panic!("Expected TenantNotFound, got: {other:?}"),
    }
}
