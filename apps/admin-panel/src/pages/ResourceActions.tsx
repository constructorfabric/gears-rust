import { Button, Popconfirm, Space } from "antd";
import { useCustomMutation, useInvalidate } from "@refinedev/core";

import { cachedAdminContext } from "../adminContext";
import { idField, type ResourceDescriptor } from "../resources";
import type { ActionDef } from "../resources";

/// Renders a resource's custom actions (suspend/approve/...) for one record.
/// Visibility is gated by the action predicate AND the capability hint; the
/// backend re-authorizes every call. Destructive actions require confirmation.
export const ResourceActions = ({
  descriptor,
  record,
  size = "small",
}: {
  descriptor: ResourceDescriptor;
  record: Record<string, unknown>;
  size?: "small" | "middle";
}) => {
  const { mutate, isLoading } = useCustomMutation();
  const invalidate = useInvalidate();
  const ctx = cachedAdminContext();
  const caps = ctx?.capabilities ?? [];

  const actions = (descriptor.actions ?? []).filter((a) => {
    if (a.capability && !caps.includes(a.capability)) return false;
    if (a.visible && !a.visible(record)) return false;
    return true;
  });

  if (!ctx || actions.length === 0) return null;

  const id = String(record[idField(descriptor)] ?? record.id);

  const run = (a: ActionDef) =>
    mutate(
      {
        url: a.path(ctx, id),
        method: (a.method ?? "POST").toLowerCase() as "post" | "patch" | "put" | "delete",
        values: (a.body ? a.body(record) : {}) as Record<string, unknown>,
        successNotification: { type: "success", message: `${a.label} succeeded` },
      },
      {
        onSuccess: () => invalidate({ resource: descriptor.key, invalidates: ["list", "detail"] }),
      },
    );

  return (
    <Space wrap size={4}>
      {actions.map((a) => {
        const danger = a.safety === "destructive" || a.safety === "operator-only";
        const button = (
          <Button key={a.name} size={size} danger={danger} loading={isLoading}>
            {a.label}
          </Button>
        );
        return danger ? (
          <Popconfirm
            key={a.name}
            title={`${a.label}?`}
            description="This action is confirmed before it runs."
            okText={a.label}
            okButtonProps={{ danger: true }}
            onConfirm={() => run(a)}
          >
            {button}
          </Popconfirm>
        ) : (
          <span key={a.name} onClick={() => run(a)}>
            {button}
          </span>
        );
      })}
    </Space>
  );
};
