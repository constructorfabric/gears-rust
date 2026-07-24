// Interpret the declarative admin resource config (admin.config.json) into the
// runtime ResourceDescriptor model.
//
// ADR-0003 concern-split: per-resource registration is DATA (JSON), not code.
// This module is the generic interpreter that turns that data into the
// descriptors the app consumes — building the few behavioral bits (path
// templates, form defaults, action wiring, option loaders, visibility
// predicates) declaratively. It holds no per-resource knowledge, so another
// gears-rust project registers resources by shipping its own JSON with no
// TypeScript changes.

import { apiFetch } from "../httpClient";
import type {
  ActionDef,
  FieldDef,
  FieldOption,
  ResourceDescriptor,
  SafetyLevel,
  TenantScope,
  Verb,
} from "./types";

// --- Generic, reusable option loaders (named infra, not per-resource code) ---

const TENANT_TYPE_PREFIX = "gts.cf.core.am.tenant_type.v1~";

/** Load registered tenant types for a create-form select (skips the base). */
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

/** Named option loaders referenced by `field.optionsSource` in the config. */
const OPTION_LOADERS: Record<string, () => Promise<FieldOption[]>> = {
  "tenant-types": loadTenantTypes,
};

// --- JSON config shape ---

interface FieldConfig {
  name: string;
  label?: string;
  type?: string;
  inList?: boolean;
  inDetail?: boolean;
  inForm?: boolean;
  createOnly?: boolean;
  required?: boolean;
  readOnly?: boolean;
  relation?: string;
  /** Name of a generic option loader in OPTION_LOADERS. */
  optionsSource?: string;
}

interface ActionConfig {
  name: string;
  label: string;
  method?: "POST" | "PATCH" | "PUT" | "DELETE";
  /** Path template with `{tenant}` / `{id}` placeholders. */
  pathTemplate: string;
  /** Static request body (e.g. a status transition). */
  body?: unknown;
  capability?: string;
  safety?: SafetyLevel;
  /** Show the action only when the record field equals this value. */
  visibleWhen?: { field: string; equals: unknown };
}

interface ResourceConfig {
  key: string;
  label: string;
  owningGear: string;
  tenantScope: string;
  safety: string;
  schema?: string;
  basePath: string;
  /** Template for an irregular list path (`{tenant}` placeholder). */
  listPathTemplate?: string;
  suppressVerbs?: string[];
  idField?: string;
  createKeyField?: string;
  bodyField?: string;
  capabilities?: { read?: string; write?: string; delete?: string };
  /** Create-form defaults; string values may embed `{tenant}`. */
  formDefaults?: Record<string, unknown>;
  fields: FieldConfig[];
  actions?: ActionConfig[];
}

export interface AdminConfig {
  resources: ResourceConfig[];
}

// --- Template + predicate helpers ---

/** Substitute `{tenant}` (home tenant) and `{id}` (record id) in a template. */
function fill(tpl: string, tenant: string, id?: string): string {
  return tpl
    .replace(/\{tenant\}/g, encodeURIComponent(tenant))
    .replace(/\{id\}/g, id !== undefined ? encodeURIComponent(id) : "");
}

function toField(f: FieldConfig): FieldDef {
  const { optionsSource, type, ...rest } = f;
  const field: FieldDef = { ...rest, type: type as FieldDef["type"] };
  if (optionsSource) {
    const loader = OPTION_LOADERS[optionsSource];
    if (!loader) {
      throw new Error(
        `Unknown optionsSource "${optionsSource}" for field "${f.name}"`,
      );
    }
    field.options = loader;
  }
  return field;
}

function toAction(a: ActionConfig): ActionDef {
  const when = a.visibleWhen;
  return {
    name: a.name,
    label: a.label,
    method: a.method,
    path: (ctx, id) => fill(a.pathTemplate, ctx.subject_tenant_id, id),
    body: a.body !== undefined ? () => a.body : undefined,
    capability: a.capability,
    safety: a.safety,
    visible: when ? (r) => r[when.field] === when.equals : undefined,
  };
}

/** Interpret the JSON config into runtime resource descriptors. */
export function buildDescriptors(config: AdminConfig): ResourceDescriptor[] {
  return config.resources.map((r) => ({
    key: r.key,
    label: r.label,
    owningGear: r.owningGear,
    tenantScope: r.tenantScope as TenantScope,
    safety: r.safety as SafetyLevel,
    schema: r.schema,
    basePath: r.basePath,
    listPath: r.listPathTemplate
      ? (ctx) => fill(r.listPathTemplate!, ctx.subject_tenant_id)
      : undefined,
    suppressVerbs: r.suppressVerbs as Verb[] | undefined,
    idField: r.idField,
    createKeyField: r.createKeyField,
    bodyField: r.bodyField,
    capabilities: r.capabilities,
    formDefaults: r.formDefaults
      ? (ctx) => {
          const out: Record<string, unknown> = {};
          for (const [k, v] of Object.entries(r.formDefaults!)) {
            out[k] =
              typeof v === "string"
                ? v.replace(/\{tenant\}/g, ctx.subject_tenant_id)
                : v;
          }
          return out;
        }
      : undefined,
    fields: r.fields.map(toField),
    actions: r.actions?.map(toAction),
  }));
}
