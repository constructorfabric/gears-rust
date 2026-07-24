import { useOne, useResource, useNavigation } from "@refinedev/core";
import { Card, Descriptions, Button, Space, Alert, Spin } from "antd";

import { getDescriptor, detailFields } from "../resources";
import { fieldLabel, renderValue } from "../resources/fields";
import { ResourceActions } from "./ResourceActions";

/// Generated detail screen from the resource descriptor.
export const ResourceShow = () => {
  const { resource, id } = useResource();
  const key = resource?.name ?? "";
  const d = getDescriptor(key);
  const { edit } = useNavigation();

  const { data, isLoading, isError, error } = useOne({
    resource: key,
    id: String(id ?? ""),
  });

  if (isError) {
    const message = (error as { message?: string })?.message ?? "Failed to load";
    return <Alert type="error" showIcon message={`${d.label} unavailable`} description={message} />;
  }
  if (isLoading || !data?.data) return <Spin />;

  const record = data.data as Record<string, unknown>;

  return (
    <Card
      title={`${d.label} detail`}
      extra={
        <Space>
          <ResourceActions descriptor={d} record={record} size="middle" />
          {d.paths?.update && (
            <Button type="primary" onClick={() => edit(key, String(id ?? ""))}>
              Edit
            </Button>
          )}
        </Space>
      }
    >
      <Descriptions column={1} bordered size="small">
        {detailFields(d).map((f) => (
          <Descriptions.Item key={f.name} label={fieldLabel(f)}>
            {renderValue(f, record[f.name])}
          </Descriptions.Item>
        ))}
      </Descriptions>
    </Card>
  );
};
