import { useEffect } from "react";
import {
  useCreate,
  useUpdate,
  useOne,
  useResource,
  useNavigation,
} from "@refinedev/core";
import { Card, Form, Button, Space, Spin, Alert, App } from "antd";

import { cachedAdminContext } from "../adminContext";
import {
  getDescriptor,
  createFields,
  editFields,
  type FieldDef,
} from "../resources";
import {
  fieldLabel,
  fieldInput,
  valueProp,
  parseFormValues,
} from "../resources/fields";

/// Generated create/edit form. Field set, validation, and immutability come
/// from the resource descriptor (`createFields` vs `editFields`). One screen
/// serves both routes; the mode is read from the active Refine action.
export const ResourceForm = () => {
  const { resource, id, action } = useResource();
  const key = resource?.name ?? "";
  const d = getDescriptor(key);
  const isEdit = action === "edit";
  const fields: FieldDef[] = isEdit ? editFields(d) : createFields(d);

  const [form] = Form.useForm();
  const { message } = App.useApp();
  const { list, show } = useNavigation();
  const { mutate: create, isLoading: creating } = useCreate();
  const { mutate: update, isLoading: updating } = useUpdate();

  const { data, isLoading: loadingOne } = useOne({
    resource: key,
    id: String(id ?? ""),
    queryOptions: { enabled: isEdit },
  });

  useEffect(() => {
    if (isEdit && data?.data) {
      form.setFieldsValue(toFormValues(fields, data.data as Record<string, unknown>));
    } else if (!isEdit && d.formDefaults) {
      const ctx = cachedAdminContext();
      if (ctx) form.setFieldsValue(d.formDefaults(ctx));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isEdit, data]);

  if (isEdit && loadingOne) return <Spin />;

  const onFinish = (values: Record<string, unknown>) => {
    let payload: Record<string, unknown>;
    try {
      payload = parseFormValues(fields, values);
    } catch (e) {
      message.error((e as Error).message);
      return;
    }
    if (isEdit) {
      update(
        { resource: key, id: String(id ?? ""), values: payload },
        { onSuccess: () => show(key, String(id ?? "")) },
      );
    } else {
      create({ resource: key, values: payload }, { onSuccess: () => list(key) });
    }
  };

  return (
    <Card title={`${isEdit ? "Edit" : "Create"} ${d.label}`}>
      {fields.length === 0 ? (
        <Alert type="info" showIcon message="This resource exposes no editable fields." />
      ) : (
        <Form form={form} layout="vertical" onFinish={onFinish}>
          {fields.map((f) => (
            <Form.Item
              key={f.name}
              name={f.name}
              label={fieldLabel(f)}
              valuePropName={valueProp(f)}
              rules={f.required ? [{ required: true, message: `${fieldLabel(f)} is required` }] : []}
            >
              {fieldInput(f)}
            </Form.Item>
          ))}
          <Space>
            <Button type="primary" htmlType="submit" loading={creating || updating}>
              {isEdit ? "Save" : "Create"}
            </Button>
            <Button onClick={() => list(key)}>Cancel</Button>
          </Space>
        </Form>
      )}
    </Card>
  );
};

/// Project a record onto form values: JSON fields are stringified for the
/// text control; everything else passes through.
function toFormValues(
  fields: FieldDef[],
  record: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of fields) {
    const v = record[f.name];
    if (v === undefined) continue;
    out[f.name] = f.type === "json" && typeof v === "object" ? JSON.stringify(v, null, 2) : v;
  }
  return out;
}
