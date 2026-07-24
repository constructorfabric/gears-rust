import type { AdminContext } from "../adminContext";

// Admin resource metadata model (DESIGN: admin resource descriptors).
//
// Each manageable object is described once, declaratively, and the data
// provider + generated screens are driven entirely off these descriptors.
// New objects/fields are added by registering a descriptor here — the core
// admin app does not change. This is the additive registry the issue calls
// for; a later increment augments it from OpenAPI and gear-contributed
// descriptors without changing this shape.

/** Per-issue safety classification for a resource or action. */
export type SafetyLevel = "normal" | "destructive" | "operator-only" | "read-only";

/** Field render/parse hint. Defaults to "string". */
export type FieldType =
  | "string"
  | "number"
  | "boolean"
  | "datetime"
  | "uuid"
  | "json"
  | "tag";

/** How the resource is scoped against the caller's tenant. */
export type TenantScope = "platform-only" | "tenant" | "global";

/** One field of a resource, with per-view visibility. */
export interface FieldDef {
  name: string;
  label?: string;
  /** Render/parse hint. Default "string". */
  type?: FieldType;
  /** Show as a column in the list table. Default false. */
  inList?: boolean;
  /** Show in the detail panel. Default true. */
  inDetail?: boolean;
  /** Editable in both create and edit forms. Default false. */
  inForm?: boolean;
  /** Settable at create time only (immutable afterwards). Implies form-create. */
  createOnly?: boolean;
  /** Required in forms. Default false. */
  required?: boolean;
  /** Shown in forms but disabled (e.g. immutable-after-create). */
  readOnly?: boolean;
  /** Link target resource key for relation fields. */
  relation?: string;
  /**
   * Render this form field as a searchable select whose options are loaded
   * lazily from the backend (e.g. tenant types from the types registry).
   */
  options?: () => Promise<FieldOption[]>;
}

/** One selectable option for an `options`-backed field. */
export interface FieldOption {
  value: string;
  label: string;
}

/** A custom (non-CRUD) action, e.g. suspend / approve / cancel. */
export interface ActionDef {
  name: string;
  label: string;
  /** HTTP method. Default "POST". */
  method?: "POST" | "PATCH" | "PUT" | "DELETE";
  /** Builds the action path (relative to the API prefix). */
  path: (ctx: AdminContext, id: string) => string;
  /** Builds the request body from the target record. */
  body?: (record: Record<string, unknown>) => unknown;
  /** Capability hint gating UI visibility. */
  capability?: string;
  /** Safety level — drives confirmation + styling. Default "normal". */
  safety?: SafetyLevel;
  /** Only show the action when this predicate holds for the record. */
  visible?: (record: Record<string, unknown>) => boolean;
}

/** The standard CRUD verbs, keyed as in `ResourcePaths`. */
export type Verb = "list" | "one" | "create" | "update" | "remove";

/** Path builders for the standard CRUD verbs. Absent verb => unsupported. */
export interface ResourcePaths {
  list: (ctx: AdminContext) => string;
  one?: (ctx: AdminContext, id: string) => string;
  create?: (ctx: AdminContext) => string;
  update?: (ctx: AdminContext, id: string) => string;
  remove?: (ctx: AdminContext, id: string) => string;
}

/** Capability hints for UI gating; backend stays the final authority. */
export interface ResourceCapabilities {
  read?: string;
  write?: string;
  delete?: string;
}

/** Complete declarative description of one admin resource. */
export interface ResourceDescriptor {
  key: string;
  label: string;
  owningGear: string;
  tenantScope: TenantScope;
  safety: SafetyLevel;
  /** Record identity field. Default "id". */
  idField?: string;
  /** HTTP verb for update. Default "PATCH". */
  updateMethod?: "PATCH" | "PUT";
  capabilities?: ResourceCapabilities;
  /**
   * OpenAPI component schema name (e.g. "Group") for this resource's record.
   * When set, the resource's fields are enriched at boot from the
   * gateway-aggregated `/openapi.json`: derived type / `required` / `readOnly`
   * fill the gaps, and the curated `fields` below override presentation
   * (visibility, labels, relations). See `resources/openapi.ts`.
   */
  schema?: string;
  /**
   * The resource's collection path in the aggregated OpenAPI spec, e.g.
   * `/resource-group/v1/groups` or, for a tenant-scoped resource,
   * `/account-management/v1/tenants/{tenant_id}/conversions`. The standard
   * CRUD `paths` are derived from this at boot (see `resources/openapiOps.ts`);
   * the API is the single source of truth for which verbs exist.
   */
  basePath: string;
  /**
   * Irregular list path that OpenAPI can't express as `GET {basePath}` — e.g.
   * tenants list via the caller's `/{home}/children` subtree. Overrides the
   * derived list path.
   */
  listPath?: (ctx: AdminContext) => string;
  /**
   * Verbs the admin policy intentionally does NOT offer even though the API
   * exposes them (e.g. conversions edit their state via actions, not a generic
   * update form). A `read-only` safety level suppresses create/update/remove
   * automatically; this list is for finer-grained cases.
   */
  suppressVerbs?: Verb[];
  /**
   * CRUD path builders. Built at boot from `basePath` + the OpenAPI spec (with
   * `listPath`/`suppressVerbs`/`safety` applied); not authored by hand. Present
   * after `ensureRegistryResolved()` runs.
   */
  paths?: ResourcePaths;
  fields: FieldDef[];
  actions?: ActionDef[];
  /** Initial create-form values (e.g. default parent to the home tenant). */
  formDefaults?: (ctx: AdminContext) => Record<string, unknown>;
  /**
   * For key-addressed resources (e.g. tenant metadata `PUT .../{type_id}`):
   * create is an upsert via the `update` path, with the record id taken from
   * this create-form field instead of being server-generated.
   */
  createKeyField?: string;
  /**
   * Send only this form field's value as the request body (a "transparent"
   * payload), instead of the whole form object. Used where the API body is
   * the bare value, e.g. tenant metadata's `PutTenantMetadataDto`.
   */
  bodyField?: string;
}
