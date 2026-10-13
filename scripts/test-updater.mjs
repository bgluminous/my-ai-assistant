// node --experimental-vm-modules scripts/test-updater.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../src/app_update.js", import.meta.url), "utf8");
const presentation = await readFile(new URL("../src/update_status.js", import.meta.url), "utf8");
const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
const defaults = { revision: 0, phase: "idle", currentVersion: "1.3.0", automatic: true,
  releasesUrl: "https://github.com/bgluminous/my-ai-assistant/releases" };
async function fixture(initial = defaults) {
  class Node {
    hidden = false; disabled = false; textContent = ""; dataset = {}; listeners = new Map();
    classList = { toggle() {} };
    addEventListener(name, fn) { this.listeners.set(name, fn); }
    async click() { if (!this.disabled) await this.listeners.get("click")?.({ target: this }); }
    focus() {} removeAttribute(name) { delete this[name]; }
    querySelectorAll() { return []; }
  }
  const nodes = new Map([...html.matchAll(/id="([^"]+)"/g)].map(m => [`#${m[1]}`, new Node()]));
  const about = new Node(); about.dataset.panel = "about-panel";
  const business = new Node(); business.dataset.panel = "usage-panel";
  const panel = new Node(); panel.id = "usage-panel";
  const selects = "[role='tab'][data-panel='about-panel']";
  nodes.set(selects, about);
  const events = new Map(); const calls = []; let shown = 0;
  const context = vm.createContext({ console,
    window: { __TAURI__: { core: {}, window: { getCurrentWindow: () => ({ show() { shown++; }, setFocus() { shown++; } }) } } },
    document: { addEventListener() {}, querySelectorAll: (query) => query.includes("tabpanel") ? [panel] : query.includes("role='tab'") ? [about, business] : [] },
  });
  const invoke = async (command, args) => { calls.push({ command, args }); return initial; };
  const exports = { el: (id) => { assert(nodes.has(id), `Unknown HTML element ${id}`); return nodes.get(id); }, invoke,
    listen: async (name, fn) => { events.set(name, fn); },
    fillStatus: (node, kind, text) => { node.textContent = text; node.hidden = false; },
    fmtDateMs: () => "date", toast: () => {}, copyText: async () => {} };
  const shared = new vm.SyntheticModule(Object.keys(exports), function () {
    for (const [k, v] of Object.entries(exports)) this.setExport(k, v);
  }, { context });
  const status = new vm.SourceTextModule(presentation, { context });
  await status.link(() => {}); await status.evaluate();
  const module = new vm.SourceTextModule(source, { context });
  await module.link(name => name === "./shared.js" ? shared : status); await module.evaluate();
  await module.namespace.initAppUpdate();
  return { nodes, calls, business, panel, status: status.namespace, shown: () => shown,
    emit: value => events.get("app-update-state")({ payload: { ...defaults, ...value } }) };
}

test("silent startup and background failures never open a browser or show a window", async () => {
  const f = await fixture();
  assert.deepEqual(f.calls.map(x => x.command), ["app_update_status"]);
  f.emit({ revision: 1, phase: "error", error: { message: "连接超时" } });
  assert.equal(f.nodes.get("#app-update-status").textContent, "连接超时");
  assert.equal(f.nodes.get("#app-update-chip").hidden, false);
  assert.equal(f.nodes.get("#app-update-check").textContent, "重试");
  assert.equal(f.shown(), 0);
  assert.equal(f.calls.length, 1);
  await f.nodes.get("#app-update-releases").click();
  assert.equal(f.calls.at(-1).command, "app_update_open_releases");
});
test("installation requires confirmation and keeps the confirmed version", async () => {
  const f = await fixture();
  f.emit({ revision: 1, phase: "available", latestVersion: "1.4.0", canInstall: true, changelog: "<script>untrusted</script>" });
  await f.nodes.get("#app-update-install").click();
  assert.equal(f.nodes.get("#app-update-confirm").hidden, false);
  assert.equal(f.calls.some(c => c.command === "app_update_install"), false);
  f.emit({ revision: 2, phase: "available", latestVersion: "1.5.0", canInstall: true });
  await f.nodes.get("#app-update-confirm-ok").click();
  assert.equal(f.calls.at(-1).command, "app_update_install");
  assert.equal(f.calls.at(-1).args.version, "1.4.0");
});
test("progress, cancellation, stale events and required updates remain visible", async () => {
  const f = await fixture();
  f.emit({ revision: 4, phase: "downloading", downloadedBytes: 50, totalBytes: 100 });
  assert.equal(f.nodes.get("#app-update-progress").value, 50);
  await f.nodes.get("#app-update-cancel").click();
  assert.equal(f.calls.at(-1).command, "app_update_cancel");
  f.emit({ revision: 3, phase: "upToDate" });
  assert.equal(f.nodes.get("#app-update-progress").hidden, false);
  f.emit({ revision: 5, phase: "error", latestVersion: "1.4.0", forceUpdate: true, error: { message: "签名校验失败" } });
  assert.equal(f.business.disabled, true);
  assert.equal(f.panel.inert, true);
  assert.equal(f.nodes.get("#app-update-required").hidden, false);
  assert.equal(f.shown(), 0);
});
test("unknown download size and pricing failures have distinct safe explanations", async () => {
  const f = await fixture();
  assert.equal(f.status.updatePresentation({ phase: "downloading", downloadedBytes: 100 }).percent, null);
  assert.equal(f.status.updatePresentation({ phase: "error" }).failed, true);
  assert.match(f.status.pricingError("http_status_404"), /不存在/);
  assert.match(f.status.pricingError("fetch_timeout"), /超时/);
  assert.match(f.status.pricingError("parse_failed"), /保留本地缓存/);
  assert.doesNotMatch(f.status.pricingError("fetch_failed: https://secret.invalid"), /secret/);
});
