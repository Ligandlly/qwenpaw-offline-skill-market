/**
 * 技能市场（内网） — frontend entry for QwenPaw 2.2.2.
 *
 * Loaded by the console as an ES module:
 *   GET /api/frontend_plugin/offline-market/files/frontend/market.js
 * Registers a sidebar entry + a page route through `window.QwenPaw`.
 * React / antd come from `window.QwenPaw.host`; there is no build step.
 *
 * The page only talks to our own plugin routes (`/api/offline-market/...`).
 * Installation is the host's job: `POST /api/skills/hub/install/start` with
 * `provider_key: "offline-market"`, then poll `/status/{task_id}`.
 */
const PLUGIN_ID = "offline-market";
const PLUGIN_VERSION = "0.1.2"; // keep in sync with plugin.json
const PLUGIN_LABEL = "技能市场（内网）";
const VISIBILITY_LABEL = { public: "公开", tenant: "仅本租户" };
const STATE_LABEL = { active: "已上架", withdrawn: "已撤回", taken_down: "已下架" };
const STATE_COLOR = { active: "green", withdrawn: "default", taken_down: "red" };

// ------------------------------------------------------------------ debug ---
// Silent by default. Turn it on without rebuilding anything, either way:
//   OfflineMarketDebug.enable()                  // lives until the tab closes
//   OfflineMarketDebug.enable()  + reload        // persisted in localStorage
//   localStorage.setItem("offline-market:debug", "1") + reload
//   window.QWENPAW_OFFLINE_MARKET_DEBUG = true   // set before the module loads
const DEBUG_STORAGE_KEY = "offline-market:debug";

function readDebugFlag() {
  try {
    const forced = window.QWENPAW_OFFLINE_MARKET_DEBUG;
    if (typeof forced === "boolean") return forced;
    return window.localStorage.getItem(DEBUG_STORAGE_KEY) === "1";
  } catch (err) {
    return false; // storage disabled (private mode, sandboxed frame)
  }
}

let debugOn = readDebugFlag();

const PREFIX = `[${PLUGIN_ID}]`;
const logger = {
  debug: (...args) => {
    if (debugOn) console.debug(`${PREFIX}·debug`, ...args);
  },
  info: (...args) => console.info(PREFIX, ...args),
  warn: (...args) => console.warn(PREFIX, ...args),
  error: (...args) => console.error(PREFIX, ...args),
};

// ANSI-shadow-ish figlet block; ASCII only so alignment survives any console.
const BANNER_ART = [
  "  ___  _____ _____ _     ___ _   _ _____ ",
  " / _ \\|  ___|  ___| |   |_ _| \\ | | ____|",
  "| | | | |_  | |_  | |    | ||  \\| |  _|  ",
  "| |_| |  _| |  _| | |___ | || |\\  | |___ ",
  " \\___/|_|   |_|   |_____|___|_| \\_|_____|",
  "",
  " __  __    _    ____  _  _______ _____ ",
  "|  \\/  |  / \\  |  _ \\| |/ / ____|_   _|",
  "| |\\/| | / _ \\ | |_) | ' /|  _|   | |  ",
  "| |  | |/ ___ \\|  _ <| . \\| |___  | |  ",
  "|_|  |_/_/   \\_\\_| \\_\\_|\\_\\_____| |_|  ",
];

function printBanner() {
  console.log(
    `%c${BANNER_ART.join("\n")}`,
    "color:#7c5cff;font-family:ui-monospace,Menlo,Consolas,monospace;font-size:11px;line-height:1.15",
  );
  console.log(
    `%c🧩 ${PLUGIN_ID}%c v${PLUGIN_VERSION} · ${PLUGIN_LABEL} · QwenPaw frontend plugin`,
    "background:#7c5cff;color:#fff;padding:1px 6px;border-radius:3px;font-weight:600",
    "color:#7c5cff;font-weight:600",
  );
  console.log(
    `%c调试日志：%cOfflineMarketDebug.enable()%c（刷新后仍生效；关闭用 disable()）`,
    "color:#888",
    "color:#7c5cff;font-weight:600",
    "color:#888",
  );
}

window.OfflineMarketDebug = {
  version: PLUGIN_VERSION,
  get enabled() {
    return debugOn;
  },
  enable(persist = true) {
    debugOn = true;
    if (persist) {
      try {
        window.localStorage.setItem(DEBUG_STORAGE_KEY, "1");
      } catch (err) {
        logger.warn("无法写入 localStorage，debug 仅本次会话有效", err);
      }
    }
    printBanner();
    logger.info(`debug 已开启（持久化：${persist ? "是" : "否"}）`);
    return true;
  },
  disable(persist = true) {
    debugOn = false;
    if (persist) {
      try {
        window.localStorage.removeItem(DEBUG_STORAGE_KEY);
      } catch (err) {
        // nothing to clean up
      }
    }
    logger.info("debug 已关闭");
    return false;
  },
};

printBanner();
logger.debug(
  `模块加载 version=${PLUGIN_VERSION} debug=${debugOn} host=${Boolean(window.QwenPaw)}`,
);

function whenHost(callback, deadlineMs = 15000) {
  const started = Date.now();
  let attempts = 0;
  logger.debug(`等待宿主 window.QwenPaw（上限 ${deadlineMs}ms）`);
  const tick = () => {
    const paw = window.QwenPaw;
    attempts += 1;
    if (paw && paw.host && paw.host.React && paw.menu && paw.route) {
      logger.debug(
        `宿主就绪：${Date.now() - started}ms / ${attempts} 次轮询`,
        { version: paw.version || "?", hasAntd: Boolean(paw.host.antd) },
      );
      callback(paw);
      return;
    }
    if (Date.now() - started > deadlineMs) {
      logger.error(
        `window.QwenPaw 在 ${deadlineMs}ms 内不可用，页面未注册`,
        { attempts, hostKeys: paw ? Object.keys(paw) : null },
      );
      return;
    }
    setTimeout(tick, 50);
  };
  tick();
}

function apiUrl(path) {
  const host = window.QwenPaw && window.QwenPaw.host;
  // NOTE: getApiUrl() already prepends "/api" (console/src/api/config.ts).
  if (host && typeof host.getApiUrl === "function") {
    const url = host.getApiUrl(path);
    logger.debug(`apiUrl(host.getApiUrl) ${path} -> ${url}`);
    return url;
  }
  const url = `/api${path.startsWith("/") ? path : `/${path}`}`;
  logger.debug(`apiUrl(本地兜底) ${path} -> ${url}`);
  return url;
}

// The console always sends the selected agent; without it the host falls back
// to `agents.active_agent` and may cold-start a different workspace, which is
// where `/api/skills*` calls can sit for a long time.
function selectedAgentId() {
  const host = window.QwenPaw && window.QwenPaw.host;
  if (host && typeof host.getSelectedAgentId === "function") {
    try {
      const value = host.getSelectedAgentId();
      if (value) return value;
    } catch (err) {
      // fall through to storage
    }
  }
  try {
    const raw =
      sessionStorage.getItem("qwenpaw-agent-storage") ||
      localStorage.getItem("qwenpaw-agent-storage");
    if (raw) {
      const stored = JSON.parse(raw);
      const value = stored && stored.state && stored.state.selectedAgent;
      if (value) return value;
    }
    return localStorage.getItem("qwenpaw-last-used-agent") || "";
  } catch (err) {
    return "";
  }
}

// A fetch without a deadline can outlive the page: behind the hub proxy a
// stalled response never settles, and the caller keeps its spinner forever.
const REQUEST_TIMEOUT_MS = 15000;

async function request(path, init) {
  const method = (init && init.method) || "GET";
  const url = apiUrl(path);
  const host = window.QwenPaw && window.QwenPaw.host;
  const token = host && typeof host.getApiToken === "function" ? host.getApiToken() : "";
  const headers = { accept: "application/json", ...((init && init.headers) || {}) };
  if (token) headers.Authorization = `Bearer ${token}`;
  const agent = selectedAgentId();
  if (agent) headers["X-Agent-Id"] = agent;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  const started = Date.now();
  logger.debug(`→ ${method} ${path}`, {
    url,
    agent: agent || "(未选中，宿主用 active_agent)",
    token: token ? "已附带" : "无",
    timeout: REQUEST_TIMEOUT_MS,
  });
  let response;
  let text;
  try {
    response = await fetch(url, {
      credentials: "same-origin",
      ...init,
      headers,
      signal: controller.signal,
    });
    // read the body under the same deadline: a body that stops mid-stream would
    // otherwise hang here with the abort timer already cleared
    text = await response.text();
  } catch (err) {
    const elapsed = Date.now() - started;
    if (err && err.name === "AbortError") {
      logger.error(`✗ ${method} ${path} 超时 ${elapsed}ms（AbortController）`);
      throw new Error(`请求超时（${REQUEST_TIMEOUT_MS / 1000}s）：${path}`);
    }
    logger.error(`✗ ${method} ${path} 网络/传输失败 ${elapsed}ms`, err);
    throw err;
  } finally {
    clearTimeout(timer);
  }
  const elapsed = Date.now() - started;
  let body = {};
  try {
    body = text ? JSON.parse(text) : {};
  } catch (err) {
    logger.warn(`响应不是 JSON（${text.length} 字节），按原文透传`, { path, status: response.status });
    body = { raw: text };
  }
  logger.debug(
    `← ${method} ${path} ${response.status} ${elapsed}ms`,
    text.length > 2000 ? `… ${text.length} 字节响应体（过长，已省略）` : body,
  );
  if (!response.ok) {
    const detail = (body && body.detail) || {};
    const error = new Error(detail.message || `HTTP ${response.status}`);
    error.code = detail.code || `http_${response.status}`;
    error.status = response.status;
    logger.warn(`✗ ${method} ${path} ${response.status} [${error.code}] ${error.message}`);
    throw error;
  }
  return body;
}

const getJson = (path) => request(path);
const postJson = (path, payload) =>
  request(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload || {}),
  });

function readFileAsBytes(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(new Error(`无法读取 ${file.name}`));
    reader.readAsArrayBuffer(file);
  });
}

/** Cheap client-side mirror of the backend gate; the backend still decides. */
function precheckArchive(archive, limits) {
  if (!archive) return "请选择技能压缩包（.zip）";
  if (!/\.zip$/i.test(archive.name)) return "只支持 .zip 压缩包";
  // Zip headers add a little overhead on top of the bundle itself; the backend
  // checks the real file count and uncompressed sizes.
  if (limits && archive.bytes.byteLength > limits.max_bundle_bytes + 1024 * 1024)
    return `压缩包超过后端总大小上限 ${limits.max_bundle_bytes} 字节`;
  return null;
}

whenHost((paw) => {
  const React = paw.host.React;
  const antd = paw.host.antd || {};
  const h = React.createElement;
  const { useEffect, useState, useCallback, useMemo } = React;

  // antd is expected; the fallbacks keep the page usable in a stripped console.
  const Card = antd.Card || ((props) => h("section", props, props.children));
  const Alert = antd.Alert;
  const Tag = antd.Tag || ((props) => h("span", props, props.children));
  const Button = antd.Button || ((props) => h("button", props, props.children));
  const Space = antd.Space || ((props) => h("span", props, props.children));
  const Input = antd.Input;
  const Select = antd.Select;
  const Tabs = antd.Tabs;
  const Table = antd.Table;
  const Empty = antd.Empty || (() => h("div", null, "暂无数据"));
  const Spin = antd.Spin || (() => h("div", null, "加载中…"));
  const Typography = antd.Typography || {};
  const Text = Typography.Text || ((props) => h("span", props, props.children));
  const Title = Typography.Title || ((props) => h("h3", props, props.children));
  const Paragraph = Typography.Paragraph || ((props) => h("p", props, props.children));

  // ------------------------------------------------------------- identity ---

  let currentUserNamePromise = null;

  // The market only ever authenticates us as `personal-<hex>`, which reads badly
  // as 发布者. The console's own login name is reachable from the page:
  // /api/auth/verify answers {username} in hub mode (the hub serves that path)
  // and standalone alike, authenticated with the same host token the console
  // uses. Purely cosmetic — ownership on the market is the tenant header.
  // A failure is not cached, so a publish after signing in can still pick it up.
  function currentUserName() {
    if (!currentUserNamePromise) {
      currentUserNamePromise = (async () => {
        try {
          const body = await request("/auth/verify");
          const name = typeof body.username === "string" ? body.username.trim() : "";
          logger.debug(`当前登录用户名 username=${name || "(空)"}`);
          return name;
        } catch (err) {
          currentUserNamePromise = null;
          logger.warn("取不到当前登录用户名，发布者只会是租户 id", err);
          return "";
        }
      })();
    }
    return currentUserNamePromise;
  }

  function useCurrentUserName() {
    const [name, setName] = useState("");
    useEffect(() => {
      let alive = true;
      currentUserName().then((value) => {
        if (alive) setName(value);
      });
      return () => {
        alive = false;
      };
    }, []);
    return name;
  }

  // ---------------------------------------------------------------- shared ---

  function useMarketData() {
    const [config, setConfig] = useState(null);
    const [limits, setLimits] = useState(null);
    const [error, setError] = useState(null);

    const reloadConfig = useCallback(async () => {
      logger.debug("加载 /offline-market/config + /limits");
      try {
        const config = await getJson("/offline-market/config");
        setConfig(config);
        logger.debug("config", config);
        if ((config.problems || []).length) {
          logger.warn("市场不可用（闸门/配置）", config.problems);
        }
      } catch (err) {
        logger.error("config 加载失败", err);
        setError(err.message);
      }
      try {
        const limits = await getJson("/offline-market/limits");
        setLimits(limits);
        logger.debug("limits", limits);
      } catch (err) {
        // limits are advisory only; the backend enforces them
        logger.warn("limits 未取到（仅影响前端预检）", err);
        setLimits(null);
      }
    }, []);

    useEffect(() => {
      reloadConfig();
    }, [reloadConfig]);

    return { config, limits, error, setError, reloadConfig };
  }

  function StatusBanner({ config, error }) {
    if (error) {
      return h(Alert, { type: "error", showIcon: true, message: "市场请求失败", description: error });
    }
    if (!config) return null;
    const problems = config.problems || [];
    if (!problems.length) {
      return h(Alert, {
        type: "success",
        showIcon: true,
        message: "市场可用",
        description: `后端 ${config.market_url}｜租户 ${config.tenant}｜扫描模式 ${config.scan_mode}`,
      });
    }
    return h(Alert, {
      type: "warning",
      showIcon: true,
      message: "市场暂不可用",
      description: h(
        "ul",
        { style: { margin: 0, paddingLeft: 18 } },
        problems.map((problem, index) => h("li", { key: index }, problem)),
      ),
    });
  }

  /** Install through the host and follow its task. */
  function useInstaller(onDone) {
    const [task, setTask] = useState(null);
    const [busySlug, setBusySlug] = useState("");

    const cancel = useCallback(async () => {
      if (!task || !task.taskId) return;
      logger.debug(`取消安装任务 ${task.taskId}（${task.slug}）`);
      try {
        await postJson(`/skills/hub/install/cancel/${task.taskId}`);
        setTask(null);
        setBusySlug("");
        logger.info(`安装已取消：${task.slug}`);
        onDone && onDone({ type: "warning", text: "已取消安装" });
      } catch (err) {
        logger.error("取消安装失败", err);
        onDone && onDone({ type: "error", text: err.message });
      }
    }, [task, onDone]);

    const install = useCallback(
      async (item) => {
        const started = Date.now();
        logger.info(`开始安装 ${item.slug}@${item.latest_version || "-"}`);
        logger.debug("安装参数", {
          provider_key: PLUGIN_ID,
          bundle_url: item.install_url,
          version: item.latest_version || "(最新)",
        });
        setBusySlug(item.slug);
        setTask(null);
        try {
          const startedTask = await postJson("/skills/hub/install/start", {
            provider_key: PLUGIN_ID,
            bundle_url: item.install_url,
            version: item.latest_version || "",
          });
          const taskId = startedTask.task_id;
          logger.debug(`宿主任务已创建 task_id=${taskId} status=${startedTask.status}`);
          setTask({ taskId, slug: item.slug, status: startedTask.status });
          for (let attempt = 0; attempt < 240; attempt += 1) {
            await new Promise((resolve) => setTimeout(resolve, 500));
            const status = await getJson(`/skills/hub/install/status/${taskId}`);
            logger.debug(`轮询 #${attempt + 1} ${taskId} -> ${status.status}`);
            setTask({ taskId, slug: item.slug, status: status.status });
            if (status.status === "completed") {
              setBusySlug("");
              setTask(null);
              logger.info(`安装完成 ${item.slug}（${Date.now() - started}ms）`, status.result || null);
              onDone &&
                onDone({
                  type: "success",
                  text: `已安装 ${(status.result && status.result.name) || item.slug}`,
                });
              return;
            }
            if (status.status === "failed" || status.status === "cancelled") {
              setBusySlug("");
              setTask(null);
              logger.error(`安装${status.status === "failed" ? "失败" : "被取消"}：${item.slug}`, status);
              onDone && onDone({ type: "error", text: status.error || status.status });
              return;
            }
          }
          setBusySlug("");
          setTask(null);
          logger.error(`安装轮询超时（120s）：${item.slug}`);
          onDone && onDone({ type: "warning", text: "安装超时，请到技能页查看" });
        } catch (err) {
          setBusySlug("");
          setTask(null);
          logger.error(`安装失败：${item.slug}`, err);
          onDone && onDone({ type: "error", text: err.message });
        }
      },
      [onDone],
    );

    return { install, cancel, busySlug, task };
  }

  // ------------------------------------------------------------------ tabs ---

  function MarketTab({ limits, onNotice, refreshToken, tenant }) {
    const [items, setItems] = useState([]);
    const [query, setQuery] = useState("");
    const [loading, setLoading] = useState(false);
    const installer = useInstaller(onNotice);

    const load = useCallback(
      async (value) => {
        setLoading(true);
        logger.debug(`市场列表加载 q=${JSON.stringify(value || "")}`);
        try {
          const body = await getJson(
            `/offline-market/skills?q=${encodeURIComponent(value || "")}&limit=50`,
          );
          const items = body.items || [];
          setItems(items);
          logger.debug(
            `市场列表 ${items.length} 条`,
            items.map(
              (item) =>
                `${item.slug}@${item.latest_version || "-"}${item.owner_name ? `(${item.owner_name})` : ""}`,
            ),
          );
        } catch (err) {
          logger.error("市场列表加载失败", err);
          onNotice({ type: "error", text: err.message });
          setItems([]);
        } finally {
          setLoading(false);
        }
      },
      [onNotice],
    );

    useEffect(() => {
      // Mount, and once more after a successful publish (refreshToken), so the
      // new version shows up without the user pressing 搜索 or 全部.
      // `query` is deliberately not a dependency: typing must not refetch.
      load(query);
    }, [load, refreshToken]);

    const columns = useMemo(
      () => [
        {
          title: "技能",
          dataIndex: "name",
          render: (_, row) =>
            h(
              "div",
              null,
              h("div", null, h(Text, { strong: true }, row.name)),
              h(Text, { type: "secondary", code: true }, `${row.slug}@${row.latest_version || "-"}`),
            ),
        },
        {
          title: "可见性",
          dataIndex: "visibility",
          width: 110,
          render: (value) =>
            h(Tag, { color: value === "public" ? "blue" : "gold" }, VISIBILITY_LABEL[value] || value),
        },
        {
          title: "发布者",
          dataIndex: "owner_name",
          width: 160,
          // `owner_name` is whatever the publisher's runtime reported; skills
          // published before that existed (or by a tenant that never sent one)
          // fall back to the tenant id the market really knows.
          render: (_, row) => {
            const label = row.owner_name || row.owner_tenant || "-";
            const mine = Boolean(tenant) && row.owner_tenant === tenant;
            return h("span", { title: row.owner_tenant || "" }, mine ? `${label}（我）` : label);
          },
        },
        { title: "版本数", dataIndex: "versions", width: 90 },
        {
          title: "操作",
          width: 150,
          render: (_, row) =>
            h(
              Button,
              {
                type: "primary",
                size: "small",
                loading: installer.busySlug === row.slug,
                disabled: Boolean(installer.busySlug) && installer.busySlug !== row.slug,
                onClick: () => installer.install(row),
              },
              "安装",
            ),
        },
      ],
      [installer],
    );

    return h(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: 12 } },
      h(
        Space,
        null,
        Input
          ? h(Input, {
              placeholder: "搜索技能名或 slug",
              value: query,
              allowClear: true,
              style: { width: 260 },
              onChange: (event) => setQuery(event.target.value),
              onPressEnter: () => load(query),
            })
          : null,
        h(Button, { onClick: () => load(query) }, "搜索"),
        h(Button, { onClick: () => load("") }, "全部"),
        installer.task
          ? h(Button, { danger: true, onClick: installer.cancel }, "取消安装")
          : null,
      ),
      loading
        ? h(Spin)
        : Table
          ? h(Table, {
              rowKey: "slug",
              size: "small",
              pagination: false,
              dataSource: items,
              columns,
              locale: { emptyText: h(Empty, { description: "还没有技能" }) },
            })
          : h(
              "div",
              null,
              items.map((item) => h(Card, { key: item.slug, size: "small" }, item.name)),
            ),
    );
  }

  function MineTab({ onNotice, refreshToken }) {
    const [items, setItems] = useState([]);
    const [loading, setLoading] = useState(false);

    const load = useCallback(async () => {
      setLoading(true);
      try {
        const body = await getJson("/offline-market/mine");
        const items = body.items || [];
        setItems(items);
        logger.debug(
          `我的发布 ${items.length} 条`,
          items.map((row) => `${row.slug}@${row.latest_version || "-"}${row.takedown_at ? "(已下架)" : row.withdrawn_at ? "(已撤回)" : ""}`),
        );
      } catch (err) {
        logger.error("我的发布加载失败", err);
        onNotice({ type: "error", text: err.message });
      } finally {
        setLoading(false);
      }
    }, [onNotice]);

    useEffect(() => {
      load();
    }, [load, refreshToken]);

    const act = useCallback(
      async (row, action) => {
        logger.info(`${action === "withdraw" ? "撤回" : "恢复"} ${row.slug}`);
        try {
          await postJson(`/offline-market/skills/${encodeURIComponent(row.slug)}/${action}`);
          logger.debug(`${action} 成功：${row.slug}`);
          onNotice({
            type: "success",
            text: action === "withdraw" ? `已撤回 ${row.slug}` : `已恢复 ${row.slug}`,
          });
          load();
        } catch (err) {
          logger.error(`${action} 失败：${row.slug}`, err);
          onNotice({ type: "error", text: err.message });
        }
      },
      [load, onNotice],
    );

    const stateOf = (row) => (row.takedown_at ? "taken_down" : row.withdrawn_at ? "withdrawn" : "active");

    const columns = [
      {
        title: "技能",
        render: (_, row) =>
          h(
            "div",
            null,
            h("div", null, h(Text, { strong: true }, row.name)),
            h(Text, { type: "secondary", code: true }, `${row.slug}@${row.latest_version || "-"}`),
          ),
      },
      {
        title: "可见性",
        dataIndex: "visibility",
        width: 110,
        render: (value) => h(Tag, { color: value === "public" ? "blue" : "gold" }, VISIBILITY_LABEL[value] || value),
      },
      {
        title: "状态",
        width: 120,
        render: (_, row) => {
          const state = stateOf(row);
          return h(Tag, { color: STATE_COLOR[state] }, STATE_LABEL[state]);
        },
      },
      { title: "版本数", dataIndex: "versions", width: 90 },
      {
        title: "操作",
        width: 170,
        render: (_, row) => {
          const state = stateOf(row);
          if (state === "taken_down") {
            return h(Text, { type: "secondary" }, "管理员已下架，无法自助恢复");
          }
          return h(
            Button,
            {
              size: "small",
              danger: state === "active",
              onClick: () => act(row, state === "active" ? "withdraw" : "restore"),
            },
            state === "active" ? "撤回" : "恢复上架",
          );
        },
      },
    ];

    return loading
      ? h(Spin)
      : Table
        ? h(Table, {
            rowKey: "slug",
            size: "small",
            pagination: false,
            dataSource: items,
            columns,
            locale: { emptyText: h(Empty, { description: "还没有发布过技能" }) },
          })
        : h("div", null, items.length, " 个技能");
  }

  function PublishTab({ limits, onNotice, onPublished }) {
    const [name, setName] = useState("");
    const [slug, setSlug] = useState("");
    const [version, setVersion] = useState("1.0.0");
    const [visibility, setVisibility] = useState("public");
    const [archive, setArchive] = useState(null);
    const [fileError, setFileError] = useState(null);
    const [busy, setBusy] = useState(false);
    const publisherName = useCurrentUserName();

    const onPick = useCallback(async (event) => {
      const picked = (event.target.files || [])[0];
      event.target.value = ""; // allow picking the same file again
      if (!picked) return;
      try {
        const bytes = await readFileAsBytes(picked);
        logger.debug(`已选择压缩包 ${picked.name} ${bytes.byteLength} 字节`, {
          lastModified: picked.lastModified,
        });
        setArchive({ name: picked.name, bytes });
        setFileError(null);
        if (!name) setName(picked.name.replace(/\.zip$/i, ""));
      } catch (err) {
        logger.error("读取压缩包失败", err);
        setFileError(err.message);
      }
    }, [name]);

    const submit = useCallback(async () => {
      const problem = precheckArchive(archive, limits);
      if (problem) {
        logger.warn(`发布预检未通过：${problem}`);
        setFileError(problem);
        return;
      }
      setBusy(true);
      setFileError(null);
      const started = Date.now();
      try {
        const fallback = archive.name.replace(/\.zip$/i, "");
        const params = new URLSearchParams({
          name: name.trim() || fallback || "untitled",
          version: version.trim(),
          visibility,
        });
        if (slug.trim()) params.set("slug", slug.trim());
        // Labels the listing; the market stores it next to our tenant id.
        const publisher = await currentUserName();
        if (publisher) params.set("publisher_name", publisher);
        logger.info(
          `发布 ${params.get("name")}@${params.get("version")}（${visibility}，发布者 ${
            publisher || "(未取到)"
          }）`,
        );
        const body = await request(`/offline-market/skills/zip?${params}`, {
          method: "POST",
          headers: { "content-type": "application/zip" },
          body: archive.bytes,
        });
        logger.info(
          `发布成功 ${body.slug}@${body.version}，${body.file_count} 个文件（${Date.now() - started}ms）`,
        );
        onNotice({
          type: "success",
          text: `已发布 ${body.slug}@${body.version}（${body.file_count} 个文件）`,
        });
        setArchive(null);
        onPublished && onPublished();
      } catch (err) {
        logger.error(`发布失败（${Date.now() - started}ms）`, err);
        setFileError(`${err.code ? `[${err.code}] ` : ""}${err.message}`);
      } finally {
        setBusy(false);
      }
    }, [archive, limits, name, slug, version, visibility, onNotice, onPublished]);

    return h(
      Card,
      { size: "small", title: "发布技能" },
      h(
        "div",
        { style: { display: "grid", gap: 10, maxWidth: 720 } },
        h(
          Space,
          { wrap: true },
          h(Text, null, "名称"),
          Input
            ? h(Input, {
                value: name,
                placeholder: "例如：周报助手",
                style: { width: 220 },
                onChange: (e) => setName(e.target.value),
              })
            : null,
          h(Text, null, "slug（可选）"),
          Input
            ? h(Input, {
                value: slug,
                placeholder: "默认由名称推导",
                style: { width: 200 },
                onChange: (e) => setSlug(e.target.value),
              })
            : null,
        ),
        h(
          Space,
          { wrap: true },
          h(Text, null, "版本"),
          Input
            ? h(Input, {
                value: version,
                placeholder: "1.0.0",
                style: { width: 140 },
                onChange: (e) => setVersion(e.target.value),
              })
            : null,
          h(Text, null, "可见性"),
          Select
            ? h(Select, {
                value: visibility,
                style: { width: 160 },
                onChange: setVisibility,
                options: [
                  { value: "public", label: "公开（全部租户）" },
                  { value: "tenant", label: "仅本租户" },
                ],
              })
            : null,
        ),
        h(
          "div",
          null,
          h("input", {
            type: "file",
            accept: ".zip,application/zip,application/x-zip-compressed",
            onChange: onPick,
            style: { color: "inherit" },
          }),
          h(
            Paragraph,
            { type: "secondary", style: { margin: "6px 0 0" } },
            "上传技能压缩包（.zip）：根目录（或压缩包里唯一的顶层目录）必须包含 SKILL.md；" +
              (limits
                ? `后端限额：${limits.max_files} 个文件 / 单文件 ${limits.max_file_bytes} 字节 / 合计 ${limits.max_bundle_bytes} 字节。`
                : "限额未取到，后端仍会校验。"),
          ),
        ),
        archive
          ? h(
              Space,
              { wrap: true },
              h(Text, { strong: true }, "已选择："),
              h(Tag, null, archive.name),
              h(Text, { type: "secondary" }, `${(archive.bytes.byteLength / 1024).toFixed(1)} KB`),
            )
          : null,
        fileError ? h(Alert, { type: "error", showIcon: true, message: fileError }) : null,
        h(
          Text,
          { type: "secondary" },
          publisherName
            ? `发布者：${publisherName}（取当前登录账号，市场列表就显示它）`
            : "发布者：租户 id（console 未提供登录用户名）",
        ),
        h(
          Space,
          null,
          h(Button, { type: "primary", loading: busy, onClick: submit }, "立即发布"),
          h(Button, { onClick: () => setArchive(null) }, "清空压缩包"),
        ),
      ),
      h(
        Paragraph,
        { type: "secondary", style: { marginTop: 12, marginBottom: 0 } },
        "版本不可变：同一个 slug + 版本只能发布一次；下架与审核由管理员在市场后台处理。",
      ),
    );
  }

  /**
   * 已安装：宿主的 /skills 列表只给 `source`，且对 hub 安装一律是
   * "customized"，拿不到来源。真正能认出"来自本市场"的只有逐条的
   * /skills/{name} 详情里的 `installed_from`。宿主的安装目录名是
   * 展示名（不是我们的 slug），所以再用市场列表建立 name -> slug 映射，
   * 才能查 /status 拿到已下架状态。
   *
   * 只在非 builtin 的技能里查详情：builtin 是宿主自带的，不可能来自本市场。
   * 逐条详情在 hub 代理后面单条就要 2~3 秒，所以按批查并边查边渲染，
   * 不再用一个大 Spin 等全部请求。
   */
  const DETAIL_BATCH = 4;

  function InstalledTab({ onNotice, refreshToken }) {
    const [rows, setRows] = useState([]);
    const [loading, setLoading] = useState(false);

    const load = useCallback(async () => {
      setLoading(true);
      setRows([]);
      try {
        const listed = await getJson("/skills");
        const skills = Array.isArray(listed) ? listed : listed.items || [];

        const candidates = skills.filter((item) => item.source !== "builtin");
        if (!candidates.length) return;

        const fromMarket = [];
        logger.debug(`已安装：非 builtin 技能 ${candidates.length} 个，开始拉详情`);
        for (let index = 0; index < candidates.length; index += DETAIL_BATCH) {
          const batch = candidates.slice(index, index + DETAIL_BATCH);
          logger.debug(
            `详情批次 ${index / DETAIL_BATCH + 1}：${batch.map((item) => item.name).join(", ")}`,
          );
          const details = await Promise.all(
            batch.map((item) =>
              // one broken skill must not hide the rest of the list
              getJson(`/skills/${encodeURIComponent(item.name)}`).catch(() => null),
            ),
          );
          details.forEach((detail) => {
            if (detail && detail.installed_from === PLUGIN_ID) fromMarket.push(detail);
          });
          logger.debug(`来自本市场的技能累计 ${fromMarket.length} 个`);
          setRows(
            fromMarket.map((item) => ({
              ...item,
              market_slug: "",
              market_state: "",
              market_reason: null,
            })),
          );
        }
        if (!fromMarket.length) return;

        // display name -> {slug, state}; the backend's lookup also returns
        // withdrawn and taken-down skills, which the market listing hides.
        const byName = {};
        try {
          const lookup = await getJson(
            `/offline-market/lookup?names=${encodeURIComponent(
              fromMarket.map((item) => item.name).join(","),
            )}`,
          );
          (lookup.items || []).forEach((item) => {
            byName[item.name] = item;
          });
        } catch (err) {
          // fall through: every row renders as "unknown"
          logger.warn("lookup 失败，已安装页统一显示未知状态", err);
        }

        const rows = fromMarket.map((item) => {
          const match = byName[item.name];
          return {
            ...item,
            market_slug: match ? match.slug : "",
            market_state: match ? match.state : "unknown",
            market_reason: match ? match.takedown_reason : null,
          };
        });
        setRows(rows);
        logger.debug(
          `已安装：来自本市场 ${rows.length} 个`,
          rows.map((row) => `${row.name} -> ${row.market_slug || "?"}/${row.market_state}`),
        );
      } catch (err) {
        logger.error("已安装列表加载失败", err);
        onNotice({ type: "error", text: err.message });
      } finally {
        setLoading(false);
      }
    }, [onNotice]);

    useEffect(() => {
      load();
    }, [load, refreshToken]);

    const columns = [
      {
        title: "已安装",
        dataIndex: "name",
        // data attributes keep the verified selectors stable for e2e
        render: (value, row) =>
          h("span", { "data-installed-name": value, "data-market-slug": row.market_slug || "" }, value),
      },
      { title: "描述", dataIndex: "description", ellipsis: true },
      {
        title: "市场状态",
        width: 240,
        render: (_, row) => {
          if (row.market_state === "taken_down") {
            return h(
              "span",
              null,
              h(Tag, { color: "red" }, "已下架"),
              row.market_reason ? h(Text, { type: "secondary" }, ` ${row.market_reason}`) : null,
            );
          }
          if (row.market_state === "withdrawn") return h(Tag, { color: "default" }, "发布者已撤回");
          if (row.market_state === "active") return h(Tag, { color: "green" }, "正常");
          if (row.market_state === "unknown") return h(Tag, null, "市场中已不存在");
          if (!row.market_state) return h(Text, { type: "secondary" }, "查询中…");
          return h(Tag, null, "—");
        },
      },
    ];

    // The table renders as data arrives; the spinner only covers the very first
    // list call, never the whole detail fan-out.
    if (loading && !rows.length) return h(Spin);
    return Table
      ? h(Table, {
          rowKey: (row) => `${row.name}:${row.last_updated || ""}`,
          size: "small",
          loading,
          pagination: false,
          dataSource: rows,
          columns,
          locale: { emptyText: h(Empty, { description: "还没有从本市场安装过技能" }) },
        })
      : h("div", null, rows.length, " 个已安装");
  }

  // ------------------------------------------------------------------ page ---

  function MarketPage() {
    const { config, limits, error, setError } = useMarketData();
    const [notice, setNotice] = useState(null);
    const [refreshToken, setRefreshToken] = useState(0);
    // Bumped only by a publish: refreshes the market list once without the
    // spinner flash that a tab switch would cause.
    const [marketToken, setMarketToken] = useState(0);

    const onNotice = useCallback((value) => {
      logger.debug(`通知 [${value.type}] ${value.text}`);
      setNotice(value);
      if (value && value.type === "success") {
        setTimeout(() => setNotice(null), 6000);
      }
    }, []);

    const onPublished = useCallback(() => {
      setRefreshToken((value) => value + 1);
      setMarketToken((value) => value + 1);
    }, []);

    const tabs = [
      {
        key: "market",
        label: "市场",
        children: h(MarketTab, {
          limits,
          onNotice,
          refreshToken: marketToken,
          tenant: (config && config.tenant) || "",
        }),
      },
      {
        key: "publish",
        label: "发布",
        children: h(PublishTab, { limits, onNotice, onPublished }),
      },
      {
        key: "mine",
        label: "我的发布",
        children: h(MineTab, { onNotice, refreshToken }),
      },
      {
        key: "installed",
        label: "已安装",
        children: h(InstalledTab, { onNotice, refreshToken }),
      },
    ];

    return h(
      "div",
      { style: { padding: 16, display: "flex", flexDirection: "column", gap: 12 } },
      h(Title, { level: 4, style: { margin: 0 } }, "技能市场（内网）"),
      h(StatusBanner, { config, error }),
      notice
        ? h(Alert, { type: notice.type, showIcon: true, message: notice.text, closable: true })
        : null,
      Tabs
        ? h(Tabs, {
            items: tabs,
            defaultActiveKey: "market",
            // A tab switch must not refetch: the pane mounts on first visit and
            // refreshing here used to run the whole N+1 fan-out twice.
            onChange: () => setError(null),
          })
        : h("div", null, tabs.map((tab) => h("section", { key: tab.key }, tab.children))),
    );
  }

  logger.debug("注册路由", {
    id: "offline-market.page",
    path: "/offline-market",
    scope: "global",
  });
  paw.route.add(PLUGIN_ID, {
    id: "offline-market.page",
    path: "/offline-market",
    component: MarketPage,
    meta: {
      domain: "extensions",
      title: "技能市场（内网）",
      description: "离线技能市场",
      scope: "global",
      layout: "standard",
      navigation: "rail",
    },
  });

  // Sidebar: right after the built-in marketplace, so both are equally visible.
  logger.debug("注册侧边栏菜单", { location: "primary.agentScoped", after: "core.marketplace" });
  paw.menu.add(PLUGIN_ID, {
    id: "offline-market.menu",
    location: "primary.agentScoped",
    after: "core.marketplace",
    order: 16,
    label: "技能市场（内网）",
    icon: "🧩",
    route: "offline-market.page",
  });

  logger.info(
    `frontend registered（路由 /offline-market，菜单 ${PLUGIN_LABEL}，debug=${debugOn ? "开" : "关"}）`,
  );
});
