// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-entity-hier-hierarchy-engine:p1:inst-full
//! Persistence layer for resource group entity management.
//!
//! All surrogate SMALLINT ID resolution happens here. The domain and API layers
//! work exclusively with string GTS type paths and UUIDs.

use async_trait::async_trait;
use resource_group_sdk::models::{
    GroupHierarchy, GroupHierarchyWithDepth, ResourceGroup, ResourceGroupWithDepth,
};
use resource_group_sdk::odata::{GroupFilterField, HierarchyFilterField};
use sea_orm::sea_query::{Alias, Expr, Query};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_odata::{CursorV1, ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
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
            code: type_path,
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

    /// Shared helper: given raw `(group_id, depth)` pairs, load groups, resolve
    /// type paths, apply `OData` filters, paginate, and return a `Page`.
    async fn build_hierarchy_page(
        &self,
        db: &impl DBRunner,
        scope: &AccessScope,
        query: &ODataQuery,
        group_depths: Vec<(Uuid, i32)>,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let (depth_filter, type_filter) = Self::parse_hierarchy_filter(query);

        let group_ids: Vec<Uuid> = group_depths.iter().map(|(id, _)| *id).collect();
        if group_ids.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                page_info: toolkit_odata::PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: query.limit.unwrap_or(25).min(200),
                },
            });
        }

        // This id list is the whole subtree, bounded by the data rather than
        // by the page size -- the pagination below happens in memory, after
        // the read. One bind parameter per id fails in the driver on a large
        // subtree, exactly as the batch deletes would without chunking.
        //
        // Half the ceiling, not all of it: `scope` compiles to predicates that
        // bind parameters of their own -- `ScopeFilter::In` and `InGroup` both
        // carry value lists -- and this call cannot see how many. A scope
        // carrying more than half the ceiling in values is a scope-side
        // problem that no chunk size chosen here can fix.
        let id_budget = toolkit_db::secure::max_bind_params_for(db)
            .div_euclid(2)
            .max(1);
        let mut group_map: std::collections::HashMap<Uuid, rg_entity::Model> =
            std::collections::HashMap::with_capacity(group_ids.len());
        for chunk in group_ids.chunks(id_budget) {
            let groups = ResourceGroupEntity::find()
                .filter(rg_entity::Column::Id.is_in(chunk.to_vec()))
                .secure()
                .scope_with(scope)
                .all(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
            group_map.extend(groups.into_iter().map(|g| (g.id, g)));
        }

        let all_type_ids: Vec<i16> = group_map.values().map(|g| g.gts_type_id).collect();
        let type_path_map = self.resolve_type_paths_batch(db, &all_type_ids).await?;

        let mut results: Vec<ResourceGroupWithDepth> = Vec::new();
        for (gid, depth) in &group_depths {
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
                if let Some(ref tf) = type_filter
                    && !tf.matches(&type_path)
                {
                    continue;
                }
                results.push(ResourceGroupWithDepth {
                    id: model.id,
                    code: type_path,
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

        results.sort_by(|a, b| {
            a.hierarchy
                .depth
                .cmp(&b.hierarchy.depth)
                .then_with(|| a.id.cmp(&b.id))
        });

        let offset = query
            .cursor
            .as_ref()
            .and_then(|c| c.k.first())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let limit_val = query.limit.unwrap_or(25).min(200);
        let limit_usize = limit_val as usize;
        let total = results.len();
        let items: Vec<ResourceGroupWithDepth> =
            results.into_iter().skip(offset).take(limit_usize).collect();

        let next_cursor = if offset + limit_usize < total {
            Self::encode_offset_cursor(offset + limit_usize, "fwd")
        } else {
            None
        };
        let prev_cursor = if offset > 0 {
            Self::encode_offset_cursor(offset.saturating_sub(limit_usize), "bwd")
        } else {
            None
        };

        Ok(Page {
            items,
            page_info: toolkit_odata::PageInfo {
                next_cursor,
                prev_cursor,
                limit: limit_val,
            },
        })
    }

    /// Resolve `type` string values to SMALLINT IDs in a validated `FilterNode`.
    ///
    /// Called AFTER `convert_expr_to_filter_node` validates the filter (String kind
    /// for `type` field). Walks the tree and replaces GTS string values
    /// with `Value::Number(id)` for `GroupFilterField::Type` fields. The resolved
    /// numeric value is then handled by `filter_node_to_condition` which converts
    /// it to `sea_orm::Value::BigInt` — `PostgreSQL` implicitly casts to SMALLINT.
    #[allow(clippy::type_complexity)]
    /// Collect every GTS type path a `type` predicate references, anywhere
    /// in the filter tree.
    fn collect_type_filter_paths(
        node: &toolkit_odata::filter::FilterNode<GroupFilterField>,
        out: &mut Vec<String>,
    ) {
        use toolkit_odata::ast::Value as V;
        use toolkit_odata::filter::FilterNode as FN;
        match node {
            FN::Binary {
                field: GroupFilterField::Type,
                value: V::String(path),
                ..
            } => out.push(path.clone()),
            FN::InList {
                field: GroupFilterField::Type,
                values,
            } => {
                for v in values {
                    if let V::String(path) = v {
                        out.push(path.clone());
                    }
                }
            }
            FN::Composite { children, .. } => {
                for child in children {
                    Self::collect_type_filter_paths(child, out);
                }
            }
            FN::Not(inner) => Self::collect_type_filter_paths(inner, out),
            _ => {}
        }
    }

    /// Rewrite every `type` predicate to compare against the surrogate id,
    /// using an already-resolved path -> id map. Purely in memory.
    fn substitute_type_filter_ids(
        node: &toolkit_odata::filter::FilterNode<GroupFilterField>,
        ids: &std::collections::HashMap<String, i16>,
    ) -> Result<toolkit_odata::filter::FilterNode<GroupFilterField>, DomainError> {
        use toolkit_odata::ast::Value as V;
        use toolkit_odata::filter::FilterNode as FN;
        let unknown =
            |path: &str| DomainError::validation(format!("Unknown type in filter: {path}"));
        Ok(match node {
            FN::Binary {
                field: GroupFilterField::Type,
                op,
                value: V::String(path),
            } => FN::Binary {
                field: GroupFilterField::Type,
                op: *op,
                value: V::Number((*ids.get(path).ok_or_else(|| unknown(path))?).into()),
            },
            FN::InList {
                field: GroupFilterField::Type,
                values,
            } => {
                let mut resolved = Vec::with_capacity(values.len());
                for v in values {
                    if let V::String(path) = v {
                        resolved.push(V::Number(
                            (*ids.get(path).ok_or_else(|| unknown(path))?).into(),
                        ));
                    } else {
                        resolved.push(v.clone());
                    }
                }
                FN::InList {
                    field: GroupFilterField::Type,
                    values: resolved,
                }
            }
            FN::Composite { op, children } => FN::Composite {
                op: *op,
                children: children
                    .iter()
                    .map(|c| Self::substitute_type_filter_ids(c, ids))
                    .collect::<Result<Vec<_>, _>>()?,
            },
            FN::Not(inner) => FN::Not(Box::new(Self::substitute_type_filter_ids(inner, ids)?)),
            other => other.clone(),
        })
    }

    /// Resolve every `type` predicate in the tree to its surrogate id.
    ///
    /// Two passes in memory around a single query, rather than one query
    /// per referenced path: a `type in (...)` filter with N values used to
    /// cost N `gts_type` SELECTs before the page query even ran (N+1 audit
    /// finding (b)).
    async fn resolve_type_filter_node(
        db: &impl DBRunner,
        node: &toolkit_odata::filter::FilterNode<GroupFilterField>,
    ) -> Result<toolkit_odata::filter::FilterNode<GroupFilterField>, DomainError> {
        let mut paths = Vec::new();
        Self::collect_type_filter_paths(node, &mut paths);
        if paths.is_empty() {
            return Ok(node.clone());
        }
        paths.sort_unstable();
        paths.dedup();

        // These paths come straight out of the client's `$filter`, so their
        // number is the caller's choice, but it is not unbounded: the HTTP
        // extractor rejects the request before it gets here if the raw
        // filter exceeds `MAX_FILTER_LEN` (8 KiB) or parses to more than
        // `MAX_NODES` (2000) nodes (`libs/toolkit/src/api/odata.rs`).
        // `toolkit_odata::ODataLimits::validate_filter` is a parallel
        // mechanism that would enforce its own bound -- it is simply dead
        // code, never called anywhere in this workspace, not a second layer
        // actually protecting this path. The chunking below is
        // defense-in-depth against the extractor's ceiling, not the only
        // barrier standing between a client and an oversized `IN (...)`, but
        // it's still needed: 2000 distinct paths is comfortably past most
        // backends' per-statement bind limit. Chunked against the bind
        // ceiling like every other client-fed list here -- `type_repo`'s
        // `resolve_ids` and the removed-parent sweep already are, and this
        // was the one that was not.
        let scope = system_scope();
        let mut ids: std::collections::HashMap<String, i16> = std::collections::HashMap::new();
        for chunk in paths.chunks(toolkit_db::secure::max_bind_params_for(db)) {
            let rows = GtsTypeEntity::find()
                .filter(gts_type::Column::SchemaId.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .all(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
            ids.extend(rows.into_iter().map(|t| (t.schema_id, t.id)));
        }

        Self::substitute_type_filter_ids(node, &ids)
    }

    /// Parse and extract hierarchy filters from an `OData` query.
    fn parse_hierarchy_filter(query: &ODataQuery) -> (Option<DepthFilter>, Option<TypeFilter>) {
        let Some(filter_expr) = query.filter() else {
            return (None, None);
        };

        let filter_node = match toolkit_odata::filter::convert_expr_to_filter_node::<
            HierarchyFilterField,
        >(filter_expr)
        {
            Ok(node) => node,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "hierarchy $filter could not be typed (e.g. Or/Ne/In on hierarchy fields); falling back to superset + in-memory filter"
                );
                return (None, None);
            }
        };

        let depth = Self::extract_depth_from_node(&filter_node);
        let type_f = Self::extract_type_from_hierarchy_node(&filter_node);
        (depth, type_f)
    }

    fn extract_depth_from_node(
        node: &toolkit_odata::filter::FilterNode<HierarchyFilterField>,
    ) -> Option<DepthFilter> {
        use toolkit_odata::filter::{FilterNode, FilterOp};

        match node {
            FilterNode::Binary {
                field: HierarchyFilterField::HierarchyDepth,
                op,
                value,
            } => {
                let v = match value {
                    toolkit_odata::filter::ODataValue::Number(n) => {
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
        node: &toolkit_odata::filter::FilterNode<HierarchyFilterField>,
    ) -> Option<TypeFilter> {
        use toolkit_odata::filter::{FilterNode, FilterOp};

        match node {
            FilterNode::Binary {
                field: HierarchyFilterField::Type,
                op: FilterOp::Eq,
                value,
            } => {
                if let toolkit_odata::filter::ODataValue::String(s) = value {
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

    async fn find_model_by_id_for_update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError> {
        use sea_orm::QuerySelect;
        let scope = system_scope();
        ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.eq(id))
            .lock(sea_orm::sea_query::LockType::Update)
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))
    }

    /// Return the id of any existing root group (`parent_id IS NULL`) whose
    /// `gts_type.schema_id` starts with the given prefix, or `None` when no
    /// such root exists. Used to enforce tenant-root uniqueness.
    ///
    /// Bypasses `SecureORM` because this check is a system invariant that
    /// must see every tenant — the caller's `AccessScope` is irrelevant for
    /// correctness here.
    async fn find_root_id_with_type_prefix<C: DBRunner>(
        &self,
        db: &C,
        type_prefix: &str,
    ) -> Result<Option<Uuid>, DomainError> {
        use sea_orm::{JoinType, QuerySelect};

        // Bypass SecureORM: tenant-root uniqueness is a system invariant that
        // must see every tenant, not only the caller's scope.
        let scope = system_scope();
        let model: Option<rg_entity::Model> = ResourceGroupEntity::find()
            .join(
                JoinType::InnerJoin,
                rg_entity::Entity::belongs_to(GtsTypeEntity)
                    .from(rg_entity::Column::GtsTypeId)
                    .to(gts_type::Column::Id)
                    .into(),
            )
            .filter(rg_entity::Column::ParentId.is_null())
            .filter(gts_type::Column::SchemaId.starts_with(type_prefix))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(model.map(|m| m.id))
    }

    /// List groups with `OData` filtering and pagination.
    ///
    /// The `type` filter field accepts GTS type path strings from the API
    /// (e.g. `$filter=type eq 'gts.cf.core.rg.type.v1~x.test.org.v1~'`).
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
        // Validate filter (String kind for `type`) and resolve string values
        // to SMALLINT IDs in the typed FilterNode — BEFORE paginate_odata.
        let resolved_filter = if let Some(ast) = query.filter.as_deref() {
            let validated =
                toolkit_odata::filter::convert_expr_to_filter_node::<GroupFilterField>(ast)
                    .map_err(|e| DomainError::validation(format!("invalid $filter: {e}")))?;
            Some(Self::resolve_type_filter_node(db, &validated).await?)
        } else {
            None
        };

        // Build base query with resolved filter applied manually
        let base_query = ResourceGroupEntity::find().secure().scope_with(scope);
        let base_query = if let Some(ref node) = resolved_filter {
            let cond = toolkit_db::odata::sea_orm_filter::filter_node_to_condition::<
                GroupFilterField,
                GroupODataMapper,
            >(node)
            .map_err(|e| DomainError::validation(format!("invalid $filter: {e}")))?;
            base_query.filter(cond)
        } else {
            base_query
        };

        // Strip filter from query — already applied above
        let mut query_no_filter = query.clone();
        query_no_filter.filter = None;

        let page = paginate_odata::<GroupFilterField, GroupODataMapper, _, _, _, _>(
            base_query,
            db,
            &query_no_filter,
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
    async fn get_descendants<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let (depth_filter, _) = Self::parse_hierarchy_filter(query);
        let sys = system_scope();

        let mut desc_query =
            ClosureEntity::find().filter(closure_entity::Column::AncestorId.eq(group_id));
        if let Some(max_desc) = depth_filter
            .as_ref()
            .and_then(DepthFilter::max_descendant_depth)
            && max_desc >= 0
        {
            desc_query = desc_query.filter(closure_entity::Column::Depth.lte(max_desc));
        }
        let rows = desc_query
            .secure()
            .scope_with(&sys)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let group_depths: Vec<(Uuid, i32)> =
            rows.iter().map(|r| (r.descendant_id, r.depth)).collect();

        self.build_hierarchy_page(db, scope, query, group_depths)
            .await
    }

    async fn get_ancestors<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let (depth_filter, _) = Self::parse_hierarchy_filter(query);
        let sys = system_scope();

        // Self-row (depth=0)
        let self_row = ClosureEntity::find()
            .filter(closure_entity::Column::AncestorId.eq(group_id))
            .filter(closure_entity::Column::Depth.eq(0))
            .secure()
            .scope_with(&sys)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let mut group_depths: Vec<(Uuid, i32)> =
            self_row.iter().map(|r| (r.descendant_id, 0)).collect();

        // Ancestor rows (depth > 0 in closure, negated to < 0 in result)
        let mut anc_query = ClosureEntity::find()
            .filter(closure_entity::Column::DescendantId.eq(group_id))
            .filter(closure_entity::Column::Depth.ne(0));
        if let Some(max_anc) = depth_filter
            .as_ref()
            .and_then(DepthFilter::max_ancestor_depth)
            && max_anc > 0
        {
            anc_query = anc_query.filter(closure_entity::Column::Depth.lte(max_anc));
        }
        let rows = anc_query
            .secure()
            .scope_with(&sys)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        for row in &rows {
            group_depths.push((row.ancestor_id, -row.depth));
        }

        self.build_hierarchy_page(db, scope, query, group_depths)
            .await
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

        // The group PK is global, so a caller-supplied `id` may already be
        // taken. The insert returns the persisted row, so re-reading it by
        // id afterwards was a second `resource_group` SELECT per create for
        // data already in hand (RG-08).
        toolkit_db::secure::secure_insert::<ResourceGroupEntity>(model, &scope, db)
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    DomainError::group_already_exists(id)
                } else if e.is_foreign_key_violation() {
                    // The parent this create read a moment ago is gone: a
                    // concurrent non-force delete of it won the race, and
                    // `fk_resource_group_parent` is `ON DELETE RESTRICT`, so
                    // the loser learns about it here rather than from its own
                    // read. That read is the caller's snapshot; the FK check
                    // is not, which is exactly why this arm exists.
                    //
                    // It exists *now* because the non-force delete runs at the
                    // backend default. Under SERIALIZABLE on both sides the
                    // same race surfaced as a `40001` and the retry loop
                    // re-read a clean answer; with the delete lowered, SSI has
                    // no second serializable party to detect against and the
                    // foreign key answers instead. Unmapped it was a 500.
                    //
                    // Which foreign key, though: this table has two, and
                    // `fk_rg_gts_type` fails when a concurrent `delete_type`
                    // removes the type between this transaction resolving it
                    // and inserting. Answering *that* with "group not found,
                    // id = parent" would name the wrong resource and the wrong
                    // cause, so only the parent constraint maps. PostgreSQL
                    // puts the constraint name in the message; SQLite says
                    // only "FOREIGN KEY constraint failed", so there the
                    // answer stays a database error -- unhelpful, but not a
                    // confident lie, and the race needs concurrent writers
                    // SQLite does not have.
                    let msg = e.to_string();
                    if msg.contains("fk_resource_group_parent")
                        && let Some(parent_id) = parent_id
                    {
                        DomainError::group_not_found(parent_id)
                    } else {
                        DomainError::database(msg)
                    }
                } else {
                    DomainError::database(e.to_string())
                }
            })
    }

    /// Update a resource group entity.
    ///
    /// Returns `rows_affected` from the `UPDATE ... WHERE id = ?` rather than
    /// answering `RecordNotFound` itself: the previous shape loaded an
    /// `ActiveModel` first to know that -- at the cost of the very read this
    /// method exists to avoid. Both current callers already read the row
    /// inside the same `SERIALIZABLE` transaction, so a `0` here is
    /// unreachable for them in practice, but the signature no longer asks
    /// them to take that on faith.
    async fn update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<u64, DomainError> {
        let scope = system_scope();

        let parent_val: sea_orm::Value = match parent_id {
            Some(pid) => sea_orm::Value::Uuid(Some(pid)),
            None => sea_orm::Value::Uuid(None),
        };

        let metadata_val: sea_orm::Value = match metadata {
            Some(v) => sea_orm::Value::Json(Some(Box::new(v.clone()))),
            None => sea_orm::Value::Json(None),
        };

        let res = ResourceGroupEntity::update_many()
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

        Ok(res.rows_affected)
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
        toolkit_db::secure::secure_insert::<ClosureEntity>(model, &scope, db)
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
    ) -> Result<u64, DomainError> {
        // `Expr`'s combinators (`eq`, `add`) live on `ExprTrait` as of
        // sea-query 1.0. Imported here rather than file-wide: its `min`
        // would shadow `Ord::min` for the paginating methods above.
        use sea_orm::ExprTrait;

        let scope = system_scope();
        // One statement for the whole ancestor chain: every row the parent
        // has as a descendant becomes a row for the child, one deeper. The
        // ancestors are neither fetched nor rebuilt here -- a create used to
        // pay a round-trip to read them and a second to write them back,
        // both inside the transaction that create holds open.
        let mut source = Query::select();
        source
            .expr(Expr::col(closure_entity::Column::AncestorId))
            .expr(Expr::val(child_id))
            .expr(Expr::col(closure_entity::Column::Depth).add(1))
            .from(ClosureEntity)
            .and_where(Expr::col(closure_entity::Column::DescendantId).eq(parent_id));

        let written = toolkit_db::secure::secure_insert_from_select::<ClosureEntity, _>(
            [
                closure_entity::Column::AncestorId,
                closure_entity::Column::DescendantId,
                closure_entity::Column::Depth,
            ],
            source,
            &scope,
            db,
        )
        .await
        .map_err(|e| match e {
            toolkit_db::secure::ScopeError::Db(db) => DomainError::Database(db),
            // stringify would wrap this in `DbErr::Custom`, which
            // `is_retryable_contention` does not recognize -- a 40001 from
            // this set-based statement would stop being retried.
            other => DomainError::database(other.to_string()),
        })?;

        Ok(written)
    }

    /// Delete every group in `ids` in one statement per bind-parameter
    /// chunk, not one `delete_by_id` per node (RG-10).
    async fn delete_by_id_many<C: DBRunner>(
        &self,
        db: &C,
        ids: &[Uuid],
    ) -> Result<(), DomainError> {
        if ids.is_empty() {
            return Ok(());
        }
        let scope = system_scope();
        // Chunk against the backend's bind-parameter ceiling, the same one
        // `secure_insert_many` respects: one `IN (...)` predicate binds one
        // parameter per id, so an unchunked delete of a large subtree fails
        // in the driver rather than in the query.
        for chunk in ids.chunks(toolkit_db::secure::max_bind_params_for(db)) {
            ResourceGroupEntity::delete_many()
                .filter(rg_entity::Column::Id.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .exec(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }
        Ok(())
    }

    /// Delete all memberships for every group in `group_ids` in a single
    /// statement, not one per node (RG-10).
    async fn delete_memberships_many<C: DBRunner>(
        &self,
        db: &C,
        group_ids: &[Uuid],
    ) -> Result<(), DomainError> {
        if group_ids.is_empty() {
            return Ok(());
        }
        let scope = system_scope();
        // Chunk against the backend's bind-parameter ceiling, the same one
        // `secure_insert_many` respects: one `IN (...)` predicate binds one
        // parameter per id, so an unchunked delete of a large subtree fails
        // in the driver rather than in the query.
        for chunk in group_ids.chunks(toolkit_db::secure::max_bind_params_for(db)) {
            MembershipEntity::delete_many()
                .filter(membership_entity::Column::GroupId.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .exec(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }
        Ok(())
    }

    /// Delete all closure rows (both as ancestor and as descendant) for
    /// every group in `group_ids`, in 2 statements per bind-parameter chunk
    /// rather than 2 per group (RG-10). One chunk covers whatever
    /// `max_bind_params_for` allows — 30 000 on `SQLite`, 60 000 on
    /// `PostgreSQL`, both below the backends' own limits on purpose — so "2"
    /// is the ordinary case, not the guarantee.
    async fn delete_all_closure_rows_many<C: DBRunner>(
        &self,
        db: &C,
        group_ids: &[Uuid],
    ) -> Result<(), DomainError> {
        if group_ids.is_empty() {
            return Ok(());
        }
        let scope = system_scope();
        // Chunked for the same reason as the other batch deletes: one bind
        // parameter per id, capped per backend.
        for chunk in group_ids.chunks(toolkit_db::secure::max_bind_params_for(db)) {
            ClosureEntity::delete_many()
                .filter(closure_entity::Column::AncestorId.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .exec(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;

            ClosureEntity::delete_many()
                .filter(closure_entity::Column::DescendantId.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .exec(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }

        Ok(())
    }

    /// Every descendant of `group_id` (the self-row excluded) with its depth
    /// relative to `group_id`, so callers don't re-derive the depth via a
    /// second per-row query (RG-05/RG-10). Ordered by depth ascending;
    /// callers needing leaf-to-root order reverse the list.
    async fn get_descendant_ids_with_depth<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<Vec<(Uuid, i32)>, DomainError> {
        use sea_orm::QueryOrder;

        let scope = system_scope();
        let rows = ClosureEntity::find()
            .filter(closure_entity::Column::AncestorId.eq(group_id))
            .filter(closure_entity::Column::Depth.ne(0))
            .order_by_asc(closure_entity::Column::Depth)
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| (r.descendant_id, r.depth))
            .collect())
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

    /// Deepest descendant of `group_id` relative to it, or `0` when it has
    /// none.
    ///
    /// One `MAX(depth)` aggregate over the closure table -- see
    /// `count_children` above for the same `SecureSelect` pattern applied to
    /// `COUNT` -- instead of `get_descendant_ids_with_depth`'s whole row set
    /// pulled into this process only to be folded down to this one number.
    async fn get_max_descendant_depth<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<i32, DomainError> {
        // `Expr::max` (the SQL aggregate) lives on `ExprTrait` as of
        // sea-query 1.0. Imported here rather than file-wide: its `max`
        // would shadow `Ord::max` for the paginating methods above.
        use sea_orm::ExprTrait;
        use sea_orm::{FromQueryResult, QuerySelect};

        #[derive(FromQueryResult)]
        struct MaxDepth {
            max_depth: Option<i32>,
        }

        let scope = system_scope();
        let rows: Vec<MaxDepth> = ClosureEntity::find()
            .filter(closure_entity::Column::AncestorId.eq(group_id))
            .filter(closure_entity::Column::Depth.ne(0))
            .secure()
            .scope_with(&scope)
            .project_all(db, |q| {
                q.select_only()
                    .column_as(Expr::col(closure_entity::Column::Depth).max(), "max_depth")
                    .into_model::<MaxDepth>()
            })
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // `MAX` over zero rows is one row holding NULL, not zero rows -- but
        // falling back to `0` either way keeps this the same "no
        // descendants" answer `get_descendant_ids_with_depth(...).max()` gave.
        Ok(rows
            .into_iter()
            .next()
            .and_then(|r| r.max_depth)
            .unwrap_or(0))
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
    ) -> Result<u64, DomainError> {
        // `Expr`'s combinators (`eq`, `add`) live on `ExprTrait` as of
        // sea-query 1.0. Imported here rather than file-wide: its `min`
        // would shadow `Ord::min` for the paginating methods above.
        use sea_orm::ExprTrait;

        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-1
        // Collect subtree: SELECT descendant_id FROM resource_group_closure
        // WHERE ancestor_id = group_id -- the group itself included, via its
        // own self-row.
        //
        // Defined as a query and used by the steps below rather than
        // materialized here. The subtree is exactly the input this operation
        // must not be linear in, and none of the decisions taken from it need
        // the rows in this process.
        let scope = system_scope();
        let subtree_query = Query::select()
            .column(closure_entity::Column::DescendantId)
            .from(ClosureEntity)
            .and_where(Expr::col(closure_entity::Column::AncestorId).eq(group_id))
            .to_owned();
        // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-1

        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-2
        // Delete affected paths: DELETE FROM resource_group_closure
        // WHERE descendant_id IN (subtree) AND ancestor_id NOT IN (subtree)
        //
        // Both predicates are that same subquery. The previous form read
        // every closure row of the subtree back into the process only to
        // decide which ancestors were external, then bound the survivors into
        // two `IN (...)` lists -- one parameter per id, unbounded, so a large
        // enough subtree failed in the driver rather than in the query.
        //
        // Each predicate wraps the subquery in a derived table instead of
        // naming `resource_group_closure` directly: MySQL rejects a `DELETE`
        // whose subquery reads the target table (ER 1093) but materializes a
        // derived table before deleting, which lifts the restriction.
        // PostgreSQL and SQLite accept either form unchanged.
        //
        // `NOT IN` here is only correct because `descendant_id` is `NOT NULL`
        // in both backend branches of the migration. Were it ever nullable, a
        // single NULL would make the predicate NULL for every row and this
        // DELETE would quietly stop deleting -- no error, no rows, a closure
        // table that keeps its stale ancestors.
        //
        // The DELETE runs before the INSERT below and cannot disturb either of
        // its sources: it only removes rows whose ancestor is *outside* the
        // subtree, while the `st` source ranges over `ancestor_id = group_id`
        // (inside by definition) and the `pa` source over
        // `descendant_id = parent_id`, whose ancestors are all outside --
        // guaranteed by the `is_descendant` check the service makes before
        // calling this.
        let desc_alias = Alias::new("st_desc");
        let subtree_for_descendants = Query::select()
            .expr(Expr::col((
                desc_alias.clone(),
                closure_entity::Column::DescendantId,
            )))
            .from_subquery(subtree_query.clone(), desc_alias)
            .to_owned();
        let anc_alias = Alias::new("st_anc");
        let subtree_for_ancestors = Query::select()
            .expr(Expr::col((
                anc_alias.clone(),
                closure_entity::Column::DescendantId,
            )))
            .from_subquery(subtree_query, anc_alias)
            .to_owned();
        let deleted = ClosureEntity::delete_many()
            .filter(closure_entity::Column::DescendantId.in_subquery(subtree_for_descendants))
            .filter(closure_entity::Column::AncestorId.not_in_subquery(subtree_for_ancestors))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?
            .rows_affected;
        // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-2

        let inserted = if let Some(parent_id) = new_parent_id {
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-3
            // Compute new ancestor paths from new parent: the closure rows
            // whose descendant is the new parent, i.e. its ancestors and its
            // own self-row. Named as a join side, not fetched.
            let ancestors = Alias::new("pa");
            let subtree = Alias::new("st");
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-3

            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4
            let mut source = Query::select();

            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a
            // FOR EACH new_ancestor, FOR EACH subtree_node: the cross product
            // of the two closure ranges. The database forms it; the pairs
            // never exist as Rust values. For a 10 000-node subtree under a
            // depth-10 parent that is 100 000 rows the process no longer
            // builds, holds, or sends.
            source
                .from_as(ClosureEntity, ancestors.clone())
                .from_as(ClosureEntity, subtree.clone())
                .and_where(
                    Expr::col((ancestors.clone(), closure_entity::Column::DescendantId))
                        .eq(parent_id),
                )
                .and_where(
                    Expr::col((subtree.clone(), closure_entity::Column::AncestorId)).eq(group_id),
                );
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a

            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a1
            // One row per pair: ancestor_id = new_ancestor, descendant_id =
            // subtree_node, depth = new_ancestor_depth +
            // subtree_node_relative_depth + 1. Column order here is the
            // column order the insert declares.
            source
                .expr(Expr::col((
                    ancestors.clone(),
                    closure_entity::Column::AncestorId,
                )))
                .expr(Expr::col((
                    subtree.clone(),
                    closure_entity::Column::DescendantId,
                )))
                .expr(
                    Expr::col((ancestors, closure_entity::Column::Depth))
                        .add(Expr::col((subtree, closure_entity::Column::Depth)))
                        .add(1),
                );
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4a1

            let written = toolkit_db::secure::secure_insert_from_select::<ClosureEntity, _>(
                [
                    closure_entity::Column::AncestorId,
                    closure_entity::Column::DescendantId,
                    closure_entity::Column::Depth,
                ],
                source,
                &scope,
                db,
            )
            .await
            .map_err(|e| match e {
                toolkit_db::secure::ScopeError::Db(db) => DomainError::Database(db),
                // stringify would wrap this in `DbErr::Custom`, which
                // `is_retryable_contention` does not recognize -- a 40001 from
                // this set-based statement would stop being retried.
                other => DomainError::database(other.to_string()),
            })?;
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-4
            written
        } else {
            // Moving to root attaches no external ancestors, so there is
            // nothing to insert.
            0
        };

        // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-closure-rebuild:p1:inst-closure-rebuild-5
        // RETURN: closure rows updated within transaction — commit handled by caller
        tracing::debug!(%group_id, deleted, inserted, "closure subtree rebuilt");
        if new_parent_id.is_some() && inserted == 0 {
            // Mirrors the `NOT IN` + `NULL` silent-corruption mode the delete
            // step's doc comment above describes: that one needs a null
            // `descendant_id` to misfire, but the effect is the same shape --
            // a reparent that looks like it succeeded while the closure table
            // quietly keeps stale (or here, no new) ancestor rows. A `NULL`
            // can't happen here (see that comment), but a parent with no
            // closure rows of its own -- which should be unreachable, every
            // group gets a self-row on create -- would leave this INSERT
            // with nothing to insert, and the DELETE above would have
            // already dropped the subtree's real ancestors on the strength of
            // the reparent this claims to perform.
            tracing::warn!(
                %group_id,
                deleted,
                "closure rebuild inserted no rows for a reparent -- the parent has no \
                 closure rows, which the NOT IN delete would then silently preserve"
            );
        }
        Ok(inserted)
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
    Single(toolkit_odata::filter::FilterOp, i32),
    And(Vec<DepthFilter>),
}

impl DepthFilter {
    fn matches(&self, depth: i32) -> bool {
        use toolkit_odata::filter::FilterOp;
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
        use toolkit_odata::filter::FilterOp;
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
        use toolkit_odata::filter::FilterOp;
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
