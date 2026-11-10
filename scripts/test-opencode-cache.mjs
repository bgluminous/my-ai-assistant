// node --experimental-vm-modules --test scripts/test-opencode-cache.mjs
// 不启动应用、浏览器或中心服务；验证跨窗口事件与在途请求的缓存隔离。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../src/usage_data.js", import.meta.url), "utf8");
async function fixture(invoke) {
  const values = new Map();
  const listeners = new Map();
  const target = new EventTarget();
  class DetailEvent extends Event { constructor(type, options) { super(type); this.detail = options?.detail; } }
  const storage = {
    get length() { return values.size; },
    key: (i) => [...values.keys()][i] ?? null,
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
  const context = vm.createContext({
    console, setTimeout, clearTimeout, CustomEvent: DetailEvent, localStorage: storage,
    window: { setTimeout, clearTimeout, dispatchEvent: target.dispatchEvent.bind(target) },
  });
  const shared = new vm.SyntheticModule(["invoke", "emit", "listen"], function () {
    this.setExport("invoke", invoke);
    this.setExport("emit", async () => {});
    this.setExport("listen", async (name, fn) => { listeners.set(name, fn); return () => {}; });
  }, { context });
  const module = new vm.SourceTextModule(source, { context });
  await module.link(() => shared);
  await module.evaluate();
  return { api: module.namespace, change: payload => listeners.get("opencode-usage-changed")({ payload }) };
}
const account = { id: "open", kind: "opencode", token: "wrk_fixture" };
const result = (tokens, syncedAt = Date.now(), syncError = null) => ({
  agg: { source: "opencode", totalTokens: tokens, models: [], totalActualUsd: 0, totalReportedCostUsd: 5 }, syncedAt, syncError,
});
test("OpenCode cache is reused by account and date; other workspaces remain isolated", async () => {
  const calls = [];
  const f = await fixture(async (cmd, args) => { calls.push({ cmd, args }); return result(10); });
  f.api.syncCursorUsageFilters([account]);
  await f.api.fetchCursorAggregate(account, "today:2026-11-10");
  await f.api.fetchCursorAggregate(account, "today:2026-11-10");
  assert.equal(calls.length, 1);
  assert.equal(f.api.getCachedAgg("open", "today:2026-11-10").agg.totalTokens, 10);
  assert.equal(f.api.getCachedAgg("other", "today:2026-11-10"), null);
  assert.equal(calls[0].cmd, "opencode_usage_fetch");
  assert.equal("token" in calls[0].args, false);
});
test("failed sync keeps the newest partial archive while preserving its old watermark", async () => {
  let value = result(10, 1000);
  const f = await fixture(async () => value);
  await f.api.fetchCursorAggregate(account, "all", { force: true });
  value = result(15, 1000, "opencode_network_error");
  await assert.rejects(f.api.fetchCursorAggregate(account, "all", { full: true }), error => {
    assert.equal(error.usageCache.agg.totalTokens, 15);assert.equal(error.usageCache.at, 1000);return true;
  });
  assert.equal(f.api.getCachedAgg("open", "all").agg.totalTokens, 15);
});
test("backup restore purges old aggregates while normal sync notices preserve fresh data", async () => {
  const f = await fixture(async () => result(10));f.api.syncCursorUsageFilters([account]);
  await f.api.fetchCursorAggregate(account, "all");
  f.change({ workspaceId: "wrk_fixture" });assert.ok(f.api.getCachedAgg("open", "all"));
  f.change({});assert.equal(f.api.getCachedAgg("open", "all"), null);
});
test("late responses after account removal cannot refill shared caches", async () => {
  let finish;
  const f = await fixture(() => new Promise(resolve => { finish = resolve; }));
  const pending = f.api.fetchCursorAggregate(account, "all");f.api.purgeAccountCache("open");
  finish(result(99));await pending;assert.equal(f.api.getCachedAgg("open", "all"), null);
});
