import { Tag, Input, InputNumber, Switch } from "antd";
import type { ReactNode } from "react";

import type { FieldDef } from "./types";

const { TextArea } = Input;

/** Human label for a field (explicit `label` or a Title-cased name). */
export function fieldLabel(f: FieldDef): string {
  if (f.label) return f.label;
  return f.name
    .replace(/_/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

/** Render a stored value for list/detail display, by field type. */
export function renderValue(f: FieldDef, value: unknown): ReactNode {
  if (value === null || value === undefined || value === "") return "—";
  switch (f.type) {
    case "tag":
      return <Tag>{String(value)}</Tag>;
    case "boolean":
      return value ? "Yes" : "No";
    case "json":
      return Array.isArray(value)
        ? value.map((v) => String(v)).join(", ") || "—"
        : JSON.stringify(value);
    default:
      return typeof value === "object" ? JSON.stringify(value) : String(value);
  }
}

/**
 * Antd form control for a field. JSON fields edit as text and are parsed on
 * submit (see `parseFormValues`). Read-only fields render disabled.
 */
export function fieldInput(f: FieldDef): ReactNode {
  const disabled = f.readOnly === true;
  switch (f.type) {
    case "boolean":
      return <Switch disabled={disabled} />;
    case "number":
      return <InputNumber disabled={disabled} style={{ width: "100%" }} />;
    case "json":
      return <TextArea disabled={disabled} rows={4} placeholder="JSON" />;
    default:
      return <Input disabled={disabled} />;
  }
}

/** valuePropName differs for the Switch control. */
export function valueProp(f: FieldDef): string {
  return f.type === "boolean" ? "checked" : "value";
}

/**
 * Coerce raw form values into the API payload: JSON text fields are parsed,
 * empty optional fields are dropped so the backend keeps its defaults.
 */
export function parseFormValues(
  fields: FieldDef[],
  values: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of fields) {
    const v = values[f.name];
    if (v === undefined || v === "") continue;
    if (f.type === "json" && typeof v === "string") {
      try {
        out[f.name] = JSON.parse(v);
      } catch {
        throw new Error(`Field "${fieldLabel(f)}" must be valid JSON`);
      }
    } else {
      out[f.name] = v;
    }
  }
  return out;
}
