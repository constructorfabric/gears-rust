//! Shared contract checks for [`super::MockTypesRegistry`] and the registry's local client.
//! [`run`] requires a fresh, empty client and seeds it through the contract.

use std::collections::BTreeSet;
use std::num::NonZeroU8;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use toolkit_security::PlatformSecurityContext;

use crate::contract::PlatformTypesRegistryApi;
use crate::field;
use crate::item_failure::AdmissionFailure;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, CandidateStatus, DeleteEntitiesRequest, DeleteItem,
    DeletionItemResult, DeletionOutcome, Entity, EntityField, EntityFilter, EntityKey, EntityKind,
    EntityLookup, FieldSelection, IdempotencyKey, LifecycleFilter, LifecycleStatus,
    ListEntitiesRequest, Operation, OperationStatus, Origin, PageRequest, Projection, Provenance,
    PublisherContext, RegisterEntitiesRequest, RegisterItem, RegistrationOperation,
    RegistrationOutcome, Validator,
};

const DRAFT: &str = "http://json-schema.org/draft-07/schema#";

const BASE: &str = "gts.cf.core.conformance.base.v1~";
const CHILD: &str = "gts.cf.core.conformance.base.v1~cf.core.conformance.child.v1~";
const DOOMED: &str = "gts.cf.core.conformance.base.v1~cf.core.conformance.doomed.v1~";
const INSTANCE: &str = "gts.cf.core.conformance.base.v1~cf.core.conformance.item.v1";
/// Never registered.
const ABSENT: &str = "gts.cf.core.conformance.absent.v1~";

const LISTED: &str = "gts.cf.core.listing.*";
const LISTED_A: &str = "gts.cf.core.listing.a.v1~";
const LISTED_B: &str = "gts.cf.core.listing.b.v1~";
const LISTED_CHAINED: &str = "gts.cf.core.listing.a.v1~cf.core.listing.c.v1~";

/// How long an accepted operation may take to complete.
const COMPLETION: Duration = Duration::from_secs(30);

/// Runs every check against `client`, which must start empty.
///
/// # Panics
/// On the first violation of the contract.
pub async fn run(client: &dyn PlatformTypesRegistryApi) {
    let c = Checker {
        client,
        keys: AtomicU32::new(0),
    };
    c.documents_resolve_through_the_chain().await;
    c.a_reference_an_instance_and_a_light_read_answer_their_fields()
        .await;
    c.validators_are_scoped_to_the_projection().await;
    c.a_repeated_key_is_answered_under_its_first_mention().await;
    let (tombstone, etag) = c
        .tombstones_keep_their_documents_and_are_never_rewritten()
        .await;
    c.a_parent_change_moves_its_children_but_not_tombstones(tombstone, etag)
        .await;
    c.discovery_applies_its_filters().await;
    c.a_cursor_resumes_only_the_query_that_issued_it().await;
    c.requests_out_of_range_are_refused().await;
    c.an_idempotency_key_binds_one_request().await;
    c.dry_runs_preview_without_allocating_versions().await;
}

// fixtures.

fn uri(gts_id: &str) -> String {
    format!("gts://{gts_id}")
}

fn gid(gts_id: &str) -> gts::GtsId {
    gts::GtsId::try_new(gts_id).expect("a valid fixture identifier")
}

fn gts_key(gts_id: &str) -> EntityKey {
    EntityKey::GtsId(gid(gts_id))
}

/// A root Type Schema; `traits` is its traits schema's properties, if any.
fn root(gts_id: &str, traits: Option<Value>) -> Value {
    let mut schema = json!({
        "$id": uri(gts_id),
        "$schema": DRAFT,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    });
    if let Some(properties) = traits {
        schema["x-gts-traits-schema"] = json!({ "type": "object", "properties": properties });
    }
    schema
}

/// A Type Schema derived from `parent`, setting `traits` if any.
fn derived(gts_id: &str, parent: &str, traits: Option<Value>) -> Value {
    let mut schema = json!({
        "$id": uri(gts_id),
        "$schema": DRAFT,
        "allOf": [{ "$ref": uri(parent) }],
    });
    if let Some(traits) = traits {
        schema["x-gts-traits"] = traits;
    }
    schema
}

fn base_traits() -> Value {
    json!({
        "tier": { "type": "string", "default": "basic" },
        "limit": { "type": "integer", "default": 10 },
    })
}

/// The base after a compatible revision: one more trait, with a default.
fn revised_base_traits() -> Value {
    let mut traits = base_traits();
    traits["region"] = json!({ "type": "string", "default": "eu" });
    traits
}

fn create(gts_id: &str, content: Value) -> RegisterItem {
    RegisterItem {
        gts_id: gid(gts_id),
        content,
        expected_resource_version: None,
        skip_compatibility_check: false,
    }
}

fn revise(gts_id: &str, content: Value, version: u64) -> RegisterItem {
    RegisterItem {
        expected_resource_version: Some(version),
        ..create(gts_id, content)
    }
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "conformance".to_owned(),
        version: "1.0.0".parse().expect("a valid version"),
    }
}

fn every_field() -> Projection {
    Projection::Select(FieldSelection::with(&EntityField::ALL))
}

fn light() -> Projection {
    Projection::Select(FieldSelection::light())
}

fn content_only() -> Projection {
    Projection::Select(FieldSelection::with(&[EntityField::Content]))
}

fn reason(error: Option<&CanonicalError>) -> String {
    let error = error.expect("a failed item carries its error");
    AdmissionFailure::from_canonical(error)
        .map_or_else(|| format!("{error:?}"), |f| f.reason.as_wire().to_owned())
}

fn assert_field_violation(error: &CanonicalError, field: &str) {
    let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        ..
    } = error
    else {
        panic!("expected a field violation on {field}: {error:?}");
    };
    assert_eq!(field_violations[0].field, field, "{error:?}");
}

/// Awaits `future` within [`COMPLETION`].
async fn bounded<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(COMPLETION, future)
        .await
        .unwrap_or_else(|_| panic!("{what}: no answer within {COMPLETION:?}"))
}

/// Whether any node of `document` satisfies `test`.
fn any_node(document: &Value, test: &dyn Fn(&Value) -> bool) -> bool {
    test(document)
        || match document {
            Value::Array(items) => items.iter().any(|v| any_node(v, test)),
            Value::Object(fields) => fields.values().any(|v| any_node(v, test)),
            _ => false,
        }
}

/// Whether some `properties` object of `document` declares every one of `names`.
fn declares(document: Option<&Value>, names: &[&str]) -> bool {
    document.is_some_and(|d| {
        any_node(d, &|node| {
            node.get("properties")
                .and_then(Value::as_object)
                .is_some_and(|p| names.iter().all(|n| p.contains_key(*n)))
        })
    })
}

/// Whether `document` still names another document by a `gts://` reference.
fn references(document: &Value) -> bool {
    any_node(document, &|node| {
        node.get("$ref")
            .and_then(Value::as_str)
            .is_some_and(|r| r.starts_with("gts://"))
    })
}

fn found(lookup: EntityLookup) -> (Entity, Validator) {
    match lookup {
        EntityLookup::Found { entity, etag } => (*entity, etag),
        other => panic!("expected Found, got {other:?}"),
    }
}

// the client.

struct Checker<'a> {
    client: &'a dyn PlatformTypesRegistryApi,
    keys: AtomicU32,
}

impl Checker<'_> {
    fn ctx() -> PlatformSecurityContext {
        PlatformSecurityContext::outbound_marker()
    }

    fn next_key(&self) -> IdempotencyKey {
        let n = self.keys.fetch_add(1, Ordering::SeqCst);
        IdempotencyKey::new(format!("conformance-{n}")).expect("a valid key")
    }

    async fn completed(&self, operation_id: uuid::Uuid) -> Operation {
        bounded("the operation completes", async {
            loop {
                let operation = self
                    .client
                    .get_operation(&Self::ctx(), operation_id)
                    .await
                    .expect("the operation reads");
                if operation.status() == OperationStatus::Completed {
                    return operation;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
    }

    /// Submits and awaits one registration; answers its items' statuses and failure reasons.
    async fn try_register(&self, items: Vec<RegisterItem>) -> Vec<(CandidateStatus, String)> {
        let mut submitted: Vec<String> = items.iter().map(|i| i.gts_id.to_string()).collect();
        let request = RegisterEntitiesRequest {
            items,
            dry_run: false,
            publisher: publisher(),
        };
        let accepted = bounded(
            "the registration is answered",
            self.client
                .register_entities(&Self::ctx(), self.next_key(), request),
        )
        .await
        .expect("the registration is accepted");
        let Operation::Registration(operation) = self.completed(accepted.operation_id).await else {
            panic!("a registration completes as a registration");
        };
        let mut answered: Vec<String> = operation
            .items
            .iter()
            .map(|i| i.gts_id.to_string())
            .collect();
        submitted.sort();
        answered.sort();
        assert_eq!(answered, submitted, "one result for every submitted item");
        operation
            .items
            .into_iter()
            .map(|item| {
                let failure = item
                    .outcome
                    .error()
                    .map_or_else(String::new, |e| reason(Some(e)));
                (item.outcome.status(), failure)
            })
            .collect()
    }

    async fn register(&self, items: Vec<RegisterItem>) {
        let ids: Vec<String> = items.iter().map(|i| i.gts_id.to_string()).collect();
        let outcomes = self.try_register(items).await;
        assert!(
            outcomes
                .iter()
                .all(|(status, _)| *status == CandidateStatus::Succeeded),
            "registering {ids:?}: {outcomes:?}"
        );
    }

    /// Submits and awaits one deletion of `gts_id` at `version`; answers its status and
    /// failure reason.
    async fn try_delete(&self, gts_id: &str, version: u64) -> (CandidateStatus, String) {
        let request = DeleteEntitiesRequest {
            items: vec![DeleteItem {
                key: gts_key(gts_id),
                expected_resource_version: version,
            }],
            dry_run: false,
            publisher: publisher(),
        };
        let accepted = bounded(
            "the deletion is answered",
            self.client
                .delete_entities(&Self::ctx(), self.next_key(), request),
        )
        .await
        .expect("the deletion is accepted");
        let Operation::Deletion(operation) = self.completed(accepted.operation_id).await else {
            panic!("a deletion completes as a deletion");
        };
        let [item] = operation.items.as_slice() else {
            panic!("one result for the deleted key: {:?}", operation.items);
        };
        assert_eq!(item.entity_key, gts_key(gts_id), "the result names the key");
        let failure = item
            .outcome
            .error()
            .map_or_else(String::new, |e| reason(Some(e)));
        (item.outcome.status(), failure)
    }

    async fn delete(&self, gts_id: &str, version: u64) {
        let outcome = self.try_delete(gts_id, version).await;
        assert_eq!(
            outcome,
            (CandidateStatus::Succeeded, String::new()),
            "deleting {gts_id}"
        );
    }

    async fn read(&self, items: Vec<BatchGetItem>, projection: Projection) -> EntityLookup {
        let asked = items[0].key.clone();
        let mut answers = self
            .client
            .batch_get_entities(
                &Self::ctx(),
                BatchGetEntitiesRequest {
                    items,
                    projection,
                    fresh: false,
                },
            )
            .await
            .expect("the batch reads")
            .0;
        assert_eq!(answers.len(), 1, "one key, one answer");
        answers.remove(&asked).expect("the key is answered")
    }

    async fn get(&self, key: EntityKey, projection: Projection) -> EntityLookup {
        self.read(vec![key.into()], projection).await
    }

    async fn get_if(
        &self,
        key: EntityKey,
        etag: &Validator,
        projection: Projection,
    ) -> EntityLookup {
        let item = BatchGetItem {
            key,
            if_none_match: Some(etag.clone()),
        };
        self.read(vec![item], projection).await
    }

    async fn listed(&self, filter: EntityFilter) -> BTreeSet<String> {
        let filter = EntityFilter {
            pattern: Some(gts::GtsIdPattern::try_new(LISTED).expect("a valid pattern")),
            ..filter
        };
        self.client
            .list_entities(
                &Self::ctx(),
                ListEntitiesRequest {
                    filter,
                    ..ListEntitiesRequest::default()
                },
            )
            .await
            .expect("lists")
            .items
            .into_iter()
            .map(|e| e.gts_id.to_string())
            .collect()
    }

    // checks.

    async fn documents_resolve_through_the_chain(&self) {
        self.register(vec![create(BASE, root(BASE, Some(base_traits())))])
            .await;
        self.register(vec![
            create(CHILD, derived(CHILD, BASE, Some(json!({ "tier": "gold" })))),
            create(
                DOOMED,
                derived(DOOMED, BASE, Some(json!({ "tier": "silver" }))),
            ),
            create(INSTANCE, json!({ "name": "item" })),
        ])
        .await;

        let (child, _) = found(self.get(gts_key(CHILD), every_field()).await);
        assert_eq!(child.kind, EntityKind::TypeSchema);
        assert_eq!(child.lifecycle_status, LifecycleStatus::Active);
        assert_eq!(
            child.content,
            Some(derived(CHILD, BASE, Some(json!({ "tier": "gold" })))),
            "the content is the submitted document"
        );
        assert!(
            matches!(
                child.origin,
                Some(Origin::Managed {
                    resource_version: 1,
                    ..
                })
            ),
            "{:?}",
            child.origin
        );
        assert_eq!(
            child.effective_traits,
            Some(json!({ "tier": "gold", "limit": 10 })),
            "traits merge down the chain and defaults fill the rest"
        );
        let resolved = child.resolved_schema.as_ref().expect("selected");
        assert!(
            declares(Some(resolved), &["name"]) && !references(resolved),
            "the resolved schema inlines the parent's body: {resolved}"
        );
        assert!(
            declares(child.effective_traits_schema.as_ref(), &["tier", "limit"]),
            "the effective traits schema is the chain's: {:?}",
            child.effective_traits_schema
        );
        assert_eq!(
            child.provenance,
            Some(Provenance {
                compatibility_check_skipped: false
            }),
            "a selected unwaived Type Schema reports false, not an absence"
        );
    }

    async fn a_reference_an_instance_and_a_light_read_answer_their_fields(&self) {
        let (child, _) = found(self.get(gts_key(CHILD), every_field()).await);
        let (by_uuid, _) = found(
            self.get(EntityKey::GtsUuid(gid(CHILD).to_uuid()), every_field())
                .await,
        );
        assert_eq!(by_uuid, child, "a Registry Reference reads the same entity");

        let (instance, _) = found(self.get(gts_key(INSTANCE), every_field()).await);
        assert_eq!(instance.kind, EntityKind::Instance);
        assert_eq!(instance.content, Some(json!({ "name": "item" })));
        assert_eq!(
            (
                &instance.resolved_schema,
                &instance.effective_traits,
                &instance.effective_traits_schema
            ),
            (&None, &None, &None),
            "an Instance has no documents, even when selected"
        );
        assert_eq!(
            instance.provenance, None,
            "an Instance has no provenance, even when selected"
        );

        let (light, _) = found(self.get(gts_key(CHILD), light()).await);
        assert_eq!(
            (&light.content, &light.effective_traits, &light.origin),
            (&None, &None, &None),
            "a light read carries only the mandatory fields"
        );
    }

    async fn validators_are_scoped_to_the_projection(&self) {
        let (_, etag) = found(self.get(gts_key(CHILD), Projection::Default).await);
        assert!(
            matches!(
                self.get_if(gts_key(CHILD), &etag, Projection::Default)
                    .await,
                EntityLookup::Unchanged { .. }
            ),
            "a matching validator transfers nothing"
        );
        assert!(
            matches!(
                self.get_if(gts_key(CHILD), &etag, content_only()).await,
                EntityLookup::Found { .. }
            ),
            "a validator read under one projection does not condition another"
        );
    }

    async fn a_repeated_key_is_answered_under_its_first_mention(&self) {
        let (_, etag) = found(self.get(gts_key(CHILD), Projection::Default).await);
        let conditional = BatchGetItem {
            key: gts_key(CHILD),
            if_none_match: Some(etag),
        };
        let lookup = self
            .read(
                vec![gts_key(CHILD).into(), conditional.clone()],
                Projection::Default,
            )
            .await;
        assert!(matches!(lookup, EntityLookup::Found { .. }), "{lookup:?}");
        let lookup = self
            .read(
                vec![conditional, gts_key(CHILD).into()],
                Projection::Default,
            )
            .await;
        assert!(
            matches!(lookup, EntityLookup::Unchanged { .. }),
            "{lookup:?}"
        );
    }

    async fn tombstones_keep_their_documents_and_are_never_rewritten(&self) -> (Entity, Validator) {
        self.delete(DOOMED, 1).await;
        let (tombstone, tombstone_etag) = found(self.get(gts_key(DOOMED), every_field()).await);
        assert_eq!(tombstone.lifecycle_status, LifecycleStatus::Deleted);
        assert_eq!(
            tombstone.effective_traits,
            Some(json!({ "tier": "silver", "limit": 10 })),
            "a tombstone keeps its documents"
        );

        let doomed = || derived(DOOMED, BASE, Some(json!({ "tier": "silver" })));
        assert_eq!(
            self.try_register(vec![create(DOOMED, doomed())]).await,
            [(CandidateStatus::Failed, "already_exists".to_owned())],
            "a tombstone is never recreated"
        );
        assert_eq!(
            self.try_register(vec![revise(DOOMED, doomed(), 2)]).await,
            [(CandidateStatus::Failed, "entity_deleted".to_owned())],
            "a tombstone is never revised"
        );

        let Some(Origin::Managed {
            resource_version: tombstone_version,
            ..
        }) = tombstone.origin
        else {
            panic!("a tombstone keeps its origin: {:?}", tombstone.origin);
        };
        for version in [tombstone_version, 1] {
            assert_eq!(
                self.try_delete(DOOMED, version).await,
                (CandidateStatus::Failed, "not_active".to_owned()),
                "a tombstone is not deleted again, whatever version is expected ({version})"
            );
        }
        assert_eq!(
            self.try_delete(ABSENT, 1).await,
            (CandidateStatus::Failed, "precondition_failed".to_owned()),
            "an absent target fails its precondition"
        );
        (tombstone, tombstone_etag)
    }

    async fn a_parent_change_moves_its_children_but_not_tombstones(
        &self,
        tombstone: Entity,
        tombstone_etag: Validator,
    ) {
        let (_, child_etag) = found(self.get(gts_key(CHILD), light()).await);
        self.register(vec![revise(
            BASE,
            root(BASE, Some(revised_base_traits())),
            1,
        )])
        .await;

        let (child, _) = found(
            self.get_if(gts_key(CHILD), &child_etag, every_field())
                .await,
        );
        assert_eq!(
            child.effective_traits,
            Some(json!({ "tier": "gold", "limit": 10, "region": "eu" })),
            "a parent's revision re-resolves its children"
        );
        assert!(
            declares(
                child.effective_traits_schema.as_ref(),
                &["tier", "limit", "region"]
            ),
            "and their effective traits schema: {:?}",
            child.effective_traits_schema
        );
        assert!(
            matches!(
                self.get_if(gts_key(CHILD), &child_etag, light()).await,
                EntityLookup::Found { .. }
            ),
            "and moves their validators, even under a projection without documents"
        );

        assert!(
            matches!(
                self.get_if(gts_key(DOOMED), &tombstone_etag, every_field())
                    .await,
                EntityLookup::Unchanged { .. }
            ),
            "a parent's revision leaves a tombstone's validator alone"
        );
        let (unchanged, _) = found(self.get(gts_key(DOOMED), every_field()).await);
        assert_eq!(unchanged, tombstone, "and its documents");
    }

    async fn discovery_applies_its_filters(&self) {
        self.register(vec![
            create(LISTED_A, root(LISTED_A, None)),
            create(LISTED_B, root(LISTED_B, None)),
        ])
        .await;
        self.register(vec![create(
            LISTED_CHAINED,
            derived(LISTED_CHAINED, LISTED_A, None),
        )])
        .await;
        self.delete(LISTED_B, 1).await;

        let set = |ids: &[&str]| ids.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>();
        let lifecycle = |lifecycle| EntityFilter {
            lifecycle,
            ..EntityFilter::default()
        };
        assert_eq!(
            self.listed(lifecycle(LifecycleFilter::Active)).await,
            set(&[LISTED_A, LISTED_CHAINED])
        );
        assert_eq!(
            self.listed(lifecycle(LifecycleFilter::Deleted)).await,
            set(&[LISTED_B])
        );
        assert_eq!(
            self.listed(lifecycle(LifecycleFilter::All)).await,
            set(&[LISTED_A, LISTED_B, LISTED_CHAINED])
        );

        let depth = |d| EntityFilter {
            max_chain_depth: NonZeroU8::new(d),
            ..EntityFilter::default()
        };
        assert_eq!(self.listed(depth(1)).await, set(&[LISTED_A]));
        assert_eq!(
            self.listed(depth(2)).await,
            set(&[LISTED_A, LISTED_CHAINED])
        );

        let kind = EntityFilter {
            kind: Some(EntityKind::Instance),
            ..EntityFilter::default()
        };
        assert_eq!(self.listed(kind).await, set(&[]));
    }

    async fn a_cursor_resumes_only_the_query_that_issued_it(&self) {
        let first = ListEntitiesRequest {
            filter: EntityFilter {
                pattern: Some(gts::GtsIdPattern::try_new(LISTED).expect("a valid pattern")),
                ..EntityFilter::default()
            },
            page: PageRequest {
                limit: Some(1),
                cursor: None,
            },
            ..ListEntitiesRequest::default()
        };
        let page = self
            .client
            .list_entities(&Self::ctx(), first.clone())
            .await
            .expect("lists");
        let cursor = page.next.expect("a second page");
        let resumed = |mut request: ListEntitiesRequest| {
            request.page.cursor = Some(cursor.clone());
            request
        };

        let next = self
            .client
            .list_entities(&Self::ctx(), resumed(first.clone()))
            .await
            .expect("the same query resumes");
        assert_ne!(
            next.items[0].gts_id, page.items[0].gts_id,
            "a resumed query moves on"
        );

        let refiltered = |filter: EntityFilter| ListEntitiesRequest {
            filter: EntityFilter {
                pattern: first.filter.pattern.clone(),
                ..filter
            },
            ..first.clone()
        };
        for changed in [
            ListEntitiesRequest {
                projection: content_only(),
                ..first.clone()
            },
            ListEntitiesRequest {
                filter: EntityFilter {
                    pattern: Some(
                        gts::GtsIdPattern::try_new("gts.cf.core.listing.a.*")
                            .expect("a valid pattern"),
                    ),
                    ..EntityFilter::default()
                },
                ..first.clone()
            },
            refiltered(EntityFilter {
                kind: Some(EntityKind::TypeSchema),
                ..EntityFilter::default()
            }),
            refiltered(EntityFilter {
                lifecycle: LifecycleFilter::All,
                ..EntityFilter::default()
            }),
            refiltered(EntityFilter {
                max_chain_depth: NonZeroU8::new(1),
                ..EntityFilter::default()
            }),
        ] {
            let error = self
                .client
                .list_entities(&Self::ctx(), resumed(changed.clone()))
                .await
                .expect_err("a changed query cannot resume");
            assert_field_violation(&error, field::CURSOR_FIELD);
        }

        let wider = ListEntitiesRequest {
            page: PageRequest {
                limit: Some(10),
                cursor: None,
            },
            ..first
        };
        self.client
            .list_entities(&Self::ctx(), resumed(wider))
            .await
            .expect("the page size is not part of the binding");
    }

    async fn requests_out_of_range_are_refused(&self) {
        for limit in [0, 101] {
            let error = self
                .client
                .list_entities(
                    &Self::ctx(),
                    ListEntitiesRequest {
                        page: PageRequest {
                            limit: Some(limit),
                            cursor: None,
                        },
                        ..ListEntitiesRequest::default()
                    },
                )
                .await
                .expect_err("a page size out of range is refused");
            assert_field_violation(&error, field::LIMIT_FIELD);
        }

        let error = self
            .client
            .register_entities(
                &Self::ctx(),
                self.next_key(),
                RegisterEntitiesRequest {
                    items: Vec::new(),
                    dry_run: false,
                    publisher: publisher(),
                },
            )
            .await
            .expect_err("an empty registration is refused");
        assert_field_violation(&error, field::ITEMS_FIELD);

        let error = self
            .client
            .delete_entities(
                &Self::ctx(),
                self.next_key(),
                DeleteEntitiesRequest {
                    items: Vec::new(),
                    dry_run: false,
                    publisher: publisher(),
                },
            )
            .await
            .expect_err("an empty deletion is refused");
        assert_field_violation(&error, field::ITEMS_FIELD);
    }

    async fn an_idempotency_key_binds_one_request(&self) {
        const KEYED: &str = "gts.cf.core.conformance.keyed.v1~";
        let request = |publisher: PublisherContext| RegisterEntitiesRequest {
            items: vec![create(KEYED, root(KEYED, None))],
            dry_run: false,
            publisher,
        };
        let key = self.next_key();
        let first = self
            .client
            .register_entities(&Self::ctx(), key.clone(), request(publisher()))
            .await
            .expect("accepted");
        let replay = self
            .client
            .register_entities(&Self::ctx(), key.clone(), request(publisher()))
            .await
            .expect("a replay is answered");
        assert_eq!(
            replay.operation_id, first.operation_id,
            "a replay answers the bound operation"
        );
        self.completed(first.operation_id).await;

        let other = PublisherContext {
            name: "someone-else".to_owned(),
            ..publisher()
        };
        let replay = self
            .client
            .register_entities(&Self::ctx(), key.clone(), request(other))
            .await
            .expect("the publisher is not part of the binding");
        assert_eq!(
            replay.operation_id, first.operation_id,
            "the publisher is not part of the binding"
        );

        let error = self
            .client
            .register_entities(
                &Self::ctx(),
                key.clone(),
                RegisterEntitiesRequest {
                    dry_run: true,
                    ..request(publisher())
                },
            )
            .await
            .expect_err("a dry run cannot reuse a commit's key");
        assert!(
            matches!(error, CanonicalError::AlreadyExists { .. }),
            "{error:?}"
        );

        let error = self
            .client
            .delete_entities(
                &Self::ctx(),
                key,
                DeleteEntitiesRequest {
                    items: vec![DeleteItem {
                        key: crate::models::EntityKey::GtsId(gid(KEYED)),
                        expected_resource_version: 1,
                    }],
                    dry_run: false,
                    publisher: publisher(),
                },
            )
            .await
            .expect_err("a deletion cannot reuse a registration's key");
        assert!(
            matches!(error, CanonicalError::AlreadyExists { .. }),
            "{error:?}"
        );

        let deletion = |key: EntityKey| DeleteEntitiesRequest {
            items: vec![DeleteItem {
                key,
                expected_resource_version: 1,
            }],
            dry_run: false,
            publisher: publisher(),
        };
        let delete_key = self.next_key();
        let by_id = self
            .client
            .delete_entities(&Self::ctx(), delete_key.clone(), deletion(gts_key(KEYED)))
            .await
            .expect("accepted");
        self.completed(by_id.operation_id).await;
        let by_uuid = self
            .client
            .delete_entities(
                &Self::ctx(),
                delete_key,
                deletion(EntityKey::GtsUuid(gid(KEYED).to_uuid())),
            )
            .await
            .expect("a Registry Reference replays the identifier's deletion");
        assert_eq!(
            by_uuid.operation_id, by_id.operation_id,
            "both spellings of one entity bind the same deletion"
        );
    }

    /// Submits `request` and answers its completed registration.
    async fn registration(&self, request: RegisterEntitiesRequest) -> RegistrationOperation {
        let accepted = bounded(
            "the registration is answered",
            self.client
                .register_entities(&Self::ctx(), self.next_key(), request),
        )
        .await
        .expect("the registration is accepted");
        let Operation::Registration(operation) = self.completed(accepted.operation_id).await else {
            panic!("a registration completes as a registration");
        };
        operation
    }

    async fn dry_runs_preview_without_allocating_versions(&self) {
        const PREVIEW: &str = "gts.cf.core.conformance.preview.v1~";
        self.a_dry_run_creation_previews(PREVIEW).await;
        let version = self.a_creation_reports_the_stored_version(PREVIEW).await;
        self.a_dry_run_reports_an_unchanged_update(PREVIEW, version)
            .await;
        self.a_dry_run_deletion_previews(PREVIEW, version).await;
    }

    /// The one outcome of a single-item registration of `item`.
    async fn register_one(&self, item: RegisterItem, dry_run: bool) -> RegistrationOutcome {
        let operation = self
            .registration(RegisterEntitiesRequest {
                items: vec![item],
                dry_run,
                publisher: publisher(),
            })
            .await;
        let [item] = <[_; 1]>::try_from(operation.items).expect("one result");
        item.outcome
    }

    async fn a_dry_run_creation_previews(&self, gts_id: &str) {
        let preview = self
            .register_one(create(gts_id, root(gts_id, None)), true)
            .await;
        assert!(
            matches!(preview, RegistrationOutcome::WouldSucceed),
            "a dry-run creation previews without a version: {preview:?}"
        );
        assert!(
            matches!(
                self.get(gts_key(gts_id), light()).await,
                EntityLookup::NotFound
            ),
            "a dry run commits nothing"
        );
    }

    async fn a_creation_reports_the_stored_version(&self, gts_id: &str) -> u64 {
        let created = self
            .register_one(create(gts_id, root(gts_id, None)), false)
            .await;
        let RegistrationOutcome::Succeeded { resource_version } = created else {
            panic!("a committed creation carries its version: {created:?}");
        };
        let (stored, _) = found(self.get(gts_key(gts_id), every_field()).await);
        assert!(
            matches!(
                stored.origin,
                Some(Origin::Managed { resource_version: v, .. }) if v == resource_version
            ),
            "the reported version is the stored one: {:?}",
            stored.origin
        );
        resource_version
    }

    async fn a_dry_run_reports_an_unchanged_update(&self, gts_id: &str, version: u64) {
        let unchanged = self
            .register_one(revise(gts_id, root(gts_id, None), version), true)
            .await;
        assert!(
            matches!(
                unchanged,
                RegistrationOutcome::Unchanged { resource_version: v } if v == version
            ),
            "a dry run reports an unchanged update at the existing version: {unchanged:?}"
        );
    }

    async fn a_dry_run_deletion_previews(&self, gts_id: &str, version: u64) {
        let accepted = bounded(
            "the deletion is answered",
            self.client.delete_entities(
                &Self::ctx(),
                self.next_key(),
                DeleteEntitiesRequest {
                    items: vec![DeleteItem {
                        key: gts_key(gts_id),
                        expected_resource_version: version,
                    }],
                    dry_run: true,
                    publisher: publisher(),
                },
            ),
        )
        .await
        .expect("the deletion is accepted");
        let Operation::Deletion(deletion) = self.completed(accepted.operation_id).await else {
            panic!("a deletion completes as a deletion");
        };
        assert!(
            matches!(
                deletion.items.as_slice(),
                [DeletionItemResult {
                    outcome: DeletionOutcome::WouldSucceed,
                    ..
                }]
            ),
            "a dry-run deletion previews without a version: {:?}",
            deletion.items
        );
        let (still, _) = found(self.get(gts_key(gts_id), light()).await);
        assert_eq!(
            still.lifecycle_status,
            LifecycleStatus::Active,
            "and deletes nothing"
        );
    }
}
