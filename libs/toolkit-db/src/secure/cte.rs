//! Common Table Expression (`WITH`) support for the Secure ORM.
//!
//! Implements [ADR-0001](../../../../docs/arch/secure-orm/ADR/0001-secure-cte-policy.md).
//!
//! # Why this is a separate type
//!
//! A CTE body is an independent `SELECT` over arbitrary tables, so the outer
//! query's scope `WHERE` does **not** reach inside it. Left unchecked that is a
//! direct tenant-isolation hole. The rule this module enforces is therefore
//! *not* "scope the outer query" but **"embed scope inside the body of every
//! CTE"** — then any table a CTE touches is already filtered.
//!
//! Isolation is structural rather than checked at runtime: a [`SecureCteSelect`]
//! is reachable only from [`SecureSelect<E, Scoped>::with_ctes`], and every CTE
//! registered on it is scoped with *that query's* `Arc<AccessScope>`. There is no
//! way to hand it a body carrying a different scope, so there is nothing to
//! compare and no error case to return.
//!
//! # Why it cannot reuse `Select<E>`
//!
//! `SecureSelect` wraps a [`sea_orm::Select<E>`], which has no `.with()` —
//! `WithClause`/`CommonTableExpression` live in `sea_query`. So a CTE query
//! cannot be a `Select<E>` at all, and executing one has to go through
//! `FromQueryResult::find_by_statement` instead of `Select<E>::all()`. Returning
//! `Self` from `with_ctes` would let the `WITH` clause silently vanish at
//! execution time; returning a distinct type makes that unrepresentable.
//!
//! # Example
//!
//! ```rust,ignore
//! // Rows of `order` that have at least one line item in the same scope.
//! let rows = order::Entity::find()
//!     .secure()
//!     .scope_with(&scope)
//!     .with_ctes()
//!     .cte::<line_item::Entity>("scoped_items", |q| {
//!         q.filter(line_item::Column::Quantity.gt(0))
//!     })
//!     .join_cte(
//!         "scoped_items",
//!         Condition::all().add(
//!             Expr::col((Alias::new("scoped_items"), Alias::new("order_id")))
//!                 .equals((order::Entity, order::Column::Id)),
//!         ),
//!     )
//!     .all(&conn)
//!     .await?;
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::sea_query::{
    Alias, CommonTableExpression, Expr, IntoIden, JoinType, Order, SelectStatement, UnionType,
    WithClause,
};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, ExprTrait, FromQueryResult, QueryFilter, QueryTrait,
    StatementBuilder,
};

use crate::secure::error::ScopeError;
use crate::secure::select::{Scoped, SecureEntityExt, SecureSelect};
use crate::secure::{AccessScope, DBRunner, DBRunnerInternal, ScopableEntity};

/// Column a recursive CTE exposes its distance-from-seed on.
///
/// Emitted into every recursive CTE body, because the depth cap has to be
/// expressible as a predicate on the recursive member. A CTE whose source entity
/// already has a column of this name would collide, so the name is deliberately
/// unlikely: double leading underscore, not a plausible domain column.
const DEPTH_COLUMN: &str = "__cte_depth";

/// A scoped query that carries `WITH` definitions.
///
/// Built by [`SecureSelect::with_ctes`]. Every CTE registered here is scoped
/// with the originating query's `AccessScope`, so mixing scopes in one statement
/// is unrepresentable rather than rejected.
///
/// Executes through `find_by_statement`, not `Select<E>` — see the module docs.
#[must_use]
pub struct SecureCteSelect<E: EntityTrait> {
    /// Outer query, scope `WHERE` already embedded by `scope_with`.
    outer: SelectStatement,
    with: WithClause,
    /// Seeds every CTE body. This is what makes same-scope structural.
    scope: Arc<AccessScope>,
    /// Mirrors [`Self::distinct`]. `sea_query` keeps `SelectStatement::distinct`
    /// private, so the flag cannot be read back off the statement.
    distinct: bool,
    /// Set by [`Self::order_by_cte`]: the query orders by a column that is *not*
    /// in the outer `SELECT` list. Harmless alone, invalid under `DISTINCT`.
    orders_by_cte: bool,
    _entity: PhantomData<E>,
}

/// A recursive CTE that walks a self-referencing hierarchy.
///
/// Both the seed and the recursive member carry the outer query's scope: the
/// recursive member reads the *real table* joined against the CTE self-reference,
/// so `build_scope_condition` applies to it exactly as it does to the seed. That
/// is why recursion is admissible here at all.
///
/// The hazard recursion does introduce is **cycles**, and it is not optional to
/// handle: `sea_query` renders `PostgreSQL`'s `CYCLE` clause but silently drops it
/// on `MySQL` and `SQLite`, so a portable guard cannot rely on it. Hence `max_depth`
/// has no default — a caller must state a bound, and it is emitted as a predicate
/// on the recursive member.
///
/// # What bounds the walk
///
/// Two independent limits, and it is worth knowing which does what.
///
/// The **depth cap** guarantees termination. The **dedup mode** decides how much
/// work happens inside that bound, and the default matters:
///
/// - [`RecursiveDedup::Union`] (default) discards rows duplicating ones already
///   produced. Because the row carries its depth, a node reached at two depths is
///   still expanded twice — so re-expansion is bounded by *(rows × depth)* rather
///   than by path count. This is plain standard-SQL `UNION`, not a
///   `PostgreSQL`-only trick, and it is what keeps a hub-heavy graph flat.
/// - [`RecursiveDedup::UnionAll`] keeps everything and therefore enumerates
///   **paths**: where several equal-length paths converge on a node, the row count
///   multiplies with the branching factor. Faster per row on a sparse
///   parent-pointer tree, a liability on anything hub-shaped.
///
/// Neither mode is a visited set: nothing prevents a node from being *revisited* at
/// a greater depth, only from being *re-emitted* identically. If you need
/// strictly-once semantics, dedup on the caller side, or expand one hop per query
/// with [`SecureCteSelect::cte`] and keep the frontier yourself.
/// [`SecureCteSelect::distinct`] deduplicates the final rows but does not prune the
/// walk.
///
/// # Shape limit: one self-referencing table
///
/// The recursive member is `FROM J JOIN <cte> ON J.link_col = <cte>.anchor_col` —
/// two tables, so both endpoints of the hop must be columns of the **same** entity
/// `J`. A walk over an edge table works (`(src_node_id, dst_node_id)` on the edge
/// entity), and so does a parent-pointer tree (`(parent_id, id)`).
///
/// What this cannot express is a hop *through* a separate table — `node -> edge ->
/// node`, where the recursion carries nodes but the connection lives in an edge
/// table. That needs a three-way join in the recursive member. Use one scoped query
/// per hop instead: a query over the edge table for the frontier's endpoints, then
/// a scoped query over the nodes it produced.
/// How the recursive member is combined with the already-walked rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecursiveDedup {
    /// `UNION` — discard rows duplicating ones already produced. The default.
    ///
    /// Dedup is on the whole projected row, and the row carries the depth, so a
    /// node reached at two different depths is still expanded twice. Re-expansion
    /// is therefore bounded by *(rows × depth)* rather than by path count — which
    /// is what keeps a hub-heavy graph flat. Plain `UNION` in a recursive CTE is
    /// standard SQL and works on `PostgreSQL`, `MySQL` 8+ and `SQLite`.
    #[default]
    Union,
    /// `UNION ALL` — keep every row the walk produces.
    ///
    /// Cheaper per row, because nothing is compared, but it enumerates **paths**:
    /// on a graph where several paths of equal length converge on a node, the row
    /// count multiplies with the branching factor. Reasonable for a sparse tree
    /// where every node has one parent; a liability on anything hub-shaped.
    UnionAll,
}

#[must_use]
pub struct RecursiveCte<J: EntityTrait> {
    name: &'static str,
    seed: Condition,
    link_col: J::Column,
    anchor_col: J::Column,
    max_depth: u32,
    dedup: RecursiveDedup,
}

impl<J: EntityTrait> RecursiveCte<J> {
    /// Name of the column carrying distance-from-seed, for ordering or filtering
    /// the CTE from the outer query.
    ///
    /// Exposed so callers do not hardcode the literal; see
    /// [`SecureCteSelect::order_by_cte`] for the usual use.
    pub const DEPTH_COLUMN: &'static str = DEPTH_COLUMN;

    /// Describe a recursive walk over `J`.
    ///
    /// - `name` — the CTE's name, referenced later via
    ///   [`SecureCteSelect::join_cte`].
    /// - `seed` — which rows start the walk, e.g. `Column::Id.eq(root)`. Scope is
    ///   applied on top of this; it is not a substitute for it.
    /// - `link_col` — column on the *next* row that points back at a walked row.
    /// - `anchor_col` — column on the *walked* row that `link_col` refers to.
    /// - `max_depth` — how many hops past the seed to follow. `0` yields the seed
    ///   rows only. Mandatory: see the type docs on cycles.
    ///
    /// The emitted join is `J.link_col = <cte>.anchor_col`. For a parent-pointer
    /// tree that is `(parent_id, id)`; for a walk over an edge table it is
    /// `(src_node_id, dst_node_id)`. The pair is deliberately *not* named
    /// child/parent — the traversal is directed-edge, not necessarily hierarchical.
    ///
    /// Defaults to [`RecursiveDedup::Union`]; override with
    /// [`dedup`](Self::dedup).
    pub fn new(
        name: &'static str,
        seed: Condition,
        link_col: J::Column,
        anchor_col: J::Column,
        max_depth: u32,
    ) -> Self {
        Self {
            name,
            seed,
            link_col,
            anchor_col,
            max_depth,
            dedup: RecursiveDedup::Union,
        }
    }

    /// Choose `UNION` or `UNION ALL` for the recursive step.
    pub fn dedup(mut self, dedup: RecursiveDedup) -> Self {
        self.dedup = dedup;
        self
    }
}

// `with_ctes` lives on the scoped select: a CTE query is only reachable from an
// already-scoped query, which is what carries the scope the bodies inherit.
impl<E> SecureSelect<E, Scoped>
where
    E: EntityTrait,
{
    /// Begin a query that carries `WITH` definitions.
    ///
    /// Every CTE registered on the result is scoped with **this** query's
    /// `AccessScope`, so a differently-scoped CTE cannot be constructed. The
    /// returned type executes through its own path; see [`SecureCteSelect`].
    pub fn with_ctes(self) -> SecureCteSelect<E> {
        let scope = self.scope_arc();
        SecureCteSelect {
            outer: QueryTrait::into_query(self.into_inner()),
            with: WithClause::new(),
            scope,
            distinct: false,
            orders_by_cte: false,
            _entity: PhantomData,
        }
    }
}

impl<E> SecureCteSelect<E>
where
    E: EntityTrait,
{
    /// Register a non-recursive CTE over `J`.
    ///
    /// `body` shapes the CTE's `SELECT` — filters, ordering, limits. Scope is
    /// applied *after* it returns, so a body cannot filter the scope predicate
    /// back off.
    ///
    /// Attaching a definition is not the same as using it: reference the CTE from
    /// the outer query with [`join_cte`](Self::join_cte).
    ///
    /// # What the closure is and is not prevented from doing
    ///
    /// Scope for `J` is applied after `body` returns, so a body cannot filter the
    /// scope predicate back off. It *can*, however, widen the body beyond `J`:
    /// `sea_orm`'s `QueryTrait::query()` is public, so a caller may reach the
    /// underlying `SelectStatement` and join a table that gets no scope predicate of
    /// its own.
    ///
    /// That is the crate's existing boundary rather than a property of this method —
    /// `filter`, [`join_cte`](Self::join_cte) and `SecureSelect::project_all` all
    /// accept caller-built `sea_query` fragments, and `into_inner()` hands over the
    /// whole builder. The guarantee is "the entity you queried is always scoped",
    /// not "no other table can appear". Tables a gear brings in itself are the
    /// gear's responsibility, and the Level-C lint in ADR-0001 is the intended
    /// mechanical check.
    pub fn cte<J>(
        mut self,
        name: &'static str,
        body: impl FnOnce(sea_orm::Select<J>) -> sea_orm::Select<J>,
    ) -> Self
    where
        J: ScopableEntity + EntityTrait,
        J::Column: ColumnTrait + Copy,
    {
        // Routed through `scope_with_arc` rather than calling
        // `build_scope_condition` directly, so there is exactly one definition of
        // "apply scope to a select" in the crate.
        let scoped = body(J::find())
            .secure()
            .scope_with_arc(Arc::clone(&self.scope));

        let mut cte = CommonTableExpression::new();
        cte.table_name(Alias::new(name))
            .query(QueryTrait::into_query(scoped.into_inner()));
        self.with.cte(cte);
        self
    }

    /// Register a recursive CTE over a self-referencing `J`.
    ///
    /// Emits, with `<scope>` present in **both** members and the union operator
    /// chosen by [`RecursiveDedup`] (`UNION` by default):
    ///
    /// ```sql
    /// WITH RECURSIVE n AS (
    ///     SELECT j.link_col, j.anchor_col, 0 AS __cte_depth FROM j
    ///      WHERE <seed> AND <scope>
    ///     UNION
    ///     SELECT j.link_col, j.anchor_col, n.__cte_depth + 1 FROM j
    ///       JOIN n ON j.link_col = n.anchor_col
    ///      WHERE <scope> AND n.__cte_depth < <max_depth>
    /// )
    /// ```
    ///
    /// # The body projects three columns, not the whole row
    ///
    /// A walk only needs the two link columns and the depth. Selecting every
    /// column of `J` would be wasted width, and under `UNION` it is worse than
    /// wasteful: dedup has to compare every selected column, and `PostgreSQL`'s
    /// `json` type has no equality operator, so a `J` carrying a
    /// `ColumnType::Json` column would fail with *"could not identify an equality
    /// operator for type json"* before the traversal ran. (`jsonb` is fine — it has
    /// equality. Narrowing sidesteps the question entirely.)
    ///
    /// Consequences for the caller:
    ///
    /// - Reference the CTE on one of those two columns via
    ///   [`join_cte`](Self::join_cte); the outer query supplies the full rows.
    /// - [`column_from_cte`](Self::column_from_cte) can read `link_col`,
    ///   `anchor_col` or [`RecursiveCte::DEPTH_COLUMN`] — no other column of `J` is
    ///   in the CTE.
    /// - Dedup under `UNION` is on `(link, anchor, depth)`, which prunes strictly
    ///   more than dedup on the full row would.
    pub fn recursive_cte<J>(mut self, spec: RecursiveCte<J>) -> Self
    where
        J: ScopableEntity + EntityTrait,
        J::Column: ColumnTrait + Copy,
    {
        let cte_ref = Alias::new(spec.name);
        let depth = Alias::new(DEPTH_COLUMN);

        // Seed: the caller's starting predicate, AND the scope. Projection is
        // narrowed to the traversal columns -- see the method docs on why the full
        // row must not be selected here.
        let mut seed = QueryTrait::into_query(
            J::find()
                .filter(spec.seed)
                .secure()
                .scope_with_arc(Arc::clone(&self.scope))
                .into_inner(),
        );
        seed.clear_selects();
        seed.column((J::default(), spec.link_col))
            .column((J::default(), spec.anchor_col))
            .expr_as(Expr::val(0), depth.clone());

        // Recursive member: reads the real table `J` joined to the CTE, so the
        // same scope condition applies here as to the seed. The depth predicate
        // is emitted by us, not the caller -- it is the termination guarantee.
        let mut step = QueryTrait::into_query(
            J::find()
                .secure()
                .scope_with_arc(Arc::clone(&self.scope))
                .into_inner(),
        );
        step.clear_selects();
        step.column((J::default(), spec.link_col))
            .column((J::default(), spec.anchor_col))
            .expr_as(
                Expr::col((cte_ref.clone(), depth.clone())).add(Expr::val(1)),
                depth.clone(),
            )
            .join(
                JoinType::InnerJoin,
                cte_ref.clone(),
                Expr::col((J::default(), spec.link_col))
                    .equals((cte_ref.clone(), spec.anchor_col.into_iden())),
            )
            .and_where(Expr::col((cte_ref, depth)).lt(spec.max_depth));

        let union = match spec.dedup {
            RecursiveDedup::Union => UnionType::Distinct,
            RecursiveDedup::UnionAll => UnionType::All,
        };
        seed.union(union, step);

        let mut cte = CommonTableExpression::new();
        cte.table_name(Alias::new(spec.name)).query(seed);
        self.with.recursive(true).cte(cte);
        self
    }

    /// Join a registered CTE into the outer query.
    ///
    /// `with_ctes`/`cte` only *define*; without a reference the `WITH` clause is
    /// valid SQL that computes nothing. `on` ties the CTE to the outer entity and
    /// is the caller's responsibility — getting it wrong changes which rows come
    /// back, but cannot cross a tenant boundary, because the CTE body never held
    /// another tenant's rows to begin with.
    ///
    /// # Avoid `OR` across two columns in `on`
    ///
    /// "either endpoint of an edge" is the natural predicate for a graph hop, but
    /// `node.id = cte.src OR node.id = cte.dst` makes `PostgreSQL` abandon the index
    /// and sequentially scan the outer table — measured at 15.2 ms against 0.30 ms
    /// for the equivalent single-column form, same rows, on 199k nodes / 600k edges.
    ///
    /// Put the alternation inside the CTE instead, so the outer predicate stays a
    /// single indexed comparison: have the CTE body project one column that already
    /// contains both endpoints (a `UNION` of the two projections), then join on
    /// that.
    pub fn join_cte(mut self, name: &'static str, on: Condition) -> Self {
        self.outer.join(JoinType::InnerJoin, Alias::new(name), on);
        self
    }

    /// Add a filter to the outer query, on top of its scope.
    pub fn filter(mut self, filter: Condition) -> Self {
        self.outer.cond_where(filter);
        self
    }

    /// Limit the outer query.
    pub fn limit(mut self, limit: u64) -> Self {
        self.outer.limit(limit);
        self
    }

    /// `SELECT DISTINCT` on the outer query.
    ///
    /// [`join_cte`](Self::join_cte) emits an inner join, so an outer row is
    /// repeated once per matching CTE row. That is usually not what you want when
    /// the CTE is a set you are testing membership against — e.g. expanding a
    /// breadth-first frontier, where several edges converge on the same node.
    ///
    /// Cannot be combined with [`order_by_cte`](Self::order_by_cte); see that
    /// method.
    ///
    /// # Requires every selected column to be comparable
    ///
    /// `SELECT DISTINCT` compares all selected columns, and by default the outer
    /// query selects every column of `E`. `PostgreSQL`'s `json` has no equality
    /// operator, so an entity carrying a `ColumnType::Json` column fails with
    /// *"could not identify an equality operator for type json"*. (`jsonb` is fine.)
    /// Narrow the projection with [`select_only`](Self::select_only) first — which
    /// you want anyway, since dedup over a wide row is wasted work.
    pub fn distinct(mut self) -> Self {
        self.outer.distinct();
        self.distinct = true;
        self
    }

    /// Order by a column of the outer entity `E`.
    ///
    /// The column is qualified with `E`'s table, so it stays unambiguous after a
    /// [`join_cte`](Self::join_cte) whose CTE happens to expose a column of the
    /// same name. Entity columns are always in the outer `SELECT` list, so this is
    /// safe to combine with [`distinct`](Self::distinct).
    pub fn order_by(mut self, col: E::Column, order: Order) -> Self {
        self.outer.order_by((E::default(), col), order);
        self
    }

    /// Order by a column of a registered CTE — e.g. a recursive walk's depth.
    ///
    /// ```rust,ignore
    /// // Shallowest hop first.
    /// .order_by_cte("subtree", RecursiveCte::<E>::DEPTH_COLUMN, Order::Asc)
    /// ```
    ///
    /// # Not combinable with `distinct()`
    ///
    /// A CTE column is not in the outer `SELECT` list, and both `PostgreSQL` and
    /// `MySQL` reject `ORDER BY` on an expression missing from a `SELECT DISTINCT`
    /// list (`SQLite` accepts it, which is worse — it means the mistake only
    /// surfaces in production). Combining the two therefore returns
    /// [`ScopeError::Invalid`] from `all`/`one`/`all_as` on **every** backend
    /// rather than emitting SQL that some engines refuse.
    ///
    /// "Distinct rows, shallowest first" needs
    /// `GROUP BY <entity cols> ORDER BY MIN(depth)`, which this API cannot express
    /// yet — dedup on the caller side for now.
    pub fn order_by_cte(mut self, cte: &'static str, col: &'static str, order: Order) -> Self {
        self.outer
            .order_by((Alias::new(cte), Alias::new(col)), order);
        self.orders_by_cte = true;
        self
    }

    /// Drop the default `SELECT` list, so only columns added afterwards are
    /// selected.
    ///
    /// `with_ctes()` inherits `Select<E>`'s projection, which is *every* column of
    /// `E`. For a query that only needs a key — a breadth-first hop returning ids,
    /// say — that means transferring every wide column too, and on `PostgreSQL` it
    /// turns an index-only scan into a heap visit per row.
    ///
    /// [`all_as`](Self::all_as) alone does **not** narrow this: it changes how rows
    /// are *deserialized*, not what the server is asked for.
    ///
    /// # Pairs with `all_as`, not `all`
    ///
    /// [`all`](Self::all) and [`one`](Self::one) deserialize into `E::Model`, which
    /// needs every column of `E` present. Narrow the list and they will fail at
    /// deserialization — use `all_as::<T>()` with a `T` matching the columns you
    /// selected.
    ///
    /// ```rust,ignore
    /// #[derive(FromQueryResult)]
    /// struct Hop { id: Uuid }
    ///
    /// let ids = node::Entity::find().secure().scope_with(&scope)
    ///     .with_ctes()
    ///     .cte::<edge::Entity>("hop", |q| q)
    ///     .join_cte("hop", on)
    ///     .select_only()
    ///     .column(node::Column::Id)
    ///     .all_as::<Hop>(runner)
    ///     .await?;
    /// ```
    pub fn select_only(mut self) -> Self {
        self.outer.clear_selects();
        self
    }

    /// Add a column of the outer entity `E` to the `SELECT` list, qualified with
    /// `E`'s table so it stays unambiguous after a [`join_cte`](Self::join_cte).
    pub fn column(mut self, col: E::Column) -> Self {
        self.outer.column((E::default(), col));
        self
    }

    /// Add a column of a registered CTE to the `SELECT` list, under `alias`.
    ///
    /// Aliasing is required rather than optional: a CTE built from an entity
    /// exposes that entity's column names, so an unaliased CTE column would
    /// collide with the outer entity's own column of the same name.
    pub fn column_from_cte(
        mut self,
        cte: &'static str,
        col: &'static str,
        alias: &'static str,
    ) -> Self {
        self.outer.expr_as(
            Expr::col((Alias::new(cte), Alias::new(col))),
            Alias::new(alias),
        );
        self
    }

    /// Add an arbitrary expression to the `SELECT` list under `alias` — an
    /// aggregate, a window function, a computed value.
    ///
    /// Scope is unaffected: the `WHERE` built by `scope_with` is independent of the
    /// projection, so narrowing or extending the select list cannot widen access.
    pub fn expr_as(mut self, expr: Expr, alias: &'static str) -> Self {
        self.outer.expr_as(expr, Alias::new(alias));
        self
    }

    /// Reject query shapes that are not portable across backends.
    ///
    /// Checked at execution rather than in the builder because the builder methods
    /// return `Self`, and the conflict only exists once both halves are present.
    ///
    /// Reuses `ScopeError::Invalid` — whose `Display` prefix reads "invalid scope",
    /// slightly off for a query-shape error — because `ScopeError` is a public enum
    /// without `#[non_exhaustive]`, so adding a variant would break downstream
    /// exhaustive matches.
    pub(crate) fn validate(&self) -> Result<(), ScopeError> {
        if self.distinct && self.orders_by_cte {
            return Err(ScopeError::Invalid(
                "SELECT DISTINCT cannot ORDER BY a CTE column that is not in the \
                 select list; drop distinct() or order by an entity column",
            ));
        }
        Ok(())
    }

    /// Render the statement for `backend` without executing it.
    ///
    /// Exists because this crate has no mock database: it is the only way for a
    /// test to assert on the emitted SQL, including that the `WITH` clause is
    /// present at all. Crate-private — `sea_orm::Statement` and `DbBackend` must
    /// not appear in this crate's public surface (see the [`runner`] module docs).
    ///
    /// Clones, so it is not on the execution path; that uses
    /// [`into_statement`](Self::into_statement).
    ///
    /// [`runner`]: crate::secure
    #[cfg(test)]
    #[must_use]
    pub(crate) fn build_statement(&self, backend: sea_orm::DbBackend) -> sea_orm::Statement {
        let query = self.outer.clone().with(self.with.clone());
        StatementBuilder::build(&query, &backend)
    }

    /// Same as [`build_statement`](Self::build_statement) but consumes `self`,
    /// avoiding a clone of the outer statement and the whole `WithClause` on every
    /// executed query.
    fn into_statement(self, backend: sea_orm::DbBackend) -> sea_orm::Statement {
        let query = self.outer.with(self.with);
        StatementBuilder::build(&query, &backend)
    }

    /// Execute and return all matching rows of `E`.
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the query fails, or `ScopeError::Invalid` if
    /// `distinct()` was combined with `order_by_cte()`.
    pub async fn all(self, runner: &impl DBRunner) -> Result<Vec<E::Model>, ScopeError>
    where
        E::Model: Send + Sync,
    {
        self.all_as::<E::Model>(runner).await
    }

    /// Execute and return at most one row of `E`.
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the query fails, or `ScopeError::Invalid` if
    /// `distinct()` was combined with `order_by_cte()`.
    pub async fn one(self, runner: &impl DBRunner) -> Result<Option<E::Model>, ScopeError> {
        self.validate()?;
        let exec = DBRunnerInternal::as_seaorm(runner);
        let stmt = self.into_statement(exec.backend());
        Ok(match exec {
            crate::secure::SeaOrmRunner::Conn(db) => {
                E::Model::find_by_statement(stmt).one(db).await?
            }
            crate::secure::SeaOrmRunner::Tx(tx) => {
                E::Model::find_by_statement(stmt).one(tx).await?
            }
        })
    }

    /// Execute and deserialize into a custom row type `T`.
    ///
    /// This controls **deserialization only** — it does not narrow the SQL. By
    /// default the outer query still selects every column of `E`, so a `T` with two
    /// fields still transfers the whole row. To narrow what the server is asked
    /// for, combine with [`select_only`](Self::select_only) and
    /// [`column`](Self::column) / [`expr_as`](Self::expr_as).
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the query fails, or `ScopeError::Invalid` if
    /// `distinct()` was combined with `order_by_cte()`.
    pub async fn all_as<T>(self, runner: &impl DBRunner) -> Result<Vec<T>, ScopeError>
    where
        T: FromQueryResult + Send + Sync,
    {
        self.validate()?;
        let exec = DBRunnerInternal::as_seaorm(runner);
        let stmt = self.into_statement(exec.backend());
        Ok(match exec {
            crate::secure::SeaOrmRunner::Conn(db) => T::find_by_statement(stmt).all(db).await?,
            crate::secure::SeaOrmRunner::Tx(tx) => T::find_by_statement(stmt).all(tx).await?,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "cte_tests.rs"]
mod tests;
