//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-authoring:p1
//! A revision's quota rules below their endpoints (D-540, D-541, D-542, D-543): the read of the
//! stored set and the replacement of the whole set. The SDK's local client calls the same
//! functions, so both surfaces answer alike.
//!
//! - **The read.** The stored set is read with its revision in one serializable read
//!   transaction, so the set row and the rules are one snapshot (D-546). A read never computes a
//!   digest and never asks Products.
//! - **The replacement** ([`replace`]) follows D-543's order: the key's stored answer first (the
//!   caller is already authorized), then the revision (404, `REVISION_NOT_DRAFT`,
//!   `NOT_DRAFT_AUTHOR`), the parse, and the validation outside any transaction: each rule
//!   item's SKU version on the sale date and Products' `validate_quota_set`, both on a task of
//!   their own (D-542), with Pricing's own checks. Every known failure but availability refuses
//!   at once, in one refusal. Then one transaction claims the key, requires the unlocked draft at
//!   the `If-Match` version (else `STALE_REVISION`) and the items the validation saw (else it rolls
//!   back and the whole capture runs again, within the transaction retry budget), and writes the
//!   rules, the set row, the next version, the audit row and the key's answer.
use super::{
    AuthoringState,
    dto::PricingPlanQuotaRulesDto,
    plans,
    support::{self, Answer, DoorError},
};
use crate::api::rest::preconditions;
use crate::domain::plan;
use crate::domain::plan_quota::{
    RuleItem, SAVE_IGNORES, Violation, catalog_violations, duplicate_violations, item_violations,
    parse_rules, refusal_status, resolved_metrics, unjudged, version_violations,
};
use crate::infra::error_mapping::DomainError;
use crate::infra::plan_quota;
use crate::infra::pricing_reads::PlanResource;
use crate::infra::storage::{
    entity::{plan_item, plan_quota_rule, plan_revision},
    repo::{plan_item_repo, plan_quota_repo, plan_revision_repo, price_book_entry_repo},
};
use crate::infra::usage_policy_wire::digest_text;
use axum::{http::StatusCode, response::Response};
use bss_pricing_sdk::digest::quota_rules_digest;
use bss_pricing_sdk::plan_quota::PlanQuotaRuleV1;
use bss_products_sdk::quota::validation::QuotaSetValidation;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use toolkit_canonical_errors::{CanonicalError, Http};
use toolkit_db::{DbTx, secure::AccessScope};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The stored set of a revision the caller's plan read admits (D-540): its rules ordered by
/// `rule_id`, their stored digest, and the revision's version, which is the `ETag`.
/// # Errors
/// 404 for a revision the tenant does not hold or the scope does not admit; 500 for a stored rule
/// that does not convert.
// @cpt-flow:cpt-cf-bss-pricing-flow-plan-quota-rules:p1
pub async fn rules(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    id: Uuid,
) -> Result<(PricingPlanQuotaRulesDto, u64), CanonicalError> {
    // @cpt-begin:cpt-cf-bss-pricing-flow-plan-quota-rules:p1:inst-quota-read
    let tenant = ctx.subject_tenant_id();
    let read = support::transaction_read(&state.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move { stored(tx, &scope, tenant, id).await })
    })
    .await;
    // @cpt-end:cpt-cf-bss-pricing-flow-plan-quota-rules:p1:inst-quota-read
    read
}

/// The revision and its stored set, in the caller's read transaction.
async fn stored(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(PricingPlanQuotaRulesDto, u64), DoorError> {
    let revision = plans::find_revision(tx, scope, tenant, id).await?;
    let set =
        plan_quota_repo::for_revision(tx, &AccessScope::for_tenant(tenant), tenant, id).await?;
    let body = PricingPlanQuotaRulesDto::of(id, revision.version, &set)?;
    Ok((body, plans::etag(revision.version)?))
}

/// `GET /plan-revisions/{id}/quota-rules` below its route: [`rules`], answered 200 with the
/// revision's version as the `ETag`.
/// # Errors
/// As [`rules`].
pub(super) async fn get(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    id: Uuid,
) -> Result<Response, CanonicalError> {
    let (body, version) = rules(state, scope, ctx, id).await?;
    support::response(StatusCode::OK, &body, Some(version))
}

/// The endpoint a replacement's key is stored under, whichever surface sent it.
fn endpoint(id: Uuid) -> String {
    format!("/bss-pricing/v1/plan-revisions/{id}/quota-rules")
}

/// A `PUT` body as JSON, read as it was sent: an empty or unreadable body is 400. Every other
/// refusal of its content (a U+0000 included) is the parse's, with its rule and member (D-541).
/// # Errors
/// 400 for an empty or unreadable body.
pub(super) fn body(bytes: &[u8]) -> Result<Value, CanonicalError> {
    if bytes.is_empty() {
        return Err(DomainError::InvalidRequest(
            "the request body is empty; send {\"quota_rules\": [...]}, [] to clear the rules"
                .to_owned(),
        )
        .into());
    }
    serde_json::from_slice(bytes).map_err(|e| {
        DomainError::InvalidRequest(format!("the request body is not readable: {e}")).into()
    })
}

/// What a replacement is asked: the revision, the version its caller read (`If-Match`), the
/// idempotency key and the body.
#[derive(Debug, Clone)]
pub struct Replace {
    pub id: Uuid,
    pub version: u64,
    pub key: String,
    pub body: Value,
}

/// `PUT /plan-revisions/{id}/quota-rules` below its route: [`replace`], answered as recorded.
/// # Errors
/// As [`replace`].
pub(super) async fn put(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    request: Replace,
) -> Result<Response, CanonicalError> {
    replace(state, scope, ctx, correlation, request)
        .await?
        .response()
}

/// Replace a draft's whole rule set (D-543), for a caller already authorized as `author` on
/// `plan` (`scope`). The answer is the key's stored one, or the one this call recorded: 200 with
/// the saved set and the revision's new version as its `ETag`.
/// # Errors
/// 409 `IDEMPOTENCY_CONFLICT` or `IDEMPOTENCY_KEY_IN_FLIGHT` for the key; 404 for a revision the
/// tenant does not hold; 409 `REVISION_NOT_DRAFT`; 403 `NOT_DRAFT_AUTHOR`; one refusal naming
/// every failure as `quota_rules[i].<field>` with its code, 409 when each is a conflict code
/// (`QUOTA_RULE_DUPLICATE`, `QUOTA_DEFINITION_CHANGED`) and 400 otherwise, either an
/// invalid-argument problem whose field violations carry the codes, with no `reason`; Products'
/// 400, 403 or 500 as Products gave it; 503 `QUOTA_CATALOG_UNAVAILABLE` or
/// `REGISTRY_UNAVAILABLE`; 409 `STALE_REVISION` in the transaction; 409 `UNIT_CONTENDED` when the
/// items keep moving; 409 `CONTENDED` when the read or the write transaction stays contended
/// past its own retries (each runs with the toolkit's retries inside the capture's).
// @cpt-algo:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1
pub async fn replace(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    request: Replace,
) -> Result<Answer, CanonicalError> {
    let tenant = ctx.subject_tenant_id();
    let endpoint = endpoint(request.id);
    // The body is parsed once: its rules bind the key, and every capture judges the same parse.
    let parsed = parse_rules(&request.body);
    let digest = request_digest(&request, &parsed)?;
    let db = state.db.db();
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-replay
    if let Some(answer) =
        support::replay_answer(&db, tenant, &endpoint, &request.key, &digest).await?
    {
        return Ok(answer);
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-replay
    let key = Keyed {
        endpoint,
        key: request.key.clone(),
        digest,
        correlation,
    };
    // @cpt-begin:cpt-cf-bss-pricing-flow-plan-quota-rules:p1:inst-quota-replace
    let answer =
        support::retry_unit_capture(&db, || attempt(state, &scope, ctx, &request, &parsed, &key))
            .await
            .map_err(CanonicalError::from);
    // @cpt-end:cpt-cf-bss-pricing-flow-plan-quota-rules:p1:inst-quota-replace
    answer
}

/// A body's parse: its rules, or every violation found (`parse_rules`).
type Parsed = Result<Vec<PlanQuotaRuleV1>, Vec<Violation>>;

/// The content a replacement's key is bound to: the `If-Match` version and the rules as they parse,
/// by their set digest (D-541), so one key replays across REST and the SDK whatever the body's
/// spelling (member order, an absent `dimension_value`, UUID case, the rules' order). A body that
/// does not parse is bound by its JSON: it is refused, and never recorded.
fn request_digest(request: &Replace, parsed: &Parsed) -> Result<Vec<u8>, CanonicalError> {
    let content = match parsed {
        Ok(rules) => json!({
            "if_match": request.version,
            "quota_rules_digest": digest_text(quota_rules_digest(rules)),
        }),
        Err(_) => json!({
            "if_match": request.version,
            "body": request.body,
        }),
    };
    Ok(preconditions::request_digest(&content)?)
}

/// The key a replacement claims and records its answer under.
#[derive(Debug, Clone)]
struct Keyed {
    endpoint: String,
    key: String,
    digest: Vec<u8>,
    correlation: Uuid,
}

/// The revision and its items as the validation judged them.
#[derive(Debug, Clone)]
struct Captured {
    revision: plan_revision::Model,
    items: Vec<plan_item::Model>,
    rule_items: Vec<RuleItem>,
    registry: Vec<(String, Vec<String>)>,
}

/// One capture and its transaction: the revision, the refusal of a body that did not parse, the
/// validation outside any transaction, then the write.
async fn attempt(
    state: &AuthoringState,
    scope: &AccessScope,
    ctx: &SecurityContext,
    request: &Replace,
    parsed: &Parsed,
    key: &Keyed,
) -> Result<Answer, DoorError> {
    let (read_scope, read_ctx, id) = (scope.clone(), ctx.clone(), request.id);
    let captured = support::transaction_door_read(&state.db.db(), move |tx| {
        let (scope, ctx) = (read_scope.clone(), read_ctx.clone());
        Box::pin(async move { capture(tx, &scope, &ctx, id).await })
    })
    .await?;
    let rules = parsed
        .as_ref()
        .map_err(|violations| DoorError::Api(refusal(violations)))?;
    let rows = validate(state, ctx, &captured, rules).await?;
    let write = Write {
        ctx: ctx.clone(),
        id: request.id,
        version: request.version,
        key: key.clone(),
        available_from: captured.revision.available_from,
        items: identity(&captured.items),
        digest: digest_text(quota_rules_digest(rules)),
        rows,
    };
    support::transaction_door(&state.db.db(), move |tx| {
        let write = write.clone();
        Box::pin(async move { commit(tx, write).await })
    })
    .await
}

/// The revision the caller may edit, its items, their entries' dimension keys and the dimension
/// registry, in one read transaction.
async fn capture(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    ctx: &SecurityContext,
    id: Uuid,
) -> Result<Captured, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let children = AccessScope::for_tenant(tenant);
    let revision = plans::find_revision(tx, scope, tenant, id).await?;
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-authorize
    plans::editable(&revision, ctx)?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-authorize
    let items = plan_item_repo::for_revision(tx, &children, tenant, id).await?;
    let mut entry_ids: Vec<Uuid> = items.iter().filter_map(|i| i.price_book_entry_id).collect();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    let keys: BTreeMap<Uuid, Option<String>> =
        price_book_entry_repo::find_many(tx, &children, tenant, &entry_ids)
            .await?
            .into_iter()
            .map(|entry| (entry.id, entry.dimension_key))
            .collect();
    let rule_items = items
        .iter()
        .map(|item| RuleItem {
            id: item.id,
            sku_id: item.sku_id,
            dimension_key: item
                .price_book_entry_id
                .and_then(|entry| keys.get(&entry).cloned().flatten()),
        })
        .collect();
    let registry = plans::dimension_registry(tx, tenant).await?;
    Ok(Captured {
        revision,
        items,
        rule_items,
        registry,
    })
}

/// Each item's SKU and entry: what the transaction compares with the capture.
fn identity(items: &[plan_item::Model]) -> BTreeMap<Uuid, (Uuid, Option<Uuid>)> {
    items
        .iter()
        .map(|item| (item.id, (item.sku_id, item.price_book_entry_id)))
        .collect()
}

/// The rows of a set that passed every check (D-542), or the refusal of every known failure but
/// availability. An empty set reads no SKU version and asks no catalog. Pricing's own checks
/// judge the items, the dimension values and each item's SKU version on the sale date; a rule
/// Pricing refused is not sent to Products, and Products judges the rest. A rule Products was
/// sent but did not judge against the definition it names is its contract break: a 500, logged
/// with the rule and the revision, before any refusal (D-543).
async fn validate(
    state: &AuthoringState,
    ctx: &SecurityContext,
    captured: &Captured,
    rules: &[PlanQuotaRuleV1],
) -> Result<Vec<plan_quota_rule::Model>, DoorError> {
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-validate
    let items = &captured.rule_items;
    let mut violations = item_violations(rules, items, &captured.registry);
    let skus: Vec<Uuid> = rules
        .iter()
        .filter_map(|rule| items.iter().find(|item| item.id == rule.item_id))
        .map(|item| item.sku_id)
        .collect();
    let sale = plan::sale_date_of(captured.revision.available_from, plans::today());
    let versions = plan_quota::sku_versions(&state.hub, ctx, &skus, sale).await?;
    violations.extend(version_violations(rules, items, &versions));
    let request = plan_quota::set_request(rules, items, &versions, plan_quota::Purpose::Authoring);
    let sent: Vec<Uuid> = request.rules.iter().map(|rule| rule.rule_id).collect();
    let verdict = if request.rules.is_empty() {
        QuotaSetValidation {
            rules: Vec::new(),
            checks: Vec::new(),
        }
    } else {
        plan_quota::validate(&state.hub, ctx, request)
            .await
            .inspect_err(|error| {
                if error.status_code() == 400 {
                    tracing::error!(
                        error = %error,
                        "pricing: Products refused a quota set that passed Pricing's caps"
                    );
                }
            })?
    };
    let broken = unjudged(
        rules.iter().filter(|rule| sent.contains(&rule.rule_id)),
        &verdict,
    );
    if !broken.is_empty() {
        for (rule_id, why) in &broken {
            tracing::error!(
                revision_id = %captured.revision.id,
                rule_id = %rule_id,
                why,
                "pricing: Products' quota verdict does not judge a rule it was sent"
            );
        }
        return Err(DoorError::Api(
            CanonicalError::internal("Products' quota verdict does not judge every rule").create(),
        ));
    }
    violations.extend(duplicate_violations(
        rules,
        &resolved_metrics(rules, &verdict),
    ));
    violations.extend(catalog_violations(rules, &verdict));
    violations.retain(|violation| violation.code != SAVE_IGNORES);
    if !violations.is_empty() {
        return Err(refusal(&violations).into());
    }
    let now = crate::infra::storage::stored_now();
    let tenant = ctx.subject_tenant_id();
    let rows = rules
        .iter()
        .map(|rule| {
            let definition = verdict
                .rules
                .iter()
                .find(|judged| judged.rule_id == rule.rule_id)
                .and_then(|judged| judged.definition.as_ref())
                .ok_or_else(|| {
                    tracing::error!(
                        revision_id = %captured.revision.id,
                        rule_id = %rule.rule_id,
                        "pricing: Products answered no definition for a rule it found no fault in"
                    );
                    CanonicalError::internal("a quota rule has no definition from Products")
                        .create()
                })?;
            plan_quota::row_of(tenant, captured.revision.id, rule, definition, now)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-validate
    Ok(rows)
}

/// One refusal of every violation (D-543): a field violation `quota_rules[i].<field>` with its
/// code each, in the order they were found, on the `plan` resource. It is 409 when every
/// violation is a conflict code, else 400. Both are an invalid-argument problem with no `reason`,
/// the 409 by the HTTP override: an aborted problem carries no field violations, and the codes
/// are in them.
fn refusal(violations: &[Violation]) -> CanonicalError {
    let status = refusal_status(violations);
    let Some((first, rest)) = violations.split_first() else {
        return CanonicalError::internal("a quota refusal names no failure").create();
    };
    let builder = PlanResource::invalid_argument().with_field_violation(
        first.path(),
        first.detail.clone(),
        first.code,
    );
    let builder = rest.iter().fold(builder, |builder, violation| {
        builder.with_field_violation(violation.path(), violation.detail.clone(), violation.code)
    });
    if status == 409 {
        builder.with_override(Http::status_code(409)).create()
    } else {
        builder.create()
    }
}

/// What one replacement's transaction writes, captured outside it.
#[derive(Debug, Clone)]
struct Write {
    ctx: SecurityContext,
    id: Uuid,
    version: u64,
    key: Keyed,
    available_from: Option<time::Date>,
    items: BTreeMap<Uuid, (Uuid, Option<Uuid>)>,
    digest: String,
    rows: Vec<plan_quota_rule::Model>,
}

/// The transaction: the key, the fence, and the rules, the set row, the next version, the audit
/// row and the key's answer together (D-543).
async fn commit(tx: &DbTx<'_>, write: Write) -> Result<Answer, DoorError> {
    let Write {
        ctx,
        id,
        version,
        key,
        available_from,
        items,
        digest,
        mut rows,
    } = write;
    let tenant = ctx.subject_tenant_id();
    let children = AccessScope::for_tenant(tenant);
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-fence
    if let Some(answer) =
        support::claim_answer(tx, tenant, &key.endpoint, &key.key, &key.digest).await?
    {
        return Ok(answer);
    }
    let Some(revision) = plan_revision_repo::find(tx, &children, tenant, id).await? else {
        return Err(DoorError::SelectionMoved);
    };
    if !plans::open_draft(&revision) || u64::try_from(revision.version).ok() != Some(version) {
        return Err(support::conflict("STALE_REVISION").into());
    }
    let current = plan_item_repo::for_revision(tx, &children, tenant, id).await?;
    let moved = revision.available_from != available_from
        || identity(&current) != items
        || rows
            .iter()
            .any(|row| !current.iter().any(|item| item.id == row.item_id));
    if moved {
        return Err(DoorError::SelectionMoved);
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-fence
    let seen = revision.version;
    let now = crate::infra::storage::stored_now();
    rows.sort_by_key(|row| row.rule_id);
    let next = plan_quota_repo::replace(
        tx,
        &children,
        tenant,
        id,
        seen,
        rows.clone(),
        digest.clone(),
        ctx.subject_id(),
        now,
    )
    .await?;
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-commit
    support::audit(
        tx,
        &ctx,
        key.correlation,
        "plan_revision.quota_rules",
        id,
        next,
    )
    .await?;
    let saved = plan_quota_repo::StoredSet {
        digest,
        rules: rows,
    };
    let body = PricingPlanQuotaRulesDto::of(id, next, &saved)?;
    let answer = support::record(
        tx,
        tenant,
        &key.endpoint,
        &key.key,
        StatusCode::OK,
        &body,
        Some(plans::etag(next)?),
    )
    .await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-commit
    Ok(answer)
}
