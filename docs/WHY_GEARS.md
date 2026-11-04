# Why Gears

> **Audience:** technical decision-makers evaluating Gears as a foundation — CTOs and chief architects, engineering/platform leads, and ISVs building products on top. It assumes familiarity with multi-tenant SaaS concerns (tenancy, auth, deployment) and marks implementation status inline where a capability is still being built.

> **Status:** evolving — captures the target architecture and direction rather than the full current implementation. For the present Gears scope and implementation status, see [GEARS.md](GEARS.md).

## What Gears actually is

Gears turns the recurring, high-risk parts of XaaS engineering into a well-architected, secure foundation of composable libraries — reusable OSS and BSS capabilities, extensible and customizable API contracts, automatic safeguards, runtime extensions, integrations automation, and efficient operation from a single server to global scale.

In practice, Gears is a versatile, well-architected set of efficient Rust libraries and a runtime for building multi-tenant backends and frontends for XaaS (SaaS, IaaS, PaaS) platforms and services — a proper architecture for business scale rather than a starter kit for early prototypes. You do not deploy "Gears" as is; instead you pick the libraries (called **gears**) you need — API gateway, account and user management, subscriptions and licenses, events and audit logging, gen ai, serverless runtime, etc — configure and compose them into your own set of foundational microservices, and finally add your own business logic on top to shape the final product.

Each gear:

- owns its REST/gRPC API and its database schema;
- talks to other gears through a stable public Rust SDK interface;
- can run in the same process as its callers or in a separate process or microservice without changing its code.

That last point is the design center of the whole system - many benefits below follow from it.

Two things Gears is **not**: it is not a ready-made SaaS you run as is, and it is not a replacement for AWS/GCP/K8s. It's a middleware that sits between the infrastructure and your product.

For a CTO and their engineering org, Gears + Studio means shipping products faster, at higher quality and scope, and cheaper to develop and run — because teams reuse proven gears (specifications, designs, APIs and code) instead of rebuilding each product or service from scratch and because Studio is optimized for Gears lifecycle automation.

### The Gears catalog

The target catalog is 200+ gears covering the generic, reusable patterns an XaaS product keeps re-implementing, grouped into APIs and contracts, Gen AI, Serverless, Core Functionality, OSS (operations), and BSS (monetization). What Gears deliberately does **not** contain is the logic that makes your product yours. That stays outside the foundation: in your own open-source or proprietary gears built on the same SDK, contracts, and controls; in serverless functions and workflows configured per deployment, tenant, or user (planned; see §3); or in external code written in any language and connected to Gears through REST/gRPC APIs.

So the foundation gives you the common platform, and you spend your effort on differentiating product logic rather than rebuilding the platform underneath it.

## Reasons to build on Gears

These points are **not steps in a sequence** — they depend on each other. Where a capability is still being built out, the section that discusses it says so.

| # | Factor | What it gives you |
|---|---|---|
| 1 | XaaS DNA in every Gear | 200+ reusable OSS and BSS gears that package common patterns, built-in XaaS concerns, and a shared product-portfolio foundation |
| 2 | Composable capabilities | Single-process AI-friendly development, configurable deployment topology from one codebase, and infrastructure-agnostic deployment |
| 3 | Multi-level logic customization | Product-level composition, third-party applications and integrations, and safe tenant/user customization |
| 4 | Security & isolation | Compile-time-validated request processing, safe extension coexistence, and policy-driven access control |
| 5 | Gears lifecycle automation | Specification and code quality control, AI tuned for Gears, and lessons converted into enforceable standards |

---

## 1. XaaS DNA in every Gear: proven patterns packaged for reuse

> 200+ reusable OSS and BSS gears that allow you to build and run your product portfolio faster. Common XaaS patterns are delivered as specifications, decision guides, stable contracts, and reference implementations; tenancy, identity, security, policy, licensing, usage, billing, audit, and observability follow one coherent architecture.

### Overview

This factor includes:

1. **Incorporate typical patterns as Gears libraries:** common XaaS patterns are represented end to end, from requirements and design decisions through stable contracts and reusable implementations.
2. **Build XaaS concerns in:** the foundational capabilities every product needs follow one architecture instead of being reimplemented and integrated differently by each team.
3. **Provide a common foundation for the product portfolio:** products reuse interoperable open-source OSS and BSS capabilities while keeping their differentiating business logic private.

### Typical problem

Almost every XaaS service splits into two parts. One is the **unique, competitive business logic** that differentiates the product and drives revenue. The other is the **foundational logic** every service needs no matter what it does — tenancy, users, products, licensing, subscriptions, provisioning, policies, secrets, authentication, notifications, AI capabilities, usage, and billing.

The foundational layer must always be secure, reliable, versatile, and performant. Its complexity grows with the service's scope and scale—what is adequate for an initial release rarely meets the requirements of a mature, large-scale product.

Many useful foundational open-source components work well as standalone services but cannot be dropped into a large-scale XaaS product unchanged because their foundational models are incomplete or incompatible with common XaaS requirements for:

- tenant hierarchies, resource ownership, and isolation;
- user roles, delegated administration, and approval workflows;
- authentication, authorization, access policies, and impersonation;
- usage metering, quotas, licensing, and admission control;
- audit trails, distributed tracing, and incident investigation;
- secrets management and controlled outbound egress;
- data retention, deletion, and operator-defined policies;
- runtime extensibility through custom data types, APIs, callbacks, and hooks.

The result is integration code around every component that also needs to be maintained, tested, and secured. Each team pays the foundational tax again and solves the same problems differently. That requires additional effort, creates security gaps, and raises maintenance, performance, and troubleshooting costs as system scale grows.

### Example: a real LLM gateway is not just a proxy

A toy LLM gateway receives a prompt, sends it to a provider, and returns the response.

A production multi-tenant LLM gateway must also, on every request:

1. ensure the model is enabled, approved, and not deprecated for this tenant;
2. ensure the tenant has an active license for the model and requested features;
3. authenticate the caller and verify its user, application, and impersonation context;
4. authorize the requested model, operation, tools, and data sources;
5. enforce token, spend, rate, and concurrent-request limits for both the tenant and user;
6. validate request shape, size, modalities, tool definitions, and structured-output schemas;
7. route to a compatible provider and region according to capability, residency, cost, and latency policy;
8. store and resolve tenant- or user-scoped provider credentials securely;
9. restrict outbound traffic to approved providers, endpoints, and redirect targets;
10. apply data-classification, retention, and content-safety policies before data leaves or enters the platform;
11. enforce deadlines, cancellation, retry, fallback, and circuit-breaking rules without duplicating side effects or charges;
12. handle streaming backpressure and client disconnects without leaking resources or continuing unwanted work;
13. record usage and cost idempotently, then reconcile reserved quota with the actual result;
14. audit authorization, routing, policy, and administrative decisions without storing sensitive content unnecessarily;
15. keep metrics, traces, and logs useful for troubleshooting while redacting prompts, credentials, and sensitive user data;
16. return stable, sanitized errors without leaking provider internals or making callers depend on one backend.

That is the difference between a working proxy prototype and a production service that can survive a security review and operate in a paid SaaS product.

### Why it matters

The same tenancy, policy, licensing, usage, security, and audit questions exist in most business services: account management, provisioning, eventing, notifications, search, audit, chat, workflows, billing, integrations, and admin APIs. Rebuilding the same rules in every service without proper control and consistency leads to fragmented and insecure systems.

### How Gears addresses it

#### 1. Incorporate typical patterns as Gears libraries

Every capability starts as a traceable spec (PRD → DESIGN → ADR → FEATURE with linked IDs), so intent is explicit and reviewable before code exists. Those specifications, designs, decision guides, and API contracts ship *with* the gears. A team can reuse them as-is or take ideas from them, so the whole SDLC—requirements → design → API → implementation → tests—starts from a proven baseline instead of a blank page.

The accepted design becomes one engineering DNA shared by every gear: SDK-first crates and DDD-light layers, route declarations and generated OpenAPI, canonical RFC 9457 errors, tracing/metrics/health/lifecycle conventions, and a uniform testing and CI approach. Each gear owns its contract and implementation behind a stable public SDK interface.

Each gear is designed to be useful independently: it can be selected and consumed through its published API and SDK, with required dependencies declared explicitly rather than hidden in another gear's internals. It ships with its specifications, design and usage documentation, can be configured or extended through declared capabilities and plugins instead of a private fork, and inherits the platform's zero-trust defaults.

#### 2. Build XaaS concerns in

API ingress/egress, authn/authz, tenancy, credentials, type registry, events, usage, licensing, and the other parts of the OSS and BSS layer are gears behind SDK and plugin contracts. An ISV picks the auth provider, policy engine, storage backend, and commercial controls its product needs instead of inheriting one hard-wired choice or rebuilding the surrounding integration layer.

These concerns are governed consistently across gears. Compile-time-validated contracts and runtime controls govern API and database operations by tenant, user, license, and policy, with integrated observability, auditing, and notifications. The secure request path and isolation mechanisms that enforce these controls are explained in §4.

#### 3. Provide a common foundation for the product portfolio

The target catalog is 200+ gears covering the generic, reusable patterns an XaaS product keeps reimplementing. Gears fits the current technology stack in two ways: run it as a **standalone platform you build on**, or **integrate it into an existing platform through plugins and adapters** that wire its contracts to services already in operation.

What Gears deliberately does **not** contain is the logic that makes your product yours. That stays outside the foundation, in one of three places:

- **your own open-source or proprietary gears** built on the same SDK, contracts, and controls;
- **serverless logic** — functions and workflows configured per deployment, tenant, or user (planned; see §3);
- **external code in any language** that uses Gears through its REST/gRPC APIs.

The foundation therefore supplies common platform capabilities across a product portfolio while each product keeps its differentiating logic private. Teams spend their effort on that logic rather than rebuilding the platform underneath it.

### Benefits

- **Executive:** wider product portfolio and faster delivery — launch more products on a common governed Gears foundation instead of funding the same platform work repeatedly.
- **CTO:** capital and operational efficiency from reusing shared security and commercial controls without giving up quality, scalability, reliability, or performance.
- **Developer:** focus on key product logic on top of a tenant-scoped, secure, and performant platform instead of reinventing foundational capabilities for each service.

References: [Architecture Manifest — secure data path](ARCHITECTURE_MANIFEST.md#31-secure-xaas-framework-with-defense-in-depth), [Gears inventory](GEARS.md), [AuthN/AuthZ and Secure ORM](toolkit_unified_system/06_authn_authz_secure_orm.md).

---

## 2. Composable capabilities: one codebase, any topology

> Separate business logic from service packaging — choose, configure, and package only what you need with DSL. Write the logic once; develop the complete product in a single process, then repackage the same code for a small on-premises deployment or a large production cloud.

### Overview

This factor includes:

1. **AI-friendly single-process product development:** write, build, run, and test the complete product locally, giving engineers and AI agents the shortest change → feedback loop.
2. **Scale down or up from one codebase:** repackage, replicate, and shard selected gears for large-scale production without rewriting business logic.
3. **Remain infrastructure agnostic:** target public cloud, private cloud, on-premises, or edge deployments without coupling product logic to one provider.

### Typical problem

Teams usually choose deployment boundaries at the same time as business boundaries. A component becomes either:

- a monolith module that is hard to scale or isolate later; or
- a microservice with a separate build, deployment, network contract, logs, and test environment from day one.

Real product scenarios cross those boundaries. Troubleshooting requires correlating metrics and logs from several services. Integration tests need containers or a cluster. Later merging two chatty services, or extracting a hot path from a monolith, becomes a refactoring project rather than an operational decision.

The problem compounds with many teams and with AI-assisted development. When several teams share one platform, a change one team needs usually becomes a pull request into a shared component — scoped, reviewed, merged, and released centrally before the requesting team can move; teams wait on a queue they don't control, or fork the shared code and inherit a permanent merge cost. And an AI agent can propose code quickly, but it cannot accelerate a change it must wait minutes or hours to verify through image builds, deployment, remote logs, and cluster E2E tests.

### Example: evolve an AI product from laptop to production

Suppose a Chat product needs API Gateway, authz, tenants, users, licensing, LLM gateway, a chat gear and usage collection for billing.

For development, compile them into one process. A developer or coding agent runs the real cross-gear scenario locally:

```text
edit → build + lints → run in-process test → inspect failure → fix
```

There are no per-component builds and deployments, no shared staging cluster, and no digging through logs from separate services just to find an integration mismatch.

In production, move the tenants, users, LLM gateway and usage collection to dedicated replicas because they have distinct latency and scaling needs. Callers keep the same SDK interface. The business gear code does not need a rewrite because the runtime swaps local dispatch for a remote client.

### Why it matters

Deployment topology should follow requirements — scalability, latency, fault isolation, tenancy, data residency, cost, and capacity — rather than the boundaries teams happened to define when they first created the repositories. The same is true of team boundaries: independent teams should ship on their own cadence, not serialize through one shared release.

This range of composition and deployment scenarios is not flexibility for its own sake. Exercising the same gears across binary, on-premises, and cluster shapes — and across many teams and editions — surfaces and hardens them, improving overall **system quality** and yielding a **proper architecture for business scale**. Composition and configuration also let each product take on only the capabilities it needs, so **customization controls complexity** instead of adding to it.

### How Gears addresses it

Gears interfaces are **versioned contracts** and no gear may depend on another's internals or schema, independent teams and software vendors can build different services on the same foundation without a central bottleneck:

- **Compose gears into executable services:** group compatible gears behind the service boundaries that fit your product, just as you compose other libraries into an application.
- **Pin and upgrade on your own cadence:** a consumer pins a gear version and upgrades when it chooses, so a producer's release never forces a consumer to move.
- **Refactor freely:** internals change behind a stable contract without breaking anyone.
- **Extend without touching the core:** a change another team needs is usually a plugin behind an existing SDK trait, a runtime extension (§3), or an additive contract version — not an edit to a shared gear.

#### 1. Enable AI-friendly single-process product development

Business logic is separated from how it is packaged into a service. A gear exposes a stable SDK interface, and consumers depend on that interface, not the implementation:

- **same process:** direct Rust-native call, with no serialization or network;
- **separate process or pod:** generated REST/gRPC client implementing the same interface.

For local development, compatible gears compile into a single process and execute realistic cross-gear flows without per-component deployment. Engineers and AI agents get fast compiler and functional logic test feedback against the complete product rather than waiting for a remote environment to build, run the tests and collect logs for troubleshooting.

#### 2. Different deployments from one codebase

Gears allow to have both different products and different deployments from one codebase by gears composition and defining needed deployment profiles: Embedded, Host + Workers, and Kubernetes Native. Embedded is the current default for local development. The distributed model has active foundations and documented gaps; full equivalence is still being completed.

Gears DSL called GDL (Gears Definition Language) makes composition explicit and validated. It declares:

- gears, versions, dependencies, and capabilities;
- how gears will be packaged into executable processes;
- plugin/provider bindings;
- OS-native or Kubernetes-based deployment;
- target infrastructure environment — public clouds, private clouds, on-premises;
- scalability configuration — limits, replicas, sharding;
- custom data types and roles;
- latency, availability, resource, and residency constraints;
- required database, cache, lock, and discovery guarantees.

The GDL compiler should reject an impossible composition before deployment — for example, a missing capability, insufficient hardware resources, incompatible SDK versions, a backend that cannot meet a consistency requirement, or a placement that violates residency constraints.

- **Scale down:** co-locate compatible gears into one binary and use local calls.
- **Scale up:** split, replicate, or shard selected gears and bind remote clients.

#### 3. Remain infrastructure agnostic

Infrastructure agnosticism comes primarily from the Gears ToolKit, which abstracts OS interfaces, database access, gear discovery, distributed-execution primitives, and other infrastructure services behind stable contracts. In every deployment shape, gear business logic uses the same interfaces while the composition selects implementations appropriate for OS-native, Kubernetes, public-cloud, private-cloud, on-premises, or edge environments.

This is analogous to operating-system drivers: applications use stable OS interfaces without knowing the details of each hardware device, while drivers adapt those interfaces to a particular implementation. Similarly, a gear requests capabilities such as persistence, discovery, messaging, locking, or remote execution without embedding the details of a specific cloud or infrastructure product; ToolKit bindings and adapters provide the environment-specific implementation.

For choices that are specific to a gear rather than common infrastructure, the gear can expose its own plugin contract. For example, a gear may define a persistence interface and provide plugins for storing its data in a relational database or a document-oriented database. The gear's domain logic remains unchanged while the product selects the implementation that fits its consistency, query, scale, and operational requirements.

Together, ToolKit abstractions and gear-specific plugins keep the product portable across environments. Teams can choose infrastructure based on cost, performance, reliability, or data-residency requirements instead of hard-wiring the system to a single vendor.

### Benefits

- **Executive:** wider product portfolio and faster delivery — one efficient codebase supports public cloud, private cloud, on-premises, and edge deployment with no single-vendor lock-in, while more teams and ISVs deliver in parallel on one foundation.
- **CTO:** change process boundaries, replication, sharding, and infrastructure targets without rewriting business logic; avoid shared-component merge queues and forks, with opt-in upgrades per team.
- **Developer / AI / ISV:** run realistic multi-gear flows locally with fast compiler/test feedback instead of waiting for cluster deployment; consume a pinned gear version for stable, reproducible builds.

References: [Deployment profiles ADR](arch/toolkit-oop/ADR/0001-cpt-cf-adr-deployment-profiles.md), [Distributed Gears PRD](arch/toolkit-oop/PRD.md), [SDK/OoP pattern](toolkit_unified_system/09_oop_grpc_sdk_pattern.md), [Gears inventory and dependency rules](GEARS.md).

---

## 3. Multi-level logic customization: products, vendors, tenants, and users

> Define the initial product shape, then let third-party vendors, AI agents, customers, and integrations extend the product at runtime. ISVs shape the compiled core by composing gears and plugins; vendors add integrations and applications through stable contracts; tenants and users customize data, UI, workflows, and automation without rebuilding the product.

### Overview

This factor includes:

1. **Select and configure what each product needs:** compose gears, plugins, policies, and deployment profiles into product editions using DSL.
2. **Let third-party vendors extend the product:** expose stable application and integration contracts so external ISVs and system integrators can build on the product without access to or changes in its core.
3. **Let tenants and users customize safely:** tailor data, UI, workflows, and automation at runtime, with independent lifecycle and scope.

### Typical problem

Large-scale XaaS products require extensibility across three different dimensions:

- **ISV-level customization:** an ISV wants a platform or ready-to-use building blocks for its service, and usually wants to ship more than one product from the same base — different deployments, free and commercial editions, hosted (SaaS) and on-prem/installable versions, and separate products for different segments. Every one of those still has to apply the same core constraints — a tenancy model, enabled capabilities, access policy, licensing, metering, admission control, audit. Off-the-shelf open-source components rarely have all of those controls in place, so integrating them safely into each edition is the hard part.

- **Third-party vendor-level extension:** an external ISV or system integrator needs to build an application or integration on top of the product without access to its source code. The extension needs its *own* data types, events, jobs, UI, connectivity settings, API mappings, and lifecycle, while remaining compatible with the product and isolated from other vendors' extensions.

- **Tenant-level customization:** a customer wants to run its own AI agent or automation workflow, asks for a custom field, widget, setting, an approval rule, or a specific per-user workflow — small changes that should not be a part of the core product.

### Example: the same order product, extended at three levels

An ISV ships an "orders" edition by **composing** the order, approval, pricing, and notification gears and selecting its policies and deployment profile. A third-party vendor adds an **adapter application** for a specific ERP through published extension contracts.

On top of that running product, tenant A adds a new orders type with a `cost_center` property and an approval **workflow** for given orders over a threshold; tenant B adds different properties and a transformation function. Neither tenant forks the order gear, and both stay inside the same governed API, tenancy, and audit boundary.

### Why it matters

First-class customization capabilities keep the compiled core small and stable while custom product editions, integrations, vendor applications, and per-customer behavior evolve independently. Small changes in the core do not require new gears — they can be delivered as extensions instead. This way, new functionality ships faster without touching or rebuilding the main system core.

### How Gears addresses it

Gears supports customization beyond the logic written in gears and fixed at compile time. It uses the [Global Type System](https://www.globaltypesystem.org) (GTS) to define and version API and data contracts, custom object types, properties, events, roles, and permissions. Serverless functions and workflows add custom server-side behavior, while cloud-side sandboxes isolate that code and enforce its application identity, tenant scope, permissions, resource limits, secrets, and network access. Together, these mechanisms support three extension levels: product composition at build time, partner applications at runtime, and tenant- or user-scoped customization.

#### 1. Select and configure what each product needs

An ISV builds its service on pre-defined gears that already carry the core controls, and shapes them to the product without modifying the gears it reuses:

- **Compose** the set of gears the product needs into its binary or cluster.
- **Configure** — with the planned DSL — the tenancy model, access-control policies, licensing, metering, and audit that the product must enforce, instead of hand-wiring those controls into each component.
- **Write plugins** behind a host gear's SDK trait — e.g. a new auth provider, a storage backend, a search engine — discovered at runtime via the type registry and resolved through `ClientHub`.
- **Write adapters** to integrate external systems behind the same contracts.

The primary way an ISV does this is Constructor Studio with a dedicated UI, AI assistant chat and GDL. **AI helps with scope** (what gears exist, what capabilities they provide, and which fit the product), and **GDL validates the result** (versions, capabilities, and options are compatible and satisfy the declared requirements and constraints). The model proposes, the compiler proves.

The result is a product that reconfigures original gears for a specific product edition's needs, so the ISV owns its deployable profile without forking the core. The same foundation can then be recomposed into other editions (free/commercial, small-scale/large-scale, hosted/on-prem, per-segment) by changing the DSL declaration rather than editing or duplicating gears.

#### 2. Let third-party partners and vendors extend the product at runtime

The Gears-based product vendor publishes governed extension contracts that external ISVs and system integrators use to build applications and integrations without access to or changes in the product source code. The extension surface can include UI extension points, versioned APIs and events, data types and custom properties, durable objects, roles and permissions, lifecycle hooks, and serverless functions and workflows.

An **application** is a packaged extension with its own vendor identity, manifest, version, compatibility range, permissions, data, UI, and behavior. An **integration** is an application that connects the product to an external system and can add connectivity settings, API mappings, transformations, synchronization jobs, and integration-owned events.

The main extension mechanisms are:

- **GTS for APIs and data:** versioned, schema-validated product and vendor-owned types, properties, events, jobs, and settings. The Types Registry rejects incompatible schemas at registration.
- **Serverless for behavior and durable state:** functions, workflows, and durable objects for mappings, transformations, synchronization, automation, and AI-agent skills. They run within the identity, permissions, and scope granted to the application.

Applications have a lifecycle independent of the core product: they can be released, installed, enabled, disabled, upgraded, or downgraded separately. Installation makes an application available; a product operator or tenant administrator activates it for selected tenants or users. Declared compatibility is checked before activation, and application data, APIs, routes, secrets, usage, and behavior are isolated by default (§4).

#### 3. Let tenants and users customize safely

Product and operations teams can introduce a new user role, tighten an access policy, or launch a new pricing or licensing model globally or for a category of tenants or users. Tenants can add their own UI customizations, data fields, settings, AI agents, workflows, and automation. Per-tenant, per-group, or per-user customization uses the same extension model at a narrower scope.

A customer, partner, or product team can customize the product without access to or changes in the product source code, either in AI chat or manually using the guidelines:

- Basic customization can be done in a declarative way.
- Advanced customization can be developed as redistributable code.
- Custom code can extend an existing product function or replace its implementation through defined extension points.
- Customization code is stored separately from the mainstream product code.
- Customization code has its own lifecycle: a customization can be released and redistributed without releasing a new product version.
- Customizations are update-safe. A product upgrade must preserve a customization when its declared extension contracts remain compatible; incompatibility must be detected before activation rather than breaking the running product.

Customizations can be scoped, layered, disabled, and moved:

- A customization can apply to the whole system, a tenant, a group of users, or one user.
- A customization can be disabled from the UI. Once disabled, the affected scope uses mainstream product behavior.
- Multiple customizations from different vendors or teams can apply to the same product module or screen and must coexist under deterministic composition and conflict rules.
- Customizations are layered. System-wide, tenant, group, and user layers combine at runtime to determine behavior for the current user, with explicit and inspectable precedence rules.
- Customizations can be exported and imported.

### Benefits

- **Executive:** ecosystem growth and customer retention — enable third-party vendors to extend product capabilities independently, increasing the product's value and integration reach without consuming the core product team's capacity.
- **CTO:** keep a small, governed compiled core while product management, operations, ISVs and tenants configure, customize and extend it at runtime.
- **Developer / AI / ISV:** keep core gears clean and stable — implement product-specific requirements and small customizations as separate extensions instead of accumulating customer-specific branches, feature flags, and logic in the core codebase.

References: [GTS architecture](ARCHITECTURE_MANIFEST.md#35-extensible-domain-model-via-global-type-system), [Serverless roadmap](GEARS.md#serverless), [ClientHub and plugins](toolkit_unified_system/03_clienthub_and_plugins.md).

---

## 4. Security & isolation: a shared platform with enforced boundaries

> Many customers, partners, and integrations share one platform safely. Tenancy, authentication, authorization, database access, and logging follow one guarded request path; third-party extensions are isolated by default; and policy governs users, AI agents, integrations, and applications.

### Overview

This factor includes:

1. **Secure, compile-time-validated request processing:** tenancy, authentication, authorization, database access, and logging follow one guarded request path.
2. **Safe coexistence of third-party applications and extensions:** deny-by-default isolation prevents cross-tenant and cross-application access while retaining shared-infrastructure efficiency.
3. **Policy-driven access control:** configurable RBAC/ABAC policies govern users, AI agents, integrations, and applications down to row and data-type scope.

### Typical problem

Once tenants, integrations, add-on services, and AI agents can define their own data and behavior (§3), a new risk appears: everything now lives in the same platform or product instance, often in the same tenant. Without a strict boundary:

- integration B could read events that integration A produced;
- one integration could report usage against a usage type another integration owns;
- a custom setting, notification type, or subscription attribute from one app or agent could leak into another;
- a function from one service could operate on objects it does not own and must not see;
- an unscoped database query could expose another tenant's rows;
- an extension could call an undeclared internal API or external route, or leak a secret through logs.

In most stacks this boundary is convention plus review, which does not hold once third parties are extending the system. Some platforms offer a dedicated per-tenant service or platform instance instead, but that approach carries higher operational and infrastructure cost and does not scale far.

### Example: two integrations that cannot see each other

Integration A declares an event type for a change in some external object. Integration B declares its own event type for a different system.

Through GTS namespacing and ABAC checks in the event-broker gear, integrations A and B cannot subscribe to or emit each other’s events: B never receives A’s events, and A never receives B’s. The same isolation applies to settings, notifications, subscription attributes, and usage types: **integration B cannot report usage under a usage type defined by integration A**. Each integration behaves as if it had its own private slice of the platform, while still running inside the shared tenant and infrastructure.

### Why it matters

Isolation is not an add-on to extensibility — it is what makes extensibility safe. Without it, opening the platform to per-tenant, per-integration, and third-party logic would inevitably mix data and behavior that must stay separate. With it, many tenants, integrations, and applications can share services without leaking into one another, avoiding the infrastructure and operational cost of a separate stack for every customer.

### How Gears addresses it

#### 1. Secure, compile-time-validated request processing

Gears ToolKit libraries provide a secure-by-default request path, while `cargo gears lint` verifies at build time that gears follow required architecture patterns:

```text
Client
  → API Gateway authenticates the API client and establishes SecurityContext
  → gear calls PolicyEnforcer as the policy enforcement point (PEP)
  → policy decision point (PDP) evaluates access and returns row-level constraints
  → PolicyEnforcer compiles those constraints into AccessScope
  → Secure ORM applies AccessScope to database queries
  → gear returns a typed response or a canonical RFC 9457 Problem
```

These runtime checks execute **on every protected request** as platform capabilities each gear inherits rather than as per-service decisions. Architecture lints complement them by rejecting invalid declarations and prohibited implementation patterns before merge. Secure ORM applies row constraints before a query reaches the database, preserving correct filtering, counts, and pagination while preventing unscoped access.

At the language level, safe Rust prevents broad classes of memory-safety vulnerabilities—including use-after-free, out-of-bounds memory access, dangling references, and data races—before code runs. The workspace-wide `unsafe_code = "forbid"` policy prevents first-party gears from bypassing those guarantees. This removes an entire vulnerability category, while the guarded request path remains responsible for application-level security such as identity, authorization, tenancy, and data access.

The same guarded path extends beyond tenant scoping: credentials use platform secret facilities, sensitive data must be kept out of logs and error details, and canonical API and error contracts avoid accidental leakage of provider or implementation internals.

#### 2. Let third-party applications and extensions coexist safely

Every GTS data type and function carries its **vendor package and namespace** in its identifier. That makes ownership machine-checkable, so the platform can scope data and logic to the integration, application, service, or user that owns them. Because ownership is part of the type or function identity rather than a runtime convention, the same boundary holds across storage, events, usage, and API access.

Locally developed and third-party applications are isolated from one another by default. An application from vendor A cannot access vendor B's data or APIs unless the required permission is explicitly requested and granted:

- Consent is explicit: the user or tenant administrator can see what data is requested, which vendor requests it, and why, then grant or reject access.
- An application declares all required permissions in a manifest that is easy to review before deployment or consent.
- Internet routes, internal APIs, and AI skills are declared explicitly; undeclared access is denied.
- Application UI logic is untrusted, including UI supplied by trusted vendors, and is sandboxed so it cannot escape its assigned boundary or interfere with other applications.
- Application secrets are stored through platform credential facilities, not in application code or user-visible configuration.
- The platform enforces sensitive-data redaction or rejection and audits application API communication without exposing secrets or sensitive payloads.

Containers, namespaces, and sandboxes complement contract- and policy-level controls where process or UI isolation is required. Many tenants and extensions can therefore share services safely instead of requiring an independent platform instance for each customer.

#### 3. Apply policy-driven access control

Gears applies **unified, data-type-based RBAC and ABAC** using namespaced GTS identifiers: a role or attribute grants access to specific types and methods, not to everything. Tenants define their own policies so different user roles, AI agents, integrations, and applications reach only the objects and operations they are allowed to use.

On every request, API Gateway establishes `SecurityContext`; `PolicyEnforcer` asks the policy decision point for a decision and row-level constraints; and `AccessScope` turns those constraints into Secure ORM predicates. A tenant, user, or integration therefore reads and writes only the rows it owns. This is **zero trust by construction**: nothing is reachable unless policy explicitly grants it, with deny-all as the default.

### Benefits

- **Executive:** higher customer density and stronger retention — serve more tenants on shared infrastructure while safely hosting sandboxed third-party integrations that broaden the product offering and reduce churn.
- **CTO:** lower security risk and review burden — platform-enforced isolation, compile-time checks, and ownership-based RBAC/ABAC limit the impact of developer mistakes instead of relying on every team to implement every boundary correctly.
- **Developer / AI / ISV:** focus on business logic and deliver it faster — inherit tenancy, authentication, policy enforcement, data scoping, secrets handling, and safe logging instead of rebuilding security controls in every service and integration.

References: [GTS architecture](ARCHITECTURE_MANIFEST.md#35-extensible-domain-model-via-global-type-system), [AuthN/AuthZ and Secure ORM](toolkit_unified_system/06_authn_authz_secure_orm.md), [ClientHub and plugins](toolkit_unified_system/03_clienthub_and_plugins.md).

---

## 5. Gears lifecycle automation: quality that improves and becomes enforceable

> Constructor Studio brings UI, DSL, and AI operations for Gears and automates the whole delivery lifecycle. It controls specification and code quality, applies AI tuned for Gears, and turns lessons learned into reusable guidance and deterministic enforcement.

### Overview

This factor includes:

1. **Specification and code quality control:** detect contradictions, gaps, traceability breaks, and violations of security, architecture, and reliability rules before release.
2. **AI tuned for Gears:** improve quality and cost through optimized model routing, Gears-specific context, and tuned open models.
3. **Re-enforceable learning:** turn lessons learned into reusable guidance, compiler constraints, static analysis, and deterministic CI checks.

### Typical problem

As a system grows, three problems appear together:

- **Drift:** each service acquires its own specs format, auth/database pattern, API and error conventions, observability, testing, and review expectations. Knowledge from one service does not transfer to the next — for people or AI agents.
- **Late detection:** a general compiler and generic linter allow many designs that compile but are wrong for this platform — an ORM query that skips tenant scoping, raw SQL in a domain layer, a route without an auth/error/schema contract, a remote call while a transaction is open, a secret reaching a log, or a dependency on a gear's internals. These surface as data leaks, lock contention, unstable APIs, and security findings, caught (if at all) by review and an ever-growing test suite. That does not scale, especially with AI-generated code.
- **Manual assembly:** choosing which gears fit, wiring them together, and upgrading versions is manual and error-prone, so integration risk grows over the product's life.

### Example: adding a tenant data-export endpoint with AI

A team asks an AI coding agent to add `GET /v1/orders/export`. A generic implementation can compile and pass a single-tenant happy-path test while still querying the database directly, omitting row-level access scope, returning ad hoc errors, skipping audit and usage controls, or exporting sensitive fields that were never covered by the requirements.

Gears lifecycle automation moves the change through progressively stronger controls:

1. **Specification quality:** traceable PRD → DESIGN → ADR → FEATURE relationships make intent reviewable and allow tooling and AI to identify contradictions, gaps, and broken traceability before implementation.
2. **Before coding, specification checks expose gaps:** who may export, which tenant and rows are in scope, which fields are sensitive, whether the capability requires a license or quota, and what must be audited.
3. **Gears-tuned AI validates the generated code:** it analyzes the code change semantically and in its full context, checking semantics, contracts, error handling, data scoping, and use of platform patterns; it flags issues directly to frontier AI agents and human authors before the code is committed.
4. **The build rejects architectural bypasses:** enforces multi-tenancy, authentication, authorization, licensing, and audit at compile and lint time so that code which skips required guards, scopes, or contracts cannot pass the build.
5. **New lessons become shared enforcement:** if review or production reveals a reusable defect—for example, unbounded exports or spreadsheet-formula injection—the fix is captured in guidance, templates, static analysis, or contract tests so every future gear and AI-generated change inherits it.

The result is not merely faster code generation. It is a controlled path from an incomplete request to a specified, on-pattern, machine-validated capability, with each discovered defect improving the next implementation.

### Why it matters

Specifications and standards do not keep implementations consistent by themselves. Without automated checks, the same architecture and security requirements must be verified repeatedly in code review, and violations will eventually be missed. Encoding recurring requirements and defects as shared lints, compiler constraints, and contract tests applies the same checks to every gear and every change.

Early, deterministic validation reduces the number of defects that reach integration testing or production and makes failures easier to diagnose. A consistent project structure and machine-readable contracts also let engineers and AI agents work with less gear-specific context, while reviewers can focus on business behavior and genuinely new risks instead of checking the same platform conventions manually.

### How Gears addresses it

#### 1. Control specification and code quality

Gears moves each recurring concern through a managed lifecycle and applies the strongest available validation at each stage:

```text
specification   →   standard   →   compile-time enforcement   →   tooling
(PRD/DESIGN/ADR)    (one DNA)      (executable architecture)      (choose/compose/upgrade)
```

- **Specification quality:** traceable PRD → DESIGN → ADR → FEATURE relationships make intent reviewable and allow tooling and AI to identify contradictions, gaps, and broken traceability before implementation.
- **Code quality:** Rust's type system and a workspace-wide `unsafe_code = "forbid"` policy; Clippy blocking raw SeaORM execution methods; Secure ORM typestate with deny-all empty access scopes; `OperationBuilder` typestate requiring route metadata; `cargo gears` / Dylint rules for layer isolation, SQL placement, versioned REST paths, and GTS conventions; schema generation and contract tests turn platform expectations into build-time checks.
- **Lifecycle quality:** the same contracts make choosing, wiring, and upgrading gears validated rather than manual.

Constructor Studio supports the lifecycle through a UI, requirements DSL, composition DSL, and AI workspace:

- **Choose:** the catalog and AI assistant identify which gears and capabilities fit a product (§3).
- **Compose and validate:** GDL declares gears, versions, bindings, deployment target, and constraints; its compiler rejects an impossible composition — missing capability, incompatible SDK/GTS versions, or an unmet consistency or residency constraint — before deployment (§2).
- **Register and discover:** each gear carries a manifest and is discovered at runtime through the Types Registry, then resolved through `ClientHub`; plugins bind behind SDK traits without hard-wiring.
- **Upgrade:** interfaces are versioned, upgrades are opt-in and checked at compose time, and the Types Registry rejects incompatible GTS changes at registration, so a bad upgrade fails early rather than in production.

#### 2. Tune AI for Gears

Studio's AI assistance is built specifically for Gears development and Gears-based product development. It combines an optimized model router, tuned open and custom models, training, and reinforcement grounded in Gears specifications, contracts, and conventions. Requirements and composition DSLs, together with continuously reinforced static analysis, give models precise Gears-specific context.

Because the model works against a known, machine-checkable structure, it can produce more correct, on-pattern output at lower token, review, and rework cost than a general-purpose assistant. Routing selects the appropriate model for quality and cost, while tuning makes common Gears tasks require less repeated explanation.

#### 3. Make learning re-enforceable

The accepted design becomes one engineering DNA every gear shares: SDK-first crates and DDD-light layers, route declarations and generated OpenAPI, canonical RFC 9457 errors, tracing/metrics/health/lifecycle conventions, and a uniform testing and CI approach. There is one way to develop, build, operate, and reason about every gear.

Where a mistake becomes known, the standard becomes a rule that fails the build rather than waiting for a test or production incident. Findings from reviews, AI analysis, or incidents are converted into specifications, decision guidance, compiler constraints, architecture lints, static analysis, contract tests, and deterministic CI checks.

The target direction widens this net: a lint for remote calls while a transaction guard is held; sensitive-data annotations that keep a field out of logs, tracing, schema output, API responses, and error details; and AI-assisted discovery of repeated review or incident patterns that then become deterministic rules. AI can identify the pattern; the compiler enforces it on every future change.

### Benefits

Together, Gears and Studio let a team build and assemble products **faster** (a running baseline of specs, designs, APIs, and code instead of a blank page), with **better scope and quality** (proven capabilities and enforced conventions), and at **lower cost across both development and operations** (reuse instead of rebuild, and defects caught early rather than in production):

- **Executive:** fewer late security, reliability, and compliance surprises, and lower cost to add or swap capabilities over the product's life.
- **CTO:** architecture rules stay enforceable as the platform and team grow; fewer one-off conventions, simpler operations, and version upgrades or provider swaps validated by tooling rather than manual audit.
- **Developer / AI:** a precise build failure with the preferred alternative beats a production incident; familiar structure and machine-checked contracts mean less context to load and a faster path from one reported problem to a repository-wide fix.

References: [Specification templates](spec-templates/README.md), [ToolKit guide](toolkit_unified_system/README.md), [`Cargo.toml`](../Cargo.toml), [`clippy.toml`](../clippy.toml), [`Gears.toml`](../Gears.toml), [Defect class → control map](toolkit_unified_system/16_defect_class_to_control_map.md), [ClientHub and plugins](toolkit_unified_system/03_clienthub_and_plugins.md).

---

## What actually makes the difference

No single one of these five factors above is unique — most can be built by a strong platform team. The value is that they hold together:

```text
XaaS DNA in every Gear (patterns + built-in concerns + portfolio foundation)
  + composable capabilities (single process + configurable deployment topology + infrastructure agnostic)
  + multi-level logic customization (product + third-party vendor + tenant/user)
  + security & isolation (guarded requests + safe coexistence + policy)
  + Gears lifecycle automation (quality control + tuned AI + re-enforceable learning)
```

Most frameworks hand you building blocks and leave assembly, deployment, extension, testing, and operations to you. Gears standardizes those too, so the parts compose instead of drifting apart as the product grows.

The DSL, GTS, and serverless layers extend the same idea: describe the platform and its constraints, let the tooling validate a workable composition, and let teams or ISVs add behavior without making the compiled core or the deployment harder to reason about.

So the claim is not "more features." It is: **Gears turns the recurring, high-risk parts of XaaS engineering into a well-architected, secure foundation of composable libraries** — reusable OSS and BSS capabilities, extensible and customizable API contracts, automatic safeguards, runtime extensions, integrations automation, and efficient operation from a single server to global scale.

## What it's worth

Gears creates value in five places: foundational capabilities are reused across a product portfolio; engineers and AI agents get a shorter local feedback loop; one codebase supports multiple product and deployment profiles; partners add applications without expanding the core product team; and secure shared infrastructure serves more customers and extensions per deployment. Lifecycle automation reduces the review and incident cost of keeping that system consistent as it grows.

A current portfolio-level estimate is that the reusable security, core, billing, and operations capabilities cover functionality otherwise represented by roughly **3–5M lines of foundational code** and avoid **30–50 person-years of repeated platform work**. These are directional planning estimates, not universal benchmarks: the realized value depends on how much of the catalog a product uses, how much equivalent platform code already exists, and which target capabilities are implemented at adoption time.

| Stakeholder | Direct value | What creates it |
|---|---|---|
| **Executive / product business** | More products and integrations per platform investment; higher customer density and retention; less infrastructure lock-in | Shared OSS/BSS foundation (§1), partner and tenant extension model (§3), secure shared services (§4) |
| **CTO / chief architect** | Fewer parallel platform implementations; flexible service boundaries and infrastructure targets; lower security and review burden | Versioned contracts and deployment composition (§2), platform-enforced isolation (§4), validated upgrades and reusable architecture checks (§5) |
| **Product management** | More flexible product positioning — create cloud, on-premises, free, commercial, and segment-specific editions with configurable capabilities and licensing instead of maintaining separate products | Product and deployment composition (§2), customizable product definitions (§3), and built-in licensing and commercial controls (§1) |
| **Third-party vendor / system integrator** | Build and release applications or integrations without modifying the product core or waiting for its release cycle | Published APIs and events, GTS contracts, serverless behavior, application packaging, permissions, and sandboxing (§3–§4) |
| **Tenant / customer** | Product-specific data, workflows, UI, and integrations without a private product fork or dedicated platform stack | Scoped and layered customization (§3), policy-driven access and application isolation (§4) |

## Further reading

- [Architecture Manifest](ARCHITECTURE_MANIFEST.md)
- [Gears inventory and implementation status](GEARS.md)
- [ToolKit Architecture & Developer Guide](toolkit_unified_system/README.md)
- [Distributed Gears PRD](arch/toolkit-oop/PRD.md)
- [Three Named Deployment Profiles ADR](arch/toolkit-oop/ADR/0001-cpt-cf-adr-deployment-profiles.md)
- [Security Overview](security/SECURITY.md)
- [WHY_RUST.md](WHY_RUST.md) — deep technical dive for developers