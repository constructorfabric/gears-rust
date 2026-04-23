// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-traits:p1
use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_odata::{ODataQuery, Page};
use modkit_security::AccessScope;
use resource_group_sdk::models::{
    ResourceGroup, ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::storage::entity::{
    gts_type, resource_group as rg_entity, resource_group_membership as membership_entity,
};

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait GroupRepositoryTrait: Send + Sync + 'static {
    // -- Read operations --

    async fn find_by_id<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ResourceGroup>, DomainError>;

    async fn find_model_by_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError>;

    /// Return the id of *any* existing root group (parent_id IS NULL) whose
    /// `gts_type.schema_id` starts with the given prefix, or `None` when no
    /// such root exists. Used to enforce tenant-root uniqueness
    /// (`cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`).
    ///
    /// Expected to be called inside a `SERIALIZABLE` transaction so the
    /// uniqueness check is race-free against concurrent root creations.
    async fn find_root_id_with_type_prefix<C: DBRunner>(
        &self,
        db: &C,
        type_prefix: &str,
    ) -> Result<Option<Uuid>, DomainError>;

    async fn list_groups<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, DomainError>;

    async fn get_descendants<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError>;

    async fn get_ancestors<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError>;

    // -- Write operations --

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
        tenant_id: Uuid,
    ) -> Result<rg_entity::Model, DomainError>;

    async fn update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<rg_entity::Model, DomainError>;

    async fn delete_by_id<C: DBRunner>(&self, db: &C, id: Uuid) -> Result<(), DomainError>;

    // -- Closure table operations --

    async fn insert_closure_self_row<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError>;

    async fn insert_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        child_id: Uuid,
        parent_id: Uuid,
    ) -> Result<(), DomainError>;

    async fn get_descendant_ids<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError>;

    async fn get_depth<C: DBRunner>(&self, db: &C, group_id: Uuid) -> Result<i32, DomainError>;

    async fn count_children<C: DBRunner>(
        &self,
        db: &C,
        parent_id: Uuid,
    ) -> Result<u64, DomainError>;

    async fn is_descendant<C: DBRunner>(
        &self,
        db: &C,
        potential_ancestor: Uuid,
        potential_descendant: Uuid,
    ) -> Result<bool, DomainError>;

    async fn delete_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        keep_self: bool,
    ) -> Result<(), DomainError>;

    async fn delete_all_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError>;

    async fn rebuild_subtree_closure<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
    ) -> Result<(), DomainError>;

    async fn has_memberships<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<bool, DomainError>;

    async fn delete_memberships<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError>;

    async fn resolve_type_paths_batch<C: DBRunner>(
        &self,
        db: &C,
        type_ids: &[i16],
    ) -> Result<std::collections::HashMap<i16, String>, DomainError>;
}

#[async_trait]
pub trait TypeRepositoryTrait: Send + Sync + 'static {
    async fn find_by_code<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<Option<ResourceGroupType>, DomainError>;

    async fn load_full_type_by_id<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<ResourceGroupType, DomainError>;

    async fn load_full_type<C: DBRunner>(
        &self,
        db: &C,
        type_model: &gts_type::Model,
    ) -> Result<ResourceGroupType, DomainError>;

    async fn resolve_id<C: DBRunner>(&self, db: &C, code: &str)
    -> Result<Option<i16>, DomainError>;

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        schema_id: &str,
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<gts_type::Model, DomainError>;

    async fn insert_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        parent_ids: &[i16],
    ) -> Result<(), DomainError>;

    async fn insert_allowed_membership_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        membership_ids: &[i16],
    ) -> Result<(), DomainError>;

    async fn delete_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<(), DomainError>;

    async fn delete_allowed_membership_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<(), DomainError>;

    async fn update_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        code: &str,
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<gts_type::Model, DomainError>;

    async fn delete_by_id<C: DBRunner>(&self, db: &C, type_id: i16) -> Result<(), DomainError>;

    async fn count_groups_of_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<u64, DomainError>;

    async fn find_groups_using_parent_type<C: DBRunner>(
        &self,
        db: &C,
        child_type_id: i16,
        parent_type_id: i16,
    ) -> Result<Vec<(Uuid, String)>, DomainError>;

    async fn find_root_groups_of_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<Vec<(Uuid, String)>, DomainError>;

    async fn list_types<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, DomainError>;

    async fn resolve_ids<C: DBRunner>(
        &self,
        db: &C,
        codes: &[String],
    ) -> Result<Vec<i16>, DomainError>;
}

#[async_trait]
pub trait MembershipRepositoryTrait: Send + Sync + 'static {
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError>;

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<membership_entity::Model, DomainError>;

    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<u64, DomainError>;

    async fn find_by_composite_key<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Option<membership_entity::Model>, DomainError>;

    async fn get_existing_membership_tenant_ids<C: DBRunner>(
        &self,
        db: &C,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Vec<Uuid>, DomainError>;
}
