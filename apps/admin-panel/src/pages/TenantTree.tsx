import { useCallback, useEffect, useState } from "react";
import { useNavigation, useDelete } from "@refinedev/core";
import { Card, Table, Button, Space, Popconfirm, Alert } from "antd";

import { apiFetch } from "../httpClient";
import { cachedAdminContext } from "../adminContext";
import { getDescriptor, idField, listFields } from "../resources";
import { fieldLabel, renderValue } from "../resources/fields";
import { ResourceActions } from "./ResourceActions";

const AM = "/account-management/v1";

// One tenant row, with lazily-loaded children. A node with children > 0 that
// has not been expanded yet carries a single placeholder child so Ant Table
// renders the expander; the placeholder is swapped for the real rows on
// expand.
type Row = Record<string, unknown> & { children?: Row[] };

const PLACEHOLDER = "__loading__";

function withPlaceholder(rows: Row[]): Row[] {
  return rows.map((r) => {
    const count = Number(r.child_count ?? 0);
    return count > 0 ? { ...r, children: [{ id: `${String(r.id)}:${PLACEHOLDER}` }] } : r;
  });
}

function isPlaceholder(r: Row): boolean {
  return String(r.id).endsWith(`:${PLACEHOLDER}`);
}

/** Recursively replace the children of the node with `id`. */
function injectChildren(rows: Row[], id: string, children: Row[]): Row[] {
  return rows.map((r) => {
    if (String(r.id) === id) return { ...r, children: withPlaceholder(children) };
    if (r.children) return { ...r, children: injectChildren(r.children, id, children) };
    return r;
  });
}

/// Tenants hierarchy view: the home tenant as the root, children loaded lazily
/// per node via `/tenants/{id}/children`. Reuses the tenants descriptor for
/// columns, row actions, and navigation, so it stays in sync with the registry.
export const TenantTree = () => {
  const d = getDescriptor("tenants");
  const idKey = idField(d);
  const { show, edit, create } = useNavigation();
  const { mutate: remove } = useDelete();

  const [rows, setRows] = useState<Row[]>([]);
  const [expandedKeys, setExpandedKeys] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchChildren = useCallback(async (id: string): Promise<Row[]> => {
    const payload = await apiFetch<{ items?: Row[] }>(`${AM}/tenants/${id}/children`);
    return payload.items ?? [];
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const ctx = cachedAdminContext();
      if (!ctx) throw new Error("No admin context — sign in first");
      const home = ctx.subject_tenant_id;
      const root = await apiFetch<Row>(`${AM}/tenants/${home}`);
      root.children = withPlaceholder(await fetchChildren(home));
      setRows([root]);
      // Expand the root by default so its children are visible immediately.
      setExpandedKeys([String(root[idKey] ?? root.id)]);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, [fetchChildren]);

  useEffect(() => {
    load();
  }, [load]);

  const onExpand = async (expanded: boolean, record: Row) => {
    const id = String(record[idKey] ?? record.id);
    setExpandedKeys((prev) =>
      expanded ? [...new Set([...prev, id])] : prev.filter((k) => k !== id),
    );
    if (!expanded) return;
    const loaded = record.children?.some((c) => !isPlaceholder(c));
    if (loaded) return;
    const kids = await fetchChildren(id);
    setRows((prev) => injectChildren(prev, id, kids));
  };

  if (error) {
    return <Alert type="error" showIcon message="Tenants unavailable" description={error} />;
  }

  const columns = [
    ...listFields(d).map((f) => ({
      title: fieldLabel(f),
      dataIndex: f.name,
      key: f.name,
      render: (v: unknown, record: Row) => (isPlaceholder(record) ? "" : renderValue(f, v)),
    })),
    {
      title: "Actions",
      key: "__actions",
      render: (_: unknown, record: Row) => {
        if (isPlaceholder(record)) return "…";
        const id = String(record[idKey] ?? record.id);
        return (
          <Space wrap size={4}>
            {d.paths.one && <Button size="small" onClick={() => show("tenants", id)}>View</Button>}
            {d.paths.update && <Button size="small" onClick={() => edit("tenants", id)}>Edit</Button>}
            {d.paths.remove && (
              <Popconfirm
                title="Delete this tenant?"
                okText="Delete"
                okButtonProps={{ danger: true }}
                onConfirm={() => remove({ resource: "tenants", id }, { onSuccess: load })}
              >
                <Button size="small" danger>Delete</Button>
              </Popconfirm>
            )}
            <ResourceActions descriptor={d} record={record} />
          </Space>
        );
      },
    },
  ];

  return (
    <Card
      title="Tenants (hierarchy)"
      extra={<Button type="primary" onClick={() => create("tenants")}>Create</Button>}
    >
      <Table
        rowKey="id"
        loading={loading}
        size="small"
        scroll={{ x: true }}
        pagination={false}
        dataSource={rows}
        columns={columns}
        expandable={{
          expandedRowKeys: expandedKeys,
          onExpand,
          rowExpandable: (r) => Boolean(r.children?.length),
        }}
      />
    </Card>
  );
};
