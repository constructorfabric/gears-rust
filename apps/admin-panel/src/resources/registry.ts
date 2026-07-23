import {
  TeamOutlined,
  ClusterOutlined,
  ApiOutlined,
  AppstoreOutlined,
  SwapOutlined,
  TagsOutlined,
  UserOutlined,
  ProfileOutlined,
  CloudServerOutlined,
  NodeIndexOutlined,
} from "@ant-design/icons";
import { createElement, type ReactNode } from "react";

import adminConfig from "./admin.config.json";
import { buildDescriptors, type AdminConfig } from "./configLoader";
import type { ResourceDescriptor } from "./types";

/**
 * The admin resource registry, built from the declarative `admin.config.json`
 * (ADR-0003 concern-split). The config is data, not code: registering a
 * resource — here or in another gears-rust project — is a JSON edit, and the
 * API-intrinsic parts (field types, CRUD verbs, tenant-scope) are filled at
 * boot from the aggregated OpenAPI spec (see `openapi.ts`). Adding a resource
 * requires no changes to this file or any TypeScript.
 */
export const RESOURCE_REGISTRY: ResourceDescriptor[] = buildDescriptors(
  adminConfig as AdminConfig,
);

/** Sidebar icon per resource key (presentation only). */
const ICONS: Record<string, () => ReactNode> = {
  tenants: () => createElement(TeamOutlined),
  "tenant-metadata": () => createElement(ProfileOutlined),
  users: () => createElement(UserOutlined),
  conversions: () => createElement(SwapOutlined),
  "resource-groups": () => createElement(ClusterOutlined),
  types: () => createElement(TagsOutlined),
  gears: () => createElement(ApiOutlined),
  upstreams: () => createElement(CloudServerOutlined),
  routes: () => createElement(NodeIndexOutlined),
};

export function resourceIcon(key: string): ReactNode {
  return (ICONS[key] ?? (() => createElement(AppstoreOutlined)))();
}
