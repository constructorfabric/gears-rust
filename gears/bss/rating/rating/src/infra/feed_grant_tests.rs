#![allow(clippy::unwrap_used, clippy::expect_used)]

use uuid::Uuid;

use super::{COLLECTOR_USAGE_RECORD, PdpFeedGrant, READ_FEED};
use crate::domain::error::DomainError;
use crate::domain::usage::feed::{FeedSubscription, MeterId};
use crate::domain::usage::reader::FeedGrant;
use crate::domain::usage::source::{SourceDefinition, TenantScope};
use crate::test_providers::FakePdp;

const T1: Uuid = Uuid::from_u128(0x10);
const T2: Uuid = Uuid::from_u128(0x20);

fn source() -> SourceDefinition {
    SourceDefinition::new(
        FeedSubscription::new([
            MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.test.a.v1~").unwrap(),
        ])
        .unwrap(),
        TenantScope::new([T1, T2]).unwrap(),
    )
}

#[tokio::test]
async fn the_grant_covers_the_scope_when_every_root_may_read_the_feed() {
    let pdp = FakePdp::new();
    let grant = PdpFeedGrant::new(pdp.enforcer());
    assert!(grant.covers(&source()).await.unwrap());
    // One question per root, asked as Rating's service identity in that root.
    let tenants: Vec<_> = pdp.subjects().iter().map(|s| s.tenant_id).collect();
    assert_eq!(tenants, vec![Some(T1), Some(T2)]);
}

#[tokio::test]
async fn a_root_outside_the_grant_or_a_denied_feed_read_narrows_it() {
    let pdp = FakePdp::new();
    let grant = PdpFeedGrant::new(pdp.enforcer());
    pdp.put_outside_grant(T2);
    assert!(!grant.covers(&source()).await.unwrap());
    pdp.bring_inside_grant(T2);
    pdp.deny(COLLECTOR_USAGE_RECORD.name(), READ_FEED);
    assert!(!grant.covers(&source()).await.unwrap());
    pdp.allow(COLLECTOR_USAGE_RECORD.name(), READ_FEED);
    assert!(grant.covers(&source()).await.unwrap());
}

#[tokio::test]
async fn an_unreachable_pdp_is_an_outage_not_a_narrowing() {
    let pdp = FakePdp::new();
    pdp.set_unavailable(true);
    let error = PdpFeedGrant::new(pdp.enforcer())
        .covers(&source())
        .await
        .unwrap_err();
    assert!(matches!(error, DomainError::AuthzUnavailable(..)));
}
