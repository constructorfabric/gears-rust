//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-authoring:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance:p1
//! The quota catalog port (D-542, D-545): the two calls Pricing makes to Products for quota rules,
//! the conversion of a stored rule, and the quota half of a detached capture.
//!
//! - **Two calls.** Each rule item's SKU version on the revision's sale date
//!   ([`sku_versions`], `ReferenceRegistryV1::sku_version_as_of`, one read per distinct SKU) and
//!   `ProductsQuotaCatalogClientV1::validate_quota_set` ([`validate`]), whose answer carries each
//!   resolved definition. Both run as the caller and on a task of their own, outside any
//!   transaction: Products' in-process clients open a connection to Products' database, which
//!   toolkit-db refuses on a task inside a transaction, and an external call never runs inside a
//!   local transaction (D-503's capture).
//! - **Outage.** A catalog that is not registered, that does not finish, or that answers a rate
//!   limit (429), a contention conflict (409 `CONTENDED` or `UNIT_CONTENDED`) or a 5xx other than
//!   500 is 503 `QUOTA_CATALOG_UNAVAILABLE`; any other Products refusal (a 400, a 403) and a 500
//!   are forwarded as Products gave them.
//! - **The stored rule.** [`row_of`] and [`stored_rule`] are the one conversion between a stored
//!   row and the SDK's [`PlanQuotaRuleV1`] with its evidence and availability observation; a
//!   stored value that does not convert is a corrupt row.
//! - **The capture.** [`QuotaObservation`] reads a revision's set and items in one read
//!   transaction, then validates the set outside it; the unit's transaction re-checks the stored
//!   state it was judged against ([`QuotaObservation::check_local`]). Every capture keeps its set
//!   digest, the empty set's included, so a set added since is seen.
use crate::api::rest::authoring::support::{self, DoorError};
use crate::domain::plan_quota::{RuleItem, digest_from_hex, names_definition, rule_violations};
use crate::infra::storage::repo::plan_quota_repo::StoredSet;
use crate::infra::{
    reference_registry, reference_work,
    storage::{
        RepoError,
        entity::{plan_item, plan_quota_rule as rule_e, plan_revision},
        repo::{plan_item_repo, plan_quota_repo, plan_revision_repo},
    },
    usage_policy_wire::digest_text,
};
use bss_pricing_sdk::plan_quota::{
    PlanQuotaBasis, PlanQuotaKind, PlanQuotaRuleV1, QuotaDefinitionRef, QuotaSubjectSelector,
};
use bss_pricing_sdk::read::{PinnedQuotaEvidence, QuotaOwnerRef, QuotaTerms, ResolvedQuotaRule};
use bss_products_sdk::ProductsQuotaCatalogClientV1;
use bss_products_sdk::quota::catalog::{
    QuotaAvailability, QuotaBasis, QuotaDefinition, QuotaDefinitionListRequest,
    QuotaDefinitionPage, QuotaDefinitionRef as ProductsDefinitionRef,
    QuotaOwnerRef as ProductsOwnerRef, QuotaVersion,
};
use bss_products_sdk::quota::gts::QuotaKind;
use bss_products_sdk::quota::units::{Amount, Factor};
use bss_products_sdk::quota::validation::{
    QuotaChangeKind, QuotaSetRequest, QuotaSetRule, QuotaSetValidation, QuotaValidationPurpose,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{Db, DbTx, secure::AccessScope};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The detail of every catalog outage.
const UNAVAILABLE: &str = "QUOTA_CATALOG_UNAVAILABLE: Products quota catalog is unavailable";

/// 503 `QUOTA_CATALOG_UNAVAILABLE`: what every catalog outage answers, at the quota rules' `PUT`,
/// the checks, submit and approve alike.
#[must_use]
pub fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(UNAVAILABLE)
        .create()
}

/// Whether `error` is a catalog outage ([`unavailable`]), not another dependency's 503 or a
/// refusal.
#[must_use]
pub fn is_unavailable(error: &CanonicalError) -> bool {
    error.status_code() == 503 && error.detail() == UNAVAILABLE
}

/// Products' answers that settle the call are forwarded as Products gave them: a 500 (a stored
/// row that does not read) and a definite refusal, a 4xx other than a rate limit or a contention
/// conflict (`reference_work::definite_refusal`). Every other answer (a 429, a 409 `CONTENDED` or
/// `UNIT_CONTENDED`, a 5xx other than 500) is an outage: 503 `QUOTA_CATALOG_UNAVAILABLE`, its own
/// status and diagnostic logged.
fn outage(error: CanonicalError) -> CanonicalError {
    let status = error.status_code();
    if status == 500 || reference_work::definite_refusal(&error) {
        return error;
    }
    tracing::warn!(
        status,
        error = %error,
        diagnostic = error.diagnostic().unwrap_or_default(),
        "pricing: the Products quota catalog did not answer"
    );
    unavailable()
}

/// A catalog call that did not finish (it panicked or was cancelled) is an outage.
fn unfinished(error: &tokio::task::JoinError) -> CanonicalError {
    tracing::warn!(%error, "pricing: a Products quota catalog call did not finish");
    unavailable()
}

/// The quota catalog, resolved per call (D-542): Products registers it at its init, possibly after
/// this gear's. Every call runs on a task of its own, as the reference registry's reads do.
/// # Errors
/// 503 `QUOTA_CATALOG_UNAVAILABLE` when Products has not registered it.
pub fn catalog(
    hub: &toolkit::ClientHub,
) -> Result<Arc<dyn ProductsQuotaCatalogClientV1>, CanonicalError> {
    hub.get::<dyn ProductsQuotaCatalogClientV1>()
        .map(|inner| Arc::new(Detached(inner)) as Arc<dyn ProductsQuotaCatalogClientV1>)
        .map_err(|_| unavailable())
}

/// The catalog with every call on a task of its own (see the module docs).
struct Detached(Arc<dyn ProductsQuotaCatalogClientV1>);

#[async_trait::async_trait]
impl ProductsQuotaCatalogClientV1 for Detached {
    async fn list_quota_definitions(
        &self,
        ctx: &SecurityContext,
        request: QuotaDefinitionListRequest,
    ) -> Result<QuotaDefinitionPage, CanonicalError> {
        let (inner, ctx) = (self.0.clone(), ctx.clone());
        tokio::spawn(async move { inner.list_quota_definitions(&ctx, request).await })
            .await
            .map_err(|e| unfinished(&e))?
    }

    async fn get_quota_definition(
        &self,
        ctx: &SecurityContext,
        definition_id: Uuid,
        version: QuotaVersion,
        locale: Option<&str>,
    ) -> Result<QuotaDefinition, CanonicalError> {
        let (inner, ctx, locale) = (self.0.clone(), ctx.clone(), locale.map(str::to_owned));
        tokio::spawn(async move {
            inner
                .get_quota_definition(&ctx, definition_id, version, locale.as_deref())
                .await
        })
        .await
        .map_err(|e| unfinished(&e))?
    }

    async fn validate_quota_set(
        &self,
        ctx: &SecurityContext,
        request: QuotaSetRequest,
    ) -> Result<QuotaSetValidation, CanonicalError> {
        let (inner, ctx) = (self.0.clone(), ctx.clone());
        tokio::spawn(async move { inner.validate_quota_set(&ctx, request).await })
            .await
            .map_err(|e| unfinished(&e))?
    }
}

/// Each SKU's version in force on `on` (D-542), one registry read per distinct SKU, as the
/// caller; a SKU with no version on that day, or one Products no longer knows (404), is absent.
/// No SKU reads nothing.
/// # Errors
/// The registry's definite refusal as Products gave it; 503 `REGISTRY_UNAVAILABLE`.
pub async fn sku_versions(
    hub: &toolkit::ClientHub,
    ctx: &SecurityContext,
    skus: &[Uuid],
    on: time::Date,
) -> Result<BTreeMap<Uuid, i64>, CanonicalError> {
    let wanted: BTreeSet<Uuid> = skus.iter().copied().collect();
    let mut out = BTreeMap::new();
    if wanted.is_empty() {
        return Ok(out);
    }
    let registry =
        reference_registry::resolve(hub).map_err(|e| support::registry_unavailable(&e))?;
    for sku in wanted {
        match registry
            .sku_version_as_of(ctx, ctx.subject_tenant_id(), sku, on)
            .await
        {
            Ok(Some(version)) => {
                out.insert(sku, version.published_version);
            }
            Ok(None) => {}
            Err(error) if error.status_code() == 404 => {}
            Err(error) if reference_work::definite_refusal(&error) => return Err(error),
            Err(error) => return Err(support::registry_unavailable(&error)),
        }
    }
    Ok(out)
}

/// Pricing's basis as Products spells it.
const fn products_basis(basis: PlanQuotaBasis) -> QuotaBasis {
    match basis {
        PlanQuotaBasis::Fixed => QuotaBasis::Fixed,
        PlanQuotaBasis::PerPlanUnit => QuotaBasis::PerPlanUnit,
    }
}

/// Pricing's definition reference as Products' (its digest in lowercase hex).
fn products_reference(reference: &QuotaDefinitionRef) -> Option<ProductsDefinitionRef> {
    Some(ProductsDefinitionRef {
        id: reference.id,
        version: QuotaVersion::new(reference.version.get()).ok()?,
        digest: digest_text(reference.digest),
    })
}

/// Why Products is asked to judge a set: a saved set and its checks send no quantity, and a sale
/// sends the quantity it buys. Products judges a quantity whenever one is given, so authoring
/// cannot send one, and an acceptance cannot send none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// The rule set an author saves, or the checks of a revision: no quantity.
    Authoring,
    /// A sale of `quantity` plan units, a canonical decimal integer.
    Acceptance { quantity: String },
}

/// The Products request for `rules` (D-542): each rule with its item's SKU and that SKU's version
/// in `versions`, a new set (`InitialSet`) with no prior limit and no locale, and the quantity of
/// an acceptance ([`Purpose`]). A rule whose item is not in `items`, or whose SKU has no version
/// from 1, is left out: Pricing's own checks report it (`item_violations`, `version_violations`).
#[must_use]
pub fn set_request(
    rules: &[PlanQuotaRuleV1],
    items: &[RuleItem],
    versions: &BTreeMap<Uuid, i64>,
    purpose: Purpose,
) -> QuotaSetRequest {
    let rules = rules
        .iter()
        .filter_map(|rule| {
            let item = items.iter().find(|item| item.id == rule.item_id)?;
            let sku_version = versions
                .get(&item.sku_id)
                .and_then(|version| u64::try_from(*version).ok())
                .and_then(|version| QuotaVersion::new(version).ok())?;
            Some(QuotaSetRule {
                rule_id: rule.rule_id,
                definition: products_reference(&rule.definition_ref)?,
                sku_id: item.sku_id,
                sku_version,
                logical_binding_key: rule.logical_binding_key.clone(),
                alternative: rule.dimension_value.clone(),
                subject_selector: rule.subject_selector.token().to_owned(),
                scope_selection: rule.scope_selection.clone(),
                amount: rule.amount.clone(),
                unit_ref: rule.unit_ref.clone(),
                basis: products_basis(rule.basis),
            })
        })
        .collect();
    let (purpose, quantity) = match purpose {
        Purpose::Authoring => (QuotaValidationPurpose::Authoring, None),
        Purpose::Acceptance { quantity } => (QuotaValidationPurpose::Acceptance, Some(quantity)),
    };
    QuotaSetRequest {
        purpose,
        change_kind: QuotaChangeKind::InitialSet,
        quantity,
        prior: Vec::new(),
        rules,
        locale: None,
    }
}

/// Validate through the catalog, as the caller, on a task of its own (D-542): a rate limit, a
/// contention conflict or a provider 5xx other than 500 is 503 `QUOTA_CATALOG_UNAVAILABLE`; any
/// other refusal (400, 403) and a 500 are forwarded.
/// # Errors
/// As above; 503 `QUOTA_CATALOG_UNAVAILABLE` when the catalog is not registered.
pub async fn validate(
    hub: &toolkit::ClientHub,
    ctx: &SecurityContext,
    request: QuotaSetRequest,
) -> Result<QuotaSetValidation, CanonicalError> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-validate
    let answer = match catalog(hub) {
        Ok(catalog) => catalog
            .validate_quota_set(ctx, request)
            .await
            .map_err(outage),
        Err(error) => Err(error),
    };
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-validate
    answer
}

fn owner_ref(reference: &ProductsOwnerRef) -> Value {
    json!({
        "id": reference.id,
        "version": reference.version.to_string(),
        "digest": reference.digest,
    })
}

const fn kind_token(kind: QuotaKind) -> &'static str {
    match kind {
        QuotaKind::Allocation => "allocation",
    }
}

/// The stored evidence of a resolved definition (D-542): its identity and digest, SKU and SKU
/// version, metric and its digest and kind, canonical unit, the rule's unit (`unit_ref`) with its
/// digest, factor, grid and maximum (`min`, `step`, `max`, the maximum null when the unit has
/// none), taken from the definition's allowed unit of that `unit_ref` (the unit is null when the
/// definition does not allow it), the mapping, profile and qualification references, both
/// policies' references and digests, change support, subject and resource types. No text and no
/// availability.
#[must_use]
pub fn evidence_json(definition: &QuotaDefinition, unit_ref: &str) -> Value {
    let unit = definition
        .allowed_units
        .iter()
        .find(|unit| unit.unit_ref == unit_ref)
        .map(|unit| {
            json!({
                "unit_ref": unit.unit_ref,
                "digest": unit.digest,
                "numerator": unit.numerator,
                "denominator": unit.denominator,
                "min": unit.min,
                "step": unit.step,
                "max": unit.max,
            })
        });
    json!({
        "definition_id": definition.definition_id,
        "version": definition.version.to_string(),
        "definition_digest": definition.definition_digest,
        "sku_id": definition.sku_id,
        "sku_version": definition.sku_version.to_string(),
        "metric_ref": definition.metric_ref,
        "metric_digest": definition.metric_digest,
        "quota_kind": kind_token(definition.quota_kind),
        "canonical_unit_ref": definition.canonical_unit_ref,
        "unit": unit,
        "mapping_ref": owner_ref(&definition.mapping_ref),
        "profile_ref": owner_ref(&definition.profile_ref),
        "qualification_ref": definition.qualification_ref.as_ref().map(owner_ref),
        "accounting_policy_ref": definition.accounting_policy_ref,
        "accounting_policy_digest": definition.accounting_policy_digest,
        "scope_policy_ref": definition.scope_policy_ref,
        "scope_policy_digest": definition.scope_policy_digest,
        "change_support": {
            "initial_set": definition.change_support.initial_set,
            "increase": definition.change_support.increase,
        },
        "subject_type_ref": definition.subject_type_ref,
        "resource_type_refs": definition.resource_type_refs,
    })
}

/// The availability observation saved with a rule: `{state, revision, observed_at, reason_code}`,
/// Products' own shape.
/// # Errors
/// 500 when the observation time does not write as RFC 3339 (a year outside 0 to 9999).
pub fn availability_json(definition: &QuotaDefinition) -> Result<Value, CanonicalError> {
    serde_json::to_value(&definition.availability).map_err(|error| {
        tracing::error!(%error, "pricing: a quota availability observation does not write");
        CanonicalError::internal("a quota availability observation does not write").create()
    })
}

/// A stored rule as the SDK shapes it, with the evidence and the availability it was saved with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    /// The rule.
    pub rule: PlanQuotaRuleV1,
    /// The definition's identity-bearing content at the save ([`evidence_json`]).
    pub evidence: Value,
    /// The availability the rule saw at its save; the checks judge the current one.
    pub availability: QuotaAvailability,
}

/// The row that stores `rule` of `revision`, with the evidence and the availability of
/// `definition`, Products' verdict for it (D-542).
/// # Errors
/// 500 when `definition` is not the one the rule names, by id, version and digest
/// (`names_definition`, the comparison the checks share): a rule is never stored without its own
/// verdict. 500 when its availability does not write.
pub fn row_of(
    tenant: Uuid,
    revision: Uuid,
    rule: &PlanQuotaRuleV1,
    definition: &QuotaDefinition,
    now: time::OffsetDateTime,
) -> Result<rule_e::Model, CanonicalError> {
    if !names_definition(&rule.definition_ref, definition) {
        tracing::error!(
            revision_id = %revision,
            rule_id = %rule.rule_id,
            "pricing: a quota rule was given the verdict of another definition"
        );
        return Err(CanonicalError::internal(
            "a quota rule was given another definition's verdict",
        )
        .create());
    }
    let scope: serde_json::Map<String, Value> = rule
        .scope_selection
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    Ok(rule_e::Model {
        revision_id: revision,
        rule_id: rule.rule_id,
        tenant_id: tenant,
        item_id: rule.item_id,
        definition_id: rule.definition_ref.id,
        definition_version: rule.definition_ref.version.to_string(),
        definition_digest: digest_text(rule.definition_ref.digest),
        dimension_value: rule.dimension_value.clone(),
        logical_binding_key: rule.logical_binding_key.clone(),
        subject_selector: rule.subject_selector.token().to_owned(),
        scope_selection_json: Value::Object(scope).to_string(),
        amount: rule.amount.clone(),
        unit_ref: rule.unit_ref.clone(),
        basis: rule.basis.token().to_owned(),
        evidence_json: evidence_json(definition, &rule.unit_ref).to_string(),
        availability_json: availability_json(definition)?.to_string(),
        created_at: now,
    })
}

/// A stored row back as the SDK shape, with its evidence and availability observation. Every
/// stored value must convert: a version that is not a canonical integer from 1, a digest that is
/// not 64 lowercase hex characters, a token outside its closed set, a scope selection that is
/// not a JSON object of strings, evidence that is not a JSON object, an observation that does
/// not read, or a rule that the typed judgement refuses.
/// # Errors
/// `CorruptRow` naming the revision and the rule.
pub fn stored_rule(row: &rule_e::Model) -> Result<StoredRule, RepoError> {
    let corrupt = |what: &str| {
        RepoError::CorruptRow(format!(
            "plan quota rule {} of revision {}: {what}",
            row.rule_id, row.revision_id
        ))
    };
    let version = row
        .definition_version
        .parse::<QuotaVersion>()
        .ok()
        .and_then(|version| NonZeroU64::new(version.get()))
        .ok_or_else(|| corrupt("its definition version is not a canonical integer from 1"))?;
    let digest = digest_from_hex(&row.definition_digest)
        .ok_or_else(|| corrupt("its definition digest is not 64 lowercase hex characters"))?;
    let subject_selector = QuotaSubjectSelector::from_token(&row.subject_selector)
        .ok_or_else(|| corrupt("its subject selector is not a known token"))?;
    let basis = PlanQuotaBasis::from_token(&row.basis)
        .ok_or_else(|| corrupt("its basis is not a known token"))?;
    let scope_selection: BTreeMap<String, String> = serde_json::from_str(&row.scope_selection_json)
        .map_err(|_| corrupt("its scope selection is not a JSON object of strings"))?;
    let evidence: Value = serde_json::from_str(&row.evidence_json)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| corrupt("its evidence is not a JSON object"))?;
    let availability: QuotaAvailability = serde_json::from_str(&row.availability_json)
        .map_err(|_| corrupt("its availability observation does not read"))?;
    let rule = PlanQuotaRuleV1 {
        rule_id: row.rule_id,
        definition_ref: QuotaDefinitionRef {
            id: row.definition_id,
            version,
            digest,
        },
        item_id: row.item_id,
        dimension_value: row.dimension_value.clone(),
        logical_binding_key: row.logical_binding_key.clone(),
        subject_selector,
        scope_selection,
        amount: row.amount.clone(),
        unit_ref: row.unit_ref.clone(),
        basis,
    };
    if let Some(violation) = rule_violations(std::slice::from_ref(&rule)).first() {
        return Err(corrupt(&format!(
            "its {} does not judge: {}",
            violation.field, violation.code
        )));
    }
    Ok(StoredRule {
        rule,
        evidence,
        availability,
    })
}

/// A rule as the `PUT` body carries it and its digest encodes it (D-541): the ten members, the
/// version a decimal string, the digest lowercase hex and the tokens of the closed sets. The SDK's
/// replace sends it, and a unit's content fingerprints it.
#[must_use]
pub fn rule_json(rule: &PlanQuotaRuleV1) -> Value {
    json!({
        "rule_id": rule.rule_id,
        "definition_ref": {
            "id": rule.definition_ref.id,
            "version": rule.definition_ref.version.to_string(),
            "digest": digest_text(rule.definition_ref.digest),
        },
        "item_id": rule.item_id,
        "dimension_value": rule.dimension_value,
        "logical_binding_key": rule.logical_binding_key,
        "subject_selector": rule.subject_selector.token(),
        "scope_selection": rule.scope_selection,
        "amount": rule.amount,
        "unit_ref": rule.unit_ref,
        "basis": rule.basis.token(),
    })
}

/// A stored rule back as the SDK shape.
/// # Errors
/// `CorruptRow` naming the revision and rule ([`stored_rule`]).
pub fn rule_of(row: &rule_e::Model) -> Result<PlanQuotaRuleV1, RepoError> {
    stored_rule(row).map(|stored| stored.rule)
}

/// The stored evidence as [`evidence_json`] writes it: only the members the pinned evidence
/// reads. Versions read through Products' own canonical rule (a decimal string from 1).
#[derive(serde::Deserialize)]
struct StoredEvidence {
    sku_id: Uuid,
    sku_version: QuotaVersion,
    metric_ref: String,
    metric_digest: String,
    quota_kind: String,
    canonical_unit_ref: String,
    unit: Option<StoredUnit>,
    mapping_ref: ProductsOwnerRef,
    profile_ref: ProductsOwnerRef,
    accounting_policy_ref: String,
    accounting_policy_digest: String,
    scope_policy_ref: String,
    scope_policy_digest: String,
    change_support: StoredChangeSupport,
}

/// The rule's unit in the stored evidence: its digest, its factor into the canonical unit, and
/// its grid and maximum. Every member is required, `max` too (`null` when the unit has none): no
/// evidence was stored without them.
#[derive(serde::Deserialize)]
struct StoredUnit {
    digest: String,
    numerator: String,
    denominator: String,
    min: String,
    step: String,
    #[serde(deserialize_with = "present")]
    max: Option<String>,
}

/// A member that must be present, `null` included: unlike a plain `Option`, a missing one does
/// not read.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(d)
}

/// The change support in the stored evidence.
#[derive(serde::Deserialize)]
struct StoredChangeSupport {
    initial_set: bool,
    increase: bool,
}

/// A rule's stored evidence ([`evidence_json`]) as the SDK's pinned evidence (D-547): the quota
/// kind, the mapping and profile references (`mapping_ref`, `profile_ref`), the rule's unit
/// digest, factor, grid and maximum (the `unit` member), both policies and the change support
/// (`change_support.initial_set`, `change_support.increase`). Digests read from 64 lowercase hex
/// characters into bytes, versions from canonical decimal strings into positive integers, the
/// kind through its token; the metric is the one the rule's definition version carried at the
/// save, the metric Products answers for that version. Products checked the grid when it
/// approved the unit, so the grid is only read here.
/// # Errors
/// `CorruptRow` for evidence that does not read so: a missing or mistyped member, a null `unit`
/// (the definition did not allow the rule's unit), a digest that is not 64 lowercase hex
/// characters, a version that is not a canonical integer from 1, a quota kind that is not a known
/// token, a unit factor that Products' parser refuses ([`Factor::parse`]: two canonical integers
/// from 1 that fit in 128 bits, without a common divisor), a unit `min`, `step` or `max` that is
/// not a canonical decimal integer ([`Amount::parse`]: at most 39 digits that fit in 128 bits), or
/// a `max` below the `min`.
pub fn evidence_of(evidence: &Value) -> Result<PinnedQuotaEvidence, RepoError> {
    let corrupt = |what: &str| RepoError::CorruptRow(format!("plan quota rule evidence: {what}"));
    let stored = <StoredEvidence as serde::Deserialize>::deserialize(evidence)
        .map_err(|error| corrupt(&format!("it does not read: {error}")))?;
    let unit = stored.unit.ok_or_else(|| {
        corrupt("its unit is null: the definition does not allow the rule's unit")
    })?;
    let digest = |text: &str, what: &str| {
        digest_from_hex(text)
            .ok_or_else(|| corrupt(&format!("{what} is not 64 lowercase hex characters")))
    };
    let version = |version: QuotaVersion| {
        NonZeroU64::new(version.get()).ok_or_else(|| corrupt("a version is not from 1"))
    };
    let owner = |reference: &ProductsOwnerRef, what: &str| {
        Ok::<_, RepoError>(QuotaOwnerRef {
            id: reference.id,
            version: version(reference.version)?,
            digest: digest(&reference.digest, what)?,
        })
    };
    Factor::parse(&unit.numerator, &unit.denominator)
        .map_err(|error| corrupt(&format!("its unit factor does not read: {error}")))?;
    let amount = |text: &str, member: &str| {
        Amount::parse(text)
            .map_err(|error| corrupt(&format!("its unit {member} does not read: {error}")))
    };
    let min = amount(&unit.min, "min")?;
    amount(&unit.step, "step")?;
    if let Some(max) = &unit.max
        && amount(max, "max")? < min
    {
        return Err(corrupt("its unit max is below its unit min"));
    }
    let quota_kind = PlanQuotaKind::from_token(&stored.quota_kind)
        .ok_or_else(|| corrupt("its quota kind is not a known token"))?;
    Ok(PinnedQuotaEvidence {
        sku_id: stored.sku_id,
        sku_version: version(stored.sku_version)?,
        metric_digest: digest(&stored.metric_digest, "its metric digest")?,
        metric_ref: stored.metric_ref,
        quota_kind,
        canonical_unit_ref: stored.canonical_unit_ref,
        unit_digest: digest(&unit.digest, "its unit digest")?,
        unit_numerator: unit.numerator,
        unit_denominator: unit.denominator,
        unit_min: unit.min,
        unit_step: unit.step,
        unit_max: unit.max,
        mapping: owner(&stored.mapping_ref, "its mapping digest")?,
        profile: owner(&stored.profile_ref, "its profile digest")?,
        accounting_policy_digest: digest(
            &stored.accounting_policy_digest,
            "its accounting policy digest",
        )?,
        accounting_policy_ref: stored.accounting_policy_ref,
        scope_policy_digest: digest(&stored.scope_policy_digest, "its scope policy digest")?,
        scope_policy_ref: stored.scope_policy_ref,
        initial_set_supported: stored.change_support.initial_set,
        increase_supported: stored.change_support.increase,
    })
}

/// A revision's stored set as the preview's quota terms (D-547): every rule in its stored order
/// (`rule_id`) with the evidence its definition pinned at the save, and the stored set digest.
/// No catalog call: the terms read only what the set stored.
/// # Errors
/// `CorruptRow` for a rule ([`stored_rule`]) or its evidence ([`evidence_of`]) that does not read,
/// or a set digest that is not 64 lowercase hex characters.
pub fn quota_terms(set: &StoredSet) -> Result<QuotaTerms, RepoError> {
    let quota_rules_digest = digest_from_hex(&set.digest).ok_or_else(|| {
        RepoError::CorruptRow("plan quota set digest is not 64 lowercase hex characters".into())
    })?;
    let rules = set
        .rules
        .iter()
        .map(|row| {
            let stored = stored_rule(row)?;
            let evidence = evidence_of(&stored.evidence).map_err(|error| match error {
                RepoError::CorruptRow(why) => RepoError::CorruptRow(format!(
                    "plan quota rule {} of revision {}: {why}",
                    row.rule_id, row.revision_id
                )),
                other => other,
            })?;
            Ok(ResolvedQuotaRule {
                rule: stored.rule,
                evidence,
            })
        })
        .collect::<Result<Vec<_>, RepoError>>()?;
    Ok(QuotaTerms {
        quota_rules_digest,
        rules,
    })
}

/// The items a capture judged a set against: each item's SKU and entry.
fn identity(items: &[plan_item::Model]) -> BTreeMap<Uuid, (Uuid, Option<Uuid>)> {
    items
        .iter()
        .map(|item| (item.id, (item.sku_id, item.price_book_entry_id)))
        .collect()
}

/// The quota half of a detached capture (D-545): the stored set and the items it was judged
/// against, the SKU versions on the sale date, and Products' answer or its failure, kept for the
/// subject's gate.
#[derive(Clone)]
pub struct QuotaObservation {
    revision_id: Uuid,
    available_from: Option<time::Date>,
    digest: String,
    items: BTreeMap<Uuid, (Uuid, Option<Uuid>)>,
    rules: Vec<PlanQuotaRuleV1>,
    versions: BTreeMap<Uuid, i64>,
    outcome: Result<QuotaSetValidation, CanonicalError>,
}

impl QuotaObservation {
    /// Read the revision's set and, when it holds rules, its items in one serializable read
    /// transaction, then (outside any transaction) the SKU versions on the revision's sale date
    /// (`available_from`, or `today`) and Products' validation for authoring, as the caller. A
    /// revision without rules keeps the empty set's digest and an empty verdict, reads no SKU
    /// version and asks no catalog: its check still sees a set added since (D-545). A set none
    /// of whose SKUs has a version asks no catalog either: Pricing's own checks report each rule.
    /// # Errors
    /// Local storage failures, a corrupt stored rule; a dependency failure is kept in the
    /// observation.
    pub async fn capture(
        db: &Db,
        hub: &toolkit::ClientHub,
        ctx: &SecurityContext,
        revision: &plan_revision::Model,
        today: time::Date,
    ) -> Result<Self, DoorError> {
        // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-govern:p1:inst-quota-recheck
        let (tenant, id) = (ctx.subject_tenant_id(), revision.id);
        let (items, set) = support::transaction_door_read(db, move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                let set = plan_quota_repo::for_revision(tx, &scope, tenant, id).await?;
                if set.rules.is_empty() {
                    return Ok((Vec::new(), set));
                }
                let items = plan_item_repo::for_revision(tx, &scope, tenant, id).await?;
                Ok((items, set))
            })
        })
        .await?;
        if set.rules.is_empty() {
            return Ok(Self {
                revision_id: id,
                available_from: revision.available_from,
                digest: set.digest,
                items: BTreeMap::new(),
                rules: Vec::new(),
                versions: BTreeMap::new(),
                outcome: Ok(QuotaSetValidation {
                    rules: Vec::new(),
                    checks: Vec::new(),
                }),
            });
        }
        let rules = set
            .rules
            .iter()
            .map(rule_of)
            .collect::<Result<Vec<_>, _>>()?;
        let rule_items: Vec<RuleItem> = items
            .iter()
            .map(|item| RuleItem {
                id: item.id,
                sku_id: item.sku_id,
                // The request reads only each item's SKU; the checks judge the dimension.
                dimension_key: None,
            })
            .collect();
        let skus: Vec<Uuid> = rules
            .iter()
            .filter_map(|rule| {
                rule_items
                    .iter()
                    .find(|item| item.id == rule.item_id)
                    .map(|item| item.sku_id)
            })
            .collect();
        let sale = crate::domain::plan::sale_date_of(revision.available_from, today);
        let (versions, outcome) = match sku_versions(hub, ctx, &skus, sale).await {
            Ok(versions) => {
                let request = set_request(&rules, &rule_items, &versions, Purpose::Authoring);
                let outcome = if request.rules.is_empty() {
                    Ok(QuotaSetValidation {
                        rules: Vec::new(),
                        checks: Vec::new(),
                    })
                } else {
                    validate(hub, ctx, request).await
                };
                (versions, outcome)
            }
            Err(error) => (BTreeMap::new(), Err(error)),
        };
        // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-govern:p1:inst-quota-recheck
        Ok(Self {
            revision_id: id,
            available_from: revision.available_from,
            digest: set.digest,
            items: identity(&items),
            rules,
            versions,
            outcome,
        })
    }

    /// In the transaction: the revision's sale date, its items and its set digest are still the
    /// captured ones. A capture of the empty set compares only the set (two statements), so a
    /// quota-free unit reads nothing more than its digest. Reads only stored state; no dependency
    /// call.
    /// # Errors
    /// `DoorError::SelectionMoved` when any moved (or the revision is gone); storage failures.
    pub async fn check_local(&self, tx: &DbTx<'_>, tenant: Uuid) -> Result<(), DoorError> {
        // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-govern:p1:inst-quota-recheck
        let scope = AccessScope::for_tenant(tenant);
        if self.rules.is_empty() {
            let set = plan_quota_repo::for_revision(tx, &scope, tenant, self.revision_id).await?;
            if set.digest != self.digest {
                return Err(DoorError::SelectionMoved);
            }
            return Ok(());
        }
        let revision = plan_revision_repo::find(tx, &scope, tenant, self.revision_id).await?;
        if revision.is_none_or(|revision| revision.available_from != self.available_from) {
            return Err(DoorError::SelectionMoved);
        }
        let items = plan_item_repo::for_revision(tx, &scope, tenant, self.revision_id).await?;
        if identity(&items) != self.items {
            return Err(DoorError::SelectionMoved);
        }
        let set = plan_quota_repo::for_revision(tx, &scope, tenant, self.revision_id).await?;
        if set.digest != self.digest {
            return Err(DoorError::SelectionMoved);
        }
        // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-govern:p1:inst-quota-recheck
        Ok(())
    }

    /// Products' answer, or the dependency failure.
    /// # Errors
    /// The captured failure.
    pub fn verdict(&self) -> Result<&QuotaSetValidation, CanonicalError> {
        self.outcome.as_ref().map_err(Clone::clone)
    }

    /// The captured rules, as stored (by `rule_id`).
    #[must_use]
    pub fn rules(&self) -> &[PlanQuotaRuleV1] {
        &self.rules
    }

    /// Each ruled SKU's version on the sale date; a SKU without one is absent.
    #[must_use]
    pub const fn versions(&self) -> &BTreeMap<Uuid, i64> {
        &self.versions
    }

    /// The captured set digest, lowercase hex, as stored.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[cfg(test)]
#[path = "plan_quota_tests.rs"]
mod plan_quota_tests;
