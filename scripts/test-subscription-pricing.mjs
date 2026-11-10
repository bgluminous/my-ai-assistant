// node --experimental-vm-modules --test scripts/test-subscription-pricing.mjs
// 执行真实套餐解析、统计渲染及账户变更事件；仅替换 IPC、图表和与月费无关的交互。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const sources = Object.fromEntries(await Promise.all(
  ["usage", "usage_data", "account_format", "shared"].map(async name =>
    [name, await readFile(new URL(`../src/${name}.js`, import.meta.url), "utf8")])
));
const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
const account = (id, kind, plan, extra = {}) => ({ id, kind, status: { plan, ...extra } });
const tick = () => new Promise(resolve => setImmediate(resolve));

function scan(usd = 900) {
  return { roots: [], sessions: 1, filesScanned: 1, aggregate: {
    totalTokens: 100, totalEquivalentUsd: usd, totalActualUsd: 0, unpricedModels: 0, unpricedTokens: 0,
    models: [{ model: "fixture", priced: true, events: 1, totalTokens: 100, equivalentUsd: usd, actualUsd: 0,
      inputTokens: 100, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0,
      inputUsd: usd, outputUsd: 0, cacheReadUsd: 0, cacheWriteUsd: 0 }], daily: [], hourly: [],
  } };
}

async function fixture(initialAccounts = [], { charts = false } = {}) {
  let accounts = initialAccounts, changed;
  const calls = [], storage = new Map(), events = new Map();
  const { document, window: dom } = parseHTML(html);
  const el = selector => {
    const node = document.querySelector(selector);
    assert.ok(node, `Unknown element ${selector}`);
    return node;
  };
  const device = el("#usage-codex-device");
  Object.defineProperty(device, "selectedOptions", { get: () => [device.querySelector("option")] });
  const hooks = { noop() {}, cache: new Map(), scans: new Map(), cursorUsage: new Map(), deletedRecords: [], theme: "#123456" };
  const chartInstances = new Map(), frames = new Map();
  let frameId = 0;
  class Chart {
    constructor(canvas, config) {
      this.canvas = canvas; this.data = config.data; this.options = config.options;
      this.hidden = new Set(); this.updates = 0; this.resizes = 0; this.destroyed = false;
      canvas.parentElement.getBoundingClientRect = () => ({ width: 320, height: 230 });
      chartInstances.set(canvas.id, this);
    }
    getDataVisibility(index) { return !this.hidden.has(index); }
    toggleDataVisibility(index) { if (this.hidden.has(index)) this.hidden.delete(index); else this.hidden.add(index); }
    update() { this.updates++; }
    resize() { this.resizes++; this.width = 320; this.height = 230; this.currentDevicePixelRatio = 1; }
    destroy() { this.destroyed = true; chartInstances.delete(this.canvas.id); }
    static getChart(id) { return chartInstances.get(id); }
  }
  const invoke = async (cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "codex_usage_devices") return [];
    if (cmd === "cursor_usage_deleted_list") return hooks.deletedRecords;
    if (cmd === "codex_usage_sync_get") return { sourceHome: "" };
    if (cmd === "codex_usage_sync_now") return { state: "idle" };
    if (cmd === "claude_scan_sessions") return hooks.scans.get("local-claude") || scan();
    if (["codex_usage_query", "codex_scan_sessions"].includes(cmd)) return hooks.scans.get("local") || scan();
    if (["cursor_usage_fetch", "cursor_usage_slice", "opencode_usage_fetch"].includes(cmd)) {
      return { agg: hooks.cursorUsage.get(args.accountId) || scan().aggregate, syncedAt: Date.now() };
    }
    throw new Error(`Unexpected IPC ${cmd}`);
  };
  const shared = {
    el, invoke, emit: async () => {}, listen: async () => () => {},
    fmtInt: String, fmtTokens: String, fmtUsd: n => `$${Number(n).toFixed(2)}`,
    escapeHtml: String, kindLabel: String, iconAction: () => document.createElement("button"),
    resetError: String, chartAnimMs: () => 0,
  };
  const context = vm.createContext({ console, document, hooks, Chart, Element: dom.Element, CustomEvent: dom.CustomEvent,
    getComputedStyle: () => ({ getPropertyValue: () => hooks.theme }),
    requestAnimationFrame: callback => { const id = ++frameId; frames.set(id, callback); return id; },
    cancelAnimationFrame: id => frames.delete(id),
    Option: function (text, value) { const node = document.createElement("option"); node.textContent = text; node.value = value; return node; },
    setTimeout: () => 1, clearTimeout() {}, setInterval: () => 1, clearInterval() {},
    window: {
      addEventListener: (type, fn) => { const handlers = events.get(type) || []; handlers.push(fn); events.set(type, handlers); },
      dispatchEvent: event => { for (const fn of events.get(event.type) || []) fn(event); },
    },
    localStorage: { get length() { return storage.size; }, key: i => [...storage.keys()][i],
      getItem: k => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v), removeItem: k => storage.delete(k) },
  });
  const sharedModule = new vm.SourceTextModule(sources.shared, { context });
  await sharedModule.link(() => { throw new Error("Unexpected shared import"); });
  await sharedModule.evaluate();
  for (const name of ["topSlices", "colorFor", "compactTokens", "fmtShare"]) shared[name] = sharedModule.namespace[name];
  const seams = `
    ${charts ? "" : "renderCharts ="} renderBreakdown = applyVisibility = syncRangeControls = hooks.noop;
    getCachedLocal = (key) => hooks.cache.get(key) || null;
    export { renderOverviewTable, renderOverviewMerged, renderLocalScan, renderCursorAccount, refreshUsage, loadView,
      destroyUsageCharts, scheduleUsageChartResize };
    export function customRange(start, end) { period = "custom"; customStart = parseYmd(start); customEnd = parseYmd(end); }
    export function choose(keys = [], mode = "month", visible = true) {
      selectedKeys.clear(); for (const key of keys) selectedKeys.add(key);
      syncSelection(); period = mode; panelVisible = visible; lastAttemptAt = Date.now();
    }
    export function seed(entries, records = []) {
      overviewResults.clear(); for (const [key, value] of entries) overviewResults.set(key, value);
      deletedRecords = records;
      deletedDirty = false;
    }
    export function hide() { panelVisible = false; }
  `;
  const imports = new Map();
  for (const code of [sources.usage, sources.usage_data]) {
    for (const [, names, path] of code.matchAll(/import\s*\{([^}]+)\}\s*from\s*"([^"]+)"/g)) {
      const namesSet = imports.get(path) || new Set();
      for (const name of names.split(",").map(s => s.trim()).filter(Boolean)) namesSet.add(name.split(/\s+as\s+/)[0]);
      imports.set(path, namesSet);
    }
  }
  const modules = new Map();
  for (const [path, names] of imports) {
    if (["./usage_data.js", "./account_format.js"].includes(path)) continue;
    modules.set(path, new vm.SyntheticModule([...names], function () {
      for (const name of names) this.setExport(name,
        path === "./shared.js" ? shared[name] || hooks.noop
        : name === "getAccounts" ? () => accounts
        : name === "onAccountsChanged" ? fn => { changed = fn; }
        : name === "getRefreshIntervalMinutes" ? () => 0
        : name === "refreshAccounts" ? async ids => { calls.push({ cmd: "refreshAccounts", ids: [...ids] }); }
        : name === "syncError" ? String : hooks.noop);
    }, { context }));
  }
  for (const name of ["usage_data", "account_format"]) {
    const module = new vm.SourceTextModule(sources[name], { context });
    await module.link(path => modules.get(path));
    await module.evaluate();
    modules.set(`./${name}.js`, module);
  }
  const entry = new vm.SourceTextModule(sources.usage + seams, { context });
  await entry.link(path => modules.get(path));
  await entry.evaluate();
  const api = entry.namespace;
  api.initUsage();
  changed(accounts);
  await tick();
  calls.length = 0;
  const panel = async id => {
    for (const fn of events.get("panelshown") || []) fn({ detail: { id } });
    await tick();
  };
  return { api, el, calls, hooks, charts: chartInstances, format: modules.get("./account_format.js").namespace,
    dispatch(type) { context.window.dispatchEvent({ type }); },
    flushFrames() {
      for (let count = 0; frames.size && count < 10; count++) {
        const callbacks = [...frames.values()]; frames.clear(); callbacks.forEach(callback => callback());
      }
    },
    change(list) { accounts = list; changed(list); },
    show: () => panel("usage-panel"), hide: () => panel("accounts-panel"),
    rows() { return [...el("#usage-overview-body").children].map(row => [...row.children].map(cell => cell.textContent)); },
  };
}

test("GPT exact plan tiers, explicit Business seats and unknown plans", async () => {
  const { format } = await fixture();
  for (const [plan, expected] of [
    ["prolite", 100], [" Pro ", 200], ["promax", 500], ["plus", 20], ["go", 8], ["free", 0],
    ["team", 25], ["business_standard", 25], ["self_serve_business_prolite", 125],
    ["business", null], ["enterprise", null], ["new_tier", null], [undefined, null],
  ]) assert.equal(format.accountPlanInfo(account("a", "codex", plan)).monthlyUsd, expected, plan);
  assert.equal(format.accountPlanInfo(account("a", "codex", "prolite")).label, "Pro（$100）");
  assert.equal(format.accountPlanInfo(account("a", "codex", "pro")).label, "Pro（$200）");
  assert.equal(format.accountPlanInfo(account("a", "codex", "promax")).label, "Pro（$500）");
});

test("Claude Max legacy tiers and Team seats never share personal pricing", async () => {
  const { format } = await fixture();
  for (const [plan, extra, expected] of [
    ["pro", {}, 20], ["free", {}, 0], ["ultra", {}, 200], ["max", {}, null],
    ["claude_max", { rateLimitTier: "default_claude_max_5x" }, 100],
    ["max", { rateLimitTier: "default_claude_max_20x" }, 200],
    ["default_claude_max_20x", {}, 200], ["default_claude_max_5x", {}, 100],
    ["team", { rateLimitTier: "default_claude_max_5x" }, null],
    ["team", { rateLimitTier: "default_claude_max_5x", seatTier: "team_premium" }, 125],
    ["claude_team", { seatTier: "team_standard" }, 25], ["enterprise", {}, null],
    ["max", { rateLimitTier: "future_tier" }, null],
  ]) assert.equal(format.accountPlanInfo(account("a", "claude", plan, extra)).monthlyUsd, expected, plan);
  assert.equal(format.accountPlanInfo(account("a", "claude", "max", { rateLimitTier: "default_claude_max_20x" })).label, "Max 20x");
});

test("screenshot accounts total $300; local and merged $900 usage both render 3x", async () => {
  const accounts = [account("p", "codex", "pro"), account("l", "codex", "prolite"), account("u", "codex", "unknown")];
  const f = await fixture(accounts);
  assert.equal(f.format.accountsPlanMonthlyUsd([...accounts, accounts[0]], "codex"), 300);
  f.api.choose(["local"]);
  f.api.renderLocalScan("local", scan(), Date.now());
  assert.equal(f.el("#sum-plan").textContent, "$300 · 3.0×");
  f.api.choose();
  f.api.seed([["local", { state: "ok", scan: scan(), at: Date.now() }]]);
  f.api.renderOverviewTable(); f.api.renderOverviewMerged();
  assert.equal(f.el("#sum-plan").textContent, "$300 · 3.0×");
  assert.equal(f.rows().find(row => row[0].includes("ChatGPT"))[3], "$300/月");
  assert.equal(f.rows().at(-1)[6], "3.0×");
});

test("merged numerator includes unknown-provider and unknown-Cursor usage; selection controls denominator", async () => {
  const f = await fixture([account("p", "codex", "prolite"), account("c", "claude", "unknown"),
    account("known", "cursor", null, { membershipType: "pro" }), account("unknown", "cursor", null)]);
  const entries = [["local", { scan: scan(100) }], ["local-claude", { scan: scan(300) }],
    ["known", { agg: scan(20).aggregate }], ["unknown", { agg: scan(180).aggregate }]];
  f.api.choose(); f.api.seed(entries); f.api.renderOverviewTable(); f.api.renderOverviewMerged();
  assert.equal(f.el("#sum-plan").textContent, "$120 · 5.0×");
  assert.equal(f.rows().at(-1)[6], "5.0×");
  f.api.choose(["local", "local-claude"]); f.api.renderOverviewTable(); f.api.renderOverviewMerged();
  assert.equal(f.el("#sum-plan").textContent, "$100 · 4.0×");
  assert.equal(f.rows().at(-1)[3], "$100/月");
});

test("Claude single-source view sums known Max tiers including last-known failed accounts", async () => {
  const f = await fixture([
    account("a", "claude", "max", { rateLimitTier: "default_claude_max_5x" }),
    account("b", "claude", "max", { rateLimitTier: "default_claude_max_20x", alive: false }),
    account("c", "claude", "unknown"), account("p", "codex", "promax"),
  ]);
  f.api.choose(["local-claude"]); f.api.renderLocalScan("local-claude", scan(), Date.now());
  assert.equal(f.el("#sum-plan").textContent, "$300 · 3.0×");
  assert.equal(f.el("#usage-results").querySelector("#card-actual"), null);
});

test("unknown/free denominators show no ratio while daily mode keeps the monthly-fee card", async () => {
  const f = await fixture();
  f.api.choose(["local"]); f.api.renderLocalScan("local", scan(), Date.now());
  assert.equal(f.el("#sum-plan").textContent, "—");
  f.change([account("f", "codex", "free")]);
  assert.equal(f.el("#sum-plan").textContent, "$0 · —");
  f.api.choose(["local"], "day"); f.api.renderLocalScan("local", scan(), Date.now());
  assert.equal(f.el("#card-plan").hidden, false);
  assert.equal(f.el("#sum-plan").textContent, "$0 · —");
});

test("failed deleted-account reads use the same known denominator in table and card", async () => {
  const f = await fixture([account("cursor", "cursor", null, { membershipType: "pro" })]);
  f.api.choose(["cursor", "deleted"]);
  f.api.seed([["cursor", { agg: scan(100).aggregate }], ["archived", { state: "error", error: "fixture" }]],
    [{ accountId: "archived", membershipType: "pro" }]);
  f.api.renderOverviewTable(); f.api.renderOverviewMerged();
  assert.equal(f.el("#sum-plan").textContent, "$40 · 2.5×");
  assert.equal(f.rows().at(-1)[3], "$40/月");
  assert.equal(f.rows().at(-1)[6], "2.5×");
  assert.ok(f.rows().every(row => row.length === 7));
  assert.deepEqual(f.rows().find(row => row[0].includes("已删除")).slice(4), ["—", "—", "—"]);
});

test("plan changes, account addition/removal and hidden views update without rescanning", async () => {
  const f = await fixture([account("p", "codex", "prolite")]);
  const at = Date.now();
  f.hooks.cache.set("local", { scan: scan(), at });
  f.api.choose(["local"]); f.api.renderLocalScan("local", scan(), at);
  f.change([account("p", "codex", "pro")]);
  assert.equal(f.el("#sum-plan").textContent, "$200 · 4.5×");
  f.change([account("p", "codex", "pro"), account("l", "codex", "prolite")]);
  assert.equal(f.el("#sum-plan").textContent, "$300 · 3.0×");
  f.api.hide(); f.change([account("l", "codex", "prolite")]);
  await f.show();
  assert.equal(f.el("#sum-plan").textContent, "$100 · 9.0×");
  assert.equal(f.calls.filter(c => /usage_query|scan_sessions|usage_fetch/.test(c.cmd)).length, 0);
  // 跨窗口旧缓存不能因为套餐变化覆盖正在展示的较新用量。
  f.hooks.cache.set("local", { scan: scan(1), at: at - 10_000 });
  f.change([account("l", "codex", "promax")]);
  assert.equal(f.el("#sum-plan").textContent, "$500 · 1.8×");
  // 即使没有用量缓存，也可以从正在展示的聚合重新计算月费。
  f.hooks.cache.clear(); f.change([]);
  assert.equal(f.el("#sum-plan").textContent, "—");
});

test("merged cards update when only account metadata changes", async () => {
  const f = await fixture([account("p", "codex", "prolite"), account("c", "claude", "pro")]);
  f.api.choose(); f.api.seed([["local", { scan: scan(900), at: Date.now() }], ["local-claude", { scan: scan(60), at: Date.now() }]]);
  f.api.renderOverviewTable(); f.api.renderOverviewMerged();
  assert.equal(f.el("#sum-plan").textContent, "$120 · 8.0×");
  f.change([account("p", "codex", "pro"), account("c", "claude", "max", { rateLimitTier: "default_claude_max_20x" })]);
  assert.equal(f.el("#sum-plan").textContent, "$400 · 2.4×");
  assert.equal(f.rows().at(-1)[6], "2.4×");
});

for (const hidden of [false, true]) {
  test(`removing Cursor with retained usage keeps totals ${hidden ? "after reopening the hidden page" : "on the visible page"}`, async () => {
    const gpt = account("gpt", "codex", "pro");
    const cursor = account("cursor", "cursor", null, { membershipType: "pro" });
    const f = await fixture([gpt, cursor]);
    f.hooks.scans.set("local", scan(600));
    f.hooks.scans.set("local-claude", scan(0));
    f.hooks.cursorUsage.set("cursor", scan(300).aggregate);
    f.api.choose();
    // 真实加载建立新鲜用量缓存；删除只需重新读取保留记录和本地切片。
    await f.api.loadView(true);
    assert.equal(f.el("#sum-plan").textContent, "$220 · 4.1×");
    if (hidden) await f.hide();
    f.calls.length = 0;
    f.hooks.deletedRecords = [{ accountId: "cursor", name: "retained", membershipType: "pro" }];
    f.change([gpt]);
    if (hidden) {
      assert.equal(f.calls.length, 0, "hidden pages defer loading until reopened");
      await f.show();
    } else await tick();

    assert.equal(f.el("#sum-equivalent").textContent, "$900.00");
    assert.equal(f.el("#sum-plan").textContent, "$220 · 4.1×");
    const rows = f.rows();
    assert.equal(rows.filter(row => row[0].includes("已删除")).length, 1);
    assert.deepEqual(rows.at(-1).slice(3), ["$220/月", "300", "$900.00", "4.1×"]);
    assert.ok(rows.every(row => row.length === 7));
    assert.deepEqual(f.calls.map(c => c.cmd), ["cursor_usage_deleted_list", "cursor_usage_slice"]);
    assert.equal(f.calls[1].args.accountId, "cursor");

    // 再次进入页面命中新鲜缓存；套餐变化仍重算包含保留记录的合计，不重新扫描。
    f.calls.length = 0;
    await f.hide(); await f.show();
    f.change([account("gpt", "codex", "prolite")]);
    assert.equal(f.el("#sum-plan").textContent, "$120 · 7.5×");
    assert.equal(f.rows().at(-1)[6], "7.5×");
    assert.equal(f.calls.length, 0);
  });
}

test("device filtering changes usage but never multiplies account monthly fees", async () => {
  const f = await fixture([account("p", "codex", "pro"), account("l", "codex", "prolite")]);
  f.api.choose(["local"]); f.api.renderLocalScan("local", scan(900), Date.now());
  const device = f.el("#usage-codex-device");
  device.firstElementChild.value = "device-a";
  f.api.renderLocalScan("local", scan(300), Date.now());
  assert.equal(f.el("#sum-plan").textContent, "$300 · 1.0×");
});

test("manual refresh includes accounts of selected GPT/Claude platforms only", async () => {
  const f = await fixture([account("p", "codex", "pro"), account("l", "codex", "prolite"), account("c", "claude", "pro"),
    account("cursor", "cursor", null, { membershipType: "pro" })]);
  f.api.choose(["local-claude"]); await f.api.refreshUsage();
  assert.deepEqual(f.calls.find(c => c.cmd === "refreshAccounts").ids, ["c"]);
  f.calls.length = 0;
  f.api.choose(["local", "cursor"]); await f.api.refreshUsage();
  assert.deepEqual(f.calls.find(c => c.cmd === "refreshAccounts").ids.sort(), ["cursor", "l", "p"]);
});

test("OpenCode plans use official prices, Teams member count and payg without a subscription fee", async () => {
  const { format } = await fixture();
  for (const [plan, extra, expected] of [["go", {}, 10], ["go-plus", {}, 40],
    ["go-teams", { memberCount: 3 }, 60], ["go-teams", {}, null], ["payg", {}, 0], ["unknown", {}, null]]) {
    assert.equal(format.accountPlanInfo(account("o", "opencode", plan, extra)).monthlyUsd, expected, plan);
  }
});
test("Cursor and OpenCode usage show equivalent costs while retaining coverage and reconciliation", async () => {
  const opencode = account("o", "opencode", "go-plus");
  const cursor = account("c", "cursor", null, { membershipType: "pro" });
  const f = await fixture([opencode, cursor]);
  const agg = { ...scan(80).aggregate, source: "opencode", totalReportedCostUsd: 12,
    models: [{ ...scan(80).aggregate.models[0], reportedCostUsd: 12 }], coverageGaps: [{ sinceMs: 1, untilMs: 2 }],
    coverageStart: Date.now() - 86400000, reconciliation: { state: "matched", date: "2026-11-09" } };
  const cursorAgg = { ...scan(20).aggregate, totalActualUsd: 3,
    models: [{ ...scan(20).aggregate.models[0], actualUsd: 3 }] };
  const assertNoSpending = () => {
    assert.equal(f.el("#usage-results").querySelector("#card-actual"), null);
    assert.equal(f.el("#usage-results").querySelector("#sum-actual"), null);
    assert.doesNotMatch(f.el("#usage-results").textContent, /实际支出|记录成本|实扣/);
    assert.doesNotMatch(f.el("#usage-overview").textContent, /实际支出|记录成本|实扣/);
    assert.equal(f.el("#usage-body").closest("table").querySelectorAll("thead th").length, 7);
    assert.ok([...f.el("#usage-body").children].every(row => row.children.length === 7));
  };
  f.api.choose(["o"]); f.api.renderCursorAccount(opencode, agg, Date.now());
  assertNoSpending();
  assert.equal(f.el("#sum-equivalent").textContent, "$80.00");
  assert.equal(f.el("#usage-body").firstElementChild.children[6].textContent, "$80.00");
  assert.equal(f.el("#sum-plan").textContent, "$40 · 2.0×");
  assert.match(f.el("#usage-meta").textContent, /历史缺口/);
  assert.match(f.el("#usage-meta").textContent, /官方模型 Token 总量核对一致/);
  f.api.choose(["c"]); f.api.renderCursorAccount(cursor, cursorAgg, Date.now());
  assertNoSpending();
  assert.equal(f.el("#sum-equivalent").textContent, "$20.00");
  assert.equal(f.el("#sum-plan").textContent, "$20 · 1.0×");
  f.api.choose(["o", "c"]); f.api.seed([["o", { agg }], ["c", { agg: cursorAgg }]]);
  f.api.renderOverviewTable(); f.api.renderOverviewMerged();
  assertNoSpending();
  assert.equal(f.el("#sum-equivalent").textContent, "$100.00");
  assert.equal(f.el("#sum-plan").textContent, "$60 · 1.7×");
  const row = f.rows().find(r => r[0].includes("opencode"));
  assert.match(row[0], /历史覆盖不完整/);
  assert.deepEqual(f.rows().at(-1).slice(3), ["$60/月", "200", "$100.00", "1.7×"]);
  for (const period of ["day", "week", "month", "all"]) {
    f.api.choose(["o", "c"], period);
    f.api.renderOverviewTable(); f.api.renderOverviewMerged();
    assertNoSpending();
    assert.equal(f.el("#sum-equivalent").textContent, "$100.00");
    assert.equal(f.el("#card-plan").hidden, false);
    assert.ok(f.rows().every(row => row.length === 7));
  }
  f.api.choose(["o"]); f.api.renderCursorAccount(opencode, { ...agg, models: [], totalTokens: 0, totalEquivalentUsd: 0 }, Date.now());
  assertNoSpending();
  assert.equal(f.el("#usage-body").children.length, 0);
});
test("OpenCode refresh uses only its workspace archive IPC and does not send credentials", async () => {
  const opencode = { ...account("o", "opencode", "go"), token: "wrk_fixture", opencodeAuthorized: true };
  const f = await fixture([opencode]);
  f.api.choose(["o"]);
  await f.api.refreshUsage();
  const call = f.calls.find(c => c.cmd === "opencode_usage_fetch");
  assert.ok(call); assert.equal(call.args.accountId, "o"); assert.equal(call.args.mode, "full");
  assert.equal("token" in call.args, false); assert.equal("refreshToken" in call.args, false);
  assert.equal(f.calls.some(c => c.cmd === "cursor_usage_fetch"), false);
});


test("all ranges show monthly fees and ratios with a uniform table format", async () => {
  const accounts = [account("g", "codex", "pro"), account("o", "opencode", "go"),
    account("c", "cursor", null, { membershipType: "free" }), account("a", "claude", "unknown")];
  const f = await fixture(accounts);
  f.api.seed([["o", { agg: scan(10).aggregate }], ["c", { agg: scan(0).aggregate }],
    ["local", { scan: scan(200) }], ["local-claude", { scan: scan(0) }]]);
  for (const mode of ["day", "week", "month", "all", "custom"]) {
    f.api.choose([], mode);
    if (mode === "custom") f.api.customRange("2026-11-09", "2026-11-10");
    f.api.renderOverviewTable(); f.api.renderOverviewMerged();
    assert.equal(f.el("#card-plan").hidden, false);
    assert.equal(f.el("#sum-plan").textContent, "$210 · 1.0×");
    assert.deepEqual(f.rows().slice(0, -1).map(row => row[3]).sort(), ["$0/月", "$10/月", "$200/月", "—"].sort());
    assert.equal(f.rows().at(-1)[6], "1.0×");
    assert.equal(f.el("#usage-overview-body").closest("table").querySelectorAll("th")[3].textContent, "套餐月费");
  }
});

test("OpenCode usage names follow the account note and update charts without rescanning", async () => {
  const original = { ...account("o", "opencode", "go", { name: "Personal", workspaceName: "Personal", email: "a@example.invalid" }),
    note: "a@example.invalid", noteAuto: true };
  const f = await fixture([original], { charts: true });
  f.api.choose(["o"]); f.api.renderCursorAccount(original, scan(10).aggregate, Date.now());
  assert.doesNotMatch(f.el("#usage-sources").textContent, /Personal/);
  assert.equal(f.el("#usage-sources").querySelector(".chip-sub"), null);
  assert.deepEqual(Array.from(f.charts.get("chart-sources").data.labels), ["a@example.invalid"]);
  f.calls.length = 0;
  f.change([{ ...original, note: "工作账户", noteAuto: false }]);
  assert.match(f.el("#usage-sources").textContent, /工作账户/);
  assert.match(f.el("#usage-meta").textContent, /工作账户/);
  assert.deepEqual(Array.from(f.charts.get("chart-sources").data.labels), ["工作账户"]);
  assert.equal(f.calls.filter(call => /usage_query|scan_sessions|usage_fetch/.test(call.cmd)).length, 0);
  f.change([{ ...original, note: "" }]);
  assert.deepEqual(Array.from(f.charts.get("chart-sources").data.labels), ["a@example.invalid"]);
});

function usageTokens(tokens) {
  const agg = scan(tokens).aggregate;
  return { ...agg, totalTokens: tokens, models: [{ ...agg.models[0], totalTokens: tokens, inputTokens: tokens }] };
}

test("source composition uses aggregate tokens, keeps same-name accounts distinct and includes shared and archived sources", async () => {
  const accounts = ["one", "two"].map(id => ({ ...account(id, "cursor", null, { membershipType: "pro" }), note: "same", noteAuto: false }));
  const f = await fixture(accounts, { charts: true });
  f.api.choose();
  f.api.seed([["one", { agg: usageTokens(100) }], ["two", { agg: usageTokens(200) }],
    ["local", { scan: { ...scan(), aggregate: usageTokens(300) } }],
    ["local-claude", { scan: { ...scan(), aggregate: usageTokens(400) } }],
    ["archived", { agg: usageTokens(50), state: "error", error: "cached" }]], [{ accountId: "archived", note: "saved" }]);
  f.api.renderOverviewMerged();
  const chart = f.charts.get("chart-sources");
  assert.deepEqual(Array.from(chart.data.datasets[0].data), [400, 300, 200, 100, 50]);
  assert.equal(chart.data.labels.filter(label => label === "same").length, 2);
  assert.ok(chart.data.labels.some(label => label.includes("已删除")));
  assert.equal(f.el("#sum-tokens").textContent, "1050");
  assert.equal(f.el("#legend-sources").children.length, 5);
  f.el("#legend-sources").firstElementChild.click();
  assert.equal(chart.getDataVisibility(0), false);
  assert.equal(f.el("#legend-sources").firstElementChild.classList.contains("slice-hidden"), true);
  f.el("#legend-sources").firstElementChild.click();
  assert.equal(chart.getDataVisibility(0), true);
});

test("source chart handles Top 8, single and empty sources, and participates in theme, resize and cleanup", async () => {
  const accounts = Array.from({ length: 10 }, (_, i) => ({ ...account(`a${i}`, "cursor", null), note: `account-${i}`, noteAuto: false }));
  const f = await fixture(accounts, { charts: true });
  f.api.choose(accounts.map(account => account.id));
  f.api.seed(accounts.map((account, i) => [account.id, { agg: usageTokens(i + 1) }]));
  f.api.renderOverviewMerged();
  const chart = f.charts.get("chart-sources");
  assert.equal(chart.data.labels.length, 9);
  assert.equal(chart.data.labels.at(-1), "其他");
  assert.equal(chart.data.datasets[0].data.at(-1), 3);
  assert.equal(chart.data.datasets[0].data.reduce((total, n) => total + n, 0), 55);
  f.api.choose(["a0"]); f.api.renderCursorAccount(accounts[0], usageTokens(40), Date.now());
  assert.equal(f.charts.get("chart-sources"), chart);
  const tooltip = chart.options.plugins.tooltip.callbacks.label({ chart, dataset: chart.data.datasets[0], parsed: 40, label: "account-0" });
  assert.match(tooltip, /100%/);
  f.hooks.theme = "#abcdef"; f.dispatch("themechange");
  assert.equal(chart.data.datasets[0].borderColor, "#abcdef");
  f.el("#usage-panel").hidden = false;
  f.api.scheduleUsageChartResize(); f.flushFrames();
  assert.ok(chart.resizes > 0);
  f.api.renderCursorAccount(accounts[0], { ...usageTokens(0), models: [] }, Date.now());
  assert.equal(chart.data.datasets[0].data.length, 0);
  assert.equal(f.el("#legend-sources").children.length, 0);
  f.api.destroyUsageCharts();
  assert.equal(chart.destroyed, true);
  assert.equal(f.charts.size, 0);
});
