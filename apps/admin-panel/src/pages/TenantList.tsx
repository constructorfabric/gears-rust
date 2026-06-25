import { useList } from "@refinedev/core";
import { Card, Table, Tag, Alert } from "antd";

/// v0 tenants list: children of the home tenant via account-management.
/// Renders backend errors (e.g. RFC-9457 not-found) as an alert rather than
/// breaking, satisfying the partial-CRUD / graceful-degradation requirement.
export const TenantList = () => {
  const { data, isLoading, isError, error } = useList({
    resource: "tenants",
  });

  if (isError) {
    const message =
      (error as { message?: string })?.message ?? "Failed to load tenants";
    return <Alert type="error" showIcon message="Tenants unavailable" description={message} />;
  }

  return (
    <Card title="Tenants (children of home tenant)">
      <Table
        rowKey="id"
        loading={isLoading}
        size="small"
        dataSource={data?.data ?? []}
        columns={[
          { title: "ID", dataIndex: "id", key: "id" },
          { title: "Name", dataIndex: "name", key: "name" },
          {
            title: "Status",
            dataIndex: "status",
            key: "status",
            render: (s: string) => (s ? <Tag>{s}</Tag> : "—"),
          },
        ]}
      />
    </Card>
  );
};
