// Created: 2026-09-07 by Constructor Tech
// The sandbox page: every call goes to the same-origin proxy, which forwards
// it to the example server. Everything the API returns is logged raw below.

const TOKENS = [
  ["e2e-token-tenant-a", "platform admin (root tenant e2e-root)"],
  ["e2e-token-tenant-a-reviewer", "second platform admin"],
  ["e2e-token-hierarchy-root", "tenant admin: hierarchy-root (no subject_type: counts as a service principal for step-up)"],
  ["e2e-token-hierarchy-l1a", "tenant admin: hierarchy-l1a (no subject_type)"],
  ["e2e-token-hierarchy-l1b", "tenant admin: hierarchy-l1b (no subject_type)"],
  ["e2e-token-tenant-b", "foreign tenant B: outside the tree (expect 403/404)"],
];
const TENANTS = [
  ["", "platform scope (omit tenant)"],
  ["00000000-df51-5b42-9538-d2b56b7ee953", "e2e-root (root)"],
  ["00000000-0000-0000-0000-000000000001", "  hierarchy-root"],
  ["00000000-0000-0000-0000-000000000002", "    hierarchy-l1a"],
  ["00000000-0000-0000-0000-000000000004", "      hierarchy-l2b"],
  ["00000000-0000-0000-0000-000000000005", "    hierarchy-l1b"],
  ["00000000-0000-0000-0000-000000000006", "      hierarchy-l2c"],
];
const BASE = "/settings-service/v1";
const state = { token: TOKENS[0][0], tenant: "", categories: [], category: null, rows: [] };

const $ = (id) => document.getElementById(id);

function log(kind, text) {
  const line = document.createElement("div");
  line.className = kind;
  line.textContent = text;
  $("log").prepend(line);
}

async function api(method, path, { body, headers = {} } = {}) {
  const init = { method, headers: { Authorization: `Bearer ${state.token}`, Accept: "application/json", ...headers } };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  log("req", `${method} ${path}${headers["If-Match"] ? `  If-Match: ${headers["If-Match"]}` : ""}`);
  const response = await fetch(path, init);
  const text = await response.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { json = { raw: text }; }
  const etag = response.headers.get("ETag");
  const challenge = response.headers.get("WWW-Authenticate");
  const summary = `${response.status}${etag ? `  ETag: ${etag}` : ""}${challenge ? `  WWW-Authenticate: ${challenge}` : ""}`;
  log(response.ok ? "ok" : "err", summary + ($("showRaw").checked || !response.ok ? "\n" + JSON.stringify(json, null, 2) : ""));
  return { ok: response.ok, status: response.status, json, etag, challenge };
}

function tenantQuery(prefix = "?") {
  return state.tenant ? `${prefix}tenant=${state.tenant}` : "";
}

function encodeKey(key) {
  return encodeURIComponent(key);
}

function leaf(key) {
  // gts.cf.core.settings.setting_type.v1~<vendor>.<package>.<category>.<name>.vN~
  const derived = key.split("~")[1] || key;
  return derived.replace(/\.v\d+$/, "");
}

async function loadCategories() {
  const { ok, json } = await api("GET", `${BASE}/categories`);
  if (!ok) return;
  state.categories = json.items;
  const nav = $("categories");
  nav.innerHTML = "";
  for (const category of state.categories) {
    const button = document.createElement("button");
    button.textContent = `${category.name}`;
    button.title = category.key;
    button.className = state.category && state.category.id === category.id ? "active" : "";
    button.onclick = () => { state.category = category; loadCategory(); };
    nav.appendChild(button);
  }
}

async function loadCategory() {
  const category = state.category;
  if (!category) return;
  for (const button of $("categories").children) button.className = button.title === category.key ? "active" : "";
  $("title").textContent = `${category.name} at ${state.tenant ? TENANTS.find((t) => t[0] === state.tenant)[1].trim() : "platform scope"}`;
  $("panel").hidden = true;
  const filter = encodeURIComponent(`category_id eq ${category.id}`);
  const [declarations, settings] = await Promise.all([
    api("GET", `${BASE}/declarations?$filter=${filter}`),
    api("GET", `${BASE}/settings?$filter=${filter}${tenantQuery("&")}`),
  ]);
  if (!declarations.ok || !settings.ok) return;
  const byKey = new Map(declarations.json.items.map((d) => [d.key, d]));
  state.rows = settings.json.items.map((item) => ({ item, declaration: byKey.get(item.key) }));
  renderRows();
}

function widgetFor(declaration, current) {
  const type = (declaration && declaration.value_type_id) || "";
  if (type.includes("bool_flag")) {
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = current === true;
    input.read = () => input.checked;
    return input;
  }
  if (/integer|number|port|duration/.test(type)) {
    const input = document.createElement("input");
    input.type = "number";
    input.value = current ?? "";
    input.read = () => Number(input.value);
    return input;
  }
  if (type.includes("json") || type.includes("text")) {
    const area = document.createElement("textarea");
    area.rows = 3;
    area.value = type.includes("json") ? JSON.stringify(current, null, 2) : String(current ?? "");
    area.read = () => (type.includes("json") ? JSON.parse(area.value) : area.value);
    return area;
  }
  const input = document.createElement("input");
  input.type = "text";
  input.value = current === null || current === undefined ? "" : String(current);
  input.read = () => input.value;
  return input;
}

function renderRows() {
  const body = $("settings").querySelector("tbody");
  body.innerHTML = "";
  for (const row of state.rows) {
    const tr = document.createElement("tr");
    const effective = row.item.effective;
    const key = row.item.key;
    const name = document.createElement("td");
    name.innerHTML = `<b>${leaf(key)}</b><br><span class="muted">${row.declaration ? row.declaration.description || "" : ""}</span>`;
    const type = document.createElement("td");
    type.textContent = row.declaration ? row.declaration.value_type_id.replace("gts.cf.toolkit.settings.type_", "").replace(".v1~", "") : "?";
    const value = document.createElement("td");
    value.className = "value";
    const source = document.createElement("td");
    const actions = document.createElement("td");
    actions.className = "actions";
    if (row.item.outcome !== "resolved") {
      value.textContent = `${row.item.outcome}: ${row.item.detail || ""}`;
    } else {
      value.textContent = JSON.stringify(effective.value) + (effective.masked ? "  (masked)" : "");
      if (effective.needs_review) value.textContent += `\n needs review: ${effective.needs_review_detail || ""}`;
      source.innerHTML = `<span class="badge ${effective.source}">${effective.source}</span> ${effective.source_scope || ""}<br>` +
        `<span class="muted">etag ${effective.etag} · ${effective.data_classification}</span>` +
        `<details><summary>trail (${effective.inheritance_trail.length})</summary>${effective.inheritance_trail
          .map((e) => `${e.scope}${e.has_override ? " [override" + (e.needs_review ? ", flagged" : "") + "]" : ""}${e.provided_value ? " ← provided" : ""}${e.set_by ? " by " + e.set_by : ""}`)
          .join("<br>")}</details>`;
      const edit = document.createElement("button");
      edit.textContent = "Edit";
      edit.onclick = () => openEditor(row, tr);
      const revert = document.createElement("button");
      revert.textContent = "Revert";
      revert.title = "POST .../value/revert with If-Match";
      revert.onclick = () => write("POST", `${BASE}/settings/${encodeKey(key)}/value/revert${tenantQuery()}`, effective.etag);
      const remove = document.createElement("button");
      remove.textContent = "Remove";
      remove.onclick = () => write("DELETE", `${BASE}/settings/${encodeKey(key)}/value${tenantQuery()}`, effective.etag);
      const history = document.createElement("button");
      history.textContent = "History";
      history.onclick = () => showHistory(key);
      const impact = document.createElement("button");
      impact.textContent = "Impact";
      impact.onclick = () => showPanel(api("GET", `${BASE}/settings/${encodeKey(key)}/impact?value=${encodeURIComponent(JSON.stringify(effective.value))}${tenantQuery("&")}`));
      const access = document.createElement("button");
      access.textContent = "Access";
      access.title = "GET/PUT/DELETE .../permissions?tenant= for the selected tenant";
      access.onclick = () => openAccess(key, tr);
      actions.append(edit, revert, remove, history, impact, access);
    }
    tr.append(name, type, value, source, actions);
    body.appendChild(tr);
  }
}

function openEditor(row, tr) {
  const effective = row.item.effective;
  const key = row.item.key;
  const existing = tr.querySelector(".editor");
  if (existing) { existing.remove(); return; }
  const editor = document.createElement("div");
  editor.className = "editor";
  const widget = widgetFor(row.declaration, effective.value);
  const validate = document.createElement("button");
  validate.textContent = "Validate";
  validate.onclick = () => showPanel(api("POST", `${BASE}/settings/${encodeKey(key)}/validate${tenantQuery()}`, { body: { value: read(widget), limit: 20 } }));
  const save = document.createElement("button");
  save.textContent = `Save (If-Match ${effective.etag})`;
  save.onclick = () => write("PUT", `${BASE}/settings/${encodeKey(key)}/value${tenantQuery()}`, effective.etag, { value: read(widget) });
  editor.append(widget, validate, save);
  tr.querySelector("td.value").appendChild(editor);
  function read(w) { try { return w.read(); } catch (e) { log("err", `not JSON: ${e.message}`); throw e; } }
}

async function openAccess(key, tr) {
  const existing = tr.querySelector(".access");
  if (existing) { existing.remove(); return; }
  const path = `${BASE}/settings/${encodeKey(key)}/permissions${tenantQuery()}`;
  const current = await api("GET", path);
  if (!current.ok) return showResult(current);
  const readout = current.json;
  const box = document.createElement("div");
  box.className = "access";
  const info = document.createElement("span");
  info.className = "muted";
  info.textContent = `effective ${readout.effective.access}` +
    (readout.effective.supplied_by ? ` (set at ${readout.effective.supplied_by})` : "") +
    ` · stored ${readout.stored ? readout.stored.access : "none"} · etag ${readout.etag} `;
  const pick = document.createElement("select");
  fill(pick, [["read_only", "read_only"], ["hidden", "hidden"]], readout.stored ? readout.stored.access : "read_only");
  const set = document.createElement("button");
  set.textContent = `Set (If-Match ${readout.etag})`;
  set.onclick = () => plainWrite("PUT", path, readout.etag, { access: pick.value });
  const clear = document.createElement("button");
  clear.textContent = "Clear";
  clear.title = "DELETE with If-Match; a no-op when no row is stored";
  clear.onclick = () => plainWrite("DELETE", path, readout.etag);
  const all = document.createElement("button");
  all.textContent = "All rows";
  all.title = "GET .../permissions/all: every restriction row in the caller's subtree";
  all.onclick = () => showPanel(api("GET", `${BASE}/settings/${encodeKey(key)}/permissions/all`));
  box.append(info, pick, set, clear, all);
  tr.querySelector("td.actions").appendChild(box);
}

async function plainWrite(method, path, etag, body) {
  showResult(await api(method, path, { body, headers: { "If-Match": etag } }));
  await loadCategory();
}

async function write(method, path, etag, body) {
  const headers = { "If-Match": etag };
  const stepUp = window.prompt("Step-up token (leave empty unless the declaration requires step-up)", "");
  if (stepUp) headers["X-Step-Up-Token"] = stepUp;
  const result = await api(method, path, { body, headers });
  showResult(result);
  await loadCategory();
}

function showResult(result) {
  $("panel").hidden = false;
  $("panel").textContent = `HTTP ${result.status}` + (result.challenge ? `\nWWW-Authenticate: ${result.challenge}` : "") + "\n" + JSON.stringify(result.json, null, 2);
}

async function showPanel(promise) {
  showResult(await promise);
}

async function showHistory(key) {
  const result = await api("GET", `${BASE}/settings/${encodeKey(key)}/history${tenantQuery()}`);
  if (!result.ok) return showResult(result);
  $("panel").hidden = false;
  $("panel").textContent = result.json.items.length
    ? result.json.items.map((r) => `${r.occurred_at}  ${r.operation.padEnd(7)} by ${r.actor}${r.actor_masked ? " (masked)" : ""}  ${JSON.stringify(r.pre_value)} → ${JSON.stringify(r.post_value)}  req ${r.request_id}`).join("\n")
    : "no history at this scope";
}

function fill(select, options, value) {
  for (const [id, label] of options) {
    const option = document.createElement("option");
    option.value = id;
    option.textContent = label;
    select.appendChild(option);
  }
  select.value = value;
}

fill($("token"), TOKENS, state.token);
fill($("tenant"), TENANTS, state.tenant);
$("token").onchange = (e) => { state.token = e.target.value; loadCategories().then(loadCategory); };
$("tenant").onchange = (e) => { state.tenant = e.target.value; loadCategory(); };
$("reload").onclick = () => loadCategories().then(loadCategory);
loadCategories().then(() => { if (state.categories[0]) { state.category = state.categories[0]; loadCategory(); } });
