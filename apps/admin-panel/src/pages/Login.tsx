import { useLogin } from "@refinedev/core";
import { Button, Card, Input, Space, Typography, Alert } from "antd";
import { useState } from "react";

import { DEV_TOKENS } from "../config";

const { Title, Paragraph } = Typography;

/// v0 login: pick a non-production dev role or paste a bearer token.
export const Login = () => {
  const { mutate: login, isLoading } = useLogin();
  const [token, setToken] = useState("");

  return (
    <div
      style={{
        display: "flex",
        justifyContent: "center",
        alignItems: "center",
        minHeight: "100vh",
      }}
    >
      <Card style={{ width: 420 }}>
        <Title level={3}>CF/Gears Admin Panel</Title>
        <Alert
          type="warning"
          showIcon
          message="Non-production"
          description="Demo static auth. The dev tokens below are not secrets and must not be used in real deployments."
          style={{ marginBottom: 16 }}
        />
        <Paragraph>Sign in as a dev role:</Paragraph>
        <Space direction="vertical" style={{ width: "100%" }}>
          {DEV_TOKENS.map((t) => (
            <Button
              key={t.token}
              block
              onClick={() => login({ token: t.token })}
              loading={isLoading}
            >
              {t.label}
            </Button>
          ))}
          <Paragraph style={{ marginTop: 12 }}>Or paste a bearer token:</Paragraph>
          <Input.Search
            placeholder="bearer token"
            enterButton="Sign in"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            onSearch={(value) => value && login({ token: value })}
            loading={isLoading}
          />
        </Space>
      </Card>
    </div>
  );
};
