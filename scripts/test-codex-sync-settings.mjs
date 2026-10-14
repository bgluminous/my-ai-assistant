// node --experimental-vm-modules scripts/test-codex-sync-settings.mjs
// 使用隔离的 IPC 和 DOM 替身，验证精简设置后的测试连接、保存与账户提示。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const source = await readFile(new URL("../src/usage_sync.js", import.meta.url), "utf8");
const promptSource = await readFile(new URL("../src/usage_sync_prompt.js", import.meta.url), "utf8");
const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
const ids = new Set([...html.matchAll(/id="([^"]+)"/g)].map((m) => `#${m[1]}`));
async function fixture(state = "idle") {
  const requests = [];
  const { document } = parseHTML(html);
  function el(id) {
    assert(ids.has(id), `Unknown HTML element ${id}`);
    const node = document.querySelector(id);
    if (!node.handlers) {
      node.handlers = new Map();
      node.addEventListener = (name, handler) => node.handlers.set(name, handler);
    }
    return node;
  }
  const config = {
    serverUrl: "https://sync.example.test/maa", hasKey: true, intervalMinutes: 5,
    sourceHome: "", deviceName: "设备 A",
  };
  const status = { state, pendingRecords: 2, lastSuccessAt: null, error: state === "paused" ? "sync_forbidden" : null };
  const context = vm.createContext({ document, setTimeout, clearTimeout });
  const shared = new vm.SyntheticModule(["el", "invoke", "listen", "fillStatus", "resetError", "copyText", "toast"], function () {
    this.setExport("el", el);
    this.setExport("invoke", async (command, args) => {
      requests.push({ command, args });
      if (command === "codex_usage_sync_get") return config;
      if (command === "codex_usage_sync_status") return status;
      if (command === "codex_usage_sync_test") return { protocolVersion: 1, spaceId: "account", epoch: "epoch" };
      if (command === "codex_usage_sync_set") {
        const { key, ...values } = args.config;
        Object.assign(config, values, { hasKey: key === null ? config.hasKey : !!key });
        return config;
      }
      throw new Error(`Unexpected command: ${command}`);
    });
    this.setExport("listen", async () => () => {});
    this.setExport("fillStatus", (node, kind, text) => { node.kind = kind; node.textContent = text; });
    this.setExport("resetError", (error) => String(error));
    this.setExport("copyText", async () => {});
    this.setExport("toast", () => {});
  }, { context });
  const prompt = new vm.SourceTextModule(promptSource, { context });
  const errors = new vm.SourceTextModule(await readFile(new URL("../src/sync_errors.js", import.meta.url), "utf8"), { context });
  await errors.link(() => shared);
  const progress = new vm.SourceTextModule(await readFile(new URL("../src/sync_progress.js", import.meta.url), "utf8"), { context });
  await progress.link((specifier) => specifier === "./shared.js" ? shared : errors);
  const module = new vm.SourceTextModule(source, { context });
  await module.link((specifier) => {
    if (specifier === "./shared.js") return shared;
    if (specifier === "./usage_sync_prompt.js") return prompt;
    if (specifier === "./sync_errors.js") return errors;
    if (specifier === "./sync_progress.js") return progress;
    throw new Error(`Unexpected import: ${specifier}`);
  });
  await module.evaluate();
  const api = module.namespace;
  api.initSyncSettings();
  await api.loadSyncSettings();
  requests.length = 0;
  return { api, el, requests, click: (id) => el(id).handlers.get("click")() };
}

test("connection test uses the form without saving or starting sync", async () => {
  const f = await fixture();
  f.el("#gpt-sync-key").value = "replacement-test-key";
  await f.click("#gpt-sync-test");
  const actions = f.requests.filter((r) => r.command !== "codex_usage_sync_status");
  assert.equal(actions.length, 1);
  assert.equal(actions[0].command, "codex_usage_sync_test");
  assert.equal(actions[0].args.config.serverUrl, "https://sync.example.test/maa");
  assert.equal(actions[0].args.config.key, "replacement-test-key");
  assert.equal("intervalMinutes" in actions[0].args.config, false);
  assert.match(f.el("#gpt-sync-result").textContent, /未上传数据或登记设备/);
});

test("paused account stays visible in usage and the Done save path remains available", async () => {
  const f = await fixture("paused");
  assert.match(f.el("#usage-sync-summary").textContent, /账户已停用/);
  assert.match(f.el("#usage-sync-summary").textContent, /联系服务端管理员/);
  assert.match(f.el("#usage-sync-summary").textContent, /统计页.*刷新/);
  f.el("#gpt-sync-device-name").value = "设备 B";
  assert.equal(await f.api.saveSyncSettings(true), true);
  const saves = f.requests.filter((r) => r.command === "codex_usage_sync_set");
  assert.equal(saves.length, 1);
  assert.equal(saves[0].args.config.deviceName, "设备 B");
  assert.equal(saves[0].args.config.key, null);
  assert.equal(f.el("#gpt-sync-key").value, "");
  assert.equal(f.el("#gpt-sync-test").disabled, false);
  f.requests.length = 0;
  assert.equal(await f.api.saveSyncSettings(true), true);
  assert.equal(f.requests.length, 0);
});

test("reset key tells users to update every machine without exposing the saved key", async () => {
  const f = await fixture();
  const message = f.api.syncError("sync_unauthorized");
  assert.match(message, /无效或已被重置/);
  assert.match(message, /所有机器/);
  assert.doesNotMatch(message, /账户已停用/);
  assert.equal(f.el("#gpt-sync-key").value, "");
  assert.match(f.el("#gpt-sync-key").placeholder, /留空保留/);
});
