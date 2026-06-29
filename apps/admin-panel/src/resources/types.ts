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
  paths: ResourcePaths;
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
