use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::error::DomainError;
use super::profile::{Category, ProfileVersion};
use super::record_intake::ReceivedRecord;

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub origin: PlanOrigin,
    pub profile_version: ProfileVersion,
    pub steps: Vec<Step>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum PlanOrigin {
    Record(RecordOrigin),
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOrigin {
    pub connector: String,
    pub type_id: String,
    pub provenance: String,
    pub version: String,
    pub observed_at: OffsetDateTime,
}

impl RecordOrigin {
    pub fn of(record: &ReceivedRecord) -> Result<Self, DomainError> {
        let observed_at = record
            .record
            .get("observed_at")
            .and_then(Value::as_str)
            .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
            .ok_or_else(|| DomainError::internal("the record has no readable observed_at"))?;
        Ok(Self {
            connector: record.connector.clone(),
            type_id: record.envelope.type_id.clone(),
            provenance: record.envelope.provenance.clone(),
            version: record.envelope.version.clone(),
            observed_at,
        })
    }
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NewFact {
    pub category: Category,
    pub property: String,
    pub value: Value,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Add {
        node_key: String,
        fact: NewFact,
        confidence: f64,
    },
    Replace {
        old_key: String,
        node_key: String,
        fact: NewFact,
        confidence: f64,
    },
    Remove {
        node_key: String,
        confidence: f64,
    },
}
