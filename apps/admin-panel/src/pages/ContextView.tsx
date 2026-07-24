import { useEffect, useState } from "react";
import { Card, Descriptions, Tag, Space, Alert, Table, Spin } from "antd";

import {
  cachedAdminContext,
  fetchEnabledGears,
  type AdminContext,
  type GearSummary,
} from "../adminContext";

/// v0 "current session/context view": shows the admin context (principal,
/// tenant, mode, capabilities) and the enabled gears summary.
export const ContextView = () => {
  const [ctx] = useState<AdminContext | null>(() => cachedAdminContext());
  const [gears, setGears] = useState<GearSummary[] | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetchEnabledGears()
      .then(setGears)
      .catch(() => setGears([]))
      .finally(() => setLoading(false));
  }, []);

  if (!ctx) {
    return <Alert type="error" message="No admin context available" />;
  }

  return (
    <Space direction="vertical" size="large" style={{ width: "100%" }}>
      {ctx.non_production_auth && (
        <Alert
          type="warning"
          showIcon
          message="Non-production authentication"
          description="This session uses the demo static auth stub. Roles are stubbed and must not be relied upon in production."
        />
      )}

      <Card title="Admin context">
        <Descriptions column={1} bordered size="small">
          <Descriptions.Item label="Subject">
            {ctx.subject_id}
          </Descriptions.Item>
          <Descriptions.Item label="Subject type">
            {ctx.subject_type ?? "—"}
          </Descriptions.Item>
          <Descriptions.Item label="Home tenant">
            {ctx.subject_tenant_id}
          </Descriptions.Item>
          <Descriptions.Item label="Admin mode">
            <Tag color={ctx.admin_mode === "platform" ? "geekblue" : "green"}>
              {ctx.admin_mode}
            </Tag>
          </Descriptions.Item>
          <Descriptions.Item label="Capabilities">
            <Space wrap>
              {ctx.capabilities.map((c) => (
                <Tag key={c}>{c}</Tag>
              ))}
            </Space>
          </Descriptions.Item>
        </Descriptions>
      </Card>

      <Card title="Enabled gears">
        {loading ? (
          <Spin />
        ) : (
          <Table
            rowKey="name"
            size="small"
            pagination={false}
            dataSource={gears ?? []}
            columns={[
              { title: "Gear", dataIndex: "name", key: "name" },
              {
                title: "Capabilities",
                dataIndex: "capabilities",
                key: "capabilities",
                render: (caps: string[]) => (caps ?? []).join(", "),
              },
              {
                title: "Mode",
                dataIndex: "deployment_mode",
                key: "deployment_mode",
              },
            ]}
          />
        )}
      </Card>
    </Space>
  );
};
