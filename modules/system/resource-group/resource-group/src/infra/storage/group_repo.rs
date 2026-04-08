// @cpt-begin:cpt-cf-resource-group-dod-entity-hier-hierarchy-engine:p1:inst-full
//! Persistence layer for resource group entity management.
//!
//! All surrogate SMALLINT ID resolution happens here. The domain and API layers
//! work exclusively with string GTS type paths and UUIDs.

use async_trait::async_trait;
use modkit_db::odata::{LimitCfg, paginate_odata};
use modkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use modkit_odata::{CursorV1, ODataQuery, Page, SortDir};
use modkit_security::AccessScope;
use resource_group_sdk::models::{
    GroupHierarchy, GroupHierarchyWithDepth, ResourceGroup, ResourceGroupWithDepth,
};
use resource_group_sdk::odata::{GroupFilterField, HierarchyFilterField};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::GroupRepositoryTrait;
use crate::infra::storage::entity::{
    gts_type::{self, Entity as GtsTypeEntity},
    resource_group::{self as rg_entity, Entity as ResourceGroupEntity},
    resource_group_closure::{self as closure_entity, Entity as ClosureEntity},
    resource_group_membership::{self as membership_entity, Entity as MembershipEntity},
};
use crate::infra::storage::odata_mapper::GroupODataMapper;
use crate::infra::storage::type_repo::TypeRepository;

/// Type alias for a pinned, boxed, Send future returning `Result<Box<Expr>, DomainError>`.
type ResolveExprFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<Box<modkit_odata::ast::Expr>, DomainError>>
            + Send
            + 'a,
    >,
>;

/// Default `OData` pagination limits for groups.
const GROUP_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// System-level access scope (no tenant/resource filtering).
fn system_scope() -> AccessScope {
    AccessScope::allow_all()
}

// @cpt-dod:cpt-cf-resource-group-dod-entity-hier-hierarchy-engine:p1
/// Repository for resource group persistence operations.
pub struct GroupRepository;

impl GroupRepository {
    // -- Private helper functions --

    /// Resolve a SMALLINT type ID to its GTS type path string.
    async fn resolve_type_path(db: &impl DBRunner, type_id: i16) -> Result<String, DomainError> {
        let scope = system_scope();
        let model = GtsTypeEntity::find()
            .filter(gts_type::Column::Id.eq(type_id))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?
            .ok_or_else(|| DomainError::database(format!("Type ID {type_id} not found")))?;
        Ok(model.schema_id)
    }

    /// Convert a database model to the SDK `ResourceGroup` type.
    fn model_to_resource_group(model: rg_entity::Model, type_path: String) -> ResourceGroup {
        ResourceGroup {
            id: model.id,
            type_path,
            name: model.name,
            hierarchy: GroupHierarchy {
                parent_id: model.parent_id,
                tenant_id: model.tenant_id,
            },
            metadata: model.metadata,
        }
    }

    /// Encode an offset value into a `CursorV1`-compatible base64url token.
    ///
    /// The hierarchy endpoint uses offset-based pagination (not keyset) because
    /// results are assembled in memory from two separate queries. The offset is
    /// stored in the `k` field and a fixed sort signature `"depth"` distinguishes
    /// these cursors from keyset cursors used by `paginate_odata`.
    fn encode_offset_cursor(offset: usize, direction: &str) -> Option<String> {
        let cursor = CursorV1 {
            k: vec![offset.to_string()],
            o: SortDir::Asc,
            s: "depth".to_owned(),
            f: None,
            d: direction.to_owned(),
        };
        cursor.encode().ok()
    }

    /// Resolve GTS type path strings in `type` filter values to SMALLINT IDs.
    ///
    /// The API exposes `type` as a string field, but the DB column `gts_type_id`
    /// is SMALLINT. This method walks the filter AST and replaces string values
    /// adjacent to `type` identifiers with their resolved SMALLINT IDs.
    async fn resolve_type_filter(
        db: &impl DBRunner,
        query: &ODataQuery,
    ) -> Result<ODataQuery, DomainError> {
        let Some(filter) = &query.filter else {
            return Ok(query.clone());
        };

        let resolved = Self::resolve_type_expr(db, filter).await?;
        let mut q = query.clone();
        q.filter = Some(resolved);
        Ok(q)
    }

    /// Resolve `type` field string values to SMALLINT IDs in a filter AST.
    ///
    /// Non-recursive: only transforms leaf `Compare` and `In` nodes where the
    /// identifier is `"type"`. Recurses through `And`/`Or`/`Not` via `Box::pin`.
    fn resolve_type_expr<'a>(
        db: &'a (impl DBRunner + 'a),
        expr: &'a modkit_odata::ast::Expr,
    ) -> ResolveExprFuture<'a> {
        use modkit_odata::ast::{Expr as E, Value as V};

        Box::pin(async move {
            Ok(Box::new(match expr {
                E::And(l, r) => E::And(
                    Self::resolve_type_expr(db, l).await?,
                    Self::resolve_type_expr(db, r).await?,
                ),
                E::Or(l, r) => E::Or(
                    Self::resolve_type_expr(db, l).await?,
                    Self::resolve_type_expr(db, r).await?,
                ),
                E::Not(inner) => E::Not(Self::resolve_type_expr(db, inner).await?),
                E::Compare(left, op, right) => {
                    if let E::Identifier(name) = left.as_ref()
                        && name == "type"
                        && let E::Value(V::String(path)) = right.as_ref()
                    {
                        let id = TypeRepository::resolve_id(db, path).await?.ok_or_else(|| {
                            DomainError::validation(format!("Unknown type in filter: {path}"))
                        })?;
                        return Ok(Box::new(E::Compare(
                            left.clone(),
                            *op,
                            Box::new(E::Value(V::Number(id.into()))),
                        )));
                    }
                    expr.clone()
                }
                E::In(left, list) => {
                    if let E::Identifier(name) = left.as_ref()
                        && name == "type"
                    {
                        let mut resolved = Vec::with_capacity(list.len());
                        for item in list {
                            if let E::Value(V::String(path)) = item {
                                let id = TypeRepository::resolve_id(db, path).await?.ok_or_else(
                                    || {
                                        DomainError::validation(format!(
                                            "Unknown type in filter: {path}"
                                        ))
                                    },
                                )?;
                                resolved.push(E::Value(V::Number(id.into())));
                            } else {
                                resolved.push(item.clone());
                            }
                        }
                        return Ok(Box::new(E::In(left.clone(), resolved)));
                    }
                    expr.clone()
                }
                _ => expr.clone(),
            }))
        })
    }

    /// Parse and extract hierarchy filters from an `OData` query.
    fn parse_hierarchy_filter(query: &ODataQuery) -> (Option<DepthFilter>, Option<TypeFilter>) {
        let Some(filter_expr) = query.filter() else {
            return (None, None);
        };

        let Ok(filter_node) =
            modkit_odata::filter::convert_expr_to_filter_node::<HierarchyFilterField>(filter_expr)
        else {
            return (None, None);
        };

        let depth = Self::extract_depth_from_node(&filter_node);
        let type_f = Self::extract_type_from_hierarchy_node(&filter_node);
        (depth, type_f)
    }

    fn extract_depth_from_node(
        node: &modkit_odata::filter::FilterNode<HierarchyFilterField>,
    ) -> Option<DepthFilter> {
        use modkit_odata::filter::{FilterNode, FilterOp};

        match node {
            FilterNode::Binary {
                field: HierarchyFilterField::HierarchyDepth,
                op,
                value,
            } => {
                let v = match value {
                    modkit_odata::filter::ODataValue::Number(n) => {
                        // BigDecimal to i32
                        n.to_string().parse::<i32>().ok()?
                    }
                    _ => return None,
                };
                Some(DepthFilter::Single(*op, v))
            }
            FilterNode::Composite {
                op: FilterOp::And,
                children,
            } => {
                let mut filters = Vec::new();
                for child in children {
                    if let Some(f) = Self::extract_depth_from_node(child) {
                        filters.push(f);
                    }
                }
                if filters.is_empty() {
                    None
                } else if filters.len() == 1 {
                    Some(filters.remove(0))
                } else {
                    Some(DepthFilter::And(filters))
                }
            }
            _ => None,
        }
    }

    fn extract_type_from_hierarchy_node(
        node: &modkit_odata::filter::FilterNode<HierarchyFilterField>,
    ) -> Option<TypeFilter> {
        use modkit_odata::filter::{FilterNode, FilterOp};

        match node {
            FilterNode::Binary {
                field: HierarchyFilterField::Type,
                op: FilterOp::Eq,
                value,
            } => {
                if let modkit_odata::filter::ODataValue::String(s) = value {
                    Some(TypeFilter::Eq(s.clone()))
                } else {
                    None
                }
            }
            FilterNode::Composite {
                op: FilterOp::And,
                children,
            } => {
                for child in children {
                    if let Some(f) = Self::extract_type_from_hierarchy_node(child) {
                        return Some(f);
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[async_trait]
impl GroupRepositoryTrait for GroupRepository {
    // -- Read operations --

    /// Find a resource group by its UUID, returning the SDK model with resolved type path.
    ///
    /// Uses the provided `AccessScope` for tenant-level filtering (`SecureORM`).
    async fn find_by_id<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ResourceGroup>, DomainError> {
        let model = ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        match model {
            Some(m) => {
                let type_path = Self::resolve_type_path(db, m.gts_type_id).await?;
                Ok(Some(Self::model_to_resource_group(m, type_path)))
            }
            None => Ok(None),
        }
    }

    /// Find the raw entity model by ID.
    async fn find_model_by_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError> {
        let scope = system_scope();
        ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.eq(id))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))
    }

    /// List groups with `OData` filtering and pagination.
    ///
    /// The `type` filter field accepts GTS type path strings from the API
    /// (e.g. `$filter=type eq 'gts.x.system.rg.type.v1~x.test.org.v1~'`).
    /// Before passing to `SeaORM`, string values for the `type` field are
    /// resolved to SMALLINT surrogate IDs at the persistence boundary.
    /// List groups with `OData` filtering and pagination.
    ///
    /// Uses the provided `AccessScope` for tenant-level filtering (`SecureORM`).
    async fn list_groups<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, DomainError> {
        // Pre-resolve: transform `type` string values → SMALLINT IDs in filter AST
        let resolved_query = Self::resolve_type_filter(db, query).await?;

        let base_query = ResourceGroupEntity::find().secure().scope_with(scope);

        let page = paginate_odata::<GroupFilterField, GroupODataMapper, _, _, _, _>(
            base_query,
            db,
            &resolved_query,
            ("id", SortDir::Desc),
            GROUP_LIMIT_CFG,
            |m: rg_entity::Model| m,
        )
        .await
        .map_err(|e| DomainError::database(e.to_string()))?;

        // Batch-resolve type paths for all groups in the page (single query)
        let type_ids: Vec<i16> = page.items.iter().map(|m| m.gts_type_id).collect();
        let type_map = self.resolve_type_paths_batch(db, &type_ids).await?;

        let groups = page
            .items
            .into_iter()
            .map(|model| {
                let type_path = type_map
                    .get(&model.gts_type_id)
                    .cloned()
                    .unwrap_or_default();
                Self::model_to_resource_group(model, type_path)
            })
            .collect();

        Ok(Page {
            items: groups,
            page_info: page.page_info,
        })
    }

    /// Query hierarchy from a reference group, returning groups with relative depth.
    ///
    /// Uses the provided `AccessScope` for tenant-level filtering (`SecureORM`).
    async fn list_hierarchy<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        // Parse OData depth and type filters early so we can push depth bounds into SQL
        let (depth_filter, type_filter) = Self::parse_hierarchy_filter(query);

        // Get closure rows involving this group (both as ancestor and descendant).
        // Push depth bounds into SQL when available to avoid loading unbounded data.
        let sys = system_scope(); // closure table has no tenant column, use system scope

        let mut desc_query =
            ClosureEntity::find().filter(closure_entity::Column::AncestorId.eq(group_id));
        if let Some(max_desc) = depth_filter
            .as_ref()
            .and_then(DepthFilter::max_descendant_depth)
            && max_desc >= 0
        {
            desc_query = desc_query.filter(closure_entity::Column::Depth.lte(max_desc));
        }
        let ancestor_rows = desc_query
            .secure()
            .scope_with(&sys)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let mut anc_query = ClosureEntity::find()
            .filter(closure_entity::Column::DescendantId.eq(group_id))
            .filter(closure_entity::Column::Depth.ne(0)); // exclude self-row
        if let Some(max_anc) = depth_filter
            .as_ref()
            .and_then(DepthFilter::max_ancestor_depth)
            && max_anc > 0
        {
            anc_query = anc_query.filter(closure_entity::Column::Depth.lte(max_anc));
        }
        let descendant_rows = anc_query
            .secure()
            .scope_with(&sys)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // Build map of group_id -> relative_depth
        let mut group_depths: Vec<(Uuid, i32)> = Vec::new();

        // Descendants: depth is positive (as stored in closure)
        for row in &ancestor_rows {
            group_depths.push((row.descendant_id, row.depth));
        }

        // Ancestors: depth is negative (negate the stored depth)
        for row in &descendant_rows {
            group_depths.push((row.ancestor_id, -row.depth));
        }

        // Load all referenced groups
        let group_ids: Vec<Uuid> = group_depths.iter().map(|(id, _)| *id).collect();
        if group_ids.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                page_info: modkit_odata::PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: query.limit.unwrap_or(25).min(200),
                    has_next_page: false,
                    has_previous_page: false,
                },
            });
        }

        let groups = ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.is_in(group_ids.clone()))
            .secure()
            .scope_with(scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let group_map: std::collections::HashMap<Uuid, rg_entity::Model> =
            groups.into_iter().map(|g| (g.id, g)).collect();

        // Batch-resolve type paths for all groups (single query)
        let all_type_ids: Vec<i16> = group_map.values().map(|g| g.gts_type_id).collect();
        let type_path_map = self.resolve_type_paths_batch(db, &all_type_ids).await?;

        // Build results with type path lookup and filtering
        let mut results: Vec<ResourceGroupWithDepth> = Vec::new();
        for (gid, depth) in &group_depths {
            // Apply depth filter
            if let Some(ref df) = depth_filter
                && !df.matches(*depth)
            {
                continue;
            }

            if let Some(model) = group_map.get(gid) {
                let type_path = type_path_map
                    .get(&model.gts_type_id)
                    .cloned()
                    .unwrap_or_default();

                // Apply type filter
                if let Some(ref tf) = type_filter
                    && !tf.matches(&type_path)
                {
                    continue;
                }

                results.push(ResourceGroupWithDepth {
                    id: model.id,
                    type_path,
                    name: model.name.clone(),
                    hierarchy: GroupHierarchyWithDepth {
                        parent_id: model.parent_id,
                        tenant_id: model.tenant_id,
                        depth: *depth,
                    },
                    metadata: model.metadata.clone(),
                });
            }
        }

        // Sort by depth for consistent ordering
        results.sort_by_key(|r| r.hierarchy.depth);

        // Parse offset from cursor (offset-based pagination for in-memory results)
        let offset = query
            .cursor
            .as_ref()
            .and_then(|c| c.k.first())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);

        let limit_val = query.limit.unwrap_or(25).min(200);
        let limit_usize = limit_val as usize;
        let total = results.len();

        // Apply offset + limit to get the current page
        let items: Vec<ResourceGroupWithDepth> =
            results.into_iter().skip(offset).take(limit_usize).collect();

        let has_next = offset + limit_usize < total;
        let has_prev = offset > 0;

        // Encode next/prev cursors using CursorV1 for round-trip compatibility
        // with the OData extractor (which decodes cursor via CursorV1::decode).
        let next_cursor = if has_next {
            let next_offset = offset + limit_usize;
            Self::encode_offset_cursor(next_offset, "fwd")
        } else {
            None
        };

        let prev_cursor = if has_prev {
            let prev_offset = offset.saturating_sub(limit_usize);
            Self::encode_offset_cursor(prev_offset, "bwd")
        } else {
            None
        };

        Ok(Page {
            items,
            page_info: modkit_odata::PageInfo {
                next_cursor,
                prev_cursor,
                limit: limit_val,
                has_next_page: has_next,
                has_previous_page: has_prev,
            },
        })
    }

    // -- Write operations --

    /// Insert a new resource group entity.
    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
        tenant_id: Uuid,
    ) -> Result<rg_entity::Model, DomainError> {
        let scope = system_scope();

        let model = rg_entity::ActiveModel {
            id: Set(id),
            parent_id: Set(parent_id),
            gts_type_id: Set(gts_type_id),
            name: Set(name.to_owned()),
            metadata: Set(metadata.cloned()),
            tenant_id: Set(tenant_id),
            ..Default::default()
        };

        modkit_db::secure::secure_insert::<ResourceGroupEntity>(model, &scope, db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        self.find_model_by_id(db, id)
            .await?
            .ok_or_else(|| DomainError::database("Insert succeeded but row not found"))
    }

    /// Update a resource group entity.
    async fn update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<rg_entity::Model, DomainError> {
        let scope = system_scope();

        let parent_val: sea_orm::Value = match parent_id {
            Some(pid) => sea_orm::Value::Uuid(Some(Box::new(pid))),
            None => sea_orm::Value::Uuid(None),
        };

        let metadata_val: sea_orm::Value = match metadata {
            Some(v) => sea_orm::Value::Json(Some(Box::new(v.clone()))),
            None => sea_orm::Value::Json(None),
        };

        ResourceGroupEntity::update_many()
            .filter(rg_entity::Column::Id.eq(id))
            .secure()
            .col_expr(rg_entity::Column::ParentId, Expr::value(parent_val))
            .col_expr(rg_entity::Column::GtsTypeId, Expr::value(gts_type_id))
            .col_expr(rg_entity::Column::Name, Expr::value(name.to_owned()))
            .col_expr(rg_entity::Column::Metadata, Expr::value(metadata_val))
            .col_expr(
                rg_entity::Column::UpdatedAt,
                Expr::value(time::OffsetDateTime::now_utc()),
            )
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        self.find_model_by_id(db, id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(id))
    }

    /// Delete a resource group entity by ID.
    async fn delete_by_id<C: DBRunner>(&self, db: &C, id: Uuid) -> Result<(), DomainError> {
        let scope = system_scope();
        ResourceGroupEntity::delete_many()
            .filter(rg_entity::Column::Id.eq(id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    // -- Closure table operations --

    /// Insert a self-row in the closure table (depth=0).
    async fn insert_closure_self_row<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        let model = closure_entity::ActiveModel {
            ancestor_id: Set(group_id),
            descendant_id: Set(group_id),
            depth: Set(0),
        };
        modkit_db::secure::secure_insert::<ClosureEntity>(model, &scope, db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    /// Insert ancestor closure rows for a new child group.
    /// For each ancestor of the parent, create a row linking ancestor -> child with depth+1.
    async fn insert_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        child_id: Uuid,
        parent_id: Uuid,
    ) -> Result<(), DomainError> {
        let scope = system_scope();

        // Get all ancestors of the parent (including parent's self-row)
        let parent_ancestors = ClosureEntity::find()
            .filter(closure_entity::Column::DescendantId.eq(parent_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // For each ancestor of parent, create ancestor -> child with depth+1
        for ancestor_row in parent_ancestors {
            let model = closure_entity::ActiveModel {
                ancestor_id: Set(ancestor_row.ancestor_id),
                descendant_id: Set(child_id),
                depth: Set(ancestor_row.depth + 1),
            };
            modkit_db::secure::secure_insert::<ClosureEntity>(model, &scope, db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }

        Ok(())
    }

    /// Get all descendants of a group (from closure table, excluding self-row).
    async fn get_descendant_ids<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError> {
        let scope = system_scope();
        let rows = ClosureEntity::find()
            .filter(closure_entity::Column::AncestorId.eq(group_id))
            .filter(closure_entity::Column::Depth.ne(0))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.descendant_id).collect())
    }

    /// Get the depth of a group from its root (max depth in closure table where
    /// this group is the descendant).
    async fn get_depth<C: DBRunner>(&self, db: &C, group_id: Uuid) -> Result<i32, DomainError> {
        let scope = system_scope();
        let rows = ClosureEntity::find()
            .filter(closure_entity::Column::DescendantId.eq(group_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.depth).max().unwrap_or(0))
    }

    /// Count direct children of a group.
    async fn count_children<C: DBRunner>(
        &self,
        db: &C,
        parent_id: Uuid,
    ) -> Result<u64, DomainError> {
        let scope = system_scope();
        let count = ResourceGroupEntity::find()
            .filter(rg_entity::Column::ParentId.eq(parent_id))
            .secure()
            .scope_with(&scope)
            .count(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(count)
    }

    /// Check if a group is a descendant of another group (for cycle detection).
    async fn is_descendant<C: DBRunner>(
        &self,
        db: &C,
        potential_ancestor: Uuid,
        potential_descendant: Uuid,
    ) -> Result<bool, DomainError> {
        let scope = system_scope();
        let row = ClosureEntity::find()
            .filter(closure_entity::Column::AncestorId.eq(potential_ancestor))
            .filter(closure_entity::Column::DescendantId.eq(potential_descendant))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(row.is_some())
    }

    /// Delete all closure rows where a given group is the descendant
    /// (its ancestor paths). Keeps the self-row if `keep_self` is true.
    async fn delete_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        keep_self: bool,
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        let mut query =
            ClosureEntity::delete_many().filter(closure_entity::Column::DescendantId.eq(group_id));

        if keep_self {
            query = query.filter(closure_entity::Column::Depth.ne(0));
        }

        query
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    /// Delete ALL closure rows for a group (both as ancestor and descendant).
    async fn delete_all_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError> {
        let scope = system_scope();

        // Delete rows where group is ancestor
        ClosureEntity::delete_many()
            .filter(closure_entity::Column::AncestorId.eq(group_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // Delete rows where group is descendant
        ClosureEntity::delete_many()
            .filter(closure_entity::Column::DescendantId.eq(group_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(())
    }

    // @cpt-algo:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1
    /// Rebuild closure rows for a subtree after a move operation.
    /// This deletes old ancestor paths for the entire subtree and
    /// inserts new paths based on the new parent.
    async fn rebuild_subtree_closure<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-1
        // Collect subtree: SELECT descendant_id FROM resource_group_closure WHERE ancestor_id = group_id
        let scope = system_scope();
        let subtree_rows = ClosureEntity::find()
            .filter(closure_entity::Column::AncestorId.eq(group_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let subtree_ids: Vec<Uuid> = subtree_rows.iter().map(|r| r.descendant_id).collect();
        let subtree_internal: std::collections::HashMap<Uuid, i32> = subtree_rows
            .iter()
            .map(|r| (r.descendant_id, r.depth))
            .collect();
        // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-1

        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-2
        // Delete affected paths: DELETE FROM resource_group_closure
        // WHERE descendant_id IN (subtree) AND ancestor_id NOT IN (subtree)
        let subtree_set: std::collections::HashSet<Uuid> = subtree_ids.iter().copied().collect();

        let all_desc_rows = ClosureEntity::find()
            .filter(closure_entity::Column::DescendantId.is_in(subtree_ids.clone()))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // Collect (ancestor_id, descendant_id) pairs where ancestor is outside subtree
        let external_pairs: Vec<(Uuid, Uuid)> = all_desc_rows
            .iter()
            .filter(|r| !subtree_set.contains(&r.ancestor_id))
            .map(|r| (r.ancestor_id, r.descendant_id))
            .collect();

        // Batch-delete: delete rows where ancestor is NOT in subtree for each subtree descendant.
        // Group by descendant_id to minimize queries.
        if !external_pairs.is_empty() {
            // Delete all external ancestor rows for all subtree descendants in one query
            // We delete rows where descendant_id IN (subtree) AND ancestor_id NOT IN (subtree)
            let external_ancestor_ids: Vec<Uuid> = external_pairs
                .iter()
                .map(|(a, _)| *a)
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            ClosureEntity::delete_many()
                .filter(closure_entity::Column::DescendantId.is_in(subtree_ids.clone()))
                .filter(closure_entity::Column::AncestorId.is_in(external_ancestor_ids))
                .secure()
                .scope_with(&scope)
                .exec(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }
        // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-2

        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-3
        // Compute new ancestor paths from new parent
        if let Some(parent_id) = new_parent_id {
            let parent_ancestors = ClosureEntity::find()
                .filter(closure_entity::Column::DescendantId.eq(parent_id))
                .secure()
                .scope_with(&scope)
                .all(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-3

            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4
            let mut new_rows: Vec<closure_entity::ActiveModel> = Vec::new();
            for ancestor_row in &parent_ancestors {
                // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a
                for &desc_id in &subtree_ids {
                    // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a1
                    let internal_depth = subtree_internal.get(&desc_id).copied().unwrap_or(0);
                    let new_depth = ancestor_row.depth + 1 + internal_depth;
                    new_rows.push(closure_entity::ActiveModel {
                        ancestor_id: Set(ancestor_row.ancestor_id),
                        descendant_id: Set(desc_id),
                        depth: Set(new_depth),
                    });
                    // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a1
                }
                // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a
            }

            for row in new_rows {
                modkit_db::secure::secure_insert::<ClosureEntity>(row, &scope, db)
                    .await
                    .map_err(|e| DomainError::database(e.to_string()))?;
            }
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4
        }

        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-5
        // RETURN: closure rows updated within transaction — commit handled by caller
        Ok(())
        // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-5
    }

    /// Check if a group has any memberships.
    async fn has_memberships<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<bool, DomainError> {
        let scope = system_scope();
        let count = MembershipEntity::find()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .secure()
            .scope_with(&scope)
            .count(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(count > 0)
    }

    /// Delete all memberships for a group.
    async fn delete_memberships<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        MembershipEntity::delete_many()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    /// Batch-resolve SMALLINT type IDs to GTS type path strings.
    ///
    /// Issues a single `SELECT ... WHERE id IN (...)` query for all distinct type IDs,
    /// returning a `HashMap` for O(1) lookup. Eliminates N+1 queries in list operations.
    async fn resolve_type_paths_batch<C: DBRunner>(
        &self,
        db: &C,
        type_ids: &[i16],
    ) -> Result<std::collections::HashMap<i16, String>, DomainError> {
        use std::collections::HashMap;

        if type_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let unique_ids: Vec<i16> = {
            let mut ids: Vec<i16> = type_ids.to_vec();
            ids.sort_unstable();
            ids.dedup();
            ids
        };

        let scope = system_scope();
        let models = GtsTypeEntity::find()
            .filter(gts_type::Column::Id.is_in(unique_ids))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(models.into_iter().map(|m| (m.id, m.schema_id)).collect())
    }
}

/// Depth filter for hierarchy queries.
enum DepthFilter {
    Single(modkit_odata::filter::FilterOp, i32),
    And(Vec<DepthFilter>),
}

impl DepthFilter {
    fn matches(&self, depth: i32) -> bool {
        use modkit_odata::filter::FilterOp;
        match self {
            Self::Single(op, v) => match op {
                FilterOp::Eq => depth == *v,
                FilterOp::Ne => depth != *v,
                FilterOp::Gt => depth > *v,
                FilterOp::Ge => depth >= *v,
                FilterOp::Lt => depth < *v,
                FilterOp::Le => depth <= *v,
                _ => true, // Unsupported ops pass through
            },
            Self::And(filters) => filters.iter().all(|f| f.matches(depth)),
        }
    }

    /// Derive the maximum descendant depth (positive) implied by this filter.
    /// Returns `None` if no upper bound can be derived.
    fn max_descendant_depth(&self) -> Option<i32> {
        use modkit_odata::filter::FilterOp;
        match self {
            Self::Single(op, v) => match op {
                FilterOp::Eq | FilterOp::Le => Some(*v),
                FilterOp::Lt => Some(*v - 1),
                _ => None,
            },
            Self::And(filters) => filters.iter().filter_map(Self::max_descendant_depth).min(),
        }
    }

    /// Derive the maximum ancestor depth (positive closure depth) implied by this filter.
    /// Since ancestors have negative relative depth, `depth ge -3` means closure depth <= 3.
    /// Returns `None` if no lower bound can be derived.
    fn max_ancestor_depth(&self) -> Option<i32> {
        use modkit_odata::filter::FilterOp;
        match self {
            Self::Single(op, v) => match op {
                FilterOp::Eq | FilterOp::Ge => Some(v.abs()),
                FilterOp::Gt => Some((v - 1).abs()),
                _ => None,
            },
            Self::And(filters) => filters.iter().filter_map(Self::max_ancestor_depth).min(),
        }
    }
}

/// Type filter for hierarchy queries.
enum TypeFilter {
    Eq(String),
}

impl TypeFilter {
    fn matches(&self, type_path: &str) -> bool {
        match self {
            Self::Eq(s) => type_path == s,
        }
    }
}
// @cpt-end:cpt-cf-resource-group-dod-entity-hier-hierarchy-engine:p1:inst-full
