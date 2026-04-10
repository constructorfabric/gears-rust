# Typed Errors Wire Compatibility — modkit-contract vs PRD #1536

**Status:** Discussion / Decision Required
**Owner:** Mike Yastrebtsov
**Context:** Branch `feature/oop_clients`, third PR review audit vs [PRD #1536](https://github.com/cyberfabric/cyberfabric-core/pull/1536) and reference PoC [striped-zebra-dev/modkit-binding-poc](https://github.com/striped-zebra-dev/modkit-binding-poc).

---

## TL;DR

Our current `modkit-contract` implementation and the PRD specify **incompatible wire envelopes** for typed errors. A PRD-conformant client cannot reconstruct a domain-typed error from our server's response, and vice versa. This is not a bug — it's a deliberate architectural choice that was made before we noticed the PRD divergence. We need to pick: extend our envelope to support both, or amend the PRD.

---

## 1. What "round-trip" means

A *typed-error round-trip* is the contract that lets a Rust caller write:

```rust
match client.charge(&ctx, &req).await {
    Err(BillingError::InsufficientFunds { available, required }) => prompt_topup(available),
    Err(BillingError::AccountFrozen { reason }) => show_unfreeze_flow(reason),
    Err(BillingError::RateLimit { retry_after_sec }) => sleep_and_retry(retry_after_sec),
    Err(other) => log_and_bail(other),
    Ok(_) => proceed(),
}
```

For that `match` to work, three things must hold:

1. The server produces a specific typed variant (`BillingError::InsufficientFunds { ... }`).
2. The variant is serialized to a wire envelope without losing variant identity or payload.
3. The client deserializes that envelope back into the **same Rust variant** with the **same payload fields**.

If any step loses information, the client falls back to `match err { _ => generic_handler() }` — at which point the SDK is no better than parsing free-form JSON.

---

## 2. The two envelope formats

### 2.1 PRD envelope — `error_code` + `error_domain` extensions

The PRD adopts RFC 9457 `ProblemDetails` and adds two extension fields:

```json
{
  "type":         "https://errors.billing/insufficient-funds",
  "title":        "Insufficient funds",
  "status":       402,
  "detail":       "Account acc_123 has 100, requested 500",
  "instance":     "/v1/payments/charge/req-7f3a",

  "error_code":   "INSUFFICIENT_FUNDS",
  "error_domain": "billing.v1",

  "available":    100,
  "required":     500
}
```

- `error_code` + `error_domain` form a **two-part discriminator**, owned by the service.
- Variant payload fields (`available`, `required`) live at the top level of the envelope.
- The macro `#[derive(ContractError)]` generates both `From<BillingError> for ProblemDetails` (server) and `TryFrom<ProblemDetails> for BillingError` (client).

**Strengths:** any domain error can be expressed as a first-class Rust enum with arbitrary payload. Client code can `match` exhaustively on domain variants.

**Weaknesses:** every service grows its own dictionary of `error_code` values. No global standard. gRPC interop is fragile — you have to map your codes into one of gRPC's 16 `Code` values by hand, and the mapping isn't reversible.

### 2.2 Our envelope — canonical category via GTS URI

We use `modkit_canonical_errors::Problem`, which is RFC 9457 plus a **canonical category** encoded in the `type` field as a GTS URI:

```json
{
  "type":    "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "title":   "Failed precondition",
  "status":  400,
  "detail":  "insufficient funds: have 100, need 500",
  "context": {
    "resource_type": "account",
    "resource_name": "acc_123",
    "data": { "available": 100, "required": 500 }
  }
}
```

- The category is one of **16 canonical errors** (per [Google AIP-193](https://aip.dev/193)): `NotFound`, `AlreadyExists`, `PermissionDenied`, `FailedPrecondition`, `Unauthenticated`, ...
- All domain detail lives inside `context` (free-form `Map<String, JsonValue>`).
- Mapping to HTTP status and to `tonic::Code` is fixed and reversible.

**Strengths:** one canonical taxonomy across the whole workspace. Direct mapping to gRPC `Code` with zero loss. HTTP status is deterministic.

**Weaknesses:** at the *type level*, `InsufficientFunds` and `AccountFrozen` are indistinguishable — both map to `FailedPrecondition`. The client has to read `context["data"]` as `serde_json::Value` and parse it manually. **No exhaustive `match` on domain variants is possible.**

---

## 3. What breaks at the wire boundary

### Direction A — PRD-conformant client reads our server

```
Our server sends:                  PRD client parses:
─────────────────────              ─────────────────────
type:    gts://...failed_          Looks for "error_code" field.
         precondition.v1~          Not present.
status:  400                       Looks for "error_domain" field.
detail:  "insufficient funds:      Not present.
          have 100, need 500"      
context: { available: 100, ... }   Can only produce a generic Problem.
                                   No way to recover BillingError::
                                   InsufficientFunds { available, required }.
                                   Falls back to string-parsing `detail`.
```

The client receives the data but cannot reconstruct the typed variant.

### Direction B — Our client reads PRD-conformant server

```
PRD server sends:                  Our client parses:
─────────────────────              ─────────────────────
type:         https://errors/      Looks for GTS URI in `type`.
              insufficient-funds   Doesn't match `gts://...err.v1~`
error_code:   INSUFFICIENT_FUNDS   prefix; treats type as opaque string.
error_domain: billing.v1           Cannot map into any of the 16
status:       402                  canonical categories.
available:    100                  Falls back to CanonicalError::Unknown
required:     500                  or CanonicalError::Internal.
                                   Loses both the status semantics (402 vs 500)
                                   and the variant payload fields.
```

Same shape of failure: the data is there, the typed variant cannot be reconstructed.

### Why this matters

This isn't an "edge case caught in testing" — it's the **default outcome** any time the two systems talk to each other. The HTTP transport works (a 4xx is still a 4xx). The JSON parses. But the type-driven contract is broken: callers can't `match` on errors and must drop down to manual parsing, defeating the whole point of the SDK.

---

## 4. Trade-off matrix

| Aspect | PRD: `error_code` + `error_domain` | Ours: canonical category (AIP-193 + GTS) |
|---|---|---|
| Domain variants distinguishable on type | ✅ Yes (each variant has its own code) | ❌ No (multiple variants → same canonical category) |
| Exhaustive `match` on errors | ✅ Yes | ❌ No (forced to read `context` Map) |
| Variant payload as typed Rust fields | ✅ Yes (top-level envelope fields) | ⚠️ Partial (lives in `context["data"]` as `Value`) |
| Global standard / cross-service consistency | ❌ Each service has its own dictionary | ✅ One workspace-wide taxonomy |
| gRPC `Code` mapping (reversible) | ❌ Hand-rolled, lossy | ✅ Built-in, lossless |
| HTTP status mapping | ⚠️ Per-variant `#[status(...)]` annotation | ✅ Deterministic from category |
| Wire compat with PRD-conformant peers | ✅ By definition | ❌ Different envelope shape |
| Wire compat with other mesh services using canonical errors | ❌ Different envelope shape | ✅ Native |

Neither column is universally better. The choice depends on whether the workspace prioritizes **typed-domain ergonomics** or **wire-protocol uniformity across services**.

---

## 5. Three options for the team

### Option A — Extend our envelope to carry both

Add `error_code` and `error_domain` as optional extension fields **next to** the existing GTS URI in `Problem`:

```json
{
  "type":         "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "title":        "Failed precondition",
  "status":       400,
  "detail":       "...",
  "context":      { ... },

  "error_code":   "INSUFFICIENT_FUNDS",   ← new, optional
  "error_domain": "billing.v1"            ← new, optional
}
```

Implement `#[derive(ContractError)]` that:
- Generates `From<BillingError> for Problem` setting both the canonical category AND `error_code` / `error_domain`.
- Generates `TryFrom<Problem> for BillingError` reading `error_code` + `error_domain` first; falls back to category-based mapping if absent.

**Effect:** PRD-conformant peers get what they expect. Mesh services that only speak canonical categories keep working. Variant payloads can live in `context["data"]` or at top level (we pick one convention).

**Cost:** ~250 LoC macro + 2 fields on `Problem` + tests. ~0.5 day.

**Risk:** `Problem` envelope shape grows. Need to coordinate with any external consumer of `modkit-canonical-errors` if there are any outside our workspace.

### Option B — Amend the PRD

Update PRD #1536 §D4 + FR-contract-error-derive + FR-error-round-trip:
- Drop `error_code` / `error_domain` extension fields.
- Adopt canonical-category + GTS URI as the wire envelope.
- Spec the `#[derive(ContractError)]` macro to emit canonical-category mappings + `context` payload (no per-variant Rust-typed reconstruction; clients read `context` for detail).

**Effect:** one envelope across the workspace. PRD-conformant peers don't exist yet (PoC is the only one), so cost of breaking them is low.

**Cost:** PRD revision + stakeholder discussion. No code change.

**Risk:** loses the typed-variant ergonomic. Every error becomes `Problem` + manual context parsing on the client side. SDK consumers will hate this for domain-rich services.

### Option C — Status quo, document the divergence

Don't implement `#[derive(ContractError)]`. Document that our SDK uses canonical categories and `context`-based payloads, not PRD's extension-field shape. Mark D4 of PRD as "intentionally not implemented; superseded by canonical-errors design".

**Effect:** zero work. We knowingly ship a non-PRD-conformant solution.

**Cost:** zero.

**Risk:** future external consumers expecting PRD compliance hit the same wall. Long-term wire-compat debt unless we accept the design and update the PRD.

---

## 6. Recommendation

**Option A (extend envelope).** Reasons:

1. It's the only path that keeps both audiences happy: PRD-conformant peers and existing canonical-error consumers.
2. The cost is small (~0.5 day) and additive — no breaking change to existing services.
3. The SDK gets the typed-variant `match` ergonomic that the PRD authors clearly wanted, which is a real DX win for domain-rich services like `billing`, `payments`, `oncall`.
4. We don't need to litigate the PRD; we just deliver a superset.

Option B is correct only if the workspace consensus is that **wire uniformity beats typed ergonomics**. If that's the call, fine — but commit to it and update the PRD in the same patch.

Option C is technical debt with no upside. Avoid.

---

## 7. Decision needed

1. Pick A, B, or C.
2. If A: confirm we're OK growing `Problem` with two optional fields, and pick a convention for variant payload location (top-level vs `context["data"]`).
3. If B: who drives the PRD amendment, and on what timeline.
4. If C: someone owns documenting the divergence in `docs/adrs/`.

Bring this to the next architecture review.

---

## Appendix — Code shape under Option A

For reference, here's roughly what the macro emits (sketch, not final):

```rust
// Author writes:
#[derive(ContractError)]
#[non_exhaustive]
pub enum BillingError {
    #[error_code("INSUFFICIENT_FUNDS")]
    #[error_domain("billing.v1")]
    #[canonical(FailedPrecondition)]
    InsufficientFunds { available: u64, required: u64 },

    #[error_code("ACCOUNT_FROZEN")]
    #[error_domain("billing.v1")]
    #[canonical(FailedPrecondition)]
    AccountFrozen { reason: String },

    #[error_code("RATE_LIMIT")]
    #[error_domain("billing.v1")]
    #[canonical(ResourceExhausted)]
    RateLimit { retry_after_sec: u32 },
}

// Macro emits (server side):
impl From<BillingError> for modkit_canonical_errors::Problem {
    fn from(e: BillingError) -> Self {
        let (canonical, code, domain, ctx) = match &e {
            BillingError::InsufficientFunds { available, required } => (
                CanonicalError::failed_precondition().create(),
                "INSUFFICIENT_FUNDS",
                "billing.v1",
                serde_json::json!({ "available": available, "required": required }),
            ),
            // ... etc ...
        };
        let mut problem = Problem::from(canonical);
        problem.error_code = Some(code.into());
        problem.error_domain = Some(domain.into());
        problem.context.insert("data".into(), ctx);
        problem
    }
}

// Macro emits (client side):
impl TryFrom<modkit_canonical_errors::Problem> for BillingError {
    type Error = modkit_canonical_errors::Problem;
    fn try_from(p: Problem) -> Result<Self, Problem> {
        match (p.error_domain.as_deref(), p.error_code.as_deref()) {
            (Some("billing.v1"), Some("INSUFFICIENT_FUNDS")) => {
                let data = p.context.get("data").cloned().unwrap_or_default();
                Ok(BillingError::InsufficientFunds {
                    available: data["available"].as_u64().unwrap_or(0),
                    required: data["required"].as_u64().unwrap_or(0),
                })
            }
            // ... etc ...
            _ => Err(p), // unknown code — let caller handle as generic Problem
        }
    }
}
```

Open question for Option A: should `error_code`/`error_domain` and `context["data"]` payload be the canonical wire location, or should we hoist payload fields to the top of the envelope (closer to PRD §2.1)? Top-level is more PRD-like; `context["data"]` keeps our existing serialization shape stable. **Recommend `context["data"]`** — smaller wire change.
