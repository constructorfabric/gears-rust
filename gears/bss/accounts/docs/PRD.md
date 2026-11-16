---
description: "Proposed requirements for BSS Accounts: billing-party identity, billing accounts, billing arrangements (consent), account-use grants and seller/issuer eligibility."
status: proposed
---

# PRD — BSS Accounts

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [Billing-party identity](#billing-party-identity)
  - [Account binding and debtor validation](#account-binding-and-debtor-validation)
  - [Buyer, debtor, payer and recipient separation — Q8](#buyer-debtor-payer-and-recipient-separation--q8)
  - [Consented account use — Q2](#consented-account-use--q2)
  - [Account-use grant](#account-use-grant)
  - [Spending admission and optional purchase approval](#spending-admission-and-optional-purchase-approval)
  - [Confirmed billing profiles](#confirmed-billing-profiles)
  - [Seller and issuer eligibility — Q7 and Q11](#seller-and-issuer-eligibility--q7-and-q11)
  - [Verified bilateral confirmation — Q10](#verified-bilateral-confirmation--q10)
  - [Prospective eligibility and retained history](#prospective-eligibility-and-retained-history)
  - [Currency and financial-account boundaries](#currency-and-financial-account-boundaries)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Sponsored Compute and self-funded Backup](#sponsored-compute-and-self-funded-backup)
  - [Separate accounts for different debtors](#separate-accounts-for-different-debtors)
  - [Holding pays without assuming the buyer debt](#holding-pays-without-assuming-the-buyer-debt)
  - [Two confirmed issuers for one seller](#two-confirmed-issuers-for-one-seller)
  - [One authorized actor records two confirmations](#one-authorized-actor-records-two-confirmations)
  - [Grant issued, consumer crashes, offer revoked](#grant-issued-consumer-crashes-offer-revoked)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Propose a BSS Accounts gear (`bss-accounts`, working name)** that owns five closely related responsibilities: the billing identity of legal parties (`LegalParty`), billing accounts (`BillingAccount`), consent to use an account for a commercial buyer (`BillingArrangement`, recorded as an offer and an acceptance), the authorization of one exact use (`AccountUseGrant`), and seller-to-issuer eligibility (`IssuerBinding`). Only these proposal documents exist; no implementation or SDK is provided.

**Accounts versus Billing.** Billing is the broader capability. The gear catalogue already separates Invoicing, Billing Ledger, Payments and Tax Engine; this proposal adds billing-account administration and billing eligibility and does not collect those responsibilities into Accounts. The gear may be presented as **Billing Accounts** to distinguish it from platform Account Management and from ledger accounts.

### 1.2 Background / Problem Statement

Resource consumption, commercial purchase and payment can belong to different parties. Accounts provides the operational account-use authority and the billing identity of the parties; it does not maintain service lifecycle or money. The wider resale product is defined in the [commercial-model PRD](../../../../docs/arch/bss-commercial-model/PRD.md). The platform decision is the [ownership ADR](../../docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md).

### 1.3 Goals (Business Outcomes)

- Admit a purchase against an account only after a durable authorization of that exact use, issued at one serialization point against revocation.
- Keep funding authority separate from resource access, debt attribution, document delivery and payment instruments.
- Give every legal party used as debtor or issuer one stable identity and one authoritative, versioned billing profile.
- Preserve historical evidence when consent, profiles or payer assignments change.

### 1.4 Glossary

- **LegalParty** — opaque canonical identity of a legal party (company) used as debtor or issuer; carries verified identity evidence and versioned billing profiles.
- **BillingAccount** — payer-owned funding context fixed to one payer tenant, one commercial seller tenant, one issuer party, one debtor party and one billing currency.
- **BillingArrangement** — the business concept of consent that in-scope purchases of one commercial buyer tenant may use one account. Recorded as a payer-owned **AccountUseOffer** and a buyer-owned **AccountUseAcceptance**; the arrangement is **effective** while the acceptance matches the offer's current immutable terms version and neither side has stopped it.
- **AccountUseGrant** — Accounts' durable record that one exact use (one purchase or renewal version, with its parties, scope, amounts and term) is authorized against one account. It authorizes **account attribution** only: it is not a money reservation, debt assumption, supplier acceptance, customer acceptance, payment-instrument mandate or document-recipient selection.
- **Billing assignment** — the account actually attributed to a purchase or subscription leg for an interval, recorded by Orders and Subscriptions with the grant as evidence.
- **IssuerBinding** — the permitted relationship between a seller tenant and an issuer party, coordinated with Ledger's book setup.

## 2. Actors

### 2.1 Human Actors

#### Payer representative

**ID**: `cpt-cf-bss-accounts-actor-payer`

Administers accounts and offers, approves uses where required and stops future use of an account or offer, within explicit tenant permissions.

#### Commercial buyer representative

**ID**: `cpt-cf-bss-accounts-actor-buyer`

Accepts or withdraws its own acceptance of an offer; does not gain account administration.

#### Billing setup operator

**ID**: `cpt-cf-bss-accounts-actor-setup`

Confirms party identity evidence, debtor and issuer profiles, representation and issuer bindings under verified authority.

### 2.2 System Actors

#### Purchase and financial consumers

**ID**: `cpt-cf-bss-accounts-actor-consumer`

Orders and Subscriptions request account-use authorization and record the outcome of each use; Rating, Invoicing and Ledger consume party and grant evidence under their own permissions.

## 3. Operational Concept & Environment

Use the [architecture manifest](../../../../docs/ARCHITECTURE_MANIFEST.md) and the [ToolKit baseline](../../../../docs/toolkit_unified_system/README.md). No gear-specific deployment exception is selected. These documents introduce no implementation crate or runtime registration.

## 4. Scope

### 4.1 In Scope

**Billing-party identity.** Accounts owns a narrow billing-party capability: an opaque canonical `LegalParty` identity, verified identity evidence recorded under configured identifier namespaces, versioned authoritative billing profiles, and the representation relationship between a party and the tenant authorized to confirm it. General external registry integration and know-your-customer verification are deferred; Accounts records evidence that an authorized setup operator confirms.

**Billing account.** `BillingAccount` is an entity owned by BSS Accounts and fixed to one payer tenant, one commercial seller tenant, one issuer party and one confirmed debtor party, with one billing currency for its charges and invoices. Distinct sellers, issuers, debtors or currencies require distinct accounts; multiple accounts with the same complete binding remain allowed. The account organizes financial assignments; it is neither a tenant nor a new RBAC role-assignment scope. A payer tenant may hold accounts with several sellers; one account may serve several subscriptions and legs. Each commercial leg has one effective billing assignment at a time. Splitting one financial obligation across several payers is not introduced.

**Billing arrangement.** Accounts records the consent that in-scope purchases of one commercial buyer tenant may use one account: a payer-owned `AccountUseOffer` (account, named buyer tenant, purchase scope, optional resource-tenant restriction, limits, approval mode, validity) and a buyer-owned `AccountUseAcceptance` bound to one immutable version of that offer. The arrangement is effective only while both match and neither side has stopped it; either side stops **new** uses by revoking its own record. When payer and commercial buyer are the same tenant the cross-tenant consent records are not required and none is fabricated; the grant is still issued through the same protocol against the account's binding, the buyer's own accepted terms and purchase permission, and the pinned tariff, coverage and consumption policy.

**Account-use grant.** Accounts authorizes each exact use through one transaction that serializes against revocation and issues an `AccountUseGrant`; consumers admit a purchase or renewal only with durable grant evidence.

**Seller and issuer eligibility.** Accounts owns confirmed issuer profiles (as parties), the seller-to-issuer `IssuerBinding`, and coordination of seller billing setup with Ledger (Q7); Ledger owns books, accounting configuration and book resolution.

### 4.2 Out of Scope

Independent account wallets, account-level credit limits and transfers between isolated account pools are deferred; no automatic pooling across sellers, books, debtors, payer tenants or currencies is introduced. Accounts does not own balances, payment instruments and mandates, debt ledgers, issued invoices, tax calculation, service lifecycle, billing assignments, book resolution or negotiated sales terms. Orders permits purchases without an order or line `contractId`; such purchases can use an effective arrangement and a grant without a negotiated contract, and this proposal does not make Contracts mandatory. Agency, debt assumption, master agreements, same-tenant inter-entity sale, automatic party deduplication, external registry lookups and KYC remain outside the initial model.

## 5. Functional Requirements

### Billing-party identity

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-party-identity`

A `LegalParty` has an opaque, stable identifier that is never reused, renamed or merged in financial history. Identity evidence is recorded as identifiers under configured **namespaces** (for example a national company register or a VAT register), each with a verification status, evidence reference and the confirming actor; a raw tax number without a namespace is never compared. Which namespaces are accepted, what evidence verification requires, and how representation and suspected duplicates are resolved are **setup contracts** of the deployment, not universal rules.

Distinctness and sameness decisions follow from verified evidence only, under the **canonical-identity semantics each namespace declares** (whether one entity holds exactly one identifier in that namespace, and how values are normalized). Two parties are the **same** when they share a verified identifier in such a namespace or a confirmed alias. Two parties are **distinct** only when verified evidence establishes distinctness under a namespace whose identifiers are canonical one-per-entity, or when an authorized setup operator records an explicit distinctness assertion with evidence; different raw identifiers, the absence of a shared identifier or the absence of an alias are **not** proof of distinct entities, because one entity can legitimately hold several valid identifiers. An unverified or undetermined pair supports **no** distinctness, same-debtor or pool decision (fail closed). A confirmed alias records equivalence for new checks; it never silently authorizes a transfer or settlement-pool merge and never changes the economic identity of historical facts, which keep their original party reference.

Each party has exactly one **representing tenant** in the initial model: the tenant authorized to confirm its evidence and edit its profile. This is an explicit initial constraint, not a necessity; reassignment is an authorized setup action with evidence. Billing profiles (legal name, addresses, tax identifiers) are versioned and authoritative in Accounts; other gears reference `(party_id, profile_version)`, and Invoicing stores immutable issuance snapshots of the values used on each document. A later profile version never rewrites an issued document. A sponsor that funds purchases for a debtor sees the debtor party id, legal name and verification status needed to match accounts, not the full profile.

**Actors**: `cpt-cf-bss-accounts-actor-setup`

### Account binding and debtor validation

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-account-binding`

**Selected account-to-debtor boundary (Q8.1).** A BillingAccount fixes exactly one debtor party in addition to its payer tenant, commercial seller tenant, issuer party and billing currency. Different debtors require different accounts; multiple accounts with the same complete binding remain permitted. The debtor recorded on a purchase and invoice is validated against the buyer's accepted terms and the account's debtor party at grant time; account separation does not add debtor semantics to Ledger by itself.

A BillingArrangement is account → commercial buyer tenant. One account may have arrangements with several buyer tenants only when each use's validated debtor is the account's single debtor party. Payer = buyer removes the cross-tenant consent records, not debtor or profile validation. Neither the payer's declaration nor a tenant identifier establishes the debtor; require a verified party reference.

**Selected account boundary.** In the resale chain, Distributor has an account with Provider, Reseller with Distributor, Customer with Reseller. A payer-level overview may aggregate a payer's accounts for reporting; it merges no obligations and permits no automatic offsets. Invoice consolidation in v1 stays within one BillingAccount: its invoice-cycle preference is validated and executed by Invoicing, which may combine compatible ready charges from several subscriptions while retaining their individual periods and attribution. A subscription may request separate invoicing for its leg. Accounts owns no bill run or issued document; cross-account reporting is a statement, not one consolidated invoice. Payer-tenant ownership remains the authorization model; the seller needs explicit permissions and scoped views for its side of the relationship. Seller affiliation grants no access to the payer's accounts with other sellers.

**Selected currency boundary.** One account has one billing currency. A payer purchasing from the same seller in EUR and USD uses separate accounts. Each commercial leg must resolve prices and billable amounts in its account's billing currency; an incompatible assignment fails validation. Different legs of one subscription may use different currencies while sharing the common period. No path exists in the initial model to re-bill an existing leg in another currency: a new currency is a new purchase version on another account, not a transfer.

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-setup`

### Buyer, debtor, payer and recipient separation — Q8

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-party-separation`

**Selected debtor and third-party payment principle (Q8).** The legal debtor for a commercial leg is the confirmed party of that leg's commercial buyer, as established by the accepted transaction terms. The buyer tenant and its party are distinct references. A BillingArrangement and a grant authorize use of the payer's account to fund the purchase; they do not transfer the buyer's obligation to the payer. Explicit assumption or transfer of debt is deferred to a separately supported workflow.

Keep five facts distinct on every leg: the buyer that authorizes the purchase, its confirmed debtor party, the payer account authorized to fund it, the document recipients, and the payment instruments mandated for collection. One organization may fill several roles, but equality is never inferred because one axis is available. A remitter reference alone establishes no ownership of unapplied amounts. Preserve the accepted debtor identity with the leg and financial documents independently of the payer assignment; changing a payer or editing a profile never reassigns a debt. Ledger determines the financial consequences for debtor and payer; Accounts records who was authorized to fund what.

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

### Consented account use — Q2

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-arrangement`

**BillingArrangement.** The consent is recorded as a payer-owned `AccountUseOffer` — account, named commercial buyer tenant, purchase scope (catalog/product scope, optional resource-tenant restriction), limits, approval mode, coverage policy and validity, each change producing a new immutable terms version — and a buyer-owned `AccountUseAcceptance` bound to one offer version. The arrangement is effective only while the acceptance names the offer's current version, both are within validity, and neither has been revoked or withdrawn. The payer revokes its offer; the buyer withdraws its acceptance; the payer blocks or closes the account. Each act stops **new** uses from the moment it commits and never un-authorizes an issued grant (see the grant requirement). Neither party can fabricate the other's consent; Q10 governs one actor acting for both parties.

Authorized actors purchasing for the buyer may use an effective arrangement subject to account usability, the buyer purchase rule in the [commercial PRD](../../../../docs/arch/bss-commercial-model/PRD.md#independent-purchase-authority--q1), supplier acceptance and any per-use payer approval. One buyer may hold acceptances for several offers; one account may hold offers to several buyer tenants only under the single-debtor rule. When payer and buyer are the same tenant, no cross-tenant offer or acceptance is required; purchase authorization, supplier acceptance, account usability, lineage and disclosure checks and the grant still apply, including when the resource tenant differs.

**Selected arrangement counterparties (Q2).** Apply account → commercial buyer independently to each leg. In Provider → Distributor → Reseller → Customer, Distributor uses its own account to buy from Provider without any arrangement with Customer. If Holding funds Reseller's purchase from Distributor, the arrangement is Holding's account → Reseller, consented to by Holding (offer) and Reseller (acceptance). Customer approves nothing by being the resource tenant; its own order authority and permitted disclosure are validated independently. If Customer is itself the buyer, its acceptance is required as the buyer.

**Q2 scope and examples.** An offer may cover Reseller's purchases only for resource tenant ACME; a purchase for another resource tenant fails scope and must not be silently charged elsewhere. Broader scope must be explicit and still requires validated lineage and disclosure per purchase (Q6 in the commercial PRD). Scope grants no resource access.

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

### Account-use grant

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-use-grant`

Every purchase, renewal, increase or payer transfer that charges an account requires an `AccountUseGrant` for that exact use. The consumer first records a **provisional intent** — a prepared terms version with a durable attempt identifier, a use reference (document or leg and version) and a digest of the exact proposed terms (buyer and seller tenants, account, debtor party, product scope, resource lineage, currency, quantity for fixed components, pinned price binding or accepted price rule and units for usage components, the accepted product term as requested coverage, consumption policy) — that incurs no obligation, purchases nothing and activates nothing. Actual metered facts later determine usage amounts under the pinned tariff; a price change outside the pinned rule is a new use requiring new authority. Obtaining a quote or a conditional, non-binding preparation under its own authority may precede the grant; what is prohibited is incurring, purchasing or activating without it.

Accounts authorizes the use in **one transaction** that locks the account and, for a sponsored use, the offer and acceptance (plus approval when required), validates the proposal against the **exact** approved terms — sponsored acceptance must name the offer's current version; self-funded use validates the account owner's accepted terms and purchase authority without fabricating an arrangement; buyer, seller, debtor party, scope, resource restriction, currency, amounts and coverage must match the approved terms, with no acceptance of changed-but-eligible terms — and records the grant with pinned versions and the terms digest. That commit is the single linearization point between authorizing a new use and revoking, withdrawing or blocking: the order of the two commits decides, and recorded times are evidence only. Revocation after the commit stops new uses and never un-authorizes the issued grant for its exact use. The grant records authorization for account attribution; it is not a money reservation, debt assumption, supplier acceptance, customer acceptance, instrument mandate or document-recipient selection.

Attempt identifiers are scoped to the authenticated consumer principal, and lookup or replay is authorized for that principal before any outcome is disclosed. Replay of the same attempt returns the original durable outcome — authorization, refusal, release or end — never a new grant; a different terms digest under the same attempt is rejected; at most one grant may ever exist for the same consumer, use reference and immutable version, including a released or ended one, so a second attempt after a lost reply returns the existing grant only when the canonical terms match, and rejects a mismatch instead of creating another; a refusal is recorded on its attempt and creates no grant, and the consumer's next try after a refusal is a new use version; outcomes are recorded monotonically and a released or ended grant never authorizes a new admission; an admitted use is ended, never released, also when its use record is delayed, and a use is released only after the consumer has committed that it was not admitted; a timeout is an unknown outcome to be reconciled by the same identifier, never treated as refusal or success. Consumers admit a purchase only after storing durable grant evidence, and their own admission serializes locally against abandonment so that a delayed worker cannot admit an abandoned or released use; the consumer's workflow deadline and the grant's coverage term bound delayed admission, so no indefinite permission to buy exists. Authorized-but-unused grants are resolved from the consumer's durable state (record the use, or release), never by arbitrary timeout deletion. Automation never admits a purchase: in Orders, admission is the caller-authorized flow or the client's retry with the same idempotency key, and scheduled Subscriptions renewals keep their own authority. Required v1 consumer work, none delivered: the consumer commits each local outcome together with the obligation to deliver it to Accounts and repeats the delivery until confirmed (at-least-once and idempotent), and it closes an unresolved attempt as not admitted only when its deadline has passed or the source definitively no longer admits it, without an age timeout, a grant expiry or a deadline extension. Until delivery a grant may remain authorized. A released or ended grant retains its history as valid attribution and correction evidence; it is neither a refund nor a deletion of financial facts.

**Actors**: `cpt-cf-bss-accounts-actor-consumer`, `cpt-cf-bss-accounts-actor-payer`

### Spending admission and optional purchase approval

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-spending`

**Spending authority and disclosure.** Commercial eligibility, authority to incur an obligation and authority to read data are separate decisions. **Default: an effective arrangement authorizes in-scope uses by users who have purchase permission in the named commercial buyer tenant**; the payer does not approve each purchase in this mode. Admission still requires buyer purchase authorization, supplier acceptance, a usable account, an effective arrangement when payer and buyer differ, scope and limit compliance, and the grant. Billing-read permission is not purchase permission.

**Coverage and consumption policy.** The offer declares what the payer authorizes per use: coverage for a single billing period or for a full commitment term up to a maximum; for fixed-price components the quantity and amount per use; for usage-based components the tariff the payer accepts (price binding or price rule, units, scope) and a **consumption policy**. **Coverage is the accepted product term (selected for v1):** the term under the product's frozen rules, with its term semantics and anchor geometry, and the maximum is a term length in those semantics, distinct from the billing cadence. v1 has no separately configured calendar funding end; products with fixed-calendar terms keep them, and the end of an offer's validity limits new authorizations only. For an initial purchase the calendar interval follows the actual service start under the existing product rules, without backdating; a renewal covers the already fixed next term. A grant authorizes exact purchase terms, never an unknowable final usage amount, and unknown future usage does not by itself refuse an otherwise authorized usage purchase. Consumption policy values: `accepted_variable_under_tariff` — the payer accepts the amounts the pinned tariff produces for actual metered consumption within scope and coverage (alerts and estimated spend may be configured but are not caps); `quota_controlled` — a named usage-quota entitlement owned by Subscriptions whose check state is enforced at the point of use by the operational support system or resource provider (OSS), not by Accounts. A quota-controlled offer must specify the entitlement scope and units, the permitted point-of-use behaviour at the limit (block or degrade) compatible with the sold service terms, stale-state handling, and the lag and overshoot exposure the payer accepts: quota state is fed from the rating pipeline with a lag, so a quota bounds further consumption but is **not** a guaranteed financial cap, and usage consumed before the check state changed is still attributed and invoiced. An offer may select `quota_controlled` only for products whose entitlements provide that quota with the specified behaviour; an unsupported or conflicting control mode fails at offer setup or admission, not the metered purchase as such. A full-term sponsored commitment is admitted only when the grant covers the full term **before** the obligation is incurred; a mismatch refuses the use or lets the buyer choose an already-defined alternative such as its own account. Variable tariff-based amounts require explicit payer acceptance of the pinned tariff and exposure. `accepted_variable_under_tariff` records that acceptance without quota enforcement. `quota_controlled` includes the same explicit funding acceptance plus the named quota control: the accepted terms and grant record funding for actual eligible usage, including the stated rating lag and overshoot exposure. These are alternative control profiles, not two mutually exclusive funding authorities; quota selection alone is no consent to unspecified exposure. Missing acceptance refuses admission. Usage outside the accepted scope/coverage remains an exception, never silently charged to another payer; usage being in scope never implies unlimited funding. No mode promises a cap implemented by declining invoices after usage occurred, no mode guarantees zero overshoot, and no financial credit or budget engine is introduced.

**Accepted terms and account defaults (selected for v1).** The applicable, explicitly accepted purchase terms determine commercial and payment conditions. Account defaults fill only values that those terms leave unspecified and permit to default; a request field cannot override a governing agreement. Consumers resolve and retain the effective values and source versions for the admitted purchase. Changing account defaults does not change an already admitted purchase; a subsequent amendment or renewal uses its own authorized version and the required fresh grant. This rule changes no Pricing or Contracts precedence within their own domains and makes no negotiated contract mandatory.

Funding permission is an independent condition, not a lower-priority source of terms. An offer's product scope, coverage, accepted tariff, limits and approval requirements still constrain account use; fixed account bindings and mandatory platform rules cannot be overridden by a contract or default. An unresolved conflict prevents admission rather than silently relaxing consent or changing the purchase terms. For example, accepted payment terms of 60 days override an account default of 30 days where account use is otherwise authorized; a contract for Backup does not authorize charging an account whose offer covers only Compute.

**Optional per-use approval.** An offer may require explicit payer approval of each use. The approval binds the authenticated consumer principal, immutable use reference and version, and the exact terms digest. Authorization must match all of them; identical commercial terms on another purchase or consumer cannot reuse the approval. A changed version requires renewed approval; a static permission is not an approval; approval of account use is not customer acceptance of an order. An unavailable required approval fails closed; no fallback to arrangement-level consent exists.

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

### Confirmed billing profiles

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-profiles`

**Selected billing-profile authority.** Accounts is authoritative for the confirmed billing profiles of debtor and issuer parties and for payer/account details, with their roles distinguished under Q8. The debtor profile describes the buyer's party; a payer-owned account references it and never substitutes its owner for the debtor. Payer and document-recipient details are separate facts. AM metadata may prefill proposed values; authorized confirmation by the representing tenant makes a profile version usable, and later AM edits never change it. Corrections to the same party's details create a new profile version; **replacing** the debtor or issuer party of an account is not a profile edit — it requires a new account and a separately authorized process that handles existing obligations. Q7 assigns issuer profiles and issuer bindings to Accounts; book configuration, accounting and book resolution stay in Ledger.

**Actors**: `cpt-cf-bss-accounts-actor-setup`

### Seller and issuer eligibility — Q7 and Q11

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-seller-setup`

**Selected seller setup boundary (Q7).** Accounts owns the confirmed issuer parties a seller tenant may invoice under (`IssuerBinding`, versioned) and coordinates seller billing setup through Ledger's provisioning contract. Ledger resolves and owns the book for each binding and context; Accounts never becomes a second book resolver and does not assume one book per issuer.

**Selected seller-type eligibility (Q11).** Selling in one's own name and provisioning seller books require a tenant type in the deployment-configured seller set, initially the platform and partner types; organization-type tenants are buyers in this baseline. The restriction is BSS configuration enforced by Ledger's provisioning guard and checked by Accounts at setup; no AM billing trait is introduced. A permitted seller may also buy upstream.

Keep four independent checks: eligible tenant type, confirmed issuer binding and book setup, applicable offer eligibility, and platform permission for the caller's action. Creating an account confers no seller eligibility. A customer organization that wishes to sell needs a separately created tenant of an eligible type under authorized setup; nothing converts the original tenant or migrates its resources or obligations. Blocking new business never deletes books or history.

**Actors**: `cpt-cf-bss-accounts-actor-setup`

### Verified bilateral confirmation — Q10

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-representation`

**Selected bilateral-confirmation actor rule (Q10).** One authenticated user may record both the offer and the acceptance of an arrangement, or both sides of a bilateral agreement, only when independently authorized for each party's action and when the operation does not require distinct participants. Two party-attributed acts are recorded, each bound to its document version and authority, with the same actor identity preserved; switching tenant context creates no second participant; one act never counts for the other party.

Operation-specific safeguards apply independently: Orders D-130 recording-party restriction, Subscriptions transfer maker-checker and break-glass participant separation are not relaxed; a delegation proof is not a bypass; a service executing a use under prior authority records that execution, not a fabricated human confirmation.

**Representation and evidence boundary.** Reuse platform authorization for the named action in the relevant tenant scope together with verified party evidence and the recorded confirmation. Ancestry alone is neither action permission nor consent. Delegation credentials, named commercial permissions, representation evidence and signature-recording formats remain delivery prerequisites; no separate BSS permission engine or representative registry is introduced, and a software permission does not by itself establish legal signature sufficiency.

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`, `cpt-cf-bss-accounts-actor-setup`

### Prospective eligibility and retained history

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-history`

Revoking an offer, withdrawing an acceptance or blocking an account prevents **new** grants from the moment it commits; the end of an offer's validity does the same for new authorizations. Issued grants keep authorizing their exact use for the pinned coverage; legitimate ends of coverage are the end of the pinned term, cancellation or termination of the purchase under its applicable terms, a completed transfer to another eligible account with the same debtor party, or the end of the service. The end of funding authorization never erases remaining obligations, including termination debt; Ledger determines debtor and payer consequences.

Access to current purchases and subscriptions and access to historical charges and documents are evaluated separately; revocation never erases payer attribution and reading previously incurred obligations never requires an ongoing arrangement. A payer transfer assigns the leg to another eligible account with the same debtor party under the supported transfer contract, in v1 only from a shared period boundary and with the new grant issued by then; replacing the debtor needs a new account and a separately authorized process. Account separation provides no user-access isolation and no right to disclose all account fields to the buyer.

**Actors**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

### Currency and financial-account boundaries

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-fr-currency-pool`

Billing currency is distinct from the seller book's functional currency and from the payment currency; no automatic conversion is introduced, and any conversion uses the explicit Pricing, Payments or Ledger contract with retained evidence. Changing an account's billing currency in place is not supported.

**Selected settlement model.** Accounts share settled funds only when compatible on all of payer tenant, debtor party, seller tenant, issuer party, Ledger-resolved book and billing currency; these are compatibility dimensions, and the pool itself is Ledger-owned financial state, not an Accounts balance, wallet, credit limit or RBAC scope. Accounts records no balance. Payment designation enforcement, allocation ordering, credit classes and per-account outstanding reporting are Ledger requirements referenced here, not Accounts behaviour. Shared funds grant no purchasing permission and enlarge no scope.

**Actors**: `cpt-cf-bss-accounts-actor-payer`

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Tenant and party isolation

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-nfr-isolation`

Every account, party, offer, acceptance and grant operation enforces its action permission and authorized party view, including lists, counts and exports. An account identifier or commercial relationship alone grants zero additional user permissions; Accounts introduces no account-level RBAC scope. The service operations (account-use authorization, grant outcome recording including release and end, and status reads) are reachable only by the named BSS service principals, which are provisioned by the operator or onboarding and never by a business worker; because a service role bounds the action and the resource type, not the purchase, Accounts checks the effective arrangement and grant-to-use linkage as applicable. Replay and mutation require the authenticated caller to own the use; separately permitted downstream readers may read another consumer's grant status after source/use validation through a narrow response schema, without replay or mutation rights. Verification: negative authorization cases, service-path enforcement tests (user and unauthorized service calls denied; foreign-consumer replay and mutation denied; separately authorized downstream status reads succeed only with valid source/use linkage) and consumer contract checks before release.

#### Target-scoped service access

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-nfr-target-scoping`

Every internal data operation is scoped to the one verified target tenant needed for that operation. Accounts obtains separate payer, buyer and consumer-home scopes and uses them within one transaction; it never enumerates all platform tenants to authorize a use. Requesting a target is insufficient: reject a PDP response that permits any alternative without that target fence. Standing service roles, target ownership evidence and per-use consent remain independent requirements. A projection derives its recipient from authoritative source state and binds that same recipient to the scope and row owner.

Verification includes deployments with 10,000, 10,001 and a proposed target of 100,000 tenants, malformed/overbroad provider answers, self-managed targets and source/recipient mismatch. This is a capacity-verification requirement, not a measured support claim. Foreground scope size must remain one tenant per decision above those counts; no total-tenant threshold may deny unrelated uses. Dependency overload fails the affected operation, without scope widening. Tenant lifecycle checks and bounded autonomous work discovery follow the commercial DESIGN §3.4 contract.

#### Evidence preservation

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-nfr-evidence`

Every confirmed profile, consent, approval and grant change retains its version, effective time and actor evidence; current edits never alter evidence referenced by prior assignments. Verification: replay and history inspection.

#### Current-view freshness

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-nfr-current-view-freshness`

Any current view that depends on an Accounts fact is served only after fresh authoritative validation of its basis for the request, page or export batch: offer and invitation views require a current, unrevoked offer and an unblocked account; grant-backed purchase, assignment and document views require a grant that is authorized or used with coverage including the shown interval, and they survive offer revocation or account blocking (which stop new uses only) while staying limited to covered billing facts. No positive disclosure decision is cached, and unavailability of Accounts withholds the affected current response. Consumers also validate the authoritative admission/use state and effective end in Orders or Subscriptions in the same read. An `authorized` or `used` grant alone is insufficient: delayed end delivery to Accounts cannot keep an ended current slice visible. Missing or unavailable source-use status fails closed. Historical financial evidence of authorized uses is separately authorized and unaffected. A numeric freshness bound is not asserted; cached or leased validation is future work once a supported bound is agreed with the authorization owner. Verification: revocation concurrent with reads, delayed and out-of-order events, source outage.

### 6.2 NFR Exclusions

No exemption from platform security or reliability baselines is selected. Account wallets and a gear-owned permission engine are outside scope.

## 7. Public Library Interfaces

### 7.1 Public API Surface

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-interface-management`

An SDK boundary provides party and profile management, account management, offer and acceptance lifecycle, approvals, grant authorization and use recording, and authorized summaries. Required capability, not a published endpoint or stable DTO; HTTP exposure follows the platform operation builder and RFC 9457 problem responses.

### 7.2 External Integration Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-contract-eligibility`

Provided to consumers: a read-only eligibility evaluation, the one-transaction grant authorization returning durable evidence (grant id, pinned versions, terms digest, coverage), use outcome recording and release, grant status reads for fresh validation, and consent, account and party lifecycle events. Wire format, locking protocol and the platform service capability it requires are specified in the [DESIGN](DESIGN.md).

## 8. Use Cases

### Sponsored Compute and self-funded Backup

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-usecase-sponsored-service`

Holding holds account H-ACME with ACME's party as debtor; ACME holds A-ACME. Holding's offer to ACME covers Compute; ACME accepts. ACME's Compute purchase obtains a grant on H-ACME and is assigned to it; Backup obtains a grant on A-ACME. A second subsidiary with its own party needs another Holding account and its own offer and acceptance. Revoking the offer prevents new grants under it; it changes no issued grant, no other subsidiary's arrangement and no earlier invoice.

**Actor**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

### Separate accounts for different debtors

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-usecase-single-debtor`

Holding funds Reseller GmbH's 100 EUR purchase and Partner Ltd's 70 EUR purchase from the same Distributor issuer through accounts H-RES and H-PART. Holding may receive an authorized aggregate report of 170 EUR; obligations and payment designations stay distinct, and a designation to H-RES never authorizes allocation to H-PART.

**Actor**: `cpt-cf-bss-accounts-actor-payer`

### Holding pays without assuming the buyer debt

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-usecase-third-party-funding`

Distributor sells to Reseller for 100 EUR under terms naming Reseller GmbH as debtor; Holding's grant on H-RES authorizes attribution to its account. A valid Holding payment allocated to the invoice settles it; non-payment leaves the debt with Reseller GmbH, authorizes no collection from Reseller's account and transfers nothing to Holding. Ending Holding's offer preserves incurred obligations.

**Actor**: `cpt-cf-bss-accounts-actor-payer`

### Two confirmed issuers for one seller

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-usecase-seller-issuers`

Reseller invoices under EU GmbH and UK Ltd, two parties with confirmed issuer bindings. For a purchase, Orders resolves the seller from the catalog offer and validates the account's issuer party against the binding; Ledger resolves the book for that issuer and context and the pin is recorded on the grant and assignment. A buyer cannot select an issuer or book by supplying an identifier; currency is a constraint, not a routing rule.

**Actor**: `cpt-cf-bss-accounts-actor-setup`

### One authorized actor records two confirmations

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-usecase-dual-confirmation`

A finance director authorized in both Holding and Reseller records Holding's offer and Reseller's acceptance as two acts with the same actor identity and separate party, version and authority evidence; a finance-reader permission alone can do neither; failure of one party's authority rejects that act even if the other succeeded.

**Actor**: `cpt-cf-bss-accounts-actor-payer`, `cpt-cf-bss-accounts-actor-buyer`

### Grant issued, consumer crashes, offer revoked

- [ ] `p1` - **ID**: `cpt-cf-bss-accounts-usecase-grant-recovery`

Orders stores a provisional intent with attempt identifier `a7`, calls Accounts, and crashes before storing the returned grant; Holding then revokes the offer. When the buyer's client retries the submit with the same idempotency key, Orders replays `a7` and receives the same grant, admits the purchase on that evidence and records the use. Had the revoke been committed before the authorization, the replay returns the recorded refusal, and the buyer's next try with another funding choice is a new attempt for a new order version. If the user abandons the intent, Orders first commits that the version was not admitted and then releases the grant; a later replay returns `released`, never a new grant. If the client never retries, Orders does not admit: once the attempt can no longer be admitted under existing rules, it is closed as not admitted and the grant is released (required v1 consumer work, not delivered).

**Actor**: `cpt-cf-bss-accounts-actor-consumer`

## 9. Acceptance Criteria

- Two accounts referencing the same debtor party compare equal by party id, never by profile text; a profile edit creates a new version visible to both while issued documents keep their snapshots; an unverified party yields no distinctness or same-debtor decision.
- With EU and UK issuers for one seller, admission validates the issuer party against the binding and Ledger's resolved book; a supplied identifier or currency cannot choose it; issuer replacement is not a same-issuer transfer.
- For Distributor's 100 sale to Reseller funded by Holding, the debtor is Reseller GmbH's party; missing payment or revoked funding transfers no debt and charges no other account; recipient and instrument facts cannot substitute for debtor identity.
- H-RES cannot fund Partner Ltd's purchase; a payment designation to H-RES authorizes no allocation to another debtor; extra accounts create no access boundaries.
- A revoke committed before `authorize_use` refuses the grant; a revoke committed after leaves the grant and its exact use authorized; replay of an attempt returns its original outcome; a mismatched terms digest is rejected; a timeout is reconciled by the same identifier.
- A user independently authorized in two tenants may record both consent acts with one preserved actor identity; tenant switching or delegation cannot bypass a distinct-participant rule or D-130.
- An organization-type tenant cannot provision seller books or become a seller by creating an account; a configured partner type still needs issuer binding, book setup and permissions.
- Current views that depend on consent or grant facts are withheld when Accounts is unavailable and reflect revocation on the next request without a positive cache; historical evidence stays readable under its own permission.

These are acceptance requirements for a future implementation, not executed tests.

## 10. Dependencies

| Dependency | Required contribution | Criticality |
|---|---|---|
| AM / platform authorization | Tenant facts, action permissions, service-principal identity, narrow service roles and target-scoped decisions for the named BSS service actions, verified representation | p1 |
| Orders / Subscriptions | Provisional intents, use recording, retained assignment evidence, fresh status reads | p1 |
| Contracts | Applicable negotiated party and financing constraints | p1 |
| Ledger / Invoicing | Book resolution and provisioning for issuer bindings; issuance snapshots of party profiles | p1 |
| Approvals | Decision inbox forwarding to Accounts | p2 |

## 11. Assumptions

A tenant is not proof of a legal party. Verified identity, representation and issuer bindings need explicit setup contracts. Consumers infer no consent from ancestry, account selection or event delivery. BSS service principals and their roles exist per deployment before Accounts serves a sponsored use; no immediate-revocation guarantee is made, and token lifetime, caches and in-flight operations must be accounted for. The service scope is a finite tenant list issued by the PDP; its provisioned ceiling is a deployment constraint, and an overflow denies the operation.

## 12. Risks

| Risk | Consequence | Required control |
|---|---|---|
| Use admitted without a grant committed before the revocation | Payer funds what it refused | One-transaction grant serialization; admission only on durable grant evidence |
| Payer confused with debtor | Incorrect receivable attribution | Separate party references validated at grant time |
| Accounts treated as access scopes | Unintended disclosure | Tenant permissions and explicit response views |
| Profile edit substitutes legal party | Historical debt silently moves | Correction versus separately authorized replacement |
| Unverified identity used for distinctness | Wrong Q13 or pool decision | No decision without verified evidence or recorded assertion |

## 13. Open Questions

The v1 architecture and component contracts are specified in [Accounts DESIGN §§3–4](DESIGN.md#37-database-schemas--tables) and [commercial DESIGN §§3.8–3.11](../../../../docs/arch/bss-commercial-model/DESIGN.md#38-orders-terms-and-subscriptions-contracts), including approval forwarding, immutable use versions, terms provenance, grants, recipients and recovery. They remain proposed for owner review, not implementation claims.

Deployment must supply accepted identity namespaces/evidence policies, service principals/roles, quotas and operational budgets. Per-tenant service assignments and cached authorization leases remain future alternatives; they do not replace the mandatory per-call single-target v1 contract. Owner acceptance and implementation verification are recorded in DESIGN, separately from design decisions.

## 14. Traceability

- [Accounts DESIGN](DESIGN.md)
- [Ownership ADR](../../docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md)
- [Resale ADR](../../docs/ADR/0003-cpt-cf-bss-adr-multi-tier-resale-model.md)
- [Commercial-model PRD](../../../../docs/arch/bss-commercial-model/PRD.md)

Q2, Q7, Q8, Q8.1, Q10 and Q11 are discussion references, not evidence of approval. No Accounts UPSTREAM_REQS exists; this draft derives from the ownership ADR and consumer gaps without changing existing consumer specifications.
