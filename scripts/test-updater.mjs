// node --experimental-vm-modules scripts/test-updater.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const source = await readFile(new URL("../src/app_update.js", import.meta.url), "utf8");
const presentation = await readFile(new URL("../src/update_status.js", import.meta.url), "utf8");
const notesSource = await readFile(new URL("../src/release_notes.js", import.meta.url), "utf8");
const historySource = await readFile(new URL("../src/update_history.js", import.meta.url), "utf8");
const markedSource = await readFile(new URL("../src/vendor/marked.esm.js", import.meta.url), "utf8");
const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
const defaults = { revision: 0, phase: "idle", currentVersion: "1.3.0", automatic: true,
  releasesUrl: "https://github.com/bgluminous/my-ai-assistant/releases" };
const settle = () => new Promise(resolve => setImmediate(resolve));
async function fixture(initial = defaults, responses = {}) {
  const { document, Event } = parseHTML(html);
  const nodes = new Map([...document.querySelectorAll("[id]")].map(node => [`#${node.id}`, node]));
  for (const node of nodes.values()) {
    node.click = async () => {
      if (!node.disabled) node.dispatchEvent(new Event("click", { bubbles: true, cancelable: true }));
      await settle();
    };
  }
  const selects = "[role='tab'][data-panel='about-panel']";
  const about = document.querySelector(selects);
  const business = document.querySelector("[role='tab'][data-panel='usage-panel']");
  const panel = document.querySelector("#usage-panel");
  nodes.set(selects, about);
  const events = new Map(); const calls = []; let shown = 0;
  const frames = new Map(); let frameId = 0;
  const context = vm.createContext({ console, URL, document,
    requestAnimationFrame: callback => { const id = ++frameId; frames.set(id, callback); return id; },
    cancelAnimationFrame: id => frames.delete(id),
    window: { __TAURI__: { core: {}, window: { getCurrentWindow: () => ({ show() { shown++; }, setFocus() { shown++; } }) } } },
  });
  const invoke = async (command, args) => {
    calls.push({ command, args });
    if (command in responses) return typeof responses[command] === "function" ? responses[command](args) : responses[command];
    if (command === "app_update_history") return ["1.4.0", "1.3.1", "1.3.0"].map(version => ({ version }));
    if (command === "app_update_changelog") return { version: args.version, changelog: `## ${args.version}\n\n- 历史修复` };
    return initial;
  };
  const exports = { el: (id) => { assert(nodes.has(id), `Unknown HTML element ${id}`); return nodes.get(id); }, invoke,
    listen: async (name, fn) => { events.set(name, fn); },
    fillStatus: (node, kind, text) => { node.textContent = text; node.hidden = false; },
    fmtDateMs: () => "date", toast: () => {}, copyText: async () => {} };
  const shared = new vm.SyntheticModule(Object.keys(exports), function () {
    for (const [k, v] of Object.entries(exports)) this.setExport(k, v);
  }, { context });
  const status = new vm.SourceTextModule(presentation, { context });
  const notes = new vm.SourceTextModule(notesSource, { context });
  const history = new vm.SourceTextModule(historySource, { context });
  const marked = new vm.SourceTextModule(markedSource, { context });
  const module = new vm.SourceTextModule(source, { context });
  const modules = { "./shared.js": shared, "./update_status.js": status, "./release_notes.js": notes,
    "./update_history.js": history, "./vendor/marked.esm.js": marked };
  await module.link(name => { assert(modules[name], `Unknown import ${name}`); return modules[name]; });
  await module.evaluate();
  await module.namespace.initAppUpdate();
  return { nodes, calls, business, panel, document, Event, frames, notes: notes.namespace, status: status.namespace, shown: () => shown,
    paint: () => { const pending = [...frames.values()]; frames.clear(); for (const frame of pending) frame(); },
    selectHistory: async version => {
      const select = nodes.get("#app-update-history-version");
      for (const option of select.querySelectorAll("option")) option.selected = false;
      select.querySelector(`option[value="${version}"]`).selected = true;
      select.dispatchEvent(new Event("change", { bubbles: true }));
      await settle();
    },
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
test("the installed version keeps its notes when no upgrade is available", async () => {
  const f = await fixture();
  assert.match(f.nodes.get("#app-update-current-notes").textContent, /检查更新后/);
  f.emit({ revision: 1, phase: "checking" });
  assert.match(f.nodes.get("#app-update-current-notes").textContent, /正在获取/);
  f.emit({ revision: 2, phase: "upToDate", latestVersion: null, currentChangelog: "修复启动异常" });
  assert.equal(f.nodes.get("#app-update-current-notes-title").textContent, "当前版本 1.3.0 · 更新日志");
  assert.equal(f.nodes.get("#app-update-current-notes").textContent, "修复启动异常");
  assert.equal(f.nodes.get("#app-update-new-notes").hidden, true);
  assert.equal(f.nodes.get("#app-update-install").hidden, true);
  assert.equal(f.nodes.get("#app-update-chip").hidden, true);
  assert.equal(f.nodes.get("#app-update-current-notes-status").hidden, true);
});
test("current and new release notes render Markdown without executing HTML", async () => {
  const f = await fixture();
  f.emit({ revision: 3, phase: "available", latestVersion: "1.4.0", canInstall: true,
    currentChangelog: "**当前版本说明**", changelog: "## 新增功能\n\n- `应用更新`\n  - 修复\n\n<script>untrusted</script>" });
  assert.equal(f.nodes.get("#app-update-new-notes").hidden, false);
  assert.equal(f.nodes.get("#app-update-notes-title").textContent, "新版本 1.4.0 · 更新日志");
  const notes = f.nodes.get("#app-update-notes");
  assert.equal(notes.tagName, "DIV");
  assert.equal(notes.querySelector("h2").textContent, "新增功能");
  assert.equal(notes.querySelector("ul li code").textContent, "应用更新");
  assert.equal(notes.querySelector("ul ul li").textContent, "修复");
  assert.equal(notes.querySelector("script"), null);
  assert.match(notes.textContent, /<script>untrusted<\/script>/);
  assert.equal(f.nodes.get("#app-update-current-notes").querySelector("strong").textContent, "当前版本说明");
  assert.equal(f.nodes.get("#app-update-current-notes").textContent, "当前版本说明");
  assert.equal(f.nodes.get("#app-update-install").hidden, false);
  f.emit({ revision: 2, phase: "upToDate", currentChangelog: "迟到的旧事件" });
  assert.equal(f.nodes.get("#app-update-current-notes").textContent, "当前版本说明");
  assert.equal(f.calls.length, 1, "rendering notes must not fetch release history");
});

test("bundled parser and license match the pinned dependency", async () => {
  assert.equal(markedSource, await readFile(new URL("../node_modules/marked/lib/marked.esm.js", import.meta.url), "utf8"));
  assert.equal(await readFile(new URL("../src/vendor/marked.LICENSE", import.meta.url), "utf8"),
    await readFile(new URL("../node_modules/marked/LICENSE", import.meta.url), "utf8"));
});

test("Markdown supports code, tables, quotes, tasks, entities, and preserves unchanged DOM", async () => {
  const f = await fixture();
  const notes = f.nodes.get("#app-update-notes");
  const markdown = "# 更新 &amp; 修复 😀\n\n> **加粗**、*强调*、~~删除~~\n\n3. 第三项\n4. 第四项\n\n- [x] 完成\n- [ ] 待办\n\n| 平台 | 版本 |\n| --- | ---: |\n| Windows | `1.5.0` |\n\n```html\n<script>&amp;</script>\n```\n\n第一行\n第二行";
  f.notes.renderReleaseNotes(notes, markdown);
  assert.equal(notes.querySelector("h1").textContent, "更新 & 修复 😀");
  assert.equal(notes.querySelector("blockquote strong").textContent, "加粗");
  assert.equal(notes.querySelector("em").textContent, "强调");
  assert.equal(notes.querySelector("del").textContent, "删除");
  assert.equal(notes.querySelector("ol").getAttribute("start"), "3");
  assert.match(notes.querySelector("ul").textContent, /☑ 完成☐ 待办/);
  assert.equal(notes.querySelector("table td code").textContent, "1.5.0");
  assert.equal(notes.querySelector("pre code").textContent, "<script>&amp;</script>");
  assert(notes.querySelector("br"));
  const heading = notes.firstChild;
  notes.scrollTop = 60;
  f.notes.renderReleaseNotes(notes, markdown);
  assert.equal(notes.firstChild, heading);
  assert.equal(notes.scrollTop, 60);
  f.notes.renderReleaseNotes(notes, "> ".repeat(70) + "嵌套内容");
  assert(notes.classList.contains("update-notes-plain"));
  assert.match(notes.textContent, /嵌套内容/);
});

test("untrusted Markdown cannot create active HTML or unsafe links", async () => {
  const f = await fixture();
  const notes = f.nodes.get("#app-update-notes");
  f.notes.renderReleaseNotes(notes, [
    "<script>alert(1)</script>", '<img src="https://tracker.invalid" onerror="alert(1)">',
    "<iframe srcdoc='<script>alert(1)</script>'></iframe>", '<svg onload="alert(1)"></svg>',
    '<form id="app-update-check"><input autofocus onfocus="alert(1)"></form>',
    "![图片](https://tracker.invalid/image.png)",
    "[脚本](<javascript:alert(1)>) [实体](<jav&#x61;script:alert(1)>)",
    "[文件](file:///C:/test.exe) [数据](data:text/html,bad) [应用](ms-settings:privacy)",
    "[相对](/local) [协议相对](//example.com) [凭据](https://user:pass@example.com)",
    '[安全 **网页**](https://example.com/?a=1&amp;b=2 "引号 &quot; onmouseover=&quot;bad")',
  ].join("\n\n"));
  assert.equal(notes.querySelectorAll("script,img,iframe,svg,form,input,[onerror],[onload],[id]").length, 0);
  const links = notes.querySelectorAll("a");
  assert.equal(links.length, 1);
  assert.equal(links[0].getAttribute("href"), "https://example.com/?a=1&b=2");
  assert.equal(links[0].getAttribute("rel"), "noopener noreferrer");
  assert.equal(links[0].getAttribute("target"), "_blank");
  assert.equal(links[0].getAttribute("onmouseover"), null);
  assert.equal(f.calls.length, 1, "rendering must never open or fetch a link");
  const event = new f.Event("click", { bubbles: true, cancelable: true });
  links[0].querySelector("strong").dispatchEvent(event);
  await settle();
  assert.equal(event.defaultPrevented, true);
  assert.equal(f.calls.at(-1).command, "app_update_open_link");
  assert.equal(f.calls.at(-1).args.url, "https://example.com/?a=1&b=2");
});

test("history is lazy, browsable, cached, and independent of the install candidate", async () => {
  const f = await fixture();
  f.emit({ revision: 1, phase: "available", latestVersion: "1.4.0", canInstall: true, changelog: "新版本" });
  assert.equal(f.calls.length, 1);
  await f.nodes.get("#app-update-history-toggle").click();
  assert.equal(f.nodes.get("#app-update-history").hidden, false);
  assert.equal(f.nodes.get("#app-update-history-toggle").getAttribute("aria-expanded"), "true");
  assert.equal(f.nodes.get("#app-update-history-version").querySelectorAll("option").length, 3);
  assert.equal(f.nodes.get("#app-update-history-notes").querySelector("h2").textContent, "1.4.0");
  await f.selectHistory("1.3.1");
  assert.equal(f.nodes.get("#app-update-history-notes").querySelector("h2").textContent, "1.3.1");
  await f.selectHistory("1.4.0");
  assert.equal(f.calls.filter(c => c.command === "app_update_changelog").length, 2);
  await f.nodes.get("#app-update-history-toggle").click();
  await f.nodes.get("#app-update-history-toggle").click();
  assert.equal(f.calls.filter(c => c.command === "app_update_history").length, 1);
  assert.equal(f.nodes.get("#app-update-notes").textContent, "新版本");
  assert.equal(f.nodes.get("#app-update-install").textContent, "更新到 1.4.0");
  f.emit({ revision: 2, phase: "installing" });
  assert.equal(f.nodes.get("#app-update-history-toggle").disabled, true);
  assert.equal(f.nodes.get("#app-update-history").inert, true);
});

test("history rejects late responses after a quick version switch", async () => {
  const pending = new Map();
  const f = await fixture(defaults, { app_update_changelog: ({ version }) => new Promise(resolve => pending.set(version, resolve)) });
  await f.nodes.get("#app-update-history-toggle").click();
  await f.selectHistory("1.3.1");
  pending.get("1.3.1")({ version: "1.3.1", changelog: "## 所选版本" });
  await settle();
  pending.get("1.4.0")({ version: "1.4.0", changelog: "错误的迟到内容" });
  await settle();
  assert.equal(f.nodes.get("#app-update-history-notes").querySelector("h2").textContent, "所选版本");
  assert.equal(f.nodes.get("#app-update-history-version").value, "1.3.1");
});

test("history failures are retryable, empty history is explicit, and update state is untouched", async () => {
  let listFails = true; let notesFail = true;
  const f = await fixture(defaults, {
    app_update_history: () => { if (listFails) throw new Error("网络故障"); return [{ version: "1.3.1" }]; },
    app_update_changelog: ({ version }) => { if (notesFail) throw new Error("日志暂不可用"); return { version, changelog: "" }; },
  });
  await f.nodes.get("#app-update-history-toggle").click();
  assert.match(f.nodes.get("#app-update-history-status").textContent, /列表获取失败.*网络故障/);
  assert.equal(f.nodes.get("#app-update-history-refresh").disabled, false);
  listFails = false;
  await f.nodes.get("#app-update-history-refresh").click();
  assert.match(f.nodes.get("#app-update-history-status").textContent, /读取 1.3.1.*失败/);
  assert.equal(f.nodes.get("#app-update-history-retry").hidden, false);
  notesFail = false;
  await f.nodes.get("#app-update-history-retry").click();
  assert.equal(f.nodes.get("#app-update-history-notes").textContent, "该版本未提供更新日志。");
  assert.doesNotMatch(f.nodes.get("#app-update-status").textContent, /失败|网络故障/);
  const empty = await fixture(defaults, { app_update_history: [] });
  await empty.nodes.get("#app-update-history-toggle").click();
  assert.match(empty.nodes.get("#app-update-history-status").textContent, /暂无已登记/);
  assert.equal(empty.calls.some(c => c.command === "app_update_changelog"), false);
});
test("missing or empty notes have explicit states without showing older releases", async () => {
  const f = await fixture();
  f.emit({ revision: 1, phase: "upToDate", currentVersion: "1.4.0", latestVersion: null,
    changelog: "更旧版本的日志", currentChangelogError: { code: "not_published" } });
  assert.equal(f.nodes.get("#app-update-new-notes").hidden, true);
  assert.equal(f.nodes.get("#app-update-current-notes-title").textContent, "当前版本 1.4.0 · 更新日志");
  assert.equal(f.nodes.get("#app-update-current-notes").textContent, "当前版本暂无已发布的更新日志。");
  f.emit({ revision: 2, phase: "available", latestVersion: "1.5.0", changelog: " \n", currentChangelog: "" });
  assert.equal(f.nodes.get("#app-update-notes").textContent, "该版本未提供更新日志。");
  assert.equal(f.nodes.get("#app-update-current-notes").textContent, "该版本未提供更新日志。");
});
test("current notes failures retain cached text without blocking the new version", async () => {
  const f = await fixture();
  const currentChangelogError = { code: "network", message: "无法连接更新源，请重试。" };
  f.emit({ revision: 1, phase: "available", latestVersion: "1.4.0", canInstall: true,
    changelog: "新版本说明", currentChangelog: "已读取的当前版本说明", currentChangelogError });
  assert.equal(f.nodes.get("#app-update-current-notes").textContent, "已读取的当前版本说明");
  assert.equal(f.nodes.get("#app-update-current-notes-status").hidden, false);
  assert.match(f.nodes.get("#app-update-current-notes-status").textContent, /获取失败.*已保留/);
  assert.equal(f.nodes.get("#app-update-install").hidden, false);
  assert.equal(f.nodes.get("#app-update-install").disabled, false);
  f.emit({ revision: 2, phase: "available", latestVersion: "1.4.0", currentChangelogError });
  assert.match(f.nodes.get("#app-update-current-notes").textContent, /获取失败.*请重试/);
  assert.equal(f.nodes.get("#app-update-current-notes-status").hidden, true);
});
test("progress, cancellation, stale events and required updates remain visible", async () => {
  const f = await fixture();
  f.emit({ revision: 4, phase: "downloading", downloadedBytes: 50, totalBytes: 100 });
  assert.equal(f.nodes.get("#app-update-progress").getAttribute("aria-valuenow"), "50");
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

test("download bursts paint once per frame without rewriting the update panel", async () => {
  const f = await fixture();
  const download = { phase: "downloading", latestVersion: "1.4.0", canInstall: true,
    currentChangelog: "当前版本说明", changelog: "## 更新说明", totalBytes: 100_000 };
  f.emit({ ...download, revision: 1, downloadedBytes: 25_000 });
  const selectors = ["#app-update-status", "#app-update-meta", "#app-update-check", "#app-update-install",
    "#app-update-notes-title", "#app-update-notes", "#app-update-current-notes-title", "#app-update-progress-text"];
  const contents = selectors.map(id => f.nodes.get(id).firstChild);
  const progress = f.nodes.get("#app-update-progress");
  for (let i = 1; i <= 100; i++) f.emit({ ...download, revision: i + 1, downloadedBytes: 25_000 + i });
  assert.equal(f.frames.size, 1);
  assert.equal(progress.firstElementChild.style.transform, "scaleX(0.25)");
  f.paint();
  assert.equal(progress.firstElementChild.style.transform, "scaleX(0.251)", "sub-percent progress must not be rounded away");
  assert.equal(progress.getAttribute("aria-valuenow"), "25");
  assert.equal(progress.dataset.animate, "true");
  for (const [i, id] of selectors.entries()) assert.equal(f.nodes.get(id).firstChild, contents[i], id);
  assert.deepEqual(f.calls.map(c => c.command), ["app_update_status"], "progress must never repeat network operations");
  f.emit({ ...download, revision: 50, downloadedBytes: 10_000 });
  assert.equal(f.frames.size, 0, "a stale event must not schedule another frame");
});

test("non-progress changes during download render immediately and cancel pending progress", async () => {
  const f = await fixture();
  const download = { phase: "downloading", totalBytes: 100, changelog: "旧日志" };
  f.emit({ ...download, revision: 1, downloadedBytes: 10 });
  f.emit({ ...download, revision: 2, downloadedBytes: 20 });
  assert.equal(f.frames.size, 1);
  f.emit({ ...download, revision: 3, downloadedBytes: 30, forceUpdate: true, changelog: "新日志" });
  assert.equal(f.frames.size, 0);
  assert.equal(f.nodes.get("#app-update-notes").textContent, "新日志");
  assert.equal(f.nodes.get("#app-update-progress").getAttribute("aria-valuenow"), "30");
  assert.equal(f.business.disabled, true);
  assert.equal(f.panel.inert, true);
});

test("terminal states flush pending progress and retry starts at its own position", async () => {
  for (const phase of ["cancelled", "error", "waiting", "installing"]) {
    const f = await fixture();
    f.emit({ revision: 1, phase: "downloading", totalBytes: 100, downloadedBytes: 40 });
    f.emit({ revision: 2, phase: "downloading", totalBytes: 100, downloadedBytes: 80 });
    f.emit({ revision: 3, phase, totalBytes: 100, downloadedBytes: 90 });
    assert.equal(f.frames.size, 0, phase);
    f.paint();
    const progress = f.nodes.get("#app-update-progress");
    assert.equal(progress.hidden, ["cancelled", "error"].includes(phase), phase);
    assert.equal(progress.dataset.animate, "false", phase);
    assert.equal(f.panel.inert, ["waiting", "installing"].includes(phase), phase);
    f.emit({ revision: 2, phase: "downloading", totalBytes: 100, downloadedBytes: 80 });
    assert.equal(f.frames.size, 0);
    f.emit({ revision: 4, phase: "downloading", totalBytes: 100, downloadedBytes: 0 });
    assert.equal(progress.hidden, false);
    assert.equal(progress.firstElementChild.style.transform, "scaleX(0)");
    assert.equal(progress.dataset.animate, "false", "retry must not animate backwards from the previous download");
  }
});

test("unknown sizes stay indeterminate and corrected totals never animate backwards", async () => {
  const f = await fixture();
  const progress = f.nodes.get("#app-update-progress");
  f.emit({ revision: 1, phase: "downloading", downloadedBytes: 10 });
  assert(progress.classList.contains("is-indeterminate"));
  assert.equal(progress.hasAttribute("aria-valuenow"), false);
  f.emit({ revision: 2, phase: "downloading", downloadedBytes: 20, totalBytes: 100 });
  f.paint();
  assert.equal(progress.classList.contains("is-indeterminate"), false);
  assert.equal(progress.firstElementChild.style.transform, "scaleX(0.2)");
  assert.equal(progress.dataset.animate, "false");
  f.emit({ revision: 3, phase: "downloading", downloadedBytes: 21, totalBytes: 100 });
  f.paint();
  assert.equal(progress.dataset.animate, "true");
  f.emit({ revision: 4, phase: "downloading", downloadedBytes: 22, totalBytes: 200 });
  f.paint();
  assert.equal(progress.firstElementChild.style.transform, "scaleX(0.11)");
  assert.equal(progress.dataset.animate, "false");
  f.emit({ revision: 5, phase: "downloading", downloadedBytes: 300, totalBytes: 200 });
  f.paint();
  assert.equal(progress.firstElementChild.style.transform, "scaleX(1)");
  assert.equal(progress.getAttribute("aria-valuenow"), "100");
});

test("unchanged snapshots still restore the automatic-update checkbox after saving", async () => {
  for (const fails of [false, true]) {
    const f = await fixture(defaults, { app_update_settings_set: () => {
      if (fails) throw new Error("保存失败");
      return defaults;
    } });
    const checkbox = f.nodes.get("#app-update-auto");
    checkbox.checked = false;
    checkbox.dispatchEvent(new f.Event("change", { bubbles: true }));
    await settle();
    assert.equal(checkbox.checked, true, "restore the authoritative setting even when the panel snapshot is unchanged");
    if (fails) assert.equal(f.nodes.get("#app-update-status").textContent, "保存失败");
  }
});
