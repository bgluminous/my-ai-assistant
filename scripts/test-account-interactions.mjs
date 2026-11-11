// node --experimental-vm-modules --test scripts/test-account-interactions.mjs
// 真实 DOM 与生产交互，替换桌面 IPC、布局坐标和动画时钟，不访问真实账户。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const orderSource = await readFile(new URL("../src/account_order.js", import.meta.url), "utf8");
const oauthSource = await readFile(new URL("../src/opencode_ui.js", import.meta.url), "utf8");
const accountSource = await readFile(new URL("../src/accounts.js", import.meta.url), "utf8")
  + "\nexport { accountRow, rowErrors, openModal, onSave, applyView, switchModal, closeModal };";
const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

async function fixture(source, { stored, invoke = async () => {}, mocks = {} } = {}) {
  const { document, window: dom } = parseHTML(html);
  Object.defineProperty(document, "activeElement", { value: document.body, writable: true });
  dom.HTMLElement.prototype.select = function () {};
  dom.HTMLElement.prototype.focus = function () { document.activeElement = this; };
  dom.HTMLElement.prototype.showModal = function () { this.open = true; };
  dom.HTMLElement.prototype.close = function () { this.open = false; };
  Object.defineProperty(dom.HTMLSelectElement.prototype, "value", {
    configurable: true,
    get() { return this.querySelector("option[selected]")?.value ?? this.querySelector("option")?.value ?? ""; },
    set(value) { for (const option of this.querySelectorAll("option")) option.toggleAttribute("selected", option.value === String(value)); },
  });
  const create = document.createElement.bind(document);
  document.createElement = tag => {
    const element = create(tag);
    if (tag === "select") {
      let selected;
      Object.defineProperty(element, "value", {
        get: () => selected ?? element.firstElementChild?.value ?? "",
        set: value => { selected = value; },
      });
    }
    return element;
  };
  const handlers = new Map(), values = new Map(stored || []), timers = new Map();
  let nextTimer = 0, now = 10000;
  const deadlines = new Map();
  class Clock extends Date { static now() { return now; } }
  const window = {
    innerHeight: 800, scrollBy() {}, matchMedia: () => ({ matches: false, addEventListener() {} }),
    addEventListener(type, fn) { const list = handlers.get(type) || []; list.push(fn); handlers.set(type, list); },
    dispatchEvent(event) { for (const fn of handlers.get(event.type) || []) fn(event); },
    requestAnimationFrame: () => 1, cancelAnimationFrame() {},
  };
  const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  const context = vm.createContext({ document, window, localStorage: storage, CustomEvent: dom.CustomEvent,
    Date: Clock, CSS: { escape: value => value },
    setInterval: () => ++nextTimer, clearInterval() {},
    setTimeout: (callback, delay = 0) => { const id = ++nextTimer; timers.set(id, callback); deadlines.set(id, now + delay); return id; },
    clearTimeout: id => { timers.delete(id); deadlines.delete(id); } });
  const calls = [];
  window.__TAURI__ = { core: { invoke: async (cmd, args) => { calls.push({ cmd, args }); return invoke(cmd, args); } } };
  const modules = new Map();
  for (const name of ["shared", "modal", "account_format", "account_order", "account_refresh", "opencode_ui"]) {
    modules.set(`./${name}.js`, new vm.SourceTextModule(
      await readFile(new URL(`../src/${name}.js`, import.meta.url), "utf8"), { context }));
  }
  for (const [, names, path] of source.matchAll(/import\s*\{([^}]+)\}\s*from\s*"([^"]+)"/g)) {
    if (modules.has(path)) continue;
    const exports = names.split(",").map(s => s.trim()).filter(Boolean);
    modules.set(path, new vm.SyntheticModule(exports, function () {
      for (const name of exports) this.setExport(name, mocks[name] || (() => {}));
    }, { context }));
  }
  const entry = new vm.SourceTextModule(source, { context });
  await entry.link(path => modules.get(path)); await entry.evaluate();
  const panel = document.querySelector("#accounts-panel");
  for (const card of panel.children) card.getBoundingClientRect = () => ({
    top: [...panel.children].indexOf(card) * 100, height: 100,
  });
  const event = (node, type, properties) => {
    const event = new dom.Event(type, { bubbles: true, cancelable: true });
    Object.assign(event, properties);
    if (node === window) window.dispatchEvent(event); else node.dispatchEvent(event);
    return event;
  };
  return { api: entry.namespace, document, window, values, storage, calls, timers, event,
    advance(ms) {
      now += ms;
      for (const [id, deadline] of [...deadlines]) if (deadline <= now && timers.has(id)) {
        const callback = timers.get(id); timers.delete(id); deadlines.delete(id); callback();
      }
    },
    backdrop(root) { const target = root.querySelector(".modal-backdrop") || root; event(target, "pointerdown", { clientX: -1, clientY: -1 }); event(target, "click", { clientX: -1, clientY: -1 }); },
    order: () => [...panel.children].map(card => card.dataset.accountKind),
    handle: kind => document.querySelector(`[data-account-kind="${kind}"] .account-card-handle`),
    button: text => [...document.querySelectorAll("dialog button")].find(button => button.textContent === text),
  };
}

test("card order normalizes saved preferences and keyboard reordering survives reopening", async () => {
  const f = await fixture(orderSource, { stored: [["accountGroupOrder", '["opencode","opencode","bad","claude"]']] });
  f.api.initAccountCardOrder(); f.api.initAccountCardOrder();
  assert.deepEqual(f.order(), ["opencode", "claude", "cursor", "codex"]);
  assert.equal(f.document.querySelectorAll(".account-card-handle").length, 4);
  f.event(f.handle("claude"), "keydown", { key: "ArrowUp" });
  assert.deepEqual(f.order(), ["claude", "opencode", "cursor", "codex"]);
  const reopened = await fixture(orderSource, { stored: [...f.values] });
  reopened.api.initAccountCardOrder(); assert.deepEqual(reopened.order(), f.order());
});

test("pointer drag commits only on release; cancel restores the previous order", async () => {
  const f = await fixture(orderSource); f.api.initAccountCardOrder();
  f.event(f.handle("opencode"), "pointerdown", { button: 0, pointerId: 7, clientY: 350 });
  f.event(f.window, "pointermove", { pointerId: 7, clientY: 10 });
  assert.equal(f.order()[0], "opencode"); assert.equal(f.values.size, 0);
  f.event(f.window, "pointercancel", { pointerId: 7 });
  assert.deepEqual(f.order(), ["cursor", "codex", "claude", "opencode"]);
  f.event(f.handle("opencode"), "pointerdown", { button: 0, pointerId: 8, clientY: 350 });
  f.event(f.window, "pointermove", { pointerId: 8, clientY: 10 });
  f.event(f.window, "pointerup", { pointerId: 8 });
  assert.equal(JSON.parse(f.values.get("accountGroupOrder"))[0], "opencode");
  assert.equal(f.document.body.classList.contains("account-cards-dragging"), false);
});

test("restoring order updates the cards; storage failure does not pretend to save", async () => {
  const f = await fixture(orderSource); f.api.initAccountCardOrder();
  f.api.setAccountGroupOrder(["claude"]);
  assert.deepEqual(f.order(), ["claude", "cursor", "codex", "opencode"]);
  f.storage.setItem = () => { throw new Error("full"); };
  f.event(f.handle("cursor"), "keydown", { key: "ArrowUp" });
  assert.deepEqual(f.order(), ["claude", "cursor", "codex", "opencode"]);
});

test("OpenCode add stays local until a method is chosen and imports only the selected workspace with its note", async () => {
  let completed;
  const f = await fixture(oauthSource, { invoke: async cmd => {
    if (cmd === "opencode_local_accounts") return [
      { workspaceId: "wrk_a", name: "Personal", email: "a@example.invalid" },
      { workspaceId: "wrk_b", name: "Work", email: "b@example.invalid" },
    ];
    if (cmd === "accounts_import_local") return { imported: [{ id: "account-b" }] };
    throw new Error(`Unexpected ${cmd}`);
  } });
  f.api.addOpenCode(ids => { completed = [...ids]; });
  assert.equal(f.calls.length, 0);
  assert.equal(f.document.querySelector("dialog .status").hidden, true);
  assert.ok(f.document.querySelector('dialog input[type="text"]'));
  assert.ok(f.button("从本机导入")); assert.ok(f.button("新增 OAuth 授权"));
  f.document.querySelector("dialog input").value = "工作备注";
  await f.button("从本机导入").onclick();
  assert.equal(f.calls.length, 1);
  assert.equal(f.document.querySelector("dialog .status").hidden, true);
  f.document.querySelector("dialog select").value = "wrk_b";
  await f.button("导入账户").onclick();
  assert.equal(JSON.stringify(f.calls[1].args), '{"kind":"opencode","workspaceId":"wrk_b","note":"工作备注"}');
  assert.deepEqual(completed, ["account-b"]);
  assert.equal(f.document.querySelectorAll("dialog").length, 0);
});

test("OAuth add carries the draft note and returns to the picker when cancelled", async () => {
  const f = await fixture(oauthSource, { invoke: async cmd => {
    if (cmd === "opencode_oauth_start") return { id: "attempt", url: "https://opencode.ai/console/device", code: "TEST", opened: true };
    if (cmd === "opencode_oauth_cancel") return null;
    throw new Error(`Unexpected ${cmd}`);
  } });
  f.api.addOpenCode(); f.document.querySelector("dialog input").value = "我的账户";
  f.button("新增 OAuth 授权").onclick(); await tick();
  assert.equal(f.calls[0].args.note, "我的账户");
  assert.equal(f.calls[0].cmd, "opencode_oauth_start");
  const dialogs = f.document.querySelectorAll("dialog");
  assert.equal(dialogs[1].querySelector(".status").hidden, false);
  assert.ok(dialogs[1].querySelector('input[type="text"]'));
  f.event(dialogs[1].querySelector(".modal-foot button"), "click"); await tick();
  assert.equal(f.calls[1].cmd, "opencode_oauth_cancel");
  assert.equal(f.document.querySelectorAll("dialog").length, 1);
  assert.equal(f.document.querySelector("dialog input").value, "我的账户");
});

test("failed OpenCode cancellation retries the same attempt and ignores an in-flight poll", async () => {
  for (const native of [false, true]) {
    const poll = deferred(); let cancels = 0, completed = 0;
    const f = await fixture(oauthSource, { invoke: (cmd, args) => {
      if (cmd === (native ? "opencode_switch_local" : "opencode_oauth_start")) {
        return { id: "attempt_retry", url: "https://example.invalid/device", interval: 3 };
      }
      const cancel = native ? cmd === "opencode_client_oauth_poll" && args.cancel : cmd === "opencode_oauth_cancel";
      if (cancel) {
        assert.equal(args.id, "attempt_retry");
        if (++cancels === 1) throw new Error("restore failed");
        return { state: "cancelled" };
      }
      return poll.promise;
    } });
    if (native) await f.api.switchOpenCode("account", () => completed++);
    else await f.api.authorizeOpenCode(null, () => completed++);
    const root = f.document.querySelector("dialog"), cancel = root.querySelector(".modal-foot button");
    const polling = [...f.timers.values()][0]();
    f.event(cancel, "click"); await tick();
    assert.equal(cancels, 1); assert.equal(root.isConnected, true);
    assert.match(root.querySelector(".status").textContent, /restore failed/);
    assert.equal(cancel.disabled, false);
    poll.resolve({ state: "complete" }); await polling;
    assert.equal(completed, 0); assert.equal(root.isConnected, true);
    f.event(cancel, "click"); await tick();
    assert.equal(cancels, 2); assert.equal(root.isConnected, false);
    assert.equal(completed, 0);
    assert.equal(f.timers.size, 0);
  }
});

test("backup partial restore reports applied data, retains preference updates, and allows retry", async () => {
  const source = await readFile(new URL("../src/settings.js", import.meta.url), "utf8") + "\nexport { doImportApply };";
  let attempts = 0; const refreshed = [];
  const f = await fixture(source, { mocks: { refreshAccounts: ids => refreshed.push(...ids) }, invoke: cmd => {
    if (cmd === "backup_import_apply") return {
      imported: ++attempts === 1 ? 1 : 0, importedIds: attempts === 1 ? ["imported"] : [],
      skippedExists: attempts === 1 ? 0 : 1, skippedInvalid: 0, usageRestored: attempts === 1 ? 2 : 0,
      opencodeEventsRestored: attempts === 1 ? 0 : 3,
      opencodeUsageError: attempts === 1 ? "opencode_archive_write_failed" : null,
      uiPrefs: { theme: "light" },
    };
    if (cmd === "opencode_client_get") return { exePath: "restored-cli", dbPath: "restored-db" };
    return {};
  } });
  f.api.initSettings();
  f.event(f.document.querySelector("#settings-btn"), "click"); await tick();
  await f.api.doImportApply("fixture-backup.json", null); await tick();
  const status = f.document.querySelector("#backup-status");
  assert.equal(status.classList.contains("warn"), true);
  assert.match(status.textContent, /部分恢复完成.*新增 1 个账号.*恢复 2 个已删除账户/);
  assert.match(status.textContent, /账户和设置已应用.*OpenCode 归档未恢复.*原有归档未改变.*重试/);
  assert.equal(f.document.documentElement.dataset.theme, "light");
  assert.deepEqual(refreshed, ["imported"]);
  assert.equal(f.document.querySelector("#opencode-exe-path").value, "restored-cli");
  assert.equal(f.document.querySelector("#opencode-db-path").value, "restored-db");
  assert.equal(f.document.querySelector("#backup-import").disabled, false);
  await f.api.doImportApply("fixture-backup.json", null); await tick();
  assert.equal(status.classList.contains("ok"), true);
  assert.match(status.textContent, /导入完成.*跳过 1 个已存在.*恢复 3 条 OpenCode/);
  assert.doesNotMatch(status.textContent, /未恢复/);
});

test("backup blocked by native authorization explains cancellation and applies no UI preferences", async () => {
  const source = await readFile(new URL("../src/settings.js", import.meta.url), "utf8") + "\nexport { doImportApply };";
  let refreshes = 0;
  const f = await fixture(source, { mocks: { refreshAccounts: () => refreshes++ }, invoke: cmd => {
    if (cmd === "backup_import_apply") throw "opencode_client_auth_pending";
    return {};
  } });
  f.api.initSettings(); f.event(f.document.querySelector("#settings-btn"), "click"); await tick();
  const beforeCalls = f.calls.length, theme = f.document.documentElement.dataset.theme;
  await f.api.doImportApply("fixture-backup.json", null); await tick();
  const status = f.document.querySelector("#backup-status");
  assert.equal(status.classList.contains("bad"), true);
  assert.match(status.textContent, /导入失败.*已有客户端授权正在进行.*先完成或取消/);
  assert.doesNotMatch(status.textContent, /已应用|恢复完成/);
  assert.equal(f.document.documentElement.dataset.theme, theme);
  assert.deepEqual(f.calls.slice(beforeCalls).map(call => call.cmd), ["backup_import_apply"]);
  assert.equal(refreshes, 0);
  assert.equal(f.document.querySelector("#backup-import").disabled, false);
});

test("editing OpenCode notes retries a failed save without losing the draft", async () => {
  let attempts = 0, completed = 0;
  const f = await fixture(oauthSource, { invoke: async cmd => {
    assert.equal(cmd, "accounts_update");
    if (++attempts === 1) throw new Error("write failed");
    return {};
  } });
  f.api.editOpenCode({ id: "a", token: "wrk_a", note: "old", opencodeAuthorized: true, refreshIntervalMinutes: 15 }, () => completed++);
  assert.equal(f.document.querySelector("dialog select").value, "15");
  f.document.querySelector("dialog select").value = "5";
  assert.equal(f.document.querySelector("dialog .status").hidden, true);
  assert.ok(f.document.querySelector('dialog input[type="text"]'));
  f.document.querySelector("dialog input").value = "new";
  await f.button("保存").onclick();
  assert.equal(f.document.querySelector("dialog input").value, "new");
  assert.equal(f.document.querySelector("dialog select").value, "5");
  assert.equal(f.document.querySelector("dialog .status").hidden, false);
  assert.match(f.document.querySelector("dialog .status").textContent, /write failed/);
  assert.equal(f.button("保存").disabled, false);
  await f.button("保存").onclick();
  assert.equal(f.calls[1].args.note, "new"); assert.equal(completed, 1);
  assert.equal(f.calls[1].args.refreshIntervalMinutes, 5);
});

test("saving an automatic OpenCode note keeps the email fallback automatic", async () => {
  const f = await fixture(oauthSource);
  f.api.editOpenCode({ id: "a", token: "wrk_a", note: "a@example.invalid", noteAuto: true });
  assert.equal(f.document.querySelector("dialog input").value, "");
  await f.button("保存").onclick();
  assert.equal(f.calls[0].args.note, "");
  assert.equal(f.calls[0].args.refreshIntervalMinutes, null);
});

test("account editors save inherited, disabled and custom refresh intervals without losing failures", async () => {
  for (const kind of ["cursor", "codex", "claude"]) {
    let current = { id: kind, kind, note: "fixture", token: "fixture", refreshIntervalMinutes: 15 };
    let fail = false;
    const f = await fixture(accountSource, { invoke: async (cmd, args) => {
      assert.equal(cmd, "accounts_update");
      if (fail) throw new Error("save failed");
      current = { ...current, refreshIntervalMinutes: args.refreshIntervalMinutes };
      return { accounts: [current], intervalMinutes: 5 };
    } });
    f.api.applyView({ accounts: [current], intervalMinutes: 5 });
    for (const value of ["0", "5", ""]) {
      f.api.openModal(current);
      const select = f.document.querySelector("#account-refresh-interval");
      assert.equal(select.value, current.refreshIntervalMinutes == null ? "" : String(current.refreshIntervalMinutes));
      select.value = value;
      fail = true; await f.api.onSave();
      assert.equal(select.value, value);
      assert.equal(f.document.querySelector("#account-modal").hidden, false);
      fail = false; await f.api.onSave();
      assert.equal(f.calls.at(-1).args.refreshIntervalMinutes, value === "" ? null : Number(value));
      assert.equal(f.document.querySelector("#account-modal").hidden, true);
    }
    f.api.openModal(null, kind);
    assert.equal(f.document.querySelector("#account-interval-field").hidden, true);
  }
});

test("all account kinds show failures only in the tooltip and keep their cached row content", async () => {
  const f = await fixture(accountSource);
  for (const kind of ["cursor", "codex", "claude", "opencode"]) {
    const account = { id: kind, kind, note: "a@example.invalid", token: "wrk_a", opencodeAuthorized: true,
      status: { alive: true, balanceUsd: 0, windows: [{ label: "每周", usedPercent: 20 }],
        plan: kind === "cursor" ? { autoPercentUsed: 20 } : "go" } };
    const healthy = f.api.accountRow(account);
    f.api.rowErrors.set(kind, "network temporarily unavailable");
    const failed = f.api.accountRow({ ...account, status: { ...account.status, refreshError: "credential renewal failed" } });
    assert.equal(failed.textContent, healthy.textContent);
    assert.equal(failed.querySelectorAll(".account-issue").length, 1);
    const icon = failed.querySelector(".account-issue");
    assert.match(icon.title, /network temporarily unavailable/);
    assert.match(icon.title, /credential renewal failed/);
    assert.equal(icon.getAttribute("aria-label"), icon.title);
    assert.equal(healthy.querySelector(".account-issue"), null);
  }
});

test("OpenCode partial reads explain each failing field without adding summary or balance lines", async () => {
  const f = await fixture(accountSource);
  const row = f.api.accountRow({ id: "a", kind: "opencode", note: "a@example.invalid", token: "wrk_a", opencodeAuthorized: true,
    status: { alive: true, balanceUsd: 0, stale: true, workspaceUsageAllowed: false,
      windows: [{ label: "5 小时", usedPercent: 0 }], fields: {
        quota: { state: "unavailable", error: "opencode_network_error", at: Date.now() - 60000 },
        balance: { state: "forbidden", error: "opencode_http_403" },
      } } });
  const icon = row.querySelector(".account-issue");
  assert.match(icon.title, /额度：/); assert.match(icon.title, /余额：/);
  assert.match(icon.title, /缓存/); assert.match(icon.title, /管理员/);
  assert.equal(row.querySelectorAll(".summary-meta, .ondemand-limit, .account-error").length, 0);
  assert.equal(row.children[4].textContent, "$0.00");
});

test("credential expiry sits below state and alongside renewal instead of inside the primary label", async () => {
  const f = await fixture(accountSource);
  const exp = Date.now() + 7 * 86400_000;
  for (const kind of ["codex", "claude", "opencode"]) {
    const row = f.api.accountRow({ id: kind, kind, note: "a@example.invalid", token: "wrk_a", refreshToken: "fixture",
      opencodeAuthorized: true, status: { alive: true, exp: exp / 1000, tokenExpiresAtMs: exp } });
    assert.equal(row.querySelector(".account-state").textContent, "有效");
    assert.match(row.querySelector(".state-tag-line .state-expiry").textContent, /d$/);
    assert.match(row.querySelector(".state-expiry").title, /登录凭据有效期/);
    assert.equal(row.querySelector(".state-tag-line .renewable").textContent, "可续期");
  }
});

test("all account editors confirm Escape and backdrop separately and restore focus after closing", async () => {
  for (const kind of ["cursor", "codex", "claude", "opencode"]) {
    const f = await fixture(kind === "opencode" ? oauthSource : accountSource);
    const trigger = f.document.querySelector(`#accounts-add-${kind}`);
    const open = () => {
      trigger.focus();
      if (kind === "opencode") f.api.editOpenCode({ id: "a", token: "wrk_a", note: "draft" });
      else f.api.openModal({ id: "a", kind, token: "token", note: "draft" });
      return f.document.querySelector(kind === "opencode" ? "dialog" : "#account-modal");
    };
    const root = open(), hint = root.querySelector(".modal-close-hint");
    f.backdrop(root);
    assert.equal(hint.hidden, false); assert.equal(hint.textContent, "再次点击关闭");
    const escape = f.event(root, "keydown", { key: "Escape" });
    assert.equal(escape.defaultPrevented, true);
    assert.equal(hint.textContent, "再次按 Esc 关闭");
    assert.equal(root.hidden, false);
    f.event(root, "keydown", { key: "Escape" });
    await tick();
    assert.ok(root.hidden || !root.isConnected);
    assert.equal(f.document.activeElement, trigger);
    const reopened = open();
    assert.equal(reopened.querySelector(".modal-close-hint").hidden, true);
    f.event(reopened, "keydown", { key: "Escape" });
    assert.equal(reopened.hidden, false, "a new session starts with no confirmation");
    f.backdrop(reopened); assert.equal(reopened.hidden, false);
    assert.equal(reopened.querySelector(".modal-close-hint").textContent, "再次点击关闭");
    f.backdrop(reopened); await tick();
    assert.ok(reopened.hidden || !reopened.isConnected);
    assert.equal(f.document.activeElement, trigger);
    const explicit = open();
    f.event(explicit.querySelector("[data-close]:not(.modal-backdrop)"), "click"); await tick();
    assert.ok(explicit.hidden || !explicit.isConnected);
    assert.equal(f.document.activeElement, trigger);
  }
});

test("Escape ignores held-key repeats without extending its two-second confirmation window", async () => {
  for (const kind of ["cursor", "opencode"]) {
    const f = await fixture(kind === "opencode" ? oauthSource : accountSource);
    if (kind === "opencode") f.api.editOpenCode({ id: "a", token: "wrk_a", note: "draft" });
    else f.api.openModal(null, kind);
    const root = f.document.querySelector(kind === "opencode" ? "dialog" : "#account-modal");
    const hint = root.querySelector(".modal-close-hint");
    f.event(root, "keydown", { key: "Escape" });
    f.advance(1999);
    f.event(root, "keydown", { key: "Escape", repeat: true });
    assert.equal(root.hidden, false);
    f.advance(1);
    assert.equal(hint.hidden, true, "repeats must not extend the original timeout");
    f.event(root, "keydown", { key: "Escape", repeat: true });
    assert.equal(hint.hidden, true, "repeats must not start a new confirmation");
    f.event(root, "keydown", { key: "Escape" });
    assert.equal(root.hidden, false);
    assert.equal(hint.textContent, "再次按 Esc 关闭");
    f.advance(1999);
    f.event(root, "keydown", { key: "Escape" }); await tick();
    assert.ok(root.hidden || !root.isConnected);
  }
});

test("close confirmation expires and resets on form interaction, typing and pointer cancellation", async () => {
  const f = await fixture(accountSource);
  f.api.openModal(null, "cursor");
  const root = f.document.querySelector("#account-modal"), hint = root.querySelector(".modal-close-hint");
  const input = f.document.querySelector("#account-note"), backdrop = root.querySelector(".modal-backdrop");
  for (const close of [() => f.backdrop(root), () => f.event(root, "keydown", { key: "Escape" })]) {
    close(); f.advance(2000);
    assert.equal(hint.hidden, true);
    for (const type of ["pointerdown", "click", "input", "change", "keydown", "pointercancel"]) {
      close();
      f.event(input, type, { key: "a" });
      assert.equal(hint.hidden, true, type);
      close(); assert.equal(root.hidden, false, type);
      f.advance(2000);
    }
  }
  f.event(input, "pointerdown"); f.event(backdrop, "click");
  assert.equal(hint.hidden, true, "dragging out of a field is not a backdrop click");
  f.event(backdrop, "pointerdown"); f.event(backdrop, "pointercancel"); f.event(backdrop, "click");
  assert.equal(hint.hidden, true);
  f.event(backdrop, "pointerdown", { pointerType: "touch" }); f.event(backdrop, "click");
  f.advance(1999); f.backdrop(root); assert.equal(root.hidden, true);
});

test("nested confirmations cancel only the top layer and clear the underlying editor's close timer", async () => {
  const f = await fixture(accountSource);
  f.api.openModal({ id: "a", kind: "cursor", note: "draft", token: "token" });
  const root = f.document.querySelector("#account-modal"), trigger = f.document.querySelector("#account-clear-usage");
  trigger.focus(); f.event(root, "keydown", { key: "Escape" });
  const pending = f.api.confirmDialog({ title: "确认", body: "确认内容", confirmText: "确认" });
  const confirm = f.document.querySelector("#switch-modal");
  assert.equal(root.hasAttribute("inert"), true);
  assert.equal(root.querySelector(".modal-close-hint").hidden, true);
  f.backdrop(root); assert.equal(root.hidden, false);
  f.backdrop(confirm); assert.equal(await pending, false);
  assert.equal(confirm.hidden, true); assert.equal(root.hidden, false);
  assert.equal(f.document.activeElement, trigger);
  assert.equal(root.hasAttribute("inert"), false);
  f.backdrop(root); assert.equal(root.hidden, false);
  const again = f.api.confirmDialog({ title: "确认", body: "内容" });
  f.event(confirm, "keydown", { key: "Escape" });
  assert.equal(await again, false); assert.equal(root.hidden, false);
  assert.equal(root.querySelector(".modal-close-hint").hidden, true);
  f.event(root, "keydown", { key: "Escape", repeat: true });
  assert.equal(root.querySelector(".modal-close-hint").hidden, true);
  f.event(root, "keydown", { key: "Escape" });
  assert.equal(root.hidden, false);
  f.event(root, "keydown", { key: "Escape" });
  assert.equal(root.hidden, true);
  assert.equal(f.document.querySelectorAll("[inert]").length, 0);
});

test("account saves block every close path, deduplicate submissions and keep failed drafts editable", async () => {
  const pending = deferred();
  const f = await fixture(accountSource, { invoke: cmd => {
    if (cmd === "accounts_update") return pending.promise;
    return {};
  } });
  f.api.openModal({ id: "a", kind: "cursor", token: "token", note: "draft" });
  const root = f.document.querySelector("#account-modal"), hint = root.querySelector(".modal-close-hint");
  const saving = f.api.onSave();
  await f.api.onSave(); assert.equal(f.calls.length, 1);
  for (const close of [() => f.backdrop(root), () => f.event(root, "keydown", { key: "Escape" }),
    () => f.event(f.document.querySelector("#account-close"), "click")]) {
    close(); close(); assert.equal(root.hidden, false);
    assert.equal(hint.hidden, false); assert.equal(hint.textContent, "正在保存，请稍候。");
  }
  pending.reject(new Error("write failed")); await saving;
  assert.equal(root.hidden, false);
  assert.equal(f.document.querySelector("#account-note").value, "draft");
  assert.equal(f.document.querySelector("#account-note").disabled, false);
  assert.equal(f.document.querySelector("#account-save").disabled, false);
  assert.match(f.document.querySelector("#account-modal-status").textContent, /write failed/);
  assert.equal(hint.hidden, true);
  f.backdrop(root); f.backdrop(root); assert.equal(root.hidden, true);
});

test("a completed old save cannot close or unlock a newly opened editor", async () => {
  const pending = [deferred(), deferred()]; let count = 0;
  const f = await fixture(accountSource, { invoke: () => pending[count++].promise });
  const account = { id: "a", kind: "cursor", token: "token", note: "first" };
  f.api.openModal(account); const first = f.api.onSave();
  f.api.closeModal();
  f.api.openModal({ ...account, note: "second" }); const second = f.api.onSave();
  pending[0].resolve({ accounts: [], refreshIntervalMinutes: 0 }); await first;
  assert.equal(f.document.querySelector("#account-modal").hidden, false);
  assert.equal(f.document.querySelector("#account-note").value, "second");
  assert.equal(f.document.querySelector("#account-save").disabled, true);
  pending[1].resolve({ accounts: [], refreshIntervalMinutes: 0 }); await second;
  assert.equal(f.document.querySelector("#account-modal").hidden, true);
});

test("deleted-account editing keeps failed saves open and uses the shared close protection", async () => {
  const f = await fixture(accountSource);
  let attempts = 0;
  const editing = f.api.editDeletedAccountDialog({ label: "归档", note: "old", save: async value => {
    assert.equal(value.note, "new");
    if (++attempts === 1) throw new Error("write failed");
  } });
  const root = f.document.querySelector("#switch-modal"), input = f.document.querySelector("#switch-modal-input");
  input.value = "new";
  f.event(root, "keydown", { key: "Escape" });
  assert.equal(root.hidden, false); assert.equal(root.querySelector(".modal-close-hint").textContent, "再次按 Esc 关闭");
  await f.api.switchModal.onOk();
  assert.equal(root.hidden, false); assert.equal(input.value, "new");
  assert.match(f.document.querySelector("#switch-modal-status").textContent, /write failed/);
  await f.api.switchModal.onOk(); assert.equal((await editing).note, "new"); assert.equal(root.hidden, true);
});

test("OpenCode saving uses the same busy hint for Escape, backdrop and explicit close", async () => {
  const pending = deferred();
  const f = await fixture(oauthSource, { invoke: () => pending.promise });
  f.api.editOpenCode({ id: "a", token: "wrk_a", note: "draft" });
  const root = f.document.querySelector("dialog"), input = root.querySelector("input");
  const saving = f.button("保存").onclick();
  await f.button("保存").onclick(); assert.equal(f.calls.length, 1);
  assert.equal(input.disabled, true);
  for (const action of [() => f.event(root, "keydown", { key: "Escape" }), () => f.backdrop(root),
    () => f.event(root.querySelector("[data-close]"), "click")]) {
    action(); action(); const hint = root.querySelector(".modal-close-hint");
    assert.equal(hint.hidden, false); assert.equal(hint.textContent, "正在保存，请稍候。");
    assert.equal(root.isConnected, true);
  }
  pending.reject(new Error("write failed")); await saving;
  assert.equal(input.disabled, false); assert.equal(input.value, "draft");
  f.event(root.querySelector("[data-close]"), "click"); await tick();
  assert.equal(root.isConnected, false);
});

test("read-only bills still close on one backdrop click and ignore an earlier load after reopening", async () => {
  const pending = deferred(); let reads = 0;
  const source = await readFile(new URL("../src/raw_events.js", import.meta.url), "utf8");
  const f = await fixture(source, { mocks: { fetchRawEvents: () => ++reads === 1 ? pending.promise : { events: [], total: 0 } } });
  const old = f.api.openRawEvents({ accountId: "old", label: "旧账单" });
  const root = f.document.querySelector("#raw-modal");
  f.backdrop(root); assert.equal(root.hidden, true);
  await f.api.openRawEvents({ accountId: "new", label: "新账单" });
  pending.resolve({ events: [], total: 100 }); await old;
  assert.equal(root.hidden, false);
  assert.match(f.document.querySelector("#raw-modal-title").textContent, /新账单/);
  assert.doesNotMatch(f.document.querySelector("#raw-modal-sub").textContent, /100/);
  f.event(root, "keydown", { key: "Escape" }); assert.equal(root.hidden, true);
});

for (const name of ["pricing", "proxy", "settings"]) {
  test(`${name} editor shares close hints and prevents closing or duplicate saves while writing`, async () => {
    const source = await readFile(new URL(`../src/${name}.js`, import.meta.url), "utf8")
      + `\nexport { ${name === "settings" ? "onDone" : "onSave"} };`;
    const pending = deferred(); let saves = 0;
    const f = await fixture(source, { mocks: { saveSyncSettings: () => { saves++; return pending.promise; } }, invoke: cmd => {
      if (cmd === `${name}_save` || cmd === "proxy_set") { saves++; return pending.promise; }
      return { mode: "system", models: [], hasUpdate: false };
    } });
    await f.api[`init${name[0].toUpperCase() + name.slice(1)}`]();
    const trigger = f.document.querySelector(name === "pricing" ? "#pricing-chip" : `#${name}-btn`);
    f.event(trigger, "click"); await tick();
    const root = f.document.querySelector(`#${name}-modal`), hint = root.querySelector(".modal-close-hint");
    f.backdrop(root); assert.equal(hint.textContent, "再次点击关闭");
    f.event(root, "keydown", { key: "Escape" }); assert.equal(hint.textContent, "再次按 Esc 关闭");
    f.backdrop(root); assert.equal(root.hidden, false);
    f.backdrop(root); assert.equal(root.hidden, true);
    f.event(trigger, "click"); await tick();
    f.event(root, "keydown", { key: "Escape" }); assert.equal(root.hidden, false);
    f.event(root, "keydown", { key: "Escape" }); assert.equal(root.hidden, true);
    f.event(trigger, "click"); await tick();
    const save = name === "settings" ? f.api.onDone : f.api.onSave;
    const saving = save(); await save(); assert.equal(saves, 1);
    f.event(root.querySelector("[data-close]:not(.modal-backdrop)"), "click");
    assert.equal(root.hidden, false); assert.equal(hint.hidden, false);
    assert.equal(hint.textContent, "正在保存，请稍候。");
    pending.resolve(name === "settings" ? true : { models: 0, mode: "system" }); await saving;
    assert.equal(root.hidden, true);
  });
}
