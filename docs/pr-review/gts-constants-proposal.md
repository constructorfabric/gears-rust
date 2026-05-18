## PR Comment: Replace String Constants with GTS Type Instances

---

### Why GTS Over String Constants

The current design uses raw string literals (`"tenant"`, `"consumption"`, `"hard"`, `"monthly"`, etc.) as discriminators throughout the quota creation API, the plugin registry, and the evaluation engine. I recommend replacing all of these with GTS instance URIs. Here is the full rationale.

#### 1. Descriptions travel with the type

A GTS schema carries a `description`, `display_name`, `tags`, and arbitrary `properties` in its registry entry. A bare string constant like `"consumption"` carries nothing — its meaning lives in scattered comments and PRD prose that drifts from the code over time. When a developer calls the quota API and sees `quota_type: "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1"` they can resolve that URI against the GTS registry and get a human-readable description, the owning team, the version history, and all custom properties attached to the schema. No documentation spelunking required.

#### 2. Discoverability via API

With string constants, the only way to know valid values is to read the PRD or look at source code. With GTS the registry is queryable:

```
GET /gts/schemas?parent=gts://gts.cf.qe.quota.type.v1~
→ [
    "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1",
    "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.allocation.v1",
    "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.rate.v1"
  ]
```

A client SDK, an admin UI, and an integration test can all enumerate valid values at runtime. There is no need to hard-code an enum on the client side or keep a separate "valid values" document in sync.

#### 3. Misprint detection at the boundary

A string constant is validated only if someone wrote a hand-rolled allowlist check. A GTS URI is validated by resolving it against the registry — the registry either has it or it does not. A typo like `"consumptoin"` or `"monhtly"` returns an immediate, structured `404 Not Found` from the GTS resolver with the offending URI in the response body, rather than a mysterious runtime failure deep in the evaluation engine. This makes integration errors obvious at the API boundary, not inside a CEL expression.

#### 4. No API versioning churn when the set of values grows

If `quota_type` is declared as a Rust `enum` (or an OpenAPI `enum` string constraint), adding a new variant is a **breaking change**:

- Generated client SDKs that pattern-match exhaustively will fail to compile.
- OpenAPI validators will reject the new value until the schema is republished under a new version.
- Consumers who cached a previous OpenAPI spec will see `422 Unprocessable Entity` for previously valid payloads.
- Any API contract that encodes the enum forces a major version bump (`/v2/...`) even when the rest of the surface is unchanged.

With a GTS URI the API field is always typed as `string (uri)`. The contract is:

> "pass a valid child instance of this base schema"

Adding `gts://gts.cf.qe.quota.type.v1~cf.qe.quota.rate.v1~` (P3 rate limiting) requires:

1. Register the new schema in the GTS registry.
2. Update the engine to handle it.

The API schema does not change. Existing clients are unaffected. No version bump. The registry becomes the extension point instead of the OpenAPI document.

The same argument applies to `period`, `enforcement_mode`, `source`, and `subject_type`. Each of these is currently a strong candidate to grow new values as the platform adds subscription tiers, new quota shapes, or tenant-hierarchy enforcement — all without touching the HTTP contract.

#### 5. Self-identifying values in logs and error responses

A string constant like `"hard"` or `"monthly"` is opaque in a log line or a Problem response body. Without surrounding context it is impossible to know which field it came from or which domain concept it represents. GTS URIs are self-describing:

```
# Log line with string constants — ambiguous
quota_check failed: subject=8f3a... type=consumption enforcement=hard period=monthly

# Log line with GTS URIs — unambiguous, greppable, linkable
quota_check failed:
  subject_type=gts://gts.cf.qe.subject.type.v1~cf.qe.subject.tenant.v1
  quota_type=gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1
  enforcement=gts://gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1
  period=gts://gts.cf.qe.quota.period.v1~cf.qe.quota.period.monthly.v1
```

Benefits in practice:

- **Grep is unambiguous.** Searching logs for `gts.cf.qe.quota.enforcement.hard` returns only enforcement decisions. Searching for `"hard"` matches anything.
- **Problem responses are self-documenting.** An RFC 9457 `Problem` body that carries a GTS URI in the `type` or detail fields points the caller directly to the registry entry for the failing concept, with no extra look-up step.
- **Cross-service correlation.** When QE emits an outbox event and Usage Collector emits a usage record, both carry the same `gts://gts.cf.uc.metric.type.v1~cf.uc.metric.llm_token.v1~` URI. A single grep across both services' logs ties the full request lifecycle together without a field-name mapping table.
- **No collision between domains.** Two different modules can both log a field named `type` without ambiguity — the URI prefix carries the domain.

#### 6. Vendor and plugin extensions without forking

The current design hard-codes the list of valid `subject_type`, `metric`, `enforcement_mode`, and `source` values. If a plugin vendor wants to introduce a new subject type (e.g. `cost_center`, `project`, `resource_group`) they cannot do so without modifying the core enum and redeploying the platform.

With GTS, a vendor registers their own child schema under the platform's parent:

```
gts://gts.cf.qe.subject.type.v1~acme.billing.subject.cost_center.v1
```

The QE engine resolves the URI and delegates to the plugin that declared it. The platform never needs to know about `cost_center` — the registry and the plugin contract are sufficient. This is the extension model the plugin architecture in ADR-0001 promises but does not deliver with raw strings.

#### 7. Custom properties on schemas

GTS schemas support arbitrary `properties` blocks. This lets the schema carry metadata that the engine can read without any code change:

```toml
[schema."gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1~".properties]
supports_rollover   = true
requires_period     = true
ledger_shape        = "gts://gts.cf.qe.ledger.shape.v1~cf.qe.ledger.shape.accumulative.v1~"
default_enforcement = "gts://gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1~"
```

The evaluation engine reads `ledger_shape` (itself a GTS URI) from the resolved schema, resolves it to get storage and lease semantics, and routes accordingly — no match arms needed. A new quota type registered by a plugin just declares its own `ledger_shape` URI; no engine code change.

#### 8. GTS-driven authorization — no hardcoded role checks

The clearest example of the pattern: `POST /quota-enforcement/quotas` (create a quota). The caller's required permission depends entirely on the `source` field of the request body — a licensing-system call must be authorized differently from a tenant-admin call or a user self-service call.

**Without GTS — fragile match arm in the handler:**

```rust
match request.source.as_str() {
    "licensing" | "operator" => {
        if !ctx.has_role("platform_operator") {
            return Err(Problem::forbidden());
        }
    }
    "tenant_admin" => {
        if !ctx.has_role("tenant_admin") {
            return Err(Problem::forbidden());
        }
    }
    "user_self" => {
        if request.subject_id != ctx.subject_id() {
            return Err(Problem::forbidden());
        }
    }
    _ => return Err(Problem::bad_request("unknown source")),
}
```

Every new `source` value — added by a plugin or a new subscription tier — requires a code change and a redeployment of the core module.

**With GTS — the handler has no role knowledge at all:**

```rust
// 1. Resolve the source schema from the GTS registry
let source_schema = gts.resolve(&request.source).await
    .map_err(|_| Problem::unprocessable("invalid source URI"))?;

// 2. Read the required permission URI from the schema's properties
let required_permission: &str = source_schema
    .properties
    .get("required_permission")
    .ok_or_else(|| Problem::internal("source schema missing required_permission"))?;

// 3. PolicyEnforcer checks the caller holds that permission
//    The PEP resolves roles, scopes, and tenant context — the handler does none of this
enforcer
    .access_scope(&ctx, &QUOTA_RESOURCE, required_permission, None)
    .await
    .map_err(|_| Problem::forbidden())?;
```

The authorization rule lives in the GTS schema of the `source` type, not in the handler. Adding a new source type (e.g. `partner_provisioned`) requires:

1. Register `~cf.qe.quota.source.partner_provisioned.v1~` in GTS with `required_permission = "...manage_partner.v1~"`.
2. Register the new permission instance.
3. Configure the PDP policy for that permission.

Zero handler code changes. The module does not need to be redeployed.

This pattern follows what `mini-chat` already does: every operation declares a GTS `AuthzPermissionV1` instance, and the PolicyEnforcer evaluates them at request time (see `libs/modkit/src/api/operation_builder.rs` `.authenticated()` + `modules/mini-chat/mini-chat/src/gts/permissions.rs`).

---

### Corrected GTS URI for Subject Type (Fixes Finding #1 and #2)

The current document uses:

```
gts.x.qe.subject-type.v1~          ← wrong vendor prefix, kebab-case, only 3 tokens
```

The correct form following confirmed platform conventions (4 tokens, underscores, `cf` vendor):

```
gts://gts.cf.qe.subject.type.v1~
```

Every child instance follows the chained format:

```
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.tenant.v1
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.user.v1
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.service_account.v1   ← P2
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.cost_center.v1       ← P3
```

---

### Proposed Full GTS Schema Catalogue

Below is **just an example** of possible schema catalogue covering every string constant in the current quota API. Each section shows the base schema URI, its description and properties, and all first-party child instances.

---

#### A. Subject Type

**Base schema**

```
URI:          gts://gts.cf.qe.subject.type.v1~
Vendo:        cf (Cyber Fabric)
Package:      qe (Quota Enforcement)
Description:  Discriminator for the entity whose quota consumption is being tracked.
              A subject type identifies the dimension on which quota isolation is applied.
Properties:
  hierarchical: bool   # whether subjects of this type form a tree
  is_uuid:      bool   # true means the subject ID is UUID, otherwise a string
  description:  string
```

**Child instances**

| URI | description | hierarchical | is_uuid |
|-----|-------------|-------------|-----------|
| `~cf.qe.subject.tenant.v1` | Top-level organizational tenant. Quota applies to all users and workloads within the tenant. | true | true |
| `~cf.qe.subject.user.v1` | Individual authenticated user within a tenant. | false | true |
| `~cf.qe.subject.service_account.v1` | Non-human workload identity. Treated as a first-class subject for machine-to-machine quota. | false | true |
| `~cf.qe.subject.cost_center.v1` | (P3) Named accounting unit within a tenant hierarchy. Supports hierarchical cap propagation. | true | false |

---

#### B. Metric Type (Usage Collector owned)

**Base schema**

```
URI:          gts://gts.cf.uc.metric.type.v1~
Vendor:       cf (Cyber Fabric)
Package:      uc (Usage Collector) ← Usage Collector owns metric definitions, QE consumes them
Description:  Identifies the resource or activity being metered.
              The same metric type is used by both Usage Collector (for recording)
              and Quota Enforcement (for counting against caps).
Properties:
  unit:        string  # human display label: "tokens", "requests", "bytes", "calls"
  granularity: uri     # gts://gts.cf.uc.metric.granularity.v1~ child instance
  aggregation: uri     # gts://gts.cf.uc.metric.aggregation.v1~ child instance
```

**Child instances**

| URI | unit | granularity (short) | aggregation (short) | description |
|-----|------|---------------------|---------------------|-------------|
| `~cf.uc.metric.api_request.v1~` | requests | `~granularity.delta.v1~` | `~aggregation.sum.v1~` | One REST or gRPC call to any platform API endpoint |
| `~cf.uc.metric.llm_token.v1~` | tokens | `~granularity.delta.v1~` | `~aggregation.sum.v1~` | LLM prompt+completion token pair, model-agnostic |
| `~cf.uc.metric.storage_byte.v1~` | bytes | `~granularity.cumulative.v1~` | `~aggregation.last.v1~` | Persistent storage consumed at snapshot time |
| `~cf.uc.metric.egress_byte.v1~` | bytes | `~granularity.delta.v1~` | `~aggregation.sum.v1~` | Data transferred out of the platform boundary |
| `~cf.uc.metric.compute_second.v1~` | seconds | `~granularity.delta.v1~` | `~aggregation.sum.v1~` | Wall-clock compute time charged to the subject |

> **Note:** new metric types are registered by the owning team in the UC namespace. QE needs no code change to enforce a new metric — it reads the schema's `unit`, `granularity`, and `aggregation` URIs and routes accordingly.

---

#### B1. Metric Granularity

**Base schema**

```
URI:          gts://gts.cf.uc.metric.granularity.v1~
Vendor:       cf (Cyber Fabric)
Package:      uc (Usage Collector)
Description:  Describes how individual metric observations relate to cumulative totals.
              Determines whether Usage Collector should sum increments or take the
              latest snapshot when computing a period value.
Properties:
  is_additive: bool  # true = readings are summed to produce period totals;
                     # false = only the latest reading within a window is meaningful
```

**Child instances**

| URI | is_additive | description |
|-----|-------------|-------------|
| `~cf.uc.metric.granularity.delta.v1~` | true | Each observation is an increment to be added to prior observations. Typical for event-driven metrics: API calls, tokens consumed, bytes transferred. The period total = Σ(all increments in the window). |
| `~cf.uc.metric.granularity.cumulative.v1~` | false | Each observation is a running total that supersedes the previous value. Typical for storage bytes sampled at a point in time. Summing two readings would double-count; only the latest reading is used. |

---

#### B2. Metric Aggregation

**Base schema**

```
URI:          gts://gts.cf.uc.metric.aggregation.v1~
Vendor:       cf (Cyber Fabric)
Package:      uc (Usage Collector)
Description:  Defines the mathematical function applied when collapsing multiple
              observations into a single value for a quota comparison or reporting window.
              Works in conjunction with granularity: the aggregation function is applied
              after observations are collected according to the granularity rule.
Properties:
  requires_ordered_observations: bool    # true = result depends on observation order
  identity_element:              string  # neutral element for the operation
```

**Child instances**

| URI | requires_ordered_observations | identity_element | description |
|-----|------------------------------|------------------|-------------|
| `~cf.uc.metric.aggregation.sum.v1~` | false | `"0"` | Observations are summed to produce the window total. Used with `delta` granularity metrics where each reading is an increment. The quota balance is `cap − Σ(increments)`. |
| `~cf.uc.metric.aggregation.max.v1~` | false | `"-∞"` | The highest observed value within the window is taken. Used with `cumulative` granularity to detect peak usage. The quota balance is `cap − max(readings)`. |
| `~cf.uc.metric.aggregation.last.v1~` | true | `"—"` | The most recent observation replaces all prior ones. Used with `cumulative` granularity where only current state matters. The quota balance is `cap − latest(reading)`. |

---

#### C. Quota Type

**Base schema**

```
URI:          gts://gts.cf.qe.quota.type.v1~
Vendor:       cf (Cyber Fabric)
Package:      qe (Quota Enforcement)
Description:  Structural shape of the quota. Determines the ledger accounting model,
              whether the period resets, and what the enforcement semantics are.
Properties:
  ledger_shape:      uri   # gts://gts.cf.qe.ledger.shape.v1~ child instance
  supports_rollover: bool
  requires_period:   bool
  is_additive:       bool  # whether multiple quotas of this type stack
```

**Child instances**

| URI | ledger_shape (short) | supports_rollover | requires_period | description |
|-----|---------------------|-------------------|-----------------|-------------|
| `~cf.qe.quota.consumption.v1~` | `~ledger.shape.accumulative.v1~` | true | true | Tracks cumulative usage against a cap. Resets each period. Typical for API calls and tokens. |
| `~cf.qe.quota.allocation.v1~` | `~ledger.shape.reservable.v1~` | false | false | Reserves capacity via lease acquire/commit. Typical for storage bytes and reserved seats. |
| `~cf.qe.quota.rate.v1~` | `~ledger.shape.accumulative.v1~` | false | false | (P3) Rolling-window rate limit. Cap applies per sliding interval, not a calendar period. |

---

#### C1. Ledger Shape

**Base schema**

```
URI:          gts://gts.cf.qe.ledger.shape.v1~
Vendor:       cf (Cyber Fabric)
Package:      qe (Quota Enforcement)
Description:  Defines the accounting model used by the quota engine to track usage
              against a cap. The ledger shape determines which storage operations are
              available, how balance is computed, and whether lease operations apply.
              This is an internal routing concept — callers never set it directly;
              the engine reads it from the resolved quota_type schema.
Properties:
  supports_period_reset: bool    # true = counter resets at each period boundary
  supports_lease:        bool    # true = acquire / commit / release operations apply
  balance_formula:       string  # human description of how remaining balance is computed
```

**Child instances**

| URI | supports_period_reset | supports_lease | balance_formula | description |
|-----|----------------------|----------------|-----------------|-------------|
| `~cf.qe.ledger.shape.accumulative.v1~` | true | false | `cap − Σ(committed_debits)` | Counter grows monotonically within a period and is compared against the cap. Resets at each period boundary. Typical for API calls and token consumption. |
| `~cf.qe.ledger.shape.reservable.v1~` | false | true | `cap − Σ(active_leases + committed_leases)` | Capacity is tentatively claimed via acquire and confirmed by commit or returned by release. No period reset. Typical for storage bytes and reserved seats. |

---

#### D. Quota Period

**Base schema**

```
URI:          gts://gts.cf.qe.quota.period.v1~
Owner:        cf.qe
Description:  Calendar or billing period that governs when a consumption quota resets.
              Not applicable to allocation quotas (see quota_type.requires_period).
Properties:
  iso_duration:     string  # ISO 8601 duration: P1D, P1W, P1M, P1Y
  calendar_aligned: bool    # true = resets at start of calendar unit, false = rolling
  orderable:        bool    # whether periods form a total order for sequencing
  description:      string
```

**Child instances**

| URI | iso_duration | calendar_aligned | description |
|-----|-------------|-----------------|-------------|
| `~cf.qe.quota.period.daily.v1` | P1D | true | Resets at midnight UTC each calendar day |
| `~cf.qe.quota.period.weekly.v1` | P1W | true | Resets at Monday 00:00 UTC each calendar week |
| `~cf.qe.quota.period.monthly.v1` | P1M | true | Resets on the 1st of each calendar month at 00:00 UTC |
| `~cf.qe.quota.period.yearly.v1` | P1Y | true | Resets on January 1st at 00:00 UTC |
| `~cf.qe.quota.period.one_time.v1` | — | false | No reset. Cap is a lifetime allowance. Useful for trial credits. |

---

#### E. Enforcement Mode

**Base schema**

```
URI:          gts://gts.cf.qe.quota.enforcement.v1~
Owner:        cf.qe
Description:  Controls how the evaluation engine responds when a debit request would
              exceed the remaining cap.
Properties:
  rejects_over_cap:   bool  # true = HTTP 429 / RESOURCE_EXHAUSTED
  allows_partial:     bool  # true = debit is clamped to remaining balance
  notifies_threshold: bool  # true = threshold events are emitted
  description:        string
```

**Child instances**

| URI | rejects_over_cap | allows_partial | description |
|-----|-----------------|----------------|-------------|
| `~cf.qe.quota.enforcement.hard.v1~` | true | false | Strictly rejects any debit that would exceed the cap. No partial grants. |
| `~cf.qe.quota.enforcement.hard_with_clamp.v1~` | false | true | (P3) Grants only the remaining balance. Caller receives actual granted amount, never exceeds cap. |
| `~cf.qe.quota.enforcement.soft.v1~` | false | false | (P3) Allows over-cap with notification only. Useful for advisory limits during migration. |

---

#### F. Quota Source

**Base schema**

```
URI:          gts://gts.cf.qe.quota.source.v1~
Owner:        cf.qe
Description:  Identifies the authority that created or granted this quota.
              Used for audit, for multi-quota arbitration ordering, and for
              determining whether the quota can be modified by the subject.
Properties:
  mutable_by_subject:  bool    # whether the subject itself can adjust the cap
  priority:            int     # lower = higher authority; used in most-restrictive-wins ordering
  required_permission: string  # GTS AuthzPermissionV1 URI; PolicyEnforcer evaluates this at request time
```

**Child instances**

| URI | priority | mutable_by_subject | required_permission | description |
|-----|---------|-------------------|---------------------|-------------|
| `~cf.qe.quota.source.licensing.v1~` | 0 | false | `~cf.qe.quota.manage_system.v1` | Quota bound to a commercial license or subscription entitlement. Highest authority. |
| `~cf.qe.quota.source.operator.v1~` | 10 | false | `~cf.qe.quota.manage_system.v1` | Platform operator override. Applies platform-wide policy limits. |
| `~cf.qe.quota.source.tenant_admin.v1~` | 20 | false | `~cf.qe.quota.manage_tenant.v1` | (P2) Tenant administrator-configured cap. Cannot exceed operator cap. |
| `~cf.qe.quota.source.user_self.v1~` | 30 | true | `~cf.qe.quota.manage_self.v1` | (P2) User-defined personal cap. Cannot exceed tenant_admin cap. |

---

#### G. Permissions (AuthzPermissionV1 instances)

Each quota operation gets a dedicated GTS `AuthzPermissionV1` instance. The PolicyEnforcer resolves these at request time; no role checks live in handler code.

**Base schema (platform-owned):**

```
URI:    gts://gts.cf.modkit.authz.permission.v1~
Owner:  cf.modkit
```

**QE permission instances:**

| URI (short) | action | resource_type pattern | description |
|-------------|--------|-----------------------|-------------|
| `~cf.qe.quota.manage_system.v1~` | `quota:manage:system` | `gts.cf.qe.resource.quota.v1~*` | Create / modify licensing- or operator-source quotas. Requires platform operator principal. |
| `~cf.qe.quota.manage_tenant.v1~` | `quota:manage:tenant` | `gts.cf.qe.resource.quota.v1~*` | Create / modify tenant_admin-source quotas. Requires tenant admin role within the caller's tenant. |
| `~cf.qe.quota.manage_self.v1~` | `quota:manage:self` | `gts.cf.qe.resource.quota.v1~*` | Create / modify user_self-source quotas. Subject ID in request must match caller. |
| `~cf.qe.quota.debit.v1~` | `quota:debit` | `gts.cf.qe.resource.quota.v1~*` | Debit, credit, and lease operations. Typically held by service accounts and the evaluation engine. |
| `~cf.qe.quota.read.v1~` | `quota:read` | `gts.cf.qe.resource.quota.v1~*` | Read quota configuration and remaining balance. Held by all authenticated principals. |

The `~cf.qe.quota.source.*` schemas each carry `required_permission` pointing to the matching entry above. The handler resolves the source URI → reads `required_permission` → calls `enforcer.access_scope(ctx, &QUOTA_RESOURCE, required_permission, None)`. No `match` arm on source string values anywhere in production code.

---

#### H. GTS Resource Type Constants (Fixes Finding #8)

These are the OpenAPI / Problem envelope resource type URIs referenced in DESIGN.md §5 but never spelled out. They follow the resource type convention (flat schema URIs, not child-schema chaining):

```
Quota policy resource:
  gts://gts.cf.qe.resource.quota.v1~
  Description: A configured quota policy record. References subject, metric,
               cap, period, and enforcement mode.

Quota lease resource:
  gts://gts.cf.qe.resource.lease.v1~
  Description: An active two-phase lease acquired against an allocation quota.
               Has TTL; expires lazily on next operation if not committed.

Quota operation resource:
  gts://gts.cf.qe.resource.operation.v1~
  Description: A debit or credit operation applied against a quota counter.
               Immutable once committed. Carries correlation_id for idempotency.

Enforcement decision resource:
  gts://gts.cf.qe.resource.decision.v1~
  Description: The evaluation result returned by QuotaResolutionEngineV1::evaluate.
               Carries granted_amount, plan entries, and applied quota references.
```

---

### Corrected Quota Creation Example

**Current (string constants):**

```json
{
  "subject_type": "tenant",
  "subject_id": "uuid-...",
  "metric": "llm_tokens",
  "quota_type": "consumption",
  "period": "monthly",
  "cap": 1000000,
  "enforcement_mode": "hard",
  "source": "licensing",
  "notification_thresholds": [0.75, 0.90, 1.0]
}
```

**Proposed (GTS URIs):**

```json
{
  "subject_type": "gts.cf.qe.subject.type.v1~cf.qe.subject.tenant.v1",
  "subject_id": "uuid-...",
  "metric": "gts.cf.uc.metric.type.v1~cf.uc.metric.llm_token.v1",
  "quota_type": "gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1",
  "period": "gts.cf.qe.quota.period.v1~cf.qe.quota.period.monthly.v1",
  "cap": 1000000,
  "enforcement_mode": "gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1",
  "source": "gts.cf.qe.quota.source.v1~cf.qe.quota.source.licensing.v1",
  "notification_thresholds": [0.75, 0.90, 1.0]
}
```

The validation path at the API boundary becomes:

1. Receive URI string
2. `GET /gts/resolve?uri={uri}` → 404 if misprint, 200 + schema if valid
3. Assert resolved schema's parent matches expected base URI (guards against cross-category confusion, e.g. passing a period URI where an enforcement URI is expected)
4. Read `properties` block from schema for engine configuration — no match arms needed

---

### Impact Summary

| String constant | Replacement base schema | Child instances | Properties usable by engine |
|----------------|------------------------|-----------------|----------------------------|
| `subject_type` | `gts.cf.qe.subject.type.v1` | 4 (2 P3) | hierarchical, is_uuid |
| `metric` | `gts.cf.uc.metric.type.v1` | 5 | unit, granularity (URI), aggregation (URI) |
| metric `granularity` value | `gts.cf.uc.metric.granularity.v1` | 2 | is_additive |
| metric `aggregation` value | `gts.cf.uc.metric.aggregation.v1` | 3 | requires_ordered_observations, identity_element |
| `quota_type` | `gts.cf.qe.quota.type.v1` | 3 (1 P3) | ledger_shape (URI), supports_rollover |
| `quota_type.ledger_shape` value | `gts.cf.qe.ledger.shape.v1` | 2 | supports_period_reset, supports_lease, balance_formula |
| `period` | `gts.cf.qe.quota.period.v1` | 5 | iso_duration, calendar_aligned |
| `enforcement_mode` | `gts.cf.qe.quota.enforcement.v1` | 3 (2 P3) | rejects_over_cap, allows_partial |
| `source` | `gts.cf.qe.quota.source.v1` | 4 (2 P2) | priority, mutable_by_subject, required_permission (URI) |
| `source.required_permission` value | `gts.cf.modkit.authz.permission.v1` | 5 | eliminates role match arms in handler |
| resource type constants | flat schemas in `gts.cf.qe.resource.*` | 4 | used in Problem envelope `type` field |

Adopting this catalogue also closes Findings #1, #2 (wrong URI prefix on existing `gts.x.qe.subject-type.v1~`) and Finding #8 (resource-type constants unnamed) from the current review.
