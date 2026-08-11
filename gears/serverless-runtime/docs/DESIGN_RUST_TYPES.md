<!--
Created:  2026-03-12 by Constructor Tech
Updated:  2026-07-30 by Constructor Tech
-->

# Serverless Runtime: Rust Domain Types and Runtime Traits

> **Companion file to [DESIGN.md](./DESIGN.md) section 3.1 "Rust Domain Types and Runtime Traits".**
>
> This file contains the complete Rust type definitions and trait interfaces for the
> Serverless Runtime domain model. These types are transport-agnostic.
>
> **Four traits, four audiences.** Per ADR `cpt-cf-serverless-runtime-adr-thin-host` and the
> 2026-07-30 amendment recording the SDK split:
>
> | Trait | Declared in | Implemented by | Called by |
> |---|---|---|---|
> | `ServerlessRuntimeService` | the host crate (internal) | the host | the host's own REST / JSON-RPC / MCP handlers |
> | `ServerlessRuntimeClientV1` | `serverless-runtime-sdk` | the host | other gears |
> | `RuntimeAdapter` | `serverless-runtime-plugin-sdk` | each runtime plugin | the host |
> | `InvocationIndexPort` | `serverless-runtime-plugin-sdk` | the host | runtime plugins |
>
> `ServerlessRuntimeService` is deliberately **not** named `*ClientV1` and is **not** a
> `ClientHub` contract: nothing outside the host crate resolves it. It is the full internal
> surface the transports are built on, which is why it carries registry, schedule, trigger and
> tenant-policy CRUD that the published consumer trait does not.
>
> `ServerlessRuntimeClientV1` is the narrower published subset — invocation and observation only
> (`serverless-sdk/docs/PRD.md` §4, `serverless-sdk/docs/DESIGN.md` §3.3). Where the two overlap
> they differ on purpose, and neither difference is a defect: the internal service returns an
> `InvocationSummary` from `control_invocation` because its callers render it immediately, while
> the published trait returns nothing there and exposes replay as its own operation so a caller
> receives the new invocation id. Each Runtime Plugin (Temporal, Starlark, cloud FaaS, …) provides
> one `RuntimeAdapter` impl bound to a single GTS adapter type.

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
/// `gts.cf.core.sless.status.v1~` GTS schema.
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

/// Callable kind derived from the GTS chain (not stored, computed from
/// `function_id`). `Function` and `Workflow` are sibling base types — neither
/// derives from the other — so the chain contains exactly one of
/// `function.v1~` or `workflow.v1~` at the base position; both branches are
/// matched independently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    Function,
    Workflow,
}

impl FunctionKind {
    /// Determines callable kind by matching the sibling base type in the GTS
    /// chain. The two branches are mutually exclusive in the sibling-type
    /// model; check order is therefore irrelevant.
    pub fn from_gts_id(function_id: &str) -> Option<Self> {
        if function_id.contains("cf.core.sless.workflow.") {
            Some(FunctionKind::Workflow)
        } else if function_id.contains("cf.core.sless.function.") {
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
/// `gts.cf.core.sless.rate_limit.v1~`); `config` is the strategy-specific
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
/// GTS ID: gts.cf.core.sless.rate_limit.v1~cf.core.sless.rate_limit.token_bucket.v1~
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
    /// GTS type ID of the adapter (e.g., gts.cf.core.sless.runtime.starlark.v1~)
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

/// The full invocation record. Owned by the Runtime Plugin, which is the system of
/// record for it. The host does **not** persist this — see [`InvocationSummary`] for
/// what the host index holds, and reach the full record through the plugin trait.
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

/// One row of the host invocation index — the lightweight, queryable projection the
/// host persists from plugin-emitted events (ADR `cpt-cf-serverless-runtime-adr-thin-host`).
/// This is what host-trait reads return: answering them never requires a plugin
/// round-trip. Fields absent here (`params`, `result`, full observability, the timeline)
/// live only with the owning plugin.
#[derive(Clone, Debug)]
pub struct InvocationSummary {
    pub invocation_id: InvocationId,
    pub function_id: FunctionId,
    /// GTS adapter type of the plugin that ran the invocation.
    pub adapter: GtsId,
    pub tenant_id: TenantId,
    pub owner: OwnerRef,
    pub status: InvocationStatus,
    pub timestamps: InvocationTimestamps,
    /// Populated when `status` is a failure state.
    pub error_summary: Option<InvocationErrorSummary>,
}

/// Redacted failure data stored in the consumer-readable host index.
/// Backend messages and arbitrary details remain plugin-local.
#[derive(Clone, Debug)]
pub struct InvocationErrorSummary {
    pub error_type_id: GtsId,
    pub category: InvocationErrorCategory,
}

/// Consumer-facing retry classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationErrorCategory {
    Retryable,
    NonRetryable,
    ResourceLimit,
    Timeout,
    Canceled,
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
    /// Allowed adapter GTS type IDs (e.g., gts.cf.core.sless.runtime.starlark.v1~).
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
    /// GTS error type ID (e.g., gts.cf.core.sless.err.v1~cf.core.sless.err.validation.v1~)
    pub error_type_id: GtsId,
    pub message: String,
    pub category: RuntimeErrorCategory,
    pub details: serde_json::Value,
}

/// Redacted plugin-to-host index update. The host maps this into the consumer SDK's
/// `InvocationErrorSummary`; neither SDK depends on the other.
#[derive(Clone, Debug)]
pub struct InvocationIndexError {
    pub error_type_id: GtsId,
    pub category: RuntimeErrorCategory,
}
```

##### Abstract Runtime Interface

```rust
use async_trait::async_trait;
use toolkit_security::SecurityContext;

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

/// Host-implemented aggregate surface — the **gear-internal** shape the REST,
/// JSON-RPC and MCP handlers are built on. It is not the contract published to
/// other gears.
///
/// Two audiences were split out of this trait and are declared elsewhere:
///
/// * **Consuming gears** use `ServerlessRuntimeClientV1`, declared in
///   `serverless-runtime-sdk`. It is deliberately narrower — invocation and
///   observation only (start, read, query, control, replay) — and carries none of
///   the CRUD below. See `serverless-sdk/docs/DESIGN.md` §3.3.
/// * **Runtime plugins** report index updates through `InvocationIndexPort`, declared
///   in `serverless-runtime-plugin-sdk` and defined below — not through this trait.
///
/// The CRUD methods below belong here by design, not by omission: registry,
/// schedule, trigger and tenant-policy management are administrative operations
/// whose callers are people or external clients, so they are reachable over the
/// transports and are deliberately absent from the published consumer trait.
#[async_trait]
pub trait ServerlessRuntimeService: Send + Sync {
    // --- Functions (registry CRUD) ---

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

    // --- Invocations ---
    //
    // `start_invocation` and `control_invocation` are the public entry points for
    // REST/JSON-RPC/MCP handlers. The host implementation applies cross-cutting
    // concerns at the dispatch boundary (auth, tenant policy, GTS validation,
    // rate limiting, Idempotency-Key dedup, response cache, dry-run short-circuit
    // — see `cpt-cf-serverless-runtime-seq-invocation-flow`) and then routes to
    // the resolved `RuntimeAdapter`. Handlers MUST NOT bypass this trait by
    // resolving the adapter directly.
    //
    // `list_invocations` / `get_invocation` are served from the host-owned
    // `invocation_index` populated via the plugin event port, and so return
    // `InvocationSummary` — the index row itself, rather than an `InvocationRecord`
    // with most of its fields left empty. Params, results, observability and the
    // full timeline require `RuntimeAdapter::get_invocation` /
    // `RuntimeAdapter::get_invocation_timeline`, which read the plugin's own store.

    async fn start_invocation(
        &self,
        ctx: &SecurityContext,
        request: InvocationRequest,
    ) -> Result<InvocationResult, RuntimeErrorPayload>;

    // Note: `Replay` mints a *new* invocation with a new id, unlike the in-place
    // actions, which is why the published consumer trait exposes replay as its own
    // operation rather than as a control action (see `serverless-sdk/docs/PRD.md` §4.1).
    // Whether `Retry` also mints a new id is undecided: the status state machine has no
    // `failed -> running` transition, so it is unspecified here pending that decision.
    async fn control_invocation(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
        action: InvocationControlAction,
    ) -> Result<InvocationSummary, RuntimeErrorPayload>;

    async fn list_invocations(
        &self,
        ctx: &SecurityContext,
        filter: InvocationListFilter,
    ) -> Result<Vec<InvocationSummary>, RuntimeErrorPayload>;

    async fn get_invocation(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
    ) -> Result<InvocationSummary, RuntimeErrorPayload>;

    // --- Schedules (host-side metadata CRUD; plugin owns the schedule itself) ---

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

    // --- Event triggers (host-side metadata CRUD; plugin owns the subscription) ---

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

    // --- Tenant policy ---

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

    // The plugin -> host event port is NOT part of this trait. It is a separate
    // contract, `InvocationIndexPort` below, declared in
    // `serverless-runtime-plugin-sdk`.
}

/// Thin plugin -> host event port. Declared in `serverless-runtime-plugin-sdk` and
/// implemented by the host; only runtime plugins call it. Kept separate from the
/// host's aggregate surface above so that a consuming gear, which depends on
/// neither, cannot see it — and so that a plugin depends on this alone rather than
/// on the registry CRUD it has no business calling.
///
/// This is a **notification surface only**. It is not a general-purpose callback
/// API: the host accepts notifications, updates the host-owned invocation index and
/// re-emits timeline events to subscribers. Nothing here returns data to the plugin,
/// and the host never calls back into the plugin through it. See ADR
/// `cpt-cf-serverless-runtime-adr-thin-host` (Consequences, and the 2026-07-30
/// amendment recording the SDK split).
#[async_trait]
pub trait InvocationIndexPort: Send + Sync {
    /// Notify the host of an invocation status transition. Plugins call this on
    /// every status change so the host invocation index stays current.
    /// `error` is present for failure statuses; full error data remains plugin-local.
    async fn publish_invocation_status(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
        status: InvocationStatus,
        error: Option<InvocationIndexError>,
    ) -> Result<(), RuntimeErrorPayload>;

    /// Notify the host of a timeline-relevant event for downstream subscribers
    /// (audit, observability). The host does not commit the timeline itself —
    /// the plugin remains the source of truth via `RuntimeAdapter::get_invocation_timeline`.
    async fn publish_invocation_event(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
        timeline_event: InvocationTimelineEvent,
    ) -> Result<(), RuntimeErrorPayload>;
}

/// Runtime Plugin contract. Each plugin implements `RuntimeAdapter` and binds it to a
/// single GTS adapter type (e.g., `gts.cf.core.sless.runtime.starlark.v1~`). The
/// host dispatches calls via `dyn RuntimeAdapter` after `ClientHub` scoped resolution
/// keyed by adapter GTS type. Plugins own the execution engine, scheduler, and event
/// subscription stack for their adapter type and report progress back to the host
/// only through `InvocationIndexPort::publish_invocation_status` /
/// `publish_invocation_event`.
#[async_trait]
pub trait RuntimeAdapter: Send + Sync {
    // --- Identity ---

    /// The single GTS adapter type ID this plugin handles.
    fn adapter_type(&self) -> &GtsId;

    // --- Lifecycle hooks ---

    /// Validate a function implementation against this adapter's constraints
    /// (limits, language/runtime, payload shape). Called by the host during
    /// function registration before the definition is persisted.
    async fn validate_implementation(
        &self,
        ctx: &SecurityContext,
        function: &FunctionDefinition,
    ) -> Result<(), RuntimeErrorPayload>;

    /// Notify the plugin that a function it owns has transitioned to `Active`.
    /// Plugins use this to provision adapter-side state (compile/cache code,
    /// register workflow definitions, etc.).
    async fn on_function_activated(
        &self,
        ctx: &SecurityContext,
        function: &FunctionDefinition,
    ) -> Result<(), RuntimeErrorPayload>;

    /// Notify the plugin that a function it owns has transitioned out of `Active`
    /// (deprecated, disabled, archived, deleted). Plugins use this to release
    /// adapter-side resources.
    async fn on_function_deactivated(
        &self,
        ctx: &SecurityContext,
        function: &FunctionDefinition,
    ) -> Result<(), RuntimeErrorPayload>;

    // --- Invocation ---

    /// Start a new invocation. The plugin synchronously returns the initial
    /// `InvocationResult` (queued/running/succeeded for fast paths) and is
    /// responsible for emitting subsequent status transitions through the
    /// host event port.
    async fn start_invocation(
        &self,
        ctx: &SecurityContext,
        request: InvocationRequest,
    ) -> Result<InvocationResult, RuntimeErrorPayload>;

    /// Apply a control action (cancel/suspend/resume/retry/replay) to an
    /// in-flight invocation. The plugin emits the resulting status transition
    /// via the host event port.
    async fn control_invocation(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
        action: InvocationControlAction,
    ) -> Result<InvocationRecord, RuntimeErrorPayload>;

    /// Return the authoritative single-invocation record from the plugin's own
    /// state store. The host uses this for deep fetches that go beyond the
    /// host-indexed aggregate row.
    async fn get_invocation(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
    ) -> Result<InvocationRecord, RuntimeErrorPayload>;

    /// Return invocations from the plugin's own state store. The host uses this
    /// for adapter-scoped queries that need fields not present in the host
    /// invocation index.
    async fn list_invocations(
        &self,
        ctx: &SecurityContext,
        filter: InvocationListFilter,
    ) -> Result<Vec<InvocationRecord>, RuntimeErrorPayload>;

    /// Return the full timeline of an invocation. The plugin is the source of
    /// truth — the host event port re-emits timeline events but does not store
    /// them.
    async fn get_invocation_timeline(
        &self,
        ctx: &SecurityContext,
        invocation_id: &InvocationId,
    ) -> Result<Vec<InvocationTimelineEvent>, RuntimeErrorPayload>;

    // --- Schedules ---
    //
    // The plugin owns the live schedule (cron evaluation, missed-policy handling,
    // concurrency policies) using its backend's native facilities. The host
    // delegates the full schedule lifecycle that affects backend state — create,
    // patch, pause, resume, delete — to the adapter. Read-only listings and
    // history queries (`list_schedules`, `get_schedule`, `list_schedule_invocations`)
    // remain host-side and are served from host-owned metadata and the
    // `invocation_index`.

    async fn create_schedule(
        &self,
        ctx: &SecurityContext,
        schedule: Schedule,
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

    // --- Event triggers (plugin owns the event subscription) ---

    async fn create_trigger(
        &self,
        ctx: &SecurityContext,
        trigger: Trigger,
    ) -> Result<Trigger, RuntimeErrorPayload>;

    async fn delete_trigger(
        &self,
        ctx: &SecurityContext,
        trigger_id: &TriggerId,
    ) -> Result<(), RuntimeErrorPayload>;
}
```

##### Additional Rust Types

```rust
/// GTS ID: gts.cf.core.sless.err.v1~cf.core.sless.err.validation.v1~
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

/// GTS ID: gts.cf.core.sless.timeline_event.v1~
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
