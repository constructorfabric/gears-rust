---
description: "Proposed Accounts mechanisms: party identity, consent records, the one-transaction account-use grant, authorization classes, fresh current-view validation and the GTS contract inventory."
status: proposed
---

# Technical Design — BSS Accounts

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 SDK and management contracts](#38-sdk-and-management-contracts)
  - [3.9 Consumer outcome and recovery protocol](#39-consumer-outcome-and-recovery-protocol)
- [4. Additional context](#4-additional-context)
  - [4.1 GTS schemas, extension and registration](#41-gts-schemas-extension-and-registration)
  - [4.2 Permissions, events and errors](#42-permissions-events-and-errors)
  - [4.3 Review and delivery verification](#43-review-and-delivery-verification)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-design-architecture`

## 1. Architecture Overview

### 1.1 Architectural Vision

| Responsibility | Proposed owner | Boundary |
|---|---|---|
| Billing-party identity, identifier evidence, representation, versioned billing profiles | BSS Accounts: `LegalParty` | Opaque id; evidence under configured namespaces; one representing tenant initially; no external registry integration, no KYC |
| Billing account identity, fixed binding and lifecycle | BSS Accounts: `BillingAccount` | One payer tenant, seller tenant, issuer party, debtor party and billing currency; no new RBAC scope |
| Consent to use an account (BillingArrangement) | BSS Accounts: `AccountUseOffer` (payer-owned) + `AccountUseAcceptance` (buyer-owned) | Effective only on matching immutable terms version; either side stops new uses |
| Authorization of one exact use | BSS Accounts: `AccountUseGrant` | One transaction; linearization point against revocation; attribution authority only |
| Seller-to-issuer eligibility and setup coordination | BSS Accounts: `IssuerBinding` | Ledger owns books, accounting and book resolution |
| Negotiated seller–buyer terms | Contracts & Agreements | One seller/issuer and buyer/debtor pair per v1 agreement (Q9) |
| Billing assignment of a purchase or leg | Orders; Subscriptions after activation | Versioned grant evidence; effective-dated segments |
| Invoice issuance and financial postings | Invoicing; Ledger | Consume party, grant and assignment evidence; payer never derived from the tenant tree |
| Tenant identity and hierarchy | Account Management | Independent of BSS |
| User permissions, service-principal identity and roles | Platform authorization | Tenant-scoped user grants; narrow service roles with mandatory per-call single-target scopes (§3.4) |

Details below are candidate contracts for review, not delivered SDKs.

### 1.2 Architecture Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-accounts-fr-party-identity` | Opaque party id, namespaced identifier evidence, alias history, versioned profiles, representation record (§3.1) |
| `cpt-cf-bss-accounts-fr-account-binding` | Fixed binding validated against grant proposals (§3.1, §3.6) |
| `cpt-cf-bss-accounts-fr-arrangement` | Two tenant-owned consent records matched by terms version (§3.1) |
| `cpt-cf-bss-accounts-fr-use-grant` | One-transaction `authorize_use`, provisional intent, replay by attempt id (§3.6) |
| `cpt-cf-bss-accounts-fr-spending` | Coverage policy and optional approval bound to the terms digest (§3.1, §3.6) |
| `cpt-cf-bss-accounts-fr-party-separation` | Debtor party, payer account, recipients and instruments as separate references |
| `cpt-cf-bss-accounts-fr-profiles` | Versioned profiles; Invoicing issuance snapshots external (§3.1) |
| `cpt-cf-bss-accounts-fr-seller-setup` | `IssuerBinding` plus Ledger provisioning and book resolution (§3.4) |
| `cpt-cf-bss-accounts-fr-representation` | Action authorization plus party evidence; two acts, one actor identity |
| `cpt-cf-bss-accounts-fr-history` | Revocation is prospective; grant history retained (§3.6) |
| `cpt-cf-bss-accounts-fr-currency-pool` | Compatibility dimensions only; pool is Ledger state |

#### NFR Allocation

| NFR ID | Allocated to | Verification |
|---|---|---|
| `cpt-cf-bss-accounts-nfr-isolation` | PDP/PEP, SecureORM, response schemas | Negative detail/list/count/export checks |
| `cpt-cf-bss-accounts-nfr-target-scoping` | Per-call RootOnly scope guard; separate payer/buyer/home repository scopes (§3.4) | Overbroad/unfenced-OR provider responses, incorrect recipient and 100,000-tenant verification |
| `cpt-cf-bss-accounts-nfr-evidence` | Versioned records and grant history | Historical replay after edits and revocation |
| `cpt-cf-bss-accounts-nfr-current-view-freshness` | Grant/offer status reads used by consumers per request, page or batch (§3.3) | Revocation concurrent with reads; delayed and out-of-order events; source outage |

#### Key ADRs

- [Ownership](../../docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md) — ownership and tenant-scoped authorization.
- [Resale](../../docs/ADR/0003-cpt-cf-bss-adr-multi-tier-resale-model.md) — one service subscription with commercial legs.

Both are proposed and explicitly registered as shared, docs-only CFS artifacts alongside the commercial PRD/DESIGN. Accounts retains its normal gear registration.

### 1.3 Architecture Layers

SDK/application boundary → domain checks for party, account, consent and grant → gear-owned persistence through SecureORM. Platform authorization, runtime-owned migrations and inter-gear SDK calls follow [ToolKit](../../../../docs/toolkit_unified_system/README.md). The proposed layout is `accounts` plus `accounts-sdk`, following the nested BSS gear layout. PostgreSQL stores authority records; the gear exposes SDK-first ClientHub interfaces and REST adapters. REST-only request/response DTOs stay in the gear and map explicitly to transport-neutral SDK models; operation specs declare authentication, rate limits, success responses and canonical Problem responses. No separate Accounts worker service or shared BSS database is required.

## 2. Principles & Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-principle-evidence-not-permission`

Account-use evidence is a commercial input, not an access grant. The caller's action permission and the party evidence must both be independently valid; event delivery is transport, never authorization. Rationale: the [ownership ADR](../../docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md).

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-constraint-bounded-owner`

Accounts owns neither billing assignments, book resolution nor money state. Cross-owner locking and reads inside `authorize_use` run under the Accounts service contract of §3.4: a PDP decision for the authenticated calling service principal and the named operation, never a raw database connection, a hand-built privileged security context or a manually constructed or `allow_all` access scope; "the gear owns its tables" is not tenant authorization. No existing gear contract is superseded by this draft. Rationale: the [ownership ADR](../../docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md).

## 3. Technical Architecture

### 3.1 Domain Model

**LegalParty.** `party_id` (opaque, never reused or merged), `representing_tenant_id`, `PartyIdentifier(namespace, value, verification{status, evidence_ref, verified_at, by})`, `PartyAlias(from, to, declared_at, by, evidence)`, `PartyProfile(profile_version, legal name, addresses, tax identifiers by namespace, confirmed_at, by)`. Namespaces declare their canonical-identity semantics (one identifier per entity or not) and normalization as deployment configuration; comparisons happen only between verified identifiers in the same accepted namespace. Same = same party id, a shared verified identifier in a canonical namespace, or a confirmed alias. Distinct = verified evidence of distinctness under a canonical namespace, or an explicit authorized distinctness assertion with evidence; different raw identifiers or a missing alias prove nothing, since one entity may hold several valid identifiers. Everything else is **undetermined**, and an undetermined pair supports no Q13 distinctness, transfer same-debtor or pool-compatibility decision (fail closed). Historical facts keep their original party ids whatever is later learned. An alias is a comparison aid: it never re-keys history, never merges records and never authorizes a transfer or pool merge by itself. Reassigning representation is an authorized setup action with evidence. Invoicing stores issuance snapshots of `(party_id, profile_version)` values; these are copies by design, distinct from the single authoritative profile.

**BillingAccount.** `account_id`, `account_version`, payer tenant (owner), seller tenant, `issuer_party_id`, `debtor_party_id`, `billing_currency`, `issuer_binding_id`, state (`active`, `blocked`, `closed`), lifecycle evidence. Blocked stops new grants; closed additionally ends future use after assignments were transferred or ended; neither deletes history. The account stores no party profile fields and no balance.

**AccountUseOffer (payer-owned) and AccountUseAcceptance (buyer-owned).** The offer carries `offer_id`, `account_id`, `buyer_tenant_id`, scope (catalog/product scope, optional resource-tenant restriction), limits, `approval_mode`, `coverage_policy{scope: single_period | full_term, max_term, per_use_amount_or_quantity, change_rule}`, validity, and a monotonic `terms_version` that changes with any edit; revocation sets `revoked_at`. `max_term` limits the length of the accepted commitment term in the product's supported term semantics; it is neither a billing cadence nor a calendar funding end. The end of the offer's validity, like revocation, stops new authorizations and withdraws no issued grant. Revocation, withdrawal and blocking take effect when they commit (v1 has no future-dated revocation); `revoked_at` and `withdrawn_at` are recorded evidence. The acceptance carries `acceptance_id`, `offer_id`, `accepted_terms_version`, consent evidence, validity, `withdrawn_at`. **Effective arrangement** = offer unrevoked and valid ∧ acceptance present, unwithdrawn and valid ∧ `accepted_terms_version == offer.terms_version`. The business name BillingArrangement denotes this effective match; `arrangement_id ≡ offer_id` in downstream references.

**AccountUseApproval (payer-owned, optional).** `approval_id`, account and optional offer, `consumer_principal`, immutable `use_ref` and version, `terms_digest` of the exact proposed use, approver evidence; required when `approval_mode = per_use`. Authorization compares the full consumer/use tuple as well as the digest; a different purchase cannot reuse the approval even if its commercial terms match.

**AccountUseGrant (payer-owned).** `grant_id`; `consumer_principal` (the authenticated service principal that requested it) and `attempt_id` (consumer-minted, unique per principal); `use_ref` (consumer gear, use kind, document or leg id, immutable version); the **pinned terms**: `terms_digest` over the explicit fields `buyer_tenant_id`, `seller_tenant_id`, `account_id`/`account_version`, routing-only `account_owner_tenant_hint`, `debtor_party_id`, product and catalog scope, resource lineage, `billing_currency`, resolved commercial/payment terms and their source versions, for fixed components `quantity` and amount, for usage components `tariff_ref` (price binding or accepted price rule identity **and version**), metering identity and units, `consumption_policy`, and `coverage{scope, accepted_term, per_use_quantity_or_amount?}` (the accepted term is defined below); pinned `offer.terms_version`, `acceptance_id`/`accepted_terms_version`, `approval_id?`, `issuer_party_id`, `issuer_binding_id`, Ledger `book_id` and `book_resolution_version` as supplied by the consumer from Ledger's resolution; state (`authorized → used → ended`, `authorized → ended`, or `authorized → released`) with monotonic outcome recording. A refusal creates no grant: it is recorded on the attempt, so that replay of that attempt returns the refusal. No final usage amount is pinned. **At most one grant per `(consumer_principal, use_ref, version)`, ever, including a released or ended one**: a second `attempt_id` for the same immutable use returns the existing grant (or its recorded outcome) instead of creating another grant after a lost reply; a new use needs a new version, and after a refusal the consumer's next try is a new use version. Renewal and transfer use identities are fixed in §3.8; versions are allocated durably by Subscriptions, never inferred from retries or the current chain version. Replay and outcome mutation are authorized for the owning consumer principal before any outcome is disclosed. Batched status reads by another consumer (for example Subscriptions reading an Orders-issued grant) require a separate, explicitly bounded service-read permission and validated source/use lineage; they confer no replay or mutation authority. Outcomes are monotonic: `authorized → used → ended`, `authorized → ended` (an admitted use that ended before its delayed `record_use` arrived) and `authorized → released` (a use that was never admitted) only; a delayed `record_use` after `ended` is recorded as late evidence and does not reopen the grant; `released` and `ended` outcomes can never authorize a new admission. The grant authorizes account attribution for exactly the pinned use; it is not a reservation of money, a debt, a supplier or customer acceptance, an instrument mandate or a document-recipient choice.

**Quota funding evidence.** A `quota_controlled` proposal pins both explicit acceptance of tariff-based amounts and the accepted rating-lag/overshoot exposure, plus quota entitlement/scope/units, limit behavior and stale-state handling. It is the quota-enforced variant of tariff-based funding, not an exception to consent. Missing acceptance or unsupported quota semantics refuses authorization; a quota cannot be used to reject already incurred eligible charges. The consumer and Accounts retain these fields in the canonical digest.

**Accepted terms and defaults (selected for v1).** The purchase consumer resolves effective commercial/payment terms through the owning Products/Pricing/Contracts contracts and permitted account defaults before requesting a grant. It retains the resolved terms and their source versions on the purchase/use and includes them in the canonical terms digest. Account defaults supply only otherwise unspecified, defaultable values; unresolved conflicts fail admission. Accounts validates the resolved proposal against its fixed account binding, effective offer/acceptance, scope, coverage, tariff and approvals; it neither authors negotiated terms nor lets them override funding consent. A default change affects only subsequent authorized versions, never an admitted use. Amendment/renewal retains the existing version and requires the applicable new authority rather than re-reading live defaults for historical billing. The terms envelope, provenance and consumer contract are specified in §3.8 and commercial DESIGN §3.8.

**Coverage: the accepted term (selected for v1).** Coverage pins the product term that the consumer accepted and froze for the use: the earliest start, the term in the product's supported semantics (a commitment length, or a fixed-calendar term where the product defines one) and the frozen anchor geometry, by value or by the consumer's terms digest. It is not a separately configured calendar funding end, and v1 has no such mode. Accounts checks at authorization that this term is within the offer's `coverage_policy`, and keeps no timer. For an initial purchase Subscriptions derives the calendar interval under the existing product rules and the actual service start, never backdated before it, and records it on the billing assignment; this is not a general re-anchoring of the term to the activation instant. A renewal use covers the already fixed interval of the next term. Orders' receipt deadline (`hold_until`, [D-191](../../orders/docs/DESIGN.md#contract-03-commercial-deadline)) constrains admission and the activation instant chosen through Pricing; it is not a deadline for the actual service start. The compatibility predicate is specified in commercial DESIGN §3.8, adopting [Orders activation clocks](../../orders/docs/DESIGN.md#contract-06-activation-clocks): unsupported geometry fails activation and requires a newly accepted execution; no worker rewrites accepted terms or extends the product fulfillment deadline.

**IssuerBinding (seller-owned).** `(seller_tenant_id, issuer_party_id)`, version, confirmation evidence, Ledger provisioning reference. Ledger's book resolution is referenced, not stored as authority.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-component-authority`

The Accounts authority exposes one consistent source for party identity, account usability, consent state and grants. It delegates financial posting, book resolution, service lifecycle and document issuance to their owners.

| Concern | Proposed responsibility boundary |
|---|---|
| Party identity and profiles | Accounts (representing tenant confirms); AM metadata may prefill; Invoicing snapshots at issuance |
| Consent and approvals | Accounts; the Approvals gear is a read-and-route inbox that forwards decisions, Accounts retains the decision state |
| Grants | Accounts transaction; consumers hold provisional intents and record outcomes |
| Invoice-cycle preference | Accounts records the account preference; Invoicing owns the effective schedule and cutoff and groups compatible ready lines within the same account; subscription separate-invoice preference is respected; issuer, seller, debtor, payer, currency, book, a single matching contractual due date, payment terms, taxes and recipient disclosure constrain grouping |
| Settlement pools, allocation, designations | Ledger |
| Payment instruments and mandates | Payments |
| Book resolution | Ledger, deterministic and versioned, pinned by consumers on assignments and facts |

### 3.3 API Contracts

SDK capabilities; the selected wire contracts and management routes are specified in §3.8:

- `evaluate_account_use(ctx, proposal) → EligibilityVerdict` — read only; never a promise.
- `authorize_use(ctx, attempt_id, use_ref, proposal) → GrantOutcome` — the one-transaction authorization of §3.6; idempotent by `(consumer_principal, attempt_id)` and limited to at most one grant per `(consumer_principal, use_ref, version)`; the proposal carries the explicit pinned fields of §3.1 and, for a sponsored use, the exact `offer_id`, and Accounts computes and stores the digest.
- `record_use(ctx, grant_id, use_ref, outcome: admitted, at)`, `record_use_ended(ctx, grant_id, ended_at, reason)`, `release(ctx, grant_id, reason)` — consumer-driven outcome recording; idempotent.
- `grant_status(ctx, grant_ids[])`, `offer_status(ctx, offer_ids[])` — batched authoritative reads used by consumers for fresh current-view validation.
- Party, account, offer, acceptance, approval and issuer-binding management operations for own-tenant users.

**Fresh current-view validation (selected first implementation).** A consumer serving a current view, list page or export batch that depends on an Accounts fact calls the batched status read for the distinct bases of that page or batch in the same request, after its own PDP evaluation for that request. Two kinds of basis exist and are validated differently: an **offer or invitation basis** (offer summaries, pending acceptances) is current only while the offer is unrevoked, unexpired and the account unblocked; a **grant-backed purchase or assignment basis** (payer purchase and leg views, document views) is current while the grant is `authorized` or `used` and its coverage (the accepted term, with the interval recorded on the billing assignment once the service has started) includes the interval shown — revoking the offer or blocking the account stops new uses but does not withdraw covered billing inquiry, whereas `released`, `ended` beyond coverage or an unavailable status withholds the row. A surviving grant-backed view is strictly limited to the covered billing facts (service identity, quantity, period, amounts, assignment, grant, documents); it never carries operational detail. No positive disclosure decision is cached; a lease or cached bound is future work after the authorization owner agrees on a supported bound. This is not atomic with a revocation committed in another database: the selected per-authority read checkpoints, version-vector validation and in-flight semantics are specified in commercial DESIGN §3.11 and require adversarial verification. In the same request the consumer must also read the authoritative source-use status from Orders (admission/termination) or Subscriptions (effective assignment/service end). It checks the current slice against both grant coverage and the source effective end. Delayed `record_use_ended` cannot keep that slice current merely because Accounts still says `authorized` or `used`; missing/unavailable source status fails closed. Historical financial evidence of authorized uses is served under a separate permission and is not withheld by consent changes.

Errors distinguish: not eligible (`FailedPrecondition` with reason), stale acceptance or changed terms (`FailedPrecondition(acceptance_stale)`), attempt replay with different digest (`AlreadyExists(payload_mismatch)`), unavailable authority (`Unavailable`, fail closed), and unauthorized caller, without leaking another party's data. A timeout at the consumer is an unknown outcome: replay by the same `attempt_id`.

### 3.4 Internal Dependencies

**Self-funded branch (buyer = payer).** When the proposal's buyer tenant is the account's payer tenant, no offer and no acceptance exist and none is fabricated: `authorize_use` locks the account (and the approval when the account's own policy requires per-use approval), and validates the account state, `account.debtor_party_id` = the buyer's accepted debtor party, seller and issuer binding, currency, product scope and lineage, and the pinned tariff, coverage and consumption policy against the account owner's own accepted purchase terms; the buyer's purchase permission in its tenant is the authority, checked by the consumer's PDP evaluation and attested as the order's owner tenant. The grant, replay, one-grant-per-use and outcome rules are identical to the sponsored branch.

**Service contract for cross-owner operations (selected for v1).** In the sponsored branch `authorize_use` locks a payer-owned account and offer and a buyer-owned acceptance in one transaction on behalf of a consumer whose user is in neither or only one of those tenants. The transaction runs under existing platform mechanisms, none new:

- *Identity.* Accounts evaluates the authenticated calling service principal (for example Orders or Subscriptions) through its PEP; it does not substitute its own identity for an incoming caller. The caller obtains its security context from the AuthN client-credentials exchange (`authn-resolver-sdk`) through ClientHub, never from a hand-built `SecurityContext`. In-process SDK calls carry that validated context; out-of-process calls authenticate the transport and preserve the verified caller identity. The platform-plane transport token identifies the hop, not business authority. Accounts may use a separate service principal for its own autonomous offer-summary projector.
- *Authority.* Consumer service principals hold narrow roles for account-use authorization, grant outcome recording (including release and end), and status reads on the required Accounts resource types, assigned at the platform root. Projector identities hold only the source-read and projection actions their jobs need. Accounts obtains and validates separate single-target PDP scopes for the caller and named operation before accessing its own store. A root role never becomes a whole-platform repository scope. Roles and identity-provider clients are provisioned by the operator or onboarding, never by a business worker.
- *Per-call scope.* Every tenant-bearing resource/parent access uses a PDP-issued scope for one explicitly named tenant: `context_tenant_id(target)`, `TenantMode::RootOnly`, `require_constraints(true)`. Before use, the service adapter verifies that every OR branch of the returned scope fences `owner_tenant_id` to that target; reject overbroad, subtree, missing-owner or unconstrained responses without rebuilding the scope. Existing scope accessors support this check. Full algorithm and repository evidence are in [commercial DESIGN §3.4](../../../../docs/arch/bss-commercial-model/DESIGN.md#34-internal-dependencies).
- *Transaction.* Obtain separate payer, buyer and consumer-home scopes (per required action/resource) before the transaction. Use payer scope for account/offer/approval/grant, buyer scope for acceptance and home scope for attempt/use-slot rows, with the same `TransactionRunner` and lock order. Do not merge scopes or widen a join. Seller/party evidence reads have their own exact target and happen before the transaction; immutable receipts are verified again against locked local bindings. A failure to authorize any target prevents the use.
- *Target provenance.* Account/grant references carry an owner hint only to route the first scoped lookup; it grants no authority. Read the exact ID in that one tenant, validate actual owner and purchase/offer/acceptance lineage, and use the verified binding for effects. Wrong or missing hints fail closed without searching another tenant. Current-use status batches partition the supplied source references by owner and obtain separate scopes. Replay and mutation still require the authenticated consumer principal and exact use/version; downstream status readers cannot mutate foreign grants.
- *Recipient binding.* An autonomous projector reads back authoritative source state, derives its recipient, obtains that recipient's scope and inserts with the same bound recipient. The event's claimed destination never selects the write scope. The operation context binds source revision, recipient, action/resource and validated scope; the repository cannot take an independent destination. This catches mismatched writes, but a broadly privileged or compromised service can still request another target. Root-assigned roles therefore remain standing privilege, not a cryptographic per-purchase authorization boundary.
- *Barriers, lifecycle and evidence.* `RootOnly` restricts scope; it does not authorize access to a self-managed target or validate tenant existence/status by itself. The actual provider must enforce the named service role/action and deliberate internal-only barrier traversal; users retain barrier-respecting scopes. Validate target lifecycle explicitly through the authoritative tenant contract. The [local SDK](../../../../gears/system/authz-resolver/authz-resolver-sdk/src/pep/enforcer.rs) and [TR plugin](../../../../gears/system/authz-resolver/plugins/tr-authz-plugin/src/domain/service.rs) demonstrate singleton requests and no subtree enumeration in the RootOnly branch, not deployed role enforcement. Authorization-owner confirmation and the §3.4 adversarial/provider tests are prerequisites for ADR acceptance. No private checkout is the authority for this service-scope contract.

**Q7 adoption work.** Ledger's current provisioning derives the legal entity from the tenant (`ledger/src/infra/provisioning/service.rs`, "one legal entity per tenant"); Q7 requires provisioning by `issuer_party_id` and a deterministic, versioned book-resolution contract with explicit context (issuer binding/version, seller tenant, issuer party, billing currency, accounting profile/version and effective time (commercial DESIGN §3.10)). Accounts stores the binding and the Ledger reference; it never resolves books. Ledger's accepted [book-ownership ADR](../../ledger/docs/ADR/0001-cpt-cf-bss-ledger-adr-book-ownership-predicate.md) names AMS/Catalog/billing-setup as owner of the tenant–account–legal-entity mapping; adopting this design requires Ledger to amend that statement (and its related payer-resolution rules) in its own documents — this package does not amend the accepted ADR.

**Q11 evidence.** Ledger configures `seller_tenant_types` with platform/partner identifiers and checks the target's AM-reported type at provisioning; the AM tenant-type trait schema is closed and has no billing trait. The configured guard remains the enforcement; the resale chain uses platform → partner → partner → organization roles without requiring AM parent links.

### 3.5 External Dependencies

No PSP, tax engine or external legal registry integration is selected. Identity evidence is confirmed by setup operators against configured namespaces; AM metadata is not verified legal identity.

### 3.6 Interactions & Sequences

#### Authorize one use

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-seq-authorize-use`

**Use cases**: `cpt-cf-bss-accounts-usecase-sponsored-service`, `cpt-cf-bss-accounts-usecase-grant-recovery`

**Actors**: `cpt-cf-bss-accounts-actor-consumer`, `cpt-cf-bss-accounts-actor-payer`

| Step | Owner | Durable state after the step | External effect |
|---|---|---|---|
| 1 | Consumer (Orders shown) | the consumer's durable attempt, stored before any call: a fresh `attempt_id`, `use_ref`, the exact proposal and `terms_digest`. In Orders this maps onto the commercial attempt of [D-188](../../orders/docs/DESIGN.md#contract-01-commercial-attempt) and its reserved candidate version, not onto a second set of statuses; that mapping is proposed, not built | none: nothing purchased, incurred or activated; a quote or a conditional non-binding preparation obtained under its own authority may already exist |
| 2 | Consumer → Accounts | `authorize_use(attempt_id, use_ref, proposal)`; Accounts computes the digest of the canonical proposal | none until commit |
| 3 | Accounts | one transaction under the service contract of §3.4: authorize the calling service principal and match it to the use's consumer identity; compute the canonical proposal digest; look up `(consumer_principal, attempt_id)` and any grant for `(consumer_principal, use_ref, version)`; reject a digest mismatch before returning any existing outcome; otherwise lock, in the order account → the offer named by `offer_id` → acceptance (→ approval) — self-funded: account (→ approval) — with row locks that conflict with the writers below and are held until commit, and validate exact terms on the values those locking reads return (§3.1 effective arrangement or self-funded checks, buyer = order owner tenant, seller = catalog seller, debtor party = buyer's accepted party = account debtor, scope, resource restriction, currency, tariff identity and version, metering and units, consumption policy, fixed quantity and amount, coverage within policy, approval digest); insert grant `authorized` with pinned terms, or record the refusal under `attempt_id`; commit | **linearization point** |
| 4 | Consumer | store grant evidence on the version and admit it in one local transaction that also checks, under the consumer's own lock and fence, that the attempt was not abandoned (Orders: the final commit of the D-188 attempt) | admission; supplier acceptance request may proceed (Q5.1 conditional) |
| 5 | Consumer → Accounts | `record_use(grant_id, use_ref, admitted)` | grant `authorized → used` |
| 6 | later | `record_use_ended(grant_id, ended_at, reason)` when the admitted use ends under its terms, also when its `record_use` has not arrived yet; or `release(grant_id)` for a use that was never admitted, sent only after the consumer has committed its local unadmitted terminal state | coverage ends; history retained |

**Use uniqueness.** Attempts and grants are transactionally unique independently of the account locks: there is one durable decision per `(consumer_principal, attempt_id)` and at most one grant per immutable use, so concurrent attempts targeting different accounts for the same use cannot both create a grant. Both same-attempt replay and a new attempt for a use that has a grant compare the canonical digest before returning its outcome. Matching requests return the existing outcome; mismatches reject. A refusal is recorded on its attempt only and reserves nothing for the use; the consumer does not ask again for a refused use and uses a new version instead. The use-slot guard and complete lock order are specified in §3.7; implementation must verify them with the grant concurrency tests. A released or ended use is not recreated under another attempt identifier.

Revocation, withdrawal, blocking and approval withdrawal take an exclusive row lock on each row they change (in the same order when they change more than one), take effect when they commit and leave `authorized` and `used` grants untouched; they refuse later attempts. `authorize_use` tests the locked state (revoked, withdrawn, blocked), so the order of the two commits on the shared row decides between "grant before revoke" and "revoke before grant". Recorded wall-clock times are evidence and never decide that race; the database clock is used only for validity windows.

Proposed for the first implementation: share-mode row locks in `authorize_use`, exclusive locks in the writers, READ COMMITTED isolation, and a bounded retry of the whole transaction on a deadlock or a serialization failure, which the attempt key makes safe. This is a design intent, not a proof. The lock modes, the isolation level and the outcomes under concurrency must be validated on PostgreSQL in the grant concurrency spike, and no deadlock-free guarantee is claimed.

| Situation | Accounts | Consumer | Invariant |
|---|---|---|---|
| timeout or unknown outcome at step 2 | grant or refusal may exist | replay `authorize_use(attempt_id, …)`; same outcome returned or step 3 performed now | one attempt, one durable decision |
| crash after step 3, before step 4 | grant `authorized` | a retry of the same stored attempt replays it and completes step 4, or settles the attempt as not admitted and then releases. Completing step 4 needs the caller: in Orders, a client retry with the same idempotency key, which is the only recovery delivered today. Without a retry, automation closes the attempt once it can no longer admit and then releases; it never admits (consumer recovery below) | admission always has grant evidence |
| crash after step 4, before step 5 | `authorized` | the obligation to deliver `record_use` was committed with the admission; the consumer repeats the delivery until Accounts confirms it (at-least-once, idempotent; consumer work, not delivered) | binding closed |
| revoke committed between steps 1 and 3 | refusal recorded under `attempt_id`; no grant | the consumer settles its attempt as refused (Orders: the candidate version is consumed); a new funding choice is a new attempt and a new version | no grant after revoke |
| revoke after step 3 | grant untouched; offer stops new attempts | step 4 proceeds on the grant | authorized use survives prospective withdrawal |
| replay for a `released` or `ended` grant | returns that outcome | new purchase needs a new attempt and a new version | no resurrection |
| same `attempt_id`, different digest | `AlreadyExists(payload_mismatch)` | consumer defect, surfaced | exact terms only |
| buyer withdrew acceptance concurrently | whichever commits first wins under the locks | refusal or authorization as recorded | serialized |
| Accounts unavailable | — | the attempt stays unresolved; retry same attempt; renewal evaluation reports authority unavailable, distinct from payment default | fail closed |
| user abandons after step 3 | `release`, after the consumer's commit | local abandonment and step 4 exclude each other under the consumer's lock, so a delayed worker cannot admit; `release` is sent only after the unadmitted terminal state is committed | no admission of released use; no release of an admitted use |
| consumer deadline passes before admission | grant stays `authorized` until released; Accounts has no grant expiry | the consumer's final check refuses admission, commits the refusal and then releases; with no client retry, the closing action does the same after it reclaims the lease | no hidden grant lifetime and no deadline extension |
| authorized grant never confirmed | reconciliation report after a configured window lists it to the consumer; **no automatic deletion** | consumer records the use or releases from its durable state; cross-process consumer state reconciliation is a contract gate, not an implemented guarantee | consumer deadline and pinned coverage bound delayed admission |
| second attempt id for the same `(consumer, use_ref, version)` after a lost reply | returns the existing grant or its recorded outcome; no second grant | consumer adopts the returned grant | at most one grant per immutable use |
| `record_use` arrives after `record_use_ended` | `ended` was accepted on the `authorized` grant; the late `record_use` is recorded as evidence; state stays `ended` | the consumer ends an admitted use from its own durable state and never releases it | monotonic outcomes; ended or released never authorize admission |
| admitted use cancelled or ended under terms | `record_use_ended` | financial consequences per the leg's terms through Ledger | history and attribution evidence preserved |

**Consumer recovery (selected for v1; consumer work, none delivered).** Automation never admits a purchase. In Orders, admission stays the caller-authorized flow: the initial request or the client's retry with the same idempotency key; a maintenance authority cannot commit an order. This is a rule for Orders and does not limit scheduled Subscriptions renewals, which run under their own authority. Automation does two things only:

- *Outcome delivery.* The local outcome (admitted, ended or not admitted) and the obligation to report it to Accounts (`record_use`, `record_use_ended` or `release`) commit in one local transaction. The consumer repeats the delivery until Accounts confirms it: at-least-once and idempotent, not exactly-once, and with no completion bound while Accounts is unavailable. Until delivery a grant may remain `authorized`. Committed and refused attempts are terminal, so their owed deliveries need their own discovery, separate from the scan of unresolved attempts. The selected carrier is a transactional consumer outbox (§3.9); delivery code is not delivered by this proposal.
- *Closing an attempt that cannot admit.* The consumer first reclaims the lapsed lease under a new fence and rechecks the state under its own lock. It then settles an unresolved attempt as not admitted only when the consumer deadline has passed or the source definitively no longer admits it (for example a cancelled or changed draft). No age timeout, no grant expiry and no deadline extension is introduced. The local terminal state is persisted first; `release` follows through outcome delivery. If the grant response is unknown, the consumer replays the same stored attempt under the same consumer identity only to learn the outcome; that replay cannot reopen or commit the order. A late original response is reconciled under the fence and cannot admit.

The claim is limited to this: without admission there is no committed version, no billing assignment and no charge. No cleanup time is promised. Required owner amendment, recorded here and not made in Orders' documents: [Orders D-236](../../orders/docs/DECISIONS.md) deferred the recovery body; its closing portion and outcome delivery become required v1 work, while recovery of the original order commit stays client-driven. The closing worker, refusal evidence and outbox protocol are specified in §3.9. This is the proposed D-236 amendment, pending owner review, not an implemented recovery body.

#### Consent and invitation

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-seq-consented-use`

**Use cases**: `cpt-cf-bss-accounts-usecase-sponsored-service`, `cpt-cf-bss-accounts-usecase-dual-confirmation`

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

A payer user creates an offer naming a buyer tenant (own-tenant write). The buyer must see the offer before any grant exists, so Accounts produces a schema-fixed **offer summary** for the buyer tenant through its own narrowly authorized offer route: the recipient is the offer's named buyer tenant, the content is limited to offer id, payer tenant display name resolved by Accounts, scope summary, limits, coverage policy and validity, with no free text; the recipient must be a resolvable tenant; visibility is limited to buyer users holding the acceptance action; invitation volume per payer is limited and a buyer tenant may block a payer. The summary carries no authority and is never the authoritative acceptance state. The buyer user accepts (own-tenant write bound to the terms version); the acceptance row lives in Accounts' store and is locked by `authorize_use`, so withdrawal serializes with authorization.

### 3.7 Database schemas & tables

The Accounts gear owns a PostgreSQL schema. Every authority table has an opaque UUID primary key, `owner_tenant_id`, monotonic `revision` (bigint), `type_id` (registry UUID), creation/update evidence and an immutable history record for each externally referenced version. Timestamps are UTC; intervals are half-open. Base fields below are columns, not an unvalidated JSON bag. GTS extension payloads, where permitted by §4, use `metadata JSONB`; frozen structured DTOs use validated JSONB with their type/version retained. No cross-gear foreign key or transaction is introduced.

| Table | Additional columns and constraints | Owner |
|---|---|---|
| `legal_parties`, `party_profiles` | Party representation; profile unique `(party_id, profile_version)`, immutable confirmed profile. Identifier evidence includes namespace-policy version, normalized value, verification actor/time/reference; alias/distinctness assertions are append-only | Representing tenant |
| `issuer_bindings` | Seller, issuer party, revision, provisioning receipt; unique active `(seller_tenant_id, issuer_party_id)` | Seller |
| `billing_accounts` | Fixed seller/issuer/debtor/currency/binding, state, defaults and invoice-cycle preference revision; fixed tuple cannot be updated | Payer |
| `account_use_offers`, `offer_versions` | Account, named buyer, terms version, validity, revocation; unique `(offer_id, terms_version)` history | Payer |
| `account_use_acceptances` | Offer, accepted version, validity, withdrawal; one active acceptance per `(offer_id, buyer_tenant_id, terms_version)`; buyer must match offer | Buyer |
| `account_use_approvals` | Account, optional offer, consumer principal and immutable use tuple, proposal digest, approval generation, validity, decision and actor evidence; approved decision immutable | Payer |
| `use_slots` | Unique `(consumer_principal, use_kind, object_id, use_version)`; nullable immutable `grant_id`. Durable synchronization/tombstone only, never spending authority | Consumer service's provisioned home tenant |
| `authorization_attempts` | Unique `(consumer_principal, attempt_id)`; complete canonical proposal bytes/digest, use tuple, terminal decision, grant or refusal reason | Consumer service's home tenant |
| `account_use_grants` | Unique use tuple above and grant UUID; pins, canonical proposal, state, actual admitted interval/outcome evidence; index `(account_id, state)` | Payer |
| `grant_outcomes` | Unique `(grant_id, outcome_id)`; use tuple, source revision, outcome kind/time, body digest; immutable evidence | Payer |
| `offer_summaries`, `buyer_payer_blocks` | Summary unique `(offer_id, buyer_tenant_id)` with basis revision vector; block unique `(buyer_tenant_id, payer_tenant_id)` | Buyer |
| `outbox` | Event/effect UUID, subject, subject revision, validated payload, delivery state and retry evidence; immutable effect identity | Source owner |

Local foreign keys link Accounts-owned rows. Foreign-key existence is never authorization. Index account state, offer account/buyer/validity, acceptance offer/version, grant use tuple and source revision; lists additionally index `(owner_tenant_id, id)`. Financial and consent history is not cascaded away on tenant/account closure.

**Serialization.** Start READ COMMITTED; scoped insert-or-conflict creates the attempt and use-slot guards, then re-read them under exclusive locks. Use the order **attempt → use slot → account → offer → acceptance → approval**; validate caller and digest before returning a stored outcome. Account/consent/approval locks are shared for authorization and exclusive for writers, held through commit. Guard rows serialize same-use requests even when they name different accounts. The first slot writer creates the only grant; the unique grant constraint is a second defense. A recorded refusal completes its attempt without filling the slot. Revoke/edit writers never acquire attempt/slot locks; multi-row management writers use the same authority-row order. Account closure takes an exclusive account lock, rejects any `authorized` or `used` grant, and therefore cannot race a new grant into a closed account. Delayed outcome delivery can postpone closure; it cannot delete a grant. `blocked → active` is an audited action; `closed` is terminal.

Use SecureORM `lock_shared()` / `lock_exclusive()` and repository methods accepting a `TransactionRunner`; acquiring a lock through a connection runner would release it too early. These methods exist in the current [Toolkit](../../../../libs/toolkit-db/src/secure/select.rs); [Orders](../../orders/orders/src/infra/storage/repo/mod.rs) already uses scoped exclusive locking. Retry the entire transaction at most three times on a deadlock/serialization failure, preserving attempt identity; exhaustion returns `Unavailable`. No SDK/network call occurs under these locks: authority inputs from other gears are immutable versioned receipts checked before the transaction. This specifies the mechanism, not a claim of a completed concurrency test.

### 3.8 SDK and management contracts

`AccountsClientV1` is exposed through ClientHub. Domain DTOs live in `accounts-sdk`; REST adapters preserve the SDK field semantics through validated transport DTOs, UUID resource references, RFC 3339 UTC instants and decimal strings (never floating-point money). Public routes are under `/bss-accounts/v1`; platform-plane methods are SDK-only in v1 and are not mounted in the public router. Commands require an idempotency key; edits require expected revision. Reusing a key with another canonical body is a conflict; replay checks caller authorization again. Lists use opaque keyset cursors bound to tenant, filter and sort, default 50 / maximum 200. Authorization precedes existence disclosure.

| Contract | Required request / response |
|---|---|
| `UseRef` | `consumer_gear` (GTS instance), `use_kind` (GTS instance), `object_id`, `version` positive integer. Authenticated principal is obtained from context, never body |
| `UseProposal` | `use_ref`, buyer/resource/seller tenant IDs, account ID/version and routing-only owner hint, issuer/debtor party IDs, issuer binding/version, optional offer/terms version and acceptance ID/version, optional approval ID, book resolution receipt, accepted terms, purchase-authority receipt from the owning consumer |
| `AcceptedTerms` | `terms_type_id`, frozen product/revision and catalog tenant, line/component IDs, quantities, fixed money or pinned usage tariff/revision and meter/unit/grain, consumption policy, coverage term and frozen anchors, chosen activation instant/compatibility profile, supplier conditional-commitment receipt, resolved payment/tax treatment references, invoice preference, and `sources[] {field_path, owner, resource_id, revision, receipt_digest}` |
| `authorize_use` | `{attempt_id, proposal}` → `Authorized {grant_id, grant_revision, use_ref, terms_digest, pins, state}` or durable `Refused {attempt_id, reason_id}`. Same attempt replay returns its original decision plus current grant outcome; no inferred new authorization |
| `evaluate_account_use` | Same proposal, read-only `eligible`, `ineligible` or unavailable; evaluated versions/reasons included, no grant or promise |
| `record_use`, `record_use_ended`, `release` | `{grant_id, use_ref, outcome_id, source_revision, at, reason_id, admitted_interval?}` → recorded state/revision. Exact outcome replay returns same receipt; conflicting body is rejected. Admission carries the consumer's committed admission identity; release carries its committed unadmitted terminal identity |
| `grant_status`, `offer_status` | At most 200 exact IDs with owner locators plus source/use references → status, source revision, checked-at, minimal binding/coverage/recipient evidence. Unknown/unauthorized IDs are indistinguishable to the caller. No instruments, profiles, arbitrary account searches or full private terms |

**Use versions.** Initial/amended Orders use is `(orders, purchase, order_id, reserved_candidate_version)` with leg/line identity in the proposal. A multi-leg order uses a stable per-leg purchase-use UUID recorded with that candidate, rather than attempting several grants under one use tuple. Subscriptions allocates a durable monotonically increasing `use_version` under the subscription lock per `(subscription_id, leg_id, use_kind)`; `object_id` is the stable UUID of that tuple. Renewal includes fixed next-period bounds; transfer includes target account and shared boundary; increase includes change-request ID and proposed quantity. Retry never allocates a version; a changed proposal or refused attempt consumes the old version and creates a new one. Chain publication versions are separate and do not re-key uses or obligations. Handoff retains the Orders grant as historical authority; Subscriptions cannot mutate it and requests its own grant for new authority.

**Canonical digest.** Digest algorithm is SHA-256 over a versioned, deterministic encoding of `{consumer_principal, proposal: UseProposal}` excluding attempt ID, transport credentials and the approval ID (approval binds this digest, so it cannot include itself). Resolve `consumer_principal` from the authenticated context, never trust a body override; the approval request pins that intended principal and authorization must match it. Include the immutable use reference and version, exact GTS versions, optional fields as explicit null, sorted object keys, arrays in declared order (set-valued arrays sorted by canonical member bytes), normalized UUIDs, UTC instants and canonical decimal strings; reject duplicate keys and non-finite numbers. Persist canonical bytes as well as digest and compare bytes on replay, so a hash match alone cannot change authority. SDK serialization fixtures define byte equality across consumers; changes to canonicalization require a new digest version. Do not use the non-cryptographic `toolkit-stable-hash` for consent evidence. Accounts recomputes rather than trusting a supplied digest.

| Public operation | Route and guard |
|---|---|
| Party and confirmed profile | `POST /parties`; `GET /parties/{id}`; `POST /parties/{id}/profiles` and `/confirmations`; representing-tenant action and expected revision, immutable confirmed profiles |
| Identity comparison evidence | `POST /party-comparisons`; setup permission, both party references, namespace-policy version, evidence, outcome `same/distinct/undetermined`; no merge or automatic representation change |
| Issuer setup | `POST /issuer-bindings`; `POST /issuer-bindings/{id}/confirmations`; seller setup permission, confirmed issuer representation and Ledger provisioning receipt |
| Account | `POST /billing-accounts`; `GET /billing-accounts/{id}`; `PATCH /billing-accounts/{id}` only defaults/preferences; `POST /billing-accounts/{id}/block`, `/unblock`, `/close`; fixed tuple immutable |
| Offer | `POST /offers`; `GET /offers/{id}`; `PATCH /offers/{id}` creates a new terms version; `POST /offers/{id}/revoke`; payer action |
| Acceptance | `POST /acceptances` with offer/terms version; `POST /acceptances/{id}/withdraw`; buyer action; no caller-selected owner |
| Approval | `POST /use-approvals` with use/proposal; `POST /use-approvals/{id}/approve`, `/reject`, `/withdraw`, `/revoke`; payer permission, generation and optional required note |
| Invitation block | `PUT /blocked-payers/{payer_id}`; `DELETE /blocked-payers/{payer_id}`; buyer acceptance-management permission; immediately withholds invitations and stops new acceptances, does not revoke already accepted use authority |

Collection GETs use the same resource permissions and restricted schemas. Party lookup across tenants is an internal purpose-bound comparison/profile-read SDK method, not a public directory. Identity normalization is a versioned deployment policy registered before namespace use; setup operators confirm evidence, which includes source/reference, normalized identifier and verification time. No supported canonical namespace means `undetermined`, never equality by display name. Offer creation charges a transactional payer quota; configured positive invitation limits and buyer block are checked before insertion/projection. Overflow is `ResourceExhausted`; quota values are deployment inputs, not unlimited defaults.

**Approval protocol.** Approval is `pending → approved | rejected | withdrawn`; only pending may be voted or withdrawn. An approved unit is immutable; a new proposal/terms version requires a new unit. `approval_mode` may require separation of requester and approver; both actions still require their own permissions. Validity expiry or an explicit audited authority revocation prevents new grants, without changing issued grants or rewriting the decision. Authorization locks the approval and validates account/offer, authenticated consumer principal, immutable use tuple and version, digest, approved generation and validity. Accounts implements the existing `ApprovalSourceV1` page/count/get/vote contract; Approvals adds the `account_use` kind to its supported source inventory and forwards context, generation and idempotency key unchanged. Accounts owns votes and deduplication; Approvals stores no decision. Duplicate/conflicting decisions are 409; stale generation is 400 under the existing inbox protocol. No cross-gear approval transaction is needed.

### 3.9 Consumer outcome and recovery protocol

Each consumer writes its local outcome and an Accounts delivery record in the **same local transaction**. The delivery record contains effect UUID, grant/use, source revision, body digest, the immutable owner locator from the grant receipt and caller principal alias; credentials are acquired at delivery time. A Toolkit transactional outbox is the selected durable carrier; an in-gear worker delivers the Accounts SDK command, so no public mutation event or broker consumer becomes grant authority. Commit notification wakes delivery only after commit. Retry preserves the effect ID; failures remain pending with backoff and an operator-visible oldest age. Discovery respects the actual owner model: Accounts may scan its home-owned attempt/slot records under that tenant scope, while Orders recovery uses bounded order/owner locators, authorizes and locks each buyer-owned parent, then uses its existing exact-parent-ID child scope for commercial attempts. Orders attempts are not home-tenant-owned. Payer-owned delivery records and source outboxes use the named paginated reconciliation/projector discovery contract in commercial DESIGN §3.4; no global unscoped pending-work scan is permitted. Outbox retention cannot delete an unconfirmed effect.

The Accounts outcome transaction locks the grant, inserts the unique outcome, checks consumer ownership and validates the state transition. `ended` may overtake `used`; a later admission receipt is evidence only. `released` plus any admitted receipt is an invariant violation requiring reconciliation, never a transition. End carries actual covered interval and consumer lifecycle reason; it cannot erase incurred attribution. Sequence gaps trigger reconciliation with the source; delivery order is not required for correctness.

Orders' existing lease/fence protects its commercial attempt. The maintenance worker claims an expired lease, rechecks under lock and writes an unadmitted terminal result only for an expired admission deadline or a definitively inadmissible source. It uses the existing deadline/source-change refusal classification and stores structured closing evidence, not a new purchase status. Unknown Accounts outcome is reconciled using the original stored attempt; a grant discovered after terminal refusal is released through the outbox. Client retry and closer serialize on the same lock/fence. Automation cannot execute the admission commit. Subscriptions' scheduler may admit an already authorized routine renewal under its own service contract; changed terms require the purchase-confirmation flow in commercial DESIGN §3.8.


## 4. Additional context

### 4.1 GTS schemas, extension and registration

This is a schema design, not only a list of names. Follow [GTS guidelines](../../../../guidelines/GTS.md), including DESIGN review §14, and the [permission contract](../../../../docs/arch/authorization/PERMISSION_GTS_TYPE.md). The owning SDK defines schemas with `#[gts_type_schema]`, `type_id`/`GtsTypeId`, `gts_id!` and `gts_uri!`; generates Draft-07 using `gts_schema_with_refs_as_string()`; and registers types and well-known instances during initialization. Hand-maintained parallel JSON schemas are not a second source of truth. Types-registry is a runtime dependency; unresolved required types make the capability unready. Schema registration, SDK generation and tests are implementation work; the identifiers and shapes below are the proposed contract.

| Full type identifier | Shape / extension contract |
|---|---|
| `gts.cf.bss.accounts.legal_party.v1~` | Concrete extensible resource: id, owner/representing tenant, revision, identifier/profile references; `metadata` is the sole vendor extension field |
| `gts.cf.bss.accounts.party_profile.v1~` | Concrete extensible versioned resource: party, profile version, legal name, structured addresses and tax identifiers, confirmation evidence; vendor additions only in `metadata` |
| `gts.cf.bss.accounts.billing_account.v1~` | Concrete extensible resource: §3.1 fixed tuple, version, state, defaults and cycle preference; `metadata` cannot change binding or authority |
| `gts.cf.bss.accounts.account_use_offer.v1~` | Concrete extensible: account, buyer, terms version, product/resource scope, limits, approval/coverage policies, validity and revocation; `metadata` only |
| `gts.cf.bss.accounts.account_use_acceptance.v1~` | Final evidence: offer/version, owner buyer, validity, consent evidence and withdrawal |
| `gts.cf.bss.accounts.account_use_approval.v1~` | Final evidence: account, optional offer, consumer principal, use tuple/version, digest, generation, validity, decision and revocation evidence |
| `gts.cf.bss.accounts.account_use_grant.v1~` | Final evidence: §3.1 pins, canonical proposal/digest version, use tuple, attempt, principal, coverage and outcome state |
| `gts.cf.bss.accounts.issuer_binding.v1~` | Final evidence: seller, issuer party, revision and Ledger provisioning receipt |
| `gts.cf.bss.accounts.offer_summary.v1~` | Final recipient-safe view: named buyer, offer/version, payer display name, scope/limits/coverage/validity and basis vector; no generic metadata |
| `gts.cf.bss.accounts.use_proposal.v1~` | Final cross-gear message: complete §3.8 UseProposal; nested GTS references are exact typed references |
| `gts.cf.bss.accounts.use_outcome.v1~` | Final message: grant/use, outcome ID, source revision, kind/time/reason and admission/terminal evidence |
| `gts.cf.bss.accounts.authority_status.v1~` | Final read response: source ID/type/revision, checked-at, status, minimal binding/coverage and recipient; absent/unauthorized represented identically |
| `gts.cf.bss.accounts.use_slot.v1~` | Final internal authorization resource for scoped synchronization rows, never a public object or spending scope |
| `gts.cf.bss.accounts.authorization_attempt.v1~` | Final internal authorization resource: consumer principal, attempt/use, canonical proposal and recorded decision |
| `gts.cf.bss.accounts.consumer.v1~` | Concrete discriminator schema: instance ID and owning gear; supported instances `gts.cf.bss.accounts.consumer.v1~cf.bss.accounts.orders.v1` and `gts.cf.bss.accounts.consumer.v1~cf.bss.accounts.subscriptions.v1` |
| `gts.cf.bss.accounts.use_kind.v1~` | Concrete discriminator schema: instance ID, owning consumer, required authority profile; registered instances below |
| `gts.cf.bss.accounts.approval_policy.v1~` | Concrete policy discriminator: instance ID, `requires_per_use_approval`, `requires_distinct_approver`; values resolved from the registered instance, not caller-supplied flags |
| `gts.cf.bss.accounts.coverage_policy.v1~` | Concrete policy discriminator: instance ID, `covers_single_period`, `covers_full_accepted_term`; limits remain offer fields |
| `gts.cf.bss.accounts.identity_namespace.v1~` | Concrete namespace-policy descriptor: canonical-identity semantics, normalization-profile reference/version, required verification evidence and effective policy version; no trust from a raw namespace string |
| `gts.cf.bss.accounts.failure_reason.v1~` | Concrete discriminator schema: instance ID, canonical error category, retryable flag, safe description; no arbitrary user-supplied text |

Resource DTOs share `id`, `type_id`, `owner_tenant_id`, `revision`; ownership is server-derived, immutable base fields are not patchable. Extensible resources are **neither abstract nor final**: the base is usable, and a vendor may add descriptive metadata without a table/API change. For example `gts.cf.bss.accounts.billing_account.v1~acme.billing.accounts.cost_center.v1~` adds `metadata.cost_center_code`, while all base fields retain their meaning. Only the extension object is open at the base; the derived schema validates its fields. Vendor metadata is never copied into payer/buyer projections, used for permission decisions or included in funding terms without a separately supported, versioned contract. Final evidence and recipient views deliberately forbid derivation and unknown fields. Internal lifecycle enums (`active/blocked/closed`, `authorized/used/ended/released`) remain closed state machines, not pluggable policies.

**Boundary pipeline.** Parse bounded input → resolve full type and allowed base ancestry in types-registry → validate generated schema and nested type references → evaluate caller action and owner scope → run semantic/evidence checks → persist/process. Authenticate before exposing registry or object details; registry failure returns unavailable. An unknown derived type, unsupported semantic profile or unregistered discriminator cannot be accepted as its base by dropping fields. Heterogeneous carriers use `Base<Value>` with `try_narrow`, not unchecked deserialization. Registry ancestry is resolved to UUIDs; no SQL `LIKE` over type strings. Persist invariant fields in columns and metadata in JSONB (§3.7); preserve exact type/version on historical evidence. Schema registration is not permission to invoke a handler.

**Versioning and traits.** Compatible optional descriptive fields use compatible minor evolution after registry checks; removing/renaming required fields or changing authority/digest semantics needs a new major version and explicit reader support. Never coerce old grants to a current schema for billing. Any schema-level `x-gts-traits` must have a typed `x-gts-traits-schema`; instances cannot supply schema keywords. This proposal introduces no new authorization trait or a billing tenant trait. Event routing uses the broker's existing metadata and subject contract, not instance-selected tenants.

**Well-known use instances.** Under `gts.cf.bss.accounts.use_kind.v1~`, register the exact instance segments `cf.bss.accounts.purchase.v1`, `cf.bss.accounts.renewal.v1`, `cf.bss.accounts.transfer.v1`, `cf.bss.accounts.increase.v1`. The first permits Orders ownership; the others permit Subscriptions ownership. The full purchase ID is `gts.cf.bss.accounts.use_kind.v1~cf.bss.accounts.purchase.v1`. These are registry instances, not bare string values in DTOs; caller/use semantics still follow §3.8. Additional kinds require an explicit supported consumer contract, not simply a new registry record.

**Policy values.** Prose shorthands `per_use`, `single_period` and `full_term` denote registered instances on the wire, not extensible Rust enums. Register `gts.cf.bss.accounts.approval_policy.v1~cf.bss.accounts.arrangement_authority.v1` (no per-use vote) and `gts.cf.bss.accounts.approval_policy.v1~cf.bss.accounts.per_use.v1` (requires a vote), with a separately configured policy instance when distinct approvers are required. Coverage instances are `gts.cf.bss.accounts.coverage_policy.v1~cf.bss.accounts.single_period.v1` and `gts.cf.bss.accounts.coverage_policy.v1~cf.bss.accounts.full_term.v1`; the engine evaluates their validated properties plus explicit offer limits. Namespace policies are versioned deployment instances of `identity_namespace`, pinned on evidence. An unknown normalization implementation is unsupported and cannot produce verified evidence. Consumers pin these exact instances/versions in the proposal digest; a new registered value cannot relax the fixed grant or authorization invariants. Consumption policies and activation profiles belong to Products; Accounts accepts only explicitly supported registered references, never an arbitrary string or a profile supplied inline by a caller.

### 4.2 Permissions, events and errors

Each row below defines permission instances by **exact construction**: base `gts.cf.toolkit.authz.permission.v1~` plus instance segment `cf.bss.accounts.<name>.v1`, for each enumerated name. The permission's `action` is that exact name and its `resource_type` is the full listed GTS type (no broad `accounts.*` grant). For example `gts.cf.toolkit.authz.permission.v1~cf.bss.accounts.offer_revoke.v1`. User roles stay tenant-scoped; internal service roles are provisioned separately. A service must obtain the additional source-read scopes below before locking related rows; authorization of the grant resource alone is not a scope for the offer or acceptance table.

| Resource type | Enumerated permission names | Audience |
|---|---|---|
| `gts.cf.bss.accounts.legal_party.v1~` | `party_read`, `party_confirm`, `party_compare` | User/setup; bounded internal read separately assigned |
| `gts.cf.bss.accounts.party_profile.v1~` | `profile_read`, `profile_confirm` | Representing tenant; internal issuance read |
| `gts.cf.bss.accounts.billing_account.v1~` | `account_read`, `account_manage`, `account_binding_read` | User; last action service only |
| `gts.cf.bss.accounts.issuer_binding.v1~` | `issuer_binding_read`, `issuer_binding_manage` | Seller setup; service read |
| `gts.cf.bss.accounts.account_use_offer.v1~` | `offer_read`, `offer_manage`, `offer_revoke`, `offer_basis_read` | Payer; last action service only |
| `gts.cf.bss.accounts.account_use_acceptance.v1~` | `acceptance_read`, `acceptance_accept`, `acceptance_withdraw`, `acceptance_basis_read` | Buyer; last action service only |
| `gts.cf.bss.accounts.account_use_approval.v1~` | `use_approval_read`, `use_approval_request`, `use_approve`, `use_approval_revoke`, `approval_basis_read` | Payer; last action service only |
| `gts.cf.bss.accounts.account_use_grant.v1~` | `use_authorize`, `use_record`, `grant_status_read` | Services only; ownership-of-consumer checks still mandatory |
| `gts.cf.bss.accounts.authorization_attempt.v1~` | `attempt_record` | Consumer service, own principal only |
| `gts.cf.bss.accounts.use_slot.v1~` | `use_slot_record` | Consumer service, own principal only |
| `gts.cf.bss.accounts.offer_summary.v1~` | `offer_summary_read`, `offer_summary_project`, `invitation_manage` | Buyer; project service only |

Accounts authorization consumers receive binding/offer/acceptance/approval/issuer read scopes plus grant and own attempt/slot actions; status-only readers receive none of the mutation actions. Autonomous Accounts offer projector receives offer/account basis reads and summary projection. Each management command scopes every mutated resource; account creation also checks referenced issuer binding. Local outbox delivery stores transport state through the source's scoped repository, not a new user-facing authority object.

**Events.** Use the existing broker base **`gts.cf.core.events.event.v1~`** (as implemented in event-broker-sdk), with abstract family `gts.cf.core.events.event.v1~cf.bss.accounts.event.v1~`. Every concrete event appends one exact segment from the following inventory and a trailing `~`:

| Concrete segment | Authoritative subject | Required payload |
|---|---|---|
| `cf.bss.accounts.offer_changed.v1` | Offer ID | Offer ID, terms version, source revision |
| `cf.bss.accounts.offer_revoked.v1` | Offer ID | Offer ID, source revision, revocation evidence reference |
| `cf.bss.accounts.acceptance_changed.v1` | Acceptance ID | Acceptance ID, offer/version, source revision |
| `cf.bss.accounts.acceptance_withdrawn.v1` | Acceptance ID | Acceptance ID, source revision |
| `cf.bss.accounts.account_changed.v1` | Account ID | Account ID, state and source revision |
| `cf.bss.accounts.account_blocked.v1` | Account ID | Account ID, source revision |
| `cf.bss.accounts.account_closed.v1` | Account ID | Account ID, source revision |
| `cf.bss.accounts.grant_authorized.v1` | Grant ID | Grant/use IDs, source revision, digest |
| `cf.bss.accounts.grant_used.v1` | Grant ID | Grant/use IDs, source revision, outcome ID |
| `cf.bss.accounts.grant_ended.v1` | Grant ID | Grant/use IDs, source revision, outcome ID |
| `cf.bss.accounts.grant_released.v1` | Grant ID | Grant/use IDs, source revision, outcome ID |
| `cf.bss.accounts.party_profile_confirmed.v1` | Party ID | Party/profile version and source revision |

For example the complete grant type is `gts.cf.core.events.event.v1~cf.bss.accounts.event.v1~cf.bss.accounts.grant_authorized.v1~`. Concrete payloads are final and carry the shared envelope's event ID, source, subject and occurrence time. Persist event and source transition atomically; the same event ID survives retries. Ordering is per named subject, **not account-global** for offer/grant events. Cross-source ordering is undefined; current views use authoritative version-vector checks. Events contain references and minimal facts, not entire private terms, arbitrary metadata or credentials. Broker audience permissions remain mandatory.

**Errors.** SDK errors map through `toolkit-canonical-errors` and `#[resource_error]` to RFC 9457 `application/problem+json`; use canonical GTS error categories, not an ad-hoc Accounts error base. The safe typed context is `{reason_id: GtsInstanceId, correlation_id, source_revision?}`. Register reasons under `gts.cf.bss.accounts.failure_reason.v1~` with exact segments `cf.bss.accounts.acceptance_stale.v1`, `cf.bss.accounts.account_unusable.v1`, `cf.bss.accounts.scope_mismatch.v1`, `cf.bss.accounts.approval_required.v1`, `cf.bss.accounts.approval_stale.v1`, `cf.bss.accounts.payload_mismatch.v1`, `cf.bss.accounts.outcome_conflict.v1`, `cf.bss.accounts.authority_unavailable.v1`, `cf.bss.accounts.unsupported_type.v1`. Semantic refusals use `FailedPrecondition`; conflicting replay uses `AlreadyExists`; unavailable authority uses `Unavailable`; malformed/unsupported schema uses `InvalidArgument`; quota/read budget uses `ResourceExhausted`. Authentication/authorization use platform categories and masked not-found behavior; they do not reveal foreign account state. A durable refusal is a successful authorization-command outcome, whereas transport/authority failure is not a recorded business refusal. Exact replay preserves the business reason; no stack traces, raw SQL or private proposal payload appears in a Problem.

### 4.3 Review and delivery verification

The schema, SDK, storage, concurrency, approval and recovery choices are specified above. Remaining work is owner acceptance of the proposed amendments, generation/registration of these contracts, implementation, deployment configuration and verification: authenticated service paths, per-call singleton scope validation and the 100,000-tenant verification target, PostgreSQL grant/revoke races, replay/outcome permutations, approval forwarding, source outage and fresh view/count/export behavior. The document does not claim those tests passed or principals provisioned. Ledger amendments and the component design are specified in the [commercial DESIGN](../../../../docs/arch/bss-commercial-model/DESIGN.md#38-orders-terms-and-subscriptions-contracts); no hidden owner choice of use version, outbox carrier or activation semantics remains.

Operational telemetry must distinguish eligibility denial, stale terms, unavailable authority and refusal history, with correlation and version evidence and recipient-safe logging.

## 5. Traceability

- [PRD](PRD.md)
- [Ownership ADR](../../docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md)
- [Resale ADR](../../docs/ADR/0003-cpt-cf-bss-adr-multi-tier-resale-model.md)
- [Commercial DESIGN](../../../../docs/arch/bss-commercial-model/DESIGN.md) — operation classes, derived views, scenario checks
