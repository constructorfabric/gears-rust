//! The schema service on an in-memory database: owner only, one person's change leaves another's alone, a refusal
//! changes nothing, and the schema is the same for a service started again over the same database.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use construct_sdk::linked_schema::default_schema;
use construct_sdk::schema_doc::SchemaDoc;
use toolkit_db::{DBProvider, Db};
use uuid::Uuid;

use super::error::DomainError;
use super::profile_schema::{ProfileSchemas, SchemaChange, SchemaLookup, SubjectSchemaRepository};
use super::schema_service::{SchemaService, StoredSchemas};
use super::subject_settings::SubjectKey;
use crate::infra::storage::subject_schema_repo::SeaOrmSubjectSchemaRepository;
use crate::test_support::{
    AllowResolver, OwnerOnlyResolver, RecordingSchemaEvents, context_of, inmem_db,
};

use crate::test_support::RecordingEffects as Effects;

type Service = SchemaService<SeaOrmSubjectSchemaRepository>;

fn service(db: &Db, effects: &Arc<Effects>, owner_only: bool) -> Service {
    service_with(
        db,
        effects,
        owner_only,
        Arc::new(RecordingSchemaEvents::default()),
    )
}

fn service_with(
    db: &Db,
    effects: &Arc<Effects>,
    owner_only: bool,
    events: Arc<RecordingSchemaEvents>,
) -> Service {
    let resolver: Arc<dyn authz_resolver_sdk::AuthZResolverApi> = if owner_only {
        Arc::new(OwnerOnlyResolver)
    } else {
        Arc::new(AllowResolver)
    };
    SchemaService::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmSubjectSchemaRepository::new()),
        Arc::clone(effects) as _,
        events,
        PolicyEnforcer::new(resolver),
    )
}

fn stored(db: &Db) -> StoredSchemas<SeaOrmSubjectSchemaRepository> {
    StoredSchemas::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmSubjectSchemaRepository::new()),
    )
}

use crate::test_support::add_pets as pets;

fn key(tenant: Uuid, subject: Uuid) -> SubjectKey {
    SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    }
}

#[tokio::test]
async fn a_person_without_a_change_has_the_default_schema() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let view = service(&db, &effects, true)
        .schema(&context_of(me, tenant), me)
        .await
        .unwrap();
    assert_eq!(view.version, 0);
    assert_eq!(view.schema, SchemaDoc::of(&default_schema()));
    assert_eq!(
        *stored(&db).schema(key(tenant, me)).await.unwrap(),
        default_schema()
    );
}

#[tokio::test]
async fn an_added_kind_is_registered_stored_and_read_back_from_the_next_message() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let events = Arc::new(RecordingSchemaEvents::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    let svc = service_with(&db, &effects, true, Arc::clone(&events));

    let view = svc.change(&ctx, me, &[pets()]).await.unwrap();

    // The change is recorded for the audit trail, with who made it.
    assert_eq!(
        *events.changed.lock().unwrap(),
        [crate::domain::schema_service::SchemaChanged {
            tenant_id: tenant,
            subject_id: me,
            caller: me,
            changes: 1,
            removed_kinds: Vec::new(),
            version: 1,
        }]
    );

    assert_eq!(view.version, 1);
    assert!(view.schema.nodes.iter().any(|kind| kind.name == "pets"));
    {
        let registered = effects.registered.lock().unwrap();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0, key(tenant, me));
        assert!(registered[0].1.contains(&"pets".to_owned()));
    }
    // What the planner, the reader and the writer read at the next message.
    let schema = stored(&db).schema(key(tenant, me)).await.unwrap();
    assert!(schema.node("pets").is_some());
    assert_eq!(schema.node("pets").unwrap().properties.len(), 2);
}

#[tokio::test]
async fn the_schema_is_the_same_after_a_restart() {
    let db = inmem_db().await;
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    service(&db, &Arc::new(Effects::default()), true)
        .change(&ctx, me, &[pets()])
        .await
        .unwrap();

    // A service and a reader built anew over the same database, as after a restart.
    let after = service(&db, &Arc::new(Effects::default()), true)
        .schema(&ctx, me)
        .await
        .unwrap();
    assert_eq!(after.version, 1);
    assert!(after.schema.nodes.iter().any(|kind| kind.name == "pets"));
    assert!(
        stored(&db)
            .schema(key(tenant, me))
            .await
            .unwrap()
            .node("pets")
            .is_some()
    );
}

#[tokio::test]
async fn a_change_for_one_person_leaves_another_as_it_was() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let tenant = Uuid::new_v4();
    let (mine, theirs) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = service(&db, &effects, true);

    svc.change(&context_of(mine, tenant), mine, &[pets()])
        .await
        .unwrap();

    let other = svc
        .schema(&context_of(theirs, tenant), theirs)
        .await
        .unwrap();
    assert_eq!(other.version, 0);
    assert!(other.schema.nodes.iter().all(|kind| kind.name != "pets"));
    assert!(
        stored(&db)
            .schema(key(tenant, theirs))
            .await
            .unwrap()
            .node("pets")
            .is_none()
    );
    // The same person id in another tenant is another person too.
    let elsewhere = Uuid::new_v4();
    assert!(
        stored(&db)
            .schema(key(elsewhere, mine))
            .await
            .unwrap()
            .node("pets")
            .is_none()
    );
}

#[tokio::test]
async fn another_persons_schema_is_refused_for_the_read_and_the_change() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me, them) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let svc = service(&db, &effects, true);
    let ctx = context_of(me, tenant);

    assert!(matches!(
        svc.schema(&ctx, them).await,
        Err(DomainError::Forbidden(_))
    ));
    assert!(matches!(
        svc.change(&ctx, them, &[pets()]).await,
        Err(DomainError::Forbidden(_))
    ));
    assert!(effects.registered.lock().unwrap().is_empty());
    assert!(
        stored(&db)
            .schema(key(tenant, them))
            .await
            .unwrap()
            .node("pets")
            .is_none()
    );
}

#[tokio::test]
async fn a_refused_change_changes_nothing() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    let svc = service(&db, &effects, true);

    // A rule of the schema: a name already in use.
    svc.change(&ctx, me, &[pets()]).await.unwrap();
    // The change that stored the first schema swept once; a refused change sweeps no more.
    let swept = effects.forgotten.lock().unwrap().len();
    let refused = svc.change(&ctx, me, &[pets()]).await;
    assert!(matches!(
        refused,
        Err(DomainError::InvalidValue {
            field: "schema",
            ..
        })
    ));

    // Graph storage refuses the types.
    *effects.refuse.lock().unwrap() = Some("incompatible with the registered type".to_owned());
    let refused = svc
        .change(
            &ctx,
            me,
            &[SchemaChange::RemoveNodeKind {
                name: "pets".to_owned(),
            }],
        )
        .await;
    assert!(matches!(refused, Err(DomainError::InvalidValue { .. })));

    let now = svc.schema(&ctx, me).await.unwrap();
    assert_eq!(now.version, 1);
    assert!(now.schema.nodes.iter().any(|kind| kind.name == "pets"));
    assert_eq!(effects.forgotten.lock().unwrap().len(), swept);
}

#[tokio::test]
async fn removing_a_kind_drops_it_from_the_schema_and_then_forgets_its_nodes() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    let svc = service(&db, &effects, true);
    svc.change(&ctx, me, &[pets()]).await.unwrap();

    let view = svc
        .change(
            &ctx,
            me,
            &[SchemaChange::RemoveNodeKind {
                name: "pets".to_owned(),
            }],
        )
        .await
        .unwrap();

    assert_eq!(view.version, 2);
    assert!(view.schema.nodes.iter().all(|kind| kind.name != "pets"));
    // Once after the first change, which stored a schema, and once after the removal.
    assert_eq!(
        *effects.forgotten.lock().unwrap(),
        vec![key(tenant, me), key(tenant, me)]
    );
    assert!(
        stored(&db)
            .schema(key(tenant, me))
            .await
            .unwrap()
            .node("pets")
            .is_none()
    );
}

#[tokio::test]
async fn a_change_that_changes_nothing_is_not_stored() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    let svc = service(&db, &effects, true);

    let view = svc.change(&ctx, me, &[]).await.unwrap();

    assert_eq!(view.version, 0);
    assert!(effects.registered.lock().unwrap().is_empty());
}

/// The stored schemas with an answer put in for one step: another change that always wins the charge, or the
/// compare-and-move.
struct ScriptedRepo {
    inner: SeaOrmSubjectSchemaRepository,
    reserve: Option<crate::domain::profile_schema::SchemaStore>,
    store: Option<crate::domain::profile_schema::SchemaStore>,
}

#[async_trait]
impl SubjectSchemaRepository for ScriptedRepo {
    async fn find<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        key: SubjectKey,
    ) -> Result<SchemaLookup, DomainError> {
        self.inner.find(conn, scope, key).await
    }

    #[allow(clippy::too_many_arguments, reason = "the repository's own signature")]
    async fn reserve_names<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        key: SubjectKey,
        expected: Option<i64>,
        current: &SchemaDoc,
        from: i64,
        to: i64,
    ) -> Result<crate::domain::profile_schema::SchemaStore, DomainError> {
        match self.reserve {
            Some(answer) => Ok(answer),
            None => {
                self.inner
                    .reserve_names(conn, scope, key, expected, current, from, to)
                    .await
            }
        }
    }

    async fn store<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        key: SubjectKey,
        expected: Option<i64>,
        doc: &SchemaDoc,
        names_charged: i64,
    ) -> Result<crate::domain::profile_schema::SchemaStore, DomainError> {
        match self.store {
            Some(answer) => Ok(answer),
            None => {
                self.inner
                    .store(conn, scope, key, expected, doc, names_charged)
                    .await
            }
        }
    }
}

/// A service over the stored schemas with one step's answer put in.
fn service_scripted(
    db: &Db,
    effects: &Arc<Effects>,
    reserve: Option<crate::domain::profile_schema::SchemaStore>,
    store: Option<crate::domain::profile_schema::SchemaStore>,
) -> SchemaService<ScriptedRepo> {
    SchemaService::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(ScriptedRepo {
            inner: SeaOrmSubjectSchemaRepository::new(),
            reserve,
            store,
        }),
        Arc::clone(effects) as _,
        Arc::new(RecordingSchemaEvents::default()),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    )
}

#[tokio::test]
async fn a_conflict_forgets_no_node_and_stores_nothing() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = service_scripted(
        &db,
        &effects,
        None,
        Some(crate::domain::profile_schema::SchemaStore::Conflict),
    );

    let result = svc
        .change(
            &context_of(me, tenant),
            me,
            &[SchemaChange::RemoveNodeKind {
                name: "project".to_owned(),
            }],
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "{result:?}"
    );
    assert!(
        effects.forgotten.lock().unwrap().is_empty(),
        "no node is forgotten for a change that was not stored"
    );
}

#[tokio::test]
async fn a_failed_forget_leaves_the_schema_changed_and_says_so() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    let svc = service(&db, &effects, true);
    svc.change(&ctx, me, &[pets()]).await.unwrap();
    effects
        .fail_forget
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let result = svc
        .change(
            &ctx,
            me,
            &[SchemaChange::RemoveNodeKind {
                name: "pets".to_owned(),
            }],
        )
        .await;

    let Err(DomainError::Unavailable(message)) = result else {
        panic!("unavailable, got {result:?}");
    };
    assert_eq!(message, crate::domain::schema_service::SWEEP_PENDING);
    assert!(message.contains("empty change"), "{message}");
    let now = svc.schema(&ctx, me).await.unwrap();
    assert_eq!(now.version, 2, "the schema is stored");
    assert!(now.schema.nodes.iter().all(|kind| kind.name != "pets"));

    // The owner finishes the removal by repeating the change: a change that changes nothing forgets the orphans.
    effects
        .fail_forget
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let calls = effects.forgotten.lock().unwrap().len();
    let repeated = svc
        .change(
            &ctx,
            me,
            &[SchemaChange::RemoveNodeKind {
                name: "pets".to_owned(),
            }],
        )
        .await;
    assert!(
        matches!(repeated, Err(DomainError::InvalidValue { .. })),
        "{repeated:?}"
    );
    let empty = svc
        .change(&ctx, me, &[])
        .await
        .expect("an empty change finishes the removal");
    assert_eq!(empty.version, 2);
    assert_eq!(effects.forgotten.lock().unwrap().len(), calls + 1);
}

#[tokio::test]
async fn a_person_may_introduce_only_so_many_names_over_the_life_of_their_schema() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let svc = service(&db, &effects, true);
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSchemaRepository::new();
    // The person has used up their share.
    crate::domain::profile_schema::SubjectSchemaRepository::store(
        &repo,
        &conn,
        &toolkit_security::AccessScope::allow_all(),
        key(tenant, me),
        None,
        &SchemaDoc::of(&default_schema()),
        crate::domain::profile_schema::MAX_NAMES_CHARGED,
    )
    .await
    .unwrap();

    let refused = svc
        .change(
            &context_of(me, tenant),
            me,
            &[crate::test_support::add_fresh_kind()],
        )
        .await;
    assert!(
        matches!(refused, Err(DomainError::InvalidValue { .. })),
        "{refused:?}"
    );
    // A change that adds no name is never refused, and another person's share is their own.
    svc.change(
        &context_of(me, tenant),
        me,
        &[SchemaChange::RemoveNodeKind {
            name: "project".to_owned(),
        }],
    )
    .await
    .expect("a removal adds no name");
    svc.change(
        &context_of(other, tenant),
        other,
        &[crate::test_support::add_fresh_kind()],
    )
    .await
    .expect("another person's share is untouched");
}

#[tokio::test]
async fn a_denial_of_the_policy_changes_nothing() {
    use crate::test_support::DenyResolver;
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = SchemaService::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmSubjectSchemaRepository::new()),
        Arc::clone(&effects) as _,
        Arc::new(RecordingSchemaEvents::default()),
        PolicyEnforcer::new(Arc::new(DenyResolver)),
    );

    let result = svc.change(&context_of(me, tenant), me, &[pets()]).await;

    assert!(result.is_err());
    assert!(effects.registered.lock().unwrap().is_empty());
}

#[tokio::test]
async fn names_are_charged_before_the_process_keeps_them_even_when_the_change_is_refused() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = service(&db, &effects, true);
    // Graph storage refuses the types, after the names would have been kept.
    *effects.refuse.lock().unwrap() = Some("refused".to_owned());

    let refused = svc
        .change(
            &context_of(me, tenant),
            me,
            &[crate::test_support::add_fresh_kind()],
        )
        .await;
    assert!(refused.is_err());

    let conn = db.conn().expect("connection");
    let SchemaLookup::Stored(row) = SeaOrmSubjectSchemaRepository::new()
        .find(
            &conn,
            &toolkit_security::AccessScope::allow_all(),
            key(tenant, me),
        )
        .await
        .unwrap()
    else {
        panic!("the charge stored a row");
    };
    assert!(
        row.names_charged > 0,
        "the refused change was charged for its names"
    );
    assert_eq!(row.version, 0, "and left the schema as it was");
    assert_eq!(
        svc.schema(&context_of(me, tenant), me)
            .await
            .unwrap()
            .version,
        0
    );
}

#[tokio::test]
async fn a_change_that_loses_the_charge_keeps_no_names_and_registers_nothing() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = service_scripted(
        &db,
        &effects,
        Some(crate::domain::profile_schema::SchemaStore::Conflict),
        None,
    );

    let result = svc
        .change(
            &context_of(me, tenant),
            me,
            &[crate::test_support::add_fresh_kind()],
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "{result:?}"
    );
    assert!(
        effects.registered.lock().unwrap().is_empty(),
        "a change that lost the charge never reaches the point where names are kept"
    );
}

#[tokio::test]
async fn nodes_forgotten_by_a_change_are_recorded_with_their_count() {
    let db = inmem_db().await;
    let effects = Arc::new(Effects::default());
    let events = Arc::new(RecordingSchemaEvents::default());
    let (tenant, me) = (Uuid::new_v4(), Uuid::new_v4());
    let ctx = context_of(me, tenant);
    let svc = service_with(&db, &effects, true, Arc::clone(&events));
    svc.change(&ctx, me, &[pets()]).await.unwrap();
    assert!(
        events.forgotten.lock().unwrap().is_empty(),
        "nothing was forgotten yet"
    );
    effects
        .forgotten_nodes
        .store(3, std::sync::atomic::Ordering::SeqCst);

    svc.change(
        &ctx,
        me,
        &[SchemaChange::RemoveNodeKind {
            name: "pets".to_owned(),
        }],
    )
    .await
    .unwrap();

    assert_eq!(
        *events.forgotten.lock().unwrap(),
        [crate::domain::schema_service::NodesForgotten {
            tenant_id: tenant,
            subject_id: me,
            nodes: 3,
        }]
    );
}
