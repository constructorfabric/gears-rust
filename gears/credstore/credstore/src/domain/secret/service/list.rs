//! `Service::list` — the collection read (`GET /credstore/v1/credentials`,
//! ADR-0005/ADR-0004).
//!
//! Child module of [`crate::domain::secret::service`] (see that module's
//! `mod list;` doc comment for why): this file's `impl Service` block
//! reaches `Service`'s private fields and helpers (`repo`, `dir`, `plugins`,
//! `metrics`, `scope_for_timed`, `resolve_stored`, `read_value_for_row`)
//! without exposing any of them beyond the `service` module subtree.

use std::collections::HashMap;
use std::sync::Arc;

use credstore_sdk::{
    CredStorePluginClientV1, Credential, CredentialListItem, OwnerId, Secret, SecretRef, TenantId,
    Validator,
};
use futures::stream::{self, StreamExt};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, Page, PageInfo, SortDir};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::authz::{self, actions};
use crate::domain::error::DomainError;
use crate::domain::secret::list_filter::{self, ListDirection, ParsedFilter};
use crate::domain::secret::model::SecretRow;
use crate::domain::secret::reduce::{self, Reduced};
use crate::domain::secret::type_resolver::ResolvedSecretType;

use super::Service;

/// Default page size when the caller sends no `limit`/`$top` (ADR-0005
/// follows the platform's generic pagination default; credstore names no
/// smaller default of its own).
const DEFAULT_LIMIT: u64 = 50;

/// Bounded parallelism for secret-mode value reads (ADR-0005 "Bulk secret
/// read"): a handful of in-flight reads amortises a remote vault's RTT
/// without stampeding it, and the cap of 25 (`list.secret_mode_cap`) bounds
/// the total there is ever to read.
const SECRET_READ_CONCURRENCY: usize = 8;

/// Map a [`toolkit_odata::Error`] (cursor/order/filter-consistency failures
/// the toolkit itself detects) onto the domain error shape the rest of this
/// module's validation already uses, so every rejection — toolkit-detected
/// or credstore-specific — renders through the same `CredentialResource`
/// mapping at the REST boundary.
fn map_odata_err(err: &toolkit_odata::Error) -> DomainError {
    use toolkit_odata::Error;
    let (field, reason): (&'static str, &'static str) = match err {
        Error::OrderMismatch => ("$orderby", "ORDER_MISMATCH"),
        Error::FilterMismatch => ("$filter", "FILTER_MISMATCH"),
        Error::InvalidCursor
        | Error::CursorInvalidBase64
        | Error::CursorInvalidJson
        | Error::CursorInvalidVersion
        | Error::CursorInvalidKeys
        | Error::CursorInvalidFields
        | Error::CursorInvalidDirection => ("cursor", "INVALID_CURSOR"),
        Error::InvalidOrderByField(_) => ("$orderby", "INVALID_ORDERBY_FIELD"),
        Error::InvalidFilter(_) => ("$filter", "INVALID_FILTER"),
        Error::InvalidLimit => ("limit", "INVALID_LIMIT"),
        Error::OrderWithCursor => ("$orderby", "ORDER_WITH_CURSOR"),
        Error::Db(_) | Error::ParsingUnavailable(_) => {
            return DomainError::internal(format!("OData: {err}"));
        }
    };
    DomainError::InvalidRequest {
        field,
        reason,
        detail: err.to_string(),
    }
}

fn invalid_cursor(detail: impl Into<String>) -> DomainError {
    DomainError::InvalidRequest {
        field: "cursor",
        reason: "INVALID_CURSOR",
        detail: detail.into(),
    }
}

/// Build the `Credential` a reduced reference resolves to — the same
/// construction [`Service::resolve_credential`] applies for the point read,
/// so a list item and a point read of the same reference always agree.
fn build_credential(key: &SecretRef, secret_type: String, reduced: &Reduced<'_>) -> Credential {
    let (fallback, version, updated_at, owner_id, validator) = match reduced.own {
        Some(o) => (
            Some(credstore_sdk::Fallback::from(o.fallback)),
            Some(o.version),
            Some(o.updated_at),
            Some(o.owner_id),
            Some(Validator {
                id: o.id,
                version: o.version,
            }),
        ),
        None => (None, None, None, None, None),
    };
    Credential {
        reference: key.clone(),
        secret_type,
        sharing: reduced.effective.sharing,
        fallback,
        status: reduced.own_status(),
        inheritance: reduced.inheritance,
        version,
        updated_at,
        owner_id,
        expires_at: reduced.effective.expires_at,
        validator,
    }
}

/// One winner whose backend value phase 2 of secret mode still has to read
/// (`Service::reduce_and_authorize`): the `Credential` phase 1 already
/// built, the reference key, a clone of the winning row, and its resolved
/// GTS id — every argument [`Service::read_value_for_row`] needs, captured
/// up front (owned, not borrowed, so the phase-2 fan-out closure needs no
/// lifetime the compiler must check against every call site) so the
/// fan-out shares nothing mutable beyond `&self`.
struct SecretReadJob {
    credential: Credential,
    key: SecretRef,
    winner: SecretRow,
    gts_id: String,
}

impl Service {
    /// The collection read (ADR-0005): one reduced item per reference,
    /// rooted at the caller's tenant and walking upward through its
    /// ancestor chain only. Selecting `secret` in `query`'s `$select`
    /// switches to secret mode (ADR-0004, "Bulk secret read").
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidRequest`] for every validation failure
    /// this endpoint defines (see [`crate::domain::secret::list_filter`]):
    /// an unsupported `$filter`/`$orderby`/`$select` field or shape, an
    /// out-of-range `limit`, a malformed or inconsistent cursor, or (secret
    /// mode) pagination present, a missing/invalid selector, or a
    /// match-set over the configured cap.
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<CredentialListItem>, DomainError> {
        if let Some(fields) = query.selected_fields() {
            list_filter::validate_select(fields)?;
        }
        let parsed_filter = match query.filter() {
            Some(expr) => list_filter::parse_filter(expr)?,
            None => ParsedFilter::default(),
        };

        if list_filter::is_secret_mode(query.selected_fields()) {
            self.list_secret_mode(ctx, query, &parsed_filter).await
        } else {
            self.list_metadata_mode(ctx, query, &parsed_filter).await
        }
    }

    async fn list_metadata_mode(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        parsed_filter: &ParsedFilter,
    ) -> Result<Page<CredentialListItem>, DomainError> {
        let limit = match query.limit {
            Some(l) if l > self.list.max_limit => {
                return Err(DomainError::InvalidRequest {
                    field: "limit",
                    reason: "INVALID_LIMIT",
                    detail: format!("limit must be <= {}", self.list.max_limit),
                });
            }
            Some(l) => l,
            None => DEFAULT_LIMIT.min(self.list.max_limit),
        };

        let (direction, cursor_reference) = if let Some(cursor) = &query.cursor {
            let effective_order =
                ODataOrderBy::from_signed_tokens(&cursor.s).map_err(|e| map_odata_err(&e))?;
            let [key] = effective_order.0.as_slice() else {
                return Err(invalid_cursor("cursor does not name a single sort key"));
            };
            if !key.field.eq_ignore_ascii_case("reference") {
                return Err(invalid_cursor("cursor was not minted by this endpoint"));
            }
            toolkit_odata::validate_cursor_against(
                cursor,
                &effective_order,
                query.filter_hash.as_deref(),
            )
            .map_err(|e| map_odata_err(&e))?;
            let [only_key] = cursor.k.as_slice() else {
                return Err(invalid_cursor("cursor does not carry exactly one key"));
            };
            let direction = match cursor.o {
                SortDir::Asc => ListDirection::Asc,
                SortDir::Desc => ListDirection::Desc,
            };
            (direction, Some(only_key.clone()))
        } else {
            (list_filter::validate_metadata_orderby(&query.order)?, None)
        };

        let req = TenantId(ctx.subject_tenant_id());
        let subject = OwnerId(ctx.subject_id());
        let chain = self.dir.ancestor_chain(ctx, req).await?;

        // Before step 1: the PDP-permitted type set becomes step 1's SQL
        // clamp (ADR-0005, ADR-0010) — never a scan of every visible row
        // followed by an in-memory authorization pass.
        let allowed_types = self
            .permitted_types(
                ctx,
                req,
                subject,
                &chain,
                parsed_filter.type_uuid_in.as_deref(),
                &[actions::LIST],
            )
            .await?;
        // A caller the gate refuses gets an empty page, never a refusal
        // (ADR-0005 §"How authorization applies to a collection", step 4):
        // there is no permitted type left to build a step 1 query from, so
        // step 1 never runs.
        if allowed_types.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                page_info: PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit,
                },
            });
        }
        let allowed_type_uuids: Vec<Uuid> = allowed_types.keys().copied().collect();

        let fetch_limit = limit
            .checked_add(1)
            .ok_or_else(|| DomainError::internal("limit + 1 overflowed u64"))?;
        // The type clamp here is the PDP-permitted set computed above (the
        // caller's own `$filter type in (…)` is already folded into it) —
        // `has_more`/cursor minting below therefore count only references
        // the caller may see.
        let mut refs = self
            .repo
            .list_candidate_references(
                req,
                subject,
                &chain,
                parsed_filter.reference_in.as_deref(),
                Some(&allowed_type_uuids),
                cursor_reference.as_deref(),
                direction.is_desc(),
                fetch_limit,
            )
            .await?;

        let has_more = refs.len() as u64 > limit;
        if has_more {
            refs.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }

        let items = self
            .reduce_and_authorize(
                ctx,
                req,
                subject,
                &chain,
                &refs,
                parsed_filter,
                &allowed_types,
                false,
            )
            .await?;

        let next_cursor = if has_more {
            let last_reference = refs
                .last()
                .cloned()
                .ok_or_else(|| DomainError::internal("has_more true but no references fetched"))?;
            let order_dir = if direction.is_desc() {
                SortDir::Desc
            } else {
                SortDir::Asc
            };
            let order = ODataOrderBy(vec![OrderKey {
                field: "reference".to_owned(),
                dir: order_dir,
            }]);
            let cursor = CursorV1 {
                k: vec![last_reference],
                o: order_dir,
                s: order.to_signed_tokens(),
                f: query.filter_hash.clone(),
                d: "fwd".to_owned(),
            };
            Some(
                cursor
                    .encode()
                    .map_err(|e| DomainError::internal(format!("cursor encode failed: {e}")))?,
            )
        } else {
            None
        };

        Ok(Page {
            items,
            page_info: PageInfo {
                next_cursor,
                // Backward pagination is out of scope for this endpoint
                // (ADR-0005 describes only a forward walk); always `None`.
                prev_cursor: None,
                limit,
            },
        })
    }

    async fn list_secret_mode(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        parsed_filter: &ParsedFilter,
    ) -> Result<Page<CredentialListItem>, DomainError> {
        if query.limit.is_some() || query.cursor.is_some() {
            return Err(DomainError::InvalidRequest {
                field: if query.limit.is_some() {
                    "limit"
                } else {
                    "cursor"
                },
                reason: list_filter::reasons::SECRET_MODE_NO_PAGINATION,
                detail: "secret mode does not paginate; remove limit/cursor".to_owned(),
            });
        }
        if !query.order.is_empty() {
            return Err(DomainError::InvalidRequest {
                field: "$orderby",
                reason: list_filter::reasons::SECRET_MODE_NO_ORDER,
                detail: "secret mode has no $orderby".to_owned(),
            });
        }
        parsed_filter.require_secret_mode_selector()?;

        let req = TenantId(ctx.subject_tenant_id());
        let subject = OwnerId(ctx.subject_id());
        let chain = self.dir.ancestor_chain(ctx, req).await?;

        // Secret mode always requires `read_secret`; a record-only field
        // named alongside `secret` additionally requires `list` (ADR-0004
        // Amendment A) — disclosing `sharing`/`inheritance`/... is `list`'s
        // privilege, not `read_secret`'s, exactly as the point read's
        // `get_item` splits the two.
        let mut required_actions = vec![actions::READ_SECRET];
        if list_filter::admin_field_selected(query.selected_fields()) {
            required_actions.push(actions::LIST);
        }

        // Before step 1: the PDP-permitted type set becomes step 1's SQL
        // clamp (ADR-0005, ADR-0010).
        let allowed_types = self
            .permitted_types(
                ctx,
                req,
                subject,
                &chain,
                parsed_filter.type_uuid_in.as_deref(),
                &required_actions,
            )
            .await?;
        let cap = self.list.secret_mode_cap;
        if allowed_types.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                page_info: PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: cap,
                },
            });
        }
        let allowed_type_uuids: Vec<Uuid> = allowed_types.keys().copied().collect();

        let cap_plus_one = cap
            .checked_add(1)
            .ok_or_else(|| DomainError::internal("secret_mode_cap + 1 overflowed u64"))?;
        // The type clamp here is the PDP-permitted set computed above (the
        // caller's own `$filter type in (…)` is already folded into it), so
        // the cap below counts only references the caller may see.
        let refs = self
            .repo
            .list_candidate_references(
                req,
                subject,
                &chain,
                parsed_filter.reference_in.as_deref(),
                Some(&allowed_type_uuids),
                None,
                false,
                cap_plus_one,
            )
            .await?;

        if refs.len() as u64 > cap {
            return Err(DomainError::InvalidRequest {
                field: "$filter",
                reason: list_filter::reasons::TOO_MANY_MATCHES,
                detail: format!("selector matches more than {cap} references; narrow it"),
            });
        }

        let items = self
            .reduce_and_authorize(
                ctx,
                req,
                subject,
                &chain,
                &refs,
                parsed_filter,
                &allowed_types,
                true,
            )
            .await?;

        Ok(Page {
            items,
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: cap,
            },
        })
    }

    /// The PDP-permitted type set (ADR-0005, ADR-0010) — computed **before**
    /// step 1 so its result becomes step 1's own `type_uuid_in` SQL clamp,
    /// rather than an in-memory filter applied after unpermitted rows have
    /// already reached the process. Distinct `secret_type_uuid`s visible to
    /// the caller across `chain` (`repo.list_visible_types`, already clamped
    /// by `caller_type_in` when the caller's `$filter` named types), each
    /// resolved and evaluated against the PDP for every action in
    /// `required_actions` (all must permit and include the caller's tenant)
    /// — metadata mode names `[list]`; secret mode names `[read_secret]`,
    /// plus `list` too when a record-only field is selected alongside
    /// `secret` (ADR-0004 Amendment A). `AccessDenied` and a scope that
    /// excludes the caller's tenant (counted via `cross_tenant_denied`) both
    /// simply exclude the type from the returned map; any other PDP error
    /// propagates.
    async fn permitted_types(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        caller_type_in: Option<&[Uuid]>,
        required_actions: &[&str],
    ) -> Result<HashMap<Uuid, ResolvedSecretType>, DomainError> {
        let type_uuids = self
            .repo
            .list_visible_types(req, subject, chain, caller_type_in)
            .await?;

        let mut allowed_types: HashMap<Uuid, ResolvedSecretType> = HashMap::new();
        for type_uuid in type_uuids {
            let resolved = self.resolve_stored(type_uuid).await?;
            let mut denied = false;
            for action in required_actions {
                let scope = match self
                    .scope_for_timed(
                        ctx,
                        &authz::credential_type_resource(&resolved.gts_id),
                        action,
                    )
                    .await
                {
                    Ok(scope) => scope,
                    Err(DomainError::AccessDenied { .. }) => {
                        denied = true;
                        break;
                    }
                    Err(e) => return Err(e),
                };
                if !self.repo.scope_includes_tenant(&scope, req.0).await? {
                    self.metrics.cross_tenant_denied();
                    denied = true;
                    break;
                }
            }
            if !denied {
                allowed_types.insert(type_uuid, resolved);
            }
        }
        Ok(allowed_types)
    }

    /// Shared tail of both modes (ADR-0005 steps 6-9): fetch `references`'
    /// rows whole (unclamped by type — see the comment at the call site
    /// below), reduce each to one item, drop what `allowed_types` (computed
    /// by [`Self::permitted_types`] before step 1) does not cover, apply the
    /// in-memory filters, and — in secret mode — read each winner's value.
    #[allow(
        clippy::too_many_arguments,
        reason = "every input the shared reduction+authorization tail needs; splitting it into \
                  a struct would only move the same eight names one level down"
    )]
    async fn reduce_and_authorize(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        references: &[String],
        parsed_filter: &ParsedFilter,
        allowed_types: &HashMap<Uuid, ResolvedSecretType>,
        secret_mode: bool,
    ) -> Result<Vec<CredentialListItem>, DomainError> {
        if references.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: every visible row of `references`, whole and unclamped by
        // type. The override-type-consistency invariant is enforced only
        // upward on write (`TYPE_MISMATCH_WITH_INHERITED` checks the
        // inherited row an ancestor can see; an ancestor writing after a
        // descendant cannot see the descendant's row to check against), so
        // reduction here must see every row a point read would see — a type
        // clamp at this step could make the list show, or serve the value
        // of, a reference the point read never resolves to.
        let rows = self
            .repo
            .list_candidates_for_references(req, subject, chain, references)
            .await?;
        let mut by_reference: HashMap<String, Vec<SecretRow>> = HashMap::new();
        for row in rows {
            by_reference
                .entry(row.reference.clone())
                .or_default()
                .push(row);
        }

        let plugin: Option<Arc<dyn CredStorePluginClientV1>> = if secret_mode {
            Some(self.plugins.resolve().await?)
        } else {
            None
        };

        // Phase (a): the synchronous pass. Builds, per reference in order,
        // the `Credential` metadata mode already needs plus — in secret
        // mode — the `SecretReadJob` phase (b) reads from; skips exactly the
        // cases it skips today (structurally-unreachable step-1/step-2
        // mismatch, the override-type-consistency invariant, a
        // post-reduction filter miss, a `declared`/value-less winner).
        let mut items = Vec::with_capacity(references.len());
        let mut jobs: Vec<SecretReadJob> = Vec::new();
        for reference in references {
            let Some(group) = by_reference.get(reference) else {
                // Step 1 selected this reference because a row matching its
                // predicate existed; step 2 uses the same predicate over
                // the same reference, so this is structurally unreachable.
                continue;
            };
            let Some(reduced) = reduce::reduce_reference(group, req, subject, chain) else {
                continue;
            };

            let effective_type = reduced.effective.secret_type_uuid;
            let Some(resolved) = allowed_types.get(&effective_type) else {
                // Step 1 admitted this reference only because a row of a
                // permitted type existed for it; the winner's type is not in
                // `allowed_types` regardless, so this is always the
                // override-type-consistency invariant being violated
                // (ADR-0005 §"Filter in SQL first…"), never an ordinary PDP
                // denial — step 1's clamp already excluded every denied
                // type before this reference was even fetched.
                self.metrics.list_type_invariant_violation();
                continue;
            };

            if !parsed_filter.matches_post_reduction(
                reduced.effective.sharing,
                reduced.own.map(|o| o.fallback),
                reduced.effective.expires_at,
            ) {
                continue;
            }

            let key = SecretRef::new(reference.clone()).map_err(|e| {
                DomainError::internal(format!("stored reference failed SecretRef validation: {e}"))
            })?;
            let credential = build_credential(&key, resolved.gts_id.clone(), &reduced);

            if !secret_mode {
                items.push(CredentialListItem {
                    credential,
                    secret: None,
                });
                continue;
            }

            let Some(winner) = reduced.winner else {
                continue;
            };
            if winner.value_id.is_none() {
                continue;
            }
            jobs.push(SecretReadJob {
                credential,
                key,
                winner: winner.clone(),
                gts_id: resolved.gts_id.clone(),
            });
        }

        if !secret_mode || jobs.is_empty() {
            return Ok(items);
        }

        // Phase (b): the winners' value reads, fanned out with bounded
        // parallelism (DESIGN §4.6) instead of one at a time. `self`,
        // `plugin`, `ctx`, and `chain` are all shared references, so
        // `buffer_unordered` inside this async fn — never `tokio::spawn` —
        // is enough; the futures never need to outlive this call.
        //
        // Set whenever `secret_mode` is true, which the `!secret_mode`
        // early return above already ruled out.
        let Some(plugin) = plugin.as_ref() else {
            return Err(DomainError::internal(
                "secret mode reached the read step without a resolved plugin",
            ));
        };

        // Each job is moved into its future by value: borrowing it from `jobs`
        // trips rustc's "implementation of `FnOnce` is not general enough" once
        // `Service::list` is driven through a generic handler.
        let job_count = jobs.len();
        let mut reads = stream::iter(jobs.into_iter().enumerate().map(|(index, job)| async move {
            let outcome = self
                .read_value_for_row(
                    plugin,
                    ctx,
                    req,
                    subject,
                    &job.key,
                    chain,
                    &job.winner,
                    &job.gts_id,
                )
                .await;
            (index, job.credential, outcome)
        }))
        .buffer_unordered(SECRET_READ_CONCURRENCY);

        // Item order in the response must match reference order regardless
        // of completion order, so results land by index rather than being
        // pushed as they arrive.
        let mut read_results: Vec<Option<(Credential, Option<Secret>)>> =
            (0..job_count).map(|_| None).collect();
        let mut first_err: Option<DomainError> = None;
        while let Some((index, credential, outcome)) = reads.next().await {
            match outcome {
                Ok(secret) => read_results[index] = Some((credential, secret)),
                Err(err) => {
                    // One backend failure fails the whole request: stop
                    // starting new reads (dropping `reads` below never polls
                    // its still-buffered futures again) without waiting for
                    // whatever is already in flight.
                    first_err = Some(err);
                    break;
                }
            }
        }
        drop(reads);
        if let Some(err) = first_err {
            return Err(err);
        }

        for result in read_results {
            // `None` only if the loop above exited without an error before
            // visiting every index, which cannot happen: the only early
            // exit is the `Err` branch, which returns above.
            let Some((credential, secret)) = result else {
                continue;
            };
            let Some(secret) = secret else {
                // A refused/missing/fingerprint-mismatched value is omitted,
                // not reported (ADR-0004 "Bulk secret read: the collection in
                // secret mode") — `read_value_for_row` already recorded the
                // relevant metric.
                continue;
            };
            items.push(CredentialListItem {
                credential,
                secret: Some(secret.secret),
            });
        }

        Ok(items)
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
