import { test, expect } from "@playwright/test";

// End-to-end browser smoke: signs in with the platform-admin dev role and
// checks that the descriptor-driven console actually renders and navigates.
// Requires the Gears backend on :8087 (`make admin`). The data it reads is
// served by that backend; this test asserts the UI wiring, not the API
// (the API is covered by testing/e2e/gears/admin).

const RESOURCES = ["Tenants", "Users", "Tenant metadata", "Resource groups", "Gears"];

test("platform admin: login, context, resource navigation, create form", async ({ page }) => {
  await page.goto("/");

  // Sign in as the platform-admin dev role.
  await page.getByRole("button", { name: "Platform admin (dev)" }).click();

  // Context view renders the projected mode, capabilities, and the non-prod banner.
  await expect(page.getByText("Admin context")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText("Non-production authentication")).toBeVisible();
  await expect(page.getByText("platform", { exact: true })).toBeVisible();
  await expect(page.getByText("tenants:read")).toBeVisible();
  await expect(page.getByText("users:write")).toBeVisible();

  // Every registered resource's list screen loads (card/table/graceful alert).
  for (const name of RESOURCES) {
    await page.getByRole("menuitem", { name: new RegExp(name, "i") }).first().click();
    await expect(page.locator(".ant-card, .ant-table, .ant-alert").first()).toBeVisible({
      timeout: 10_000,
    });
  }

  // The tenant create form is generated from the descriptor's create fields.
  await page.getByRole("menuitem", { name: /Tenants/i }).first().click();
  await page.getByRole("button", { name: /^Create$/ }).first().click();
  await expect(page.getByText("Create Tenants")).toBeVisible();
  await expect(page.getByLabel("Name")).toBeVisible();
  await expect(page.getByLabel("Type")).toBeVisible();
});
