import type { DataProvider } from "@refinedev/core";

import { API_PREFIX } from "./config";
import { apiFetch } from "./httpClient";
import { cachedAdminContext } from "./adminContext";

// v0 resource map. OpenAPI-first discovery + a curated registry replaces this
// hardcoding in a later increment (see ADR-0003). Each resource declares how
// to build its list/detail paths from the admin context.
type PathFn = (id?: string) => string;

interface ResourceDef {
  list: PathFn;
  one?: PathFn;
}

const RESOURCES: Record<string, ResourceDef> = {
  tenants: {
    list: () => {
      const ctx = cachedAdminContext();
      const root = ctx?.subject_tenant_id ?? "";
      return `/account-management/v1/tenants/${root}/children`;
    },
    one: (id) => `/account-management/v1/tenants/${id}`,
  },
  "resource-groups": {
    list: () => "/resource-group/v1/groups",
    one: (id) => `/resource-group/v1/groups/${id}`,
  },
  types: {
    list: () => "/types-registry/v1/entities",
  },
  gears: {
    list: () => "/gear-orchestrator/v1/gears",
  },
};

// Gears list endpoints return either a bare array (gears, entities) or a
// cursor `Page<T>` envelope `{ items, page_info }`. Normalize both, and
// guarantee every record has an `id` for Refine's row key.
function normalizeList(payload: unknown): Record<string, unknown>[] {
  const items: unknown[] = Array.isArray(payload)
    ? payload
    : ((payload as { items?: unknown[] })?.items ?? []);
  return items.map((it, idx) => {
    const rec = (it ?? {}) as Record<string, unknown>;
    if (rec.id === undefined) {
      rec.id = rec.gts_id ?? rec.name ?? String(idx);
    }
    return rec;
  });
}

function resourceDef(resource: string): ResourceDef {
  const def = RESOURCES[resource];
  if (!def) {
    throw new Error(`Unknown admin resource: ${resource}`);
  }
  return def;
}

export const dataProvider: DataProvider = {
  getApiUrl: () => API_PREFIX,

  getList: async ({ resource }) => {
    const def = resourceDef(resource);
    const payload = await apiFetch<unknown>(def.list());
    const data = normalizeList(payload);
    return { data: data as never, total: data.length };
  },

  getOne: async ({ resource, id }) => {
    const def = resourceDef(resource);
    if (!def.one) {
      throw new Error(`Resource ${resource} has no detail endpoint`);
    }
    const data = await apiFetch<Record<string, unknown>>(def.one(String(id)));
    return { data: data as never };
  },

  create: async () => {
    throw new Error("create is not implemented in the v0 admin data provider");
  },

  update: async () => {
    throw new Error("update is not implemented in the v0 admin data provider");
  },

  deleteOne: async () => {
    throw new Error(
      "deleteOne is not implemented in the v0 admin data provider",
    );
  },

  custom: async ({ url, method, payload }) => {
    const data = await apiFetch<unknown>(url, {
      method: (method ?? "get").toUpperCase(),
      ...(payload
        ? {
            body: JSON.stringify(payload),
            headers: { "Content-Type": "application/json" },
          }
        : {}),
    });
    return { data: data as never };
  },
};
