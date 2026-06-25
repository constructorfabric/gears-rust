import { apiFetch } from "./httpClient";
import { CONTEXT_STORAGE_KEY } from "./config";

/// Mirror of account-management AdminContextDto.
export interface AdminContext {
  subject_id: string;
  subject_type?: string;
  subject_tenant_id: string;
  admin_mode: "platform" | "tenant";
  capabilities: string[];
  non_production_auth: boolean;
}

/// One gear as reported by the gear orchestrator.
export interface GearSummary {
  name: string;
  capabilities: string[];
  dependencies: string[];
  deployment_mode: string;
  instances: unknown[];
}

export async function fetchAdminContext(): Promise<AdminContext> {
  const ctx = await apiFetch<AdminContext>(
    "/account-management/v1/admin/context",
  );
  localStorage.setItem(CONTEXT_STORAGE_KEY, JSON.stringify(ctx));
  return ctx;
}

export function cachedAdminContext(): AdminContext | null {
  const raw = localStorage.getItem(CONTEXT_STORAGE_KEY);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as AdminContext;
  } catch {
    return null;
  }
}

export async function fetchEnabledGears(): Promise<GearSummary[]> {
  return apiFetch<GearSummary[]>("/gear-orchestrator/v1/gears");
}
