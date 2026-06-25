import type { AccessControlProvider } from "@refinedev/core";

import { cachedAdminContext } from "./adminContext";

/// Maps Refine `can({resource, action})` checks onto the coarse capability
/// hints from the admin context. UI gating only — the backend remains the
/// final authority and re-checks every action.
///
/// Refine actions: list/show -> read, create/edit -> write, delete -> write.
const ACTION_TO_VERB: Record<string, string> = {
  list: "read",
  show: "read",
  create: "write",
  edit: "write",
  clone: "write",
  delete: "write",
};

export const accessControlProvider: AccessControlProvider = {
  can: async ({ resource, action }) => {
    const caps = cachedAdminContext()?.capabilities ?? [];
    if (!resource) return { can: true };

    const verb = ACTION_TO_VERB[action] ?? action;
    const needed = `${resource}:${verb}`;
    // Allow when the exact capability is present, or any capability for the
    // resource exists and the action is a read.
    const can =
      caps.includes(needed) ||
      (verb === "read" && caps.some((c) => c.startsWith(`${resource}:`)));
    return { can };
  },
};
