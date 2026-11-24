//! Closed `UPPER_SNAKE` reason tokens of the `InvalidArgument` field violations
//! the gear raises (DESIGN section 3.3, "Error Model": discriminators ride as
//! `errors[].reason` tokens, never as private variants).

/// `tenant_id` is nil.
pub const TENANT_ID_REQUIRED: &str = "TENANT_ID_REQUIRED";
/// `metric` is not a metric type id: a GTS type id derived from
/// `gts.cf.core.quota.metric.v1~`.
pub const METRIC_INVALID: &str = "METRIC_INVALID";
/// A request through the quota resolver plugin carries other than one metric
/// item, or several amounts.
pub const ONE_METRIC_ITEM_REQUIRED: &str = "ONE_METRIC_ITEM_REQUIRED";
/// The metric type does not admit the subject type.
pub const METRIC_NOT_ADMITTED: &str = "METRIC_NOT_ADMITTED";
/// The `id` of a metric object is empty.
pub const METRIC_ID_INVALID: &str = "METRIC_ID_INVALID";
/// The `id` of a metric object is longer than the bound.
pub const METRIC_ID_TOO_LONG: &str = "METRIC_ID_TOO_LONG";
/// A request names no subject.
pub const SUBJECTS_REQUIRED: &str = "SUBJECTS_REQUIRED";
/// A request names more subjects than the bound.
pub const SUBJECTS_TOO_MANY: &str = "SUBJECTS_TOO_MANY";
/// A subject id is empty.
pub const SUBJECT_ID_REQUIRED: &str = "SUBJECT_ID_REQUIRED";
/// A subject id is longer than the bound.
pub const SUBJECT_ID_TOO_LONG: &str = "SUBJECT_ID_TOO_LONG";
/// A subject type is not a quota subject type id: a GTS type id derived from
/// `gts.cf.core.quota.subj.v1~`.
pub const SUBJECT_TYPE_INVALID: &str = "SUBJECT_TYPE_INVALID";
/// Two subjects carry the same type.
pub const SUBJECT_TYPE_DUPLICATE: &str = "SUBJECT_TYPE_DUPLICATE";
/// A tenant-type subject names a tenant other than the request's.
pub const TENANT_SUBJECT_MISMATCH: &str = "TENANT_SUBJECT_MISMATCH";
/// A subject type is abstract.
pub const SUBJECT_TYPE_ABSTRACT: &str = "SUBJECT_TYPE_ABSTRACT";
/// A subject does not fit the schema of its subject type.
pub const SUBJECT_INVALID: &str = "SUBJECT_INVALID";
/// The metric object carries no `metadata`.
pub const METADATA_REQUIRED: &str = "METADATA_REQUIRED";
/// The metric object does not fit the resolved schema of its metric type.
pub const CONTRACT_VIOLATION: &str = "CONTRACT_VIOLATION";

// --- quota lifecycle ---

/// The `kind` of the metric type does not take the quota type: a `counter`
/// metric takes consumption and rate Quotas, a `gauge` metric allocation
/// Quotas.
pub const QUOTA_TYPE_KIND_MISMATCH: &str = "QUOTA_TYPE_KIND_MISMATCH";
/// A patch names an immutable field (`metric`, `quota_type`, `period`,
/// `subject`); a breaking change is a deactivate followed by a create.
pub const IMMUTABLE_FIELD: &str = "IMMUTABLE_FIELD";
/// A patch changes nothing.
pub const PATCH_EMPTY: &str = "PATCH_EMPTY";
/// A consumption Quota carries no period.
pub const PERIOD_REQUIRED: &str = "PERIOD_REQUIRED";
/// An allocation Quota carries a period field, `null` included.
pub const PERIOD_NOT_ALLOWED: &str = "PERIOD_NOT_ALLOWED";
/// A notification threshold is outside `1..=100`.
pub const THRESHOLD_OUT_OF_RANGE: &str = "THRESHOLD_OUT_OF_RANGE";
/// Notification thresholds are not strictly ascending.
pub const THRESHOLDS_NOT_ASCENDING: &str = "THRESHOLDS_NOT_ASCENDING";
/// A validity window ends before it starts.
pub const VALIDITY_WINDOW_INVERTED: &str = "VALIDITY_WINDOW_INVERTED";
/// The canonical JSON of `metadata` exceeds the configured size limit.
pub const METADATA_TOO_LARGE: &str = "METADATA_TOO_LARGE";
/// A list filter names one half of the subject only.
pub const LIST_SUBJECT_INCOMPLETE: &str = "LIST_SUBJECT_INCOMPLETE";
/// A list filter names more ids than the configured maximum.
pub const LIST_TOO_MANY_IDS: &str = "LIST_TOO_MANY_IDS";
/// A list page limit is zero or above the configured maximum.
pub const LIST_LIMIT_OUT_OF_RANGE: &str = "LIST_LIMIT_OUT_OF_RANGE";
/// A status filter is neither `active` nor `deactivated`.
pub const STATUS_INVALID: &str = "STATUS_INVALID";
/// A cap above `Quota::MAX_CAP`; caps live in `0..=i64::MAX`.
pub const CAP_OUT_OF_RANGE: &str = "CAP_OUT_OF_RANGE";
/// A list continuation cursor that storage does not decode.
pub const CURSOR_INVALID: &str = "CURSOR_INVALID";

// --- consumption operations ---

/// The requested amount is zero or negative.
pub const INVALID_AMOUNT: &str = "INVALID_AMOUNT";
/// A write operation carried no idempotency key.
pub const IDEMPOTENCY_KEY_REQUIRED: &str = "IDEMPOTENCY_KEY_REQUIRED";
/// An idempotency key is longer than the bound.
pub const IDEMPOTENCY_KEY_TOO_LONG: &str = "IDEMPOTENCY_KEY_TOO_LONG";
/// No committed operation is registered under the key the caller named.
pub const UNKNOWN_OPERATION: &str = "UNKNOWN_OPERATION";
/// No Quota exists under the identifier the caller named.
pub const UNKNOWN_QUOTA: &str = "UNKNOWN_QUOTA";

// --- lease operations ---

/// A lease `ttl` is missing or outside the configured `[min, max]` window. It
/// is never clamped: the holder is entitled to exactly the TTL it asked for.
pub const TTL_OUT_OF_BOUNDS: &str = "TTL_OUT_OF_BOUNDS";

// --- batch debit ---

/// A batch carries no items.
pub const BATCH_EMPTY: &str = "BATCH_EMPTY";
/// A batch's items name different tenants; the envelope has one.
pub const BATCH_TENANT_MIXED: &str = "BATCH_TENANT_MIXED";
/// Two items of one batch carry the same idempotency key.
pub const BATCH_ITEM_KEY_DUPLICATE: &str = "BATCH_ITEM_KEY_DUPLICATE";
/// A batch has more items than the configured maximum.
pub const BULK_TOO_LARGE: &str = "BULK_TOO_LARGE";
/// Two items of one bulk envelope name the same Quota.
pub const BULK_QUOTA_DUPLICATE: &str = "BULK_QUOTA_DUPLICATE";
/// A snapshot read names no subject.
pub const SNAPSHOT_SUBJECTS_REQUIRED: &str = "SNAPSHOT_SUBJECTS_REQUIRED";
/// A snapshot read names more subjects than the operator allows.
pub const SNAPSHOT_TOO_MANY_SUBJECTS: &str = "SNAPSHOT_TOO_MANY_SUBJECTS";
/// A snapshot page size outside `1..=page_size`.
pub const SNAPSHOT_LIMIT_OUT_OF_RANGE: &str = "SNAPSHOT_LIMIT_OUT_OF_RANGE";
/// A tenant-type snapshot subject names a tenant other than the request's.
pub const SNAPSHOT_TENANT_MISMATCH: &str = "SNAPSHOT_TENANT_MISMATCH";
