import type { IResourceItem } from "@refinedev/core";

import { RESOURCE_REGISTRY, resourceIcon } from "./registry";
import type { ResourceDescriptor, FieldDef } from "./types";

export type { ResourceDescriptor, FieldDef, ActionDef } from "./types";
export { RESOURCE_REGISTRY, resourceIcon } from "./registry";

const BY_KEY: Record<string, ResourceDescriptor> = Object.fromEntries(
  RESOURCE_REGISTRY.map((d) => [d.key, d]),
);

/** Look up a descriptor by Refine resource key, or throw if unknown. */
export function getDescriptor(key: string): ResourceDescriptor {
  const d = BY_KEY[key];
  if (!d) {
    throw new Error(`Unknown admin resource: ${key}`);
  }
  return d;
}

/** Record identity field for a resource ("id" unless overridden). */
export function idField(d: ResourceDescriptor): string {
  return d.idField ?? "id";
}

/** Fields shown as list columns. */
export function listFields(d: ResourceDescriptor): FieldDef[] {
  return d.fields.filter((f) => f.inList);
}

/** Fields shown in the detail/show panel (all unless explicitly hidden). */
export function detailFields(d: ResourceDescriptor): FieldDef[] {
  return d.fields.filter((f) => f.inDetail !== false);
}

/** Fields editable in the create form. */
export function createFields(d: ResourceDescriptor): FieldDef[] {
  return d.fields.filter((f) => f.inForm || f.createOnly);
}

/** Fields editable in the edit form (create-only fields are immutable). */
export function editFields(d: ResourceDescriptor): FieldDef[] {
  return d.fields.filter((f) => f.inForm && !f.createOnly);
}

/**
 * Build the Refine `resources` array (navigation + route metadata) from the
 * registry. A verb route is advertised only when its path builder exists, so
 * resources without create/edit/delete degrade gracefully.
 */
export function refineResources(): IResourceItem[] {
  return RESOURCE_REGISTRY.map((d) => ({
    name: d.key,
    list: `/${d.key}`,
    ...(d.paths.one ? { show: `/${d.key}/show/:id` } : {}),
    ...(d.paths.create ? { create: `/${d.key}/create` } : {}),
    ...(d.paths.update ? { edit: `/${d.key}/edit/:id` } : {}),
    meta: {
      label: d.label,
      icon: resourceIcon(d.key),
      canDelete: Boolean(d.paths.remove),
    },
  }));
}
