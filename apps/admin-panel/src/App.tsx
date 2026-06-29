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
import { AppstoreOutlined } from "@ant-design/icons";
import { createElement } from "react";

import "@refinedev/antd/dist/reset.css";

import { authProvider } from "./authProvider";
import { dataProvider } from "./dataProvider";
import { accessControlProvider } from "./accessControlProvider";
import { refineResources, RESOURCE_REGISTRY } from "./resources";
import { Login } from "./pages/Login";
import { ContextView } from "./pages/ContextView";
import { ResourceList } from "./pages/ResourceList";
import { ResourceShow } from "./pages/ResourceShow";
import { ResourceForm } from "./pages/ResourceForm";
import { TenantTree } from "./pages/TenantTree";

// Per-resource list-screen overrides (the "per-resource override" escape hatch
// from ADR-0003). Resources not listed here use the generated ResourceList.
const LIST_OVERRIDES: Record<string, JSX.Element> = {
  tenants: <TenantTree />,
};

// Per-resource CRUD routes are generated from the registry; a verb route is
// rendered only when the descriptor advertises it (create/edit/show).
const resourceRoutes = RESOURCE_REGISTRY.flatMap((d) => {
  const routes = [
    <Route
      key={`${d.key}-list`}
      path={`/${d.key}`}
      element={LIST_OVERRIDES[d.key] ?? <ResourceList />}
    />,
  ];
  if (d.paths.create) {
    routes.push(
      <Route key={`${d.key}-create`} path={`/${d.key}/create`} element={<ResourceForm />} />,
    );
  }
  if (d.paths.update) {
    routes.push(
      <Route key={`${d.key}-edit`} path={`/${d.key}/edit/:id`} element={<ResourceForm />} />,
    );
  }
  if (d.paths.one) {
    routes.push(
      <Route key={`${d.key}-show`} path={`/${d.key}/show/:id`} element={<ResourceShow />} />,
    );
  }
  return routes;
});

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
              meta: { label: "Context", icon: createElement(AppstoreOutlined) },
            },
            ...refineResources(),
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
              {resourceRoutes}
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
