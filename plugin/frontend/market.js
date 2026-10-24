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
const VISIBILITY_LABEL = { public: "公开", tenant: "仅本租户" };
const STATE_LABEL = { active: "已上架", withdrawn: "已撤回", taken_down: "已下架" };
const STATE_COLOR = { active: "green", withdrawn: "default", taken_down: "red" };

function whenHost(callback, deadlineMs = 15000) {
  const started = Date.now();
  const tick = () => {
    const paw = window.QwenPaw;
    if (paw && paw.host && paw.host.React && paw.menu && paw.route) {
      callback(paw);
      return;
    }
    if (Date.now() - started > deadlineMs) {
      console.error("[offline-market] window.QwenPaw never became available");
      return;
    }
    setTimeout(tick, 50);
  };
  tick();
}

function apiUrl(path) {
  const host = window.QwenPaw && window.QwenPaw.host;
  // NOTE: getApiUrl() already prepends "/api" (console/src/api/config.ts).
  if (host && typeof host.getApiUrl === "function") return host.getApiUrl(path);
  return `/api${path.startsWith("/") ? path : `/${path}`}`;
}

async function request(path, init) {
  const host = window.QwenPaw && window.QwenPaw.host;
  const token = host && typeof host.getApiToken === "function" ? host.getApiToken() : "";
  const headers = { accept: "application/json", ...((init && init.headers) || {}) };
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(apiUrl(path), {
    credentials: "same-origin",
    ...init,
    headers,
  });
  const text = await response.text();
  let body = {};
  try {
    body = text ? JSON.parse(text) : {};
  } catch (err) {
    body = { raw: text };
  }
  if (!response.ok) {
    const detail = (body && body.detail) || {};
    const error = new Error(detail.message || `HTTP ${response.status}`);
    error.code = detail.code || `http_${response.status}`;
    error.status = response.status;
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

function readFileAsText(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ""));
    reader.onerror = () => reject(new Error(`无法读取 ${file.name}`));
    reader.readAsText(file);
  });
}

/** Cheap client-side mirror of the backend gate; the backend still decides. */
function precheck(files, limits) {
  const paths = Object.keys(files);
  if (!paths.length) return "请至少添加一个文件";
  if (!paths.includes("SKILL.md")) return "根目录必须有 SKILL.md";
  if (limits) {
    if (paths.length > limits.max_files) return `文件数超过上限 ${limits.max_files}`;
    let total = 0;
    for (const path of paths) {
      const size = new Blob([files[path]]).size;
      if (size > limits.max_file_bytes) return `${path} 超过单文件上限 ${limits.max_file_bytes} 字节`;
      if (path.split("/").length > limits.max_path_depth) return `${path} 层级超过 ${limits.max_path_depth}`;
      if (path.startsWith("/") || path.split("/").some((seg) => !seg || seg === "." || seg === ".."))
        return `${path} 路径不合法`;
      total += size;
    }
    if (total > limits.max_bundle_bytes) return `总大小超过上限 ${limits.max_bundle_bytes} 字节`;
  }
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

  // ---------------------------------------------------------------- shared ---

  function useMarketData() {
    const [config, setConfig] = useState(null);
    const [limits, setLimits] = useState(null);
    const [error, setError] = useState(null);

    const reloadConfig = useCallback(async () => {
      try {
        setConfig(await getJson("/offline-market/config"));
      } catch (err) {
        setError(err.message);
      }
      try {
        setLimits(await getJson("/offline-market/limits"));
      } catch (err) {
        // limits are advisory only; the backend enforces them
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
      try {
        await postJson(`/skills/hub/install/cancel/${task.taskId}`);
        setTask(null);
        setBusySlug("");
        onDone && onDone({ type: "warning", text: "已取消安装" });
      } catch (err) {
        onDone && onDone({ type: "error", text: err.message });
      }
    }, [task, onDone]);

    const install = useCallback(
      async (item) => {
        setBusySlug(item.slug);
        setTask(null);
        try {
          const started = await postJson("/skills/hub/install/start", {
            provider_key: PLUGIN_ID,
            bundle_url: item.install_url,
            version: item.latest_version || "",
          });
          const taskId = started.task_id;
          setTask({ taskId, slug: item.slug, status: started.status });
          for (let attempt = 0; attempt < 240; attempt += 1) {
            await new Promise((resolve) => setTimeout(resolve, 500));
            const status = await getJson(`/skills/hub/install/status/${taskId}`);
            setTask({ taskId, slug: item.slug, status: status.status });
            if (status.status === "completed") {
              setBusySlug("");
              setTask(null);
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
              onDone && onDone({ type: "error", text: status.error || status.status });
              return;
            }
          }
          setBusySlug("");
          setTask(null);
          onDone && onDone({ type: "warning", text: "安装超时，请到技能页查看" });
        } catch (err) {
          setBusySlug("");
          setTask(null);
          onDone && onDone({ type: "error", text: err.message });
        }
      },
      [onDone],
    );

    return { install, cancel, busySlug, task };
  }

  // ------------------------------------------------------------------ tabs ---

  function MarketTab({ limits, onNotice }) {
    const [items, setItems] = useState([]);
    const [query, setQuery] = useState("");
    const [loading, setLoading] = useState(false);
    const installer = useInstaller(onNotice);

    const load = useCallback(
      async (value) => {
        setLoading(true);
        try {
          const body = await getJson(
            `/offline-market/skills?q=${encodeURIComponent(value || "")}&limit=50`,
          );
          setItems(body.items || []);
        } catch (err) {
          onNotice({ type: "error", text: err.message });
          setItems([]);
        } finally {
          setLoading(false);
        }
      },
      [onNotice],
    );

    useEffect(() => {
      load("");
    }, [load]);

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
        { title: "发布者", dataIndex: "owner_tenant", width: 140 },
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
        setItems(body.items || []);
      } catch (err) {
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
        try {
          await postJson(`/offline-market/skills/${encodeURIComponent(row.slug)}/${action}`);
          onNotice({
            type: "success",
            text: action === "withdraw" ? `已撤回 ${row.slug}` : `已恢复 ${row.slug}`,
          });
          load();
        } catch (err) {
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
    const [files, setFiles] = useState({});
    const [fileError, setFileError] = useState(null);
    const [busy, setBusy] = useState(false);

    const onPick = useCallback(async (event) => {
      const picked = Array.from(event.target.files || []);
      if (!picked.length) return;
      const next = {};
      const failures = [];
      for (const file of picked) {
        // webkitRelativePath keeps sub-directories when a folder is selected
        const path = file.webkitRelativePath || file.name;
        try {
          next[path] = await readFileAsText(file);
        } catch (err) {
          failures.push(err.message);
        }
      }
      setFiles(next);
      setFileError(failures.length ? failures.join("；") : null);
      if (!name && picked.length === 1) {
        const base = (picked[0].name || "").replace(/\.md$/i, "");
        if (base && base !== "SKILL") setName(base);
      }
    }, [name]);

    const submit = useCallback(async () => {
      const problem = precheck(files, limits);
      if (problem) {
        setFileError(problem);
        return;
      }
      setBusy(true);
      setFileError(null);
      try {
        const body = await postJson("/offline-market/skills", {
          name: name.trim() || "untitled",
          slug: slug.trim() || null,
          version: version.trim(),
          visibility,
          files,
        });
        onNotice({ type: "success", text: `已发布 ${body.slug}@${body.version}` });
        setFiles({});
        onPublished && onPublished();
      } catch (err) {
        setFileError(`${err.code ? `[${err.code}] ` : ""}${err.message}`);
      } finally {
        setBusy(false);
      }
    }, [files, limits, name, slug, version, visibility, onNotice, onPublished]);

    const fileKeys = Object.keys(files);

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
            multiple: true,
            onChange: onPick,
            style: { color: "inherit" },
          }),
          h(
            Paragraph,
            { type: "secondary", style: { margin: "6px 0 0" } },
            "选择技能目录里的文件（至少要包含根目录的 SKILL.md）；" +
              (limits
                ? `后端限额：${limits.max_files} 个文件 / 单文件 ${limits.max_file_bytes} 字节 / 合计 ${limits.max_bundle_bytes} 字节。`
                : "限额未取到，后端仍会校验。"),
          ),
        ),
        fileKeys.length
          ? h(
              "div",
              null,
              h(Text, { strong: true }, `${fileKeys.length} 个文件：`),
              h(
                Space,
                { wrap: true, size: 4 },
                fileKeys.map((path) => h(Tag, { key: path }, path)),
              ),
            )
          : null,
        fileError ? h(Alert, { type: "error", showIcon: true, message: fileError }) : null,
        h(
          Space,
          null,
          h(Button, { type: "primary", loading: busy, onClick: submit }, "立即发布"),
          h(Button, { onClick: () => setFiles({}) }, "清空文件"),
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
   */
  function InstalledTab({ onNotice, refreshToken }) {
    const [rows, setRows] = useState([]);
    const [loading, setLoading] = useState(false);

    const load = useCallback(async () => {
      setLoading(true);
      try {
        const listed = await getJson("/skills");
        const skills = Array.isArray(listed) ? listed : listed.items || [];

        const details = await Promise.all(
          skills.map(async (item) => {
            try {
              return await getJson(`/skills/${encodeURIComponent(item.name)}`);
            } catch (err) {
              return null;
            }
          }),
        );
        const fromMarket = details.filter(
          (detail) => detail && detail.installed_from === PLUGIN_ID,
        );
        if (!fromMarket.length) {
          setRows([]);
          return;
        }

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
      } catch (err) {
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
          return h(Tag, null, "—");
        },
      },
    ];

    return loading
      ? h(Spin)
      : Table
        ? h(Table, {
            rowKey: (row) => `${row.name}:${row.last_updated || ""}`,
            size: "small",
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

    const onNotice = useCallback((value) => {
      setNotice(value);
      if (value && value.type === "success") {
        setTimeout(() => setNotice(null), 6000);
      }
    }, []);

    const onPublished = useCallback(() => setRefreshToken((value) => value + 1), []);

    const tabs = [
      {
        key: "market",
        label: "市场",
        children: h(MarketTab, { limits, onNotice }),
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
            onChange: () => {
              setError(null);
              setRefreshToken((value) => value + 1);
            },
          })
        : h("div", null, tabs.map((tab) => h("section", { key: tab.key }, tab.children))),
    );
  }

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
  paw.menu.add(PLUGIN_ID, {
    id: "offline-market.menu",
    location: "primary.agentScoped",
    after: "core.marketplace",
    order: 16,
    label: "技能市场（内网）",
    icon: "🧩",
    route: "offline-market.page",
  });

  console.info("[offline-market] frontend registered");
});
