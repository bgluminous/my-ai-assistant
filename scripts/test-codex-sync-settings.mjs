// node --experimental-vm-modules scripts/test-codex-sync-settings.mjs
// 使用隔离的 IPC 和 DOM 替身，验证账户提示与测试连接、手动恢复的行为。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../src/usage_sync.js", import.meta.url), "utf8");
const promptSource = await readFile(new URL("../src/usage_sync_prompt.js", import.meta.url), "utf8");
async function fixture(state = "idle") {
  const requests = [];
  const nodes = new Map();
  function el(id) {
    if (!nodes.has(id)) nodes.set(id, {
      value: "", checked: false, disabled: false, handlers: new Map(),
      addEventListener(name, handler) { this.handlers.set(name, handler); },
    });
    return nodes.get(id);
  }
  const config = {
    serverUrl: "https://sync.example.test/maa", hasKey: true, intervalMinutes: 5,
    sourceHome: "", deviceName: "设备 A",
  };
  const status = { state, pendingRecords: 2, lastSuccessAt: null, error: state === "paused" ? "sync_forbidden" : null };
  const context = vm.createContext({ document: { querySelector: el } });
  const shared = new vm.SyntheticModule(["el", "invoke", "listen", "fillStatus", "resetError", "copyText", "toast"], function () {
    this.setExport("el", el);
    this.setExport("invoke", async (command, args) => {
      requests.push({ command, args });
      if (command === "codex_usage_sync_get") return config;
      if (command === "codex_usage_sync_status") return status;
      if (command === "codex_usage_sync_test") return { protocolVersion: 1, spaceId: "account", epoch: "epoch" };
      if (command === "codex_usage_sync_now") {
        status.state = "idle";
        status.error = null;
        status.pendingRecords = 0;
        return status;
      }
      if (command === "codex_usage_sync_set") return config;
      throw new Error(`Unexpected command: ${command}`);
    });
    this.setExport("listen", async () => () => {});
    this.setExport("fillStatus", (node, kind, text) => { node.kind = kind; node.textContent = text; });
    this.setExport("resetError", (error) => String(error));
    this.setExport("copyText", async () => {});
    this.setExport("toast", () => {});
  }, { context });
  const prompt = new vm.SourceTextModule(promptSource, { context });
  const module = new vm.SourceTextModule(source, { context });
  await module.link((specifier) => {
    if (specifier === "./shared.js") return shared;
    if (specifier === "./usage_sync_prompt.js") return prompt;
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
  assert.match(f.el("#gpt-sync-result").textContent, /未上传数据或登记设备/);
});

test("disabled account remains visible and can recover with manual sync", async () => {
  const f = await fixture("paused");
  assert.match(f.el("#gpt-sync-status").textContent, /账户已停用/);
  assert.match(f.el("#gpt-sync-status").textContent, /联系服务端管理员/);
  assert.equal(f.el("#gpt-sync-now").disabled, false);
  await f.click("#gpt-sync-now");
  assert.deepEqual(f.requests.filter((r) => r.command !== "codex_usage_sync_status").map((r) => r.command), ["codex_usage_sync_now"]);
  assert.match(f.el("#gpt-sync-result").textContent, /同步完成/);
  assert.match(f.el("#gpt-sync-status").textContent, /已就绪/);
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
