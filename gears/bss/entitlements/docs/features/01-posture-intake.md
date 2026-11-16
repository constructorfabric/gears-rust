---
description: "Consume CommercialPostureV1 from Subscriptions, fence it by revision and digest, and journal every accepted, stale, replayed or conflicting delivery."
---

# Feature: Posture Intake

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-posture-intake-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-posture-intake`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Receive and fence a posture revision](#receive-and-fence-a-posture-revision)
  - [SDK contract and validation](#sdk-contract-and-validation)
  - [Adapter route configuration](#adapter-route-configuration)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Intake fences](#intake-fences)
- [4. States (CDSL)](#4-states-cdsl)
  - [Intake state](#intake-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Fenced posture intake](#fenced-posture-intake)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->


**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

**Release scope.** V1 provides core intake, schema/binding validation and replay. Nonempty service dispositions, `update_disposition`, output-delivery profiles and configured hooks are v2. Reject these in v1 before accepting a new desired head; do not silently strip fields. Qualification and purchase admission must already refuse such offers.

## 1. Feature Context

### 1.1 Overview

Entitlements accepts committed posture from Subscriptions and verifies its producer, scope, revision and digest. One local transaction saves the posture and derivation work for all enabled capabilities. Subscription identity stays in intake and internal correlation records. Adapter work uses scope, assignment and revision.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-posture-intake`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Publishes `CommercialPostureV1` and reads intake state per revision. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects stale, replayed and conflicting deliveries. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [commercial posture contract](../DESIGN.md#331-commercial-posture-contract).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** the `CommercialPostureV1` version agreed with Subscriptions (gate E-U06 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md)).

## 2. Actor Flows (CDSL)

### Receive and fence a posture revision

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-posture-intake`

**Actor:** Subscriptions workers, through durable SDK delivery and replay as described in the [commercial posture contract](../DESIGN.md#331-commercial-posture-contract).

**Success:** the posture revision is accepted and its derivation work is committed to the outbox, or the recorded intake result of an earlier delivery is returned.

**Errors:** unauthenticated producer, a caller that is not the principal named for the action, tenant scope outside the producer's authority, a scope answer that is not fenced to its target, unsupported contract version, stale revision, digest conflict.

1. [ ] - `p1` - Receive `CommercialPostureV1` through a Subscriptions worker calling `submit_commercial_posture`, including replay of a saved delivery, under the [commercial posture contract](../DESIGN.md#331-commercial-posture-contract). Authenticate the caller as the Subscriptions service principal named for the intake action. For that caller, obtain the single-target scope of the ordering tenant and the single-target scope of the resource tenant, and pass each answer through the returned-scope guard (ENT-D-04). The payer and seller axes grant no intake or provider authority (ENT-D-05, ENT-D-15). The separate Pricing read below uses its explicit catalog tenant and owner authorization. Intake returns `taken`, `replay`, `stale`, `conflict` or `refused` (ENT-D-12). Refuse an unsupported contract version or an unknown access, lifecycle action or resolution outcome before accepting it as the head or creating derivation work. An unfamiliar closure reason follows the narrow compatibility rule below; it is not an unknown outcome. Once the producer is authenticated and the journal target is authorized and identifiable, record the refused delivery in the journal with its reason and open a finding, so the refusal is quarantined and visible in the intake read; never treat a refused delivery as absent posture. - `inst-receive-and-fence-1`
2. [ ] - `p1` - Compare `(orderingTenantId, subscriptionId, revision)` and the payload digest with the intake journal under the [intake fences](#intake-fences). Validate the document and check equal-revision digest conflicts before processing its resolution or reference. Valid resolution evidence uses the separate generation fence below. A lower revision is stale: record it without deriving desired state. An equal revision with an equal digest is a replay: return the recorded intake result and do no new desired-state work. An equal revision with a different digest is a conflict: refuse it, record both digests and open a visible finding. A higher revision is accepted and supersedes the current head; the outcomes of earlier revisions are retained. - `inst-receive-and-fence-2`
3. [ ] - `p1` - Validate lifecycleAction and the complete serviceDispositions collection under DESIGN §3.3.1. Validate activation resolution by authenticated operation/preparation revision and digest. Persist a non-activation generation fence even when no manifest or commitment exists; fence delayed preparation before dispatch and retain earlier generation outcomes for recovery. Process preparation withdrawal and disposition/hold/schedule updates even when capability members or commercial status are unchanged; an unresolved component-to-assignment generation binding, or a missing notice where the frozen terms require one, never authorizes deletion. - `inst-receive-and-fence-3`

   Save the accepted posture and one derivation work item per enabled capability in the same local transaction. Pin the configured core recovery policy with that work for dependency reads before capability selection; [feature 06](06-reconciliation-recovery.md#recovery-matrix) defines its fields and escalation rules. The frozen snapshot selects quota, license and resource work. A `prepare_activation` revision also schedules manifest derivation under [initial readiness](02-initial-readiness.md).

Rules are read from that accepted snapshot, never from the live plan. Quantity is taken from each member; lots are provenance only and are not re-derived.

   A crash after this commit and before dispatch is recovered by outbox replay under the original identity, and no second derivation is created. From this step on, work carries `(namespace, resource tenant, assignmentId)` and the revision; `subscriptionId` does not leave intake.
4. [ ] - `p1` - Expose durable intake receipts through `read_posture_receipt` to Subscriptions and a safe operator projection through `list_posture_receipts` / `get_posture_receipt`. Each receipt identifies the exact delivery, its saved decision and whether its accepted revision is still current. Include replay metadata and the last delivery time; never expose an uncommitted `received` state. An accepted intake is not application; the desired state and the observed provider state are read through the capability features and [reconciliation](06-reconciliation-recovery.md). The read never implies usable access from an accepted posture. - `inst-receive-and-fence-4`

### SDK contract and validation

This section specifies the SDK boundary of DESIGN §3.3.1 and §3.3.8. SDK models stay transport-neutral. REST DTOs and codecs stay in the API layer. Every call carries `SecurityContext`; payload tenant fields never grant authority.

| Contract | Required fields and result |
| --- | --- |
| `submit_commercial_posture` | The owner SDK `CommercialPostureV1`, including identity, tenant axes, complete grants, accepted evidence and lifecycle context. Return `PostureIntakeResultV1`. Never flatten or discard owner extensions before checking the digest. |
| `PostureIntakeResultV1` | Durable `intakeId` when a scoped journal record exists, `orderingTenantId`, `subscriptionId`, received `revision`, `digest`, `operationId`, `result`, current accepted `headRevision`/`headDigest` when present, and safe `reasonCode` for stale/conflict/refused. A conflict also returns the submitted/stored digests and authorized finding reference. Results are `taken`, `replay`, `stale`, `conflict`, `refused`. A replay returns the original `decision`, receipt identity and current head without deriving again. |
| `read_posture_receipt(orderingTenantId, subscriptionId, revision, digest, operationId)` | Subscriptions-only SDK read under `posture_intake_read` for one ordering tenant. Exact delivery identity is required; a conflict cannot return the accepted receipt for another digest. Returns `PostureIntakeReceiptV1`, or `not_found` when no committed matching receipt exists. |
| `PostureIntakeReceiptV1` | `intakeId`, exact delivery identity, immutable `decision = taken / stale / conflict / refused`, safe `reasonCode` when not taken, original `decidedAt`, current `headRevision`/`headDigest` when present, `acceptedState = current / superseded / not_accepted`, `replayCount` and `lastDeliveryAt`. Conflict includes submitted/stored digests. No application status or access guarantee. |
| `read_posture_application_result` | Exact ordering tenant, subscription and revision. Return the same identity/digest, `status = pending / applied / failed`, `observedAt`, `findings` and `findingsTruncated`. Include at most 16 authorized findings in stable ID order. Unknown revision is not an empty successful result. |
| `read_service_fulfillment` | Exact ordering tenant, subscription and posture revision, with a cursor for the stable result snapshot. Return logical service assignments and correlated fulfillment evidence under [feature 05](05-resource-execution.md#work-order-sdk-contract). Use `service_fulfillment_read`; no native resource roster. |
| Accepted evidence | `catalogTenantId`, `acceptanceId`, expected `termsDigest`, exact acquisition/order-version/line provenance and expected entitlement snapshot identity/schema/count/digest. Read the complete immutable receipt through the Pricing SDK below. Missing, duplicate or changed entries block derivation; they never mean removal. |

**Posture delivery recovery.** Subscriptions retains the committed payload and its delivery identity, then calls `submit_commercial_posture` through its worker. If the call or reply is lost, it queries `read_posture_receipt` with the exact ordering tenant, subscription, revision, digest and operation ID. A saved receipt supplies the original decision. `not_found` allows replay of the same saved payload and identity; it does not prove that an in-flight call cannot still commit. An unavailable read remains unknown; retrying the original delivery is safe under the same intake fence. Never allocate a new revision just to recover a reply.

Once Subscriptions knows that intake accepted the revision, Entitlements owns application retries. Subscriptions uses `read_posture_application_result` to track application, rather than treating an intake receipt as provider success. A stale, conflicting or refused receipt retains its own decision and does not become acceptance. Entitlements does not pull missing posture revisions from a Subscriptions posture-read endpoint in v1.

Activation-resolution recovery is separate. Entitlements reads the immutable Subscriptions activation decision by the saved operation identity under feature 02. That read can settle preparation but cannot replace posture delivery or advance the desired head.

**Pricing receipt read.** Use the existing public `PricingAcceptanceV1.acceptance(ctx, AcceptanceQuery)` through ClientHub, not `PricingReadV1.resolve`, a current plan read or cross-gear storage. The SDK declarations are in [Pricing acceptance](../../../pricing/pricing-sdk/src/acceptance.rs). `AcceptanceQuery` contains `catalog = CatalogRef { tenant_id }` and `acceptance_id`; it returns one complete `AcceptanceReceipt`. No receipt-pagination method is assumed.

| Boundary | Required behavior |
| --- | --- |
| Reference from Subscriptions | E-U06 carries the catalog tenant, acceptance ID, expected terms digest, exact acquisition identity and entitlement snapshot evidence in authenticated posture provenance. A bare acceptance ID is insufficient. Preserve the original purchase reference; do not infer its catalog from the caller, resource tenant or current seller. |
| Entitlements to Pricing | A derivation worker calls with the Entitlements service principal's current `SecurityContext`, under ENT-D-04. The catalog tenant is the explicit lookup target, not an impersonated caller tenant. Do not forward an adapter identity or synthesize a seller context. |
| Pricing authorization | Pricing authorizes action `read` on `gts.cf.bss.pricing.acceptance.v1~` for the exact acceptance resource and catalog owner before lookup, including retries. Its PDP scope must be constrained to that target. E-U08 must grant this owner action; ordering/resource-tenant access or possession of a receipt ID does not grant it. No catalog-write or unrestricted catalog-read role is required. |
| Existing result | `AcceptanceReceipt` includes acceptance ID, request/terms digests, the accepted `NewSaleQuery` (order/version/line, plan/revision, original quantity and tenant axes), accepted/hold times and accepted bindings. The current receipt query identifies the catalog owner through its seller axis; validate that against the requested catalog. |
| Required E-U03 extension | The same receipt read must return complete frozen quota, grant and service evidence for the selected profile, with versioned schema, snapshot ID/count/digest. Required referenced descriptors, schemas, mappings and policies resolve within this retained evidence. References identify that saved content; they do not instruct Entitlements to fetch arbitrary URLs or current catalog objects. This extension is not claimed to exist in the current SDK. |
| Validation before use | Match acceptance ID, expected terms digest and exact order/version/line to the authenticated provenance. Check accepted plan/item/binding references, original purchase axes and resource scope against the saved acquisition association. Verify complete branch counts, schemas and canonical digests using the owner codecs. No receipt for another purchase can satisfy the member, even if its SKU or amount matches. |
| Missing or failed read | Denial, authorized not-found, unsupported schema, corrupt receipt or dependency failure remain distinct. They block new derivation and remain visible under the existing recovery/finding rules. None means an empty set, current terms, unlimited rights or successful application. |

Pricing owns the versioned receipt codec and terms digest. E-U03 supplies public SDK verification helpers for the added evidence; Entitlements imports no private Pricing wire or storage types. E-U03 must include the new entitlement evidence in that immutable contract without changing historical receipts. Quota/grant/service branches must come from the same acceptance; a price-only legacy receipt cannot satisfy a newly required branch. Authoritative historical receipt reads remain available after hold expiry, plan replacement or catalog withdrawal. They prove accepted content, not current purchase or activation eligibility. Entitlements calls neither `hold` nor `check_fulfilment` to reconstruct that history; existing owner activation guards remain unchanged.

Perform the SDK read outside the local transaction. On return, recheck the intake revision, source/generation fences and exact expected receipt reference before saving validated application evidence and derivation work. A superseded read cannot overwrite the current target. Persist its verification identity as `(catalogTenantId, acceptanceId, termsDigest, snapshotDigest)` with the local ordering-tenant intake/source association. Retain only the application evidence and provenance needed here; prices and billing logic remain owned by Pricing.

After a lost read response or restart, repeat the same authorized read and verify the same immutable digests. A new acquisition or changed acceptance reference must pass this read and validation, even if similar evidence is cached. Retained verified evidence may serve the same bound source under current local authority; it grants no permission to query another catalog or expose the receipt. Existing restrictions, revocation and operation recovery use their retained target/binding under ENT-D-13 without waiting for a fresh Pricing read.

E-U03/E-U06 cover the shared receipt/reference fields; E-U08 covers catalog-scoped receipt authorization. E-AC68 proves lookup identity, complete historical evidence, tenant separation and lost-response recovery. This feature adds no new Pricing read method or public Entitlements receipt endpoint.

**Intake findings.** Refusal and digest-conflict findings use `subject.kind = posture_intake` with the exact receipt identity and the ordering tenant as owner. Save the finding and its event intent with the receipt. [Feature 06](06-reconciliation-recovery.md#public-reads-and-operator-commands) defines the typed subject and scoped read path; no assignment is required.

**Receipt recovery.** Save the receipt with the intake decision, accepted head and derivation/outbox work in the same transaction. Stale, conflicting and refused deliveries also retain their scoped receipts. Authentication or scope failures that cannot safely identify an authorized journal target return an error, not a fabricated receipt. A reused operation ID with changed content cannot overwrite its earlier receipt.

`replay` is a delivery answer, not a new durable decision. It references the saved decision and creates no derivation work. A replay using a new operation ID retains an association to that same decision, so either operation can recover it. Repeated reads allocate nothing. Resolve receipt and current head in one consistent read: an earlier `taken` remains taken after a higher revision commits, while `acceptedState` becomes `superseded`. A conflicting delivery is `not_accepted` and does not change the accepted receipt. Operator disposition closes the finding, not the saved decision.

A read before commit returns `not_found`, not `received`. Subscriptions retries the original operation and content; absence does not prove refusal or authorize a new commercial action. A receipt confirms intake only. `read_posture_application_result` separately reports application. An unavailable read remains an error, not `not_found`.

**Operator inspection.** `list_posture_receipts(ctx, orderingTenantId, query: &ODataQuery)` and `get_posture_receipt(orderingTenantId, intakeId)` require `posture_intake_inspect` on the ordering tenant's intake resource. Platform operator status alone grants no access. Apply current PolicyEnforcer authorization and the single-target scope guard to every read. Resource-tenant permission alone does not grant access to ordering-tenant receipts.

Expose these methods as authenticated `GET /bss-entitlements/v1/posture-intakes` and `GET /bss-entitlements/v1/posture-intakes/{id}`. `id` is the opaque receipt UUIDv7, never a subscription ID. The safe DTO includes `id`, opaque `stream_id`, revision, submitted/stored digests, decision, accepted state, head revision/digest, safe reason, replay count and timestamps. It excludes subscription IDs, full posture, commercial terms, credentials and provider payloads. REST fields use snake_case; SDK fields above are semantic names. The stream ID groups receipts without exposing the subscription identifier.

Use the existing scoped tenant context and the [shared OData collection contract](06-reconciliation-recovery.md#odata-collection-contract). The SDK returns `Page<PostureIntakeInspectionV1>`. REST returns `items` and `page_info`, with default order `created_at asc, id asc` and 50/200 limits. Allowed `$filter` fields are `id`, `stream_id`, `decision`, `accepted_state` and `created_at`; allowed `$orderby` fields are `created_at` and `id`. Field names and types are declared once for SDK validation and OpenAPI.

Each page reflects one read snapshot; clients refresh to see head or replay changes. Reads return 200 with `Cache-Control: no-store`; malformed queries and invalid limits return 400, missing authentication 401 and missing method permission 403. Unknown or out-of-scope IDs return the same 404. Standard throttling and availability errors use RFC 9457. These routes expose only the diagnostic projection, never the internal posture record.

E-U06 covers receipt SDK adoption and lost-response recovery. E-U08 covers both read permissions. Feature delivery includes `gts.cf.bss.entitlements.posture_intake_receipt.v1~` for the service receipt and `gts.cf.bss.entitlements.posture_intake_inspection.v1~` for the safe operator DTO, plus OpenAPI read DTOs and scoped pagination fixtures. The inspection schema exposes only the listed fields; field selection cannot reveal internal fields.

**Repeated intake findings.** Upsert a refused/conflicting delivery finding in the same transaction as its scoped receipt. Its deduplication key is `(ownerTenantId, streamId, revision, submittedDigest, decision, reasonCode, conflictingDigest)`, with the last value present only for a digest conflict. A transport operation ID is not a new incident. Use a unique storage constraint so concurrent retries converge on one finding.

The finding keeps its original intake receipt as its immutable subject. Later matching receipts reference that finding; they do not change its owner or subject. Track first/last occurrence and a bounded counter without adding a new finding or notification on each retry. Counter saturation cannot wrap or fail intake. Keep receipt decisions and accepted-head state unchanged. A repeated incident does not reopen an acknowledged, resolved or dispositioned finding. A new revision, payload digest or failure reason creates a distinct incident. Correlated resolution still follows feature 06. Requests without an authorized journal scope use the security error path and cannot create a finding in a caller-chosen tenant.

**Service member reference (E-U06).** Members requiring service fulfillment carry the exact `serviceBindingRef` and committed quantity, logical scope, access and lifecycle authority defined in [feature 05](05-resource-execution.md#work-order-sdk-contract). Resolve capability, service type and profile from the frozen binding. Do not assume today’s posture wire type already carries them or infer execution from a SKU label.

**Member authority inputs (E-U06).** Subscriptions owns this proposed extension of `CommercialPostureV1`. Its SDK and GTS schema must carry the following values for each license member and any other capability that uses them. These are required contract semantics, not fields claimed to exist in the current SDK. Adoption must preserve them in the canonical posture digest and retained payload used for redelivery.

| Input | Source and validation |
| --- | --- |
| Purchase component reference | Each member carries the Subscriptions-owned `fulfillmentRef`, an opaque UUIDv7 for its purchased component. Preserve it in the canonical posture digest. Validate its retained association before derivation; it grants no authority. See the lifecycle and read contract in [feature 06](06-reconciliation-recovery.md#public-reads-and-operator-commands). |
| Accepted rule binding | The member identifies its binding in the accepted Pricing snapshot. Resolve its exact definition, version and digest, quantity rule, holder policy, validity policy and restriction/revocation policies. Never resolve the current plan. |
| Effective validity | Subscriptions supplies a UTC start and an exclusive UTC end. For grants, `validityPolicy.mode = finite` requires a finite end; `unbounded_allowed` also permits an explicit unbounded end. Other capability policies remain separate. An unbounded end must be explicit and allowed by the frozen policy; missing validity is not unlimited. A finite end must be after the start. Preparation authority is checked separately and cannot invent activation T. |
| Holder scope | Subscriptions supplies the logical holder kind, reference and scope generation within the resource tenant. For v1 grants, the frozen binding selects `tenant` or `logical_scope` under [feature 04](04-license-grants.md#grant-purchase-and-application-contracts). The supplied kind and scope must match; `tenant` identifies the resource tenant itself. Use owner SDK identifiers. Validate target authority; payload identity is not authorization. Neither payer nor seller nor the calling user is a default holder. |
| Member access | Subscriptions supplies `grant`, `restrict` or `revoke`, mapped to `Granted`, `Restricted` or `Revoked`. It also supplies the effective boundary and any restriction interval needed by the frozen policy. Document-level `access` identifies lifecycle context; it cannot replace member access. Preparation and withdrawal still require their generation authority. |

Reject ambiguous binding matches and values outside the accepted policy before creating new grants or increases. Do not fill missing values from a live plan or adapter state. Restriction and revocation of a known source retain the ENT-D-13 path below. A separate export right after cancellation needs its own accepted binding and explicit member authority; it cannot keep the normal service grant active.

The owner SDK supplies the posture's field types and enum spelling. Internal `Granted`, `Restricted` and `Revoked` map from `grant`, `restrict` and `revoke`. `prepare` and `withdrawn` require their operation/generation context. Unsupported behavior-bearing values produce a recorded refusal after authorization, not an empty grant set. The closure-reason exception below does not relax those checks. Quota claim, resolution and dispositions are conditional on the affected capability. Their absence blocks new preparation, grants and increases before dispatch. Restriction or revocation of a known source uses retained identity/binding and current commercial authority without reloading purchase or Billing evidence (ENT-D-13). Destructive cleanup still requires its exact resolution/disposition authority.

**Closure-reason compatibility (E-U06).** For `activationResolution.outcome = closed_without_activation`, the reason explains the Subscriptions decision. It does not select cleanup actions. Accept an unfamiliar reason code when the contract version, required fields, owner-defined reason syntax/bounds, producer authority, tenant, operation, preparation generation, decision version and digest are valid. Validate any supplied manifest and commitment bindings as usual.

The Subscriptions SDK must decode known reason codes and preserve an unknown code without rejecting the envelope. Its GTS schema and codec must support that extension. Retain the original value in the resolution, canonical digest, replay and recovery reads; never replace it with a generic `unknown` code. Apply the same rule to delivered posture and recovered resolutions. This is a proposed owner-contract requirement, not a local bypass of schema validation.

A missing or malformed required reason, an unsupported contract version, an unknown resolution outcome, an unauthenticated producer or a mismatched binding is still refused or quarantined. No terminal outcome is inferred. A valid unfamiliar reason alone opens no blocking finding. Record the confirmed closure and use only its existing generation-scoped cleanup authority. It does not prove effect settlement, authorize new actions or change the treatment of readiness failure reasons.

**Primitive rules for Entitlements-owned contracts.** IDs are opaque nonempty strings of at most 255 UTF-8 bytes, unless their owner schema sets a narrower bound. Revisions, generations and sequences are unsigned 64-bit values; they never wrap. JSON codecs represent these values as decimal strings to preserve exactness. Digests are 64 lowercase hexadecimal characters.

Times are UTC RFC 3339 instants. Optional fields are omitted, not null. Unknown behavior-bearing enum values fail validation. The Subscriptions-owned closure reason uses the lossless compatibility rule above. GTS identifies the schema; accepted evidence pins its version and digest. Contracts owned by other components, including the quota contract, keep their own codecs.

The readiness envelope sent to Subscriptions uses its agreed SDK codec, including signed 64-bit JSON integers. [Feature 02](02-initial-readiness.md#readiness-sdk-results) defines this exception and the required E-U06 changes. Never apply the decimal-string codec to that envelope.

For Entitlements-owned content, use canonical JSON. Sort object keys by UTF-8 byte order. Use exact decimal strings and no insignificant whitespace. Encode Unicode as UTF-8 without normalization. Escape only JSON-required characters, with lowercase hexadecimal for control escapes. Reject duplicate object keys before normalization.

Sort set arrays by stable ID and reject duplicate IDs. Keep array order when the schema says it matters. Hash the UTF-8 bytes with SHA-256, excluding the object's digest member. Desired-state hashes also exclude transport retries, request timestamps and observation state.

An operation's request digest covers all immutable request content, including target, authority and boundary. Changing authority needs a new operation. Rotating credentials under the same authority does not change semantic content.

**Shared plugin context.** Quota uses its accepted contract unchanged. License and resource calls carry `binding_id`, pinned `plugin_instance_id`, `contract_major`, capability profile reference/version/digest, `provider_target`, `provider_namespace` and `source_key`. The source key contains authenticated namespace, resource tenant and assignment ID. Requests also carry the exact scope identity/generation and authority evidence. The saved binding must match every field. `purpose` is `application` or `readiness_check`; neither grants extra rights. No subscription, payer or seller identifier enters this context.

Authenticate before any replay lookup. Validate the full request before accepting a new revision. Check a recorded operation's request digest before applying the stale-revision filter. Unknown operations and operations with unknown effects have different results. The capability features define those results. A transport error never becomes a saved refusal.

**Peer clients and startup.** Register the public Entitlements client in `init` without resolving Subscriptions. Declare no startup `deps` on Subscriptions and no required peer binding that would fail startup when it is absent. Publish schemas independently of the peer. The Subscriptions client is resolved lazily for operation recovery; a missing client produces an unavailable dependency result for that work.

Do not cache absence permanently. A later attempt must be able to find a newly available client without restarting Entitlements. Retain the original work identity, request and backoff state. Other local reads and independently authorized work remain available. Follow [DESIGN §3.4](../DESIGN.md#34-internal-dependencies) for both gears. Posture intake stores its receipt and work without a synchronous callback to Subscriptions.

**Registration and permissions.** Implement the GTS type and permission inventory of DESIGN §4.3, the three capability plugin types and the durable binding rules of §3.3.3. Feature 07 adds its hook plugin and hook payloads under §3.3.10; it reuses this registration infrastructure. Register schemas before advertising instances. Validate exact profile evidence and register each typed client under its full instance ID.

Preparation dispatch uses the durable start-marker and closure fence of [feature 02](02-initial-readiness.md#readiness-sdk-results). Save the marker before allowing a call that can change prepared rights or service state. Expired leases and missing replies cannot erase it. Preparation-enabled instances expose the settlement interface through the same saved binding. Service-only methods remain absent from REST. A PDP denial creates no tenant record and reveals no prior operation.

### Adapter route configuration

[Adapter selection sequence](../diagrams/adapter-instance-selection.svg) shows the calls for a new or existing binding in one editable SVG. The field definitions and validation rules are below.

Entitlements owns `AdapterRouteConfigurationV1`, with GTS type `gts.cf.bss.entitlements.adapter_routes.v1~`. The deployment operator supplies it through the gear configuration. V1 loads it at startup and has no route editing API. This feature owns its SDK/GTS schema, validation, persistence and selection helpers. Capability features use those helpers before their first dispatch; posture intake itself calls no adapter.

The configuration is a complete snapshot with `schemaVersion`, a positive `generation`, `digest` and `routes`. Generation is a checked unsigned 64-bit value, encoded as a decimal string. The digest is lowercase hexadecimal SHA-256 over canonical JSON without the digest field. An explicit empty route list disables new bindings. Missing configuration is a startup error, not a default route. Reject unknown fields and unsupported schema versions.

Each route has the following fields. SDK names are shown below; configuration fields use snake_case.

| Field | Type and meaning |
| --- | --- |
| `routeId`, `generation` | Deployment-local stable ID of 1 to 128 visible ASCII characters and a positive unsigned 64-bit generation, encoded as a decimal string. IDs are unique within a snapshot. Changed route content requires a higher route generation. |
| `match.capability`, `match.contractMajor` | One of the three existing capability interfaces: `quota`, `license` or `resource`, plus a positive integer contract major. It selects the exact plugin type in DESIGN §3.3.3. No fallback to another capability or major. |
| `match.profile` | Exact capability profile reference, version and digest from the accepted terms. Use its owner SDK types. This is not a plugin ID or a readiness profile. |
| `match.resourceScope` | Exact resource tenant UUID and tagged logical scope: `resource_tenant`, or `member_scope` with its authorized reference and scope generation. Use the same scope types as the accepted member and binding. V1 has no wildcard, arbitrary predicate or caller-supplied environment label. |
| `match.serviceTypeRef` | Exact descriptor ID/version/digest when the accepted service binding or service-specific metric requires one. Otherwise absent. Derive it from accepted evidence, never from a SKU label or GTS namespace text. It selects a service type, not a native instance. |
| `select.eligibleInstanceIds` | Nonempty unique list of full GTS instances of the selected capability type/major. One entry pins an instance. Several entries may serve only the same authorized target and profile. |
| `select.vendor` | Optional exact vendor label that further narrows the eligible list. Omission adds no vendor filter. It supplies no identity or authority proof. |
| `target` | Expected stable provider ID, namespace and authenticated observation-producer identity, using the binding SDK types. All are required. The selected adapter must confirm the target and actual provider scope identity/generation before binding persistence. No credentials, URLs or native resource IDs. |

All match fields use exact equality against accepted evidence and an authorized scope. Absence of `serviceTypeRef` matches only work without that type constraint. It is not a wildcard. Resolve exactly one route before instance selection. No match leaves the new binding unavailable with a visible configuration result. Overlapping matches are invalid even when one adapter is offline; do not choose the first route or the highest route generation.

The eligible list is an allowlist, not a failover order. Validate each candidate's schema, advertised profile/service type and accepted qualification evidence. Use ToolKit selection only within that set. Lower plugin priority wins; equal best priorities are ambiguous. The winner must have a scoped client and confirm the expected target. Missing client, target mismatch or unavailability does not select a substitute. E-U01/E-U02/E-U05 qualification still applies; route configuration cannot qualify an adapter.

**Readiness and purchase authority.** The deployment supplies immutable readiness profiles alongside route configuration. Subscriptions pins its readiness profile through the pre-purchase binding contract. Route matching uses each required capability's accepted profile and scope. A route contains no replacement readiness profile, hook list, quota amount, grant policy or service parameters. Unknown or incompatible profile evidence blocks new work; a route cannot repair it by changing purchased terms.

**Publication and recovery.** Validate the complete snapshot before activating it. Retain its generation/digest and immutable route records in the Entitlements store. The same snapshot generation/digest is a replay; changed content under that generation is a conflict. A lower snapshot generation cannot become active. Compare route content against retained records, including removed routes; a reused ID cannot reset its generation. Reject duplicate IDs and overlapping match keys before activation. Invalid configuration fails startup without replacing the active snapshot.

Activate a valid snapshot atomically under the existing secure storage boundary. New-binding transactions compare their selected snapshot generation/digest with the active snapshot and recheck eligibility. A race restarts selection. Old workers cannot save bindings from a retired snapshot; they must load the active snapshot or stop creating new bindings. Save route ID/generation and snapshot generation/digest with the verified `AdapterBinding`, assignment and delivery intent. No provider effect may precede that commit.

Retiring a route or changing priorities affects only new bindings. Existing work, restrictions, cleanup and result recovery use the saved instance, target and authority even if the route is removed. Keep referenced clients and old bindings available for that recovery. A provider identity mismatch blocks delivery and opens a finding; it never authorizes migration. Reverting deployment content requires a higher snapshot generation and, for changed routes, higher route generations. Cached selections use the same version and scope checks as fresh selection.

The adapter design owns OSS connections, credentials, native service mapping and receiving components. This route contract names the expected integration boundary only. E-AC70 covers configuration validation, purchase-profile compatibility, publication races and recovery after route removal.

## 3. Processes / Business Logic (CDSL)

### Intake fences

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-intake-fences`

The journal head of a target is the highest accepted revision for `(orderingTenantId, subscriptionId)`. Every delivery is compared with that head by revision first and by digest second. The operation id identifies a retry; it substitutes for neither the revision nor the digest.

| Incoming revision vs head | Digest vs head | Outcome | Journal record |
| --- | --- | --- | --- |
| No head yet | any | Accepted; becomes the first head. | Head with revision, digest, operation id and time. |
| Lower | any | Stale; no desired-state application. | Stale delivery with reason; head unchanged. Valid resolution evidence is processed separately. |
| Equal | equal | Replay; no new desired-state work. | Replay count and last delivery time on the existing result. |
| Equal | different | Conflict; refused, visible until an audited disposition. | Conflict with both digests and reason; head unchanged; finding opened. |
| Higher | any | Accepted; supersedes the head. | New head; the previous head becomes `superseded` and keeps its outcomes. |

The digest is lowercase hexadecimal SHA-256 over the normalized canonical JSON payload, excluding its digest field (ENT-D-12, Subscriptions SUB-D-91). Use the owner SDK codec and normalization; Entitlements must not define a second posture codec. Application results belong to their own revision and cannot mark a newer head complete.

**Separate resolution fence.** After authentication, scope, schema and digest validation, process `activationResolution` independently of the desired-head fence. A valid stale or replayed posture can settle its exact preparation generation without deriving its old grants or replacing the current head. Journal a missing `resolutionRef` target with durable recovery work in the same transaction. Refused or conflicting documents supply no resolution authority. Delivered and recovered results use the same identity, binding and conflict checks in [initial readiness](02-initial-readiness.md#readiness-manifest-and-observation-envelope). A recovered historical activation cannot restore rights after a newer restriction or cancellation.

Journal entries, including stale and conflicting deliveries, are kept for at least the accepted delayed-message and retry horizon stated in [DESIGN §3.7](../DESIGN.md#37-database-schemas--tables); if a stale sender cannot be retired conclusively, the fence stays.

## 4. States (CDSL)

### Intake state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-intake`

A durable delivery receipt has one immutable decision: `taken`, `stale`, `conflict` or `refused`. There is no durable `received` state. Before the transaction commits, no receipt or accepted posture exists; a crash is recovered by replaying the original delivery.

Accepted posture state is separate: `current` while that revision/digest is the accepted head, then `superseded` after a higher revision is accepted. Deliveries that were not accepted report `not_accepted`. A conflict is a separate receipt and never changes the earlier accepted revision. Replay updates delivery metadata without replacing the original decision or deriving more work.

These are intake observations. They are not subscription statuses, not application states and not a replacement for the state machine of any provider. A later accepted revision supersedes the target but retains earlier outcomes. A refused or lost delivery never silently broadens access and never narrows it either; the desired head is unchanged until a higher revision is accepted.

## 5. Definitions of Done

### Fenced posture intake

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-posture-intake`

Implement producer and scope checks, the ordering table, and durable intake history. Save the posture and derivation work together with the outbox. Expose intake state per subscription revision after restart or response loss.

Before enabling intake, prove the specified contract version and owner-defined canonical digest with Subscriptions under E-U06. Verify replay, conflict and lost-delivery cases. Intake calls no adapter. Derivation never reads a live plan.

The shared foundation supplies public and capability-plugin SDK contracts, versioned GTS schemas, host registration and durable adapter bindings. [DESIGN §3.3.3](../DESIGN.md#333-adapter-capability-profile) defines these contracts. Infrastructure owns registry and ClientHub access. Capability features select adapters and dispatch work. [Extension hooks](07-extension-hooks.md) supplies its own typed plugin contract through this foundation; it is not a fourth rights capability. The main gear imports no concrete plugins and requires no unused optional plugin. Register and recover peer clients under the startup contract above; qualify independent startup and later peer availability under E-AC77.

Implement the specified identifiers and metadata in [GTS contracts and discovery](../DESIGN.md#gts-contracts-and-discovery) as versioned SDK schemas. Implement [Adapter route configuration](#adapter-route-configuration), including schema validation, immutable snapshots, atomic activation, binding fences and cache invalidation; the capability features use it before creating bindings.

The shared SDK also defines [fulfillment-output descriptors and public read/retrieval contracts](../DESIGN.md#337-fulfillment-outputs). Capability handlers own output production/persistence; shared infrastructure supplies scoped storage and authorization. Intake does not issue licenses or retrieve their protected content.

**Implements:** `cpt-cf-bss-entitlements-flow-posture-intake`, `cpt-cf-bss-entitlements-algo-intake-fences`, `cpt-cf-bss-entitlements-state-intake`.

## 6. Acceptance Criteria

- [ ] Start Subscriptions and Entitlements in either order, then start each with the peer omitted but its own required infrastructure present. Both register their public client and serve local reads without a dependency cycle or startup wait. Required preparation remains unavailable, never successful (E-AC77).
- [ ] Add or restore the peer after a failed client lookup. Queued calls recover with the original operation, revision and payload without restarting the caller or duplicating work. No startup or database transaction waits for the peer. Posture intake never waits for a readiness callback (E-AC77).

- [ ] Route snapshots validate against the SDK/GTS schema. Missing configuration, unknown fields, duplicate IDs, overlapping matches and reused generations with changed content fail without replacing the active snapshot (E-AC70).
- [ ] Exact authorized scope and accepted profile/service type select one qualified integration. Payload labels, unknown service types, an unavailable winning instance or tied priorities cannot select another target or weaken the profile.
- [ ] A route update racing binding persistence cannot save the old selection. Restart or an old worker cannot reactivate a retired snapshot; replay of the active snapshot remains idempotent.
- [ ] Removing a route prevents new bindings while existing revisions, restrictions, cleanup and recovery retain their original instance and target. A changed provider identity blocks delivery without fallback.

- [ ] With distinct ordering, resource and catalog tenants, Pricing receipt lookup succeeds only with the exact acceptance read permission in the catalog tenant. Caller/resource scope or a known receipt ID is insufficient (E-AC68).
- [ ] A returned receipt with the wrong order/version/line, terms digest, catalog owner or required evidence branch cannot derive a grant, quota or service target. Matching SKU/amount is not sufficient.
- [ ] A lost receipt response and restart recover the same immutable snapshot after plan replacement or hold expiry. No live plan read, new hold or reconstructed acceptance is used.
- [ ] Missing, denied, corrupt or unsupported evidence remains visible and cannot become an empty set. A read completed after posture supersession cannot replace the newer desired target. Known-source restrictions and recovery still use retained bindings.

- [ ] Concurrent repeats of one refused or conflicting payload, including new transport operation IDs, keep one finding and its original receipt subject. Occurrence metadata advances without duplicate notifications. Disposition remains intact; a new revision or changed digest/reason is a separate incident.
- [ ] A lost request is retried with the same saved payload and identity after `not_found` or an unavailable intake read. A racing original request still creates one intake decision and one set of derivation work. No Subscriptions posture-read endpoint is required.
- [ ] A committed intake loses its response. The exact SDK read recovers its receipt without new derivation. After a higher revision commits, the original decision remains taken and its accepted state is superseded (E-AC62).
- [ ] Accepted and conflicting deliveries of the same revision return their own receipts by digest and operation. Changed content cannot replace an operation's original receipt. Stale and refused deliveries remain readable.
- [ ] A read before commit returns not found, never received or applied. Retrying the original delivery is safe. Replay through another operation retains the original decision association.
- [ ] Receipt inspection uses the shared OData/Page contract, declared filter/order fields and 50/200 bounds. SDK and REST agree; plain query keys and foreign or changed-query cursors are refused.
- [ ] Operator inspection requires ordering-tenant permission, hides internal posture and subscription identifiers, and enforces tenant-bound pagination. A resource-tenant role or guessed receipt ID grants no access.

- [ ] A valid `closed_without_activation` resolution carries a new owner-valid reason code. Intake accepts the closure, retains the exact reason and digest, fences that generation and creates only the authorized cleanup work. No reason-specific behavior or automatic settlement is inferred.
- [ ] Delivered posture, operation recovery, restart and replay preserve the same unfamiliar reason code. Changing that code under the same immutable resolution conflicts.
- [ ] Missing or malformed reason, unsupported contract version, unknown outcome, wrong tenant/generation, invalid authority or mismatched commitment still refuses or quarantines the resolution without authorizing cleanup.

- [ ] The same revision with the same digest is delivered twice, once more under another transport key: every delivery returns the same intake result, the journal holds one head and exactly one set of derivation work items exists in the outbox.
- [ ] A delivery with an equal revision and a different digest is refused: the head and its derivation work are unchanged, the conflict is visible with both digests in the intake state read and in a finding, and no derivation work is emitted for the refused payload.
- [ ] Revision N is lost in delivery and recovered under the same identity and digest. The original delivery later returns a replay result. A crash after acceptance recovers the same derivation work without a second application.
- [ ] Capability schemas derive from the ToolKit plugin base; registered instance IDs match metadata and scoped-client keys. Unsupported profiles, failed registration and missing clients cannot produce an available adapter.
- [ ] For the same product, authorized EU and US scopes select their respective routes and clients without sharing a cached selection. Overlapping routes and tied best priorities fail explicitly; caller-supplied instance IDs cannot override routing.
- [ ] A route-generation, priority or conformance change invalidates selection for new bindings. A change racing selection is revalidated before binding persistence; existing bindings and recovery still resolve their original instance and target.

- [ ] Normalized equivalent posture payloads produce the same owner-defined digest. A changed field with an old digest is refused before advancing the head. Unknown schema/enum values cannot create an empty target.
- [ ] The same SDK call through local and remote clients enforces the same producer and target scope. No service-only method appears in public OpenAPI.
