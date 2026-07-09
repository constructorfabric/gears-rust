//! Foundation REST handlers, grouped by resource. Each handler is a thin
//! pass-through: it pulls the gateway-resolved `SecurityContext`, dispatches
//! to the domain [`crate::domain::Service`], and lifts `UsageCollectorError`
//! through the host-owned canonical mapping. PDP authorization runs inside
//! each `Service` catalog method.

mod usage_records;
mod usage_types;

pub(crate) use usage_records::{
    handle_create_usage_records, handle_deactivate_usage_record, handle_get_usage_record,
    handle_list_usage_records, handle_query_aggregated_usage_records,
};
pub(crate) use usage_types::{
    handle_create_usage_type, handle_delete_usage_type, handle_get_usage_type,
    handle_list_usage_types,
};
