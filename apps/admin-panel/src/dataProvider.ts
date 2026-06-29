import type { DataProvider } from "@refinedev/core";

import { API_PREFIX } from "./config";
import { apiFetch } from "./httpClient";
import { cachedAdminContext, type AdminContext } from "./adminContext";
import {
  getDescriptor,
  idField,
  type ResourceDescriptor,
} from "./resources";

// Descriptor-driven Gears data provider. CRUD verbs are resolved from the
// resource registry: the data provider holds no per-resource knowledge beyond
// what the descriptors declare, so new resources need no provider changes.

function requireContext(): AdminContext {
  const ctx = cachedAdminContext();
  if (!ctx) {
    throw new Error("No admin context — sign in first");
  }
  return ctx;
}

// Gears list endpoints return either a bare array (gears, entities) or a
// cursor `Page<T>` envelope `{ items, page_info }`. Normalize both, and
// guarantee every record carries an `id` (Refine's row key) by falling back
// to the descriptor's identity field.
function normalizeList(
  payload: unknown,
  d: ResourceDescriptor,
): Record<string, unknown>[] {
  // Cursor pages use `items`; types-registry wraps its rows in `entities`.
  const env = payload as { items?: unknown[]; entities?: unknown[] };
  const items: unknown[] = Array.isArray(payload)
    ? payload
    : (env?.items ?? env?.entities ?? []);
  return items.map((it, idx) => normalizeRecord(it, d, idx));
}

function normalizeRecord(
  it: unknown,
  d: ResourceDescriptor,
  idx = 0,
): Record<string, unknown> {
  const rec = { ...((it ?? {}) as Record<string, unknown>) };
  if (rec.id === undefined) {
    rec.id = rec[idField(d)] ?? rec.gts_id ?? rec.name ?? String(idx);
  }
  return rec;
}

function unsupported(verb: string, resource: string): never {
  throw new Error(`${verb} is not supported for "${resource}"`);
}

// The write payload: the whole form object, or — for transparent-body
// resources — just the declared body field's value.
function requestBody(
  d: ResourceDescriptor,
  vars: Record<string, unknown>,
): unknown {
  return d.bodyField ? vars[d.bodyField] : vars;
}

export const dataProvider: DataProvider = {
  getApiUrl: () => API_PREFIX,

  getList: async ({ resource }) => {
    const d = getDescriptor(resource);
    const payload = await apiFetch<unknown>(d.paths.list(requireContext()));
    const data = normalizeList(payload, d);
    return { data: data as never, total: data.length };
  },

  getOne: async ({ resource, id }) => {
    const d = getDescriptor(resource);
    if (!d.paths.one) unsupported("getOne", resource);
    const raw = await apiFetch<Record<string, unknown>>(
      d.paths.one(requireContext(), String(id)),
    );
    return { data: normalizeRecord(raw, d) as never };
  },

  create: async ({ resource, variables }) => {
    const d = getDescriptor(resource);
    const vars = (variables ?? {}) as Record<string, unknown>;
    const ctx = requireContext();
    // Key-addressed upsert: create is a PUT to the entry path, id from a field.
    if (d.createKeyField && d.paths.update) {
      const id = String(vars[d.createKeyField]);
      const raw = await apiFetch<Record<string, unknown>>(d.paths.update(ctx, id), {
        method: d.updateMethod ?? "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(requestBody(d, vars)),
      });
      return { data: normalizeRecord(raw, d) as never };
    }
    if (!d.paths.create) unsupported("create", resource);
    const raw = await apiFetch<Record<string, unknown>>(d.paths.create(ctx), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(requestBody(d, vars)),
    });
    return { data: normalizeRecord(raw, d) as never };
  },

  update: async ({ resource, id, variables }) => {
    const d = getDescriptor(resource);
    if (!d.paths.update) unsupported("update", resource);
    const vars = (variables ?? {}) as Record<string, unknown>;
    const raw = await apiFetch<Record<string, unknown>>(
      d.paths.update(requireContext(), String(id)),
      {
        method: d.updateMethod ?? "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(requestBody(d, vars)),
      },
    );
    return { data: normalizeRecord(raw, d) as never };
  },

  deleteOne: async ({ resource, id }) => {
    const d = getDescriptor(resource);
    if (!d.paths.remove) unsupported("deleteOne", resource);
    await apiFetch<unknown>(d.paths.remove(requireContext(), String(id)), {
      method: "DELETE",
    });
    return { data: { id } as never };
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
