import { useList } from "@refinedev/core";
import { Card, Table, Alert } from "antd";

/// Minimal generic list for v0 resources that don't yet have a bespoke
/// screen. Columns are inferred from the first row's keys. Replaced by
/// OpenAPI/registry-driven generation in a later increment (ADR-0003).
export const GenericList = ({
  resource,
  title,
}: {
  resource: string;
  title: string;
}) => {
  const { data, isLoading, isError, error } = useList({ resource });

  if (isError) {
    const message =
      (error as { message?: string })?.message ?? "Failed to load";
    return (
      <Alert type="error" showIcon message={`${title} unavailable`} description={message} />
    );
  }

  const rows = (data?.data ?? []) as Record<string, unknown>[];
  const keys = rows.length > 0 ? Object.keys(rows[0]) : ["id"];
  const columns = keys.map((k) => ({
    title: k,
    dataIndex: k,
    key: k,
    render: (v: unknown) =>
      typeof v === "object" ? JSON.stringify(v) : String(v ?? "—"),
  }));

  return (
    <Card title={title}>
      <Table
        rowKey="id"
        loading={isLoading}
        size="small"
        dataSource={rows}
        columns={columns}
        scroll={{ x: true }}
      />
    </Card>
  );
};
