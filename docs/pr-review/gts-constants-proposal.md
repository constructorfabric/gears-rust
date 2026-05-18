## PR Comment: Replace String Constants with GTS Type Instances

**Files:** `DESIGN.md` §5 (Resource Types), §6 (Plugin Registry), PRD §5.1 (Subject Type Registry), quota creation API schema
**Related findings:** Finding #1 (CRITICAL, wrong GTS URI prefix), Finding #2 (CRITICAL, second occurrence), Finding #8 (MEDIUM, resource-type constants undefined)

---

### Why GTS Over String Constants

The current design uses raw string literals (`"tenant"`, `"consumption"`, `"hard"`, `"monthly"`, etc.) as discriminators throughout the quota creation API, the plugin registry, and the evaluation engine. I recommend replacing all of these with GTS instance URIs. Here is the full rationale.

#### 1. Descriptions travel with the type

A GTS schema carries a `description`, `display_name`, `tags`, and arbitrary `properties` in its registry entry. A bare string constant like `"consumption"` carries nothing — its meaning lives in scattered comments and PRD prose that drifts from the code over time. When a developer calls the quota API and sees `quota_type: "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1~"` they can resolve that URI against the GTS registry and get a human-readable description, the owning team, the version history, and all custom properties attached to the schema. No documentation spelunking required.

#### 2. Discoverability via API

With string constants, the only way to know valid values is to read the PRD or look at source code. With GTS the registry is queryable:

```
GET /gts/schemas?parent=gts://gts.cf.qe.quota.type.v1~
→ [
    "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1~",
    "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.allocation.v1~",
    "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.rate.v1~"
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

#### 5. Vendor and plugin extensions without forking

The current design hard-codes the list of valid `subject_type`, `metric`, `enforcement_mode`, and `source` values. If a plugin vendor wants to introduce a new subject type (e.g. `cost_center`, `project`, `resource_group`) they cannot do so without modifying the core enum and redeploying the platform.

With GTS, a vendor registers their own child schema under the platform's parent:

```
gts://gts.cf.qe.subject.type.v1~acme.billing.subject.cost_center.v1~
```

The QE engine resolves the URI and delegates to the plugin that declared it. The platform never needs to know about `cost_center` — the registry and the plugin contract are sufficient. This is the extension model the plugin architecture in ADR-0001 promises but does not deliver with raw strings.

#### 5. Custom properties on schemas

GTS schemas support arbitrary `properties` blocks. This lets the schema carry metadata that the engine can read without any code change:

```toml
[schema."gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1~".properties]
supports_rollover   = true
requires_period     = true
counter_table       = "quota_consumption_counters"
default_enforcement = "gts://gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1~"
```

The evaluation engine reads `counter_table` from the resolved schema instead of a match arm. A new quota type defined by a plugin just needs to register its schema with the right properties — no engine code change.

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
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.tenant.v1~
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.user.v1~
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.service_account.v1~   ← P2
gts://gts.cf.qe.subject.type.v1~cf.qe.subject.cost_center.v1~       ← P3
```

---

### Proposed Full GTS Schema Catalogue

Below is the complete schema catalogue covering every string constant in the current quota API. Each section shows the base schema URI, its description and properties, and all first-party child instances.

---

#### A. Subject Type

**Base schema**

```
URI:          gts://gts.cf.qe.subject.type.v1~
Owner:        cf.qe
Description:  Discriminator for the entity whose quota consumption is being tracked.
              A subject type identifies the dimension on which quota isolation is applied.
Properties:
  hierarchical: bool   # whether subjects of this type form a tree
  id_format:    string # hint for ID validation (uuid, email, slug, ...)
```

**Child instances**

| URI | description | hierarchical | id_format |
|-----|-------------|-------------|-----------|
| `~cf.qe.subject.tenant.v1~` | Top-level organizational tenant. Quota applies to all users and workloads within the tenant. | true | uuid |
| `~cf.qe.subject.user.v1~` | Individual authenticated user within a tenant. | false | uuid |
| `~cf.qe.subject.service_account.v1~` | Non-human workload identity. Treated as a first-class subject for machine-to-machine quota. | false | uuid |
| `~cf.qe.subject.cost_center.v1~` | (P3) Named accounting unit within a tenant hierarchy. Supports hierarchical cap propagation. | true | slug |

---

#### B. Metric Type (Usage Collector owned)

**Base schema**

```
URI:          gts://gts.cf.uc.metric.type.v1~
Owner:        cf.uc   ← Usage Collector owns metric definitions, QE consumes them
Description:  Identifies the resource or activity being metered.
              The same metric type is used by both Usage Collector (for recording)
              and Quota Enforcement (for counting against caps).
Properties:
  unit:        string  # human label: "tokens", "requests", "bytes", "calls"
  granularity: string  # "cumulative" | "delta"
  aggregation: string  # "sum" | "max" | "last"
```

**Child instances**

| URI | unit | granularity | description |
|-----|------|-------------|-------------|
| `~cf.uc.metric.api_request.v1~` | requests | delta | One REST or gRPC call to any platform API endpoint |
| `~cf.uc.metric.llm_token.v1~` | tokens | delta | LLM prompt+completion token pair, model-agnostic |
| `~cf.uc.metric.storage_byte.v1~` | bytes | cumulative | Persistent storage consumed at snapshot time |
| `~cf.uc.metric.egress_byte.v1~` | bytes | delta | Data transferred out of the platform boundary |
| `~cf.uc.metric.compute_second.v1~` | seconds | delta | Wall-clock compute time charged to the subject |

> **Note:** new metric types are registered by the owning team in the UC namespace. QE needs no code change to enforce a new metric — it reads the schema's `unit` and `aggregation` properties.

---

#### C. Quota Type

**Base schema**

```
URI:          gts://gts.cf.qe.quota.type.v1~
Owner:        cf.qe
Description:  Structural shape of the quota. Determines which counter table is used,
              whether the period resets, and what the enforcement semantics are.
Properties:
  counter_table:     string  # "quota_consumption_counters" | "quota_allocation_counters"
  supports_rollover: bool
  requires_period:   bool
  is_additive:       bool    # whether multiple quotas of this type stack
```

**Child instances**

| URI | counter_table | supports_rollover | requires_period | description |
|-----|--------------|-------------------|-----------------|-------------|
| `~cf.qe.quota.consumption.v1~` | quota_consumption_counters | true | true | Tracks cumulative usage against a cap. Resets each period. Typical for API calls and tokens. |
| `~cf.qe.quota.allocation.v1~` | quota_allocation_counters | false | false | Reserves capacity via lease acquire/commit. Typical for storage bytes and reserved seats. |
| `~cf.qe.quota.rate.v1~` | quota_consumption_counters | false | false | (P3) Rolling-window rate limit. Cap applies per sliding interval, not a calendar period. |

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
```

**Child instances**

| URI | iso_duration | calendar_aligned | description |
|-----|-------------|-----------------|-------------|
| `~cf.qe.quota.period.daily.v1~` | P1D | true | Resets at midnight UTC each calendar day |
| `~cf.qe.quota.period.weekly.v1~` | P1W | true | Resets at Monday 00:00 UTC each calendar week |
| `~cf.qe.quota.period.monthly.v1~` | P1M | true | Resets on the 1st of each calendar month at 00:00 UTC |
| `~cf.qe.quota.period.yearly.v1~` | P1Y | true | Resets on January 1st at 00:00 UTC |
| `~cf.qe.quota.period.one_time.v1~` | — | false | No reset. Cap is a lifetime allowance. Useful for trial credits. |

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
  mutable_by_subject: bool  # whether the subject itself can adjust the cap
  priority:           int   # lower = higher authority; used in most-restrictive-wins ordering
```

**Child instances**

| URI | priority | mutable_by_subject | description |
|-----|---------|-------------------|-------------|
| `~cf.qe.quota.source.licensing.v1~` | 0 | false | Quota bound to a commercial license or subscription entitlement. Highest authority. |
| `~cf.qe.quota.source.operator.v1~` | 10 | false | Platform operator override. Applies platform-wide policy limits. |
| `~cf.qe.quota.source.tenant_admin.v1~` | 20 | false | (P2) Tenant administrator-configured cap. Cannot exceed operator cap. |
| `~cf.qe.quota.source.user_self.v1~` | 30 | true | (P2) User-defined personal cap. Cannot exceed tenant_admin cap. |

---

#### G. GTS Resource Type Constants (Fixes Finding #8)

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
  "subject_type": "gts://gts.cf.qe.subject.type.v1~cf.qe.subject.tenant.v1~",
  "subject_id": "uuid-...",
  "metric": "gts://gts.cf.uc.metric.type.v1~cf.uc.metric.llm_token.v1~",
  "quota_type": "gts://gts.cf.qe.quota.type.v1~cf.qe.quota.consumption.v1~",
  "period": "gts://gts.cf.qe.quota.period.v1~cf.qe.quota.period.monthly.v1~",
  "cap": 1000000,
  "enforcement_mode": "gts://gts.cf.qe.quota.enforcement.v1~cf.qe.quota.enforcement.hard.v1~",
  "source": "gts://gts.cf.qe.quota.source.v1~cf.qe.quota.source.licensing.v1~",
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

| String constant | Replacement base schema | Child instances defined | Properties usable by engine |
|----------------|------------------------|------------------------|----------------------------|
| `subject_type` | `gts.cf.qe.subject.type.v1` | 4 (2 P3) | hierarchical, id_format |
| `metric` | `gts.cf.uc.metric.type.v1` | 5 | unit, granularity, aggregation |
| `quota_type` | `gts.cf.qe.quota.type.v1` | 3 (1 P3) | counter_table, supports_rollover |
| `period` | `gts.cf.qe.quota.period.v1` | 5 | iso_duration, calendar_aligned |
| `enforcement_mode` | `gts.cf.qe.quota.enforcement.v1` | 3 (2 P3) | rejects_over_cap, allows_partial |
| `source` | `gts.cf.qe.quota.source.v1` | 4 (2 P2) | priority, mutable_by_subject |
| resource type constants | flat schemas in `gts.cf.qe.resource.*` | 4 | used in Problem envelope `type` field |

Adopting this catalogue also closes Findings #1, #2 (wrong URI prefix on existing `gts.x.qe.subject-type.v1~`) and Finding #8 (resource-type constants unnamed) from the current review.
