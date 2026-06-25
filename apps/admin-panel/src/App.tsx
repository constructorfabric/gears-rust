import { Refine, Authenticated } from "@refinedev/core";
import {
  ThemedLayoutV2,
  ErrorComponent,
  useNotificationProvider,
  RefineThemes,
} from "@refinedev/antd";
import routerBindings, {
  CatchAllNavigate,
  NavigateToResource,
} from "@refinedev/react-router-v6";
import { BrowserRouter, Routes, Route, Outlet } from "react-router-dom";
import { ConfigProvider, App as AntdApp } from "antd";
import { TeamOutlined, ClusterOutlined, ApiOutlined, AppstoreOutlined } from "@ant-design/icons";

import "@refinedev/antd/dist/reset.css";

import { authProvider } from "./authProvider";
import { dataProvider } from "./dataProvider";
import { accessControlProvider } from "./accessControlProvider";
import { Login } from "./pages/Login";
import { ContextView } from "./pages/ContextView";
import { TenantList } from "./pages/TenantList";
import { GenericList } from "./pages/GenericList";

export const App = () => (
  <BrowserRouter basename={import.meta.env.BASE_URL}>
    <ConfigProvider theme={RefineThemes.Blue}>
      <AntdApp>
        <Refine
          dataProvider={dataProvider}
          authProvider={authProvider}
          accessControlProvider={accessControlProvider}
          routerProvider={routerBindings}
          notificationProvider={useNotificationProvider}
          resources={[
            {
              name: "context",
              list: "/",
              meta: { label: "Context", icon: <AppstoreOutlined /> },
            },
            {
              name: "tenants",
              list: "/tenants",
              meta: { label: "Tenants", icon: <TeamOutlined /> },
            },
            {
              name: "resource-groups",
              list: "/resource-groups",
              meta: { label: "Resource groups", icon: <ClusterOutlined /> },
            },
            {
              name: "gears",
              list: "/gears",
              meta: { label: "Gears", icon: <ApiOutlined /> },
            },
          ]}
          options={{ syncWithLocation: true, warnWhenUnsavedChanges: true }}
        >
          <Routes>
            <Route
              element={
                <Authenticated key="auth" fallback={<CatchAllNavigate to="/login" />}>
                  <ThemedLayoutV2>
                    <Outlet />
                  </ThemedLayoutV2>
                </Authenticated>
              }
            >
              <Route index element={<ContextView />} />
              <Route path="/tenants" element={<TenantList />} />
              <Route
                path="/resource-groups"
                element={<GenericList resource="resource-groups" title="Resource groups" />}
              />
              <Route
                path="/gears"
                element={<GenericList resource="gears" title="Gears" />}
              />
              <Route path="*" element={<ErrorComponent />} />
            </Route>
            <Route
              element={
                <Authenticated key="anon" fallback={<Outlet />}>
                  <NavigateToResource resource="context" />
                </Authenticated>
              }
            >
              <Route path="/login" element={<Login />} />
            </Route>
          </Routes>
        </Refine>
      </AntdApp>
    </ConfigProvider>
  </BrowserRouter>
);
