# Serverless Runtime: Rust Domain Types and Runtime Traits

> **Companion file to [DESIGN.md](./DESIGN.md) section 3.1 "Rust Domain Types and Runtime Traits".**
>
> This file contains the complete Rust type definitions and trait interfaces for the
> Serverless Runtime domain model. These types are transport-agnostic and intended for
> SDK or core runtime crates. The abstract `ServerlessRuntime` trait can be implemented
> by adapters (Temporal, Starlark, cloud FaaS).

##### Core Types (Rust)

```rust
use time::OffsetDateTime;
use serde_json::Value as JsonValue;

pub type GtsId = String;
pub type FunctionId = String;
pub type InvocationId = String;
pub type ScheduleId = String;
pub type TriggerId = String;
pub type TenantId = String;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionStatus {
    Draft,
    Active,
    Deprecated,
    Disabled,
    Archived,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationMode {
    Sync,
    Async,
}

/// Invocation lifecycle status. Matches the short enum values in the
/// `gts.x.core.serverless.status.v1~` GTS schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationStatus {
    Queued,
    Running,
    Suspended,
    Succeeded,
    Failed,
    Canceled,
    Compensating,
    Compensated,
    DeadLettered,
}

/// Function kind derived from GTS chain (not stored, computed from function_id).
/// In the 2-tier hierarchy, all types contain `function.v1~` in the chain;
/// workflows additionally contain `workflow.v1~`. Check workflow first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    Function,
    Workflow,
}

impl FunctionKind {
    /// Determines function kind by checking the GTS chain for known base types.
    /// Workflow is checked first because all types contain `function.v1~` in the
    /// new 2-tier hierarchy (function base → workflow derived).
    pub fn from_gts_id(function_id: &str) -> Option<Self> {
        if function_id.contains("x.core.serverless.workflow.") {
            Some(FunctionKind::Workflow)
        } else if function_id.contains("x.core.serverless.function.") {
            Some(FunctionKind::Function)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConcurrencyPolicy {
    Allow,
    Forbid,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissedSchedulePolicy {
    Skip,
    CatchUp,
    Backfill,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImplementationKind {
    Code,
    WorkflowSpec,
    AdapterRef,
}

#[derive(Clone, Debug)]
pub struct FunctionSchema {
    pub params: JsonValue,
    pub returns: JsonValue,
    pub errors: Vec<GtsId>,
}

#[derive(Clone, Debug)]
pub struct FunctionTraits {
    pub supported_invocations: Vec<InvocationMode>,
    pub default_invocation: InvocationMode,
    /// When false, the function can only be invoked internally via r_invoke_v1().
    pub entrypoint: bool,
    pub is_idempotent: bool,
    pub caching: ResponseCachingPolicy,
    pub rate_limit: Option<RateLimit>,
    pub limits: FunctionLimits,
    pub retry: RetryPolicy,
    pub workflow: Option<WorkflowTraits>,
}

/// Response caching policy for a function (BR-118, BR-132).
///
/// Caching is only active when **all** conditions are met:
/// 1. The caller provides an `Idempotency-Key` header.
/// 2. `max_age_seconds > 0`.
/// 3. The function's `is_idempotent` trait is `true`.
///
/// Cache key depends on function owner type:
/// - `user` owner: `(subject_id, function_id, function_version, idempotency_key)`
/// - `tenant`/`system` owner: `(tenant_id, function_id, function_version, idempotency_key)`
///
/// Cache scope is per function owner — never shared across tenants.
/// Only successful (`succeeded`) results are cached.
#[derive(Clone, Debug)]
pub struct ResponseCachingPolicy {
    /// TTL in seconds for cached successful results. `0` disables caching.
    pub max_age_seconds: u64,
}

/// Function-level rate limiting reference. Enforced per-function
/// per-tenant (isolated across tenants, aggregated across users within tenant).
/// Applies to both sync and async invocation modes.
///
/// `strategy` is the GTS type ID of the rate limiter plugin (derived from
/// `gts.x.core.serverless.rate_limit.v1~`); `config` is the strategy-specific
/// settings as an opaque JSON object validated by the resolved plugin.
#[derive(Clone, Debug)]
pub struct RateLimit {
    /// GTS type ID of the rate limiting strategy. The runtime resolves
    /// the rate limiter plugin from this value.
    pub strategy: GtsId,
    /// Strategy-specific configuration. Opaque to the platform; the resolved
    /// plugin deserializes this into its own config type.
    pub config: serde_json::Value,
}

/// System-default token bucket rate limiter configuration.
/// GTS ID: gts.x.core.serverless.rate_limit.v1~x.core.serverless.rate_limit.token_bucket.v1~
///
/// Both per-second and per-minute limits are enforced independently.
/// `burst_size` applies to the per-second bucket only.
#[derive(Clone, Debug)]
pub struct TokenBucketRateLimit {
    /// Maximum sustained invocations per second. `0` = no per-second limit.
    pub max_requests_per_second: f64,
    /// Maximum sustained invocations per minute. `0` = no per-minute limit.
    pub max_requests_per_minute: u64,
    /// Maximum instantaneous burst for the per-second bucket.
    pub burst_size: u64,
}

/// Admission decision returned by a `RateLimiter` plugin.
#[derive(Clone, Debug)]
pub enum RateLimitDecision {
    /// Request is allowed.
    Allow,
    /// Request is rejected. `retry_after_seconds` is the suggested wait time.
    Reject { retry_after_seconds: u64 },
}

/// Plugin trait for rate limiting. Each plugin handles exactly one strategy
/// GTS type. The runtime resolves the plugin based on `rate_limit.strategy`
/// and passes the opaque `config` for admission checks.
///
/// The default system implementation handles `token_bucket.v1~` using an
/// in-process token bucket. Custom plugins may implement distributed rate
/// limiting (e.g., Redis-backed), sliding window, or adaptive throttling.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// The single GTS type ID this plugin handles.
    fn strategy_type(&self) -> &GtsId;

    /// Check whether an invocation should be admitted.
    async fn check(
        &self,
        ctx: &SecurityContext,
        function_id: &FunctionId,
        config: &serde_json::Value,
    ) -> RateLimitDecision;
}

#[derive(Clone, Debug)]
pub struct WorkflowTraits {
    pub compensation: CompensationConfig,
    pub checkpointing: CheckpointingConfig,
    pub max_suspension_days: u64,
}

#[derive(Clone, Debug)]
pub struct CompensationConfig {
    /// GTS ID of function to invoke on workflow failure, or None for no compensation.
    pub on_failure: Option<FunctionId>,
    /// GTS ID of function to invoke on workflow cancellation, or None for no compensation.
    pub on_cancel: Option<FunctionId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointingStrategy {
    Automatic,
    Manual,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct CheckpointingConfig {
    pub strategy: CheckpointingStrategy,
}

/// Base limits; adapters may extend with additional fields (memory_mb, cpu, etc.)
#[derive(Clone, Debug)]
pub struct FunctionLimits {
    pub timeout_seconds: u64,
    pub max_concurrent: u64,
    /// Adapter-specific limits (e.g., memory_mb, cpu for Starlark adapter)
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f32,
    pub non_retryable_errors: Vec<GtsId>,
}

/// Implementation with explicit adapter for limits validation.
#[derive(Clone, Debug)]
pub struct FunctionImplementation {
    /// GTS type ID of the adapter (e.g., gts.x.core.serverless.adapter.starlark.v1~)
    pub adapter: GtsId,
    pub kind: ImplementationKind,
    pub payload: ImplementationPayload,
}

#[derive(Clone, Debug)]
pub enum ImplementationPayload {
    Code { language: String, source: String },
    WorkflowSpec { format: String, spec: JsonValue },
    AdapterRef { definition_id: String },
}

/// Function definition. Identity is the GTS instance address (external).
/// Function type (function/workflow) is derived from the GTS chain.
#[derive(Clone, Debug)]
pub struct FunctionDefinition {
    pub version: String,
    pub tenant_id: TenantId,
    /// Owner determines default visibility (per PRD BR-002):
    /// - User-scoped: private by default
    /// - Tenant-scoped: visible to tenant users by default
    /// - System: platform-provided
    pub owner: OwnerRef,
    pub status: DefinitionStatus,
    pub tags: Vec<String>,
    pub title: String,
    pub description: String,
    pub schema: FunctionSchema,
    pub traits: FunctionTraits,
    pub implementation: FunctionImplementation,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerType {
    /// User-scoped: private to the owning user by default.
    User,
    /// Tenant-scoped: visible to authorized users within the tenant by default.
    Tenant,
    /// System-provided: managed by the platform.
    System,
}

#[derive(Clone, Debug)]
pub struct OwnerRef {
    pub owner_type: OwnerType,
    pub id: String,
    pub tenant_id: TenantId,
}

#[derive(Clone, Debug)]
pub struct InvocationRecord {
    pub invocation_id: InvocationId,
    /// GTS type ID; function type (function/workflow) is derived from the chain.
    pub function_id: FunctionId,
    pub function_version: String,
    pub tenant_id: TenantId,
    pub status: InvocationStatus,
    pub mode: InvocationMode,
    pub params: JsonValue,
    pub result: Option<JsonValue>,
    pub error: Option<RuntimeErrorPayload>,
    pub timestamps: InvocationTimestamps,
    pub observability: InvocationObservability,
}

#[derive(Clone, Debug)]
pub struct InvocationTimestamps {
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub suspended_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug)]
pub struct InvocationObservability {
    pub correlation_id: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub metrics: InvocationMetrics,
}

#[derive(Clone, Debug)]
pub struct InvocationMetrics {
    pub duration_ms: Option<u64>,
    pub billed_duration_ms: Option<u64>,
    pub cpu_time_ms: Option<u64>,
    pub memory_limit_mb: u64,
    pub max_memory_used_mb: Option<u64>,
    pub step_count: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Schedule {
    pub schedule_id: ScheduleId,
    pub tenant_id: TenantId,
    pub function_id: FunctionId,
    pub name: String,
    pub timezone: String,
    pub expression: ScheduleExpression,
    pub input_overrides: JsonValue,
    pub missed_policy: MissedSchedulePolicy,
    pub max_catch_up_runs: u32,
    pub execution_context: String,
    pub concurrency_policy: ConcurrencyPolicy,
    pub enabled: bool,
    pub next_run_at: Option<OffsetDateTime>,
    pub last_run_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct ScheduleExpression {
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct Trigger {
    pub trigger_id: TriggerId,
    pub tenant_id: TenantId,
    /// GTS event type ID to listen for.
    pub event_type_id: GtsId,
    /// Filter expression (CEL subset).
    pub event_filter_query: Option<String>,
    pub function_id: FunctionId,
    pub dead_letter_queue: Option<DeadLetterQueueConfig>,
    pub batch: Option<BatchConfig>,
    pub execution_context: String,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct BatchConfig {
    pub enabled: bool,
    pub max_size: u32,
    pub max_wait_ms: u32,
}

#[derive(Clone, Debug)]
pub struct DeadLetterQueueConfig {
    pub enabled: bool,
    /// Retry policy before moving to DLQ.
    pub retry_policy: RetryPolicy,
    /// GTS type ID of the topic to publish dead-lettered events to,
    /// or None for the platform-default DLQ topic. Topic type and
    /// management are defined by the EventBroker.
    pub dlq_topic: Option<GtsId>,
}

#[derive(Clone, Debug)]
pub struct TenantRuntimePolicy {
    pub tenant_id: TenantId,
    pub enabled: bool,
    pub quotas: TenantQuotas,
    pub retention: TenantRetention,
    pub policies: TenantPolicies,
    pub idempotency: TenantIdempotency,
    pub defaults: TenantDefaults,
}

#[derive(Clone, Debug)]
pub struct TenantQuotas {
    pub max_concurrent_executions: u64,
    pub max_definitions: u64,
    pub max_schedules: u64,
    pub max_triggers: u64,
    pub max_execution_history_mb: u64,
    pub max_memory_per_execution_mb: u64,
    pub max_cpu_per_execution: f32,
    pub max_execution_duration_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct TenantRetention {
    pub execution_history_days: u64,
    pub audit_log_days: u64,
}

#[derive(Clone, Debug)]
pub struct TenantPolicies {
    /// Allowed adapter GTS type IDs (e.g., gts.x.core.serverless.adapter.starlark.v1~).
    /// Validated against `implementation.adapter` at function registration time.
    pub allowed_runtimes: Vec<GtsId>,
    /// When true, function publishing requires administrative approval.
    pub require_approval_for_publish: bool,
    /// Domain allowlist for outbound HTTP calls.
    pub allowed_outbound_domains: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct TenantIdempotency {
    pub deduplication_window_seconds: u64,
}

/// Default limits for new functions (base limits only; adapters may add more).
#[derive(Clone, Debug)]
pub struct TenantDefaults {
    pub timeout_seconds: u64,
    pub memory_mb: u64,
    pub cpu: f32,
}
```

##### Runtime Errors

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeErrorCategory {
    Retryable,
    NonRetryable,
    ResourceLimit,
    Timeout,
    Canceled,
}

#[derive(Clone, Debug)]
pub struct RuntimeErrorPayload {
    /// GTS error type ID (e.g., gts.x.core.serverless.err.v1~x.core.serverless.err.validation.v1~)
    pub error_type_id: GtsId,
    pub message: String,
    pub category: RuntimeErrorCategory,
    pub details: serde_json::Value,
}
```

##### Abstract Runtime Interface

```rust
use async_trait::async_trait;
use modkit_security::SecurityContext;

#[derive(Clone, Debug)]
pub struct InvocationRequest {
    pub function_id: FunctionId,
    pub mode: InvocationMode,
    pub params: serde_json::Value,
    pub dry_run: bool,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct InvocationResult {
    pub record: InvocationRecord,
    /// `true` when the result was produced by a dry-run invocation.
    /// The embedded record is synthetic and was not persisted.
    pub dry_run: bool,
    /// `true` when the result was served from the response cache (cache hit).
    /// The embedded record is the original record from the execution that
    /// produced the cached result. No new invocation was created.
    pub cached: bool,
}

/// Actions for function lifecycle status transitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionStatusAction {
    /// Mark function as deprecated (still callable but discouraged).
    Deprecate,
    /// Disable function (not callable, can be re-enabled).
    Disable,
    /// Re-enable a disabled function.
    Enable,
    /// Activate a draft function.
    Activate,
    /// Archive a deprecated or disabled function for historical reference.
    Archive,
}

/// Control actions for invocation lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationControlAction {
    /// Cancel a running or queued invocation.
    Cancel,
    /// Suspend a running invocation (workflow only).
    Suspend,
    /// Resume a suspended invocation.
    Resume,
    /// Retry a failed invocation with same parameters.
    Retry,
    /// Replay a completed invocation (create new invocation with same parameters).
    Replay,
}

#[async_trait]
pub trait ServerlessRuntime: Send + Sync {
    async fn register_function(
        &self,
        ctx: &SecurityContext,
        function: FunctionDefinition,
    ) -> Result<FunctionDefinition, RuntimeErrorPayload>;

    async fn validate_function(
        &self,
        ctx: &SecurityContext,
        function: FunctionDefinition,
    ) -> Result<(), RuntimeErrorPayload>;

    async fn list_functions(
        &self,
        ctx: &SecurityContext,
        filter: FunctionListFilter,
    ) -> Result<Vec<FunctionDefinition>, RuntimeErrorPayload>;

    async fn get_function(
        &self,
        ctx: &SecurityContext,
        function_id: &FunctionId,
    ) -> Result<FunctionDefinition, RuntimeErrorPayload>;

    async fn update_function(
        &self,
        ctx: &SecurityContext,
        function_id: &FunctionId,
        function: FunctionDefinition,
    ) -> Result<FunctionDefinition, RuntimeErrorPayload>;

    async fn update_function_status(
        &self,
        ctx: &SecurityContext,
        function_id: &FunctionId,
        action: FunctionStatusAction,
    ) -> Result<FunctionDefinition, RuntimeErrorPayload>;

    async fn delete_function(
        &self,
        ctx: &SecurityContext,
        function_id: &FunctionId,
    ) -> Result<(), RuntimeErrorPayload>;

    async fn start_invocation(
        &self,
        ctx: &SecurityContext,
        request: InvocationRequest,
    ) -> Result<InvocationResult, RuntimeErrorPayload>;

    async fn get_invocation(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
    ) -> Result<InvocationRecord, RuntimeErrorPayload>;

    async fn control_invocation(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
        action: InvocationControlAction,
    ) -> Result<InvocationRecord, RuntimeErrorPayload>;

    async fn list_invocations(
        &self,
        ctx: &SecurityContext,
        filter: InvocationListFilter,
    ) -> Result<Vec<InvocationRecord>, RuntimeErrorPayload>;

    async fn get_invocation_timeline(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
    ) -> Result<Vec<InvocationTimelineEvent>, RuntimeErrorPayload>;

    async fn create_schedule(
        &self,
        ctx: &SecurityContext,
        schedule: Schedule,
    ) -> Result<Schedule, RuntimeErrorPayload>;

    async fn list_schedules(
        &self,
        ctx: &SecurityContext,
        filter: ScheduleListFilter,
    ) -> Result<Vec<Schedule>, RuntimeErrorPayload>;

    async fn get_schedule(
        &self,
        ctx: &SecurityContext,
        schedule_id: &ScheduleId,
    ) -> Result<Schedule, RuntimeErrorPayload>;

    async fn patch_schedule(
        &self,
        ctx: &SecurityContext,
        schedule_id: &ScheduleId,
        patch: SchedulePatch,
    ) -> Result<Schedule, RuntimeErrorPayload>;

    async fn pause_schedule(
        &self,
        ctx: &SecurityContext,
        schedule_id: &ScheduleId,
    ) -> Result<Schedule, RuntimeErrorPayload>;

    async fn resume_schedule(
        &self,
        ctx: &SecurityContext,
        schedule_id: &ScheduleId,
    ) -> Result<Schedule, RuntimeErrorPayload>;

    async fn delete_schedule(
        &self,
        ctx: &SecurityContext,
        schedule_id: &ScheduleId,
    ) -> Result<(), RuntimeErrorPayload>;

    async fn get_schedule_history(
        &self,
        ctx: &SecurityContext,
        schedule_id: &ScheduleId,
    ) -> Result<Vec<InvocationRecord>, RuntimeErrorPayload>;

    async fn create_trigger(
        &self,
        ctx: &SecurityContext,
        trigger: Trigger,
    ) -> Result<Trigger, RuntimeErrorPayload>;

    async fn list_triggers(
        &self,
        ctx: &SecurityContext,
        filter: TriggerListFilter,
    ) -> Result<Vec<Trigger>, RuntimeErrorPayload>;

    async fn get_trigger(
        &self,
        ctx: &SecurityContext,
        trigger_id: &TriggerId,
    ) -> Result<Trigger, RuntimeErrorPayload>;

    async fn update_trigger(
        &self,
        ctx: &SecurityContext,
        trigger_id: &TriggerId,
        patch: TriggerPatch,
    ) -> Result<Trigger, RuntimeErrorPayload>;

    async fn delete_trigger(
        &self,
        ctx: &SecurityContext,
        trigger_id: &TriggerId,
    ) -> Result<(), RuntimeErrorPayload>;

    async fn get_tenant_runtime_policy(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
    ) -> Result<TenantRuntimePolicy, RuntimeErrorPayload>;

    async fn update_tenant_runtime_policy(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
        policy: TenantRuntimePolicy,
    ) -> Result<TenantRuntimePolicy, RuntimeErrorPayload>;

    async fn get_tenant_usage(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
    ) -> Result<TenantUsage, RuntimeErrorPayload>;

    async fn get_tenant_usage_history(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
        filter: UsageHistoryFilter,
    ) -> Result<Vec<TenantUsage>, RuntimeErrorPayload>;
}
```

##### Additional Rust Types

```rust
/// GTS ID: gts.x.core.serverless.err.v1~x.core.serverless.err.validation.v1~
/// Validation error extending base error, containing multiple issues.
/// Returned only when validation fails; success returns the validated definition.
#[derive(Clone, Debug)]
pub struct ValidationError {
    pub message: String,
    pub category: RuntimeErrorCategory,
    pub details: Option<serde_json::Value>,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Clone, Debug)]
pub struct ValidationIssue {
    pub error_type: String,
    pub location: ValidationLocation,
    pub message: String,
    pub suggestion: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ValidationLocation {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct FunctionListFilter {
    pub tenant_id: Option<TenantId>,
    pub function_id_prefix: Option<String>,
    pub status: Option<DefinitionStatus>,
    pub owner_type: Option<OwnerType>,
    pub runtime: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct InvocationListFilter {
    pub tenant_id: Option<TenantId>,
    pub function_id: Option<FunctionId>,
    pub status: Option<InvocationStatus>,
    pub mode: Option<InvocationMode>,
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ScheduleListFilter {
    pub tenant_id: Option<TenantId>,
    pub function_id: Option<FunctionId>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct TriggerListFilter {
    pub tenant_id: Option<TenantId>,
    pub event_type_id: Option<GtsId>,
    pub function_id: Option<FunctionId>,
}

#[derive(Clone, Debug)]
pub struct TriggerPatch {
    pub event_type_id: Option<GtsId>,
    pub event_filter_query: Option<String>,
    pub function_id: Option<FunctionId>,
    pub dead_letter_queue: Option<DeadLetterQueueConfig>,
    pub batch: Option<BatchConfig>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct SchedulePatch {
    pub name: Option<String>,
    pub timezone: Option<String>,
    pub expression: Option<ScheduleExpression>,
    pub input_overrides: Option<JsonValue>,
    pub missed_policy: Option<MissedSchedulePolicy>,
    pub max_catch_up_runs: Option<u32>,
    pub concurrency_policy: Option<ConcurrencyPolicy>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct TenantUsage {
    pub tenant_id: TenantId,
    pub timestamp: OffsetDateTime,
    pub current: UsageMetrics,
    pub quotas: TenantQuotas,
    pub utilization_percent: UsageUtilization,
}

#[derive(Clone, Debug)]
pub struct UsageMetrics {
    pub concurrent_executions: u64,
    pub total_definitions: u64,
    pub total_schedules: u64,
    pub total_triggers: u64,
    pub execution_history_mb: u64,
}

#[derive(Clone, Debug)]
pub struct UsageUtilization {
    pub concurrent_executions: f64,
    pub definitions: f64,
    pub schedules: f64,
    pub triggers: f64,
    pub execution_history: f64,
}

#[derive(Clone, Debug, Default)]
pub struct UsageHistoryFilter {
    pub start_time: Option<OffsetDateTime>,
    pub end_time: Option<OffsetDateTime>,
    pub granularity: Option<UsageGranularity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageGranularity {
    Hourly,
    Daily,
    Weekly,
}

/// GTS ID: gts.x.core.serverless.timeline_event.v1~
#[derive(Clone, Debug)]
pub struct InvocationTimelineEvent {
    pub at: OffsetDateTime,
    pub event_type: TimelineEventType,
    pub status: InvocationStatus,
    pub step_name: Option<String>,
    pub duration_ms: Option<u64>,
    pub message: Option<String>,
    pub details: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineEventType {
    Started,
    StepStarted,
    StepCompleted,
    StepFailed,
    StepRetried,
    Suspended,
    Resumed,
    SignalReceived,
    CheckpointCreated,
    CompensationStarted,
    CompensationCompleted,
    CompensationFailed,
    Succeeded,
    Failed,
    Canceled,
    DeadLettered,
}
```
