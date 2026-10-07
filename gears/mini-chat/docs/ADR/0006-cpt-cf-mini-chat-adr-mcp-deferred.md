---
status: accepted
date: 2026-09-26
---

# MCP server support is deferred out of P1

**ID**: `cpt-cf-mini-chat-adr-mcp-deferred`

## Context and Problem Statement

PRD §5.9 and §7.1 specify MCP server support as P1:
* a tenant MCP server registry;
* hub discovery;
* role-level access;
* tool discovery and injection;
* tool execution in an agentic loop;
* an MCP admin REST API.

DESIGN described the full design: components `McpPool`/`McpService`, tables `mcp_servers`, `mcp_server_tools` and `role_mcp_servers`, twelve REST endpoints marked "stable", and "MCP Implementation Phases" with several phases marked **Done** (rate limiter, DLP redactor, refresh worker, OAuth, hub sync).

None of this was built:
* no MCP modules, routes, tables or migrations;
* the only trace is the unused catalog flag `ModelToolSupport.mcp`.

The DESIGN text came from a design-only change that added the PRD and design for MCP server support. The "Done" markers were never true.

## Decision Drivers

* The documents must not describe unimplemented functionality as implemented or stable.
* The MCP design work has value and should be kept for when the feature is scheduled.
* Clients and operators must not rely on `/v1/mcp-servers*` or `/v1/admin/roles/*` endpoints.

## Considered Options

* Implement MCP now to match the documents.
* Record MCP as deferred, keep its design as a feature document, and mark the requirements "Future".

## Decision Outcome

Chosen option: "Record MCP as deferred, keep its design as a feature document, and mark the requirements Future". Implementing MCP is a large feature and out of scope for aligning P1.

| Capability | Requirements / design | Status |
|---|---|---|
| MCP server registry and admin API | `cpt-cf-mini-chat-fr-mcp-server-registry`, `cpt-cf-mini-chat-interface-mcp-api` | Future |
| MCP hub discovery | `cpt-cf-mini-chat-fr-mcp-hub-discovery` | Future (already P2 in PRD) |
| Role-level MCP access | `cpt-cf-mini-chat-fr-mcp-role-access` | Future |
| Tool discovery and injection | `cpt-cf-mini-chat-fr-mcp-tool-discovery` | Future |
| Tool execution in the agentic loop | `cpt-cf-mini-chat-fr-mcp-tool-execution` | Future |
| `McpPool`, `McpService`, MCP tables | `cpt-cf-mini-chat-component-mcp-pool`, `cpt-cf-mini-chat-component-mcp-service`, `cpt-cf-mini-chat-dbtable-mcp-servers`, `cpt-cf-mini-chat-dbtable-mcp-server-tools`, `cpt-cf-mini-chat-dbtable-role-mcp-servers` | Future |
| MCP metrics (`mini_chat_mcp_*`) | PRD §6.2 | Future |

The MCP design text moves from DESIGN to [features/mcp-servers-support.md](../features/mcp-servers-support.md) and is marked "not implemented". DESIGN and PRD keep the requirement IDs with a reference to this ADR.

### Consequences

* Good, because DESIGN now describes only the system that runs.
* Good, because the MCP design is preserved and linked.
* Bad, because a P1 capability promised in the PRD is not delivered; product must reschedule it.

### Confirmation

* Code review: the gear has no MCP implementation; the catalog flag `ModelToolSupport.mcp` (Mini Chat SDK) is set only by test fixtures.
* DESIGN §3.3 endpoint table lists no MCP endpoints.

## More Information

* Re-evaluate when MCP is scheduled. The implementation must then remove the "Future" markers and bring the feature document back into DESIGN.

## Traceability

* **PRD**: [PRD.md](../PRD.md) §5.9, §7.1, §9
* **DESIGN**: [DESIGN.md](../DESIGN.md) §4 "MCP Servers Support"

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-fr-mcp-server-registry`
* `cpt-cf-mini-chat-fr-mcp-hub-discovery`
* `cpt-cf-mini-chat-fr-mcp-role-access`
* `cpt-cf-mini-chat-fr-mcp-tool-discovery`
* `cpt-cf-mini-chat-fr-mcp-tool-execution`
* `cpt-cf-mini-chat-interface-mcp-api`
* `cpt-cf-mini-chat-design-mcp-servers`
