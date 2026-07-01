// Runtime OpenAPI-driven field discovery.
//
// ADR-0003 (revised 2026-06-30): discovery moves from a fully hand-curated
// registry toward reading the gateway-aggregated `/openapi.json` at runtime.
// This module derives a resource's field set (name / type / required /
// read-only) from its OpenAPI component schema, so descriptors no longer
// duplicate the API's own type truth. What OpenAPI cannot express
// (per-view visibility, labels, relations, custom-action wiring, safety,
// tenant scope) stays curated in the descriptor and overrides the derived
// defaults. The transport for that curated overlay (config / `x-cf-admin-*`
// extensions / descriptor endpoint) is still pending; this engine is
// independent of that choice.

import { apiFetch } from "../httpClient";
import { RESOURCE_REGISTRY } from "./registry";
import type { FieldDef, FieldType, ResourceDescriptor } from "./types";

/** Minimal slice of an OpenAPI 3.1 document — only what discovery reads. */
interface OpenApiSpec {
  components?: { schemas?: Record<string, JsonSchema> };
}

/** Minimal JSON Schema node (utoipa output). */
interface JsonSchema {
  // utoipa emits either a single type or `["string", "null"]` for nullable.
  type?: string | string[];
  format?: string;
  readOnly?: boolean;
  required?: string[];
  properties?: Record<string, JsonSchema>;
  allOf?: JsonSchema[];
  $ref?: string;
}

/** Pick the non-null member of a possibly-nullable `type`. */
function baseType(t: JsonSchema["type"]): string | undefined {
  if (Array.isArray(t)) return t.find((x) => x !== "null");
  return t;
}

/** Map a JSON Schema property to the admin field render/parse hint. */
function fieldType(prop: JsonSchema): FieldType {
  if (prop.$ref) return "json"; // nested entity / relation — rendered as JSON
  if (prop.format === "uuid") return "uuid";
  if (prop.format === "date-time") return "datetime";
  switch (baseType(prop.type)) {
    case "integer":
    case "number":
      return "number";
    case "boolean":
      return "boolean";
    case "string":
      return "string";
    case "object":
    case "array":
      return "json";
    default:
      return "json";
  }
}

/**
 * Derive field descriptors from a component schema's properties. Only the
 * facts OpenAPI states authoritatively are emitted: name, type, `required`,
 * and `readOnly`. Presentation (visibility, labels) is left to the curated
 * overlay.
 */
export function schemaToFields(schema: JsonSchema): FieldDef[] {
  // Flatten a top-level allOf (utoipa uses it to compose/extend schemas).
  const merged: JsonSchema = schema.allOf
    ? schema.allOf.reduce<JsonSchema>(
        (acc, part) => ({
          properties: { ...acc.properties, ...part.properties },
          required: [...(acc.required ?? []), ...(part.required ?? [])],
        }),
        { properties: { ...schema.properties }, required: [...(schema.required ?? [])] },
      )
    : schema;

  const required = new Set(merged.required ?? []);
  return Object.entries(merged.properties ?? {}).map(([name, prop]) => ({
    name,
    type: fieldType(prop),
    required: required.has(name),
    readOnly: prop.readOnly === true,
  }));
}

/** Resolve a named component schema's fields, or `[]` if absent. */
export function deriveFields(spec: OpenApiSpec, schemaName: string): FieldDef[] {
  const schema = spec.components?.schemas?.[schemaName];
  return schema ? schemaToFields(schema) : [];
}

/**
 * Merge OpenAPI-derived fields with the curated overlay. The curated entry
 * wins on every property it sets (label, type override, visibility, relation,
 * options, …); derived facts (type / required / readOnly) fill the gaps.
 * Derived-only fields are appended so the descriptor need not list them.
 * Curated field order is preserved; appended fields keep schema order.
 */
export function mergeFields(curated: FieldDef[], derived: FieldDef[]): FieldDef[] {
  const byName = new Map(derived.map((f) => [f.name, f]));
  const out: FieldDef[] = curated.map((c) => {
    const d = byName.get(c.name);
    byName.delete(c.name);
    return d ? { ...d, ...c } : c;
  });
  for (const d of byName.values()) out.push(d);
  return out;
}

// The aggregated spec is fetched once and cached for the session.
let specPromise: Promise<OpenApiSpec> | null = null;

/** Fetch and cache the gateway-aggregated OpenAPI document. */
export function loadOpenApiSpec(): Promise<OpenApiSpec> {
  if (!specPromise) {
    specPromise = apiFetch<OpenApiSpec>("/openapi.json").catch((err) => {
      specPromise = null; // allow a later retry
      throw err;
    });
  }
  return specPromise;
}

// Enrich each descriptor's `fields` in place, exactly once.
let resolved = false;

/**
 * Fill descriptor fields from the OpenAPI spec for resources that declare a
 * `schema`. Runs once per session, mutating the in-memory registry before
 * any screen renders (called from the auth gate). Idempotent and best-effort:
 * a spec fetch failure leaves the curated fields untouched so the panel still
 * works against a backend that does not serve `/openapi.json`.
 */
export async function ensureSchemasResolved(): Promise<void> {
  if (resolved) return;
  const withSchema = RESOURCE_REGISTRY.filter((d) => d.schema);
  if (withSchema.length === 0) {
    resolved = true;
    return;
  }
  let spec: OpenApiSpec;
  try {
    spec = await loadOpenApiSpec();
  } catch {
    return; // keep curated fields; retry on the next auth check
  }
  for (const d of withSchema as ResourceDescriptor[]) {
    const derived = deriveFields(spec, d.schema!);
    if (derived.length > 0) d.fields = mergeFields(d.fields, derived);
  }
  resolved = true;
}
