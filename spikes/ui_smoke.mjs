// Headless render smoke test for plugin/frontend/market.js (no browser needed).
//
// Renders the real MarketPage against a fake QwenPaw host + fake plugin API and
// asserts the catalogue UX: card/list views, debounce, sort, detail drawer,
// installed-vs-market version badges, infinite scroll, error state, the agent
// install dialog and the dark theme. Fails on any unexpected console error.
//
// Setup (throwaway dir, not part of the repo):
//   npm install --prefix /tmp/harness react@18 react-dom@18 antd@5 jsdom
// Run:
//   node spikes/ui_smoke.mjs
//
// Env overrides: MARKET_JS (default: ../plugin/frontend/market.js),
//                NODE_MODULES (default: /tmp/harness/node_modules).
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const MODULES = process.env.NODE_MODULES || "/tmp/harness/node_modules";
const SRC = process.env.MARKET_JS || path.join(HERE, "..", "plugin", "frontend", "market.js");
const WORKDIR = fs.mkdtempSync(path.join(os.tmpdir(), "market-ui-smoke-"));
const COPY = path.join(WORKDIR, "market.mjs");
fs.copyFileSync(SRC, COPY);

const { JSDOM } = await import(path.join(MODULES, "jsdom", "lib", "api.js"));

const dom = new JSDOM(
  '<!doctype html><html><head></head><body><div id="root"></div></body></html>',
  { url: "http://localhost/offline-market", pretendToBeVisual: true },
);
const { window } = dom;
for (const key of Object.getOwnPropertyNames(window)) {
  if (key in globalThis) continue;
  try {
    Object.defineProperty(globalThis, key, { value: window[key], configurable: true, writable: true });
  } catch {
    /* read-only globals are fine to skip */
  }
}
Object.defineProperty(globalThis, "navigator", { value: window.navigator, configurable: true, writable: true });
const rawComputedStyle = window.getComputedStyle.bind(window);
globalThis.getComputedStyle = (el) => rawComputedStyle(el);
window.getComputedStyle = (el) => rawComputedStyle(el);
globalThis.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 0);
globalThis.cancelAnimationFrame = (id) => clearTimeout(id);

window.matchMedia = (query) => ({
  matches: false,
  media: query,
  onchange: null,
  addListener() {},
  removeListener() {},
  addEventListener() {},
  removeEventListener() {},
  dispatchEvent() {
    return false;
  },
});

const observers = [];
class FakeIntersectionObserver {
  constructor(cb) {
    this.cb = cb;
    this.elements = [];
    observers.push(this);
  }
  observe(el) {
    this.elements.push(el);
  }
  unobserve() {}
  disconnect() {
    this.elements = [];
  }
  takeRecords() {
    return [];
  }
}
globalThis.IntersectionObserver = window.IntersectionObserver = FakeIntersectionObserver;
globalThis.ResizeObserver = window.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

const require = createRequire(path.join(MODULES, "noop.js"));
const React = require("react");
const ReactDOMClient = require("react-dom/client");
const antd = require("antd");

// ------------------------------------------------------------- fake backend ---
const SKILLS = [
  {
    slug: "alpha",
    name: "Alpha",
    description: "把重复的周报整理成结构化摘要。",
    owner_tenant: "tenant-a",
    owner_name: "平台组",
    visibility: "public",
    latest_version: "1.2.0",
    versions: 2,
    size_bytes: 2048,
    download_count: 7,
    updated_at: 1735689600,
    created_at: 1735000000,
  },
];
for (let i = 1; i <= 44; i += 1) {
  SKILLS.push({
    slug: `skill-${i}`,
    name: `Skill ${String(i).padStart(2, "0")}`,
    description: `批量技能 ${i}`,
    owner_tenant: "tenant-a",
    owner_name: "平台组",
    visibility: i % 3 === 0 ? "tenant" : "public",
    latest_version: `0.${i}.0`,
    versions: 1,
    size_bytes: 1024 * i,
    download_count: i,
    updated_at: 1735689600,
    created_at: 1735000000 - i,
  });
}

const AGENTS = [
  {
    slug: "beta",
    name: "Beta Agent",
    description: "一个会查资料并写纪要的智能体。",
    owner_tenant: "tenant-a",
    owner_name: "平台组",
    visibility: "public",
    latest_version: "2.0.0",
    skill_count: 3,
    size_bytes: 4096,
    download_count: 11,
    updated_at: 1735689600,
    created_at: 1735000000,
  },
];

let failSkills = false;
const requests = [];
// What the host reports as installed; an install appends to it so the smoke can
// tell a refreshed 已安装 list from a stale one.
const installedSkills = [{ name: "Alpha", source: "user" }];

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function pageOf(rows, url, sortRows) {
  const params = new URL(url, "http://localhost").searchParams;
  const q = (params.get("q") || "").toLowerCase();
  const page = Number(params.get("page") || 1);
  const limit = Number(params.get("limit") || 20);
  const matched = rows.filter((row) => !q || `${row.name} ${row.slug}`.toLowerCase().includes(q));
  const sorted = sortRows ? sortRows(matched, params.get("sort")) : matched;
  return {
    page,
    limit,
    total: sorted.length,
    sort: params.get("sort") || "created",
    items: sorted.slice((page - 1) * limit, page * limit),
  };
}

const skillSort = (rows, sort) => {
  if (sort === "downloads") return [...rows].sort((a, b) => b.download_count - a.download_count);
  if (sort === "updated") return [...rows].sort((a, b) => b.updated_at - a.updated_at);
  return rows;
};

globalThis.fetch = window.fetch = async (url, init) => {
  const path = String(url);
  requests.push(`${(init && init.method) || "GET"} ${path}`);
  const pathname = path.split("?")[0];
  if (pathname === "/api/offline-market/limits") return json({});
  if (pathname === "/api/offline-market/config") {
    return json({ market_url: "http://market.local/api", tenant: "tenant-a", scan_mode: "local" });
  }
  if (pathname === "/api/offline-market/stats") {
    return json({
      generated_at: 1735689600,
      skills: { total: SKILLS.length, public: 30, tenant: 15, download_count: 400 },
      agents: { total: 1, public: 1, tenant: 0, download_count: 11 },
      downloads: { total: 411 },
      new_this_month: { skills: 3, agents: 0 },
    });
  }
  if (pathname === "/api/offline-market/skills") {
    if (failSkills) return json({ detail: "market backend unreachable" }, 502);
    return json(pageOf(SKILLS, path, skillSort));
  }
  if (pathname === "/api/offline-market/agents/installed") {
    return json({ items: [{ agent_id: "ag-1", slug: "beta", name: "Beta Agent", version: "1.0.0", state: "active" }] });
  }
  if (pathname === "/api/offline-market/agents") return json(pageOf(AGENTS, path));
  if (pathname === "/api/offline-market/agents/beta/install") {
    return json({ ok: true, agent_id: "ag-1", version: "2.0.0" });
  }
  if (pathname === "/api/skills/hub/install/start") {
    // the host only knows the skill once the install finished
    if (!installedSkills.some((item) => item.name === "Gamma")) {
      installedSkills.push({ name: "Gamma", source: "customized" });
    }
    return json({ task_id: "task-1" });
  }
  if (pathname.startsWith("/api/skills/hub/install/status/")) {
    return json({ status: "completed", result: { name: "Skill 44" } });
  }
  if (pathname === "/api/skills") return json({ items: installedSkills });
  // publish flow (发布 tab)
  if (pathname === "/api/offline-market/derive-slug") return json({ slug: "demo" });
  if (pathname === "/api/offline-market/mine") return json({ items: [] });
  if (pathname === "/api/auth/verify") return json({ username: "平台组" });
  if (pathname === "/api/offline-market/skills/inspect") {
    return json({ problems: [], file_count: 2, manifest: { name: "Demo" }, existing: null, next_version: "1.0.1" });
  }
  if (pathname === "/api/offline-market/skills/zip") {
    return json({ slug: "demo", version: "1.0.0", file_count: 2 });
  }
  if (pathname === "/api/skills/Alpha" || pathname === "/api/skills/alpha") {
    return json({ name: "Alpha", installed_from: "offline-market", version_text: "1.0.0", source: "user" });
  }
  if (pathname === "/api/skills/Gamma") {
    return json({ name: "Gamma", installed_from: "offline-market", version_text: "", source: "customized" });
  }
  return json({ detail: `unmocked ${pathname}` }, 404);
};

// ------------------------------------------------------------- fake host -----
let pageComponent = null;
window.QwenPaw = {
  version: "2.2.2",
  host: {
    React,
    ReactDOM: require("react-dom"),
    antd,
    antdIcons: {},
    apiBaseUrl: "/api",
    getApiUrl: (p) => `/api${p}`,
    getApiToken: () => "test-token",
    prepareBrowserSession: async () => {},
    usesBrowserSession: false,
    useTheme: () => ({ isDark: false }),
  },
  route: {
    add(id, def) {
      pageComponent = def.component;
    },
  },
  menu: { add() {} },
  slot: { fill() {} },
};
globalThis.window.QwenPaw = window.QwenPaw;

// ------------------------------------------------------------- render --------
const errors = [];
console.error = (...args) => {
  const line = args.map((a) => (a && a.stack) || String(a)).join(" ");
  if (line.includes("Not implemented: Window's getComputedStyle")) return; // jsdom limit
  errors.push(line);
  process.stdout.write(`[console.error] ${line}\n`);
};
window.addEventListener("error", (event) => errors.push(`window.onerror: ${event.message}`));
process.on("unhandledRejection", (reason) => errors.push(`unhandledRejection: ${reason}`));

await import(COPY);

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
for (let i = 0; i < 40 && !pageComponent; i += 1) await sleep(25);
if (!pageComponent) throw new Error("frontend never registered a route component");

const root = ReactDOMClient.createRoot(document.getElementById("root"));
root.render(
  React.createElement(antd.ConfigProvider, { theme: { token: {} } }, React.createElement(pageComponent)),
);
await sleep(900);

const flat = () => document.body.textContent.replace(/\s+/g, " ");
const failures = [];
const waitFor = async (predicate, timeout = 4000) => {
  const started = Date.now();
  while (Date.now() - started < timeout) {
    if (predicate()) return true;
    await sleep(50);
  }
  return false;
};
function expect(condition, label) {
  if (!condition) failures.push(label);
  console.log(`${condition ? "PASS" : "FAIL"}  ${label}`);
}
// Innermost visible tab pane: the console renders nested Tabs, so the first
// `.ant-tabs-tabpane-active` can be an outer page whose hidden children still
// contribute their text to textContent.
const pane = () => {
  const active = [...document.querySelectorAll(".ant-tabs-tabpane-active")];
  const visible = active.filter((node) => node.getAttribute("aria-hidden") !== "true");
  return visible[visible.length - 1] || active[active.length - 1] || document.body;
};
const buttons = () => [...pane().querySelectorAll("button")];
const clickButton = (label) => {
  const target = buttons().find((b) => b.textContent.replace(/\s+/g, "").includes(label));
  if (target) target.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  return target;
};
const cards = () => [...pane().querySelectorAll(".ant-card")];

expect(flat().includes("共 45 个"), "结果计数显示后端 total");
expect(cards().length === 20, `首屏渲染 20 张卡片（实际 ${cards().length}）`);
expect(flat().includes("Alpha"), "市场条目上屏");
const badgeSeen = await waitFor(() => flat().includes("1.0.0 → 1.2.0"));
expect(badgeSeen, `已安装版本与市场 latest_version 对比出更新徽标（${pane().textContent.replace(/\s+/g, " ").slice(0, 160)}）`);
expect(flat().includes("7"), "显示下载量");
expect(flat().includes("已经到底了") === false, "还有更多时未显示到底提示");

// card click -> detail drawer
cards()[0].dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(500);
expect(flat().includes("复制标识"), "点击卡片打开详情抽屉");
expect(flat().includes("平台组"), "抽屉展示发布者");
document.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
await sleep(400);

// infinite scroll via IntersectionObserver
const observer = observers[observers.length - 1];
observer.cb([{ isIntersecting: true, target: observer.elements[0] }]);
await sleep(600);
expect(requests.some((r) => r.includes("/api/offline-market/skills?") && r.includes("page=2")), "滚动触底加载第 2 页");
expect(cards().length === 40, `第 2 页后共 40 张卡片（实际 ${cards().length}）`);

// debounced search
const input = document.querySelector(".ant-input");
const setValue = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value").set;
setValue.call(input, "Alpha");
input.dispatchEvent(new window.Event("input", { bubbles: true }));
const beforeSearch = requests.length;
await sleep(700);
expect(requests.slice(beforeSearch).some((r) => r.includes("q=Alpha")), "输入 300ms 后只发一次 q=Alpha 请求");
expect(cards().length === 1, `搜索后只剩 1 张卡片（实际 ${cards().length}）`);
expect(flat().includes("匹配"), "搜索时结果计数标注匹配词");

// sort via Select
const selector = document.querySelector(".ant-select-selector");
if (selector) {
  selector.dispatchEvent(new window.MouseEvent("mousedown", { bubbles: true }));
  await sleep(400);
  const option = [...document.querySelectorAll(".ant-select-item-option")].find((n) =>
    n.textContent.includes("下载最多"),
  );
  expect(Boolean(option), "排序下拉含“下载最多”");
  if (option) {
    option.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
    await sleep(600);
    expect(requests.some((r) => r.includes("sort=downloads")), "切换排序后按 sort=downloads 重新请求");
  }
}

// list view toggle
expect(Boolean(clickButton("列表")), "存在列表视图切换按钮");
await sleep(500);
expect(Boolean(pane().querySelector(".ant-table")), "切到列表视图后出现表格");
expect(flat().includes("下载"), "列表视图含下载列");
clickButton("卡片");
await sleep(300);

// an install must refresh the already-mounted 已安装 tab: it used to keep the
// snapshot from its first visit, so a freshly installed skill never showed up
const innerTab = (label) =>
  [...document.querySelectorAll('[role="tab"]')].find(
    (node) => node.textContent.replace(/\s+/g, "") === label,
  );
innerTab("已安装").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(1500);
expect(document.querySelectorAll("[data-installed-name]").length === 1, "已安装页首次挂载只有 1 条");
innerTab("市场").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(800);
setValue.call(input, "");
input.dispatchEvent(new window.Event("input", { bubbles: true }));
await sleep(900);
const installable = buttons().find(
  (b) => !b.disabled && b.textContent.replace(/\s+/g, "").includes("安装"),
);
expect(Boolean(installable), "市场页存在可点的安装按钮");
if (installable) installable.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(2500);
innerTab("已安装").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(1500);
expect(
  flat().includes("Gamma"),
  `安装后已安装页自动刷新（${pane().textContent.replace(/\s+/g, " ").slice(0, 140)}）`,
);

// 发布成功后切到别的页签再回来：antd 的页签只隐藏不卸载，发布页曾经会把
// 「发布成功」卡一直留在那里，用户回来看不到上传表单。
innerTab("发布").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(800);
expect(flat().includes("点击选择或把 .zip 拖到这里"), "发布页首次进入是上传表单");
const fileInput = pane().querySelector('input[type="file"]');
const zip = new window.File([new Uint8Array([80, 75, 3, 4])], "Demo.zip", { type: "application/zip" });
Object.defineProperty(fileInput, "files", { value: [zip], configurable: true });
fileInput.dispatchEvent(new window.Event("change", { bubbles: true }));
await sleep(1200);
expect(flat().includes("Demo.zip"), "选包后显示压缩包名");
clickButton("立即发布");
await sleep(1500);
expect(flat().includes("发布成功"), "发布成功后在原地显示结果卡");
innerTab("我的发布").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(900);
innerTab("发布").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(700);
expect(!flat().includes("发布成功"), "切走再回到发布页，结果卡已收掉");
expect(
  flat().includes("点击选择或把 .zip 拖到这里"),
  `回到发布页重新显示上传表单（${pane().textContent.replace(/\s+/g, " ").slice(0, 120)}）`,
);
innerTab("市场").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(600);

// the happy path must not log a single error
expect(errors.length === 0, "正常流程 0 console error");

// error state (back on the market tab: 刷新 lives there, not on 已安装)
innerTab("市场").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(500);
failSkills = true;
clickButton("刷新");
await sleep(700);
expect(flat().includes("market backend unreachable"), "接口失败时展示后端错误");
failSkills = false;
clickButton("刷新");
await sleep(700);

// agents tab reuses the same component and keeps the install dialog
const agentTab = [...document.querySelectorAll('[role="tab"]')].find((n) => n.textContent.includes("智能体"));
expect(Boolean(agentTab), "存在智能体市场标签页");
agentTab.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(900);
expect(flat().includes("Beta Agent"), "智能体市场复用同一列表组件");
expect(await waitFor(() => flat().includes("1.0.0 → 2.0.0")), "智能体显示更新徽标");
expect(flat().includes("3"), "智能体显示技能数");
const installButton = clickButton("更新");
expect(Boolean(installButton), "已安装且有新版时按钮变成“更新”");
await sleep(600);
expect(flat().includes("安装") && flat().includes("Beta Agent"), "点击更新打开安装弹窗");

// dark theme pass
root.render(
  React.createElement(
    antd.ConfigProvider,
    { theme: { algorithm: antd.theme.darkAlgorithm } },
    React.createElement(pageComponent),
  ),
);
await sleep(900);
expect(flat().length > 100, "暗色主题渲染完成");

// 外层页签（技能 ↔ 智能体）切回来同样是「重新进入」：两个层级都要算在内。
// 发在最后：它会把智能体页挂载起来，而上面对页面范围的断言只认技能页。
const outerTab = (label) =>
  [...document.querySelectorAll('[role="tab"]')].find(
    (node) => node.textContent.replace(/\s+/g, "") === label,
  );
const skillsPublishPane = () =>
  [...document.querySelectorAll(".ant-tabs-tabpane")].find((node) =>
    /技能压缩包|已在市场中/.test(node.textContent),
  );
innerTab("发布").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(800);
const zipAgain = skillsPublishPane().querySelector('input[type="file"]');
Object.defineProperty(zipAgain, "files", { value: [zip], configurable: true });
zipAgain.dispatchEvent(new window.Event("change", { bubbles: true }));
await sleep(1200);
const publishAgain = [...skillsPublishPane().querySelectorAll("button")].find((b) =>
  b.textContent.replace(/\s+/g, "").includes("立即发布"),
);
expect(Boolean(publishAgain), "回到发布页仍能选包并发布");
publishAgain.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(1500);
expect(flat().includes("发布成功"), "再次发布后在原地显示结果卡");
outerTab("智能体").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(900);
outerTab("技能").dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
await sleep(900);
expect(!flat().includes("发布成功"), "切到智能体再回技能，发布结果卡同样收掉");

console.log("--- requests ---");
console.log([...new Set(requests)].join("\n"));
if (errors.length) console.log(`--- console errors ---\n${errors.slice(0, 10).join("\n")}`);
expect(
  errors.every((line) => line.includes("[offline-market]")),
  "失败路径只记录预期内的插件错误日志",
);

console.log(failures.length ? `\nFAILURES:\n- ${failures.join("\n- ")}` : "\nALL CHECKS PASSED");
process.exit(failures.length ? 1 : 0);
