import {
  TeamOutlined,
  ClusterOutlined,
  ApiOutlined,
  AppstoreOutlined,
  SwapOutlined,
  TagsOutlined,
  UserOutlined,
  ProfileOutlined,
  CloudServerOutlined,
  NodeIndexOutlined,
} from "@ant-design/icons";
import { createElement, type ReactNode } from "react";

import { apiFetch } from "../httpClient";
import type { ResourceDescriptor, FieldOption } from "./types";

const AM = "/account-management/v1";

// GTS id prefix shared by every tenant-type entity in the types registry.
const TENANT_TYPE_PREFIX = "gts.cf.core.am.tenant_type.v1~";

/// Load the registered tenant types for the create-form Type select. Reads the
/// types registry and keeps only tenant-type entities (the base prefix itself
/// is skipped — it is not a concrete, assignable type).
async function loadTenantTypes(): Promise<FieldOption[]> {
  const res = await apiFetch<{ entities?: { gts_id: string }[] }>(
    "/types-registry/v1/entities",
  );
  return (res.entities ?? [])
    .map((e) => e.gts_id)
    .filter((id) => id.startsWith(TENANT_TYPE_PREFIX) && id !== TENANT_TYPE_PREFIX)
    .map((id) => {
      const seg = id.split("~").filter(Boolean).pop() ?? id;
      const name = seg.split(".").slice(-2, -1)[0] ?? seg;
      return { value: id, label: name };
    });
}

/**
 * The curated admin resource registry. Authority boundary is the Gears APIs
 * (issue: "prefer existing Gears APIs"); routes/fields mirror the served
 * OpenAPI of account-management, resource-group, types-registry and the gear
 * orchestrator. Adding a resource = appending a descriptor here.
 */
export const RESOURCE_REGISTRY: ResourceDescriptor[] = [
  {
    key: "tenants",
    label: "Tenants",
    owningGear: "account-management",
    tenantScope: "tenant",
    safety: "destructive",
    capabilities: { read: "tenants:read", write: "tenants:write", delete: "tenants:write" },
    // Field types/required/readOnly are derived at boot from the `TenantDto`
    // OpenAPI schema; entries below carry only what the spec can't express —
    // visibility, labels, relations, the GTS-backed type select, and which
    // fields are create-only. `status` keeps a `tag` render override (the spec
    // types it as an enum ref).
    schema: "TenantDto",
    // Pre-fill a valid create form: parent = home tenant, and a registered
    // tenant-type GTS id (the `tenant_type` is a GTS chain, not a free word —
    // the API rejects anything not starting with `gts.`).
    formDefaults: (ctx) => ({
      parent_id: ctx.subject_tenant_id,
      tenant_type: "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~",
      self_managed: false,
    }),
    // CRUD paths derive from the tenants collection (POST create, and
    // GET/PATCH/DELETE on `/tenants/{tenant_id}`). There is no global tenant
    // list endpoint, so list is overridden to the caller's home-tenant subtree;
    // tenant isolation is enforced server-side.
    basePath: `${AM}/tenants`,
    listPath: (ctx) => `${AM}/tenants/${ctx.subject_tenant_id}/children`,
    fields: [
      { name: "id", inList: true },
      { name: "name", inList: true, inForm: true },
      { name: "status", type: "tag", inList: true },
      {
        name: "tenant_type",
        label: "Type",
        inList: true,
        createOnly: true,
        required: true,
        options: loadTenantTypes,
      },
      { name: "parent_id", label: "Parent", relation: "tenants", createOnly: true },
      { name: "self_managed", createOnly: true },
      { name: "child_count", label: "Children", inList: true },
    ],
    actions: [
      {
        name: "suspend",
        label: "Suspend",
        path: (_ctx, id) => `${AM}/tenants/${id}/suspend`,
        capability: "tenants:suspend",
        safety: "destructive",
        visible: (r) => r.status === "active",
      },
      {
        name: "unsuspend",
        label: "Unsuspend",
        path: (_ctx, id) => `${AM}/tenants/${id}/unsuspend`,
        capability: "tenants:suspend",
        safety: "normal",
        visible: (r) => r.status === "suspended",
      },
    ],
  },

  {
    key: "conversions",
    label: "Conversions",
    owningGear: "account-management",
    tenantScope: "tenant",
    safety: "normal",
    capabilities: { read: "conversions:read", write: "conversions:write" },
    // Field types/required/readOnly derive at boot from the OpenAPI schema; the
    // entries below add only presentation the spec can't carry (tag render,
    // labels, list visibility). `side`/`comment` are kept curated as the served
    // DTO names them differently (`initiator_side`) or omits them.
    schema: "OwnConversionRequestDto",
    // List/detail derive from the conversions collection. The API also exposes
    // POST (request) and PATCH (state change), but the panel drives state via
    // the approve/reject/cancel actions below, not a generic create/edit form.
    basePath: `${AM}/tenants/{tenant_id}/conversions`,
    suppressVerbs: ["create", "update"],
    fields: [
      { name: "id", inList: true, readOnly: true },
      { name: "status", type: "tag", inList: true },
      { name: "target_mode", label: "Target mode", type: "string", inList: true },
      { name: "side", inList: true },
      { name: "comment" },
      { name: "created_at", inList: true },
    ],
    actions: (
      [
        { name: "approve", label: "Approve", status: "approved", safety: "normal" as const },
        { name: "reject", label: "Reject", status: "rejected", safety: "destructive" as const },
        { name: "cancel", label: "Cancel", status: "cancelled", safety: "destructive" as const },
      ]
    ).map((a) => ({
      name: a.name,
      label: a.label,
      method: "PATCH" as const,
      path: (ctx, id) => `${AM}/tenants/${ctx.subject_tenant_id}/conversions/${id}`,
      body: () => ({ status: a.status }),
      capability: "conversions:write",
      safety: a.safety,
      visible: (r: Record<string, unknown>) => r.status === "pending",
    })),
  },

  {
    key: "users",
    label: "Users",
    owningGear: "account-management",
    tenantScope: "tenant",
    safety: "destructive",
    capabilities: { read: "users:read", write: "users:write", delete: "users:write" },
    // IdP-backed users for the caller's home tenant. The API exposes only
    // list/create/delete (no get-one / update), so those are the only verbs
    // that derive; the UI degrades gracefully where an op is unsupported.
    basePath: `${AM}/tenants/{tenant_id}/users`,
    schema: "UserDto",
    fields: [
      { name: "id", inList: true, readOnly: true },
      { name: "username", inList: true, inForm: true, required: true },
      { name: "email", inList: true, inForm: true },
      { name: "display_name", label: "Display name", inList: true, inForm: true },
      { name: "first_name", label: "First name", inForm: true },
      { name: "last_name", label: "Last name", inForm: true },
    ],
  },

  {
    key: "tenant-metadata",
    label: "Tenant metadata",
    owningGear: "account-management",
    tenantScope: "tenant",
    safety: "destructive",
    capabilities: {
      read: "tenant-metadata:read",
      write: "tenant-metadata:write",
      delete: "tenant-metadata:write",
    },
    // Entries are keyed by their GTS type_id and upserted with PUT; the body
    // is the bare metadata value (PutTenantMetadataDto is transparent).
    updateMethod: "PUT",
    idField: "type_id",
    createKeyField: "type_id",
    bodyField: "value",
    schema: "TenantMetadataEntryDto",
    // Derives list/one/update(PUT)/remove; there is no POST collection endpoint
    // (create is the keyed PUT upsert above).
    basePath: `${AM}/tenants/{tenant_id}/metadata`,
    fields: [
      { name: "type_id", label: "Type id", inList: true, createOnly: true },
      { name: "value", inList: true, inForm: true },
      { name: "updated_at", label: "Updated", inList: true },
    ],
  },

  {
    key: "resource-groups",
    label: "Resource groups",
    owningGear: "resource-group",
    tenantScope: "tenant",
    safety: "destructive",
    capabilities: {
      read: "resource-groups:read",
      write: "resource-groups:write",
      delete: "resource-groups:write",
    },
    // Field types/required/readOnly are derived at boot from the `GroupDto`
    // OpenAPI schema (resource-group gear); the entries below only add
    // presentation hints (visibility, labels, relations) the spec can't carry.
    schema: "GroupDto",
    // Full CRUD derives from the groups collection (update is PUT per spec).
    basePath: `/resource-group/v1/groups`,
    fields: [
      { name: "id", inList: true },
      { name: "name", inList: true, inForm: true },
      { name: "type", label: "Type", inList: true, createOnly: true },
      { name: "parent_id", label: "Parent", relation: "resource-groups", inForm: true },
      { name: "metadata", inForm: true },
    ],
  },

  {
    key: "types",
    label: "Types / GTS",
    owningGear: "types-registry",
    tenantScope: "global",
    // Register/list/get only — no update/delete on the API yet (read-only).
    safety: "read-only",
    // Entities are addressed by their GTS id, not the internal uuid: the
    // get-one endpoint treats the path param as a gts_id (passing the uuid
    // 404s as "no entity with GTS ID").
    idField: "gts_id",
    capabilities: { read: "types:read" },
    // `id`/`segments`/`content`/`description` derive from the GtsEntityDto
    // schema; only the two list-visible, relabelled fields stay curated.
    schema: "GtsEntityDto",
    // The API exposes register (POST) + list/get, but v0 policy is read-only,
    // so the `read-only` safety above suppresses the write verbs; list/get
    // derive. Item id is a gts_id (URL-encoded by the resolver).
    basePath: `/types-registry/v1/entities`,
    fields: [
      { name: "gts_id", label: "GTS id", inList: true, readOnly: true },
      { name: "is_schema", label: "Schema?", inList: true },
    ],
  },

  {
    key: "gears",
    label: "Gears",
    owningGear: "gear-orchestrator",
    tenantScope: "global",
    safety: "read-only",
    idField: "name",
    capabilities: { read: "gears:read" },
    // List-only (the orchestrator exposes no item/CRUD ops). No component
    // schema — the list element is an unnamed array type — so fields stay
    // curated.
    basePath: `/gear-orchestrator/v1/gears`,
    fields: [
      { name: "name", inList: true, readOnly: true },
      { name: "capabilities", type: "json", inList: true },
      { name: "dependencies", type: "json" },
      { name: "deployment_mode", label: "Mode", inList: true },
    ],
  },

  {
    // Egress gateway upstreams — read-only in v0. Upstream/route bodies are
    // deeply nested (endpoints, auth, headers, plugins, rate limits, CORS);
    // editing them via the admin UI is deferred to a dedicated config editor.
    key: "upstreams",
    label: "Egress upstreams",
    owningGear: "oagw",
    tenantScope: "tenant",
    safety: "read-only",
    // Nested fields (server/auth/headers/plugins/cors/…) derive as JSON from
    // the UpstreamResponse schema; only list-visible columns stay curated.
    schema: "UpstreamResponse",
    // The API exposes full CRUD, but v0 is read-only (the `read-only` safety
    // suppresses writes); list/get derive.
    basePath: `/oagw/v1/upstreams`,
    fields: [
      { name: "id", inList: true, readOnly: true },
      { name: "alias", inList: true },
      { name: "protocol", type: "tag", inList: true },
      { name: "enabled", inList: true },
      { name: "rate_limit", label: "Rate limit" },
    ],
  },

  {
    key: "routes",
    label: "Egress routes",
    owningGear: "oagw",
    tenantScope: "tenant",
    safety: "read-only",
    // Nested fields (match/plugins/cors/…) derive as JSON from the RouteResponse
    // schema; only list-visible columns stay curated.
    schema: "RouteResponse",
    // Full CRUD in the API; v0 read-only (writes suppressed by `read-only`
    // safety); list/get derive.
    basePath: `/oagw/v1/routes`,
    fields: [
      { name: "id", inList: true, readOnly: true },
      { name: "upstream_id", label: "Upstream", inList: true },
      { name: "priority", inList: true },
      { name: "enabled", inList: true },
      { name: "rate_limit", label: "Rate limit" },
    ],
  },
];

/** Sidebar icon per resource key (presentation only). */
const ICONS: Record<string, () => ReactNode> = {
  tenants: () => createElement(TeamOutlined),
  "tenant-metadata": () => createElement(ProfileOutlined),
  users: () => createElement(UserOutlined),
  conversions: () => createElement(SwapOutlined),
  "resource-groups": () => createElement(ClusterOutlined),
  types: () => createElement(TagsOutlined),
  gears: () => createElement(ApiOutlined),
  upstreams: () => createElement(CloudServerOutlined),
  routes: () => createElement(NodeIndexOutlined),
};

export function resourceIcon(key: string): ReactNode {
  return (ICONS[key] ?? (() => createElement(AppstoreOutlined)))();
}
