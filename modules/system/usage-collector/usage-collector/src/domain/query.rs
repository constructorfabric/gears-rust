//! Pre-authorization query request types used between the REST handler and
//! [`Service`](crate::domain::Service).
//!
//! These mirror the SDK's plugin-facing [`AggregationQuery`] / [`RawQuery`]
//! minus the PDP-compiled [`AccessScope`]: the gateway constructs a request at
//! the REST boundary, the domain `Service` calls the PDP, and `with_scope`
//! lifts the request into a fully-scoped plugin query before delegation. The
//! types are intentionally crate-local — plugins and remote SDK consumers only
//! ever see the scoped types from `usage-collector-sdk`.

use chrono::{DateTime, Utc};
use modkit_security::AccessScope;
use usage_collector_sdk::CursorV1;
use usage_collector_sdk::models::{
    AggregationFn, AggregationQuery, BucketSize, GroupByDimension, RawQuery,
};
use uuid::Uuid;

/// Pre-authorization parameters for an aggregated usage query.
///
/// Encoding the authorization boundary in the type system removes the
/// "remember to overwrite `scope`" failure mode that an
/// `AccessScope::default()` placeholder field invited.
#[derive(Debug, Clone)]
pub struct AggregationQueryRequest {
    /// Mandatory time range (from, to).
    pub time_range: (DateTime<Utc>, DateTime<Utc>),
    /// Aggregation function to apply.
    pub function: AggregationFn,
    /// Dimensions to group results by.
    pub group_by: Vec<GroupByDimension>,
    /// Required when `group_by` contains `TimeBucket`.
    pub bucket_size: Option<BucketSize>,
    /// Optional filter: usage type name.
    pub usage_type: Option<String>,
    /// Optional filter: resource UUID.
    pub resource_id: Option<Uuid>,
    /// Optional filter: resource type.
    pub resource_type: Option<String>,
    /// Optional filter: subject UUID.
    pub subject_id: Option<Uuid>,
    /// Optional filter: subject type.
    pub subject_type: Option<String>,
    /// Optional filter: source module name.
    pub source: Option<String>,
}

impl AggregationQueryRequest {
    /// Embed the PDP-compiled [`AccessScope`] and produce the plugin-facing
    /// [`AggregationQuery`].
    ///
    /// Sole constructor for [`AggregationQuery`] inside the gateway — keeping
    /// it that way is what makes "query with default / un-compiled scope"
    /// structurally unrepresentable.
    #[must_use]
    pub fn with_scope(self, scope: AccessScope) -> AggregationQuery {
        AggregationQuery {
            scope,
            time_range: self.time_range,
            function: self.function,
            group_by: self.group_by,
            bucket_size: self.bucket_size,
            usage_type: self.usage_type,
            resource_id: self.resource_id,
            resource_type: self.resource_type,
            subject_id: self.subject_id,
            subject_type: self.subject_type,
            source: self.source,
        }
    }
}

/// Pre-authorization parameters for a raw (unaggregated) usage record query.
///
/// Same role and rationale as [`AggregationQueryRequest`].
#[derive(Debug, Clone)]
pub struct RawQueryRequest {
    /// Mandatory time range (from, to).
    pub time_range: (DateTime<Utc>, DateTime<Utc>),
    /// Optional filter: usage type name.
    pub usage_type: Option<String>,
    /// Optional filter: resource UUID.
    pub resource_id: Option<Uuid>,
    /// Optional filter: resource type.
    pub resource_type: Option<String>,
    /// Optional filter: subject type.
    pub subject_type: Option<String>,
    /// Optional filter: subject UUID.
    pub subject_id: Option<Uuid>,
    /// Pagination cursor (exclusive lower-bound keyset cursor; ascending by
    /// `(timestamp, id)`). `None` for the first page.
    pub cursor: Option<CursorV1>,
    /// Records per page. Validated in `[1, MAX_PAGE_SIZE]` by the gateway.
    pub page_size: u32,
}

impl RawQueryRequest {
    /// Embed the PDP-compiled [`AccessScope`] and produce the plugin-facing
    /// [`RawQuery`] (sole constructor — see
    /// [`AggregationQueryRequest::with_scope`]).
    #[must_use]
    pub fn with_scope(self, scope: AccessScope) -> RawQuery {
        RawQuery {
            scope,
            time_range: self.time_range,
            usage_type: self.usage_type,
            resource_id: self.resource_id,
            resource_type: self.resource_type,
            subject_type: self.subject_type,
            subject_id: self.subject_id,
            cursor: self.cursor,
            page_size: self.page_size,
        }
    }
}
