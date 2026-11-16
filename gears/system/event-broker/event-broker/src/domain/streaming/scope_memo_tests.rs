//! The memo in isolation: keys built from the requests `PolicyEnforcer`
//! sends, a fixed base instant, no runtime and no PDP call.

use std::sync::{Arc, PoisonError};
use std::thread;
use std::time::Duration;

use authz_resolver_sdk::{
    AccessRequest, BarrierMode, Capability, EvaluationRequest, PolicyEnforcer, ResourceType,
    TenantMode,
};
use serde_json::json;
use tokio::time::Instant;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner};
use crate::test_support::authz_doubles::AllowAllAuthZ;

use super::{MemoInsert, MemoKey, MemoResult, ScopeMemo};

const SUBJECT: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001);
const OTHER_SUBJECT: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0002);
const HOME: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0011);
const OTHER_HOME: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0012);
const ROOT: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0021);
const OTHER_ROOT: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0022);

const USER: &str = "gts.x.eb.test.user.v1~";
const SERVICE: &str = "gts.x.eb.test.service.v1~";
const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.orders.v1";
const OTHER_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.invoices.v1";

const PROPS: &[&str] = &["owner_tenant_id", "event_type", "event_subject_type"];
const RESOURCE: ResourceType = ResourceType::from_static("gts.x.eb.test.consume.v1~", PROPS);
const OTHER_RESOURCE: ResourceType =
    ResourceType::from_static("gts.x.eb.test.consume_group.v1~", PROPS);
const NARROW_RESOURCE: ResourceType =
    ResourceType::from_static("gts.x.eb.test.consume.v1~", &["owner_tenant_id"]);

const TTL: Duration = Duration::from_secs(60);

/// Everything one consume call varies, turned into the request the enforcer
/// would send. Plumbing only: every assert names its values inline.
#[derive(Clone)]
struct Call {
    subject_id: Uuid,
    subject_type: &'static str,
    subject_tenant: Uuid,
    token_scopes: Vec<String>,
    bearer_token: Option<&'static str>,
    capabilities: Vec<Capability>,
    resource: ResourceType,
    action: &'static str,
    properties: Vec<(&'static str, serde_json::Value)>,
    tenant_mode: TenantMode,
    root: Uuid,
    barrier_mode: BarrierMode,
    tenant_status: Option<Vec<String>>,
    require_constraints: bool,
}

impl Call {
    /// One consume scope question for one topic in one tenant context.
    fn consume() -> Self {
        Self {
            subject_id: SUBJECT,
            subject_type: USER,
            subject_tenant: HOME,
            token_scopes: vec!["events:read".to_owned()],
            bearer_token: None,
            capabilities: Vec::new(),
            resource: RESOURCE,
            action: "consume",
            properties: vec![
                ("topic_id", json!(TOPIC)),
                (pep_properties::OWNER_TENANT_ID, json!(ROOT.to_string())),
            ],
            tenant_mode: TenantMode::RootOnly,
            root: ROOT,
            barrier_mode: BarrierMode::Respect,
            tenant_status: None,
            require_constraints: true,
        }
    }

    /// The same call for a distinct topic, so `n` names a distinct key.
    fn numbered(n: usize) -> Self {
        let mut call = Self::consume();
        call.properties[0] = ("topic_id", json!(format!("{TOPIC}.{n}")));
        call
    }

    fn request(&self) -> EvaluationRequest {
        let mut ctx = SecurityContext::builder()
            .subject_id(self.subject_id)
            .subject_type(self.subject_type)
            .subject_tenant_id(self.subject_tenant)
            .token_scopes(self.token_scopes.clone());
        if let Some(token) = self.bearer_token {
            ctx = ctx.bearer_token(token);
        }
        let ctx = ctx
            .build()
            .expect("a test context has every required field");
        let enforcer = PolicyEnforcer::new(Arc::new(AllowAllAuthZ))
            .with_capabilities(self.capabilities.clone());
        let mut access = AccessRequest::new()
            .context_tenant_id(self.root)
            .tenant_mode(self.tenant_mode.clone())
            .barrier_mode(self.barrier_mode);
        if let Some(statuses) = &self.tenant_status {
            access = access.tenant_status(statuses.clone());
        }
        for (name, value) in &self.properties {
            access = access.resource_property(*name, value.clone());
        }
        enforcer.build_request_with(
            &ctx,
            &self.resource,
            self.action,
            None,
            self.require_constraints,
            &access,
        )
    }

    fn key(&self) -> MemoKey {
        MemoKey::try_from(&self.request()).expect("an enforcer request always serializes")
    }
}

fn scope() -> CompiledScope {
    CompiledScope::compile(
        &AccessScope::allow_all(),
        &TenantMode::RootOnly,
        &TenantSetInterner::new(),
    )
    .expect("an allow-all answer compiles in a root-only context")
}

fn stored_count(memo: &ScopeMemo) -> usize {
    memo.entries
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len()
}

/// Fills `memo` with `ScopeMemo::CAPACITY` distinct keys, numbered from 0,
/// each `Decision(true)` until `expires_at(n)`.
fn fill(memo: &ScopeMemo, base: Instant, expires_at: impl Fn(usize) -> Instant) {
    for n in 0..ScopeMemo::CAPACITY {
        let stored = memo.insert(
            Call::numbered(n).key(),
            MemoResult::Decision(true),
            expires_at(n),
            base,
        );
        assert_eq!(stored, MemoInsert::Stored, "key {n}");
    }
}

#[test]
fn the_key_holds_the_request_as_the_pdp_receives_it() {
    let key = Call::consume().key();

    assert_eq!(
        *key.request,
        json!({
            "subject": {
                "id": SUBJECT.to_string(),
                "type": USER,
                "properties": { "tenant_id": HOME.to_string() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": "gts.x.eb.test.consume.v1~",
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": ROOT.to_string(),
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": ROOT.to_string(),
                    "barrier_mode": "respect",
                },
                "token_scopes": ["events:read"],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "event_type", "event_subject_type"],
            },
        })
    );
}

#[test]
fn the_same_request_built_twice_gives_equal_keys() {
    let forward = Call::consume();
    let mut reversed = Call::consume();
    reversed.properties.reverse();

    let (first, second) = (forward.key(), reversed.key());

    assert_eq!(first.fingerprint, second.fingerprint);
    assert_eq!(first, second);
}

#[test]
fn each_field_the_call_sends_gives_another_key() {
    let base = Call::consume().key();
    let variants: Vec<(&str, Call)> = vec![
        (
            "subject id",
            Call {
                subject_id: OTHER_SUBJECT,
                ..Call::consume()
            },
        ),
        (
            "subject type",
            Call {
                subject_type: SERVICE,
                ..Call::consume()
            },
        ),
        (
            "subject tenant",
            Call {
                subject_tenant: OTHER_HOME,
                ..Call::consume()
            },
        ),
        (
            "token scopes",
            Call {
                token_scopes: vec!["events:read".to_owned(), "events:admin".to_owned()],
                ..Call::consume()
            },
        ),
        (
            "action",
            Call {
                action: "consume_ignore_barrier",
                ..Call::consume()
            },
        ),
        (
            "resource type",
            Call {
                resource: OTHER_RESOURCE,
                ..Call::consume()
            },
        ),
        (
            "resource property value",
            Call {
                properties: vec![
                    ("topic_id", json!(OTHER_TOPIC)),
                    (pep_properties::OWNER_TENANT_ID, json!(ROOT.to_string())),
                ],
                ..Call::consume()
            },
        ),
        (
            "added resource property",
            Call {
                properties: vec![
                    ("topic_id", json!(TOPIC)),
                    (pep_properties::OWNER_TENANT_ID, json!(ROOT.to_string())),
                    ("event_type", json!("gts.x.eb.orders.created.v1~")),
                ],
                ..Call::consume()
            },
        ),
        (
            "tenant context root",
            Call {
                root: OTHER_ROOT,
                ..Call::consume()
            },
        ),
        (
            "tenant mode",
            Call {
                tenant_mode: TenantMode::Subtree,
                ..Call::consume()
            },
        ),
        (
            "barrier_mode",
            Call {
                barrier_mode: BarrierMode::Ignore,
                ..Call::consume()
            },
        ),
        (
            "tenant status",
            Call {
                tenant_status: Some(vec!["active".to_owned()]),
                ..Call::consume()
            },
        ),
        (
            "require_constraints",
            Call {
                require_constraints: false,
                ..Call::consume()
            },
        ),
        (
            "supported properties",
            Call {
                resource: NARROW_RESOURCE,
                ..Call::consume()
            },
        ),
        (
            "capabilities",
            Call {
                capabilities: vec![Capability::TenantHierarchy],
                ..Call::consume()
            },
        ),
    ];

    for (label, call) in variants {
        let key = call.key();
        assert_ne!(key, base, "{label}");
    }
}

#[test]
fn respect_and_ignore_never_share_an_entry() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let respect = Call::consume().key();
    let ignore = Call {
        barrier_mode: BarrierMode::Ignore,
        ..Call::consume()
    }
    .key();

    let stored = memo.insert(
        respect.clone(),
        MemoResult::Decision(true),
        base + TTL,
        base,
    );

    assert_eq!(stored, MemoInsert::Stored);
    assert_eq!(memo.get(&ignore, base), None);
    assert_eq!(memo.get(&respect, base), Some(MemoResult::Decision(true)));
}

/// The bearer token never reaches the key: a cached scope serves until it
/// expires even when the caller's token expires first; decision caching tied
/// to the token is left to the `AuthZ` protocol (#5502).
#[test]
fn a_bearer_token_is_not_part_of_the_key() {
    let without = Call::consume();
    let with_a = Call {
        bearer_token: Some("token-a"),
        ..Call::consume()
    };
    let with_b = Call {
        bearer_token: Some("token-b"),
        ..Call::consume()
    };
    assert!(with_a.request().context.bearer_token.is_some());

    let key = without.key();

    assert_eq!(with_a.key(), key);
    assert_eq!(with_b.key(), key);
}

#[test]
fn an_entry_is_returned_until_it_expires() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let key = Call::consume().key();
    let scope = Arc::new(scope());

    let stored = memo.insert(
        key.clone(),
        MemoResult::Scope(Arc::clone(&scope)),
        base + TTL,
        base,
    );

    assert_eq!(stored, MemoInsert::Stored);
    assert_eq!(
        memo.get(&key, base + Duration::from_millis(59_999)),
        Some(MemoResult::Scope(scope))
    );
}

#[test]
fn an_entry_is_gone_at_its_expiry_and_is_removed() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let key = Call::consume().key();
    let stored = memo.insert(key.clone(), MemoResult::Decision(true), base + TTL, base);
    assert_eq!(stored, MemoInsert::Stored);

    assert_eq!(memo.get(&key, base + TTL), None);
    assert_eq!(stored_count(&memo), 0);
}

#[test]
fn an_entry_is_read_with_its_expiry_until_it_expires() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let key = Call::consume().key();
    let stored = memo.insert(key.clone(), MemoResult::Decision(true), base + TTL, base);
    assert_eq!(stored, MemoInsert::Stored);

    assert_eq!(
        memo.get_entry(&key, base + Duration::from_millis(59_999)),
        Some((MemoResult::Decision(true), base + TTL))
    );
    assert_eq!(memo.get_entry(&key, base + TTL), None);
    assert_eq!(stored_count(&memo), 0, "the expired entry is removed");
}

#[test]
fn an_absent_key_reads_as_none() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let stored = memo.insert(
        Call::numbered(1).key(),
        MemoResult::Decision(true),
        base + TTL,
        base,
    );
    assert_eq!(stored, MemoInsert::Stored);

    assert_eq!(memo.get(&Call::numbered(2).key(), base), None);
}

#[test]
fn a_full_memo_refuses_a_new_key() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    fill(&memo, base, |_| base + TTL);
    let extra = Call::numbered(ScopeMemo::CAPACITY).key();

    let stored = memo.insert(extra.clone(), MemoResult::Decision(true), base + TTL, base);

    assert_eq!(stored, MemoInsert::Full);
    assert_eq!(memo.get(&extra, base), None);
    assert_eq!(stored_count(&memo), ScopeMemo::CAPACITY);
}

#[test]
fn a_full_memo_drops_expired_entries_to_make_room() {
    let base = Instant::now();
    let soon = base + Duration::from_secs(10);
    let memo = ScopeMemo::new();
    fill(&memo, base, |n| {
        if [100, 500, 900].contains(&n) {
            soon
        } else {
            base + TTL
        }
    });
    let extra = Call::numbered(ScopeMemo::CAPACITY).key();

    let stored = memo.insert(extra.clone(), MemoResult::Decision(false), soon + TTL, soon);

    assert_eq!(stored, MemoInsert::Stored);
    assert_eq!(stored_count(&memo), ScopeMemo::CAPACITY - 3 + 1);
    assert_eq!(memo.get(&extra, soon), Some(MemoResult::Decision(false)));
    assert_eq!(
        memo.get(&Call::numbered(0).key(), soon),
        Some(MemoResult::Decision(true))
    );
    assert_eq!(
        memo.get(&Call::numbered(ScopeMemo::CAPACITY - 1).key(), soon),
        Some(MemoResult::Decision(true))
    );
}

#[test]
fn a_full_memo_still_replaces_an_existing_key() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    fill(&memo, base, |_| base + TTL);
    let key = Call::numbered(0).key();

    let stored = memo.insert(key.clone(), MemoResult::Decision(false), base + TTL, base);

    assert_eq!(stored, MemoInsert::Stored);
    assert_eq!(memo.get(&key, base), Some(MemoResult::Decision(false)));
    assert_eq!(stored_count(&memo), ScopeMemo::CAPACITY);
}

#[test]
fn a_scope_result_is_returned_as_the_same_allocation() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let key = Call::consume().key();
    let scope = Arc::new(scope());
    let stored = memo.insert(
        key.clone(),
        MemoResult::Scope(Arc::clone(&scope)),
        base + TTL,
        base,
    );
    assert_eq!(stored, MemoInsert::Stored);

    let Some(MemoResult::Scope(read)) = memo.get(&key, base) else {
        panic!("expected the stored scope");
    };

    assert!(Arc::ptr_eq(&read, &scope));
}

/// Eight threads race 1,600 distinct live keys into one memo. The memo has
/// no await, so the threads only contend for the lock and the test needs no
/// timeout bound.
#[test]
fn concurrent_inserts_never_pass_the_capacity() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 200;
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let batches: Vec<Vec<MemoKey>> = (0..THREADS)
        .map(|t| {
            (0..PER_THREAD)
                .map(|i| Call::numbered(t * PER_THREAD + i).key())
                .collect()
        })
        .collect();

    let outcomes: Vec<MemoInsert> = thread::scope(|s| {
        let handles: Vec<_> = batches
            .into_iter()
            .map(|keys| {
                let memo = &memo;
                s.spawn(move || {
                    keys.into_iter()
                        .map(|key| memo.insert(key, MemoResult::Decision(true), base + TTL, base))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("an inserting thread never panics"))
            .collect()
    });

    let stored = outcomes
        .iter()
        .filter(|outcome| **outcome == MemoInsert::Stored)
        .count();
    let full = outcomes
        .iter()
        .filter(|outcome| **outcome == MemoInsert::Full)
        .count();
    assert_eq!((stored, full), (1_024, 576));
    assert_eq!(stored_count(&memo), 1_024);
}

#[test]
fn a_poisoned_memo_still_serves() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let joined = thread::scope(|s| {
        s.spawn(|| {
            let _held = memo.entries.lock();
            panic!("poison the memo lock");
        })
        .join()
    });
    assert!(joined.is_err());
    assert!(memo.entries.is_poisoned());
    let key = Call::consume().key();

    let stored = memo.insert(key.clone(), MemoResult::Decision(true), base + TTL, base);

    assert_eq!(stored, MemoInsert::Stored);
    assert_eq!(memo.get(&key, base), Some(MemoResult::Decision(true)));
}

#[test]
fn two_requests_with_one_fingerprint_stay_apart() {
    let base = Instant::now();
    let memo = ScopeMemo::new();
    let first = MemoKey::with_fingerprint(7, json!({ "topic_id": "a" }));
    let second = MemoKey::with_fingerprint(7, json!({ "topic_id": "b" }));

    let stored_first = memo.insert(first.clone(), MemoResult::Decision(true), base + TTL, base);
    let stored_second = memo.insert(
        second.clone(),
        MemoResult::Decision(false),
        base + TTL,
        base,
    );

    assert_eq!(
        (stored_first, stored_second),
        (MemoInsert::Stored, MemoInsert::Stored)
    );
    assert_eq!(memo.get(&first, base), Some(MemoResult::Decision(true)));
    assert_eq!(memo.get(&second, base), Some(MemoResult::Decision(false)));
    assert_eq!(stored_count(&memo), 2);
}
