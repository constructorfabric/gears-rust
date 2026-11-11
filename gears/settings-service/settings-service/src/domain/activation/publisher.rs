// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-record:p1
//! The Activation Publisher's recording half: which subscriptions a change
//! affects, and the activation state written for them in the change's own
//! transaction.
//!
//! `record_change` runs under the key's mutation lock the Value Writer holds,
//! inside the change's transaction, after the value row and its audit record.
//! It takes no lock of its own and calls no Tenant Resolver: the subscriptions
//! that qualify, and the ancestor chain of each qualifying tenant of a
//! `cascading` key, were decided under the same lock before the transaction
//! opened and arrive here.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use secrecy::SecretString;
use serde_json::Value;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::canonical::{CanonicalError, ExpectedValue, canonical_json};
use super::subscriptions::ActivationRepository;
use super::{SubscriberTenant, Subscription};
use crate::domain::access::AccessRepository;
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::resolver::{EffectiveOver, effective_over};
use crate::domain::resolution::{ValueResolver, scope_class};
use crate::domain::value::{StoredValue, ValueRepository};
use crate::field;

/// A subscription that qualified for a change, with the ancestor chain of its
/// tenant for a `cascading` key (empty for `global` and `local`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qualified {
    /// The subscription, as the one read of the key's subscriptions saw it.
    pub subscription: Subscription,
    /// Its tenant.
    pub tenant: Uuid,
    /// The tenant's root-to-self chain, for a `cascading` key.
    pub chain: Vec<Uuid>,
}

/// One activation state a change writes.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRow {
    /// The id of the subscription in the change's read set.
    pub subscription_id: Uuid,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// The encoded expected value at that tenant.
    pub expected_value: String,
}

/// The activation states of one change, for the repository.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateWrite {
    /// The changed key.
    pub key: String,
    /// The id of the change's audit record.
    pub change_ref: Uuid,
    /// One row per affected subscription.
    pub rows: Vec<StateRow>,
}

/// What `record_change` is told about one change, inside its transaction.
#[domain_model]
pub struct RecordedChange<'a> {
    /// The changed setting.
    pub declaration: &'a Declaration,
    /// The scope the change was written at, `X`.
    pub written: Uuid,
    /// The root tenant.
    pub root: Uuid,
    /// The row `X` held before the change, if any: the pre-image.
    pub pre_image: Option<&'a StoredValue>,
    /// The id of the change's audit record.
    pub change_ref: Uuid,
    /// The subscriptions that qualified, under the lock.
    pub qualified: &'a [Qualified],
    /// The plaintext of the value in force at the affected tenants after the
    /// change, for a `secret`-trait setting: the inline value, a staged one,
    /// the inherited fallback's or the default's.
    pub plaintext: Option<&'a SecretString>,
}

/// What the Value Writer's commit is handed to record a change's activation
/// state: the publisher, the subscriptions that qualified under the lock and the
/// plaintext the expected value of a secret is hashed from.
#[domain_model]
pub struct ActivationInput<'a, D, V, A, R> {
    /// The publisher to record the change with.
    pub publisher: &'a ActivationPublisher<D, V, A, R>,
    /// The subscriptions that qualified under the key's mutation lock.
    pub qualified: &'a [Qualified],
    /// The plaintext of the value in force at the affected tenants after the
    /// change, for a `secret`-trait setting.
    pub plaintext: Option<&'a SecretString>,
}

/// The publisher.
#[domain_model]
pub struct ActivationPublisher<D, V, A, R> {
    resolver: Arc<ValueResolver<D, V, A>>,
    repo: R,
}

/// The scopes a value is selected among, root to self, with the rows found
/// there.
type RowsOver = (Vec<Uuid>, HashMap<Uuid, StoredValue>);

/// An affected subscription with the value in force at its tenant after the
/// change.
struct Affected<'a> {
    qualified: &'a Qualified,
    value: Value,
}

fn not_canonical(err: &CanonicalError) -> DomainError {
    DomainError::Validation {
        field: "value".to_owned(),
        code: field::VALUE_NOT_CANONICAL,
        message: err.to_string(),
    }
}

/// The expected value of one affected subscription: the salted hash of the
/// plaintext for a `secret`-trait setting, the canonical encoding of the value
/// in force otherwise.
fn row_of(change: &RecordedChange<'_>, affected: &Affected<'_>) -> Result<StateRow, DomainError> {
    let subscription = &affected.qualified.subscription;
    let expected = if change.declaration.has_secret_trait {
        let plaintext = change.plaintext.ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "a change of secret setting `{}` reached the activation record without its plaintext",
                change.declaration.key
            ),
        })?;
        ExpectedValue::of_secret(&change.declaration.key, &subscription.tenant, plaintext)
    } else {
        ExpectedValue::of_value(&affected.value)
    }
    .map_err(|e| not_canonical(&e))?;
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-8
    Ok(StateRow {
        subscription_id: subscription.id,
        tenant: subscription.tenant.clone(),
        expected_value: expected.into_string(),
    })
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-8
}

/// The effective value at `tenant` before the change: the rows read after it,
/// with the written scope's row put back as the pre-image held it — removed
/// when there was none, as for a first override, in which case the value is
/// what the scope inherited.
fn before(
    change: &RecordedChange<'_>,
    tenant: Uuid,
    inspected: &[Uuid],
    rows_after: &HashMap<Uuid, StoredValue>,
) -> EffectiveOver {
    let mut rows = rows_after.clone();
    match change.pre_image {
        Some(row) => {
            rows.insert(change.written, row.clone());
        }
        None => {
            rows.remove(&change.written);
        }
    }
    effective_over(change.declaration, tenant, change.root, inspected, &rows)
}

impl<D, V, A, R> ActivationPublisher<D, V, A, R>
where
    D: DeclarationRepository,
    V: ValueRepository + Clone,
    A: AccessRepository,
    R: ActivationRepository,
{
    /// A publisher over the resolver and the repository.
    pub fn new(resolver: Arc<ValueResolver<D, V, A>>, repo: R) -> Self {
        Self { resolver, repo }
    }

    /// Record the change: keep the affected subscriptions, encode each expected
    /// value and write the `awaiting` states in the caller's transaction.
    /// Returns the pairs of subscriber and tenant that now hold this change's
    /// state, for the end-of-request publish.
    ///
    /// # Errors
    /// [`DomainError`] when a read or the write fails, or a value cannot be
    /// encoded: the caller's transaction rolls back and nothing is stored.
    pub async fn record_change<C: DBRunner>(
        &self,
        tx: &C,
        change: &RecordedChange<'_>,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-17
        if change.qualified.is_empty() {
            return Ok(Vec::new());
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-18
        let affected = self.affected(tx, change).await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-18
        if affected.is_empty() {
            return Ok(Vec::new());
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-19
        let rows = affected
            .iter()
            .map(|a| row_of(change, a))
            .collect::<Result<Vec<_>, _>>()?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-19
        self.repo
            .write_states(
                tx,
                &StateWrite {
                    key: change.declaration.key.clone(),
                    change_ref: change.change_ref,
                    rows,
                },
            )
            .await
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-17
    }

    /// The qualified subscriptions the change affects, each with the effective
    /// value at its tenant after the change, resolved through the transaction
    /// and bypassing the cache.
    async fn affected<'q, C: DBRunner>(
        &self,
        tx: &C,
        change: &'q RecordedChange<'q>,
    ) -> Result<Vec<Affected<'q>>, DomainError> {
        let declaration = change.declaration;
        let secret = declaration.has_secret_trait;
        let cascading = declaration.scope_class == scope_class::CASCADING;
        // The rows over one tenant's chain, read once however many subscribers
        // watch that tenant.
        let mut read: HashMap<Uuid, RowsOver> = HashMap::new();
        let mut affected = Vec::new();
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-1
        for qualified in change.qualified {
            let tenant = qualified.tenant;
            let (inspected, rows_after) = match read.entry(tenant) {
                Entry::Occupied(found) => found.into_mut(),
                Entry::Vacant(slot) => slot.insert(
                    self.resolver
                        .rows_over(tx, declaration, tenant, change.root, &qualified.chain)
                        .await?,
                ),
            };
            let (inspected, rows_after) = (&*inspected, &*rows_after);
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-2
            // A nearer override shadows the change: a valid row at a tenant of
            // the chain below the written scope — read from the store in this
            // transaction — keeps the subscription's value where it was. A row
            // flagged for review does not shadow, as the resolver skips it.
            if cascading && tenant != change.written {
                let below = qualified
                    .chain
                    .iter()
                    .position(|t| *t == change.written)
                    .map_or(&[][..], |at| &qualified.chain[at + 1..]);
                if below
                    .iter()
                    .any(|t| rows_after.get(t).is_some_and(|row| !row.needs_review))
                {
                    continue;
                }
            }
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-2
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-3
            let after = effective_over(declaration, tenant, change.root, inspected, rows_after);
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-3
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-5
            if secret {
                // The pre-image holds only a reference: every write counts.
                affected.push(Affected {
                    qualified,
                    value: after.value,
                });
                continue;
            }
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-5
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-4
            let before = before(change, tenant, inspected, rows_after);
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-4
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-6
            let differs = canonical_json(&after.value).map_err(|e| not_canonical(&e))?
                != canonical_json(&before.value).map_err(|e| not_canonical(&e))?;
            if differs {
                affected.push(Affected {
                    qualified,
                    value: after.value,
                });
            }
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-6
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-7
            // Else the subscription is not affected and gets no row: a write of
            // an identical value affects nothing.
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-7
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-1
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-9
        Ok(affected)
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-9
    }
}

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod publisher_tests;
