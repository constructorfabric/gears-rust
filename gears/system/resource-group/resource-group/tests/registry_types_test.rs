#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Resource-group over types-registry type definitions (T38b): which operations
//! read a definition, which lifecycle they accept, and that reads and cleanup
//! never depend on the registry.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use resource_group::domain::error::DomainError;
use resource_group::domain::type_catalog::{MembershipSchema, RgType, RgTypeCatalog, TypeUse};
use resource_group_sdk::{CreateGroupRequest, UpdateGroupRequest};
use serde_json::json;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use common::CreateTypeRequest;

fn code(name: &str) -> String {
    format!(
        "gts.cf.core.rg.type.v1~x.test.{name}.i{}.v1~",
        Uuid::now_v7().as_simple()
    )
}

/// A member type, and a root type and a child type under it, both admitting it.
fn publish_family() -> (String, String, String) {
    let member = code("member");
    common::publish_type(CreateTypeRequest {
        code: member.clone(),
        ..CreateTypeRequest::default()
    });
    let root = code("root");
    common::publish_type(CreateTypeRequest {
        code: root.clone(),
        can_be_root: true,
        allowed_parent_types: vec![root.clone()],
        allowed_membership_types: vec![member.clone()],
        metadata_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "tier": { "type": "string" } }
        })),
    });
    let child = code("child");
    common::publish_type(CreateTypeRequest {
        code: child.clone(),
        allowed_parent_types: vec![root.clone()],
        allowed_membership_types: vec![member.clone()],
        ..CreateTypeRequest::default()
    });
    (member, root, child)
}

/// Rows in RG's GTS type identity dictionary.
async fn dictionary_rows(db: &toolkit_db::DBProvider<toolkit_db::DbError>) -> Vec<(i16, String)> {
    use resource_group::infra::storage::entity::gts_type::Entity;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    let conn = db.conn().unwrap();
    let mut rows: Vec<_> = Entity::find()
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .all(&conn)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.schema_id))
        .collect();
    rows.sort();
    rows
}

// -------- new links require an Active type --------

#[tokio::test]
async fn creating_a_group_of_an_unknown_or_deleted_type_is_type_not_found() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);

    let unknown = code("never");
    let err = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(unknown, "g".to_owned()),
            tenant,
        )
        .await
        .expect_err("unknown type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");

    let (_, root, _) = publish_family();
    common::delete_published_type(&root);
    let err = groups
        .create_group(&ctx, CreateGroupRequest::new(root, "g".to_owned()), tenant)
        .await
        .expect_err("deleted type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
}

/// The membership schema a group type binds is part of its definition: a
/// group cannot be created while that schema is unknown or deleted, and the
/// refusal writes nothing.
#[tokio::test]
async fn creating_a_group_needs_its_bound_membership_schema() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let suffix = root
        .trim_end_matches('~')
        .rsplit_once('~')
        .expect("a derived RG type code")
        .1;
    let bound = format!(
        "{}{suffix}~",
        resource_group_sdk::gts::GROUP_MEMBERSHIP_RESOURCE_TYPE
    );
    common::delete_published_type(&bound);
    let before = dictionary_rows(&db).await;

    let err = groups
        .create_group(&ctx, CreateGroupRequest::new(root, "g".to_owned()), tenant)
        .await
        .expect_err("the bound membership schema is deleted");
    assert!(
        matches!(&err, DomainError::TypeNotFound { code } if *code == bound),
        "{err:?}"
    );
    assert_eq!(dictionary_rows(&db).await, before, "nothing keyed");
    let listed = groups
        .list_groups(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    assert!(listed.items.is_empty(), "no group row");
}

#[tokio::test]
async fn a_new_membership_needs_an_active_group_type_and_member_type() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;

    members
        .add_membership(&ctx, group.id, &member, "r1")
        .await
        .expect("both active");

    common::delete_published_type(&member);
    let err = members
        .add_membership(&ctx, group.id, &member, "r2")
        .await
        .expect_err("deleted member type");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");

    let (other_member, other_root, _) = publish_family();
    let other_group = common::create_root_group(&groups, &ctx, &other_root, "g2", tenant).await;
    common::delete_published_type(&other_root);
    let err = members
        .add_membership(&ctx, other_group.id, &other_member, "r1")
        .await
        .expect_err("deleted group type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn a_member_type_must_be_a_concrete_type_identifier() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;

    for invalid in [
        "not-a-gts-id",
        "gts.cf.test.rg.*",
        "gts.x.test.*",
        // An instance identifier (no trailing `~`) is not a type.
        "gts.cf.core.rg.type.v1~x.test._.thing.v1",
        "",
    ] {
        let err = members
            .add_membership(&ctx, group.id, invalid, "r1")
            .await
            .expect_err("not a concrete type identifier");
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{invalid:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn move_checks_group_visibility_before_reading_its_type() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let owner = Uuid::now_v7();
    let (_, root, _) = publish_family();
    let group =
        common::create_root_group(&groups, &common::make_ctx(owner), &root, "root", owner).await;
    let unavailable = common::make_group_service_with(db, common::make_unavailable_types_catalog());
    let error = unavailable
        .move_group(&common::make_ctx(Uuid::now_v7()), group.id, None)
        .await
        .unwrap_err();
    assert!(matches!(error, DomainError::GroupNotFound { id } if id == group.id));
}

// -------- reads and cleanup never consult the registry --------

#[tokio::test]
async fn reads_and_cleanup_work_while_the_registry_is_unavailable() {
    let db = common::test_db().await;
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, child) = publish_family();
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db.clone());
    let r = common::create_root_group(&groups, &ctx, &root, "root", tenant).await;
    let c = common::create_child_group(&groups, &ctx, &child, r.id, "child", tenant).await;
    members
        .add_membership(&ctx, r.id, &member, "m1")
        .await
        .unwrap();

    let offline_groups =
        common::make_group_service_with(db.clone(), common::make_unavailable_types_catalog());
    let offline_members =
        common::make_membership_service_with(db, common::make_unavailable_types_catalog());

    offline_groups.get_group(&ctx, r.id).await.expect("get");
    assert_eq!(
        offline_groups
            .list_groups(&ctx, &ODataQuery::default())
            .await
            .expect("list")
            .items
            .len(),
        2
    );
    offline_groups
        .get_group_descendants(&ctx, r.id, &ODataQuery::default())
        .await
        .expect("hierarchy");
    offline_members
        .list_memberships(&ctx, &ODataQuery::default())
        .await
        .expect("list memberships");
    offline_groups
        .update_group(
            &ctx,
            c.id,
            UpdateGroupRequest {
                name: "renamed".to_owned(),
                parent_id: Some(r.id),
                metadata: None,
            },
        )
        .await
        .expect("a rename reads no definition");
    offline_members
        .remove_membership(&ctx, r.id, &member, "m1")
        .await
        .expect("remove membership");
    offline_groups
        .delete_group(&ctx, r.id, true)
        .await
        .expect("force delete");
}

#[tokio::test]
async fn groups_of_a_deleted_type_stay_readable_updatable_and_deletable() {
    let db = common::test_db().await;
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, child) = publish_family();
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let r1 = common::create_root_group(&groups, &ctx, &root, "r1", tenant).await;
    let r2 = common::create_child_group(&groups, &ctx, &root, r1.id, "r2", tenant).await;
    let c = common::create_child_group(&groups, &ctx, &child, r1.id, "c", tenant).await;
    members
        .add_membership(&ctx, c.id, &member, "m1")
        .await
        .expect("the child type admits the member type");
    members
        .add_membership(&ctx, r1.id, &member, "m2")
        .await
        .unwrap();

    common::delete_published_type(&root);
    common::delete_published_type(&child);

    groups.get_group(&ctx, r1.id).await.expect("read");
    let updated = groups
        .update_group(
            &ctx,
            r1.id,
            UpdateGroupRequest {
                name: "r1".to_owned(),
                parent_id: None,
                metadata: Some(serde_json::json!({ "tier": "gold" })),
            },
        )
        .await
        .expect("an existing group's deleted type still defines its metadata");
    assert_eq!(
        updated.metadata,
        Some(serde_json::json!({ "tier": "gold" }))
    );
    let err = groups
        .update_group(
            &ctx,
            r1.id,
            UpdateGroupRequest {
                name: "r1".to_owned(),
                parent_id: None,
                metadata: Some(serde_json::json!({ "tier": 7 })),
            },
        )
        .await
        .expect_err("the retained schema applies");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    let err = groups
        .move_group(&ctx, c.id, None)
        .await
        .expect_err("the retained schema does not allow a root child");
    assert!(
        matches!(err, DomainError::InvalidParentType { .. }),
        "{err:?}"
    );
    let unmoved = groups.get_group(&ctx, c.id).await.expect("read");
    assert_eq!(
        unmoved.hierarchy.parent_id,
        Some(r1.id),
        "the refused move changed nothing"
    );
    let moved = groups
        .move_group(&ctx, c.id, Some(r2.id))
        .await
        .expect("the retained schema allows a root parent");
    assert_eq!(moved.hierarchy.parent_id, Some(r2.id));
    let stored = groups.get_group(&ctx, c.id).await.expect("read");
    assert_eq!(stored.hierarchy.parent_id, Some(r2.id));

    for (group, resource) in [(c.id, "m1"), (r1.id, "m2")] {
        members
            .remove_membership(&ctx, group, &member, resource)
            .await
            .expect("cleanup of a deleted type's membership");
    }
    groups
        .delete_group(&ctx, r1.id, true)
        .await
        .expect("cleanup");
}

// -------- replacement metadata and portable membership key bounds --------

/// A new group's metadata is validated against its type's schema, as a root
/// and under a parent alike, and a refused create stores nothing.
#[tokio::test]
async fn group_creation_validates_metadata_against_the_type_schema() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let invalid = Some(json!({ "tier": 7 }));

    let err = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(root.clone(), "bad-root").with_metadata(invalid.clone()),
            tenant,
        )
        .await
        .expect_err("a root with invalid metadata");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");

    let r = common::create_root_group(&groups, &ctx, &root, "r", tenant).await;
    let err = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(root, "bad-child")
                .with_parent_id(Some(r.id))
                .with_metadata(invalid),
            tenant,
        )
        .await
        .expect_err("a child with invalid metadata");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");

    let listed = groups
        .list_groups(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    let ids: Vec<_> = listed.items.iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![r.id], "only the valid root is stored");
}

#[tokio::test]
async fn required_metadata_cannot_be_cleared_by_a_replacement_update() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let root = code("required_metadata");
    common::publish_type(common::CreateTypeRequest {
        code: root.clone(),
        can_be_root: true,
        metadata_schema: Some(json!({"type": "object"})),
        ..common::CreateTypeRequest::default()
    });
    let registry = common::types_registry();
    let mut schema = registry.content(&root).expect("published group schema");
    schema["allOf"][1]["required"] = json!(["metadata"]);
    registry.seed(&root, schema);
    let metadata = json!({ "tier": "gold" });
    let group = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(root, "original".to_owned())
                .with_metadata(Some(metadata.clone())),
            tenant,
        )
        .await
        .expect("required metadata is present");

    let error = groups
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                name: "changed".to_owned(),
                parent_id: None,
                metadata: None,
            },
        )
        .await
        .expect_err("PUT must validate removal of required metadata");
    assert!(matches!(error, DomainError::Validation { .. }), "{error:?}");
    let stored = groups.get_group(&ctx, group.id).await.unwrap();
    assert_eq!(stored.metadata, Some(metadata));
    assert_eq!(stored.name, "original");
}

#[tokio::test]
async fn optional_metadata_can_be_cleared_but_requires_a_definition() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let group = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(root, "original".to_owned())
                .with_metadata(Some(json!({ "tier": "gold" }))),
            tenant,
        )
        .await
        .unwrap();
    let replacement = UpdateGroupRequest {
        name: "changed".to_owned(),
        parent_id: None,
        metadata: None,
    };
    let offline = common::make_group_service_with(db, common::make_unavailable_types_catalog());
    let error = offline
        .update_group(&ctx, group.id, replacement.clone())
        .await
        .expect_err("metadata removal must not bypass an unavailable definition");
    assert!(matches!(error, DomainError::InternalError), "{error:?}");
    assert!(
        groups
            .get_group(&ctx, group.id)
            .await
            .unwrap()
            .metadata
            .is_some()
    );
    let updated = groups
        .update_group(&ctx, group.id, replacement)
        .await
        .expect("the schema permits absent metadata");
    assert!(updated.metadata.is_none());
}

#[tokio::test]
async fn membership_resource_ids_are_bounded_by_utf8_bytes() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;
    // The member type is stored as a dictionary key: only the resource
    // identifier's own bytes count.
    let resource_bytes = resource_group_sdk::MAX_MEMBERSHIP_RESOURCE_ID_BYTES;
    let boundary = "a".repeat(resource_bytes);
    members
        .add_membership(&ctx, group.id, &member, &boundary)
        .await
        .expect("exactly the supported byte budget");

    for oversized in [
        format!("{boundary}a"),
        "\u{e9}".repeat(resource_bytes.div_ceil(2) + 1),
    ] {
        let error = members
            .add_membership(&ctx, group.id, &member, &oversized)
            .await
            .expect_err("the resource identifier is too wide");
        assert!(matches!(error, DomainError::Validation { .. }), "{error:?}");
        assert!(error.to_string().contains("2600"), "{error}");
    }
    let stored = members
        .list_memberships(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(stored.items.len(), 1);
    assert_eq!(stored.items[0].resource_id, boundary);
}

// -------- the transaction rechecks the type it read --------

/// Answers every lookup with another type's definition, as if the group had
/// been re-created under the same id with a different type between the
/// pre-transaction read and the transaction.
struct WrongType {
    inner: Arc<dyn RgTypeCatalog>,
    other: String,
}

#[async_trait]
impl RgTypeCatalog for WrongType {
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        _code: &str,
        usage: TypeUse,
    ) -> Result<RgType, DomainError> {
        self.inner.get_type(ctx, &self.other, usage).await
    }

    async fn get_membership_schema(
        &self,
        ctx: &SecurityContext,
        membership_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        self.inner.get_membership_schema(ctx, membership_type).await
    }

    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_type: &str,
        resource_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        self.inner
            .get_membership_rule(ctx, group_type, resource_type)
            .await
    }
}

#[tokio::test]
async fn a_definition_read_for_another_type_is_refused_inside_the_transaction() {
    let db = common::test_db().await;
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let (_, other_root, _) = publish_family();
    let groups = common::make_group_service(db.clone());
    let g = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;

    let skewed: Arc<dyn RgTypeCatalog> = Arc::new(WrongType {
        inner: common::make_types_catalog(),
        other: other_root,
    });
    let skewed_groups = common::make_group_service_with(db.clone(), skewed.clone());
    let err = skewed_groups
        .update_group(
            &ctx,
            g.id,
            UpdateGroupRequest {
                name: "g".to_owned(),
                parent_id: None,
                metadata: Some(serde_json::json!({ "tier": "gold" })),
            },
        )
        .await
        .expect_err("the in-transaction row has another type");
    assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");

    let err = skewed_groups
        .move_group(&ctx, g.id, None)
        .await
        .expect_err("the in-transaction row has another type");
    assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");
    // A membership rule is read for the pre-read row's own type,
    // so no catalog answer names another type; the transaction compares its
    // row with that pre-read row.
}

/// Rewrites the group row through a second connection while the membership
/// rule is read, between the pre-transaction read and the transaction.
struct ChangeRowDuringRuleRead {
    inner: Arc<dyn RgTypeCatalog>,
    raw: sea_orm::DatabaseConnection,
    sql: String,
}

#[async_trait]
impl RgTypeCatalog for ChangeRowDuringRuleRead {
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
        usage: TypeUse,
    ) -> Result<RgType, DomainError> {
        self.inner.get_type(ctx, code, usage).await
    }

    async fn get_membership_schema(
        &self,
        ctx: &SecurityContext,
        membership_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        self.inner.get_membership_schema(ctx, membership_type).await
    }

    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_type: &str,
        resource_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        use sea_orm::ConnectionTrait;
        self.raw
            .execute_unprepared(&self.sql)
            .await
            .expect("rewrite the group row");
        self.inner
            .get_membership_rule(ctx, group_type, resource_type)
            .await
    }
}

/// Removes a test database directory, also when the test fails.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("leaving {}: {e}", self.0.display());
        }
    }
}

/// A file database, so a second connection sees the services' rows.
async fn file_db() -> (
    Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    sea_orm::DatabaseConnection,
    TempDir,
) {
    use resource_group::infra::storage::migrations::Migrator;
    use sea_orm_migration::MigratorTrait;
    let dir = std::env::temp_dir().join(format!("rg-race-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = TempDir(dir);
    let dsn = format!("sqlite://{}?mode=rwc", dir.0.join("rg.db").display());
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    let raw = sea_orm::Database::connect(&dsn).await.unwrap();
    (Arc::new(toolkit_db::DBProvider::new(db)), raw, dir)
}

/// A membership is checked against the group row it read before the
/// transaction: a row whose group type changed by the
/// time the transaction reads it is a conflict, and nothing is written.
#[tokio::test]
async fn a_group_row_changed_after_the_rule_read_refuses_the_membership() {
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, _) = publish_family();
    let (_, other_root, _) = publish_family();
    let (db, raw, _dir) = file_db().await;
    let groups = common::make_group_service(db.clone());
    // Keys the other group's type in the dictionary.
    common::create_root_group(&groups, &ctx, &other_root, "other", tenant).await;
    let g = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;
    let catalog: Arc<dyn RgTypeCatalog> = Arc::new(ChangeRowDuringRuleRead {
        inner: common::make_types_catalog(),
        raw,
        sql: format!(
            "UPDATE resource_group SET gts_type_id = \
             (SELECT id FROM gts_type WHERE schema_id = '{other_root}') \
             WHERE id = X'{}'",
            g.id.as_simple()
        ),
    });
    let members = common::make_membership_service_with(db, catalog);
    let err = members
        .add_membership(&ctx, g.id, &member, "r1")
        .await
        .expect_err("the row changed under the rule");
    assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");
    let stored = members
        .list_memberships(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    assert!(stored.items.is_empty(), "nothing written");
}

/// A membership insert keys its member type in the same transaction, so an
/// insert that fails leaves no dictionary row behind.
#[tokio::test]
async fn a_failed_membership_insert_keys_no_type() {
    let db = common::test_db().await;
    let before = dictionary_rows(&db).await;
    let err = common::insert_membership_directly(&db, Uuid::now_v7(), &code("never_keyed"), "r1")
        .await
        .expect_err("no such group");
    // The dictionary row was written, then the membership's foreign key
    // refused the insert.
    assert!(
        matches!(&err, DomainError::Database(e) if e.to_string().contains("FOREIGN KEY")),
        "{err:?}"
    );
    assert_eq!(dictionary_rows(&db).await, before);
}

/// Existing keys do not admit deleted or unpublished types for new links.
/// Rejected writes add no dictionary rows.
#[tokio::test]
async fn an_existing_dictionary_key_does_not_admit_a_deleted_or_unknown_type() {
    use resource_group::domain::repo::GroupRepositoryTrait;
    use resource_group::infra::storage::group_repo::GroupRepository;

    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db.clone());
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;
    members
        .add_membership(&ctx, group.id, &member, "r1")
        .await
        .unwrap();

    // Never published, but keyed in the dictionary.
    let unknown = code("unknown");
    GroupRepository
        .ensure_type_key(&db.conn().unwrap(), &unknown)
        .await
        .unwrap();
    common::delete_published_type(&root);
    common::delete_published_type(&member);
    let dictionary = dictionary_rows(&db).await;
    assert!(dictionary.iter().any(|(_, c)| c == &root));
    assert!(dictionary.iter().any(|(_, c)| c == &member));
    assert!(dictionary.iter().any(|(_, c)| c == &unknown));

    for (code, what) in [(&root, "deleted"), (&unknown, "unknown")] {
        let err = groups
            .create_group(
                &ctx,
                CreateGroupRequest::new(code.clone(), "x".to_owned()),
                tenant,
            )
            .await
            .expect_err(what);
        assert!(
            matches!(err, DomainError::TypeNotFound { .. }),
            "{what}: {err:?}"
        );
    }
    let (_, live_root, _) = publish_family();
    common::publish_type(CreateTypeRequest {
        code: live_root.clone(),
        can_be_root: true,
        allowed_membership_types: vec![member.clone(), unknown.clone()],
        ..CreateTypeRequest::default()
    });
    let live = common::create_root_group(&groups, &ctx, &live_root, "live", tenant).await;
    let dictionary = dictionary_rows(&db).await;
    for (code, what) in [(&member, "deleted"), (&unknown, "unknown")] {
        members
            .add_membership(&ctx, live.id, code, "r2")
            .await
            .expect_err(what);
    }
    assert_eq!(
        dictionary_rows(&db).await,
        dictionary,
        "refusals add no keys"
    );
    let stored = members
        .list_memberships(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(
        stored.items.len(),
        1,
        "only the original membership: {stored:?}"
    );
}

/// Reads, filters and removals never create dictionary keys.
#[tokio::test]
async fn reads_filters_and_removals_create_no_dictionary_keys() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db.clone());
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;
    members
        .add_membership(&ctx, group.id, &member, "r1")
        .await
        .unwrap();
    let before = dictionary_rows(&db).await;

    let unknown = code("unknown");
    for filter in [
        format!("type eq '{unknown}'"),
        format!("type ne '{unknown}'"),
        format!("not (type in ('{unknown}', '{root}'))"),
    ] {
        let parsed = toolkit_odata::parse_filter_string(&filter).unwrap();
        let query = ODataQuery::default().with_filter(parsed.into_expr());
        groups.list_groups(&ctx, &query).await.expect(&filter);
    }
    let parsed =
        toolkit_odata::parse_filter_string(&format!("resource_type eq '{unknown}'")).unwrap();
    let page = members
        .list_memberships(&ctx, &ODataQuery::default().with_filter(parsed.into_expr()))
        .await
        .unwrap();
    assert!(page.items.is_empty());
    let err = members
        .remove_membership(&ctx, group.id, &unknown, "r1")
        .await
        .expect_err("nothing is stored under an unknown type");
    assert!(
        matches!(err, DomainError::MembershipNotFound { .. }),
        "{err:?}"
    );

    assert_eq!(dictionary_rows(&db).await, before);
}

#[tokio::test]
async fn stored_user_memberships_are_removed_across_schema_versions() {
    common::assert_stored_membership_cleanup(common::test_db().await).await;
}

#[tokio::test]
async fn in_process_type_filter_rejects_lists_over_the_bind_budget() {
    use toolkit_odata::ast::{Expr, Value};
    let db = common::test_db().await;
    let max = toolkit_db::secure::max_bind_params_for(&db.conn().unwrap());
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let ctx = common::make_ctx(Uuid::now_v7());
    for field in ["type", "resource_type"] {
        let query = ODataQuery::default().with_filter(Expr::In(
            Box::new(Expr::Identifier(field.into())),
            vec![Expr::Value(Value::String("unknown".into())); max + 1],
        ));
        let err = if field == "type" {
            groups.list_groups(&ctx, &query).await.unwrap_err()
        } else {
            members.list_memberships(&ctx, &query).await.unwrap_err()
        };
        assert!(
            matches!(err, DomainError::Validation { ref message } if message.contains("bind")),
            "{err:?}"
        );
    }
}
