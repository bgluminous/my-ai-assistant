// node --experimental-vm-modules scripts/test-codex-cache.mjs
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
  return { api: module.namespace, change: (reason) => listeners.get("codex-usage-changed")({ payload: { reason } }) };
}
function scan(tokens) { return { aggregate: { totalTokens: tokens }, roots: [], sessions: 1, filesScanned: 1 }; }

test("old request cannot restore a previous space after configuration changes", async () => {
  let finishOld;
  let calls = 0;
  const f = await fixture(() => ++calls === 1 ? new Promise((resolve) => { finishOld = resolve; }) : Promise.resolve(scan(20)));
  const old = f.api.fetchCodexScan({ key: "all" });
  const rejected = assert.rejects(old, /sync_cancelled/);
  f.change("config");
  const fresh = await f.api.fetchCodexScan({ key: "all" });
  finishOld(scan(999));
  await rejected;
  assert.equal(fresh.scan.aggregate.totalTokens, 20);
  assert.equal(f.api.getCachedScan("all", "").scan.aggregate.totalTokens, 20);
});

test("device-filtered requests never overwrite the all-device cache", async () => {
  const calls = [];
  const f = await fixture((cmd, args) => { calls.push({ cmd, args }); return Promise.resolve(scan(args.deviceId ? 10 : 30)); });
  await f.api.fetchCodexScan({ key: "all", deviceId: "device-a" });
  await f.api.fetchCodexScan({ key: "all" });
  assert.equal(calls.length, 2);
  assert.equal(f.api.getCachedScan("all", "", "device-a").scan.aggregate.totalTokens, 10);
  assert.equal(f.api.getCachedScan("all", "").scan.aggregate.totalTokens, 30);
});

test("clearing reloads only the archive until an explicit scan or sync", async () => {
  const calls = [];
  const f = await fixture((cmd) => { calls.push(cmd); return Promise.resolve(scan(1)); });
  await f.api.fetchCodexScan({ key: "all" });
  f.change("clear");
  await f.api.fetchCodexScan({ key: "all" });
  await f.api.fetchCodexScan({ key: "all", force: true });
  assert.deepEqual(calls, ["codex_scan_sessions", "codex_usage_query", "codex_scan_sessions"]);
});

test("unrelated Claude scans continue to use their existing command", async () => {
  const calls = [];
  const f = await fixture((cmd) => { calls.push(cmd); return Promise.resolve(scan(1)); });
  f.change("clear");
  await f.api.fetchClaudeScan({ key: "all" });
  assert.deepEqual(calls, ["claude_scan_sessions"]);
});

test("main and tray caches reload the same all-device archive after sync", async () => {
  let tokens = 10;
  const calls = [];
  const backend = (cmd, args) => { calls.push({ cmd, args }); return Promise.resolve(scan(tokens)); };
  const main = await fixture(backend);
  const tray = await fixture(backend);
  await main.api.fetchCodexScan({ key: "all" });
  await tray.api.fetchCodexScan({ key: "all" });
  tokens = 25;
  for (const f of [main, tray]) f.change("sync");
  for (const f of [main, tray]) {
    assert.equal(f.api.getCachedScan("all", ""), null);
    const result = await f.api.fetchCodexScan({ key: "all" });
    assert.equal(result.scan.aggregate.totalTokens, 25);
  }
  assert.ok(calls.slice(-2).every((c) => c.cmd === "codex_usage_query" && c.args.deviceId === null));
});
