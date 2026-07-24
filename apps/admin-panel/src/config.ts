// Same-origin base: in dev Vite proxies /cf to the example server (port 8087);
// in production the SPA is served under /cf/admin by the example server, so /cf
// is reachable directly. The API Gateway prefix is /cf.
export const API_PREFIX = "/cf";

export const TOKEN_STORAGE_KEY = "cf-admin-token";
export const CONTEXT_STORAGE_KEY = "cf-admin-context";

// NON-PRODUCTION dev tokens shipped by config/admin.yaml's static auth stub.
// Presented on the login screen as one-click role logins.
export const DEV_TOKENS: { label: string; token: string }[] = [
  { label: "Platform admin (dev)", token: "platform-admin-dev-token" },
  { label: "Tenant admin (dev)", token: "tenant-admin-dev-token" },
];
