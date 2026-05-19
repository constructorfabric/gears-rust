//! `AccessScope` → SQL WHERE fragment translator.

use modkit_macros::domain_model;
use modkit_security::{AccessScope, ScopeFilter, ScopeValue, pep_properties};
use uuid::Uuid;

use crate::domain::error::ScopeTranslationError;

/// Property name for the resource identifier used in usage records.
const PROP_RESOURCE_ID: &str = "resource_id";

/// Property name for the resource type used in usage records.
const PROP_RESOURCE_TYPE: &str = "resource_type";

/// Property name for the source module used in usage records.
const PROP_MODULE: &str = "module";

/// Property name for the subject identifier used in usage records.
const PROP_SUBJECT_ID: &str = "subject_id";

/// Property name for the subject type used in usage records.
const PROP_SUBJECT_TYPE: &str = "subject_type";

/// A typed SQL bind parameter produced by [`scope_to_sql`].
///
/// Callers bind these values positionally to a sqlx query in the order they appear
/// in the returned `Vec<SqlValue>`.
#[derive(Debug, Clone, PartialEq)]
#[domain_model]
pub enum SqlValue {
    Uuid(Uuid),
    UuidArray(Vec<Uuid>),
    Text(String),
    TextArray(Vec<String>),
}

/// Returns `true` when any filter in `scope` constrains the row-level
/// `resource_id` or `subject_id` columns. The continuous aggregate
/// (`usage_agg_1h`) groups those columns out — see `infra/continuous_aggregate.rs`
/// — so any query whose scope filters on them must execute against the raw
/// hypertable or the generated SQL will reference columns that don't exist on
/// the view. Centralised here so the property-name constants stay private to
/// this module.
#[must_use]
pub fn scope_constrains_record_ids(scope: &AccessScope) -> bool {
    scope.constraints().iter().any(|c| {
        c.filters()
            .iter()
            .any(|f| f.property() == PROP_RESOURCE_ID || f.property() == PROP_SUBJECT_ID)
    })
}

// @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1
/// Translate an `AccessScope` into a SQL WHERE fragment and positional bind parameters.
///
/// The returned fragment is ready to embed in `WHERE (<fragment>)`. Bind parameters
/// must be appended after any pre-existing parameters the caller already has.
///
/// # Errors
///
/// Returns [`ScopeTranslationError::EmptyScope`] when the scope has no constraints
/// (deny-all or allow-all — callers must fail closed in both cases).
///
/// Returns [`ScopeTranslationError::UnsupportedPredicate`] when the scope contains
/// `InGroup`/`InGroupSubtree` predicates or an unrecognised property name.
pub fn scope_to_sql(scope: &AccessScope) -> Result<(String, Vec<SqlValue>), ScopeTranslationError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-1
    if scope.constraints().is_empty() {
        return Err(ScopeTranslationError::EmptyScope);
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-1

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-2
    let mut group_fragments: Vec<String> = Vec::new();
    let mut bind_params: Vec<SqlValue> = Vec::new();
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-2

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3
    for constraint in scope.constraints() {
        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3a
        let mut predicate_fragments: Vec<String> = Vec::new();
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3a

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b
        for filter in constraint.filters() {
            // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-i
            // Early variant rejection keeps the "InGroup/InGroupSubtree" error label
            // independent of which property the filter targets. `build_uuid_filter`
            // and `build_text_filter` also enforce the same invariant locally — so a
            // future refactor that bypasses this loop still cannot smuggle an
            // unsupported variant past the SQL builders.
            if matches!(
                filter,
                ScopeFilter::InGroup(_) | ScopeFilter::InGroupSubtree(_)
            ) {
                return Err(ScopeTranslationError::UnsupportedPredicate {
                    kind: "InGroup/InGroupSubtree".to_owned(),
                });
            }
            // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-i

            let param_n = bind_params.len() + 1;

            // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-ii
            // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-iii
            // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-iv
            let property = filter.property();
            let (frag, val) = match property {
                pep_properties::OWNER_TENANT_ID => build_uuid_filter(filter, param_n, "tenant_id")?,
                PROP_RESOURCE_ID => build_uuid_filter(filter, param_n, "resource_id")?,
                PROP_RESOURCE_TYPE => build_text_filter(filter, param_n, "resource_type")?,
                PROP_MODULE => build_text_filter(filter, param_n, "module")?,
                PROP_SUBJECT_ID => build_uuid_filter(filter, param_n, "subject_id")?,
                PROP_SUBJECT_TYPE => build_text_filter(filter, param_n, "subject_type")?,
                other => {
                    return Err(ScopeTranslationError::UnsupportedPredicate {
                        kind: format!("unknown property: {other}"),
                    });
                }
            };
            predicate_fragments.push(frag);
            bind_params.push(val);
            // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-iv
            // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-iii
            // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b-ii
        }
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3b

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3c
        if predicate_fragments.is_empty() {
            return Err(ScopeTranslationError::EmptyScope);
        }
        group_fragments.push(format!("({})", predicate_fragments.join(" AND ")));
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3c
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-3

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-4
    let sql_fragment = format!("({})", group_fragments.join(" OR "));
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-4

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-5
    Ok((sql_fragment, bind_params))
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-scope-to-sql:p1:inst-s2s-5
}

fn build_uuid_filter(
    filter: &ScopeFilter,
    param_n: usize,
    column: &str,
) -> Result<(String, SqlValue), ScopeTranslationError> {
    match filter {
        ScopeFilter::Eq(f) => {
            let uuid =
                f.value()
                    .as_uuid()
                    .ok_or_else(|| ScopeTranslationError::UnsupportedPredicate {
                        kind: format!("non-UUID value for {column}"),
                    })?;
            Ok((format!("{column} = ${param_n}"), SqlValue::Uuid(uuid)))
        }
        ScopeFilter::In(f) => {
            let uuids: Result<Vec<Uuid>, ScopeTranslationError> = f
                .values()
                .iter()
                .map(|v| {
                    v.as_uuid()
                        .ok_or_else(|| ScopeTranslationError::UnsupportedPredicate {
                            kind: format!("non-UUID value for {column}"),
                        })
                })
                .collect();
            Ok((
                format!("{column} = ANY(${param_n})"),
                SqlValue::UuidArray(uuids?),
            ))
        }
        ScopeFilter::InGroup(_) | ScopeFilter::InGroupSubtree(_) => {
            Err(ScopeTranslationError::UnsupportedPredicate {
                kind: "InGroup/InGroupSubtree".to_owned(),
            })
        }
    }
}

fn build_text_filter(
    filter: &ScopeFilter,
    param_n: usize,
    column: &str,
) -> Result<(String, SqlValue), ScopeTranslationError> {
    // Mirror `build_uuid_filter`'s type discipline: a misconfigured PDP that
    // surfaces a non-string scope value (e.g. `ScopeValue::Uuid`) on a text
    // column must produce a translator error here, not a silently stringified
    // value that reaches the DB as a row mismatch.
    fn require_string(value: &ScopeValue, column: &str) -> Result<String, ScopeTranslationError> {
        match value {
            ScopeValue::String(s) => Ok(s.clone()),
            _ => Err(ScopeTranslationError::UnsupportedPredicate {
                kind: format!("non-text value for {column}"),
            }),
        }
    }

    match filter {
        ScopeFilter::Eq(f) => {
            let text = require_string(f.value(), column)?;
            Ok((format!("{column} = ${param_n}"), SqlValue::Text(text)))
        }
        ScopeFilter::In(f) => {
            let texts: Result<Vec<String>, ScopeTranslationError> = f
                .values()
                .iter()
                .map(|v| require_string(v, column))
                .collect();
            Ok((
                format!("{column} = ANY(${param_n})"),
                SqlValue::TextArray(texts?),
            ))
        }
        ScopeFilter::InGroup(_) | ScopeFilter::InGroupSubtree(_) => {
            Err(ScopeTranslationError::UnsupportedPredicate {
                kind: "InGroup/InGroupSubtree".to_owned(),
            })
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "scope_tests.rs"]
mod scope_tests;
