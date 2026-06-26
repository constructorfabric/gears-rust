import {
  useList,
  useNavigation,
  useDelete,
  useResource,
} from "@refinedev/core";
import { Card, Table, Button, Space, Popconfirm, Alert } from "antd";

import {
  getDescriptor,
  idField,
  listFields,
} from "../resources";
import { fieldLabel, renderValue } from "../resources/fields";
import { ResourceActions } from "./ResourceActions";

/// Generated list screen: columns + row actions come from the resource
/// descriptor. Backend errors (e.g. RFC-9457 not-found / 403) render as an
/// inline alert so partial-CRUD resources degrade gracefully.
export const ResourceList = () => {
  const { resource } = useResource();
  const key = resource?.name ?? "";
  const d = getDescriptor(key);
  const { show, edit, create } = useNavigation();
  const { mutate: remove } = useDelete();

  const { data, isLoading, isError, error } = useList({ resource: key });

  if (isError) {
    const message = (error as { message?: string })?.message ?? "Failed to load";
    return (
      <Alert type="error" showIcon message={`${d.label} unavailable`} description={message} />
    );
  }

  const rows = (data?.data ?? []) as Record<string, unknown>[];
  const idKey = idField(d);

  const columns = listFields(d).map((f) => ({
    title: fieldLabel(f),
    dataIndex: f.name,
    key: f.name,
    render: (v: unknown) => renderValue(f, v),
  }));

  return (
    <Card
      title={d.label}
      extra={
        d.paths.create ? (
          <Button type="primary" onClick={() => create(key)}>
            Create
          </Button>
        ) : null
      }
    >
      <Table
        rowKey="id"
        loading={isLoading}
        size="small"
        scroll={{ x: true }}
        dataSource={rows}
        columns={[
          ...columns,
          {
            title: "Actions",
            key: "__actions",
            fixed: "right" as const,
            render: (_: unknown, record: Record<string, unknown>) => {
              const id = String(record[idKey] ?? record.id);
              return (
                <Space wrap size={4}>
                  {d.paths.one && (
                    <Button size="small" onClick={() => show(key, id)}>
                      View
                    </Button>
                  )}
                  {d.paths.update && (
                    <Button size="small" onClick={() => edit(key, id)}>
                      Edit
                    </Button>
                  )}
                  {d.paths.remove && (
                    <Popconfirm
                      title={`Delete this ${d.label.replace(/s$/, "").toLowerCase()}?`}
                      okText="Delete"
                      okButtonProps={{ danger: true }}
                      onConfirm={() => remove({ resource: key, id })}
                    >
                      <Button size="small" danger>
                        Delete
                      </Button>
                    </Popconfirm>
                  )}
                  <ResourceActions descriptor={d} record={record} />
                </Space>
              );
            },
          },
        ]}
      />
    </Card>
  );
};
