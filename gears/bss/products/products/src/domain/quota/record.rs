//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! Judging one capability record (P-D-268; DESIGN §3.1 "Judging a record"), by the algorithm
//! `cpt-cf-bss-products-algo-quota-catalog-record`. Discovery and manual input pass the same
//! checks in the algorithm's order, and the first that fails decides. Each refusal names its code
//! and field; a registry outage at any check is no verdict at all.
//!
//! Identity resolves every reference before it asks for schema validation, so a reference of a
//! wrong family or version answers its own code, never the schema's generic invalidity. The
//! registry is asked as the caller, outside any transaction, and one [`Resolutions`] keeps its
//! answers for a whole intake.
use std::collections::{BTreeSet, HashMap};

use bss_products_sdk::quota::digest::{content_digest, record_digest};
use bss_products_sdk::quota::gts::{
    ACCOUNTING_POLICY_TYPE, CAPABILITY_TYPE, METRIC_TYPE, QuotaAccountingPolicyV1,
    QuotaCapabilityV1, QuotaChangeSupport, QuotaLimits, QuotaMetricV1, QuotaOperations,
    QuotaScopePolicyV1, QuotaUnitV1, SCOPE_POLICY_TYPE, UNIT_TYPE,
};
use bss_products_sdk::quota::units::{Amount, Factor};
use gts::{GtsId, GtsInstanceId, GtsTypeId};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::evidence::{Evidence, EvidenceSet};
use super::locale::check_localized;
use super::ports::{
    QuotaRegistry, RegistryInstance, RegistryTypeSchema, RegistryUnavailable, RegistryVerdict,
};

/// The record does not parse into the closed v1 shape, or the schema refuses it.
pub const QUOTA_RECORD_INVALID: &str = "QUOTA_RECORD_INVALID";
/// A reference names nothing registered.
pub const QUOTA_REFERENCE_UNKNOWN: &str = "QUOTA_REFERENCE_UNKNOWN";
/// A reference names an entity of another family, or an abstract type.
pub const QUOTA_REFERENCE_FAMILY: &str = "QUOTA_REFERENCE_FAMILY";
/// A segment carries a minor version, or a Products segment another major than `v1`.
pub const QUOTA_VERSION_UNSUPPORTED: &str = "QUOTA_VERSION_UNSUPPORTED";
/// The metric is not an allocation metric.
pub const QUOTA_CAPABILITY_UNSUPPORTED: &str = "QUOTA_CAPABILITY_UNSUPPORTED";
/// A unit's factor or canonical unit is not exact and direct.
pub const QUOTA_UNIT_INVALID: &str = "QUOTA_UNIT_INVALID";
/// The record's unit and the metric have different canonical units.
pub const QUOTA_UNIT_MISMATCH: &str = "QUOTA_UNIT_MISMATCH";
/// A policy is outside its v1 meaning.
pub const QUOTA_POLICY_INVALID: &str = "QUOTA_POLICY_INVALID";

/// The most characters `capability_key` and `mapping_key` hold.
const MAX_KEY_CHARS: usize = 255;
/// The most resource types one record counts.
const MAX_RESOURCE_TYPES: usize = 200;

/// Why one record is refused: a stable code, the field it names and a plain explanation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRefusal {
    pub code: &'static str,
    pub field: String,
    pub detail: String,
}

impl RecordRefusal {
    fn new(code: &'static str, field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: field.into(),
            detail: detail.into(),
        }
    }
}

/// A record is refused, or the registry could not answer (P-D-268: an outage aborts the intake).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeError {
    Refused(RecordRefusal),
    Unavailable(String),
}

impl From<RecordRefusal> for JudgeError {
    fn from(refusal: RecordRefusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<RegistryUnavailable> for JudgeError {
    fn from(outage: RegistryUnavailable) -> Self {
        Self::Unavailable(outage.0)
    }
}

/// A record that passed every check, with what it resolved to.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgedRecord {
    pub record: QuotaCapabilityV1,
    /// The record as received.
    pub raw: Value,
    pub record_digest: String,
    pub evidence: EvidenceSet,
    pub metric: QuotaMetricV1,
    pub unit: QuotaUnitV1,
    /// The metric's canonical unit; the record's own unit when that is canonical.
    pub canonical_unit: QuotaUnitV1,
    pub accounting: QuotaAccountingPolicyV1,
    pub scope: QuotaScopePolicyV1,
}

/// Registry answers memoized for one intake run, so a snapshot of many records asks once per
/// entity; a schema verdict is kept per type and record digest.
#[domain_model]
#[derive(Debug, Default)]
pub struct Resolutions {
    instances: HashMap<String, Option<RegistryInstance>>,
    types: HashMap<String, Option<RegistryTypeSchema>>,
    verdicts: HashMap<(String, String), RegistryVerdict>,
}

impl Resolutions {
    async fn instance(
        &mut self,
        registry: &dyn QuotaRegistry,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryInstance>, JudgeError> {
        if let Some(known) = self.instances.get(id) {
            return Ok(known.clone());
        }
        let read = registry.instance(ctx, id).await?;
        self.instances.insert(id.to_owned(), read.clone());
        Ok(read)
    }

    async fn type_schema(
        &mut self,
        registry: &dyn QuotaRegistry,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryTypeSchema>, JudgeError> {
        if let Some(known) = self.types.get(id) {
            return Ok(known.clone());
        }
        let read = registry.type_schema(ctx, id).await?;
        self.types.insert(id.to_owned(), read.clone());
        Ok(read)
    }

    async fn verdict(
        &mut self,
        registry: &dyn QuotaRegistry,
        ctx: &SecurityContext,
        type_id: &str,
        raw: &Value,
        digest: &str,
    ) -> Result<RegistryVerdict, JudgeError> {
        let key = (type_id.to_owned(), digest.to_owned());
        if let Some(known) = self.verdicts.get(&key) {
            return Ok(known.clone());
        }
        let verdict = registry.validate(ctx, type_id, raw).await?;
        self.verdicts.insert(key, verdict.clone());
        Ok(verdict)
    }
}

/// Judge one raw capability record by the record algorithm's checks, in its order.
///
/// # Errors
/// `JudgeError::Refused` with the first failing check's code and field; `JudgeError::Unavailable`
/// on a registry outage.
// @cpt-algo:cpt-cf-bss-products-algo-quota-catalog-record:p1
pub async fn judge_record(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    raw: &Value,
    memo: &mut Resolutions,
) -> Result<JudgedRecord, JudgeError> {
    let record = shape(raw)?;
    let digest = record_digest(raw);
    let resolved = identity(registry, ctx, raw, &record, &digest, memo).await?;
    let canonical = units(registry, ctx, &record, &resolved, memo).await?;
    let (accounting, scope) = policies(&resolved)?;
    let evidence = evidence(&resolved, &canonical.0);
    Ok(JudgedRecord {
        record,
        raw: raw.clone(),
        record_digest: digest,
        evidence,
        metric: resolved.metric,
        unit: resolved.unit,
        canonical_unit: canonical.1,
        accounting,
        scope,
    })
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-shape
/// The closed v1 shape: the record parses, its bounds are canonical decimals with a positive
/// step and `min` not above `max`, its keys are 1 to 255 characters, and it counts 1 to 200
/// unique resource types.
fn shape(raw: &Value) -> Result<QuotaCapabilityV1, RecordRefusal> {
    let invalid =
        |field: &str, detail: String| RecordRefusal::new(QUOTA_RECORD_INVALID, field, detail);
    let record: QuotaCapabilityV1 = serde_json::from_value(raw.clone())
        .map_err(|e| invalid(&shape_field(raw), e.to_string()))?;
    let amount = |field: &str, text: &str| {
        Amount::parse(text).map_err(|e| invalid(field, format!("{text:?}: {e}")))
    };
    let min = amount("limits.min", &record.limits.min)?;
    let step = amount("limits.step", &record.limits.step)?;
    if step.get() == 0 {
        return Err(invalid(
            "limits.step",
            "the step must be positive".to_owned(),
        ));
    }
    if let Some(max) = &record.limits.max
        && amount("limits.max", max)? < min
    {
        return Err(invalid("limits.max", "max is below min".to_owned()));
    }
    for (field, key) in [
        ("capability_key", &record.capability_key),
        ("mapping_key", &record.mapping_key),
    ] {
        if !(1..=MAX_KEY_CHARS).contains(&key.chars().count()) {
            return Err(invalid(field, format!("1 to {MAX_KEY_CHARS} characters")));
        }
    }
    let unique: BTreeSet<&str> = record
        .resource_type_refs
        .iter()
        .map(AsRef::as_ref)
        .collect();
    if !(1..=MAX_RESOURCE_TYPES).contains(&record.resource_type_refs.len())
        || unique.len() != record.resource_type_refs.len()
    {
        return Err(invalid(
            "resource_type_refs",
            format!("1 to {MAX_RESOURCE_TYPES} unique resource types"),
        ));
    }
    Ok(record)
}

/// The record's members and how each one must read.
const MEMBERS: [&str; 14] = [
    "id",
    "type_id",
    "capability_key",
    "metric_ref",
    "unit_ref",
    "subject_type_ref",
    "resource_type_refs",
    "limits",
    "operations",
    "mapping_key",
    "accounting_policy_ref",
    "scope_policy_ref",
    "change_support",
    "properties",
];

/// The field a record that does not parse fails on: its first unknown member, else the first
/// member, or member of a nested object, that is missing or does not read as its type.
fn shape_field(raw: &Value) -> String {
    let Some(record) = raw.as_object() else {
        return "record".to_owned();
    };
    if let Some(unknown) = record.keys().find(|key| !MEMBERS.contains(&key.as_str())) {
        return unknown.clone();
    }
    MEMBERS
        .into_iter()
        .find_map(|name| member_fault(name, record.get(name)))
        .unwrap_or_else(|| "record".to_owned())
}

fn reads<T: DeserializeOwned>(value: &Value) -> bool {
    serde_json::from_value::<T>(value.clone()).is_ok()
}

fn member_fault(name: &str, value: Option<&Value>) -> Option<String> {
    let Some(value) = value else {
        return Some(name.to_owned());
    };
    let reads_as_its_type = match name {
        "id" => reads::<Uuid>(value),
        "type_id" | "subject_type_ref" => reads::<GtsTypeId>(value),
        "resource_type_refs" => reads::<Vec<GtsTypeId>>(value),
        "capability_key" | "mapping_key" => value.is_string(),
        "properties" => reads::<Map<String, Value>>(value),
        "limits" => {
            return nested_fault(name, value, reads::<QuotaLimits>(value), Value::is_string);
        }
        "operations" => {
            return nested_fault(
                name,
                value,
                reads::<QuotaOperations>(value),
                Value::is_boolean,
            );
        }
        "change_support" => {
            return nested_fault(
                name,
                value,
                reads::<QuotaChangeSupport>(value),
                Value::is_boolean,
            );
        }
        _ => reads::<GtsInstanceId>(value),
    };
    (!reads_as_its_type).then(|| name.to_owned())
}

/// The member of a nested object that is unknown, missing or of the wrong kind, as `name.member`.
fn nested_fault(
    name: &str,
    value: &Value,
    reads: bool,
    leaf: fn(&Value) -> bool,
) -> Option<String> {
    if reads {
        return None;
    }
    let (required, optional): (&[&str], &[&str]) = match name {
        "limits" => (&["min", "step"], &["max"]),
        "operations" => (
            &[
                "apply",
                "read_application",
                "recover_operation",
                "restrict",
                "revoke",
                "resume",
            ],
            &[],
        ),
        _ => (&["initial_set", "increase"], &[]),
    };
    let Some(object) = value.as_object() else {
        return Some(name.to_owned());
    };
    let known = |key: &str| required.contains(&key) || optional.contains(&key);
    let fault = object.keys().find(|key| !known(key)).cloned().or_else(|| {
        required
            .iter()
            .chain(optional)
            .find(|key| match object.get(**key) {
                None => required.contains(*key),
                Some(member) => !leaf(member),
            })
            .map(|key| (*key).to_owned())
    });
    Some(fault.map_or_else(|| name.to_owned(), |member| format!("{name}.{member}")))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-shape

/// What identity resolved, in the order the evidence keeps it.
struct Resolved {
    chain: Vec<RegistryTypeSchema>,
    metric_entity: RegistryInstance,
    metric: QuotaMetricV1,
    unit_entity: RegistryInstance,
    unit: QuotaUnitV1,
    accounting_entity: RegistryInstance,
    scope_entity: RegistryInstance,
    subject: RegistryTypeSchema,
    resources: Vec<RegistryTypeSchema>,
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-identity
/// Every segment of `id` is major-only, and every Products quota segment is exactly `v1`.
pub(crate) fn major_only(field: &str, id: &str) -> Result<(), RecordRefusal> {
    let parsed = GtsId::try_new(id)
        .map_err(|e| RecordRefusal::new(QUOTA_RECORD_INVALID, field, e.to_string()))?;
    for segment in parsed.segments() {
        if segment.ver_minor().is_some() {
            return Err(RecordRefusal::new(
                QUOTA_VERSION_UNSUPPORTED,
                field,
                format!("{} carries a minor version", segment.raw()),
            ));
        }
        let products_quota = segment.vendor() == "cf"
            && segment.package() == "bss"
            && segment.namespace() == "products"
            && segment.type_name().starts_with("quota_");
        if products_quota && segment.ver_major_opt() != Some(1) {
            return Err(RecordRefusal::new(
                QUOTA_VERSION_UNSUPPORTED,
                field,
                format!("{} is not v1", segment.raw()),
            ));
        }
    }
    Ok(())
}

/// The family schema an instance's type chain starts with.
fn family_of(type_id: &str) -> Option<String> {
    GtsId::try_new(type_id)
        .ok()
        .and_then(|id| id.chain_ids().into_iter().next())
}

/// The live instance `id` of `family`: exactly that type, or (`derived_too`) a type derived from
/// it.
async fn instance_of(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    memo: &mut Resolutions,
    field: &str,
    id: &str,
    family: &str,
    derived_too: bool,
) -> Result<RegistryInstance, JudgeError> {
    let instance = memo.instance(registry, ctx, id).await?.ok_or_else(|| {
        RecordRefusal::new(
            QUOTA_REFERENCE_UNKNOWN,
            field,
            format!("{id} is not registered"),
        )
    })?;
    let in_family = if derived_too {
        family_of(&instance.type_id).as_deref() == Some(family)
    } else {
        instance.type_id == family
    };
    let names_itself = instance.content.get("id").and_then(Value::as_str) == Some(id);
    if !in_family || !names_itself {
        return Err(RecordRefusal::new(
            QUOTA_REFERENCE_FAMILY,
            field,
            format!("{id} is a {}, not a {family} instance", instance.type_id),
        )
        .into());
    }
    Ok(instance)
}

/// The live, concrete type `id`.
async fn concrete_type(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    memo: &mut Resolutions,
    field: &str,
    id: &str,
) -> Result<RegistryTypeSchema, JudgeError> {
    let schema = memo.type_schema(registry, ctx, id).await?.ok_or_else(|| {
        RecordRefusal::new(
            QUOTA_REFERENCE_UNKNOWN,
            field,
            format!("{id} is not registered"),
        )
    })?;
    if schema.is_abstract {
        return Err(
            RecordRefusal::new(QUOTA_REFERENCE_FAMILY, field, format!("{id} is abstract")).into(),
        );
    }
    Ok(schema)
}

/// The record's identity: major-only versions, the capability family, every schema of its
/// `type_id` chain, its references of the right families with English metric text, concrete
/// subject and resource types, and finally the registry's schema validation of the whole record.
async fn identity(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    raw: &Value,
    record: &QuotaCapabilityV1,
    digest: &str,
    memo: &mut Resolutions,
) -> Result<Resolved, JudgeError> {
    let type_id = record.gts_type.as_ref();
    for (field, id) in [
        ("type_id", type_id),
        ("metric_ref", record.metric_ref.as_ref()),
        ("unit_ref", record.unit_ref.as_ref()),
        (
            "accounting_policy_ref",
            record.accounting_policy_ref.as_ref(),
        ),
        ("scope_policy_ref", record.scope_policy_ref.as_ref()),
    ] {
        major_only(field, id)?;
    }
    if family_of(type_id).as_deref() != Some(CAPABILITY_TYPE) {
        return Err(RecordRefusal::new(
            QUOTA_REFERENCE_FAMILY,
            "type_id",
            format!("{type_id} is not the capability schema or derived from it"),
        )
        .into());
    }
    let mut chain = Vec::new();
    for ancestor in GtsId::try_new(type_id)
        .map(|id| id.chain_ids())
        .unwrap_or_default()
    {
        let leaf = ancestor == type_id;
        chain.push(concrete_or_ancestor(registry, ctx, memo, &ancestor, leaf).await?);
    }

    let metric_ref = record.metric_ref.as_ref();
    let metric_entity = instance_of(
        registry,
        ctx,
        memo,
        "metric_ref",
        metric_ref,
        METRIC_TYPE,
        true,
    )
    .await?;
    if let Some(kind) = metric_entity
        .content
        .get("quota_kind")
        .and_then(Value::as_str)
        && kind != "allocation"
    {
        return Err(RecordRefusal::new(
            QUOTA_CAPABILITY_UNSUPPORTED,
            "metric_ref",
            format!("a {kind} metric is not an allocation metric"),
        )
        .into());
    }
    let metric: QuotaMetricV1 = parse_family(&metric_entity, "metric_ref")?;
    check_localized("metric_ref.display_name", &metric.display_name)?;
    check_localized("metric_ref.description", &metric.description)?;

    let unit_ref = record.unit_ref.as_ref();
    let unit_entity =
        instance_of(registry, ctx, memo, "unit_ref", unit_ref, UNIT_TYPE, false).await?;
    let unit: QuotaUnitV1 = parse_family(&unit_entity, "unit_ref")?;
    let accounting_entity = instance_of(
        registry,
        ctx,
        memo,
        "accounting_policy_ref",
        record.accounting_policy_ref.as_ref(),
        ACCOUNTING_POLICY_TYPE,
        false,
    )
    .await?;
    let scope_entity = instance_of(
        registry,
        ctx,
        memo,
        "scope_policy_ref",
        record.scope_policy_ref.as_ref(),
        SCOPE_POLICY_TYPE,
        false,
    )
    .await?;

    let subject = concrete_type(
        registry,
        ctx,
        memo,
        "subject_type_ref",
        record.subject_type_ref.as_ref(),
    )
    .await?;
    let mut resources = Vec::with_capacity(record.resource_type_refs.len());
    for (i, resource) in record.resource_type_refs.iter().enumerate() {
        let field = format!("resource_type_refs[{i}]");
        resources.push(concrete_type(registry, ctx, memo, &field, resource.as_ref()).await?);
    }

    match memo.verdict(registry, ctx, type_id, raw, digest).await? {
        RegistryVerdict::Valid => {}
        RegistryVerdict::Invalid(why) => {
            return Err(RecordRefusal::new(QUOTA_RECORD_INVALID, "properties", why).into());
        }
        RegistryVerdict::UnknownSchema => {
            return Err(RecordRefusal::new(
                QUOTA_REFERENCE_UNKNOWN,
                "type_id",
                format!("{type_id} is not registered"),
            )
            .into());
        }
        RegistryVerdict::Uncompilable(why) => {
            return Err(RecordRefusal::new(
                QUOTA_RECORD_INVALID,
                "type_id",
                format!("the schema {type_id} does not compile: {why}"),
            )
            .into());
        }
    }
    Ok(Resolved {
        chain,
        metric_entity,
        metric,
        unit_entity,
        unit,
        accounting_entity,
        scope_entity,
        subject,
        resources,
    })
}

/// One schema of the `type_id` chain: registered, and the leaf concrete.
async fn concrete_or_ancestor(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    memo: &mut Resolutions,
    id: &str,
    leaf: bool,
) -> Result<RegistryTypeSchema, JudgeError> {
    if leaf {
        return concrete_type(registry, ctx, memo, "type_id", id).await;
    }
    Ok(memo.type_schema(registry, ctx, id).await?.ok_or_else(|| {
        RecordRefusal::new(
            QUOTA_REFERENCE_UNKNOWN,
            "type_id",
            format!("{id} is not registered"),
        )
    })?)
}

/// An instance's content as its family's struct; content that does not read as the family is a
/// family refusal.
fn parse_family<T: DeserializeOwned>(
    entity: &RegistryInstance,
    field: &str,
) -> Result<T, RecordRefusal> {
    serde_json::from_value(entity.content.clone()).map_err(|e| {
        RecordRefusal::new(
            QUOTA_REFERENCE_FAMILY,
            field,
            format!("{} does not read as its family: {e}", entity.gts_id),
        )
    })
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-identity

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-units
/// The record's unit converts directly into the metric's canonical unit, which refers to itself
/// with factor 1/1; both factors are positive and reduced. Answers the canonical unit.
async fn units(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    record: &QuotaCapabilityV1,
    resolved: &Resolved,
    memo: &mut Resolutions,
) -> Result<(RegistryInstance, QuotaUnitV1), JudgeError> {
    let unit = &resolved.unit;
    Factor::parse(&unit.numerator, &unit.denominator).map_err(|e| {
        RecordRefusal::new(QUOTA_UNIT_INVALID, "unit_ref", format!("its factor: {e}"))
    })?;
    let canonical_ref = resolved.metric.canonical_unit_ref.as_str();
    let not_canonical =
        |detail: String| RecordRefusal::new(QUOTA_UNIT_INVALID, "metric_ref", detail);
    major_only("metric_ref.canonical_unit_ref", canonical_ref)?;
    let (canonical_entity, canonical) = if canonical_ref == record.unit_ref.as_ref() {
        (resolved.unit_entity.clone(), unit.clone())
    } else {
        let entity = memo
            .instance(registry, ctx, canonical_ref)
            .await?
            .filter(|entity| entity.type_id == UNIT_TYPE)
            .ok_or_else(|| not_canonical(format!("{canonical_ref} is not a registered unit")))?;
        if entity.content.get("id").and_then(Value::as_str) != Some(canonical_ref) {
            return Err(not_canonical(format!("{canonical_ref} does not name itself")).into());
        }
        let parsed: QuotaUnitV1 = serde_json::from_value(entity.content.clone())
            .map_err(|e| not_canonical(format!("{canonical_ref} does not read as a unit: {e}")))?;
        (entity, parsed)
    };
    let factor = Factor::parse(&canonical.numerator, &canonical.denominator)
        .map_err(|e| not_canonical(format!("{canonical_ref}'s factor: {e}")))?;
    if canonical.canonical_unit_ref != canonical_ref || !factor.is_one() {
        return Err(not_canonical(format!(
            "{canonical_ref} is not canonical: it must refer to itself with factor 1/1"
        ))
        .into());
    }
    if unit.canonical_unit_ref != canonical_ref {
        return Err(RecordRefusal::new(
            QUOTA_UNIT_MISMATCH,
            "unit_ref",
            format!(
                "the unit converts into {}, the metric counts in {canonical_ref}",
                unit.canonical_unit_ref
            ),
        )
        .into());
    }
    Ok((canonical_entity, canonical))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-units

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-policies
/// The policies' v1 meaning: allocated capacity summed with English text, and the resource owner
/// in one verified scope with unique required dimensions.
fn policies(
    resolved: &Resolved,
) -> Result<(QuotaAccountingPolicyV1, QuotaScopePolicyV1), RecordRefusal> {
    let accounting: QuotaAccountingPolicyV1 =
        serde_json::from_value(resolved.accounting_entity.content.clone()).map_err(|e| {
            RecordRefusal::new(QUOTA_POLICY_INVALID, "accounting_policy_ref", e.to_string())
        })?;
    check_localized("accounting_policy_ref.description", &accounting.description)
        .map_err(|r| RecordRefusal::new(QUOTA_POLICY_INVALID, r.field, r.detail))?;
    let scope: QuotaScopePolicyV1 = serde_json::from_value(resolved.scope_entity.content.clone())
        .map_err(|e| {
        RecordRefusal::new(QUOTA_POLICY_INVALID, "scope_policy_ref", e.to_string())
    })?;
    let unique: BTreeSet<_> = scope.required_dimensions.iter().collect();
    if unique.len() != scope.required_dimensions.len() {
        return Err(RecordRefusal::new(
            QUOTA_POLICY_INVALID,
            "scope_policy_ref",
            "a required dimension repeats",
        ));
    }
    Ok((accounting, scope))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-policies

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-evidence
/// Every resolved schema and instance as `{uuid, resource_version, digest}`, the digest taken
/// over its registry content.
fn evidence(resolved: &Resolved, canonical: &RegistryInstance) -> EvidenceSet {
    let mut set = EvidenceSet::default();
    let schemas = resolved
        .chain
        .iter()
        .chain(std::iter::once(&resolved.subject))
        .chain(&resolved.resources);
    for schema in schemas {
        set.insert(Evidence {
            gts_id: schema.gts_id.clone(),
            uuid: schema.uuid,
            resource_version: schema.resource_version,
            digest: content_digest(&schema.content),
        });
    }
    for instance in [
        &resolved.metric_entity,
        &resolved.unit_entity,
        canonical,
        &resolved.accounting_entity,
        &resolved.scope_entity,
    ] {
        set.insert(Evidence {
            gts_id: instance.gts_id.clone(),
            uuid: instance.uuid,
            resource_version: instance.resource_version,
            digest: content_digest(&instance.content),
        });
    }
    set
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-record:p1:inst-quota-evidence

#[cfg(test)]
#[path = "record_tests.rs"]
mod record_tests;
