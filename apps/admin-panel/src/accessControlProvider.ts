import type { AccessControlProvider } from "@refinedev/core";

import { cachedAdminContext } from "./adminContext";
import { getDescriptor } from "./resources";

/// Maps Refine `can({resource, action})` checks onto the capability hints a
/// resource descriptor declares, against the admin context. UI gating only —
/// the backend remains the final authority and re-checks every action.
///
/// A verb is gated only when the descriptor declares a capability for it; a
/// resource that declares none (e.g. an ungated read-only view) is allowed.
/// Refine actions: list/show -> read, create/edit -> write, delete -> delete.
const ACTION_TO_VERB: Record<string, "read" | "write" | "delete"> = {
  list: "read",
  show: "read",
  create: "write",
  edit: "write",
  clone: "write",
  delete: "delete",
};

export const accessControlProvider: AccessControlProvider = {
  can: async ({ resource, action }) => {
    if (!resource) return { can: true };

    // Resources outside the registry (e.g. the context view) are never gated.
    let caps: { read?: string; write?: string; delete?: string } | undefined;
    try {
      caps = getDescriptor(resource).capabilities;
    } catch {
      return { can: true };
    }

    const verb = ACTION_TO_VERB[action] ?? "read";
    const required = verb === "delete" ? (caps?.delete ?? caps?.write) : caps?.[verb];
    // Ungated when the descriptor declares no capability for this verb.
    if (!required) return { can: true };

    const held = cachedAdminContext()?.capabilities ?? [];
    return { can: held.includes(required) };
  },
};
