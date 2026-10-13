// node --experimental-vm-modules --test scripts/test-codex-refresh.mjs
// 执行生产刷新编排与 usage_data，只替换 IPC、时钟与绘图，避免访问真实账户和同步服务。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const sources = Object.fromEntries(await Promise.all(
  ["accounts", "usage", "tray_panel", "usage_data"].map(async name =>
    [name, await readFile(new URL(`../src/${name}.js`, import.meta.url), "utf8")])
));
const tick = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function scan(tokens = 10) {
  return { aggregate: { totalTokens: tokens, totalEquivalentUsd: 1, models: [], daily: [], hourly: [] }, roots: [], sessions: 1 };
}

async function fixture(name, { sync, accounts = [], syncStatus = { state: "idle" } } = {}) {
  const calls = [], timers = new Map(), intervals = new Map(), listeners = new Map(), nodes = new Map();
  const storage = new Map([["trayTab", "cursor"]]);
  const target = new EventTarget();
  let nextTimer = 0, period = 0;
  class Node {
    value = ""; hidden = false; disabled = false; textContent = ""; dataset = {}; children = []; handlers = new Map();
    classList = { toggle() {}, add() {}, remove() {}, contains() { return false; } };
    style = {}; selectedOptions = [{ textContent: "全部设备" }];
    addEventListener(type, handler) { this.handlers.set(type, handler); }
    querySelectorAll() { return []; } querySelector() { return new Node(); }
    setAttribute() {} getAttribute() { return ""; } removeAttribute() {} focus() {}
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    click() { return this.handlers.get("click")?.({ target: this }); }
  }
  const el = id => { if (!nodes.has(id)) nodes.set(id, new Node()); return nodes.get(id); };
  const setTimer = map => (callback, ms) => { const id = ++nextTimer; map.set(id, { callback, ms }); return id; };
  const clock = { setTimeout: setTimer(timers), clearTimeout: id => timers.delete(id),
    setInterval: setTimer(intervals), clearInterval: id => intervals.delete(id) };
  const invoke = async (cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "codex_usage_sync_now") return sync ? sync(args) : { state: "idle" };
    if (cmd === "accounts_set_interval") { period = args.intervalMinutes; return { accounts, intervalMinutes: period }; }
    if (cmd === "accounts_list") return { accounts, intervalMinutes: period };
    if (cmd === "account_refresh") return accounts.find(a => a.id === args.id);
    if (cmd === "cursor_usage_deleted_list" || cmd === "codex_usage_devices") return [];
    if (cmd === "codex_usage_sync_get") return { sourceHome: "" };
    if (cmd === "codex_usage_sync_status") return syncStatus;
    if (cmd === "app_update_status") return { revision: 0 };
    if (["codex_usage_query", "codex_scan_sessions", "claude_scan_sessions"].includes(cmd)) return scan();
    if (cmd === "cursor_usage_fetch") return { agg: scan().aggregate, syncedAt: Date.now() };
    throw new Error(`Unexpected IPC: ${cmd}`);
  };
  const hooks = { statuses: [], renders: [], noop() {} };
  const shared = { el, invoke, emit: async () => {}, listen: async (type, fn) => {
    const handlers = listeners.get(type) || []; handlers.push(fn); listeners.set(type, handlers); return () => {};
  }, resetError: error => String(error).replace(/^Error: /, ""), toast: (kind, text) => hooks.statuses.push({ kind, text }),
  dismissToast() {}, fillStatus: (node, kind, text) => { node.textContent = text; },
  chartAnimMs: () => 0 };
  class DetailEvent extends Event { constructor(type, options) { super(type); this.detail = options?.detail; } }
  const context = vm.createContext({ console, ...clock, hooks, Element: Node, Option: Node, CustomEvent: DetailEvent,
    window: { ...clock, addEventListener: target.addEventListener.bind(target), dispatchEvent: target.dispatchEvent.bind(target) },
    document: { querySelector: el, querySelectorAll: () => [], createElement: () => new Node(), addEventListener() {} },
    localStorage: { get length() { return storage.size; }, key: i => [...storage.keys()][i],
      getItem: k => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v), removeItem: k => storage.delete(k) },
  });
  // 测试接缝只替换绘图/行渲染，生产调度、数据加载、事件与加载状态逻辑原样执行。
  const presentation = {
    accounts: `render = updateRow = hooks.noop; export { refreshAll, loadInitial };`,
    usage: `renderOverviewTable = hooks.noop;
      renderOverviewMerged = () => scanForTest();
      function scanForTest() { return { models: [] }; }
      renderLocalScan = (key, value) => hooks.renders.push({ key, value });
      renderCursorAccount = hooks.noop;
      rebuildChips = applyVisibility = syncRangeControls = hooks.noop;
      deletedDirty = false;
      export { refreshUsage, loadView };
      export function choose(key) { selection = key; selectedKeys.clear(); if (key !== "all") selectedKeys.add(key); }
      export function state() { return { loading, manual: !!manualRefresh, local: overviewResults.get("local") }; }`,
    tray_panel: `render = applyTab = applyDayHead = syncHeaderRefresh = syncOverviewLoading = overviewTip = hooks.noop;
      renderOverview = (data) => hooks.renders.push(data);
      export { refreshAll, loadOverview, shiftDay, setTab };
      export function choose(tab) { trayTab = tab; }
      export function state() { return { refreshing, overviewLoading }; }`,
  }[name];
  let text = sources[name];
  if (name === "tray_panel") text = text.replace("/* ---------- 初始化 ---------- */", presentation + "\n/* ---------- 初始化 ---------- */");
  else text += "\n" + presentation;
  const imports = new Map();
  for (const code of [text, sources.usage_data]) {
    for (const [, names, path] of code.matchAll(/import\s*\{([^}]+)\}\s*from\s*"([^"]+)"/g)) {
      const exports = imports.get(path) || new Set();
      for (const field of names.split(",").map(s => s.trim()).filter(Boolean)) exports.add(field.split(/\s+as\s+/)[0]);
      imports.set(path, exports);
    }
  }
  const modules = new Map();
  for (const [path, names] of imports) {
    if (path === "./usage_data.js") continue;
    modules.set(path, new vm.SyntheticModule([...names], function () {
      for (const key of names) this.setExport(key,
        path === "./shared.js" ? shared[key] || hooks.noop
        : key === "syncError" ? shared.resetError
        : key === "getAccounts" ? () => accounts
        : key === "getRefreshIntervalMinutes" ? () => period
        : key === "updatePresentation" ? () => ({}) : hooks.noop);
    }, { context }));
  }
  const data = new vm.SourceTextModule(sources.usage_data, { context });
  await data.link(path => modules.get(path));
  await data.evaluate();
  modules.set("./usage_data.js", data);
  const entry = new vm.SourceTextModule(text, { context });
  await entry.link(path => modules.get(path));
  await entry.evaluate();
  if (name === "usage") entry.namespace.initUsage();
  await tick();
  calls.length = 0;
  return { api: entry.namespace, calls, timers, intervals, hooks, el,
    async event(reason) { for (const fn of listeners.get("codex-usage-changed") || []) fn({ payload: { reason } }); await tick(); },
  };
}

test("global timer runs once without accounts, changes period and stops including delayed startup", async () => {
  const f = await fixture("accounts");
  await f.api.setRefreshInterval(5);
  assert.equal([...f.intervals.values()][0].ms, 300_000);
  await f.api.refreshAll();
  assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 1);
  assert.equal(f.calls.find(c => c.cmd === "codex_usage_sync_now").args.trigger, "interval");
  await f.api.setRefreshInterval(15);
  assert.equal(f.intervals.size, 1);
  assert.equal([...f.intervals.values()][0].ms, 900_000);
  await f.api.loadInitial();
  const startup = [...f.timers.values()].find(t => t.ms === 3000);
  assert.ok(startup);
  await f.api.setRefreshInterval(0);
  const before = f.calls.length;
  startup.callback();
  await f.api.refreshAll();
  assert.equal(f.intervals.size, 0);
  assert.equal(f.calls.length, before);
});

test("one timer cycle syncs once while all managed accounts refresh", async () => {
  const accounts = ["cursor", "codex", "claude"].map(kind => ({ id: kind, kind }));
  const f = await fixture("accounts", { accounts });
  await f.api.setRefreshInterval(5);
  await f.api.refreshAll();
  assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 1);
  assert.equal(f.calls.filter(c => c.cmd === "account_refresh").length, 3);
});

test("statistics refresh always syncs, including Claude-only selection, and awaits failure", async () => {
  const remote = deferred();
  const f = await fixture("usage", { sync: () => remote.promise });
  f.api.choose("local-claude");
  const refresh = f.el("#usage-refresh").click();
  await tick();
  assert.equal(f.api.state().loading, true);
  assert.ok(f.calls.some(c => c.cmd === "claude_scan_sessions"));
  assert.ok(f.calls.some(c => c.cmd === "codex_usage_sync_now"));
  await f.event("sync");
  assert.equal(f.api.state().loading, true);
  remote.reject(new Error("sync_network"));
  await refresh;
  assert.equal(f.api.state().loading, false);
  assert.ok(f.hooks.statuses.some(s => s.text.includes("sync_network")));
  assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 1);
});

test("merged statistics reads Codex after sync while Claude proceeds; own event does not restart", async () => {
  const remote = deferred();
  const f = await fixture("usage", { sync: () => remote.promise });
  const refresh = f.api.refreshUsage();
  await tick();
  assert.ok(f.calls.some(c => c.cmd === "claude_scan_sessions"));
  assert.ok(!f.calls.some(c => c.cmd === "codex_usage_query"));
  await f.event("sync");
  remote.resolve({ state: "idle" });
  await refresh;
  assert.equal(f.api.state().local.scan.aggregate.totalTokens, 10);
  assert.equal(f.api.state().loading, false);
  assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 1);
});

test("all Tray account tabs and overview manually sync without accounts and finish loading", async () => {
  for (const tab of ["cursor", "codex", "claude", "overview"]) {
    const f = await fixture("tray_panel");
    f.api.choose(tab);
    await f.el("#tray-refresh").click();
    await tick();
    assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 1);
    assert.equal(f.calls.find(c => c.cmd === "codex_usage_sync_now").args.trigger, "manual");
    assert.equal(f.api.state().refreshing, false);
    assert.equal(f.api.state().overviewLoading, false);
  }
});

test("switching to Tray overview and moving the date use viewing triggers without date filters", async () => {
  const f = await fixture("tray_panel");
  f.api.setTab("overview");
  await f.api.loadOverview();
  f.api.shiftDay(-1);
  await f.api.loadOverview();
  const syncs = f.calls.filter(c => c.cmd === "codex_usage_sync_now");
  assert.equal(syncs.length, 2);
  assert.ok(syncs.every(c => JSON.stringify(c.args) === '{"trigger":"viewing"}'));
  assert.ok(f.calls.filter(c => c.cmd === "codex_usage_query").some(c => c.args.untilMs != null));
});

test("Tray viewing asks backend freshness once; completion event only reloads the archive", async () => {
  const f = await fixture("tray_panel");
  f.api.choose("overview");
  await f.api.loadOverview({ viewing: true });
  assert.equal(f.calls.find(c => c.cmd === "codex_usage_sync_now").args.trigger, "viewing");
  await f.event("sync");
  await f.api.loadOverview();
  assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 1);
  assert.equal(f.api.state().overviewLoading, false);
});

test("Tray sync failure preserves local totals and error across cached redraws", async () => {
  const f = await fixture("tray_panel", { sync: async () => { throw new Error("sync_network"); } });
  f.api.choose("overview");
  await f.api.loadOverview({ viewing: true });
  await f.api.loadOverview();
  const local = f.hooks.renders.at(-1).rows.find(row => row.id === "local");
  assert.equal(local.tokens, 10);
  assert.match(local.error, /sync_network/);
  assert.equal(f.api.state().overviewLoading, false);
});

test("Tray shows another window's sync error without starting another sync", async () => {
  const f = await fixture("tray_panel", { syncStatus: { state: "error", error: "sync_network" } });
  f.api.choose("overview");
  await f.event("sync");
  await f.api.loadOverview();
  assert.match(f.hooks.renders.at(-1).rows.find(row => row.id === "local").error, /sync_network/);
  assert.equal(f.calls.filter(c => c.cmd === "codex_usage_sync_now").length, 0);
});
