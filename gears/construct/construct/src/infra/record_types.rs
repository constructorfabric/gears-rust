use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::gts::{RECORD_BASE_SCHEMA, RECORD_BASE_TYPE};
use jsonschema::error::ValidationErrorKind;
use jsonschema::{Draft, ValidationError, Validator};
use serde_json::Value;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{GtsTypeSchema, TypesRegistryClient};

use crate::domain::error::{DomainError, is_retryable};
use crate::domain::record_intake::{Place, RecordTypes, Refusal, RefusalReason};

/// How long one type lookup may take before it counts as the types registry
/// being unavailable.
pub const DEFAULT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// [`RecordTypes`] backed by the types registry, read through `ClientHub`.
///
/// A record is checked in three steps: against the base envelope, which this
/// gear knows without the registry; then its type is looked up, must be
/// concrete and must derive from the base; then the record is checked against
/// that type's effective schema, which includes the base. The first broken
/// rule is reported, with its place as a JSON pointer. A value from the record
/// is never repeated: the type is named only once the envelope has accepted
/// it as a well-formed type id.
///
/// A registered type version does not change, so the compiled schema of each
/// type is kept for the life of the process.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-types:p1
pub struct RegistryRecordTypes {
    hub: Arc<ClientHub>,
    envelope: Validator,
    lookup_timeout: Duration,
    compiled: Mutex<HashMap<String, Arc<Validator>>>,
}

impl RegistryRecordTypes {
    /// # Errors
    ///
    /// When the base schema this gear ships does not compile, which is a
    /// defect in this gear.
    pub fn new(hub: Arc<ClientHub>) -> anyhow::Result<Self> {
        let base: Value = serde_json::from_str(RECORD_BASE_SCHEMA)?;
        Ok(Self {
            hub,
            envelope: compile(&base)?,
            lookup_timeout: DEFAULT_LOOKUP_TIMEOUT,
            compiled: Mutex::new(HashMap::new()),
        })
    }

    /// Replace [`DEFAULT_LOOKUP_TIMEOUT`].
    #[must_use]
    pub fn with_lookup_timeout(mut self, lookup_timeout: Duration) -> Self {
        self.lookup_timeout = lookup_timeout;
        self
    }

    /// Look the type up, or say why the record is refused.
    async fn lookup(&self, type_id: &str) -> Result<GtsTypeSchema, DomainError> {
        let Some(registry) = self.hub.try_get::<dyn TypesRegistryClient>() else {
            return Err(DomainError::Unavailable(
                "no types registry client in ClientHub".to_owned(),
            ));
        };
        let Ok(found) =
            tokio::time::timeout(self.lookup_timeout, registry.get_type_schema(type_id)).await
        else {
            return Err(DomainError::Unavailable(format!(
                "types registry gave no answer within {:?}",
                self.lookup_timeout
            )));
        };
        match found {
            Ok(schema) => Ok(schema),
            Err(CanonicalError::NotFound { .. } | CanonicalError::InvalidArgument { .. }) => {
                Err(Refusal::new(
                    RefusalReason::UnknownType,
                    Some(type_id),
                    Place::pointer("/type"),
                    "the type is not registered",
                )
                .into())
            }
            Err(err) if is_retryable(&err) => Err(DomainError::Unavailable(format!(
                "types registry unavailable: {}",
                describe(&err)
            ))),
            Err(err) => Err(DomainError::internal(format!(
                "types registry lookup failed: {}",
                describe(&err)
            ))),
        }
    }

    /// The compiled effective schema of a type, compiled on first use.
    fn validator_for(
        &self,
        type_id: &str,
        schema: &GtsTypeSchema,
    ) -> Result<Arc<Validator>, DomainError> {
        if let Some(validator) = self.cached(type_id) {
            return Ok(validator);
        }
        let validator = Arc::new(
            compile(&schema.effective_schema())
                .map_err(|err| DomainError::internal(format!("{type_id}: {err}")))?,
        );
        if let Ok(mut compiled) = self.compiled.lock() {
            compiled.insert(type_id.to_owned(), Arc::clone(&validator));
        }
        Ok(validator)
    }

    fn cached(&self, type_id: &str) -> Option<Arc<Validator>> {
        self.compiled.lock().ok()?.get(type_id).cloned()
    }
}

/// The error with its diagnostic, which holds the real cause of an internal
/// error; the message never reaches a caller.
fn describe(err: &CanonicalError) -> String {
    match err.diagnostic() {
        Some(diagnostic) => format!("{err}: {diagnostic}"),
        None => err.to_string(),
    }
}

fn compile(schema: &Value) -> anyhow::Result<Validator> {
    // Draft 7, as the schemas declare: a GTS id in `$id` defeats the draft
    // detection.
    jsonschema::options()
        .with_draft(Draft::Draft7)
        .build(schema)
        .map_err(|err| anyhow::anyhow!("record schema does not compile: {}", err.masked()))
}

/// The first rule the record breaks, if any.
fn first_violation(
    validator: &Validator,
    record: &Value,
    type_id: Option<&str>,
) -> Result<(), DomainError> {
    match validator.iter_errors(record).next() {
        None => Ok(()),
        Some(err) => Err(violation(&err, type_id).into()),
    }
}

/// A refusal for one schema error: the place is the instance path, narrowed to
/// the member for a missing or an unexpected property, and the rule is the
/// keyword that failed.
fn violation(err: &ValidationError<'_>, type_id: Option<&str>) -> Refusal {
    let mut pointer = err.instance_path().to_string();
    match err.kind() {
        ValidationErrorKind::Required { property } => {
            if let Some(name) = property.as_str() {
                pointer = format!("{pointer}/{name}");
            }
        }
        ValidationErrorKind::AdditionalProperties { unexpected } => {
            if let Some(name) = unexpected.first() {
                pointer = format!("{pointer}/{name}");
            }
        }
        _ => {}
    }
    let place = if pointer.is_empty() {
        Place::Whole
    } else {
        Place::Pointer(pointer)
    };
    let schema_path = err.schema_path().to_string();
    let rule = schema_path.rsplit('/').next().unwrap_or_default();
    Refusal::new(RefusalReason::SchemaViolation, type_id, place, rule)
}

#[async_trait]
impl RecordTypes for RegistryRecordTypes {
    async fn check(&self, record: &Value) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-base
        // The type is not named: it may be the very member that is broken.
        first_violation(&self.envelope, record, None)?;
        // The base schema requires `type` as a well-formed type id.
        let type_id = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-base

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-lookup
        let schema = self.lookup(type_id).await?;
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-lookup

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-derives
        if schema.raw_schema.get("x-gts-abstract") == Some(&Value::Bool(true)) {
            return Err(Refusal::new(
                RefusalReason::AbstractType,
                Some(type_id),
                Place::pointer("/type"),
                "the type is abstract",
            )
            .into());
        }
        let derives = schema
            .ancestors()
            .skip(1)
            .any(|ancestor| ancestor.type_id.as_ref() == RECORD_BASE_TYPE);
        if !derives {
            return Err(Refusal::new(
                RefusalReason::NotARecordType,
                Some(type_id),
                Place::pointer("/type"),
                "the type does not derive from the record base type",
            )
            .into());
        }
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-derives

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-schema
        let validator = self.validator_for(type_id, &schema)?;
        first_violation(&validator, record, Some(type_id))
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-schema
    }
}
