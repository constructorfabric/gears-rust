//! The connector record types Construct takes at intake.
//!
//! The schemas under `schemas/` are byte-for-byte copies of the connector
//! boundary schemas in rolos-cyber (`docs/construct/GTS/schemas/`, ADR 0003),
//! which stay the source of truth. They are submitted to the link-time schema
//! inventory, which the types registry drains when it starts: the connectors
//! are not Rust and cannot register their types at link time, and the
//! in-host types registry fills only at boot.
//!
//! @cpt-dod:cpt-cf-construct-dod-record-intake-types:p1

use toolkit_gts::InventoryTypeSchema;

/// The abstract base every connector record type derives from. It fixes the
/// envelope: `type`, `provenance`, `version`, `observed_at`, the optional
/// `subject_id`, and the `payload` a derived type refines.
pub const RECORD_BASE_TYPE: &str = "gts.cf.connectors.core.record.v1~";

/// A raw chat message from the chat export.
pub const CHAT_MESSAGE_TYPE: &str =
    "gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~";

/// A fact candidate from an enrichment connector.
pub const ENRICHMENT_FACT_CANDIDATE_TYPE: &str =
    "gts.cf.connectors.core.record.v1~cf.construct.enrichment.fact_candidate.v1~";

/// A course catalog entry from the mastery connector.
pub const MASTERY_COURSE_CATALOG_TYPE: &str =
    "gts.cf.connectors.core.record.v1~cf.construct.mastery.course_catalog.v1~";

/// A student's mastery from the mastery connector.
pub const MASTERY_STUDENT_MASTERY_TYPE: &str =
    "gts.cf.connectors.core.record.v1~cf.construct.mastery.student_mastery.v1~";

/// The schema of the record base type: the envelope every record must pass.
pub const RECORD_BASE_SCHEMA: &str =
    include_str!("../schemas/gts.cf.connectors.core.record.v1~.schema.json");

/// One record type this crate registers: its type id and its schema file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordTypeSchema {
    pub type_id: &'static str,
    pub schema: &'static str,
}

/// Every record type this crate registers: the base and its derived types.
pub const RECORD_TYPES: [RecordTypeSchema; 5] = [
    RecordTypeSchema {
        type_id: RECORD_BASE_TYPE,
        schema: RECORD_BASE_SCHEMA,
    },
    RecordTypeSchema {
        type_id: CHAT_MESSAGE_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~.schema.json"
        ),
    },
    RecordTypeSchema {
        type_id: ENRICHMENT_FACT_CANDIDATE_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.connectors.core.record.v1~cf.construct.enrichment.fact_candidate.v1~.schema.json"
        ),
    },
    RecordTypeSchema {
        type_id: MASTERY_COURSE_CATALOG_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.connectors.core.record.v1~cf.construct.mastery.course_catalog.v1~.schema.json"
        ),
    },
    RecordTypeSchema {
        type_id: MASTERY_STUDENT_MASTERY_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.connectors.core.record.v1~cf.construct.mastery.student_mastery.v1~.schema.json"
        ),
    },
];

macro_rules! submit_record_type {
    ($index:literal) => {
        toolkit_gts::inventory::submit! {
            InventoryTypeSchema {
                type_id: RECORD_TYPES[$index].type_id,
                schema_fn: || RECORD_TYPES[$index].schema.to_owned(),
            }
        }
    };
}

submit_record_type!(0);
submit_record_type!(1);
submit_record_type!(2);
submit_record_type!(3);
submit_record_type!(4);

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
