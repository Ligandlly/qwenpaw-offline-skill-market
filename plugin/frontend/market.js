/**
 * 内网市场 — frontend entry for QwenPaw 2.2.2.
 *
 * Loaded by the console as an ES module:
 *   GET /api/frontend_plugin/offline-market/files/frontend/market.js
 * Registers a sidebar entry + a page route through `window.QwenPaw`.
 * React / antd come from `window.QwenPaw.host`; there is no build step.
 *
 * The page has three top-level tabs: 技能 (host-native install flow), 智能体
 * (snapshot publish, host-API install, local ledger) and 概览 (a read-only
 * dashboard). Everything goes through our own plugin routes
 * (`/api/offline-market/...`); the browser never talks to the market backend
 * or the host API directly.
 */
const PLUGIN_ID = "offline-market";
const PLUGIN_VERSION = "0.1.7"; // keep in sync with plugin.json
const PLUGIN_LABEL = "内网市场";
const VISIBILITY_LABEL = { public: "公开", tenant: "仅本租户" };
const STATE_LABEL = { active: "已上架", withdrawn: "已撤回", taken_down: "已下架" };
const STATE_COLOR = { active: "green", withdrawn: "default", taken_down: "red" };
// 卡片高度上限：描述最多两行（2 × 20px 行高），卡片整体再兜一层上限，
// 避免超长描述把网格里的卡片撑得过长。
const CARD_MAX_HEIGHT = 320;
const DESC_MAX_HEIGHT = 40;
const AGENT_STATE_LABEL = {
  active: "正常",
  withdrawn: "已撤回",
  taken_down: "已下架",
  gone: "市场中已不存在",
  unknown: "未知",
};
const AGENT_STATE_COLOR = {
  active: "green",
  withdrawn: "default",
  taken_down: "red",
  gone: "default",
  unknown: "default",
};

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
    `%c${PLUGIN_ID}%c v${PLUGIN_VERSION} · ${PLUGIN_LABEL} · QwenPaw frontend plugin`,
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
  const { timeoutMs = REQUEST_TIMEOUT_MS, ...fetchInit } = init || {};
  const method = fetchInit.method || "GET";
  const url = apiUrl(path);
  const host = window.QwenPaw && window.QwenPaw.host;
  const token = host && typeof host.getApiToken === "function" ? host.getApiToken() : "";
  const headers = { accept: "application/json", ...((fetchInit && fetchInit.headers) || {}) };
  if (token) headers.Authorization = `Bearer ${token}`;
  const agent = selectedAgentId();
  if (agent) headers["X-Agent-Id"] = agent;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const started = Date.now();
  logger.debug(`→ ${method} ${path}`, {
    url,
    agent: agent || "(未选中，宿主用 active_agent)",
    token: token ? "已附带" : "无",
    timeout: timeoutMs,
  });
  let response;
  let text;
  try {
    response = await fetch(url, {
      credentials: "same-origin",
      ...fetchInit,
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
      throw new Error(`请求超时（${timeoutMs / 1000}s）：${path}`);
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
    // axum returns either {detail: "text"} or {detail: {code, message}}; keep both
    const payload = (body && body.detail) || {};
    const detail = typeof payload === "string" ? {} : payload;
    const plain = typeof payload === "string" ? payload : (body && body.message) || "";
    const error = new Error(detail.message || plain || `HTTP ${response.status}`);
    error.code = detail.code || `http_${response.status}`;
    error.status = response.status;
    error.detail = detail;
    error.step = detail.step;
    error.workspace_kept = detail.workspace_kept;
    error.rollback_failed = detail.rollback_failed;
    logger.warn(`✗ ${method} ${path} ${response.status} [${error.code}] ${error.message}`);
    throw error;
  }
  return body;
}

const getJson = (path) => request(path);

/**
 * `My Skill!` -> `my-skill`. Only a UI fallback for when the market cannot be
 * asked: Chinese names give "" here and are left to the backend, which
 * transliterates them to pinyin (`周报助手` -> `zhoubaozhushou`).
 */
function fallbackSlug(name) {
  const slug = String(name || "")
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64)
    .replace(/-+$/g, "");
  return /^[a-z0-9]/.test(slug) ? slug : "";
}
const postJson = (path, payload) =>
  request(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload || {}),
  });

// Agent operations snapshot skills / create workspaces, so they can exceed the
// page's normal 15s deadline (the plugin waits up to 60s for the host).
const postJsonSlow = (path, payload) =>
  request(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload || {}),
    timeoutMs: 180000,
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
  const { useEffect, useState, useCallback, useMemo, useRef } = React;

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
  const Modal = antd.Modal;
  const Drawer = antd.Drawer;
  const Steps = antd.Steps;
  const Radio = antd.Radio;
  const Tooltip = antd.Tooltip;
  const Empty = antd.Empty || (() => h("div", null, "暂无数据"));
  const Spin = antd.Spin || (() => h("div", null, "加载中…"));
  const Typography = antd.Typography || {};
  const Text = Typography.Text || ((props) => h("span", props, props.children));
  const Title = Typography.Title || ((props) => h("h3", props, props.children));
  const Paragraph = Typography.Paragraph || ((props) => h("p", props, props.children));

  // --------------------------------------------------------------- theme ---

  const useAntdToken =
    antd.theme && typeof antd.theme.useToken === "function" ? antd.theme.useToken : null;

  const EMPTY_TOKEN_STATE = { token: {} };

  function isDarkColor(value) {
    const match = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(String(value || ""));
    if (!match) return false;
    let hex = match[1];
    if (hex.length === 3) {
      hex = hex
        .split("")
        .map((part) => part + part)
        .join("");
    }
    const red = parseInt(hex.slice(0, 2), 16);
    const green = parseInt(hex.slice(2, 4), 16);
    const blue = parseInt(hex.slice(4, 6), 16);
    return (0.299 * red + 0.587 * green + 0.114 * blue) / 255 < 0.5;
  }

  // antd v5 tokens when the console provides them (it does), neutral fallbacks
  // otherwise. Keeps our own surfaces readable on both themes without a CSS
  // file, and never overrides the host's own look.
  function useMarketTheme() {
    const state = useAntdToken ? useAntdToken() : EMPTY_TOKEN_STATE;
    const token = state.token || {};
    const surface = token.colorBgContainer || "#ffffff";
    const dark = isDarkColor(surface);
    return {
      dark,
      token,
      palette: {
        ink: token.colorText || (dark ? "#e5e7eb" : "#0f172a"),
        muted: token.colorTextSecondary || (dark ? "#a1a1aa" : "#64748b"),
        faint: token.colorTextTertiary || (dark ? "#71717a" : "#94a3b8"),
        line: token.colorBorderSecondary || (dark ? "#303030" : "#e2e8f0"),
        surface,
        surfaceAlt: token.colorFillTertiary || (dark ? "#262626" : "#f1f5f9"),
        border: dark
          ? "1px solid rgba(255, 255, 255, 0.12)"
          : "1px solid rgba(148, 163, 184, 0.18)",
        shadow: dark
          ? "0 10px 30px rgba(0, 0, 0, 0.45)"
          : "0 10px 30px rgba(15, 23, 42, 0.06)",
        skills: token.colorPrimary || "#6366f1",
        agents: "#14b8a6",
        downloads: "#0ea5e9",
        fresh: "#f59e0b",
      },
    };
  }


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
                  // lets the market list flip this row to 已安装 without a reload
                  slug: item.slug,
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

  /**
   * Which market rows are already installed on this agent.
   *
   * The host's skill list only reports display names, and a hub install always
   * shows up as `source: "customized"`; the only real evidence is
   * `/skills/{name}.installed_from` — a detail call that costs 2~3s each behind
   * the hub proxy. So: one cheap list call, then confirm only the market rows
   * the host actually has (i.e. the installed ones), instead of the whole
   * catalogue fan-out the 已安装 tab needs. Positives are cached for the tab's
   * lifetime; a failure is not: the buttons stay clickable and the host's own
   * install call still refuses a duplicate.
   */
  function useInstalledSlugs(items) {
    const [slugs, setSlugs] = useState(() => new Set());
    const [versions, setVersions] = useState(() => ({}));
    const confirmed = useRef(new Map()); // display name -> installed version_text
    const marked = useRef(new Set()); // slugs installed from this page

    useEffect(() => {
      let alive = true;
      if (!items.length) {
        setSlugs(new Set());
        setVersions({});
        return undefined;
      }
      (async () => {
        try {
          const listed = await getJson("/skills");
          const skills = Array.isArray(listed) ? listed : listed.items || [];
          const onHost = skills.filter((item) => item.source !== "builtin");
          const names = new Set(onHost.map((item) => item.name));
          const unknown = items.filter(
            (item) => item.name && names.has(item.name) && !confirmed.current.has(item.name),
          );
          logger.debug(
            `已安装检查：宿主非 builtin ${onHost.length} 个，市场共 ${items.length} 条，待确认 ${unknown.length} 条`,
          );
          await Promise.all(
            unknown.map(async (item) => {
              // one unreadable skill must not hide the rest
              const detail = await getJson(`/skills/${encodeURIComponent(item.name)}`).catch(
                () => null,
              );
              if (detail && detail.installed_from === PLUGIN_ID) {
                confirmed.current.set(
                  item.name,
                  String(detail.version_text || detail.version || detail.last_updated || ""),
                );
              }
            }),
          );
          if (!alive) return;
          const next = new Set();
          const nextVersions = {};
          items.forEach((item) => {
            if (marked.current.has(item.slug) || confirmed.current.has(item.name)) {
              next.add(item.slug);
              const version = confirmed.current.get(item.name);
              if (version) nextVersions[item.slug] = version;
            }
          });
          logger.debug(`已安装检查完成：市场里已安装 ${next.size} 条`, [...next]);
          setSlugs(next);
          setVersions(nextVersions);
        } catch (err) {
          logger.warn("已安装检查失败（按钮照旧可点，重复安装由宿主自身拦）", err);
        }
      })();
      return () => {
        alive = false;
      };
    }, [items]);

    const mark = useCallback((slug) => {
      if (!slug) return;
      marked.current.add(slug);
      setSlugs((prev) => (prev.has(slug) ? prev : new Set(prev).add(slug)));
    }, []);

    return [slugs, mark, versions];
  }
  // ------------------------------------------------------------ catalogue ---
  // Shared by the skill and agent market tabs: debounced search, server-side
  // sorting, paging with infinite scroll, card/list switch and a detail
  // drawer. Both tabs pass their own install handler; everything else is
  // identical, so the two lists can never drift apart again.

  const CATALOGUE_LIMIT = 20;
  const VIEW_KEY = "offline-market:view";
  const SORT_OPTIONS = [
    ["created", "最新发布"],
    ["downloads", "下载最多"],
    ["updated", "最近更新"],
  ];

  function storedView() {
    try {
      return window.localStorage.getItem(VIEW_KEY) === "list" ? "list" : "card";
    } catch (err) {
      return "card"; // storage disabled: cards are the nicer default anyway
    }
  }

  function formatStamp(seconds) {
    if (!seconds) return "—";
    const date = new Date(Number(seconds) * 1000);
    if (Number.isNaN(date.getTime())) return "—";
    return [
      date.getFullYear(),
      String(date.getMonth() + 1).padStart(2, "0"),
      String(date.getDate()).padStart(2, "0"),
    ].join("-");
  }

  function formatBytes(value) {
    const size = Number(value) || 0;
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    return `${(size / 1024 / 1024).toFixed(1)} MB`;
  }

  function versionParts(text) {
    return String(text || "")
      .trim()
      .replace(/^v/i, "")
      .split(/[.+\-]/)
      .filter((part) => part !== "")
      .map((part) => (/^\d+$/.test(part) ? Number(part) : part));
  }

  // 1.2.10 > 1.2.9; non-numeric segments (dates, hashes) compare as strings so
  // the answer stays stable instead of throwing.
  function compareVersionText(left, right) {
    const a = versionParts(left);
    const b = versionParts(right);
    const size = Math.max(a.length, b.length);
    for (let index = 0; index < size; index += 1) {
      const x = a[index] === undefined ? 0 : a[index];
      const y = b[index] === undefined ? 0 : b[index];
      if (x === y) continue;
      if (typeof x === "number" && typeof y === "number") return x < y ? -1 : 1;
      return String(x) < String(y) ? -1 : 1;
    }
    return 0;
  }

  function isNewerVersion(latest, installed) {
    if (!latest || !installed) return false;
    return compareVersionText(latest, installed) > 0;
  }

  function useDebounced(value, delay) {
    const [settled, setSettled] = useState(value);
    useEffect(() => {
      const timer = setTimeout(() => setSettled(value), delay);
      return () => clearTimeout(timer);
    }, [value, delay]);
    return settled;
  }

  /**
   * Run `reset` when a screen becomes visible again.
   *
   * antd's Tabs mounts a pane on first visit and afterwards only hides it, so a
   * screen that ends on a terminal state (the 「发布成功」 card) still shows that
   * state after the user leaves and comes back. The publish pages want the
   * upload form back instead; this hook fires on the hidden → visible edge only,
   * so a form that is still being filled in is never disturbed.
   */
  function useResetOnShow(visible, reset) {
    const wasVisible = useRef(visible);
    useEffect(() => {
      if (visible && !wasVisible.current) reset();
      wasVisible.current = visible;
    }, [visible, reset]);
  }

  function useCatalogue({ kind, refreshToken, onNotice }) {
    const [items, setItems] = useState([]);
    const [total, setTotal] = useState(0);
    const [query, setQuery] = useState("");
    const appliedQuery = useDebounced(query, 300);
    const [sort, setSort] = useState("created");
    const [view, setViewState] = useState(storedView);
    const [page, setPage] = useState(1);
    // start in the loading state: the first paint must not flash the empty state
    const [loading, setLoading] = useState(true);
    const [loadingMore, setLoadingMore] = useState(false);
    const [error, setError] = useState("");
    const [hasMore, setHasMore] = useState(false);
    const requestId = useRef(0);

    const setView = useCallback((value) => {
      setViewState(value);
      try {
        window.localStorage.setItem(VIEW_KEY, value);
      } catch (err) {
        // private mode: the switch still works for this session
      }
    }, []);

    const fetchPage = useCallback(
      async (targetPage, append) => {
        const id = requestId.current + 1;
        requestId.current = id;
        if (append) {
          setLoadingMore(true);
        } else {
          setLoading(true);
          setError("");
        }
        logger.debug(
          `市场目录加载 kind=${kind} q=${JSON.stringify(appliedQuery)} sort=${sort} page=${targetPage}`,
        );
        try {
          const body = await getJson(
            `/offline-market/${kind}?q=${encodeURIComponent(appliedQuery)}` +
              `&sort=${encodeURIComponent(sort)}&page=${targetPage}&limit=${CATALOGUE_LIMIT}`,
          );
          if (id !== requestId.current) return; // a newer request won
          const next = body.items || [];
          const count = Number(body.total) || 0;
          setItems((previous) => (append ? previous.concat(next) : next));
          setTotal(count);
          setPage(targetPage);
          setHasMore(next.length > 0 && targetPage * CATALOGUE_LIMIT < count);
          logger.debug(`市场目录 ${kind} 第 ${targetPage} 页 ${next.length} 条（共 ${count}）`);
        } catch (err) {
          if (id !== requestId.current) return;
          logger.error(`市场目录加载失败 kind=${kind}`, err);
          setError(err.message || String(err));
          if (!append) {
            setItems([]);
            setTotal(0);
            setHasMore(false);
          }
          onNotice && onNotice({ type: "error", text: err.message });
        } finally {
          if (id === requestId.current) {
            setLoading(false);
            setLoadingMore(false);
          }
        }
      },
      [kind, appliedQuery, sort, onNotice],
    );

    useEffect(() => {
      // Mount, query/sort change, and once after a publish (refreshToken).
      setPage(1);
      fetchPage(1, false);
    }, [fetchPage, refreshToken]);

    const loadMore = useCallback(() => {
      if (loading || loadingMore || !hasMore) return;
      fetchPage(page + 1, true);
    }, [loading, loadingMore, hasMore, page, fetchPage]);

    const retry = useCallback(() => {
      setPage(1);
      fetchPage(1, false);
    }, [fetchPage]);

    return {
      items,
      total,
      query,
      setQuery,
      appliedQuery,
      sort,
      setSort,
      view,
      setView,
      loading,
      loadingMore,
      error,
      hasMore,
      loadMore,
      retry,
    };
  }

  function CatalogueView({
    kind,
    tenant,
    catalogue,
    installed,
    installedVersions,
    busySlug,
    onInstall,
    emptyText,
    searchPlaceholder,
  }) {
    const [detail, setDetail] = useState(null);
    const sentinel = useRef(null);
    const loadMoreRef = useRef(catalogue.loadMore);
    loadMoreRef.current = catalogue.loadMore;

    useEffect(() => {
      const node = sentinel.current;
      if (!node || !catalogue.hasMore || typeof IntersectionObserver === "undefined") {
        return undefined;
      }
      const observer = new IntersectionObserver(
        (entries) => {
          if (entries.some((entry) => entry.isIntersecting)) loadMoreRef.current();
        },
        { rootMargin: "200px" },
      );
      observer.observe(node);
      return () => observer.disconnect();
    }, [catalogue.hasMore, catalogue.items.length, catalogue.loading]);

    const installedVersion = (row) => (installedVersions && installedVersions[row.slug]) || "";
    const isInstalled = (row) => installed.has(row.slug);

    const stateTag = (row) => {
      if (!isInstalled(row)) return null;
      const local = installedVersion(row);
      if (isNewerVersion(row.latest_version, local)) {
        return h(Tag, { color: "orange" }, `可更新 ${local} → ${row.latest_version}`);
      }
      return h(Tag, { color: "green" }, local ? `已安装 ${local}` : "已安装");
    };

    const actionButton = (row) => {
      if (!isInstalled(row)) {
        return h(
          Button,
          {
            type: "primary",
            size: "small",
            loading: busySlug === row.slug,
            disabled: Boolean(busySlug) && busySlug !== row.slug,
            onClick: () => onInstall(row),
          },
          "安装",
        );
      }
      const local = installedVersion(row);
      if (isNewerVersion(row.latest_version, local)) {
        return h(
          Button,
          {
            type: "primary",
            size: "small",
            loading: busySlug === row.slug,
            disabled: Boolean(busySlug) && busySlug !== row.slug,
            onClick: () => onInstall(row),
          },
          "更新",
        );
      }
      return h(Button, { size: "small", disabled: true }, "已安装");
    };

    const nameCell = (row) =>
      h(
        "div",
        null,
        h("div", null, h(Text, { strong: true }, row.name)),
        h(Text, { type: "secondary", code: true }, `${row.slug}@${row.latest_version || "-"}`),
      );

    const publisherCell = (row) => {
      const label = row.owner_name || row.owner_tenant || "-";
      const mine = Boolean(tenant) && row.owner_tenant === tenant;
      return h("span", { title: row.owner_tenant || "" }, mine ? `${label}（我）` : label);
    };

    const columns = [
      { title: "名称", dataIndex: "name", render: (_, row) => nameCell(row) },
      {
        title: "描述",
        dataIndex: "description",
        ellipsis: true,
        render: (value) => value || h(Text, { type: "secondary" }, "—"),
      },
      kind === "agents"
        ? { title: "技能", dataIndex: "skill_count", width: 70 }
        : { title: "版本数", dataIndex: "versions", width: 80 },
      {
        title: "可见性",
        dataIndex: "visibility",
        width: 100,
        render: (value) =>
          h(Tag, { color: value === "public" ? "blue" : "gold" }, VISIBILITY_LABEL[value] || value),
      },
      { title: "发布者", dataIndex: "owner_name", width: 150, render: (_, row) => publisherCell(row) },
      { title: "下载", dataIndex: "download_count", width: 80, render: (value) => Number(value) || 0 },
      { title: "更新", dataIndex: "updated_at", width: 110, render: (value) => formatStamp(value) },
      {
        title: "操作",
        width: 160,
        render: (_, row) =>
          h(
            "div",
            {
              style: { display: "flex", flexDirection: "column", gap: 4, alignItems: "flex-start" },
              // the row itself opens the drawer; buttons must not double-fire
              onClick: (event) => event.stopPropagation(),
            },
            actionButton(row),
            stateTag(row),
          ),
      },
    ];

    const cardGrid = () =>
      h(
        "div",
        {
          style: {
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(min(100%, 280px), 1fr))",
            gap: 12,
          },
        },
        catalogue.items.map((row) =>
          h(
            Card,
            {
              key: row.slug,
              size: "small",
              hoverable: true,
              onClick: () => setDetail(row),
              // 防御：卡片高度设上限，描述再长也不会把整张卡片撑开
              style: { cursor: "pointer", maxHeight: CARD_MAX_HEIGHT, overflow: "hidden" },
            },
            h(
              "div",
              { style: { display: "flex", justifyContent: "space-between", gap: 8 } },
              h(Text, { strong: true, style: { fontSize: 15 } }, row.name),
              h(
                Tag,
                { color: row.visibility === "public" ? "blue" : "gold" },
                VISIBILITY_LABEL[row.visibility] || row.visibility,
              ),
            ),
            h(
              Text,
              { type: "secondary", code: true },
              `${row.slug}@${row.latest_version || "-"}`,
            ),
            h(
              Paragraph,
              {
                type: "secondary",
                // 行内省略依赖 -webkit-line-clamp，宿主 CSS 不一定保留该属性，
                // 因此同时锁死高度（2 行 × 20px），任何情况下都不会撑高卡片。
                title: row.description || "",
                style: {
                  margin: "8px 0",
                  lineHeight: "20px",
                  height: DESC_MAX_HEIGHT,
                  maxHeight: DESC_MAX_HEIGHT,
                  display: "-webkit-box",
                  WebkitLineClamp: 2,
                  WebkitBoxOrient: "vertical",
                  overflow: "hidden",
                  wordBreak: "break-word",
                },
              },
              row.description || "—",
            ),
            h(
              Space,
              { size: 12, wrap: true },
              h(Text, { type: "secondary" }, `↓ ${Number(row.download_count) || 0}`),
              h(Text, { type: "secondary" }, `更新 ${formatStamp(row.updated_at)}`),
              kind === "agents"
                ? h(Text, { type: "secondary" }, `${row.skill_count || 0} 个技能`)
                : h(Text, { type: "secondary" }, `${row.versions || 0} 个版本`),
            ),
            h(
              "div",
              {
                style: {
                  marginTop: 12,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                },
              },
              h(
                "div",
                {
                  style: { display: "flex", flexWrap: "wrap", gap: 6, alignItems: "center" },
                  onClick: (event) => event.stopPropagation(),
                },
                stateTag(row),
                h(Text, { type: "secondary" }, row.owner_name || row.owner_tenant || "-"),
              ),
              h(
                "div",
                { onClick: (event) => event.stopPropagation() },
                actionButton(row),
              ),
            ),
          ),
        ),
      );

    const sortControl = Select
      ? h(Select, {
          value: catalogue.sort,
          style: { width: 130 },
          onChange: catalogue.setSort,
          options: SORT_OPTIONS.map(([value, label]) => ({ value, label })),
        })
      : h(
          Space,
          null,
          SORT_OPTIONS.map(([value, label]) =>
            h(
              Button,
              {
                key: value,
                size: "small",
                type: catalogue.sort === value ? "primary" : "default",
                onClick: () => catalogue.setSort(value),
              },
              label,
            ),
          ),
        );

    const viewControl = h(
      Space,
      null,
      h(
        Button,
        {
          size: "small",
          type: catalogue.view === "card" ? "primary" : "default",
          onClick: () => catalogue.setView("card"),
        },
        "卡片",
      ),
      h(
        Button,
        {
          size: "small",
          type: catalogue.view === "list" ? "primary" : "default",
          onClick: () => catalogue.setView("list"),
        },
        "列表",
      ),
    );

    const detailBody = detail
      ? h(
          "div",
          null,
          h(
            Paragraph,
            { type: "secondary", style: { marginTop: 0 } },
            detail.description || "没有描述",
          ),
          [
            ["标识", `${detail.slug}@${detail.latest_version || "-"}`],
            ["状态", h(Tag, { color: "green" }, "已上架")],
            [
              "可见性",
              h(
                Tag,
                { color: detail.visibility === "public" ? "blue" : "gold" },
                VISIBILITY_LABEL[detail.visibility] || detail.visibility,
              ),
            ],
            ["发布者", detail.owner_name || detail.owner_tenant || "-"],
            ["下载量", String(Number(detail.download_count) || 0)],
            ["更新时间", formatStamp(detail.updated_at)],
            ["版本数", String(detail.versions || 0)],
            ["大小", formatBytes(detail.size_bytes)],
            ...(kind === "agents"
              ? [["技能数", String(detail.skill_count || 0)]]
              : []),
            ["本机", isInstalled(detail) ? installedVersion(detail) || "已安装（版本未知）" : "未安装"],
          ].map(([label, value]) =>
            h(
              "div",
              {
                key: label,
                style: {
                  display: "grid",
                  gridTemplateColumns: "84px 1fr",
                  gap: 8,
                  padding: "6px 0",
                  borderBottom: "1px solid rgba(128,128,128,0.16)",
                },
              },
              h(Text, { type: "secondary" }, label),
              h("div", { style: { wordBreak: "break-all" } }, value),
            ),
          ),
          h(
            "div",
            { style: { marginTop: 16, display: "flex", gap: 8 } },
            actionButton(detail),
            h(
              Button,
              {
                size: "small",
                onClick: () => {
                  if (!navigator.clipboard) return;
                  navigator.clipboard.writeText(detail.slug).catch(() => {});
                },
              },
              "复制标识",
            ),
          ),
        )
      : null;

    const detailHost = !detail
      ? null
      : Drawer
        ? h(
            Drawer,
            {
              open: true,
              width: 420,
              title: detail.name,
              onClose: () => setDetail(null),
            },
            detailBody,
          )
        : Modal
          ? h(
              Modal,
              {
                open: true,
                width: 520,
                title: detail.name,
                footer: null,
                onCancel: () => setDetail(null),
              },
              detailBody,
            )
          : null;

    return h(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: 12 } },
      h(
        "div",
        { style: { display: "flex", flexWrap: "wrap", gap: 8, alignItems: "center" } },
        Input
          ? h(Input, {
              placeholder: searchPlaceholder,
              value: catalogue.query,
              allowClear: true,
              style: { width: 260 },
              onChange: (event) => catalogue.setQuery(event.target.value),
            })
          : null,
        sortControl,
        viewControl,
        h(
          Text,
          { type: "secondary" },
          `共 ${catalogue.total} 个${
            catalogue.appliedQuery ? `（匹配“${catalogue.appliedQuery}”）` : ""
          }`,
        ),
        catalogue.loading ? h(Spin, { size: "small" }) : null,
        h(Button, { size: "small", onClick: catalogue.retry }, "刷新"),
      ),
      catalogue.error
        ? h(Alert, {
            type: "error",
            showIcon: true,
            message: "列表加载失败",
            description: catalogue.error,
            action: h(Button, { size: "small", onClick: catalogue.retry }, "重试"),
          })
        : null,
      !catalogue.loading && !catalogue.items.length
        ? h(Empty, { description: catalogue.error ? "加载失败，请重试" : emptyText })
        : catalogue.view === "card" || !Table
          ? cardGrid()
          : h(Table, {
              rowKey: "slug",
              size: "small",
              pagination: false,
              dataSource: catalogue.items,
              columns,
              onRow: (row) => ({ onClick: () => setDetail(row), style: { cursor: "pointer" } }),
              locale: { emptyText: h(Empty, { description: emptyText }) },
            }),
      h(
        "div",
        { ref: sentinel, style: { textAlign: "center", padding: "4px 0" } },
        catalogue.loadingMore
          ? h(Spin, { size: "small" })
          : catalogue.hasMore
            ? h(Button, { size: "small", onClick: catalogue.loadMore }, "加载更多")
            : catalogue.items.length
              ? h(Text, { type: "secondary" }, "已经到底了")
              : null,
      ),
      detailHost,
    );
  }


  function MarketTab({ limits, onNotice, refreshToken, tenant, onInstalled }) {
    const catalogue = useCatalogue({ kind: "skills", refreshToken, onNotice });
    const [installed, markInstalled, installedVersions] = useInstalledSlugs(catalogue.items);
    // Local to the list: notify the page, and remember the row just installed
    // so it turns 已安装 immediately instead of after a reload.
    const onInstallDone = useCallback(
      (notice) => {
        onNotice(notice);
        if (notice && notice.type === "success" && notice.slug) {
          markInstalled(notice.slug);
          // The 已安装 tab stays mounted after its first visit, so it only
          // reloads when the page bumps its token. Installing without this
          // left a stale list behind.
          if (onInstalled) onInstalled(notice.slug);
        }
      },
      [onNotice, markInstalled, onInstalled],
    );
    const installer = useInstaller(onInstallDone);

    return h(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: 12 } },
      installer.task
        ? h(Alert, {
            type: "info",
            showIcon: true,
            message: `正在安装 ${installer.busySlug}（${installer.task.status}）`,
            action: h(
              Button,
              { size: "small", danger: true, onClick: installer.cancel },
              "取消安装",
            ),
          })
        : null,
      h(CatalogueView, {
        kind: "skills",
        tenant,
        catalogue,
        installed,
        installedVersions,
        busySlug: installer.busySlug,
        onInstall: (row) => installer.install(row),
        emptyText: "还没有技能",
        searchPlaceholder: "搜索技能名或标识符",
      }),
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

  // ------------------------------------------------------ publish layout ---
  // The publish form is the one screen a publisher stares at, so it borrows the
  // host console's import flow instead of dumping inputs on a Card: a numbered
  // 选择 → 确认 → 完成 progress, a drop zone, and a dry-run report from the
  // market that lists every problem at once (the backend's own rules, no second
  // copy of them here).

  /** Numbered section head — same visual rhythm as the host's import pages. */
  function FormSection({ index, title, hint, extra, palette }) {
    return h(
      "div",
      { style: { display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" } },
      h(
        "span",
        {
          style: {
            width: 22,
            height: 22,
            borderRadius: 7,
            display: "inline-flex",
            alignItems: "center",
            justifyContent: "center",
            background: palette.dark
              ? "rgba(99, 102, 241, 0.24)"
              : "rgba(99, 102, 241, 0.12)",
            color: palette.skills,
            fontSize: 12,
            fontWeight: 600,
          },
        },
        String(index),
      ),
      h("span", { style: { fontWeight: 600, color: palette.ink } }, title),
      hint ? h("span", { style: { fontSize: 12, color: palette.faint } }, hint) : null,
      h("span", { style: { flex: "1 1 40px" } }),
      extra || null,
    );
  }

  /** Label above control, hint below: the console's vertical form rhythm. */
  function FormField({ label, hint, children, palette, grow }) {
    return h(
      "div",
      {
        style: {
          display: "grid",
          gap: 6,
          flex: grow ? `${grow} 1 180px` : "1 1 180px",
          minWidth: 160,
        },
      },
      h("span", { style: { fontSize: 12, color: palette.muted } }, label),
      children,
      hint ? h("span", { style: { fontSize: 12, color: palette.faint } }, hint) : null,
    );
  }

  /** Folder-with-arrow glyph; inline SVG keeps us free of an icon package. */
  function ArchiveGlyph({ size = 26, color = "currentColor" }) {
    return h(
      "svg",
      {
        width: size,
        height: size,
        viewBox: "0 0 24 24",
        fill: "none",
        stroke: color,
        strokeWidth: 1.6,
        strokeLinecap: "round",
        strokeLinejoin: "round",
        "aria-hidden": "true",
      },
      h("path", { d: "M12 16V8" }),
      h("path", { d: "M8.5 11.5 12 8l3.5 3.5" }),
      h("path", { d: "M20 16.5A3.5 3.5 0 0 0 17.5 10h-1.1A5.5 5.5 0 1 0 6 14.8" }),
    );
  }

  /**
   * Drop zone that also works with a click. Wraps a hidden `<input type=file>`
   * (the host's own SkillPool does the same) plus drag handlers, so a publisher
   * can either drag a `.zip` from the desktop or browse for it.
   */
  function PackageDropZone({
    archive,
    dragging,
    disabled,
    onDragOver,
    onDragLeave,
    onDrop,
    onPick,
    onClear,
    palette,
    limits,
  }) {
    const inputRef = useRef(null);
    const open = useCallback(() => {
      if (!disabled && inputRef.current) inputRef.current.click();
    }, [disabled]);
    return h(
      "div",
      {
        role: "button",
        tabIndex: disabled ? -1 : 0,
        "aria-label": "选择或拖入技能压缩包",
        onClick: open,
        onKeyDown: (event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            open();
          }
        },
        onDragOver: (event) => {
          event.preventDefault();
          if (!disabled) onDragOver(event);
        },
        onDragLeave,
        onDrop,
        style: {
          display: "grid",
          justifyItems: "center",
          gap: 6,
          padding: "24px 16px",
          borderRadius: 12,
          textAlign: "center",
          cursor: disabled ? "not-allowed" : "pointer",
          background: dragging
            ? palette.dark
              ? "rgba(99, 102, 241, 0.18)"
              : "rgba(99, 102, 241, 0.06)"
            : palette.surfaceAlt,
          border: dragging ? `1px dashed ${palette.skills}` : `1px dashed ${palette.line}`,
          transition: "background 120ms ease, border-color 120ms ease",
        },
      },
      h("input", {
        ref: inputRef,
        type: "file",
        accept: ".zip,application/zip,application/x-zip-compressed",
        style: { display: "none" },
        onChange: onPick,
        disabled,
      }),
      h(ArchiveGlyph, { color: archive ? palette.skills : palette.faint }),
      h(
        "span",
        { style: { color: palette.ink, fontWeight: 500 } },
        archive ? archive.name : "点击选择或把 .zip 拖到这里",
      ),
      h(
        "span",
        { style: { fontSize: 12, color: palette.faint } },
        archive
          ? `${formatBytes(archive.bytes.byteLength)} · 点这里换一个包`
          : "根目录（或压缩包里唯一的顶层目录）必须包含 SKILL.md",
      ),
      archive
        ? h(
            Button,
            {
              size: "small",
              type: "link",
              style: { padding: 0, height: "auto" },
              onClick: (event) => {
                event.stopPropagation();
                onClear();
              },
            },
            "移除这个包",
          )
        : null,
      limits && !archive
        ? h(
            "span",
            { style: { fontSize: 12, color: palette.faint } },
            `限额 ${limits.max_files} 个文件 / 单文件 ${formatBytes(limits.max_file_bytes)} / 合计 ${formatBytes(
              limits.max_bundle_bytes,
            )}`,
          )
        : null,
    );
  }

  /** `Input` + trailing button; antd 5 deprecates `addonAfter` for this. */
  function withAddon(control, addon) {
    if (!control || !addon) return control;
    if (Space && Space.Compact) {
      return h(Space.Compact, { style: { width: "100%" } }, control, addon);
    }
    return h("div", { style: { display: "flex", gap: 6 } }, control, addon);
  }

  /** One read-only fact from the dry run (file count, size, …). */
  function ReportStat({ label, value, palette }) {
    return h(
      "div",
      {
        style: {
          display: "grid",
          gap: 2,
          padding: "8px 10px",
          borderRadius: 10,
          background: palette.surfaceAlt,
          border: palette.border,
          minWidth: 96,
        },
      },
      h("span", { style: { fontSize: 12, color: palette.faint } }, label),
      h("span", { style: { color: palette.ink, fontVariantNumeric: "tabular-nums" } }, value),
    );
  }

  function PublishTab({ limits, onNotice, onPublished, active = true }) {
    const { palette } = useMarketTheme();
    const [name, setName] = useState("");
    const [slug, setSlug] = useState("");
    // 手改过的标识符不再自动覆盖：有人故意把同一个 id 复用到新包上。
    const slugEdited = useRef(false);
    const [version, setVersion] = useState("1.0.0");
    const [visibility, setVisibility] = useState("public");
    const [archive, setArchive] = useState(null);
    // 试运行报告：问题清单、manifest、文件体积、建议版本都从这里来。
    const [report, setReport] = useState(null);
    const [inspecting, setInspecting] = useState(false);
    // 试运行本身失败（市场不可达、插件版本太旧…）也要看得见：静默失败会让人
    // 以为「选完包什么也没发生」。
    const [inspectError, setInspectError] = useState(null);
    const [fileError, setFileError] = useState(null);
    const [dragging, setDragging] = useState(false);
    const [busy, setBusy] = useState(false);
    const [result, setResult] = useState(null);
    // 切走再切回来时收掉「发布成功」卡：绕一圈回来期待的仍是上传表单。名称/
    // 版本留在原处，接着发下一个版本是最常见的动作。
    const clearResult = useCallback(() => setResult(null), []);
    useResetOnShow(active, clearResult);
    const publisherName = useCurrentUserName();
    const derivedName = useDebounced(name, 300);
    const stableSlug = useDebounced(slug, 500);
    const stableVersion = useDebounced(version, 600);
    // 每次选包都递增：同名包再选一次也要重推一遍（防抖不会自己重跑）。
    const [pickRevision, setPickRevision] = useState(0);
    // 试运行是异步的：只认最后一次请求的回答。
    const inspectSeq = useRef(0);
    // 同一份表单 + 同一个包只干跑一次：重复的请求只会多解一次 zip。
    const lastInspect = useRef("");

    /**
     * 标识符跟着名称走：换一个压缩包就换一次名称，标识符随之重新推导（不是
     * 只第一次填）。规则在后端（`/offline-market/derive-slug`）：中文名转拼音，
     * 英文名做同样的归一化，所以页面不用再维护第二份规则。
     */
    useEffect(() => {
      if (slugEdited.current) return;
      const wanted = derivedName.trim();
      if (!wanted) {
        setSlug("");
        return;
      }
      let cancelled = false;
      getJson(`/offline-market/derive-slug?name=${encodeURIComponent(wanted)}`)
        .then((body) => {
          if (cancelled || slugEdited.current) return;
          setSlug((body && body.slug) || "");
        })
        .catch((err) => {
          if (cancelled || slugEdited.current) return;
          // 市场不可达时退回本地 ASCII 规则；中文名留空，发布时后端仍会推。
          setSlug(fallbackSlug(wanted));
          logger.warn(`标识符推导失败，退回本地规则：${wanted}`, err);
        });
      return () => {
        cancelled = true;
      };
    }, [derivedName, pickRevision]);

    const takeFile = useCallback(async (picked) => {
      if (!picked) return;
      if (!/\.zip$/i.test(picked.name || "")) {
        setFileError("只支持 .zip 压缩包，请重新选择");
        return;
      }
      try {
        const bytes = await readFileAsBytes(picked);
        logger.debug(`已选择压缩包 ${picked.name} ${bytes.byteLength} 字节`, {
          lastModified: picked.lastModified,
        });
        setArchive({ name: picked.name, bytes });
        setFileError(null);
        setReport(null);
        setInspectError(null);
        setResult(null);
        lastInspect.current = ""; // 同一个包再选一次也要重新校验
        // 每次都跟着换名称：上一次的名称属于上一个包，不能只填第一次。
        setName(picked.name.replace(/\.zip$/i, ""));
        // 上一个包推出来的标识符立刻作废：防抖那 300ms 里点发布也不能带上
        // 旧 id （留空时后端会按名称重新推导）。
        if (!slugEdited.current) setSlug("");
        setPickRevision((value) => value + 1);
      } catch (err) {
        logger.error("读取压缩包失败", err);
        setFileError(err.message);
      }
    }, []);

    const onPick = useCallback(
      (event) => {
        const picked = (event.target.files || [])[0];
        event.target.value = ""; // allow picking the same file again
        takeFile(picked);
      },
      [takeFile],
    );

    const pickDropped = useCallback((dataTransfer) => {
      if (!dataTransfer) return undefined;
      const files = dataTransfer.files || [];
      if (files.length) return files[0];
      // 少数 webview 只在 items 里给出文件。
      const items = dataTransfer.items || [];
      for (let index = 0; index < items.length; index += 1) {
        const item = items[index];
        if (item && item.kind === "file" && item.getAsFile) {
          const file = item.getAsFile();
          if (file) return file;
        }
      }
      return undefined;
    }, []);

    const onDrop = useCallback(
      (event) => {
        event.preventDefault();
        event.stopPropagation();
        setDragging(false);
        takeFile(pickDropped(event.dataTransfer));
      },
      [pickDropped, takeFile],
    );

    // 整块表单都是投放目标：拖偏一点（掉在报告或表单上）也要能收。
    const isFileDrag = (event) => {
      const types = (event.dataTransfer && event.dataTransfer.types) || [];
      return Array.prototype.indexOf.call(types, "Files") >= 0;
    };

    const onZoneDragOver = useCallback((event) => {
      if (!isFileDrag(event)) return;
      event.preventDefault();
      event.stopPropagation();
      setDragging(true);
    }, []);

    const onZoneDragLeave = useCallback((event) => {
      event.stopPropagation();
      setDragging(false);
    }, []);

    /**
     * 选完包就带着当前表单去市场干跑一次（`/offline-market/skills/inspect`）：
     * 解包、闸门、slug 归属、版本冲突全在填表阶段回显，而不是点了发布才报第
     * 一个错。改了版本或标识符会重跑，因为「版本已存在」取决于它们。
     */
    useEffect(() => {
      if (!archive) return;
      const local = precheckArchive(archive, limits);
      if (local) {
        setReport(null);
        return;
      }
      const params = new URLSearchParams({
        name: derivedName.trim() || archive.name.replace(/\.zip$/i, "") || "untitled",
        version: stableVersion.trim(),
        visibility,
      });
      if (slugEdited.current && stableSlug.trim()) params.set("slug", stableSlug.trim());
      const signature = `${pickRevision}|${params.toString()}`;
      // 同一份表单只干跑一次。去重必须在分配 seq 之前：否则「防抖追上来的
      // 第二次 effect」会把 seq 抬上去却不发请求，前一次请求的回答就全被丢弃，
      // 页面永远卡在「正在校验压缩包…」、发布按钮一直灰着。
      if (lastInspect.current === signature) return;
      lastInspect.current = signature;
      const seq = ++inspectSeq.current;
      setInspectError(null);
      setInspecting(true);
      request(`/offline-market/skills/inspect?${params}`, {
        method: "POST",
        headers: { "content-type": "application/zip" },
        body: archive.bytes,
      })
        .then((body) => {
          if (seq === inspectSeq.current) setReport(body);
        })
        .catch((err) => {
          // 试运行只是相机预览：拿不到报告也能发布，交给后端兜底——但失败
          // 必须显示出来，否则「选完包没反应」比报错更难查。
          if (seq !== inspectSeq.current) return;
          lastInspect.current = ""; // 允许重试同一份表单
          const reason = (err && err.message) || String(err);
          logger.warn("试运行失败（不影响发布）", err);
          setInspectError(
            err && err.status === 404
              ? `插件还没加载试运行接口（${reason}）：重启 QwenPaw 后重试，或直接发布让市场校验。`
              : `${reason}。可以直接发布，市场会在发布时再校验一次。`,
          );
        })
        .finally(() => {
          if (seq === inspectSeq.current) setInspecting(false);
        });
    }, [archive, derivedName, stableSlug, stableVersion, visibility, limits, pickRevision]);

    const problems = (report && report.problems) || [];
    const blocked = !archive || problems.length > 0;
    const retryInspect = useCallback(() => {
      lastInspect.current = "";
      setInspectError(null);
      setPickRevision((value) => value + 1);
    }, []);

    const reset = useCallback(() => {
      setArchive(null);
      setReport(null);
      setInspectError(null);
      setFileError(null);
      setResult(null);
      inspectSeq.current += 1;
      lastInspect.current = "";
      setInspecting(false);
    }, []);

    const submit = useCallback(async () => {
      if (!archive) {
        setFileError("请先选择技能压缩包");
        return;
      }
      const problem = precheckArchive(archive, limits);
      if (problem) {
        logger.warn(`发布预检未通过：${problem}`);
        setFileError(problem);
        return;
      }
      if (problems.length) {
        logger.warn(`试运行仍有 ${problems.length} 个问题，已阻止提交`);
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
        setResult({
          slug: body.slug,
          version: body.version,
          fileCount: body.file_count,
          name: params.get("name"),
        });
        setArchive(null);
        setReport(null);
        onPublished && onPublished();
      } catch (err) {
        logger.error(`发布失败（${Date.now() - started}ms）`, err);
        setFileError(`${err.code ? `[${err.code}] ` : ""}${err.message}`);
      } finally {
        setBusy(false);
      }
    }, [
      archive,
      limits,
      name,
      slug,
      version,
      visibility,
      problems,
      onNotice,
      onPublished,
    ]);

    const step = result ? 2 : archive ? 1 : 0;
    const steps = Steps
      ? h(Steps, {
          size: "small",
          current: step,
          items: [
            { title: "选择压缩包" },
            { title: "校验信息" },
            { title: "发布完成" },
          ],
          style: { marginBottom: 16 },
        })
      : null;

    const manifest = report && report.manifest;
    const existing = report && report.existing;

    const reportPanel = !archive
      ? null
      : inspecting && !report
        ? h(Text, { type: "secondary" }, "正在校验压缩包…")
        : inspectError
          ? h(Alert, {
              type: "warning",
              showIcon: true,
              message: "试运行没跑起来",
              description: h(
                Space,
                { direction: "vertical", size: 8 },
                h(
                  Text,
                  { style: { fontSize: 12 } },
                  `已选 ${archive.name} · ${formatBytes(archive.bytes.byteLength)}`,
                ),
                h(Text, { type: "secondary", style: { fontSize: 12 } }, inspectError),
                h(
                  Button,
                  { size: "small", onClick: retryInspect, disabled: inspecting },
                  "重试",
                ),
              ),
            })
          : report
          ? h(
              "div",
              { style: { display: "grid", gap: 10 } },
              h(
                "div",
                { style: { display: "flex", flexWrap: "wrap", gap: 8 } },
                h(ReportStat, {
                  label: "文件",
                  value: `${report.file_count} 个`,
                  palette,
                }),
                h(ReportStat, {
                  label: "体积",
                  value: formatBytes(report.size_bytes),
                  palette,
                }),
                h(ReportStat, {
                  label: "标识符",
                  value: report.slug || "—",
                  palette,
                }),
                manifest
                  ? h(ReportStat, {
                      label: "SKILL.md 名称",
                      value: manifest.name || "—",
                      palette,
                    })
                  : null,
              ),
              manifest && manifest.description
                ? h(
                    "div",
                    {
                      style: {
                        fontSize: 12,
                        color: palette.muted,
                        padding: "0 2px",
                        lineHeight: 1.6,
                      },
                    },
                    manifest.description,
                  )
                : null,
              existing
                ? h(
                    Alert,
                    {
                      type: existing.mine ? "info" : "warning",
                      showIcon: true,
                      message: existing.mine
                        ? `这个标识符已在市场中（最新版本 ${existing.latest_version || "—"}）`
                        : `标识符属于其他团队（${existing.owner_tenant}）`,
                      description:
                        existing.mine && report.next_version
                          ? h(
                              Space,
                              { size: 6, wrap: true },
                              h(
                                Text,
                                { type: "secondary" },
                                `继续迭代就用 ${report.next_version}`,
                              ),
                              h(
                                Button,
                                {
                                  size: "small",
                                  onClick: () => setVersion(report.next_version),
                                },
                                `用 ${report.next_version}`,
                              ),
                            )
                          : null,
                    },
                  )
                : null,
              problems.length
                ? h(Alert, {
                    type: "error",
                    showIcon: true,
                    message: `校验未通过（${problems.length} 个问题）`,
                    description: h(
                      "ul",
                      { style: { margin: 0, paddingLeft: 18, lineHeight: 1.8 } },
                      problems.map((item, index) =>
                        h(
                          "li",
                          { key: `${item.code}-${index}` },
                          h(Tag, { color: "red", style: { marginRight: 6 } }, item.code),
                          item.message,
                        ),
                      ),
                    ),
                  })
                : h(Alert, {
                    type: "success",
                    showIcon: true,
                    message: "校验通过，可以发布",
                  }),
            )
          : null;

    const footer = h(
      "div",
      {
        style: {
          display: "flex",
          alignItems: "center",
          gap: 10,
          flexWrap: "wrap",
          marginTop: 4,
          padding: "10px 12px",
          borderRadius: 10,
          background: palette.surfaceAlt,
          border: palette.border,
        },
      },
      h(
        "span",
        { style: { fontSize: 12, color: palette.muted } },
        archive
          ? `${archive.name} · ${formatBytes(archive.bytes.byteLength)}${
              report ? ` · ${report.file_count} 个文件` : ""
            }`
          : "尚未选择压缩包",
      ),
      h("span", { style: { flex: "1 1 24px" } }),
      h(Button, { onClick: reset, disabled: busy || !archive }, "清空"),
      h(
        Button,
        {
          type: "primary",
          loading: busy,
          disabled: blocked || inspecting,
          onClick: submit,
        },
        busy ? "发布中…" : "立即发布",
      ),
    );

    return h(
      Card,
      {
        size: "small",
        title: "发布技能",
        extra: limits
          ? Tooltip
            ? h(
                Tooltip,
                {
                  title: `后端限额：${limits.max_files} 个文件 / 单文件 ${formatBytes(
                    limits.max_file_bytes,
                  )} / 合计 ${formatBytes(limits.max_bundle_bytes)}`,
                },
                h(Text, { type: "secondary" }, "市场校验规则"),
              )
            : h(Text, { type: "secondary" }, "市场校验规则")
          : null,
      },
      steps,
      result
        ? h(
            "div",
            {
              style: {
                display: "grid",
                gap: 12,
                justifyItems: "center",
                textAlign: "center",
                padding: "20px 12px",
              },
            },
            h(
              "div",
              {
                style: {
                  width: 44,
                  height: 44,
                  borderRadius: 22,
                  display: "grid",
                  placeItems: "center",
                  background: palette.dark
                    ? "rgba(34, 197, 94, 0.18)"
                    : "rgba(34, 197, 94, 0.12)",
                  color: "#22c55e",
                  fontSize: 22,
                },
              },
              "✓",
            ),
            h("span", { style: { fontSize: 16, fontWeight: 600, color: palette.ink } }, "发布成功"),
            h(
              Text,
              { type: "secondary" },
              `${result.slug}@${result.version} · ${result.fileCount} 个文件 · 已在市场中`,
            ),
            h(
              Space,
              null,
              h(
                Button,
                {
                  type: "primary",
                  onClick: () => {
                    setResult(null);
                    // 保留名称/标识符：接着发下一个版本是最常见的动作。
                  },
                },
                "再发布一个版本",
              ),
              h(
                Button,
                {
                  onClick: () => {
                    setResult(null);
                    setName("");
                    setSlug("");
                    setVersion("1.0.0");
                    slugEdited.current = false;
                  },
                },
                "发布另一个技能",
              ),
            ),
          )
        : h(
            "div",
            {
              style: { display: "grid", gap: 18, maxWidth: 760 },
              // 投放目标扩大到整块表单：拖偏一点也不能「没反应」。
              onDragOver: (event) => {
                if (busy || !isFileDrag(event)) return;
                event.preventDefault();
                setDragging(true);
              },
              onDragLeave: () => setDragging(false),
              onDrop,
            },
            h(
              "div",
              { style: { display: "grid", gap: 10 } },
              h(FormSection, {
                index: 1,
                title: "技能压缩包",
                hint: "解包与校验在本地完成后发给市场",
                palette,
              }),
              h(PackageDropZone, {
                archive,
                dragging,
                disabled: busy,
                onDragOver: onZoneDragOver,
                onDragLeave: onZoneDragLeave,
                onDrop,
                onPick,
                onClear: reset,
                palette,
                limits,
              }),
              reportPanel,
            ),
            h(
              "div",
              { style: { display: "grid", gap: 12 } },
              h(FormSection, {
                index: 2,
                title: "发布信息",
                hint: "标识符与版本决定了市场的安装路径",
                palette,
              }),
              h(
                "div",
                { style: { display: "flex", flexWrap: "wrap", gap: 12 } },
                h(
                  FormField,
                  {
                    label: "名称",
                    palette,
                    hint: "市场列表里显示的名字",
                    grow: 2,
                  },
                  Input
                    ? h(Input, {
                        value: name,
                        placeholder: "例如：周报助手",
                        onChange: (event) => setName(event.target.value),
                      })
                    : null,
                ),
                h(
                  FormField,
                  {
                    label: "标识符",
                    palette,
                    hint: "按名称推导（中文转拼音，如 zhoubaozhushou）",
                  },
                  Input
                    ? withAddon(
                        h(Input, {
                          value: slug,
                          placeholder: "zhoubaozhushou",
                          onChange: (event) => {
                            slugEdited.current = true;
                            setSlug(event.target.value);
                          },
                        }),
                        h(
                          Button,
                          {
                            onClick: () => {
                              // 交还给自动推导：中文名会重新转成拼音。
                              slugEdited.current = false;
                              setSlug("");
                              setPickRevision((value) => value + 1);
                            },
                          },
                          "重算",
                        ),
                      )
                    : null,
                ),
              ),
              h(
                "div",
                { style: { display: "flex", flexWrap: "wrap", gap: 12 } },
                h(
                  FormField,
                  {
                    label: "版本",
                    palette,
                    hint:
                      existing && existing.mine && report.next_version
                        ? `已发布过，建议 ${report.next_version}`
                        : "同一个标识符 + 版本只能发布一次",
                  },
                  Input
                    ? h(Input, {
                        value: version,
                        placeholder: "1.0.0",
                        onChange: (event) => setVersion(event.target.value),
                      })
                    : null,
                ),
                h(
                  FormField,
                  {
                    label: "可见性",
                    palette,
                    hint: "公开后其他租户也能安装",
                    grow: 2,
                  },
                  Radio && Radio.Group
                    ? h(
                        Radio.Group,
                        {
                          value: visibility,
                          onChange: (event) => setVisibility(event.target.value),
                          optionType: "button",
                          buttonStyle: "solid",
                          options: [
                            { value: "public", label: "公开" },
                            { value: "tenant", label: "仅本租户" },
                          ],
                        },
                      )
                    : Select
                      ? h(Select, {
                          value: visibility,
                          style: { width: "100%" },
                          onChange: setVisibility,
                          options: [
                            { value: "public", label: "公开（全部租户）" },
                            { value: "tenant", label: "仅本租户" },
                          ],
                        })
                      : null,
                ),
              ),
              h(
                "div",
                { style: { fontSize: 12, color: palette.faint, lineHeight: 1.7 } },
                publisherName
                  ? `发布者：${publisherName}（取当前登录账号，市场列表就显示它）`
                  : "发布者：租户 id（console 未提供登录用户名）",
              ),
            ),
            fileError
              ? h(Alert, { type: "error", showIcon: true, message: fileError, closable: true })
              : null,
            footer,
          ),
      h(
        Paragraph,
        { type: "secondary", style: { marginTop: 12, marginBottom: 0 } },
        "版本不可变：同一个标识符 + 版本只能发布一次；下架与审核由管理员在市场后台处理。",
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

  // ---------------------------------------------------------- agent market ---

  function AgentInstallDialog({
    target,
    name,
    setName,
    version,
    setVersion,
    installing,
    step,
    error,
    result,
    onClose,
    onInstall,
  }) {
    if (!target) return null;
    const lines = [];
    if (error) {
      lines.push(`${error.code ? `[${error.code}] ` : ""}${error.message}`);
      if (error.step) lines.push(`失败步骤：${error.step}`);
      if (error.rollback_failed) {
        lines.push("自动回滚失败：宿主里可能残留一个未完成的智能体，请到智能体列表检查。");
      } else if (error.workspace_kept) {
        lines.push(`配置已回滚；工作区保留在 ${error.workspace_kept}，可手工清理。`);
      }
    }

    const body = h(
      "div",
      { style: { display: "grid", gap: 10 } },
      h(
        Space,
        { wrap: true },
        h(Text, { strong: true }, target.name),
        h(Text, { type: "secondary", code: true }, `${target.slug}@${target.latest_version || "-"}`),
      ),
      h(
        Space,
        { wrap: true },
        h(Text, null, "本机显示名"),
        Input
          ? h(Input, {
              value: name,
              disabled: installing || Boolean(result),
              style: { width: 260 },
              onChange: (event) => setName(event.target.value),
            })
          : null,
        h(Text, null, "版本"),
        Input
          ? h(Input, {
              value: version,
              disabled: installing || Boolean(result),
              style: { width: 120 },
              placeholder: "latest",
              onChange: (event) => setVersion(event.target.value),
            })
          : null,
      ),
      h(
        Paragraph,
        { type: "secondary", style: { margin: 0 } },
        "安装会在本机新建一个独立智能体：写入人设、由 QwenPaw 宿主扫描并安装捆绑技能、应用安全配置子集。不会覆盖现有智能体。",
      ),
      installing
        ? h(Alert, { type: "info", showIcon: true, message: "正在安装", description: step })
        : null,
      error
        ? h(Alert, {
            type: "error",
            showIcon: true,
            message: "安装失败",
            description: h(
              "ul",
              { style: { margin: 0, paddingLeft: 18 } },
              lines.map((line, index) => h("li", { key: index }, line)),
            ),
          })
        : null,
      result
        ? h(Alert, {
            type: "success",
            showIcon: true,
            message: `已安装 ${result.name || target.name}`,
            description: `agent_id：${result.agent_id}${
              result.workspace_dir ? `｜工作区：${result.workspace_dir}` : ""
            }`,
          })
        : null,
    );

    const footer = result
      ? [h(Button, { key: "close", onClick: onClose }, "关闭")]
      : [
          h(Button, { key: "cancel", onClick: onClose, disabled: installing }, "取消"),
          h(
            Button,
            { key: "install", type: "primary", loading: installing, onClick: onInstall },
            "开始安装",
          ),
        ];

    if (Modal) {
      return h(
        Modal,
        {
          open: true,
          title: "安装智能体",
          footer,
          closable: !installing,
          maskClosable: !installing,
          onCancel: onClose,
        },
        body,
      );
    }
    return h(
      Card,
      { size: "small", title: "安装智能体" },
      body,
      h("div", { style: { marginTop: 12 } }, h(Space, null, footer)),
    );
  }

  function AgentMarketTab({
    tenant,
    installed,
    installedVersions,
    markInstalled,
    onNotice,
    refreshToken,
    onInstalled,
  }) {
    const catalogue = useCatalogue({ kind: "agents", refreshToken, onNotice });
    const [target, setTarget] = useState(null);
    const [targetName, setTargetName] = useState("");
    const [installVersion, setInstallVersion] = useState("latest");
    const [installing, setInstalling] = useState(false);
    const [installStep, setInstallStep] = useState("");
    const [installError, setInstallError] = useState(null);
    const [installResult, setInstallResult] = useState(null);

    useEffect(() => {
      if (!installing) return undefined;
      const steps = [
        "创建智能体…",
        "等待工作区就绪…",
        "写入人设…",
        "安装技能（宿主扫描）…",
        "应用配置…",
      ];
      setInstallStep(steps[0]);
      let index = 0;
      const timer = setInterval(() => {
        index = Math.min(index + 1, steps.length - 1);
        setInstallStep(steps[index]);
      }, 2500);
      return () => clearInterval(timer);
    }, [installing]);

    const openInstall = useCallback((row) => {
      setTarget(row);
      setTargetName(row.name || "");
      setInstallVersion("latest");
      setInstallError(null);
      setInstallResult(null);
      setInstallStep("创建智能体…");
    }, []);

    const closeInstall = useCallback(() => {
      if (installing) return;
      setTarget(null);
      setInstallError(null);
      setInstallResult(null);
    }, [installing]);

    const runInstall = useCallback(async () => {
      if (!target) return;
      setInstalling(true);
      setInstallError(null);
      setInstallResult(null);
      const started = Date.now();
      try {
        logger.info(`开始安装智能体 ${target.slug}`);
        const body = await postJsonSlow(
          `/offline-market/agents/${encodeURIComponent(target.slug)}/install`,
          {
            version: installVersion.trim() || "latest",
            target_name: targetName.trim() || undefined,
          },
        );
        logger.info(`智能体安装完成 ${target.slug}（${Date.now() - started}ms）`, body);
        markInstalled(target.slug, body.version);
        if (onInstalled) onInstalled(target.slug);
        setInstallResult(body);
        onNotice({
          type: "success",
          text: `已安装智能体 ${body.name || target.name}（${body.agent_id}）`,
        });
      } catch (err) {
        logger.error(`智能体安装失败 ${target.slug}（${Date.now() - started}ms）`, err);
        setInstallError(err);
      } finally {
        setInstalling(false);
        setInstallStep("");
      }
    }, [target, targetName, installVersion, markInstalled, onNotice, onInstalled]);

    return h(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: 12 } },
      h(CatalogueView, {
        kind: "agents",
        tenant,
        catalogue,
        installed,
        installedVersions,
        busySlug: installing && target ? target.slug : "",
        onInstall: openInstall,
        emptyText: "还没有智能体",
        searchPlaceholder: "搜索智能体名或标识符",
      }),
      h(AgentInstallDialog, {
        target,
        name: targetName,
        setName: setTargetName,
        version: installVersion,
        setVersion: setInstallVersion,
        installing,
        step: installStep,
        error: installError,
        result: installResult,
        onClose: closeInstall,
        onInstall: runInstall,
      }),
    );
  }

  function AgentPublishTab({ onNotice, onPublished, active = true }) {
    const { palette } = useMarketTheme();
    const [agents, setAgents] = useState([]);
    const [loadingAgents, setLoadingAgents] = useState(true);
    const [source, setSource] = useState("");
    const [slug, setSlug] = useState("");
    const [version, setVersion] = useState("1.0.0");
    const [visibility, setVisibility] = useState("public");
    const [preview, setPreview] = useState(null);
    const [previewBusy, setPreviewBusy] = useState(false);
    const [busy, setBusy] = useState(false);
    // 发布成功的结果卡（跟技能发布页一致）：打包+上传可能要几十秒，只在页面
    // 顶部弹一条 Alert 的话，滚到下面的用户根本看不到哪儿变了。
    const [result, setResult] = useState(null);
    const [elapsed, setElapsed] = useState(0);
    const [error, setError] = useState("");
    // 跟技能发布页一致：切走再切回来，重新显示发布表单而不是上次的成功卡。
    const clearResult = useCallback(() => setResult(null), []);
    useResetOnShow(active, clearResult);
    const publisherName = useCurrentUserName();

    // 没有真进度可拿（一次打包+上传就一个请求），就把等待秒数报出来。
    useEffect(() => {
      if (!busy) {
        setElapsed(0);
        return undefined;
      }
      const started = Date.now();
      const timer = setInterval(
        () => setElapsed(Math.round((Date.now() - started) / 1000)),
        1000,
      );
      return () => clearInterval(timer);
    }, [busy]);

    const loadAgents = useCallback(async () => {
      setLoadingAgents(true);
      try {
        const body = await getJson("/offline-market/host-agents");
        const items = body.items || [];
        setAgents(items);
        logger.debug(
          `本机可发布智能体 ${items.length} 个`,
          items.map((item) => `${item.id}(${item.name})`),
        );
      } catch (err) {
        logger.error("本机智能体列表加载失败", err);
        setError(err.message);
      } finally {
        setLoadingAgents(false);
      }
    }, []);

    useEffect(() => {
      loadAgents();
    }, [loadAgents]);

    const selected = useMemo(() => agents.find((item) => item.id === source) || null, [agents, source]);

    const doPreview = useCallback(async () => {
      if (!source) {
        setError("请先选择要发布的智能体");
        return;
      }
      setPreviewBusy(true);
      setError("");
      try {
        const body = await postJsonSlow("/offline-market/agents/preview", {
          source_agent_id: source,
        });
        setPreview(body);
        logger.info(
          `智能体快照预览 ${source}：${body.file_count} 个文件，${body.skills.length} 个技能`,
        );
      } catch (err) {
        logger.error("智能体快照预览失败", err);
        setError(`${err.code ? `[${err.code}] ` : ""}${err.message}`);
        setPreview(null);
      } finally {
        setPreviewBusy(false);
      }
    }, [source]);

    const submit = useCallback(async () => {
      if (!source) {
        setError("请先选择要发布的智能体");
        return;
      }
      if (!version.trim()) {
        setError("请填写版本号");
        return;
      }
      setBusy(true);
      setError("");
      setResult(null);
      try {
        const publisher = await currentUserName();
        const body = await postJsonSlow("/offline-market/agents/publish", {
          source_agent_id: source,
          slug: slug.trim() || undefined,
          version: version.trim(),
          visibility,
          publisher_name: publisher || undefined,
        });
        logger.info(`智能体发布成功 ${body.slug}@${body.version}`, body);
        setResult({
          slug: body.slug,
          version: body.version,
          files: body.file_count,
          skills: body.skill_count,
          visibility,
          name: body.name || (selected && (selected.name || selected.id)) || source,
        });
        onNotice({
          type: "success",
          text: `已发布 ${body.slug}@${body.version}（${body.file_count} 个文件 / ${body.skill_count} 个技能）`,
        });
        setPreview(null);
        onPublished && onPublished();
      } catch (err) {
        logger.error("智能体发布失败", err);
        setError(`${err.code ? `[${err.code}] ` : ""}${err.message}`);
      } finally {
        setBusy(false);
      }
    }, [source, slug, version, visibility, selected, onNotice, onPublished]);

    const picker = Select
      ? h(Select, {
          value: source || undefined,
          style: { width: 360 },
          placeholder: "选择本机智能体",
          loading: loadingAgents,
          disabled: busy,
          onChange: (value) => {
            setSource(value);
            setPreview(null);
          },
          options: agents.map((item) => ({
            value: item.id,
            label: `${item.name || item.id} (${item.id})`,
          })),
        })
      : h(
          "select",
          {
            value: source,
            style: { width: 360, padding: "4px 8px" },
            onChange: (event) => {
              setSource(event.target.value);
              setPreview(null);
            },
          },
          h("option", { value: "" }, "选择本机智能体"),
          agents.map((item) => h("option", { key: item.id, value: item.id }, `${item.name || item.id} (${item.id})`)),
        );

    // 发布成功后的结果卡：点完按钮原地就要有东西变。
    const successPanel = !result
      ? null
      : h(
      "div",
      {
        style: {
          display: "grid",
          gap: 12,
          justifyItems: "center",
          textAlign: "center",
          padding: "20px 12px",
        },
      },
      h(
        "div",
        {
          style: {
            width: 44,
            height: 44,
            borderRadius: 22,
            display: "grid",
            placeItems: "center",
            background: palette.dark ? "rgba(34, 197, 94, 0.18)" : "rgba(34, 197, 94, 0.12)",
            color: "#22c55e",
            fontSize: 22,
          },
        },
        "✓",
      ),
      h("span", { style: { fontSize: 16, fontWeight: 600, color: palette.ink } }, "发布成功"),
      h(
        Text,
        { type: "secondary" },
        `${result.slug}@${result.version} · ${result.files} 个文件 · ${result.skills} 个技能 · ${
          result.visibility === "public" ? "公开" : "仅本租户"
        }`,
      ),
      h(
        Paragraph,
        { type: "secondary", style: { margin: 0, fontSize: 12 } },
        `${result.name} 已上架，其他用户现在就能在「智能体 → 市场」里装到自己的 runtime。`,
      ),
      h(
        Space,
        { wrap: true },
        h(
          Button,
          {
            type: "primary",
            onClick: () => {
              setResult(null);
              // 同名智能体接着发下一个版本是最常见的动作，版本号顺手抬一位。
              const parts = String(result.version || "").split(".");
              if (parts.length === 3 && parts.every((part) => /^\d+$/.test(part))) {
                setVersion(`${parts[0]}.${parts[1]}.${Number(parts[2]) + 1}`);
              }
            },
          },
          "再发布一个版本",
        ),
        h(
          Button,
          {
            onClick: () => {
              setResult(null);
              setSource("");
              setSlug("");
              setVersion("1.0.0");
              setPreview(null);
            },
          },
          "发布另一个智能体",
        ),
      ),
    );

    return h(
      Card,
      { size: "small", title: "发布智能体" },
      result
        ? successPanel
        : h(
        "div",
        { style: { display: "grid", gap: 10, maxWidth: 760 } },
        agents.length
          ? null
          : h(Alert, {
              type: "warning",
              showIcon: true,
              message: "本机没有可发布的智能体",
              description: "先在 QwenPaw 里创建一个智能体，再回到这里发布。",
            }),
        h(
          Space,
          { wrap: true },
          h(Text, null, "来源智能体"),
          picker,
          h(Button, { onClick: loadAgents, loading: loadingAgents, disabled: busy }, "刷新"),
        ),
        selected
          ? h(
              Paragraph,
              { type: "secondary", style: { margin: 0 } },
              `${selected.name || selected.id}｜${selected.description || "无描述"}｜工作区 ${selected.workspace_dir || "-"}`,
            )
          : null,
        h(
          Space,
          { wrap: true },
          h(Text, null, "标识符（可选）"),
          Input
            ? h(Input, {
                value: slug,
                placeholder: "留空则按名称推导（中文名取拼音）",
                style: { width: 200 },
                disabled: busy,
                onChange: (event) => setSlug(event.target.value),
              })
            : null,
          h(Text, null, "版本"),
          Input
            ? h(Input, {
                value: version,
                placeholder: "1.0.0",
                style: { width: 140 },
                disabled: busy,
                onChange: (event) => setVersion(event.target.value),
              })
            : null,
          h(Text, null, "可见性"),
          Select
            ? h(Select, {
                value: visibility,
                style: { width: 160 },
                disabled: busy,
                onChange: setVisibility,
                options: [
                  { value: "public", label: "公开（全部租户）" },
                  { value: "tenant", label: "仅本租户" },
                ],
              })
            : null,
        ),
        h(
          Space,
          null,
          h(
            Button,
            { onClick: doPreview, loading: previewBusy, disabled: !source || busy },
            "预览将打包内容",
          ),
          h(
            Button,
            { type: "primary", onClick: submit, loading: busy, disabled: !source || busy },
            busy ? "发布中…" : "立即发布",
          ),
        ),
        busy
          ? h(Alert, {
              type: "info",
              showIcon: true,
              message: `正在打包并上传智能体…已等待 ${elapsed}s`,
              description:
                "人设 MD 与已启用技能会一起打包上传，体量大的智能体可能要 1-3 分钟，发布完成前请不要关掉页面。",
            })
          : null,
        preview
          ? h(
              Card,
              { size: "small", title: "预览" },
              h(
                "div",
                { style: { display: "grid", gap: 6 } },
                h(Text, null, `人设文件：${preview.md_files.join("、") || "无"}`),
                h(Text, null, `技能：${preview.skills.join("、") || "无"}`),
                h(
                  Text,
                  { type: "secondary" },
                  `${preview.file_count} 个文件 / ${(preview.size_bytes / 1024).toFixed(1)} KB`,
                ),
              ),
            )
          : null,
        error ? h(Alert, { type: "error", showIcon: true, message: error }) : null,
        h(
          Text,
          { type: "secondary" },
          publisherName
            ? `发布者：${publisherName}（取当前登录账号）`
            : "发布者：租户 id（console 未提供登录用户名）",
        ),
      ),
      h(
        Paragraph,
        { type: "secondary", style: { marginTop: 12, marginBottom: 0 } },
        "只打包五种白名单人设 MD + 已启用技能；不会上传 MEMORY.md、会话、记忆、模型、渠道、MCP、邮箱或凭据。版本不可变。",
      ),
    );
  }

  function AgentMineTab({ onNotice, refreshToken }) {
    const [items, setItems] = useState([]);
    const [loading, setLoading] = useState(false);

    const load = useCallback(async () => {
      setLoading(true);
      try {
        const body = await getJson("/offline-market/agents/mine");
        const next = body.items || [];
        setItems(next);
        logger.debug(`我的智能体发布 ${next.length} 条`);
      } catch (err) {
        logger.error("我的智能体发布加载失败", err);
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
        logger.info(`${action === "withdraw" ? "撤回" : "恢复"}智能体 ${row.slug}`);
        try {
          await postJson(`/offline-market/agents/${encodeURIComponent(row.slug)}/${action}`);
          onNotice({
            type: "success",
            text: action === "withdraw" ? `已撤回 ${row.slug}` : `已恢复 ${row.slug}`,
          });
          load();
        } catch (err) {
          logger.error(`${action} 智能体失败 ${row.slug}`, err);
          onNotice({ type: "error", text: err.message });
        }
      },
      [load, onNotice],
    );

    const stateOf = (row) =>
      row.takedown_at ? "taken_down" : row.withdrawn_at ? "withdrawn" : "active";

    const columns = [
      {
        title: "智能体",
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
        title: "状态",
        width: 120,
        render: (_, row) => {
          const state = stateOf(row);
          return h(Tag, { color: STATE_COLOR[state] }, STATE_LABEL[state]);
        },
      },
      { title: "技能", dataIndex: "skill_count", width: 70 },
      { title: "版本数", dataIndex: "versions", width: 80 },
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
            locale: { emptyText: h(Empty, { description: "还没有发布过智能体" }) },
          })
        : h("div", null, items.length, " 个智能体");
  }

  function AgentInstalledTab({ onNotice, refreshToken, onUninstalled }) {
    const [items, setItems] = useState([]);
    const [loading, setLoading] = useState(false);
    const [busyId, setBusyId] = useState("");

    const load = useCallback(async () => {
      setLoading(true);
      try {
        const body = await getJson("/offline-market/agents/installed");
        const next = body.items || [];
        setItems(next);
        logger.debug(
          `已安装智能体 ${next.length} 个`,
          next.map((item) => `${item.agent_id}->${item.slug}/${item.state}`),
        );
      } catch (err) {
        logger.error("已安装智能体加载失败", err);
        onNotice({ type: "error", text: err.message });
      } finally {
        setLoading(false);
      }
    }, [onNotice]);

    useEffect(() => {
      load();
    }, [load, refreshToken]);

    const remove = useCallback(
      async (row) => {
        setBusyId(row.agent_id);
        try {
          const body = await postJson(
            `/offline-market/agents/${encodeURIComponent(row.agent_id)}/uninstall`,
          );
          logger.info(`已卸载智能体 ${row.agent_id}`, body);
          onNotice({
            type: "success",
            text: `已卸载 ${row.name || row.agent_id}${
              body.workspace_kept ? `；工作区保留在 ${body.workspace_kept}` : ""
            }`,
          });
          onUninstalled && onUninstalled(row.slug);
          load();
        } catch (err) {
          logger.error(`卸载智能体失败 ${row.agent_id}`, err);
          onNotice({ type: "error", text: err.message });
        } finally {
          setBusyId("");
        }
      },
      [load, onNotice, onUninstalled],
    );

    const askRemove = useCallback(
      (row) => {
        const detail = `只摘除宿主里的智能体配置；工作区${
          row.workspace_dir ? `（${row.workspace_dir}）` : ""
        }会保留，需要时请手工清理。`;
        if (Modal && typeof Modal.confirm === "function") {
          Modal.confirm({
            title: `卸载 ${row.name || row.agent_id}？`,
            content: detail,
            okText: "卸载",
            okType: "danger",
            cancelText: "取消",
            onOk: () => remove(row),
          });
          return;
        }
        if (window.confirm(`卸载 ${row.name || row.agent_id}？\n${detail}`)) {
          remove(row);
        }
      },
      [remove],
    );

    const columns = [
      {
        title: "智能体",
        render: (_, row) =>
          h(
            "div",
            null,
            h("div", null, h(Text, { strong: true }, row.name || row.agent_id)),
            h(Text, { type: "secondary", code: true }, row.agent_id),
          ),
      },
      {
        title: "市场标识",
        render: (_, row) =>
          h(Text, { code: true }, `${row.slug || "-"}@${row.version || "-"}`),
      },
      {
        title: "市场状态",
        width: 150,
        render: (_, row) =>
          h(
            Tag,
            { color: AGENT_STATE_COLOR[row.state] || "default" },
            AGENT_STATE_LABEL[row.state] || row.state || "未知",
          ),
      },
      {
        title: "工作区",
        dataIndex: "workspace_dir",
        ellipsis: true,
        render: (value) =>
          value ? h("span", { title: value }, value) : h(Text, { type: "secondary" }, "—"),
      },
      {
        title: "操作",
        width: 120,
        render: (_, row) =>
          h(
            Button,
            {
              size: "small",
              danger: true,
              loading: busyId === row.agent_id,
              onClick: () => askRemove(row),
            },
            "卸载",
          ),
      },
    ];

    return loading
      ? h(Spin)
      : Table
        ? h(Table, {
            rowKey: "agent_id",
            size: "small",
            pagination: false,
            dataSource: items,
            columns,
            locale: { emptyText: h(Empty, { description: "还没有从本市场安装过智能体" }) },
          })
        : h("div", null, items.length, " 个已安装智能体");
  }

  function AgentMarketPage({ config, onNotice, active = true }) {
    const [marketToken, setMarketToken] = useState(0);
    // See SkillsPage: a mounted pane must know when it is back in view.
    const [activeKey, setActiveKey] = useState("market");
    const [revision, setRevision] = useState(0);
    const [installed, setInstalled] = useState(() => new Set());
    const [installedVersions, setInstalledVersions] = useState(() => ({}));

    const loadInstalled = useCallback(async () => {
      try {
        const body = await getJson("/offline-market/agents/installed");
        const next = new Set();
        const versions = {};
        (body.items || []).forEach((item) => {
          if (!item.slug) return;
          next.add(item.slug);
          if (item.version) versions[item.slug] = String(item.version);
        });
        setInstalled(next);
        setInstalledVersions(versions);
        logger.debug(`智能体已安装集合 ${next.size} 个`, [...next]);
      } catch (err) {
        logger.warn("智能体已安装集合加载失败（安装按钮照常可点）", err);
      }
    }, []);

    useEffect(() => {
      loadInstalled();
    }, [loadInstalled, revision]);

    const markInstalled = useCallback((slug, version) => {
      if (!slug) return;
      setInstalled((previous) => {
        if (previous.has(slug)) return previous;
        return new Set(previous).add(slug);
      });
      if (version) {
        setInstalledVersions((previous) =>
          previous[slug] === String(version)
            ? previous
            : { ...previous, [slug]: String(version) },
        );
      }
    }, []);

    const onPublished = useCallback(() => {
      setMarketToken((value) => value + 1);
    }, []);

    // Installing an agent changes both the 已安装 tab and the local installed
    // set; bump the revision so the tab reloads instead of staying stale.
    const onAgentInstalled = useCallback(() => {
      setRevision((value) => value + 1);
    }, []);

    const onUninstalled = useCallback((slug) => {
      setRevision((value) => value + 1);
      if (!slug) return;
      setInstalled((previous) => {
        if (!previous.has(slug)) return previous;
        const next = new Set(previous);
        next.delete(slug);
        return next;
      });
      setInstalledVersions((previous) => {
        if (!previous[slug]) return previous;
        const next = { ...previous };
        delete next[slug];
        return next;
      });
    }, []);

    const tabs = [
      {
        key: "market",
        label: "市场",
        children: h(AgentMarketTab, {
          tenant: (config && config.tenant) || "",
          installed,
          installedVersions,
          markInstalled,
          onNotice,
          refreshToken: marketToken,
          onInstalled: onAgentInstalled,
        }),
      },
      {
        key: "publish",
        label: "发布",
        children: h(AgentPublishTab, {
          onNotice,
          onPublished,
          active: active && activeKey === "publish",
        }),
      },
      {
        key: "mine",
        label: "我的发布",
        children: h(AgentMineTab, { onNotice, refreshToken: marketToken }),
      },
      {
        key: "installed",
        label: "已安装",
        children: h(AgentInstalledTab, {
          onNotice,
          refreshToken: revision,
          onUninstalled,
        }),
      },
    ];

    return h(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: 12 } },
      h(Alert, {
        type: "info",
        showIcon: true,
        message: "智能体安装会在本机新建一个独立智能体",
        description:
          "人设与配置来自市场；捆绑技能仍由 QwenPaw 宿主扫描安装。卸载只摘除配置，不删除工作区。",
      }),
      Tabs
        ? h(Tabs, {
            items: tabs,
            defaultActiveKey: "market",
            onChange: (key) => setActiveKey(key),
          })
        : h("div", null, tabs.map((tab) => h("section", { key: tab.key }, tab.children))),
    );
  }

  // ------------------------------------------------------------------ page ---

  function SkillsPage({ config, limits, onNotice, active = true }) {
    const [refreshToken, setRefreshToken] = useState(0);
    // Which inner tab is on screen. antd keeps every visited pane mounted and
    // only hides it, so the 发布 pane needs to know when it is back in view.
    const [activeKey, setActiveKey] = useState("market");
    // Bumped only by a publish: refreshes the market list once without the
    // spinner flash that a tab switch would cause.
    const [marketToken, setMarketToken] = useState(0);

    const onPublished = useCallback(() => {
      setRefreshToken((value) => value + 1);
      setMarketToken((value) => value + 1);
    }, []);

    // Installing a skill changes the 已安装 tab only; the market list keeps its
    // scroll position and marks the row locally.
    const onInstalled = useCallback(() => {
      setRefreshToken((value) => value + 1);
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
          onInstalled,
        }),
      },
      {
        key: "publish",
        label: "发布",
        children: h(PublishTab, {
          limits,
          onNotice,
          onPublished,
          // 外层页签也算：技能 ↔ 智能体 ↔ 概览切回来同样是「重新进入」。
          active: active && activeKey === "publish",
        }),
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

    return Tabs
      ? h(Tabs, {
          items: tabs,
          defaultActiveKey: "market",
          onChange: (key) => setActiveKey(key),
        })
      : h("div", null, tabs.map((tab) => h("section", { key: tab.key }, tab.children)));
  }

  // ---------------------------------------------------------------- icons ---
  // Inline SVG keeps the dashboard independent of whichever icon package the
  // host bundle happens to expose, and avoids emoji fonts rendering differently
  // across Windows/Linux consoles.

  function MarketGlyph({ size = 18, color = "currentColor" }) {
    return h(
      "svg",
      {
        width: size,
        height: size,
        viewBox: "0 0 24 24",
        fill: "none",
        stroke: color,
        strokeWidth: 1.8,
        strokeLinecap: "round",
        strokeLinejoin: "round",
        "aria-hidden": "true",
      },
      h("rect", { key: "a", x: 3, y: 3, width: 7, height: 7, rx: 2 }),
      h("rect", { key: "b", x: 14, y: 3, width: 7, height: 7, rx: 2 }),
      h("rect", { key: "c", x: 3, y: 14, width: 7, height: 7, rx: 2 }),
      h("path", { key: "d", d: "M17.5 14v7M14 17.5h7" }),
    );
  }

  function SidebarMarketEntry() {
    return h(
      "a",
      {
        href: "/offline-market",
        title: "内网市场：技能与智能体",
        "aria-label": "打开内网市场",
        style: {
          display: "flex",
          alignItems: "center",
          gap: 10,
          width: "100%",
          boxSizing: "border-box",
          padding: "9px 11px",
          margin: "4px 0 2px",
          borderRadius: 11,
          border: "1px solid rgba(255, 255, 255, 0.16)",
          background: "linear-gradient(135deg, #6366f1 0%, #4f46e5 55%, #4338ca 100%)",
          boxShadow: "0 6px 16px rgba(79, 70, 229, 0.24)",
          color: "#ffffff",
          textDecoration: "none",
          fontSize: 13,
          fontWeight: 650,
          lineHeight: 1.2,
          whiteSpace: "nowrap",
          overflow: "hidden",
        },
      },
      h(MarketGlyph, { size: 18 }),
      h("span", { style: { flex: 1, textAlign: "left" } }, "内网市场"),
      h("span", { style: { fontSize: 15, opacity: 0.72 } }, "›"),
    );
  }

  function MetricIcon({ name, color = "currentColor", size = 24 }) {
    const common = {
      width: size,
      height: size,
      viewBox: "0 0 24 24",
      fill: "none",
      stroke: color,
      strokeWidth: 1.8,
      strokeLinecap: "round",
      strokeLinejoin: "round",
      "aria-hidden": "true",
    };
    if (name === "skills") {
      return h(
        "svg",
        common,
        h("rect", { key: "a", x: 3, y: 3, width: 7, height: 7, rx: 2 }),
        h("rect", { key: "b", x: 14, y: 3, width: 7, height: 7, rx: 2 }),
        h("rect", { key: "c", x: 3, y: 14, width: 7, height: 7, rx: 2 }),
        h("path", { key: "d", d: "M17.5 14v7M14 17.5h7" }),
      );
    }
    if (name === "agents") {
      return h(
        "svg",
        common,
        h("rect", { key: "a", x: 4, y: 7, width: 16, height: 12, rx: 4 }),
        h("path", { key: "b", d: "M12 3v4" }),
        h("circle", { key: "c", cx: 12, cy: 2.6, r: 1.2 }),
        h("circle", { key: "d", cx: 9, cy: 12.5, r: 1.2, fill: color, stroke: "none" }),
        h("circle", { key: "e", cx: 15, cy: 12.5, r: 1.2, fill: color, stroke: "none" }),
        h("path", { key: "f", d: "M9 16h6" }),
      );
    }
    if (name === "downloads") {
      return h(
        "svg",
        common,
        h("path", { key: "a", d: "M12 3v10.5" }),
        h("path", { key: "b", d: "m7.5 9.5 4.5 4.5 4.5-4.5" }),
        h("path", { key: "c", d: "M4 16.5v2A2.5 2.5 0 0 0 6.5 21h11a2.5 2.5 0 0 0 2.5-2.5v-2" }),
      );
    }
    if (name === "new") {
      return h(
        "svg",
        common,
        h("path", {
          key: "a",
          d: "m12 3 1.6 4.9L18.5 9.5l-4.9 1.6L12 16l-1.6-4.9L5.5 9.5l4.9-1.6L12 3Z",
        }),
        h("path", {
          key: "b",
          d: "M18.5 14.5 19.3 17l2.2.8-2.2.8-.8 2.4-.8-2.4-2.2-.8 2.2-.8.8-2.5Z",
        }),
      );
    }
    return null;
  }

  // --------------------------------------------------------------- overview ---

  const numberFormatter = new Intl.NumberFormat("zh-CN");

  function formatNumber(value) {
    const number = Number(value);
    return numberFormatter.format(Number.isFinite(number) ? number : 0);
  }

  function formatTime(seconds) {
    const date = seconds ? new Date(seconds * 1000) : new Date();
    try {
      return date.toLocaleString("zh-CN", {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      });
    } catch (err) {
      return date.toISOString();
    }
  }

  function OverviewTab() {
    const [data, setData] = useState(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState(null);

    const load = useCallback(async () => {
      setLoading(true);
      setError(null);
      logger.debug("加载 /offline-market/stats");
      try {
        const body = await getJson("/offline-market/stats");
        setData(body);
        logger.debug("概览统计", body);
      } catch (err) {
        logger.error("概览统计加载失败", err);
        setError(
          err.status === 404
            ? "当前市场后端版本不支持概览统计，请升级后端后重试。"
            : err.message,
        );
      } finally {
        setLoading(false);
      }
    }, []);

    useEffect(() => {
      load();
    }, [load]);

    const skills = (data && data.skills) || {};
    const agents = (data && data.agents) || {};
    const totals = (data && data.totals) || {};
    const skillTotal = Number(skills.total) || 0;
    const agentTotal = Number(agents.total) || 0;
    const itemTotal = skillTotal + agentTotal;
    const skillPct = itemTotal ? Math.round((skillTotal / itemTotal) * 100) : 0;
    const monthSkill = Number(skills.new_this_month) || 0;
    const monthAgent = Number(agents.new_this_month) || 0;
    const monthTotal = Number(totals.new_this_month) || monthSkill + monthAgent;
    const downloadTotal = Number(totals.downloads) || 0;

    const theme = useMarketTheme();
    const palette = theme.palette;
    const cardStyle = {
      position: "relative",
      overflow: "hidden",
      padding: 22,
      borderRadius: 18,
      background: palette.surface,
      border: palette.border,
      boxShadow: palette.shadow,
    };

    const hero = h(
      "div",
      {
        style: {
          position: "relative",
          overflow: "hidden",
          padding: "28px 32px",
          borderRadius: 24,
          color: "#ffffff",
          background:
            "radial-gradient(circle at 84% 18%, rgba(255,255,255,0.22), transparent 28%), linear-gradient(120deg, #4338ca 0%, #6366f1 48%, #0ea5e9 100%)",
          boxShadow: "0 18px 42px rgba(79, 70, 229, 0.28)",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 24,
          flexWrap: "wrap",
        },
      },
      h(
        "div",
        null,
        h(
          "div",
          {
            style: {
              display: "inline-flex",
              alignItems: "center",
              gap: 8,
              fontSize: 11,
              fontWeight: 700,
              letterSpacing: 1.4,
              textTransform: "uppercase",
              opacity: 0.82,
            },
          },
          h("span", {
            style: {
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: "#6ee7b7",
              boxShadow: "0 0 0 4px rgba(110, 231, 183, 0.18)",
            },
          }),
          "Live overview",
        ),
        h(
          "h2",
          {
            style: {
              margin: "10px 0 8px",
              fontSize: 30,
              lineHeight: 1.2,
              fontWeight: 800,
              letterSpacing: 0.2,
            },
          },
          "内网市场概览",
        ),
        h(
          "div",
          { style: { maxWidth: 580, fontSize: 14, lineHeight: 1.75, opacity: 0.86 } },
          "技能与智能体，一处掌握。统计公开内容与本租户私有内容，撤回或下架后不再计入总数。",
        ),
      ),
      h(
        "div",
        { style: { display: "flex", alignItems: "center", gap: 16, flexWrap: "wrap" } },
        h(
          "div",
          { style: { textAlign: "right" } },
          h("div", { style: { fontSize: 11, opacity: 0.68, letterSpacing: 0.6 } }, "更新时间"),
          h(
            "div",
            { style: { marginTop: 4, fontSize: 13, fontWeight: 600 } },
            formatTime(data && data.generated_at),
          ),
        ),
        h(
          Button,
          {
            loading,
            onClick: load,
            style: {
              color: "#ffffff",
              background: "rgba(255, 255, 255, 0.16)",
              borderColor: "rgba(255, 255, 255, 0.30)",
              borderRadius: 10,
              fontWeight: 600,
            },
          },
          "刷新数据",
        ),
      ),
    );

    if (loading && !data) {
      return h(
        "div",
        { style: { display: "flex", flexDirection: "column", gap: 16 } },
        hero,
        h(
          "div",
          {
            style: {
              minHeight: 260,
              display: "grid",
              placeItems: "center",
              borderRadius: 18,
              background: palette.surface,
              border: "1px solid rgba(148, 163, 184, 0.18)",
            },
          },
          h(Spin, { tip: "正在汇总市场数据…" }),
        ),
      );
    }

    const metrics = [
      {
        key: "skills",
        label: "技能总数",
        value: skillTotal,
        hint: "个可安装技能",
        icon: "skills",
        accent: palette.skills,
        soft: theme.dark
          ? "linear-gradient(135deg, rgba(99, 102, 241, 0.28), rgba(99, 102, 241, 0.10))"
          : "linear-gradient(135deg, #eef2ff, #e0e7ff)",
      },
      {
        key: "agents",
        label: "智能体总数",
        value: agentTotal,
        hint: "个可安装智能体",
        icon: "agents",
        accent: palette.agents,
        soft: theme.dark
          ? "linear-gradient(135deg, rgba(20, 184, 166, 0.28), rgba(20, 184, 166, 0.10))"
          : "linear-gradient(135deg, #ecfdf5, #ccfbf1)",
      },
      {
        key: "downloads",
        label: "累计下载",
        value: downloadTotal,
        hint: "次成功取包",
        icon: "downloads",
        accent: palette.downloads,
        soft: theme.dark
          ? "linear-gradient(135deg, rgba(14, 165, 233, 0.28), rgba(14, 165, 233, 0.10))"
          : "linear-gradient(135deg, #eff6ff, #dbeafe)",
      },
      {
        key: "new",
        label: "本月新增",
        value: monthTotal,
        hint: "个新内容",
        icon: "new",
        accent: palette.fresh,
        soft: theme.dark
          ? "linear-gradient(135deg, rgba(245, 158, 11, 0.30), rgba(245, 158, 11, 0.10))"
          : "linear-gradient(135deg, #fffbeb, #fef3c7)",
      },
    ];

    const metricCards = metrics.map((metric) =>
      h(
        "div",
        { key: metric.key, style: cardStyle },
        h("div", {
          style: {
            position: "absolute",
            left: 0,
            top: 0,
            bottom: 0,
            width: 4,
            background: metric.accent,
          },
        }),
        h(
          "div",
          { style: { display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12 } },
          h(
            "div",
            null,
            h(
              "div",
              { style: { fontSize: 13, fontWeight: 600, color: palette.muted, letterSpacing: 0.3 } },
              metric.label,
            ),
            h(
              "div",
              {
                style: {
                  marginTop: 8,
                  fontSize: 38,
                  lineHeight: 1.05,
                  fontWeight: 800,
                  color: palette.ink,
                  fontVariantNumeric: "tabular-nums",
                  letterSpacing: -0.5,
                },
              },
              formatNumber(metric.value),
            ),
            h("div", { style: { marginTop: 8, fontSize: 12, color: palette.faint } }, metric.hint),
          ),
          h(
            "div",
            {
              style: {
                width: 48,
                height: 48,
                flex: "0 0 auto",
                borderRadius: 15,
                display: "grid",
                placeItems: "center",
                fontSize: 23,
                background: metric.soft,
                border: `1px solid ${metric.accent}22`,
              },
            },
            h(MetricIcon, { name: metric.icon, color: metric.accent }),
          ),
        ),
      ),
    );

    const legendRow = (color, label, value, percent) =>
      h(
        "div",
        { style: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 } },
        h(
          "div",
          { style: { display: "flex", alignItems: "center", gap: 9 } },
          h("span", { style: { width: 9, height: 9, borderRadius: 3, background: color } }),
          h("span", { style: { fontSize: 13, color: palette.ink, fontWeight: 600 } }, label),
        ),
        h(
          "div",
          { style: { display: "flex", alignItems: "baseline", gap: 7 } },
          h("span", { style: { fontSize: 16, fontWeight: 700, color: palette.ink } }, formatNumber(value)),
          h("span", { style: { fontSize: 11, color: palette.faint } }, `${percent}%`),
        ),
      );

    const progressRow = (label, value, total, color) => {
      const percent = total > 0 ? Math.min(100, Math.round((value / total) * 100)) : 0;
      return h(
        "div",
        { style: { marginTop: 16 } },
        h(
          "div",
          { style: { display: "flex", alignItems: "baseline", justifyContent: "space-between", marginBottom: 7 } },
          h("span", { style: { fontSize: 13, fontWeight: 600, color: palette.ink } }, label),
          h(
            "span",
            { style: { fontSize: 12, color: palette.faint } },
            `${formatNumber(value)} / ${formatNumber(total)}`,
          ),
        ),
        h(
          "div",
          { style: { height: 8, borderRadius: 999, background: palette.surfaceAlt, overflow: "hidden" } },
          h("div", {
            style: {
              height: "100%",
              width: `${percent}%`,
              borderRadius: 999,
              background: `linear-gradient(90deg, ${color}, ${color}99)`,
              transition: "width 0.4s ease",
            },
          }),
        ),
      );
    };

    const composition = h(
      "div",
      { style: { ...cardStyle, display: "flex", flexDirection: "column", gap: 18 } },
      h(
        "div",
        null,
        h("div", { style: { fontSize: 16, fontWeight: 700, color: palette.ink } }, "内容构成"),
        h("div", { style: { marginTop: 4, fontSize: 12, color: palette.faint } }, "当前可浏览的技能与智能体占比"),
      ),
      h(
        "div",
        { style: { display: "flex", alignItems: "center", gap: 24, flexWrap: "wrap" } },
        h(
          "div",
          {
            style: {
              width: 148,
              height: 148,
              flex: "0 0 auto",
              borderRadius: "50%",
              background: itemTotal
                ? `conic-gradient(${palette.skills} 0 ${skillPct}%, ${palette.agents} ${skillPct}% 100%)`
                : palette.line,
              display: "grid",
              placeItems: "center",
              boxShadow: theme.dark
            ? "inset 0 0 0 1px rgba(255, 255, 255, 0.08)"
            : "inset 0 0 0 1px rgba(15, 23, 42, 0.04)",
            },
          },
          h(
            "div",
            {
              style: {
                width: 108,
                height: 108,
                borderRadius: "50%",
                background: palette.surface,
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                justifyContent: "center",
                boxShadow: "0 2px 10px rgba(15, 23, 42, 0.06)",
              },
            },
            h(
              "div",
              { style: { fontSize: 27, lineHeight: 1.1, fontWeight: 800, color: palette.ink } },
              formatNumber(itemTotal),
            ),
            h("div", { style: { marginTop: 2, fontSize: 11, color: palette.faint } }, "内容总数"),
          ),
        ),
        h(
          "div",
          { style: { flex: "1 1 160px", display: "flex", flexDirection: "column", gap: 14 } },
          legendRow(palette.skills, "技能", skillTotal, itemTotal ? skillPct : 0),
          legendRow(palette.agents, "智能体", agentTotal, itemTotal ? 100 - skillPct : 0),
        ),
      ),
      h(
        "div",
        {
          style: {
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: 12,
            paddingTop: 16,
            borderTop: `1px dashed ${palette.line}`,
          },
        },
        h(
          "div",
          null,
          h("div", { style: { fontSize: 11, color: palette.faint } }, "技能下载"),
          h("div", { style: { marginTop: 4, fontSize: 18, fontWeight: 700, color: palette.ink } }, formatNumber(skills.downloads)),
        ),
        h(
          "div",
          null,
          h("div", { style: { fontSize: 11, color: palette.faint } }, "智能体下载"),
          h("div", { style: { marginTop: 4, fontSize: 18, fontWeight: 700, color: palette.ink } }, formatNumber(agents.downloads)),
        ),
      ),
    );

    const monthly = h(
      "div",
      { style: { ...cardStyle, display: "flex", flexDirection: "column" } },
      h(
        "div",
        { style: { display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 } },
        h(
          "div",
          null,
          h("div", { style: { fontSize: 16, fontWeight: 700, color: palette.ink } }, "本月新增"),
          h("div", { style: { marginTop: 4, fontSize: 12, color: palette.faint } }, "按内容创建时间统计"),
        ),
        h(
          "span",
          {
            style: {
              padding: "3px 10px",
              borderRadius: 999,
              fontSize: 12,
              fontWeight: 700,
              color: theme.dark ? "#fbbf24" : "#b45309",
              background: theme.dark ? "rgba(245, 158, 11, 0.14)" : "#fffbeb",
              border: theme.dark
                ? "1px solid rgba(245, 158, 11, 0.45)"
                : "1px solid #fde68a",
            },
          },
          `+${formatNumber(monthTotal)}`,
        ),
      ),
      h(
        "div",
        { style: { marginTop: 18, display: "flex", alignItems: "baseline", gap: 8 } },
        h("span", { style: { fontSize: 42, lineHeight: 1, fontWeight: 800, color: palette.ink } }, formatNumber(monthTotal)),
        h("span", { style: { fontSize: 13, color: palette.muted } }, "件新内容"),
      ),
      progressRow("技能", monthSkill, skillTotal, palette.skills),
      progressRow("智能体", monthAgent, agentTotal, palette.agents),
      h(
        "div",
        {
          style: {
            marginTop: "auto",
            paddingTop: 18,
            fontSize: 12,
            lineHeight: 1.7,
            color: palette.faint,
          },
        },
        "下载量按成功获取安装包的次数统计；撤回或下架不会抹除历史下载。",
      ),
    );

    return h(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: 16 } },
      hero,
      error
        ? h(Alert, {
            type: "error",
            showIcon: true,
            message: "概览数据加载失败",
            description: error,
          })
        : null,
      h(
        "div",
        {
          style: {
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(min(100%, 210px), 1fr))",
            gap: 16,
          },
        },
        metricCards,
      ),
      h(
        "div",
        {
          style: {
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(min(100%, 300px), 1fr))",
            gap: 16,
            alignItems: "stretch",
          },
        },
        composition,
        monthly,
      ),
    );
  }

  function MarketPage() {
    const { config, limits, error, setError } = useMarketData();
    const [notice, setNotice] = useState(null);
    const [activeKey, setActiveKey] = useState("skills");

    const onNotice = useCallback((value) => {
      logger.debug(`通知 [${value.type}] ${value.text}`);
      setNotice(value);
      if (value && value.type === "success") {
        setTimeout(() => setNotice(null), 6000);
      }
    }, []);

    const tabs = [
      {
        key: "skills",
        label: "技能",
        children: h(SkillsPage, { config, limits, onNotice, active: activeKey === "skills" }),
      },
      {
        key: "agents",
        label: "智能体",
        children: h(AgentMarketPage, { config, onNotice, active: activeKey === "agents" }),
      },
      {
        key: "overview",
        label: "概览",
        children: h(OverviewTab),
      },
    ];

    return h(
      "div",
      { style: { padding: 16, display: "flex", flexDirection: "column", gap: 12 } },
      h(Title, { level: 4, style: { margin: 0 } }, "内网市场"),
      h(StatusBanner, { config, error }),
      notice
        ? h(Alert, { type: notice.type, showIcon: true, message: notice.text, closable: true })
        : null,
      Tabs
        ? h(Tabs, {
            items: tabs,
            defaultActiveKey: "skills",
            // A tab switch must not refetch: the pane mounts on first visit and
            // refreshing here used to run the whole N+1 fan-out twice.
            onChange: (key) => {
              setActiveKey(key);
              setError(null);
            },
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
      title: "内网市场",
      description: "离线技能与智能体市场",
      scope: "global",
      layout: "standard",
      navigation: "rail",
    },
  });

  // Sidebar: `sider.top` renders outside the collapsible nav, directly below
  // the New Task button. A menu item would always live inside that nav group.
  if (paw.slot && typeof paw.slot.fill === "function") {
    logger.debug("注册侧边栏顶部入口", { slot: "sider.top" });
    paw.slot.fill(PLUGIN_ID, "sider.top", () => h(SidebarMarketEntry), {
      id: "offline-market.sider",
      order: 10,
    });
  } else {
    // Older hosts without the slot API still get a discoverable nav entry.
    logger.debug("注册侧边栏菜单（slot 不可用的兜底）", { before: "core.inbox" });
    paw.menu.add(PLUGIN_ID, {
      id: "offline-market.menu",
      location: "primary.agentScoped",
      before: "core.inbox",
      order: 5,
      label: "内网市场",
      icon: h(MarketGlyph, { size: 18 }),
      route: "offline-market.page",
    });
  }

  logger.info(
    `frontend registered（路由 /offline-market，菜单 ${PLUGIN_LABEL}，debug=${debugOn ? "开" : "关"}）`,
  );
});
