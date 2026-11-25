---
description: "Entitlements uses ToolKit hook plugins and outbox delivery for service-specific extensions."
status: accepted
date: 2026-11-16
decision-makers: "BSS Entitlements owners"
---

Created: 2026-11-16 by Virtuozzo International GmbH
Updated: 2026-11-18 by Virtuozzo International GmbH

# ADR-0002: Run Typed Hooks Through ToolKit Plugins

**ID**: `cpt-cf-bss-entitlements-adr-hook-plugins`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [ToolKit hook plugins](#toolkit-hook-plugins)
  - [Reuse the Subscriptions hook interface directly](#reuse-the-subscriptions-hook-interface-directly)
  - [Require a Serverless bridge](#require-a-serverless-bridge)
  - [HTTP webhooks or broker triggers](#http-webhooks-or-broker-triggers)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Services need custom readiness checks and actions after delivery, such as checking SSO readiness or updating a CRM. Entitlements must retain control of purchased rights, application state and readiness.

Subscriptions already defines `LifecycleHookPluginV1` through ToolKit plugins, scoped ClientHub clients and durable post-commit delivery. Entitlements can use the same pattern without a function runtime. Each hook point defines its own inputs and results.

## Decision Drivers

- Reuse ToolKit plugin registration, scoped clients and outbox delivery.
- Allow different integrations without changing Entitlements business logic.
- Keep commercial decisions and provider execution with their owners.
- Recover lost starts and results under the original identity.
- Restrict each integration's credentials and permitted actions.
- Enable no-hook profiles without a hook implementation.

## Considered Options

1. Use the Subscriptions ToolKit plugin and outbox pattern with Entitlements hook contracts.
2. Call `LifecycleHookPluginV1` directly for Entitlements hooks.
3. Require an executor bridge to Serverless for the first hook release (v2).
4. Manage HTTP webhooks or broker triggers as another host execution mechanism.

## Decision Outcome

Hooks are planned for v2. V1 does not depend on this capability. SDK/GTS version suffixes describe the contract version and remain unchanged.

Choose **ToolKit hook plugins with outbox delivery**. Entitlements owns the GTS inputs/results, hook points, journal, scheduling and result validation. Integration plugins implement the typed SDK, durable execution recovery and downstream calls. The host resolves configured instances through scoped ClientHub and imports no concrete plugin crate.

The points are `readiness_check`, `after_assignment_outcome` and `after_output_state_changed`. Readiness handlers return `passed` or `not_ready` after required adapter criteria pass. They cannot veto a purchase, change rights or provision essential access. Notice handlers run after the source transaction commits; their failures cannot delay restriction, revocation or cleanup.

Bindings pin the plugin instance, implementation version and digest, schemas and restricted execution policy. Existing work keeps that binding. A changed integration needs a new qualified profile version. If the old implementation is unavailable, report failure. Never switch existing work to new code.

V2 loads only deployment-approved integration plugins. In-process plugins are trusted platform code, not a sandbox for customer code. Scoped service identities, credential restrictions, code review and conformance tests enforce the approved integration boundary.

Serverless is not a v2 hook dependency. A future plugin could use Serverless through the same SDK. This would allow function code updates without a plugin release. Its runtime, identity and recovery mapping belong to that implementation's design.

The pre-purchase activation configuration contract belongs to [initial readiness](../features/02-initial-readiness.md#pre-purchase-activation-configuration). It applies to preparation with or without hooks. Hooks add no separate purchase protocol.

### Consequences

- Reuse the same platform integration pattern as Subscriptions. Each host retains its own payloads and authority rules. Integration plugins may share connector code without sharing commercial decisions.
- Entitlements calls no Subscriptions lifecycle hook and adds no execution dependency on that gear.
- Configured plugins must pass E-U09. No-hook profiles remain independent; a configured check cannot be skipped during an outage.
- The plugin saves admission and results across restarts. Entitlements saves delivery intents, check reservations and accepted evidence. An uncertain start never permits a replacement invocation.
- Entitlements owns delivery retries and readiness rechecks. The plugin owns retries within one admitted execution. Notices may cause repeated downstream calls; implementations must deduplicate by notice identity.
- Plugin updates may require deployment. V2 does not promise runtime code editing, tenant-uploaded code, binding migration or manual replay.

### Confirmation

Qualify configured hooks without Serverless. Test a deployment with no hook plugin, scoped identity, implementation changes, lost starts/results, restart, stale readiness evidence, notice deduplication and nonblocking cleanup. Document validation alone does not close E-U09.

## Pros and Cons of the Options

### ToolKit hook plugins

- Benefit: reuses the Subscriptions integration pattern and existing platform infrastructure.
- Benefit: supports typed readiness results and recoverable post-commit actions without a new runtime dependency.
- Cost: integration code must be packaged and deployed; in-process code is trusted.
- Decision: chosen for v2. Keep contract details in feature 07.

### Reuse the Subscriptions hook interface directly

- Benefit: one existing trait with `before_apply` and `after_applied`.
- Cost: those methods describe commercial transitions. Entitlements needs preparation evidence and assignment/output notices with different identities and authority. Reusing the trait would require changing both contracts or hiding their meaning in generic payloads.
- Decision: reuse the infrastructure pattern and connector code where useful. Do not force the same business interface or create a shared BSS API without agreed requirements from both owners.

### Require a Serverless bridge

- Benefit: code can be deployed independently of a plugin release.
- Cost: adds runtime availability, deployment selection, execution identity and result-recovery dependencies. V2 does not require runtime code replacement, so these dependencies are optional.
- Decision: defer to a possible plugin implementation. The host contract does not select a runtime.

### HTTP webhooks or broker triggers

- Benefit: can connect to existing external integrations.
- Cost: direct host support duplicates authentication, delivery and recovery mechanisms; triggers also need correlated readiness results.
- Decision: an integration plugin may use these transports. Entitlements exposes no second delivery mechanism.

## More Information

- [Subscriptions plugin contracts](../../../subscriptions/docs/DESIGN.md#337-adapter-plugins-and-profiles) define the lifecycle hook pattern.
- [ToolKit plugin architecture](../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md) defines schemas, instances and scoped clients.

This decision adds no billing, allocation or quota-usage ledger. Functions, tenant authoring and runtime workflow replacement remain future scope.

## Traceability

- **PRD:** [extension hooks](../PRD.md#67-extension-hooks), `cpt-cf-bss-entitlements-fr-extension-hooks`.
- **DESIGN:** [§3.3.10](../DESIGN.md#3310-extension-hooks), `cpt-cf-bss-entitlements-principle-bounded-hooks`.
- **Feature:** [07 Extension hooks](../features/07-extension-hooks.md).
- **Decision and adoption:** ENT-D-25 in [DECISIONS](../DECISIONS.md), E-U09 in [UPSTREAM_REQS](../UPSTREAM_REQS.md#26-hook-plugin-execution).
