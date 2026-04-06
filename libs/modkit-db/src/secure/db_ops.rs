use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, InsertResult, IntoActiveModel, ModelTrait,
    QueryFilter,
    sea_query::{IntoIden, OnConflict, SimpleExpr},
};
use std::marker::PhantomData;

use crate::secure::cond::build_scope_condition;
use crate::secure::error::ScopeError;
use crate::secure::{
    AccessScope, DBRunner, DBRunnerInternal, ScopableEntity, Scoped, SeaOrmRunner, SecureEntityExt,
    Unscoped,
};

/// Convert a `sea_orm::Value` to a [`ScopeValue`] for comparison with scope filter values.
///
/// Supports UUID, String. Returns `None` for unsupported types.
fn sea_value_to_scope_value(v: &sea_orm::Value) -> Option<modkit_security::ScopeValue> {
    use modkit_security::ScopeValue;
    match v {
        sea_orm::Value::Uuid(Some(u)) => Some(ScopeValue::Uuid(**u)),
        sea_orm::Value::String(Some(s)) => {
            // Try UUID first for consistent matching
            if let Ok(uuid) = uuid::Uuid::parse_str(s) {
                Some(ScopeValue::Uuid(uuid))
            } else {
                Some(ScopeValue::String(s.to_string()))
            }
        }
        sea_orm::Value::BigInt(Some(n)) => Some(ScopeValue::Int(*n)),
        sea_orm::Value::Int(Some(n)) => Some(ScopeValue::Int(i64::from(*n))),
        sea_orm::Value::SmallInt(Some(n)) => Some(ScopeValue::Int(i64::from(*n))),
        sea_orm::Value::TinyInt(Some(n)) => Some(ScopeValue::Int(i64::from(*n))),
        sea_orm::Value::Bool(Some(b)) => Some(ScopeValue::Bool(*b)),
        _ => None,
    }
}

/// Validate that the values in an `ActiveModel` satisfy at least one constraint
/// in the provided `AccessScope`.
///
/// This is the INSERT-time counterpart of `build_scope_condition`: instead of
/// adding `WHERE` clauses to a query, it checks the `ActiveModel`'s column
/// values in-memory against every scope filter.
///
/// # Semantics
///
/// - Multiple constraints are **OR-ed**: the insert is allowed if ANY constraint
///   matches entirely.
/// - Filters within a constraint are **AND-ed**: ALL filters must match for
///   that constraint to pass.
/// - A filter whose property resolves to a column where the `ActiveModel` value
///   is `NotSet` is **skipped** (the column is not being inserted, so there's
///   nothing to validate).
/// - A filter whose property does **not** resolve (unknown property) causes
///   that constraint to fail (fail-closed), consistent with the query-path
///   behavior in `build_scope_condition`.
///
/// # Errors
///
/// Returns `ScopeError::Denied` if no constraint matches the `ActiveModel`.
fn validate_insert_scope<A>(am: &A, scope: &AccessScope) -> Result<(), ScopeError>
where
    A: ActiveModelTrait,
    A::Entity: ScopableEntity + EntityTrait,
    <A::Entity as EntityTrait>::Column: ColumnTrait + Copy,
{
    if scope.is_unconstrained() || A::Entity::IS_UNRESTRICTED {
        return Ok(());
    }
    if scope.is_deny_all() {
        return Err(ScopeError::Denied(
            "insert denied: scope has no constraints",
        ));
    }

    // OR over constraints: at least one must match entirely.
    'next_constraint: for constraint in scope.constraints() {
        // AND over filters within this constraint.
        for filter in constraint.filters() {
            let Some(col) = <A::Entity as ScopableEntity>::resolve_property(filter.property())
            else {
                // Unknown property → this constraint fails (fail-closed).
                continue 'next_constraint;
            };

            // Extract the column value from the ActiveModel.
            match am.get(col) {
                sea_orm::ActiveValue::NotSet => {
                    // Column not being set in this insert — skip this filter.
                    // (e.g., auto-generated columns, defaults)
                }
                sea_orm::ActiveValue::Set(v) | sea_orm::ActiveValue::Unchanged(v) => {
                    let Some(sv) = sea_value_to_scope_value(&v) else {
                        // Unsupported column type — can't match filter.
                        continue 'next_constraint;
                    };

                    if !filter.values().contains(&sv) {
                        continue 'next_constraint;
                    }
                }
            }
        }
        // All filters in this constraint matched → insert is allowed.
        return Ok(());
    }

    Err(ScopeError::Denied(
        "insert denied: entity values do not satisfy any scope constraint",
    ))
}

/// Secure insert helper for Scopable entities.
///
/// This helper performs a standard `INSERT` through `SeaORM` but wraps database
/// errors into a unified `ScopeError` type for consistent error handling across
/// secure data-access code.
///
/// # Scope Validation
///
/// Validates **all** scope constraints against the `ActiveModel`'s column values,
/// not just `tenant_id`. For each constraint in the scope, every filter's property
/// is resolved to a column via `ScopableEntity::resolve_property`, and the
/// `ActiveModel`'s value for that column is checked against the filter's values.
/// At least one constraint must match entirely (OR semantics) for the insert to
/// proceed.
///
/// # Responsibilities
///
/// - Does **not** inspect the `SecurityContext` or enforce tenant scoping rules.
/// - Does **not** automatically populate any entity fields.
/// - Callers are responsible for:
///   - Setting all required fields before calling.
///   - Validating that the operation is authorized within the current
///     `SecurityContext` (e.g., verifying `tenant_id` or resource ownership).
///
/// # Behavior by Entity Type
///
/// ## Tenant-scoped entities (have `tenant_col`)
/// - Must have a valid, non-empty `tenant_id` set in the `ActiveModel` before insert.
/// - The `tenant_id` should come from the request payload or be validated against
///   `SecurityContext` by the service layer before calling this helper.
///
/// ## Global entities (no `tenant_col`)
/// - May be inserted freely without tenant validation.
/// - Typical examples include system-wide configuration or audit logs.
///
/// # Recommended Field Population
///
/// When inserting entities, populate these fields from `SecurityContext` in service code:
/// - `tenant_id`: from payload or validated via `ctx.scope()`
/// - `owner_id`: from `ctx.subject_id()`
/// - `created_by`: from `ctx.subject_id()` if applicable
///
/// # Example
///
/// ```ignore
/// use modkit_db::secure::{secure_insert, SecurityContext};
///
/// // Domain/service layer validates tenant_id beforehand
/// let am = user::ActiveModel {
///     id: Set(Uuid::new_v4()),
///     tenant_id: Set(tenant_id),
///     owner_id: Set(ctx.subject_id()),
///     email: Set("user@example.com".to_string()),
///     ..Default::default()
/// };
///
/// // Simple secure insert wrapper
/// let user = secure_insert::<user::Entity>(am, &ctx, conn).await?;
/// ```
///
/// # Errors
///
/// - Returns `ScopeError::Db` if the database insert fails.
/// - Returns `ScopeError::Denied` if the `ActiveModel` values do not satisfy any scope constraint.
/// - Returns `ScopeError::TenantNotInScope` for tenant isolation violations.
pub async fn secure_insert<E>(
    am: E::ActiveModel,
    scope: &AccessScope,
    runner: &impl DBRunner,
) -> Result<E::Model, ScopeError>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
    E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
    E::Model: sea_orm::IntoActiveModel<E::ActiveModel>,
{
    // Tenant-scoped entities must have tenant_id set in the ActiveModel.
    if let Some(tenant_col) = E::tenant_col()
        && let sea_orm::ActiveValue::NotSet = am.get(tenant_col)
    {
        return Err(ScopeError::Invalid("tenant_id is required"));
    }

    validate_insert_scope(&am, scope)?;

    match DBRunnerInternal::as_seaorm(runner) {
        SeaOrmRunner::Conn(db) => Ok(am.insert(db).await?),
        SeaOrmRunner::Tx(tx) => Ok(am.insert(tx).await?),
    }
}

/// Secure update helper for updating a single entity by ID inside a scope.
///
/// # Security
/// - Verifies the target row exists **within the scope** before updating.
/// - For tenant-scoped entities, forbids changing `tenant_id` (immutable).
///
/// # Errors
/// - `ScopeError::Denied` if the row is not accessible in the scope.
/// - `ScopeError::Denied("tenant_id is immutable")` if caller attempts to change `tenant_id`.
pub async fn secure_update_with_scope<E>(
    am: E::ActiveModel,
    scope: &AccessScope,
    id: uuid::Uuid,
    runner: &impl DBRunner,
) -> Result<E::Model, ScopeError>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
    E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
    E::Model: sea_orm::IntoActiveModel<E::ActiveModel> + sea_orm::ModelTrait<Entity = E>,
{
    let existing = E::find()
        .secure()
        .scope_with(scope)
        .and_id(id)?
        .one(runner)
        .await?;

    let Some(existing) = existing else {
        return Err(ScopeError::Denied(
            "entity not found or not accessible in current security scope",
        ));
    };

    if let Some(tcol) = E::tenant_col() {
        let stored = match existing.get(tcol) {
            sea_orm::Value::Uuid(Some(u)) => *u,
            _ => return Err(ScopeError::Invalid("tenant_id has unexpected type")),
        };

        let incoming = match am.get(tcol) {
            sea_orm::ActiveValue::Set(v) | sea_orm::ActiveValue::Unchanged(v) => match v {
                sea_orm::Value::Uuid(Some(u)) => Some(*u),
                sea_orm::Value::Uuid(None) => {
                    return Err(ScopeError::Invalid("tenant_id is required"));
                }
                _ => {
                    return Err(ScopeError::Invalid("tenant_id has unexpected type"));
                }
            },
            sea_orm::ActiveValue::NotSet => None,
        };

        if let Some(incoming) = incoming
            && incoming != stored
        {
            return Err(ScopeError::Denied("tenant_id is immutable"));
        }
    }

    match DBRunnerInternal::as_seaorm(runner) {
        SeaOrmRunner::Conn(db) => Ok(am.update(db).await?),
        SeaOrmRunner::Tx(tx) => Ok(am.update(tx).await?),
    }
}

/// Helper to validate a tenant ID is in the scope.
///
/// Use this when manually setting `tenant_id` in `ActiveModels` to ensure
/// the value matches the security scope.
///
/// For unconstrained scopes (allow-all), this always succeeds.
///
/// # Errors
/// Returns `ScopeError::Denied` if tenant scope is missing.
/// Returns `ScopeError::TenantNotInScope` if the tenant ID is not in any constraint.
pub fn validate_tenant_in_scope(
    tenant_id: uuid::Uuid,
    scope: &AccessScope,
) -> Result<(), ScopeError> {
    if scope.is_unconstrained() {
        return Ok(());
    }
    let prop = modkit_security::pep_properties::OWNER_TENANT_ID;
    if !scope.has_property(prop) {
        return Err(ScopeError::Denied(
            "tenant scope required for tenant-scoped insert",
        ));
    }
    if scope.contains_uuid(prop, tenant_id) {
        return Ok(());
    }
    Err(ScopeError::TenantNotInScope { tenant_id })
}

/// A type-safe wrapper around `SeaORM`'s `Insert` that enforces scoping.
///
/// This wrapper uses the typestate pattern to ensure that insert operations
/// cannot be executed without first applying access control via
/// `.scope_with_model()` (validated) or `.scope_unchecked()` (unvalidated).
///
/// Unlike the simpler `secure_insert()` helper, this wrapper preserves `SeaORM`'s
/// builder methods like `on_conflict()` for upsert semantics.
///
/// # Example
/// ```ignore
/// use modkit_db::secure::{AccessScope, SecureInsertExt};
/// use sea_orm::sea_query::OnConflict;
///
/// let scope = AccessScope::for_tenants(vec![tenant_id]);
/// let am = user::ActiveModel {
///     tenant_id: Set(tenant_id),
///     email: Set("user@example.com".to_string()),
///     ..Default::default()
/// };
///
/// user::Entity::insert(am)
///     .secure()                        // Returns SecureInsertOne<E, Unscoped>
///     .scope_with_model(&scope, &am)?  // Returns SecureInsertOne<E, Scoped>
///     .on_conflict(OnConflict::...)     // Builder methods still available
///     .exec(conn)                  // Now can execute
///     .await?;
/// ```
#[derive(Debug)]
pub struct SecureInsertOne<A, S>
where
    A: ActiveModelTrait,
{
    pub(crate) inner: sea_orm::Insert<A>,
    pub(crate) _state: PhantomData<S>,
}

/// Extension trait to convert a regular `SeaORM` `Insert` into a `SecureInsertOne`.
pub trait SecureInsertExt<A: ActiveModelTrait>: Sized {
    /// Convert this insert operation into a secure (unscoped) insert.
    /// You must call `.scope_with_model()` or `.scope_unchecked()` before executing.
    fn secure(self) -> SecureInsertOne<A, Unscoped>;
}

impl<A> SecureInsertExt<A> for sea_orm::Insert<A>
where
    A: ActiveModelTrait,
{
    fn secure(self) -> SecureInsertOne<A, Unscoped> {
        SecureInsertOne {
            inner: self,
            _state: PhantomData,
        }
    }
}

// Methods available only on Unscoped inserts
impl<A> SecureInsertOne<A, Unscoped>
where
    A: ActiveModelTrait + Send,
    A::Entity: ScopableEntity + EntityTrait,
    <A::Entity as EntityTrait>::Column: ColumnTrait + Copy,
{
    /// Transition to `Scoped` state **without** validating the `ActiveModel`
    /// against the scope constraints.
    ///
    /// # Safety (logical)
    ///
    /// This method performs **no** validation. The caller is responsible for
    /// ensuring the `ActiveModel` satisfies the scope (e.g., correct
    /// `tenant_id`). Prefer [`scope_with_model`](Self::scope_with_model)
    /// which validates all scope constraints automatically.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError`] if the access scope cannot be applied.
    pub fn scope_unchecked(
        self,
        scope: &AccessScope,
    ) -> Result<SecureInsertOne<A, Scoped>, ScopeError> {
        let _ = scope;
        Ok(SecureInsertOne {
            inner: self.inner,
            _state: PhantomData,
        })
    }

    /// Apply access control scope with explicit `ActiveModel` validation.
    ///
    /// This method validates **all** scope constraints against the `ActiveModel`'s
    /// column values (not just `tenant_id`). See [`validate_insert_scope`] for
    /// the full semantics.
    ///
    /// # Errors
    /// - Returns `ScopeError::Denied` if the `ActiveModel` values do not satisfy
    ///   any scope constraint.
    pub fn scope_with_model(
        self,
        scope: &AccessScope,
        am: &A,
    ) -> Result<SecureInsertOne<A, Scoped>, ScopeError> {
        validate_insert_scope(am, scope)?;
        Ok(SecureInsertOne {
            inner: self.inner,
            _state: PhantomData,
        })
    }
}

// Fluent builder methods (available only on Scoped inserts to prevent pre-scope execution)
impl<A> SecureInsertOne<A, Scoped>
where
    A: ActiveModelTrait,
    A::Entity: ScopableEntity + EntityTrait,
    <A::Entity as EntityTrait>::Column: ColumnTrait + Copy,
{
    /// Set the `ON CONFLICT` clause for upsert semantics using `SecureOnConflict`.
    ///
    /// This is the recommended way to add upsert semantics as it enforces
    /// tenant immutability at compile/validation time.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let on_conflict = SecureOnConflict::<Entity>::columns([Column::TenantId, Column::UserId])
    ///     .update_columns([Column::Theme, Column::Language])?;
    ///
    /// Entity::insert(am)
    ///     .secure()
    ///     .scope_unchecked(&scope)?
    ///     .on_conflict(on_conflict)
    ///     .exec(conn)
    ///     .await?;
    /// ```
    #[must_use]
    pub fn on_conflict(mut self, on_conflict: SecureOnConflict<A::Entity>) -> Self {
        self.inner = self.inner.on_conflict(on_conflict.build());
        self
    }

    /// Set the `ON CONFLICT` clause using raw `SeaORM` `OnConflict`.
    ///
    /// # Safety
    ///
    /// This method bypasses tenant immutability validation. The caller is
    /// responsible for ensuring that `tenant_id` is not included in update columns.
    /// Use `on_conflict()` with `SecureOnConflict` for automatic validation.
    #[must_use]
    pub fn on_conflict_raw(mut self, on_conflict: OnConflict) -> Self {
        self.inner = self.inner.on_conflict(on_conflict);
        self
    }
}

// Execution methods (require Scoped state)
impl<A> SecureInsertOne<A, Scoped>
where
    A: ActiveModelTrait,
{
    /// Execute the insert operation.
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the database operation fails.
    #[allow(clippy::disallowed_methods)]
    pub async fn exec<C>(self, runner: &C) -> Result<InsertResult<A>, ScopeError>
    where
        C: DBRunner,
        A: Send,
    {
        match DBRunnerInternal::as_seaorm(runner) {
            SeaOrmRunner::Conn(db) => Ok(self.inner.exec(db).await?),
            SeaOrmRunner::Tx(tx) => Ok(self.inner.exec(tx).await?),
        }
    }

    /// Execute the insert and return the inserted model.
    ///
    /// This is useful when you need the inserted data with any database-generated
    /// values (like auto-increment IDs or default values).
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the database operation fails.
    #[allow(clippy::disallowed_methods)]
    pub async fn exec_with_returning<C>(
        self,
        runner: &C,
    ) -> Result<<A::Entity as EntityTrait>::Model, ScopeError>
    where
        C: DBRunner,
        A: Send,
        <A::Entity as EntityTrait>::Model: IntoActiveModel<A>,
    {
        match DBRunnerInternal::as_seaorm(runner) {
            SeaOrmRunner::Conn(db) => Ok(self.inner.exec_with_returning(db).await?),
            SeaOrmRunner::Tx(tx) => Ok(self.inner.exec_with_returning(tx).await?),
        }
    }

    /// Unwrap the inner `SeaORM` `Insert` for advanced use cases.
    ///
    /// # Safety
    /// The caller must ensure they don't remove or bypass the security
    /// validation that was applied during `.scope_with_model()` / `.scope_unchecked()`.
    #[must_use]
    pub fn into_inner(self) -> sea_orm::Insert<A> {
        self.inner
    }
}

/// A secure builder for `ON CONFLICT DO UPDATE` clauses that enforces tenant immutability.
///
/// For tenant-scoped entities (`ScopableEntity::tenant_col() != None`), this builder
/// ensures that `tenant_id` is never included in the update columns. Attempting to
/// update `tenant_id` via `update_columns()` or `value()` returns an error.
///
/// # Security Rationale
///
/// `ON CONFLICT DO UPDATE` can be exploited to change an entity's tenant:
/// ```sql
/// INSERT INTO users (id, tenant_id, email) VALUES ($1, $2, $3)
/// ON CONFLICT (id) DO UPDATE SET tenant_id = excluded.tenant_id;
/// ```
/// This would allow moving a row from one tenant to another, violating tenant isolation.
///
/// # Example
///
/// ```ignore
/// use modkit_db::secure::{SecureOnConflict, SecureInsertExt};
/// use sea_orm::ActiveValue::Set;
///
/// let scope = AccessScope::single(ScopeConstraint::new(vec![
///     ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
///     ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, vec![user_id]),
/// ]));
/// let am = settings::ActiveModel {
///     tenant_id: Set(tenant_id),
///     user_id: Set(user_id),
///     theme: Set(Some("dark".to_string())),
///     language: Set(Some("en".to_string())),
/// };
///
/// // Build secure on_conflict - validates tenant_id is not updated
/// let on_conflict = SecureOnConflict::<settings::Entity>::columns([
///         settings::Column::TenantId,
///         settings::Column::UserId,
///     ])
///     .update_columns([settings::Column::Theme, settings::Column::Language])?;
///
/// settings::Entity::insert(am)
///     .secure()
///     .scope_unchecked(&scope)?
///     .on_conflict(on_conflict)
///     .exec(conn)
///     .await?;
/// ```
#[derive(Debug, Clone)]
pub struct SecureOnConflict<E: EntityTrait> {
    inner: OnConflict,
    _entity: PhantomData<E>,
}

impl<E> SecureOnConflict<E>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    /// Start building an `ON CONFLICT` clause with the specified conflict columns.
    ///
    /// These are the columns that define uniqueness (typically the primary key
    /// or a unique constraint).
    #[must_use]
    pub fn columns<C, I>(cols: I) -> Self
    where
        C: IntoIden,
        I: IntoIterator<Item = C>,
    {
        Self {
            inner: OnConflict::columns(cols),
            _entity: PhantomData,
        }
    }

    /// Specify columns to update on conflict.
    ///
    /// # Errors
    ///
    /// Returns `ScopeError::Denied("tenant_id is immutable")` if the entity has
    /// a tenant column and it appears in the update columns list.
    pub fn update_columns<C, I>(mut self, cols: I) -> Result<Self, ScopeError>
    where
        C: IntoIden + Copy + 'static,
        I: IntoIterator<Item = C>,
    {
        let cols: Vec<C> = cols.into_iter().collect();

        // Check if tenant column is in the update list
        if let Some(tenant_col) = E::tenant_col() {
            let tenant_iden = tenant_col.into_iden();
            for col in &cols {
                let col_iden = col.into_iden();
                if col_iden.to_string() == tenant_iden.to_string() {
                    return Err(ScopeError::Denied("tenant_id is immutable"));
                }
            }
        }

        self.inner.update_columns(cols);
        Ok(self)
    }

    /// Set a custom update expression for a column on conflict.
    ///
    /// # Errors
    ///
    /// Returns `ScopeError::Denied("tenant_id is immutable")` if the entity has
    /// a tenant column and the specified column matches it.
    pub fn value<C>(mut self, col: C, expr: SimpleExpr) -> Result<Self, ScopeError>
    where
        C: IntoIden + Copy + 'static,
    {
        // Check if this is the tenant column
        if let Some(tenant_col) = E::tenant_col() {
            let tenant_iden = tenant_col.into_iden();
            let col_iden = col.into_iden();
            if col_iden.to_string() == tenant_iden.to_string() {
                return Err(ScopeError::Denied("tenant_id is immutable"));
            }
        }

        self.inner.value(col, expr);
        Ok(self)
    }

    /// Consume the builder and return the underlying `SeaORM` `OnConflict`.
    ///
    /// Call this after configuring all update columns/values.
    #[must_use]
    pub fn build(self) -> OnConflict {
        self.inner
    }

    /// Get a reference to the inner `OnConflict` for chaining with `SeaORM` methods
    /// that are not wrapped by this builder.
    ///
    /// # Safety
    ///
    /// The caller must ensure they don't add tenant column updates through the
    /// inner `OnConflict` directly, as this would bypass the security check.
    #[must_use]
    pub fn inner_mut(&mut self) -> &mut OnConflict {
        &mut self.inner
    }
}

/// A type-safe wrapper around `SeaORM`'s `UpdateMany` that enforces scoping.
///
/// This wrapper uses the typestate pattern to ensure that update operations
/// cannot be executed without first applying access control via `.scope_with()`.
///
/// # Example
/// ```ignore
/// use modkit_db::secure::{AccessScope, SecureUpdateExt};
///
/// let scope = AccessScope::for_tenants(vec![tenant_id]);
/// let result = user::Entity::update_many()
///     .col_expr(user::Column::Status, Expr::value("active"))
///     .secure()           // Returns SecureUpdateMany<E, Unscoped>
///     .scope_with(&scope)? // Returns SecureUpdateMany<E, Scoped>
///     .exec(conn)         // Now can execute
///     .await?;
/// ```
#[derive(Clone, Debug)]
pub struct SecureUpdateMany<E: EntityTrait, S> {
    pub(crate) inner: sea_orm::UpdateMany<E>,
    pub(crate) _state: PhantomData<S>,
    pub(crate) tenant_update_attempted: bool,
}

// Fluent builder methods (available in all typestates).
impl<E, S> SecureUpdateMany<E, S>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    /// Set a column expression (mirrors `SeaORM`'s `UpdateMany::col_expr`).
    #[must_use]
    pub fn col_expr(mut self, col: E::Column, expr: sea_orm::sea_query::SimpleExpr) -> Self {
        if let Some(tcol) = E::tenant_col()
            && std::mem::discriminant(&col) == std::mem::discriminant(&tcol)
        {
            self.tenant_update_attempted = true;
        }
        self.inner = self.inner.col_expr(col, expr);
        self
    }

    /// Add an additional filter. Scope conditions remain in place once applied.
    #[must_use]
    pub fn filter(mut self, filter: sea_orm::Condition) -> Self {
        self.inner = QueryFilter::filter(self.inner, filter);
        self
    }
}

/// Extension trait to convert a regular `SeaORM` `UpdateMany` into a `SecureUpdateMany`.
pub trait SecureUpdateExt<E: EntityTrait>: Sized {
    /// Convert this update operation into a secure (unscoped) update.
    /// You must call `.scope_with()` before executing.
    fn secure(self) -> SecureUpdateMany<E, Unscoped>;
}

impl<E> SecureUpdateExt<E> for sea_orm::UpdateMany<E>
where
    E: EntityTrait,
{
    fn secure(self) -> SecureUpdateMany<E, Unscoped> {
        SecureUpdateMany {
            inner: self,
            _state: PhantomData,
            tenant_update_attempted: false,
        }
    }
}

// Methods available only on Unscoped updates
impl<E> SecureUpdateMany<E, Unscoped>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    /// Apply access control scope to this update, transitioning to the `Scoped` state.
    ///
    /// This applies the implicit policy:
    /// - Empty scope → deny all (no rows updated)
    /// - Tenants only → update only in specified tenants
    /// - Resources only → update only specified resource IDs
    /// - Both → AND them together
    ///
    #[must_use]
    pub fn scope_with(self, scope: &AccessScope) -> SecureUpdateMany<E, Scoped> {
        let cond = build_scope_condition::<E>(scope);
        SecureUpdateMany {
            inner: self.inner.filter(cond),
            _state: PhantomData,
            tenant_update_attempted: self.tenant_update_attempted,
        }
    }
}

// Methods available only on Scoped updates
impl<E> SecureUpdateMany<E, Scoped>
where
    E: EntityTrait,
{
    /// Execute the update operation.
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the database operation fails.
    #[allow(clippy::disallowed_methods)]
    pub async fn exec(self, runner: &impl DBRunner) -> Result<sea_orm::UpdateResult, ScopeError> {
        if self.tenant_update_attempted {
            return Err(ScopeError::Denied("tenant_id is immutable"));
        }
        match DBRunnerInternal::as_seaorm(runner) {
            SeaOrmRunner::Conn(db) => Ok(self.inner.exec(db).await?),
            SeaOrmRunner::Tx(tx) => Ok(self.inner.exec(tx).await?),
        }
    }

    /// Unwrap the inner `SeaORM` `UpdateMany` for advanced use cases.
    ///
    /// # Safety
    /// The caller must ensure they don't remove or bypass the security
    /// conditions that were applied during `.scope_with()`.
    #[must_use]
    pub fn into_inner(self) -> sea_orm::UpdateMany<E> {
        self.inner
    }
}

/// A type-safe wrapper around `SeaORM`'s `DeleteMany` that enforces scoping.
///
/// This wrapper uses the typestate pattern to ensure that delete operations
/// cannot be executed without first applying access control via `.scope_with()`.
///
/// # Example
/// ```ignore
/// use modkit_db::secure::{AccessScope, SecureDeleteExt};
///
/// let scope = AccessScope::for_tenants(vec![tenant_id]);
/// let result = user::Entity::delete_many()
///     .filter(user::Column::Status.eq("inactive"))
///     .secure()           // Returns SecureDeleteMany<E, Unscoped>
///     .scope_with(&scope)? // Returns SecureDeleteMany<E, Scoped>
///     .exec(conn)         // Now can execute
///     .await?;
/// ```
#[derive(Clone, Debug)]
pub struct SecureDeleteMany<E: EntityTrait, S> {
    pub(crate) inner: sea_orm::DeleteMany<E>,
    pub(crate) _state: PhantomData<S>,
}

/// Extension trait to convert a regular `SeaORM` `DeleteMany` into a `SecureDeleteMany`.
pub trait SecureDeleteExt<E: EntityTrait>: Sized {
    /// Convert this delete operation into a secure (unscoped) delete.
    /// You must call `.scope_with()` before executing.
    fn secure(self) -> SecureDeleteMany<E, Unscoped>;
}

impl<E> SecureDeleteExt<E> for sea_orm::DeleteMany<E>
where
    E: EntityTrait,
{
    fn secure(self) -> SecureDeleteMany<E, Unscoped> {
        SecureDeleteMany {
            inner: self,
            _state: PhantomData,
        }
    }
}

// Methods available only on Unscoped deletes
impl<E> SecureDeleteMany<E, Unscoped>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    /// Apply access control scope to this delete, transitioning to the `Scoped` state.
    ///
    /// This applies the implicit policy:
    /// - Empty scope → deny all (no rows deleted)
    /// - Tenants only → delete only in specified tenants
    /// - Resources only → delete only specified resource IDs
    /// - Both → AND them together
    ///
    #[must_use]
    pub fn scope_with(self, scope: &AccessScope) -> SecureDeleteMany<E, Scoped> {
        let cond = build_scope_condition::<E>(scope);
        SecureDeleteMany {
            inner: self.inner.filter(cond),
            _state: PhantomData,
        }
    }
}

// Methods available only on Scoped deletes
impl<E> SecureDeleteMany<E, Scoped>
where
    E: EntityTrait,
{
    /// Add additional filters to the scoped delete.
    /// The scope conditions remain in place.
    #[must_use]
    pub fn filter(mut self, filter: sea_orm::Condition) -> Self {
        self.inner = QueryFilter::filter(self.inner, filter);
        self
    }

    /// Execute the delete operation.
    ///
    /// # Errors
    /// Returns `ScopeError::Db` if the database operation fails.
    #[allow(clippy::disallowed_methods)]
    pub async fn exec(self, runner: &impl DBRunner) -> Result<sea_orm::DeleteResult, ScopeError> {
        match DBRunnerInternal::as_seaorm(runner) {
            SeaOrmRunner::Conn(db) => Ok(self.inner.exec(db).await?),
            SeaOrmRunner::Tx(tx) => Ok(self.inner.exec(tx).await?),
        }
    }

    /// Unwrap the inner `SeaORM` `DeleteMany` for advanced use cases.
    ///
    /// # Safety
    /// The caller must ensure they don't remove or bypass the security
    /// conditions that were applied during `.scope_with()`.
    #[must_use]
    pub fn into_inner(self) -> sea_orm::DeleteMany<E> {
        self.inner
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "db_ops_tests.rs"]
mod tests;
