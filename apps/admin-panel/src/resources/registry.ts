import {
  TeamOutlined,
  ClusterOutlined,
  ApiOutlined,
  AppstoreOutlined,
  SwapOutlined,
  TagsOutlined,
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
    formDefaults: (ctx) => ({ parent_id: ctx.subject_tenant_id, self_managed: false }),
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
];

/** Sidebar icon per resource key (presentation only). */
const ICONS: Record<string, () => ReactNode> = {
  tenants: () => createElement(TeamOutlined),
  conversions: () => createElement(SwapOutlined),
  "resource-groups": () => createElement(ClusterOutlined),
  types: () => createElement(TagsOutlined),
  gears: () => createElement(ApiOutlined),
};

export function resourceIcon(key: string): ReactNode {
  return (ICONS[key] ?? (() => createElement(AppstoreOutlined)))();
}
