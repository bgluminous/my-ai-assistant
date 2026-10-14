// node --experimental-vm-modules --test scripts/test-codex-sync-progress.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const code = await readFile(new URL("../src/sync_progress.js", import.meta.url), "utf8");
const errors = await readFile(new URL("../src/sync_errors.js", import.meta.url), "utf8");
const tick = () => new Promise(resolve => setImmediate(resolve));
const snapshot = (sequence, phase = "uploading", extra = {}) => ({
  sequence, state: "running", pendingRecords: 1800, lastSuccessAt: null, error: null,
  progress: { roundId: 1, phase, uploadedRecords: 500, uploadTotal: 2300,
    downloadedRecords: 800, downloadedPages: 2, finishedAt: null }, ...extra,
});
async function fixture(initial = snapshot(1), delayed = null) {
  const { document } = parseHTML('<main><div id="stats"></div><div id="settings"></div><div id="tray"></div></main>');
  const listeners = new Map(), calls = [], timers = new Map();
  let now = 100_000, timerId = 0;
  class Clock extends Date { static now() { return now; } }
  const context = vm.createContext({ document, Date: Clock,
    setTimeout: (callback, ms) => { const id = ++timerId; timers.set(id, { at: now + ms, callback }); return id; },
    clearTimeout: id => timers.delete(id),
  });
  const shared = new vm.SyntheticModule(["invoke", "listen", "resetError"], function () {
    this.setExport("invoke", async cmd => {
      calls.push(cmd);
      assert(listeners.has("codex-usage-sync-status"), "subscribe before snapshot");
      assert.equal(cmd, "codex_usage_sync_status", "progress must not start sync or reload statistics");
      return delayed ? delayed : initial;
    });
    this.setExport("listen", async (name, fn) => { assert(!listeners.has(name)); listeners.set(name, fn); return () => {}; });
    this.setExport("resetError", error => String(error).replace(/^Error: /, ""));
  }, { context });
  const errorModule = new vm.SourceTextModule(errors, { context });
  await errorModule.link(() => shared);
  const module = new vm.SourceTextModule(code, { context });
  await module.link(path => path === "./shared.js" ? shared : errorModule);
  await module.evaluate();
  const api = module.namespace;
  const node = id => document.getElementById(id);
  api.mountSyncProgress(node("stats"));
  api.mountSyncProgress(node("settings"), { showDisabled: true });
  api.mountSyncProgress(node("tray"), { compact: true });
  await tick();
  return { api, node, calls, timers, document,
    send: status => listeners.get("codex-usage-sync-status")({ payload: status }),
    advance(ms) {
      now += ms;
      for (const [id, task] of [...timers]) if (task.at <= now) { timers.delete(id); task.callback(); }
    },
  };
}

test("three surfaces use real upload progress and share one read without starting sync", async () => {
  const f = await fixture();
  assert.deepEqual(f.calls, ["codex_usage_sync_status"]);
  for (const id of ["stats", "settings", "tray"]) {
    const n = f.node(id);
    assert.equal(n.hidden, false);
    assert.match(n.textContent, /500 \/ 2,300 条/);
    assert.equal(n.querySelector("progress").getAttribute("value"), "21");
  }
  f.send(snapshot(2, "downloading"));
  assert.match(f.node("stats").textContent, /800 条 · 2 页/);
  assert.equal(f.node("stats").querySelector("progress").hasAttribute("value"), false);
  assert.equal(f.calls.length, 1);
});

test("late query and out of order events cannot overwrite newer progress", async () => {
  let resolve;
  const delayed = new Promise(yes => { resolve = yes; });
  const f = await fixture(snapshot(1), delayed);
  f.send(snapshot(10, "downloading"));
  resolve(snapshot(1));
  await tick();
  f.send(snapshot(9, "uploading"));
  assert.match(f.node("stats").textContent, /正在下载/);
  const late = f.document.createElement("div");
  f.api.mountSyncProgress(late);
  assert.match(late.textContent, /正在下载/);
  await tick();
  assert.match(late.textContent, /正在下载/);
});

test("success collapses only in tray after five seconds and reopening does not extend it", async () => {
  const f = await fixture();
  const done = snapshot(2, "completed", { state: "idle", lastSuccessAt: 100_000 });
  done.progress.finishedAt = 100_000;
  f.send(done);
  f.advance(4999);
  assert.equal(f.node("tray").hidden, false);
  f.advance(1);
  assert.equal(f.node("tray").hidden, true);
  assert.equal(f.node("stats").hidden, false);
  assert.match(f.node("settings").textContent, /已上传 500 条 · 已下载 800 条/);
  f.send(done);
  assert.equal(f.node("tray").hidden, true);
});

test("failures persist with committed counts; new configuration hides unconfigured views", async () => {
  const f = await fixture();
  f.send(snapshot(2, "failed", { state: "paused", error: "sync_forbidden" }));
  f.advance(60_000);
  assert.equal(f.node("tray").hidden, false);
  assert.match(f.node("tray").textContent, /账户已停用/);
  assert.match(f.node("stats").textContent, /已上传 500 条/);
  assert.equal(f.node("tray").querySelector("progress").hidden, true);
  f.send({ sequence: 3, state: "disabled", pendingRecords: 0, progress: null, error: null });
  assert.equal(f.node("stats").hidden, true);
  assert.equal(f.node("tray").hidden, true);
  assert.equal(f.node("settings").hidden, false);
  assert.match(f.node("settings").textContent, /未启用，仅采集本机数据/);
  f.send(snapshot(2));
  assert.equal(f.node("stats").hidden, true);
});

test("zero uploads and early phases never claim an overall percentage", async () => {
  const f = await fixture();
  for (const phase of ["waiting", "scanning", "storing", "connecting", "recovering", "uploading"]) {
    const s = snapshot(2, phase);
    s.progress.uploadTotal = 0;
    s.progress.uploadedRecords = 0;
    f.send(s);
    assert.equal(f.node("stats").querySelector("progress").hasAttribute("value"), false);
    assert.equal(f.node("stats").querySelector(".sync-progress-percent").hidden, true);
  }
});

test("status lives outside source filters and has separate device errors", async () => {
  const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
  const { document } = parseHTML(html);
  assert.equal(document.querySelector("#usage-local-form #usage-sync-summary"), null);
  assert.ok(document.querySelector("#settings-sync #gpt-sync-progress"));
  assert.ok(document.querySelector("#usage-codex-device-error"));
  const tray = parseHTML(await readFile(new URL("../src/tray.html", import.meta.url), "utf8")).document;
  assert.ok(tray.querySelector("#tray-sync-progress"));
});
