import type { AuthProvider } from "@refinedev/core";

import { TOKEN_STORAGE_KEY, CONTEXT_STORAGE_KEY } from "./config";
import { fetchAdminContext, cachedAdminContext } from "./adminContext";
import { ensureSchemasResolved } from "./resources/openapi";

/// Bearer-token auth provider over the platform auth flow.
///
/// v0 login accepts a bearer token directly (the non-production static auth
/// stub ships two dev tokens). `check` validates the token by fetching the
/// admin context; `getIdentity` reflects the cached context.
export const authProvider: AuthProvider = {
  login: async ({ token }: { token?: string }) => {
    if (!token) {
      return {
        success: false,
        error: { name: "LoginError", message: "A bearer token is required" },
      };
    }
    localStorage.setItem(TOKEN_STORAGE_KEY, token);
    try {
      await fetchAdminContext();
      await ensureSchemasResolved();
    } catch {
      localStorage.removeItem(TOKEN_STORAGE_KEY);
      return {
        success: false,
        error: {
          name: "LoginError",
          message: "Token rejected by the server",
        },
      };
    }
    return { success: true, redirectTo: "/" };
  },

  logout: async () => {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
    localStorage.removeItem(CONTEXT_STORAGE_KEY);
    return { success: true, redirectTo: "/login" };
  },

  check: async () => {
    const token = localStorage.getItem(TOKEN_STORAGE_KEY);
    if (!token) {
      return { authenticated: false, redirectTo: "/login" };
    }
    try {
      await fetchAdminContext();
      await ensureSchemasResolved();
      return { authenticated: true };
    } catch {
      localStorage.removeItem(TOKEN_STORAGE_KEY);
      return { authenticated: false, redirectTo: "/login" };
    }
  },

  onError: async (error) => {
    if (error?.status === 401) {
      return { logout: true, redirectTo: "/login" };
    }
    return {};
  },

  getIdentity: async () => {
    const ctx = cachedAdminContext();
    if (!ctx) return null;
    return {
      id: ctx.subject_id,
      name: ctx.subject_type ?? ctx.subject_id,
      adminMode: ctx.admin_mode,
    };
  },

  getPermissions: async () => {
    return cachedAdminContext()?.capabilities ?? [];
  },
};
