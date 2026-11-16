#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use github_mirror::domain::error::DomainError;
use github_mirror::domain::repo::{IssueRecord, ListingFilter, PageWindow, RepoRecord};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_odata::ODataQuery;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

struct EmptyScopeResolver;

#[async_trait]
impl AuthZResolverApi for EmptyScopeResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        Vec::<Uuid>::new(),
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

async fn seeded_service(
    enforcer: PolicyEnforcer,
) -> (Arc<common::ConcreteService>, SecurityContext) {
    let db = common::inmem_db().await;
    let seed = common::service_over(db.clone(), "https://api.github.com");
    let ctx = common::caller();
    seed.upsert_repo(
        &ctx,
        RepoRecord {
            node_id: None,
            id: 900,
            owner: "acme".to_owned(),
            name: "widget".to_owned(),
            full_name: "acme/widget".to_owned(),
            default_branch: "main".to_owned(),
            private: true,
            pushed_at: None,
            stars: 0,
            forks: 0,
            description: None,
            clone_url: None,
        },
    )
    .await
    .expect("seed repository");
    seed.upsert_issue(
        &ctx,
        "acme",
        "widget",
        IssueRecord {
            author_login: None,
            author_json: None,
            assignees_json: None,
            labels_json: None,
            comments_count: None,
            locked: None,
            node_id: None,
            id: 1,
            repo_id: 900,
            number: 1,
            title: "must remain hidden".to_owned(),
            body: None,
            state: "open".to_owned(),
            is_pull_request: false,
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            closed_at: None,
            html_url: None,
        },
    )
    .await
    .expect("seed issue");
    (
        common::service_with_enforcer(
            db,
            "https://api.github.com",
            Arc::new(common::FakeGithub { result: None }),
            enforcer,
        ),
        ctx,
    )
}

#[tokio::test]
async fn permitted_empty_scope_returns_empty_repo_page() {
    let (service, ctx) = seeded_service(PolicyEnforcer::new(Arc::new(EmptyScopeResolver))).await;
    let page = service
        .list_repos(&ctx, &ODataQuery::default())
        .await
        .expect("a permitted empty scope is an empty collection");
    assert!(page.items.is_empty());
    assert!(page.page_info.next_cursor.is_none());
}

#[tokio::test]
async fn permitted_empty_scope_returns_empty_numbered_repo_page() {
    let (service, ctx) = seeded_service(PolicyEnforcer::new(Arc::new(EmptyScopeResolver))).await;
    let items = service
        .list_repos_page(&ctx, PageWindow::first(25))
        .await
        .expect("a permitted empty scope is an empty collection");
    assert!(items.is_empty());
}

#[tokio::test]
async fn empty_scope_still_refuses_nested_issue_list_parent_gate() {
    let (service, ctx) = seeded_service(PolicyEnforcer::new(Arc::new(EmptyScopeResolver))).await;
    let error = service
        .list_issues(
            &ctx,
            "acme",
            "widget",
            PageWindow::first(25),
            ListingFilter::default(),
        )
        .await
        .expect_err("nested lists retain the parent-resource authorization gate");
    assert!(matches!(error, DomainError::Forbidden(_)), "got {error:?}");
}

#[tokio::test]
async fn explicit_pdp_denial_still_refuses_list() {
    let (service, ctx) = seeded_service(common::deny_enforcer()).await;
    let error = service
        .list_repos(&ctx, &ODataQuery::default())
        .await
        .expect_err("explicit denial must not become an empty collection");
    assert!(matches!(error, DomainError::Forbidden(_)), "got {error:?}");
}
