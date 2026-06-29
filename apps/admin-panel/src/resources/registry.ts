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

import type { ResourceDescriptor } from "./types";

const AM = "/account-management/v1";

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
    updateMethod: "PATCH",
    // Pre-fill a valid create form: parent = home tenant, and a registered
    // tenant-type GTS id (the `tenant_type` is a GTS chain, not a free word —
    // the API rejects anything not starting with `gts.`).
    formDefaults: (ctx) => ({
      parent_id: ctx.subject_tenant_id,
      tenant_type: "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~",
      self_managed: false,
    }),
    paths: {
      // Hierarchy view: children of the caller's home tenant. There is no
      // global tenant list endpoint; tenant isolation is enforced server-side.
      list: (ctx) => `${AM}/tenants/${ctx.subject_tenant_id}/children`,
      one: (_ctx, id) => `${AM}/tenants/${id}`,
      create: () => `${AM}/tenants`,
      update: (_ctx, id) => `${AM}/tenants/${id}`,
      remove: (_ctx, id) => `${AM}/tenants/${id}`,
    },
    fields: [
      { name: "id", type: "uuid", inList: true, readOnly: true },
      { name: "name", inList: true, inForm: true, required: true },
      { name: "status", type: "tag", inList: true },
      { name: "tenant_type", label: "Type", inList: true, createOnly: true, required: true },
      { name: "parent_id", label: "Parent", type: "uuid", relation: "tenants", createOnly: true },
      { name: "self_managed", type: "boolean", createOnly: true },
      { name: "depth", type: "number" },
      { name: "child_count", label: "Children", type: "number", inList: true },
      { name: "created_at", type: "datetime" },
      { name: "updated_at", type: "datetime" },
      { name: "deleted_at", type: "datetime" },
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
    paths: {
      // Own conversion requests for the caller's home tenant.
      list: (ctx) => `${AM}/tenants/${ctx.subject_tenant_id}/conversions`,
      one: (ctx, id) => `${AM}/tenants/${ctx.subject_tenant_id}/conversions/${id}`,
    },
    fields: [
      { name: "id", type: "uuid", inList: true, readOnly: true },
      { name: "status", type: "tag", inList: true },
      { name: "target_mode", label: "Target mode", inList: true },
      { name: "side", inList: true },
      { name: "comment" },
      { name: "created_at", type: "datetime" },
      { name: "updated_at", type: "datetime" },
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
    paths: {
      // IdP-backed users for the caller's home tenant. The IdP plugin
      // exposes list/create/delete only (no get-one / update), so the
      // descriptor advertises just those verbs and the UI degrades
      // gracefully. Shown as unavailable when no IdP supports the op.
      list: (ctx) => `${AM}/tenants/${ctx.subject_tenant_id}/users`,
      create: (ctx) => `${AM}/tenants/${ctx.subject_tenant_id}/users`,
      remove: (ctx, id) => `${AM}/tenants/${ctx.subject_tenant_id}/users/${id}`,
    },
    fields: [
      { name: "id", type: "uuid", inList: true, readOnly: true },
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
    paths: {
      list: (ctx) => `${AM}/tenants/${ctx.subject_tenant_id}/metadata`,
      one: (ctx, id) => `${AM}/tenants/${ctx.subject_tenant_id}/metadata/${id}`,
      update: (ctx, id) => `${AM}/tenants/${ctx.subject_tenant_id}/metadata/${id}`,
      remove: (ctx, id) => `${AM}/tenants/${ctx.subject_tenant_id}/metadata/${id}`,
    },
    fields: [
      { name: "type_id", label: "Type id", inList: true, createOnly: true, required: true },
      { name: "value", type: "json", inList: true, inForm: true, required: true },
      { name: "updated_at", label: "Updated", type: "datetime", inList: true },
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
    updateMethod: "PUT",
    paths: {
      list: () => `/resource-group/v1/groups`,
      one: (_ctx, id) => `/resource-group/v1/groups/${id}`,
      create: () => `/resource-group/v1/groups`,
      update: (_ctx, id) => `/resource-group/v1/groups/${id}`,
      remove: (_ctx, id) => `/resource-group/v1/groups/${id}`,
    },
    fields: [
      { name: "id", type: "uuid", inList: true, readOnly: true },
      { name: "name", inList: true, inForm: true, required: true },
      { name: "type_path", label: "Type", inList: true, createOnly: true, required: true },
      { name: "parent_id", label: "Parent", type: "uuid", relation: "resource-groups", inForm: true },
      { name: "metadata", type: "json", inForm: true },
    ],
  },

  {
    key: "types",
    label: "Types / GTS",
    owningGear: "types-registry",
    tenantScope: "global",
    // Register/list/get only — no update/delete on the API yet (read-only).
    safety: "read-only",
    capabilities: { read: "types:read" },
    paths: {
      list: () => `/types-registry/v1/entities`,
      one: (_ctx, id) => `/types-registry/v1/entities/${id}`,
    },
    fields: [
      { name: "gts_id", label: "GTS id", inList: true, readOnly: true },
      { name: "is_schema", label: "Schema?", type: "boolean", inList: true },
      { name: "id", type: "uuid" },
      { name: "segments", type: "json" },
      { name: "content", type: "json" },
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
    paths: {
      list: () => `/gear-orchestrator/v1/gears`,
    },
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
    paths: {
      list: () => `/oagw/v1/upstreams`,
      one: (_ctx, id) => `/oagw/v1/upstreams/${id}`,
    },
    fields: [
      { name: "id", inList: true, readOnly: true },
      { name: "alias", inList: true },
      { name: "protocol", type: "tag", inList: true },
      { name: "enabled", type: "boolean", inList: true },
      { name: "server", type: "json" },
      { name: "tags", type: "json" },
      { name: "auth", type: "json" },
      { name: "headers", type: "json" },
      { name: "plugins", type: "json" },
      { name: "rate_limit", label: "Rate limit", type: "json" },
      { name: "cors", type: "json" },
    ],
  },

  {
    key: "routes",
    label: "Egress routes",
    owningGear: "oagw",
    tenantScope: "tenant",
    safety: "read-only",
    paths: {
      list: () => `/oagw/v1/routes`,
      one: (_ctx, id) => `/oagw/v1/routes/${id}`,
    },
    fields: [
      { name: "id", inList: true, readOnly: true },
      { name: "upstream_id", label: "Upstream", inList: true },
      { name: "priority", type: "number", inList: true },
      { name: "enabled", type: "boolean", inList: true },
      { name: "match", type: "json" },
      { name: "tags", type: "json" },
      { name: "plugins", type: "json" },
      { name: "rate_limit", label: "Rate limit", type: "json" },
      { name: "cors", type: "json" },
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
