// Runtime OpenAPI operation/path discovery.
//
// ADR-0003 (revised 2026-07-02): resource routes are API-intrinsic, so they
// are derived from the gateway-aggregated `/openapi.json` rather than
// hand-written per resource. Given a resource's `basePath` (its collection
// path), this module finds the standard CRUD operations the spec actually
// exposes and returns path *templates*; a resolver fills the tenant and
// record-id parameters at call time. Presentation and policy (irregular list
// paths, verb suppression, custom actions, labels) stay in the descriptor —
// see `resources/types.ts` and `registry.ts`.

import type { AdminContext } from "../adminContext";
import type { ResourcePaths, Verb } from "./types";

/** Minimal slice of an OpenAPI document — only the path map is read here. */
export interface OpenApiPaths {
  paths?: Record<string, Record<string, unknown>>;
}

/** A custom (non-CRUD) action discovered as a POST-only leaf under the item. */
export interface DerivedAction {
  name: string;
  method: "POST";
  template: string;
}

/** Operations derived for one resource from the spec. */
export interface DerivedOps {
  /** Raw OpenAPI path template per CRUD verb (absent verb => unsupported). */
  templates: Partial<Record<Verb, string>>;
  /** HTTP verb the spec uses for the item update (PUT preferred over PATCH). */
  updateMethod?: "PUT" | "PATCH";
  /** POST-only leaf operations under the item path (e.g. suspend/unsuspend). */
  actions: DerivedAction[];
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Derive the standard CRUD operations for a resource from the aggregated spec.
 * `basePath` is the collection path (e.g. `/resource-group/v1/groups` or
 * `/account-management/v1/tenants/{tenant_id}/conversions`). The item path is
 * `basePath/{param}`; list/create sit on the collection, one/update/remove on
 * the item. Only operations the spec declares are returned.
 */
export function deriveOps(spec: OpenApiPaths, basePath: string): DerivedOps {
  const paths = spec.paths ?? {};
  const has = (k: string, m: string): boolean =>
    Boolean(paths[k]) && m in paths[k];
  const keys = Object.keys(paths).filter(
    (k) => k === basePath || k.startsWith(`${basePath}/`),
  );

  // Item path = collection + a single path parameter segment.
  const itemRe = new RegExp(`^${escapeRe(basePath)}/\\{[^/]+\\}$`);
  const item = keys.find((k) => itemRe.test(k));

  const templates: Partial<Record<Verb, string>> = {};
  if (has(basePath, "get")) templates.list = basePath;
  if (has(basePath, "post")) templates.create = basePath;

  let updateMethod: "PUT" | "PATCH" | undefined;
  if (item) {
    if (has(item, "get")) templates.one = item;
    if (has(item, "put")) {
      templates.update = item;
      updateMethod = "PUT";
    } else if (has(item, "patch")) {
      templates.update = item;
      updateMethod = "PATCH";
    }
    if (has(item, "delete")) templates.remove = item;
  }

  // Actions are POST-only leaves under the item path. A leaf that also has a
  // GET is a sub-collection (e.g. `/tenants/{id}/users`), not an action.
  const actions: DerivedAction[] = [];
  if (item) {
    const actRe = new RegExp(`^${escapeRe(item)}/([a-z0-9-]+)$`);
    for (const k of keys) {
      const m = actRe.exec(k);
      if (m && has(k, "post") && !has(k, "get")) {
        actions.push({ name: m[1], method: "POST", template: k });
      }
    }
  }

  return { templates, updateMethod, actions };
}

/** Path parameter names in a template, in order. */
function paramsOf(template: string): string[] {
  return [...template.matchAll(/\{([^}]+)\}/g)].map((m) => m[1]);
}

/**
 * Fill a path template's parameters. The **last** parameter is the record id
 * (only for item operations, where `id` is passed); any earlier tenant-named
 * parameter is the caller's home tenant. This resolves both `/tenants/{id}`
 * (the sole param is the record id) and
 * `/tenants/{tenant_id}/conversions/{request_id}` (tenant from context, the
 * trailing id from the row).
 */
export function resolvePath(
  template: string,
  ctx: AdminContext,
  id?: string,
): string {
  const params = paramsOf(template);
  const last = params[params.length - 1];
  return template.replace(/\{([^}]+)\}/g, (_full, name: string) => {
    if (id !== undefined && name === last) return encodeURIComponent(id);
    if (/tenant/i.test(name)) return encodeURIComponent(ctx.subject_tenant_id);
    // A non-tenant, non-id param should not occur for our resources; fall back
    // to the id so the URL is at least well-formed.
    return id !== undefined ? encodeURIComponent(id) : "";
  });
}

/**
 * Build the descriptor's runtime `paths` from derived templates, applying an
 * optional irregular-list override and a verb-suppression set (policy that
 * intentionally offers less than the API exposes, e.g. read-only resources).
 */
export function buildPaths(
  ops: DerivedOps,
  opts: {
    listPath?: (ctx: AdminContext) => string;
    suppress?: readonly Verb[];
  } = {},
): ResourcePaths {
  const suppressed = new Set(opts.suppress ?? []);
  const t = ops.templates;
  const enabled = (v: Verb): boolean => !suppressed.has(v) && Boolean(t[v]);

  return {
    list: opts.listPath
      ? opts.listPath
      : (ctx) => resolvePath(t.list ?? "", ctx),
    one: enabled("one") ? (ctx, id) => resolvePath(t.one!, ctx, id) : undefined,
    create: enabled("create") ? (ctx) => resolvePath(t.create!, ctx) : undefined,
    update: enabled("update")
      ? (ctx, id) => resolvePath(t.update!, ctx, id)
      : undefined,
    remove: enabled("remove")
      ? (ctx, id) => resolvePath(t.remove!, ctx, id)
      : undefined,
  };
}
