// node --experimental-vm-modules --test scripts/test-account-interactions.mjs
// 真实 DOM 与生产交互，替换桌面 IPC、布局坐标和动画时钟，不访问真实账户。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const orderSource = await readFile(new URL("../src/account_order.js", import.meta.url), "utf8");
const oauthSource = await readFile(new URL("../src/opencode_ui.js", import.meta.url), "utf8");
const linkedSource = await readFile(new URL("../src/linked_client_ui.js", import.meta.url), "utf8");
const accountSource = await readFile(new URL("../src/accounts.js", import.meta.url), "utf8")
  + "\nexport { accountRow, rowErrors, openModal, onSave, applyView, switchModal, closeModal };";
const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

async function fixture(source, { stored, invoke = async () => {}, mocks = {}, events, markup = html, realModules = [], nowMs = 10000 } = {}) {
  const { document, window: dom } = parseHTML(markup);
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
  let nextTimer = 0, now = nowMs;
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
  window.__TAURI__ = { event: events, core: { invoke: async (cmd, args) => { calls.push({ cmd, args }); return invoke(cmd, args); } } };
  const modules = new Map();
  for (const name of ["shared", "modal", "codex_auth_editor", "codex_auth_add", "account_format", "account_order", "account_refresh", "client_dialog", "linked_client_ui", "opencode_ui", ...realModules]) {
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
    order: () => [...document.querySelectorAll("#accounts-body tr")].map(row => row.dataset.id),
    handle: id => document.querySelector(`[data-id="${id}"] .account-order-handle`),
    button: text => [...document.querySelectorAll("dialog button")].find(button => button.textContent === text),
  };
}

function accountBackend() {
  const accounts = ["cursor", "opencode", "cursor"].map((kind, i) => ({
    id: "id" + i, kind, token: "fixture", note: "账户" + i,
  }));
  const state = { accounts, intervalMinutes: 0, accountOrder: ["id2", "id1", "id0"], orderRevision: 1 };
  const listeners = [];
  const events = { async listen(name, callback) {
    if (name === "accounts-changed") listeners.push(callback);
    return () => {};
  } };
  const broadcast = () => { for (const fn of listeners) fn({ payload: structuredClone(state) }); };
  return { state, broadcast, events, async invoke(cmd, args) {
    if (cmd === "accounts_list") return structuredClone(state);
    if (cmd === "accounts_move") {
      const order = state.accountOrder.filter(id => id !== args.id);
      order.splice(args.beforeId === null ? order.length : order.indexOf(args.beforeId), 0, args.id);
      state.accountOrder = order;
      state.orderRevision++;
      broadcast();
      return structuredClone(state);
    }
  } };
}

async function accountWindow(backend) {
  const f = await fixture(accountSource, backend);
  f.api.initAccounts();
  await tick();
  return f;
}

test("one list shows mixed accounts, keyboard moves reach another window and survive reopening", async () => {
  const backend = accountBackend();
  const first = await accountWindow(backend), second = await accountWindow(backend);
  assert.deepEqual(first.order(), ["id2", "id1", "id0"]);
  assert.deepEqual([...first.document.querySelectorAll(".account-name-line .tag")].map(n => n.textContent),
    ["Cursor", "OpenCode", "Cursor"]);
  first.event(first.handle("id0"), "keydown", { key: "ArrowUp" }); await tick();
  second.advance(150);
  assert.deepEqual(second.order(), ["id2", "id0", "id1"]);
  second.event(second.handle("id2"), "keydown", { key: "ArrowDown" }); await tick();
  first.advance(150);
  assert.deepEqual(first.order(), ["id0", "id2", "id1"]);
  assert.deepEqual((await accountWindow(backend)).order(), first.order());
});

test("drag previews across providers, Escape cancels, failed storage restores committed order", async () => {
  const backend = accountBackend(), f = await accountWindow(backend);
  const original = f.order();
  const drag = () => {
    const list = f.document.querySelector("#accounts-body");
    for (const row of list.children) row.getBoundingClientRect = () => ({ top: [...list.children].indexOf(row) * 100, height: 100 });
    f.event(f.handle("id1"), "pointerdown", { button: 0, pointerId: 7, clientY: 150 });
    f.event(f.window, "pointermove", { pointerId: 7, clientY: 10 });
    assert.deepEqual(f.order(), ["id1", "id2", "id0"]);
  };
  drag();
  f.event(f.handle("id1"), "keydown", { key: "Escape" });
  assert.deepEqual(f.order(), original);
  assert.equal(f.calls.some(c => c.cmd === "accounts_move"), false);
  drag();
  f.event(f.window, "pointerup", { pointerId: 7 }); await tick();
  assert.deepEqual(f.order(), ["id1", "id2", "id0"]);
  const failed = await accountWindow({ ...backend, invoke: (cmd, args) => {
    if (cmd === "accounts_move") throw Error("disk");
    return backend.invoke(cmd, args);
  } });
  failed.event(failed.handle("id1"), "keydown", { key: "ArrowDown" }); await tick();
  assert.deepEqual(failed.order(), f.order());
});

test("empty list keeps the add picker and dispatches OpenCode to its own authorization flow", async () => {
  const backend = accountBackend(); backend.state.accounts = []; backend.state.accountOrder = [];
  const f = await accountWindow(backend);
  assert.equal(f.order().length, 0);
  assert.equal(f.document.querySelector("#accounts-empty").hidden, false);
  f.document.querySelector("#accounts-add").click();
  const picker = f.document.querySelector("#account-type-modal");
  assert.equal(picker.hidden, false);
  assert.equal(picker.querySelectorAll("[data-kind]").length, 4);
  picker.querySelector('[data-kind="opencode"]').click();
  assert.equal(picker.hidden, true);
  assert.ok(f.button("新增 OAuth 授权"));
});

test("older IPC replies cannot roll back a newer order broadcast", async () => {
  const f = await fixture(orderSource);
  const order = f.api.trackAccountOrder(), accounts = [{id:"a"},{id:"b"}];
  order.apply({ accountOrder:["b","a"], orderRevision:2 });
  order.apply({ accountOrder:["a","b"], orderRevision:1 });
  assert.deepEqual(Array.from(order.rows(accounts), a => a.id), ["b","a"]);
});

test("provider selection happens once and every account form names its fixed provider", async () => {
  const f = await accountWindow(accountBackend()), q = s => f.document.querySelector(s);
  assert.equal(q("#account-kind"), null);
  for (const [kind, label] of [["cursor", "Cursor"], ["codex", "ChatGPT"], ["claude", "Claude"]]) {
    q("#accounts-add").click();
    assert.equal(q("#account-type-modal").querySelectorAll("[data-kind]").length, 4);
    q(`#account-type-modal [data-kind="${kind}"]`).click();
    assert.equal(q("#account-modal-title").textContent, `添加 ${label} 账户`);
    assert.equal(q("#account-add-mode").hidden, kind !== "codex");
    assert.equal(q("#account-import-local").textContent, `从本机导入 ${label}`);
    f.api.closeModal();
    f.api.openModal({ id: kind, kind, token: "fixture" });
    assert.equal(q("#account-modal-title").textContent, `编辑 ${label} 账户`);
    assert.equal(q("#account-add-mode").hidden, true);
    assert.equal(q("#account-auth-mode").hidden, kind !== "codex");
    f.api.closeModal();
  }
});

const addAuthText = '{"tokens":{"access_token":"fixture-file","refresh_token":"fixture-refresh","id_token":"fixture-id"},"extra":9007199254740993}';

function addBackend() {
  const backend = accountBackend(); backend.state.accounts = []; backend.state.accountOrder = [];
  return { ...backend, async invoke(cmd, args) {
    if (cmd === "codex_auth_pick_file") return { name: "auth.json", text: addAuthText };
    if (cmd === "accounts_add") {
      const row = { id: "added", kind: args.account.kind, note: args.account.note, token: "fixture-added" };
      backend.state.accounts.push(row); backend.state.accountOrder.push(row.id);
      return structuredClone(backend.state);
    }
    if (cmd === "account_refresh") return backend.state.accounts.find(a => a.id === args.id);
    return backend.invoke(cmd, args);
  } };
}

test("ChatGPT add tabs keep independent drafts, support keyboard selection and submit only active JSON", async () => {
  const f = await accountWindow(addBackend()), q = s => f.document.querySelector(s);
  f.api.openModal(null, "codex");
  assert.equal(q("#account-add-mode-token").getAttribute("aria-selected"), "true");
  assert.equal(q("#account-token-fields").hidden, false);
  q("#account-note").value = "共用备注";
  q("#account-token").value = "manual-access"; q("#account-refresh-token").value = "manual-refresh";
  f.event(q("#account-add-mode-token"), "keydown", { key: "ArrowRight" });
  assert.equal(f.document.activeElement, q("#account-add-mode-file"));
  q("#account-auth-pick").click(); await tick();
  assert.equal(q("#account-auth-file-name").textContent, "auth.json");
  f.event(q("#account-add-mode-file"), "keydown", { key: "End" });
  assert.equal(q("#account-auth-field").hidden, false);
  const json = addAuthText.replace("fixture-file", "fixture-json");
  q("#account-auth-json").value = json;
  f.event(q("#account-add-mode-json"), "keydown", { key: "Home" });
  assert.equal(q("#account-token").value, "manual-access");
  assert.equal(q("#account-refresh-token").value, "manual-refresh");
  f.event(q("#account-add-mode-token"), "keydown", { key: "ArrowLeft" });
  assert.equal(q("#account-auth-json").value, json);
  assert.equal(q("#account-note").value, "共用备注");
  assert.equal(q("#account-add-mode-file").tabIndex, -1);
  assert.equal(f.calls.some(c => c.cmd === "accounts_add"), false);
  await f.api.onSave(); await tick();
  const write = f.calls.find(c => c.cmd === "accounts_add");
  assert.deepEqual(JSON.parse(JSON.stringify(write.args.account)), { kind: "codex", note: "共用备注", codexAuthText: json });
  assert.equal(f.calls.filter(c => c.cmd === "accounts_add").length, 1);
  assert.equal(f.calls.filter(c => c.cmd === "account_refresh").length, 1);
  assert.equal(q("#account-modal").hidden, true);
  assert.equal(q("#account-auth-json").value, "");
  assert.equal(q("#account-token").value, "");
  assert.equal(q("#account-auth-file-name").textContent, "尚未选择文件");
  f.api.openModal(null, "codex");
  assert.equal(q("#account-add-mode-token").getAttribute("aria-selected"), "true");
});

test("file add preserves its exact source across cancelled and failed reselection", async () => {
  const backend = addBackend(); let selection = 0;
  const f = await accountWindow({ ...backend, invoke: (cmd, args) => {
    if (cmd === "codex_auth_pick_file") {
      selection++;
      if (selection === 2) return null;
      if (selection === 3) throw Error("codex_auth_access_token");
    }
    return backend.invoke(cmd, args);
  } });
  const q = s => f.document.querySelector(s);
  f.api.openModal(null, "codex"); q("#account-add-mode-file").click();
  await f.api.onSave();
  assert.match(q("#account-auth-status").textContent, /请选择/);
  for (let i = 0; i < 3; i++) { q("#account-auth-pick").click(); await tick(); }
  assert.equal(q("#account-auth-file-name").textContent, "auth.json");
  assert.match(q("#account-auth-status").textContent, /tokens.access_token/);
  q("#account-add-mode-json").click(); q("#account-auth-json").value = "invalid unused JSON";
  q("#account-add-mode-file").click();
  await f.api.onSave();
  assert.equal(f.calls.find(c => c.cmd === "accounts_add").args.account.codexAuthText, addAuthText);
});

test("Token add ignores other drafts and keeps the existing token request shape", async () => {
  const f = await accountWindow(addBackend()), q = s => f.document.querySelector(s);
  f.api.openModal(null, "codex");
  q("#account-add-mode-json").click(); q("#account-auth-json").value = "invalid unused JSON";
  q("#account-add-mode-token").click();
  await f.api.onSave();
  assert.equal(f.calls.some(c => c.cmd === "accounts_add"), false);
  q("#account-token").value = " manual "; q("#account-refresh-token").value = " refresh ";
  await f.api.onSave();
  assert.deepEqual(JSON.parse(JSON.stringify(f.calls.find(c => c.cmd === "accounts_add").args.account)),
    { kind: "codex", note: "", token: "manual", refreshToken: "refresh" });
});

test("JSON validation, duplicates and failed writes retain drafts without leaking backend credential text", async () => {
  const backend = addBackend(); let error = "";
  const f = await accountWindow({ ...backend, invoke: (cmd, args) => {
    if (cmd === "accounts_add") throw Error(error);
    return backend.invoke(cmd, args);
  } });
  const q = s => f.document.querySelector(s);
  f.api.openModal(null, "codex"); q("#account-add-mode-json").click();
  await f.api.onSave();
  assert.match(q("#account-auth-status").textContent, /请粘贴/);
  assert.equal(f.calls.some(c => c.cmd === "accounts_add"), false);
  for (const [text, code, expected] of [
    ['{\n"tokens":}', "codex_auth_json:2:10", /第 2 行，第 10 列/],
    ["[]", "codex_auth_object", /JSON 对象/],
    ['{"tokens":{}}', "codex_auth_access_token", /tokens.access_token/],
    ['{"tokens":{"access_token":"a","refresh_token":42}}', "codex_auth_refresh_token", /tokens.refresh_token/],
    [addAuthText, "duplicate_account", /已存在/],
    [addAuthText, "disk full", /保存失败/],
  ]) {
    error = code; q("#account-auth-json").value = text; q("#account-note").value = "keep note";
    await f.api.onSave();
    const status = code === "disk full" ? q("#account-modal-status") : q("#account-auth-status");
    assert.match(status.textContent, expected);
    assert.equal(status.textContent.includes("fixture-refresh"), false);
    assert.equal(q("#account-auth-json").value, text);
    assert.equal(q("#account-note").value, "keep note");
    assert.equal(q("#account-modal").hidden, false);
    assert.equal(q("#account-save").disabled, false);
    assert.equal(q("#account-add-mode-json").disabled, false);
  }
  assert.equal(f.calls.some(c => c.cmd === "account_refresh"), false);
});

test("closing during file selection discards late replies without changing the next account form", async () => {
  const backend = addBackend(), pending = deferred();
  const f = await accountWindow({ ...backend, invoke: (cmd, args) =>
    cmd === "codex_auth_pick_file" ? pending.promise : backend.invoke(cmd, args) });
  const q = s => f.document.querySelector(s);
  f.api.openModal(null, "codex"); q("#account-add-mode-file").click(); q("#account-auth-pick").click();
  assert.equal(q("#account-save").disabled, true);
  await f.api.onSave();
  assert.equal(f.calls.some(c => c.cmd === "accounts_add"), false);
  q("#account-cancel").click();
  f.api.openModal(null, "codex"); q("#account-add-mode-json").click();
  q("#account-auth-json").value = "new draft";
  pending.resolve({ name: "old.json", text: addAuthText }); await tick();
  assert.equal(q("#account-auth-json").value, "new draft");
  assert.equal(q("#account-auth-file-name").textContent, "尚未选择文件");
  assert.equal(q("#account-save").disabled, false);
});

test("saving full Auth locks every source and close action and prevents duplicate submissions", async () => {
  const backend = addBackend(), pending = deferred();
  const f = await accountWindow({ ...backend, invoke: (cmd, args) =>
    cmd === "accounts_add" ? pending.promise : backend.invoke(cmd, args) });
  const q = s => f.document.querySelector(s);
  f.api.openModal(null, "codex"); q("#account-add-mode-json").click(); q("#account-auth-json").value = addAuthText;
  const saving = f.api.onSave();
  for (const selector of ["#account-add-mode-token", "#account-add-mode-file", "#account-add-mode-json", "#account-auth-pick", "#account-import-local", "#account-auth-json"]) {
    assert.equal(q(selector).disabled, true);
  }
  f.event(q("#account-add-mode-json"), "keydown", { key: "Home" });
  q("#account-cancel").click(); f.event(f.document, "keydown", { key: "Escape" });
  f.backdrop(q("#account-modal")); await f.api.onSave();
  assert.equal(q("#account-modal").hidden, false);
  assert.equal(q("#account-add-mode-json").getAttribute("aria-selected"), "true");
  assert.equal(f.calls.filter(c => c.cmd === "accounts_add").length, 1);
  pending.reject(Error("disk full")); await saving;
  assert.equal(q("#account-auth-json").value, addAuthText);
  assert.equal(q("#account-auth-pick").disabled, false);
  assert.equal(q("#account-add-mode-token").disabled, false);
});

test("legacy card preferences reach the backend when the account list first opens", async () => {
  const f = await fixture(orderSource, { stored: [["accountGroupOrder", '["opencode","cursor","claude","codex"]']] });
  await f.api.loadAccountList();
  assert.equal(f.calls[0].cmd, "accounts_list");
  assert.deepEqual([...f.calls[0].args.legacyGroups], ["opencode","cursor","claude","codex"]);
});

test("toolbar refreshes all accounts and exports all or the exact selected row", async () => {
  const f = await accountWindow(accountBackend());
  const header = [...f.document.querySelectorAll("#accounts-panel .heading-side button")];
  assert.deepEqual(header.map(button => button.id), ["accounts-refresh", "accounts-import", "accounts-export", "accounts-logout", "accounts-add"]);
  f.calls.length = 0;
  header[0].click(); await tick();
  assert.deepEqual(f.calls.filter(c => c.cmd === "account_refresh").map(c => c.args.id).sort(), ["id0", "id1", "id2"]);
  for (const id of f.order()) {
    const row = f.document.querySelector(`tr[data-id="${id}"]`);
    const buttons = row.querySelectorAll('button[aria-label="导出此账户"]');
    assert.equal(buttons.length, 1);
    assert.equal(buttons[0], row.querySelector(".account-actions").lastElementChild);
    buttons[0].click(); await tick();
    assert.equal(f.calls.at(-1).cmd, "account_export_one");
    assert.equal(f.calls.at(-1).args.id, id);
  }
  header[2].click(); await tick();
  assert.equal(f.calls.at(-1).cmd, "accounts_export_all");
  header[1].click(); await tick();
  assert.equal(f.calls.at(-1).cmd, "accounts_import_file");
});


test("sign-out reuses the add picker and confirms only the chosen local client", async () => {
  const backend = accountBackend();
  backend.state.accounts = []; backend.state.accountOrder = [];
  const f = await accountWindow({ ...backend, invoke: (cmd, args) =>
    cmd.endsWith("_client_status") ? { running: false } : backend.invoke(cmd, args) });
  const q = s => f.document.querySelector(s);
  assert.equal(q("#accounts-export").disabled, true);
  assert.equal(q("#accounts-import").disabled, false);
  for (const kind of ["cursor", "codex", "claude"]) {
    q("#accounts-logout").click();
    assert.deepEqual([...q("#account-type-modal").querySelectorAll("[data-kind]")]
      .filter(b => !b.hidden).map(b => b.dataset.kind), ["cursor", "codex", "claude"]);
    q('#account-type-modal [data-kind="' + kind + '"]').click(); await tick();
    assert.equal(f.calls.at(-1).cmd, kind + "_client_status");
    assert.equal(q("#switch-modal").hidden, false);
    q("#switch-cancel").click(); await tick();
    assert.equal(f.calls.some(c => c.cmd.endsWith("_logout_local")), false);
  }
  q("#accounts-add").click();
  assert.equal(q('#account-type-modal [data-kind="opencode"]').hidden, false);
});

function authBackend() {
  const documents = new Map(["a", "b"].map(id => [id, {
    tokens: { access_token: "fixture-access-" + id, refresh_token: "fixture-refresh-" + id, account_id: id, id_token: "fixture-id-" + id },
    last_refresh: "2026-11-18T12:00:00Z", custom: { owner: id, nested: [1, 2] },
  }]));
  const rows = ["a", "b"].map(id => ({ id, kind: "codex", note: "Account " + id, noteAuto: true,
    token: documents.get(id).tokens.access_token, refreshToken: documents.get(id).tokens.refresh_token,
    codexAuthRevision: "r1", refreshIntervalMinutes: 5, autoRefreshToken: true }));
  const view = () => structuredClone({ accounts: rows, accountOrder: ["b", "a"], intervalMinutes: 0 });
  const read = id => ({ text: JSON.stringify(documents.get(id), null, 2), revision: rows.find(a => a.id === id).codexAuthRevision });
  return { rows, documents, read, async invoke(cmd, args) {
    if (cmd === "accounts_list") return view();
    if (cmd === "codex_auth_read") return read(args.id);
    if (cmd === "codex_auth_project") {
      let auth;
      try { auth = JSON.parse(args.text); } catch { throw new Error("codex_auth_json:2:9"); }
      if (args.patch) {
        auth.tokens.access_token = args.patch.accessToken;
        if (args.patch.refreshToken) auth.tokens.refresh_token = args.patch.refreshToken;
        else delete auth.tokens.refresh_token;
      }
      if (!auth?.tokens?.access_token) throw new Error("codex_auth_access_token");
      return { text: JSON.stringify(auth, null, 2), accessToken: auth.tokens.access_token, refreshToken: auth.tokens.refresh_token };
    }
    if (cmd === "accounts_update") {
      const row = rows.find(a => a.id === args.id);
      if (args.updateCredentials) {
        if (args.expectedAuthRevision !== row.codexAuthRevision) throw new Error("codex_auth_conflict");
        const next = JSON.parse(args.codexAuthText);
        documents.set(row.id, next);
        row.token = next.tokens.access_token;
        row.refreshToken = next.tokens.refresh_token;
        row.codexAuthRevision += "x";
      }
      if ("note" in args) row.note = args.note;
      if (args.updateInterval) row.refreshIntervalMinutes = args.refreshIntervalMinutes;
      if ("autoRefreshToken" in args) row.autoRefreshToken = args.autoRefreshToken;
      return view();
    }
  } };
}

test("ChatGPT Auth is loaded by ID on demand and both modes keep edits until a single save", async () => {
  const backend = authBackend(), f = await accountWindow(backend);
  const q = s => f.document.querySelector(s), click = async s => { q(s).click(); await tick(); };
  f.api.openModal(backend.rows[1]);
  assert.equal(q("#account-auth-field").hidden, true);
  assert.equal(f.calls.some(c => c.cmd === "codex_auth_read"), false);
  q("#account-token").value = "fixture-first-edit";
  await click("#account-mode-auth");
  const raw = q("#account-auth-json");
  assert.equal(f.calls.find(c => c.cmd === "codex_auth_read").args.id, "b");
  const draft = JSON.parse(raw.value);
  assert.equal(draft.tokens.access_token, "fixture-first-edit");
  draft.tokens.access_token = "fixture-pasted";
  delete draft.tokens.refresh_token;
  draft.more = { enabled: true };
  raw.value = JSON.stringify(draft);
  await click("#account-mode-token");
  assert.equal(q("#account-token").value, "fixture-pasted");
  assert.equal(q("#account-refresh-token").value, "");
  await click("#account-mode-auth");
  assert.equal(raw.value, JSON.stringify(draft));
  await click("#account-mode-token");
  q("#account-token").value = "fixture-final";
  q("#account-note").value = "新备注";
  q("#account-refresh-interval").value = "";
  q("#account-auto-refresh-token").checked = false;
  await f.api.onSave();
  assert.equal(q("#account-modal").hidden, true);
  const writes = f.calls.filter(c => c.cmd === "accounts_update");
  assert.equal(writes.length, 1);
  assert.equal(writes[0].args.id, "b");
  assert.equal(writes[0].args.expectedAuthRevision, "r1");
  assert.equal(writes[0].args.note, "新备注");
  assert.equal(writes[0].args.refreshIntervalMinutes, null);
  assert.equal(writes[0].args.autoRefreshToken, false);
  assert.equal("token" in writes[0].args, false);
  f.api.openModal(backend.rows[1]);
  await click("#account-mode-auth");
  draft.tokens.access_token = "fixture-final";
  assert.deepEqual(JSON.parse(raw.value), draft);
  assert.equal(backend.documents.get("a").tokens.account_id, "a");
});

test("viewing Auth sends no account update, a metadata-only edit preserves renewed credentials", async () => {
  const backend = authBackend(), f = await accountWindow(backend);
  const q = s => f.document.querySelector(s);
  const open = async () => { f.api.openModal(structuredClone(backend.rows[0])); q("#account-mode-auth").click(); await tick(); };
  await open();
  await f.api.onSave();
  assert.equal(f.calls.filter(c => c.cmd === "accounts_update").length, 0);
  assert.equal(q("#account-auth-json").value, "");
  await open();
  backend.rows[0].codexAuthRevision = "r2";
  backend.documents.get("a").tokens.id_token = "fixture-renewed";
  q("#account-note").value = "edited note";
  await f.api.onSave();
  const write = f.calls.at(-1);
  assert.equal(write.cmd, "accounts_update");
  assert.deepEqual(Object.keys(write.args).sort(), ["id", "note", "updateCredentials", "updateInterval"]);
  assert.equal(write.args.updateCredentials, false);
  assert.equal(backend.documents.get("a").tokens.id_token, "fixture-renewed");
});

test("invalid JSON and concurrent renewal preserve the draft and reload only replaces credentials", async () => {
  const backend = authBackend(), f = await accountWindow(backend);
  const q = s => f.document.querySelector(s), click = async s => { q(s).click(); await tick(); };
  f.api.openModal(structuredClone(backend.rows[0]));
  await click("#account-mode-auth");
  const raw = q("#account-auth-json"), note = q("#account-note"), status = q("#account-auth-status");
  raw.value = '{\n"tokens":}';
  await click("#account-mode-token");
  assert.equal(raw.value, '{\n"tokens":}');
  assert.equal(q("#account-auth-field").hidden, false);
  assert.match(status.textContent, /第 2 行，第 9 列/);
  await f.api.onSave();
  assert.equal(f.calls.some(c => c.cmd === "accounts_update"), false);
  note.value = "keep me";
  const draft = structuredClone(backend.documents.get("a")); draft.custom.extra = true;
  raw.value = JSON.stringify(draft);
  backend.documents.get("a").tokens.access_token = "fixture-renewed";
  backend.rows[0].codexAuthRevision = "r2";
  await f.api.onSave();
  assert.equal(q("#account-modal").hidden, false);
  assert.equal(raw.value, JSON.stringify(draft));
  assert.equal(note.value, "keep me");
  assert.equal(q("#account-auth-reload").hidden, false);
  await click("#account-auth-reload");
  assert.equal(JSON.parse(raw.value).tokens.access_token, "fixture-renewed");
  assert.equal(note.value, "keep me");
  await f.api.onSave();
  assert.equal(f.calls.at(-1).args.updateCredentials, false);
});

test("first Auth read detects renewal after typing in Token mode and keeps that input", async () => {
  const backend = authBackend(), f = await accountWindow(backend);
  const q = s => f.document.querySelector(s);
  f.api.openModal(structuredClone(backend.rows[0]));
  q("#account-token").value = "manual-draft";
  backend.rows[0].codexAuthRevision = "r2";
  backend.documents.get("a").tokens.access_token = "renewed-access";
  q("#account-mode-auth").click(); await tick();
  assert.equal(q("#account-token").value, "manual-draft");
  assert.equal(q("#account-token-fields").hidden, false);
  assert.equal(q("#account-auth-reload").hidden, false);
  assert.equal(f.calls.some(c => c.cmd === "accounts_update"), false);
  q("#account-auth-reload").click(); await tick();
  assert.equal(q("#account-auth-field").hidden, false);
  assert.equal(JSON.parse(q("#account-auth-json").value).tokens.access_token, "renewed-access");
});

test("a pending Auth read from a closed account never populates the next editor", async () => {
  const backend = authBackend(), first = deferred();
  const f = await accountWindow({ ...backend, invoke: (cmd, args) =>
    cmd === "codex_auth_read" && args.id === "a" ? first.promise : backend.invoke(cmd, args) });
  const q = s => f.document.querySelector(s);
  f.api.openModal(backend.rows[0]); q("#account-mode-auth").click();
  q("#account-cancel").click();
  f.api.openModal(backend.rows[1]); q("#account-mode-auth").click(); await tick();
  first.resolve(backend.read("a")); await tick();
  assert.equal(JSON.parse(q("#account-auth-json").value).tokens.account_id, "b");
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
      if (cmd === "local_clients_status") return { cli: { installed: true }, desktop: { installed: false } };
      if (cmd === (native ? "local_clients_switch" : "opencode_oauth_start")) {
        return { state: "pending", id: "attempt_retry", url: "https://example.invalid/device", interval: 3 };
      }
      const cancel = native ? cmd === "opencode_client_oauth_poll" && args.cancel : cmd === "opencode_oauth_cancel";
      if (cancel) {
        assert.equal(args.id, "attempt_retry");
        if (++cancels === 1) throw new Error("restore failed");
        return { state: "cancelled" };
      }
      return poll.promise;
    } });
    if (native) {
      void f.api.switchOpenCode("account", () => completed++);
      await tick(); await f.button("仅 CLI").onclick();
    } else await f.api.authorizeOpenCode(null, () => completed++);
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

test("native OpenCode authorization retries a failed poll and settles an expired attempt", async () => {
  let polls = 0;
  const f = await fixture(linkedSource, { invoke: (cmd, args) => {
    if (cmd === "local_clients_status") return { cli: { installed: true }, desktop: { installed: false } };
    if (cmd === "local_clients_switch") return { state: "pending", id: "retry", url: "https://example.invalid/device" };
    assert.equal(cmd, "opencode_client_oauth_poll");
    assert.equal(args.id, "retry");
    assert.equal(args.cancel, false);
    throw ++polls === 1 ? "opencode_cli_failed" : "opencode_oauth_expired";
  } });
  const switching = f.api.switchLocalClients({ id: "test", kind: "opencode" });
  await tick();
  await f.button("仅 CLI").onclick();
  f.advance(3000);
  await tick();
  assert.equal(polls, 1);
  assert.equal(f.timers.size, 1);
  f.advance(3000);
  await tick();
  assert.equal(polls, 2);
  assert.equal(f.timers.size, 0);
  assert.match(f.document.querySelector("dialog .status").textContent, /授权已取消或超时/);
  assert.equal(await switching, false);
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
  f.api.editOpenCode({ id: "a", token: "wrk_a", note: "old", opencodeAuthorized: true, refreshIntervalMinutes: 15, autoRefreshToken: false }, () => completed++);
  const renewal = f.document.querySelector('dialog input[role="switch"]');
  assert.equal(renewal.checked, false);
  renewal.checked = true;
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
  assert.equal(renewal.checked, true);
  assert.equal(f.calls[1].args.autoRefreshToken, true);
  assert.equal(f.calls[1].args.updateCredentials, false);
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

test("renewal editors preserve failed drafts and never resubmit credentials changed in the background", async () => {
  for (const kind of ["codex", "claude"]) {
    let current = { id: kind, kind, note: "fixture", token: "old-access", refreshToken: "old-refresh" };
    let fail = true;
    const f = await fixture(accountSource, { invoke: async (cmd, args) => {
      assert.equal(cmd, "accounts_update");
      if (fail) throw new Error("disk full");
      current = { ...current, autoRefreshToken: args.autoRefreshToken };
      return { accounts: [current], intervalMinutes: 0 };
    } });
    f.api.applyView({ accounts: [current], intervalMinutes: 0 });
    f.api.openModal(current);
    const toggle = f.document.querySelector("#account-auto-refresh-token");
    assert.equal(f.document.querySelector("#account-auto-refresh-field").hidden, false);
    assert.equal(toggle.checked, true);
    toggle.checked = false;
    current = { ...current, token: "renewed-access", refreshToken: "renewed-refresh" };
    f.api.applyView({ accounts: [current], intervalMinutes: 0 });
    await f.api.onSave();
    assert.equal(toggle.checked, false);
    assert.equal(f.document.querySelector("#account-modal").hidden, false);
    assert.equal(f.calls.at(-1).args.updateCredentials, false);
    fail = false; await f.api.onSave();
    assert.equal(f.calls.at(-1).args.autoRefreshToken, false);
    f.api.openModal(current);
    assert.equal(toggle.checked, false);
    assert.equal(f.document.querySelector("#account-token").value, "renewed-access");
    f.document.querySelector("#account-refresh-token").value = "manual-refresh";
    await f.api.onSave();
    assert.equal(f.calls.at(-1).args.updateCredentials, true);
    f.api.openModal(null, kind);
    assert.equal(f.document.querySelector("#account-auto-refresh-field").hidden, true);
  }
  const f = await fixture(accountSource);
  f.api.openModal({ id: "cursor", kind: "cursor", token: "fixture" });
  assert.equal(f.document.querySelector("#account-auto-refresh-field").hidden, true);
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
  assert.equal(row.children[5].textContent, "$0.00");
});

async function accountAndTrayWindows(accounts, nowMs) {
  const backend = { nowMs, invoke: async cmd => cmd === "accounts_list" ? { accounts, intervalMinutes: 0 } : null };
  const main = await accountWindow(backend);
  const tray = await fixture(await readFile(new URL("../src/tray_panel.js", import.meta.url), "utf8"), {
    ...backend, markup: await readFile(new URL("../src/tray.html", import.meta.url), "utf8"),
    stored: [["trayTab", "accounts"]], realModules: ["usage_data"],
  });
  await tick();
  return { main, tray };
}

test("subscription tooltips in the account list and tray preserve local minutes and calendar dates", async () => {
  const midnight = new Date(2026, 11, 1, 0, 0, 58).toISOString();
  const afternoon = new Date(2026, 11, 1, 15, 30, 49).toISOString();
  const accounts = [
    { id: "chat", kind: "codex", status: { planActiveStart: "2026-11-01", planActiveUntil: afternoon, planWillRenew: true } },
    { id: "cursor", kind: "cursor", status: { billingCycleStart: midnight, billingCycleEnd: afternoon } },
    { id: "midnight", kind: "opencode", status: { billingCycleEnd: midnight, planWillRenew: false } },
    { id: "date", kind: "opencode", status: { billingCycleEnd: "2026-12-01" } },
    { id: "seconds", kind: "opencode", status: { billingCycleEnd: Date.parse(afternoon) / 1000 } },
    { id: "millis", kind: "opencode", status: { billingCycleEnd: Date.parse(afternoon) } },
    { id: "missing", kind: "codex", status: {} },
    { id: "zero", kind: "opencode", status: { billingCycleEnd: 0 } },
    { id: "invalid", kind: "cursor", status: { billingCycleEnd: "invalid" } },
  ].map(account => ({ ...account, opencodeAuthorized: true, status: { alive: true, ...account.status } }));
  const { main, tray } = await accountAndTrayWindows(accounts);

  for (const [id, mainTitle, trayTitle] of [
    ["chat", "订阅有效期：2026/11/01 ~ 2026/12/01 15:30（到期自动续期）", "订阅至 2026/12/01 15:30（到期自动续期）"],
    ["cursor", "本期计费周期：2026/12/01 00:00 ~ 2026/12/01 15:30（到期自动续期，额度重置）",
      "本期计费周期截止 2026/12/01 15:30（到期自动续期，额度重置）"],
    ["midnight", "本期计费周期截止 2026/12/01 00:00（到期不续期）", "订阅至 2026/12/01 00:00（到期不续期）"],
    ["date", "本期计费周期截止 2026/12/01", "订阅至 2026/12/01"],
    ["seconds", "本期计费周期截止 2026/12/01 15:30", "订阅至 2026/12/01 15:30"],
    ["millis", "本期计费周期截止 2026/12/01 15:30", "订阅至 2026/12/01 15:30"],
  ]) {
    const expiry = main.document.querySelector(`[data-id="${id}"] .plan-expiry`);
    const summary = tray.document.querySelector(`[data-id="${id}"] .tray-sub span`);
    assert.equal(expiry.title, mainTitle);
    assert.equal(summary.title.split("\n").at(-1), trayTitle);
    assert.match(expiry.textContent, /^\d+d$/);
    assert.ok(summary.textContent.includes(`有效期 ${expiry.textContent}`));
    assert.doesNotMatch(summary.textContent, /2026/);
  }
  for (const id of ["missing", "zero", "invalid"]) {
    assert.equal(main.document.querySelector(`[data-id="${id}"] .plan-expiry`), null);
    const summary = tray.document.querySelector(`[data-id="${id}"] .tray-sub span`);
    assert.doesNotMatch(summary.title, /订阅至|计费周期截止|有效期/);
  }
});

test("quota reset tooltips agree across windows for absolute, anchored, date-only and missing times", async () => {
  const now = new Date(2026, 10, 30, 15, 30).getTime();
  const nowSec = now / 1000;
  const midnight = new Date(2026, 11, 1, 0, 0, 47);
  const meter = (label, timing) => ({ label, usedPercent: 25, ...timing });
  const sand = (id, nextResetAt) => ({ id, kind: "cursor", status: {
    sand: { included: true, usagePercent: 25, nextResetAt },
  } });
  const accounts = [
    { id: "chat", kind: "codex", lastRefreshAt: nowSec, status: { windows: [
      meter("5 小时", { resetAt: String(midnight.getTime() / 1000), resetsInSeconds: 30 }),
      meter("每周", { resetAt: nowSec + 86400 }),
    ] } },
    { id: "claude", kind: "claude", status: { windows: [
      meter("5 小时", { resetAt: nowSec - 120 }), meter("每周", { resetAt: nowSec + 86399 }),
    ] } },
    { id: "open", kind: "opencode", lastRefreshAt: nowSec - 7200, status: { windows: [
      meter("滚动", { resetsInSeconds: 10800 }), meter("每周", {}), meter("每月", { resetAt: nowSec + 3 * 86400 }),
    ] } },
    { id: "unanchored", kind: "codex", status: { windows: [meter("每周", { resetsInSeconds: 1800 })] } },
    sand("sand-time", midnight.toISOString()), sand("sand-today", "2026-11-30"),
    sand("sand-past", "2026-11-29"), sand("sand-invalid", "2026-02-30"),
    { id: "plan", kind: "cursor", status: { plan: { used: 25, limit: 100 } } },
  ].map(account => ({ ...account, opencodeAuthorized: true, status: { alive: true, ...account.status } }));
  const { main, tray } = await accountAndTrayWindows(accounts, now);
  for (const [id, index, inline, full] of [
    ["chat", 0, "重置 00:00", "2026/12/01 00:00"], ["chat", 1, "重置 12-01", "2026/12/01 15:30"],
    ["claude", 0, "已重置", "2026/11/30 15:28"], ["claude", 1, "重置 15:29", "2026/12/01 15:29"],
    ["open", 0, "重置 16:30", "2026/11/30 16:30"], ["open", 1, null, null],
    ["open", 2, "重置 12-03", "2026/12/03 15:30"], ["unanchored", 0, null, null],
    ["sand-time", 0, "重置 00:00", "2026/12/01 00:00"], ["sand-today", 0, "重置 11-30", "2026/11/30"],
    ["sand-past", 0, "已重置", "2026/11/29"], ["sand-invalid", 0, null, null],
  ]) {
    const bar = main.document.querySelectorAll(`[data-id="${id}"] .quota-bar`)[index];
    const mini = tray.document.querySelectorAll(`[data-id="${id}"] .tray-bar`)[index];
    const label = bar.querySelector(".quota-bar-label").textContent;
    const reset = bar.querySelector(".quota-bar-extra");
    assert.equal(reset?.textContent ?? null, inline, id);
    assert.equal(reset?.title ?? null, full && `${full} 重置`, id);
    assert.equal(mini.title, `${label} 剩余 75%` + (full ? `\n${full} 重置` : ""), id);
    assert.equal(mini.querySelector(".tray-bar-val").textContent, "75%");
    assert.doesNotMatch(mini.textContent, /重置/);
  }
  const planDetail = main.document.querySelector('[data-id="plan"] .quota-bar-extra');
  assert.equal(planDetail.textContent, "25 / 100");
  assert.equal(planDetail.title, planDetail.textContent);
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
    const trigger = f.document.querySelector("#accounts-add");
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

test("OpenCode raw bills export all filtered pages with blank unknown costs and token subcounts", async () => {
  const source = await readFile(new URL("../src/raw_events.js", import.meta.url), "utf8");
  let exported;
  const events = Array.from({ length: 302 }, (_, i) => ({
    timestampMs: 1700000000000 - i, requestId: "req-" + i, product: "go",
    model: i === 301 ? "other" : "sample", displayModel: "normalized-" + (i === 301 ? "other" : "sample"),
    pricedAs: null, equivalentUsd: null, reportedCostUsd: i === 0 ? 0 : null,
    inputTokens: 10, cacheReadTokens: 30, cacheWriteTokens: 40, cacheWrite1hTokens: 12,
    outputTokens: 20, reasoningTokens: 15, totalTokens: 100,
  }));
  const f = await fixture(source, { mocks: {
    fetchRawEvents: async (id, args) => {
      assert.equal(args.kind, "opencode");
      return { events, total: 302, syncedAt: 100, coverageGaps: [{}] };
    },
    exportUsageEventsCsv: async (name, csv) => { exported = { name, csv }; return {cancelled:true}; },
  } });
  f.api.initRawEvents(); await f.api.openRawEvents({ accountId:"wk", kind:"opencode", label:"Work" });
  const elem = id => f.document.querySelector("#raw-" + id);
  assert.equal(elem("clear").hidden, true);
  assert.match(elem("status").textContent, /归档缺口/);
  assert.equal(elem("body").children.length, 300);
  elem("search").value = "normalized-sample"; f.event(elem("search"), "input");
  assert.match(elem("count").textContent, /301 条.*30,100 tok.*记录成本 \$0.00/);
  assert.match(elem("count").textContent, /300 条未提供成本/);
  elem("export").click(); await tick();
  assert.match(exported.name, /^opencode-usage_/);
  const lines = exported.csv.trimEnd().split("\r\n");
  assert.equal(lines.length, 302);
  assert.match(lines[0], /官方记录成本/); assert.doesNotMatch(lines[0], /实扣/);
  assert.match(lines[1], /,12,15,0,$/); assert.match(lines.at(-1), /req-300.*?,12,15,,$/);
  elem("more").click(); assert.equal(elem("body").children.length, 301);
});

test("Cursor live and retained bills keep their charges, CSV format and clear permissions", async () => {
  const source = await readFile(new URL("../src/raw_events.js", import.meta.url), "utf8");
  let exported;
  const row = { model:"sample", displayModel:"sample", pricedAs:"sample", inputTokens:10, cacheReadTokens:8,
    cacheWriteTokens:2, outputTokens:5, totalTokens:25, actualUsd:0.004, equivalentUsd:0.003, timestampMs:1000 };
  const f = await fixture(source, { mocks: {
    fetchRawEvents: async (id, {kind}) => {
      assert.equal(kind, "cursor");
      return {events:[row], total:1, syncedAt:2000};
    },
    exportUsageEventsCsv: async (_, csv) => { exported = csv; return {cancelled:true}; },
  } });
  f.api.initRawEvents();
  for (const deleted of [false, true]) {
    await f.api.openRawEvents({ accountId:"id", label:"Cursor", deleted });
    const elem = id => f.document.querySelector("#raw-" + id);
    assert.equal(elem("clear").hidden, deleted);
    assert.equal(elem("cost-heading").textContent, "实扣");
    assert.equal(elem("body").children.length, 1);
    assert.equal(elem("modal-sub").textContent.includes("已删除"), deleted);
    elem("export").click(); await tick();
    assert.match(exported, /实扣\(USD\)/);
    assert.match(exported, /0\.004000,0\.003000/);
    assert.doesNotMatch(exported, /请求 ID|1h|官方记录成本/);
    elem("close").click();
  }
});

test("OpenCode empty archives distinguish first sync, coverage gaps and a completed empty range", async () => {
  const source = await readFile(new URL("../src/raw_events.js", import.meta.url), "utf8");
  let data;
  const f = await fixture(source, { mocks: { fetchRawEvents: async () => ({events:[],total:0,...data}) } });
  for (const [coverage, message] of [
    [{syncedAt:0}, /尚未完成首次同步/],
    [{syncedAt:200,coverageGaps:[{start:11,end:12}]}, /已归档部分/],
    [{syncedAt:200,pendingSince:201}, /已归档部分/],
    [{syncedAt:200,coverageGaps:[]}, /该范围内没有用量事件/],
  ]) {
    data = coverage;
    await f.api.openRawEvents({accountId:"wk",kind:"opencode",label:"Work",start:10,end:100});
    assert.match(f.document.querySelector("#raw-empty").textContent, message);
    assert.equal(f.document.querySelector("#raw-export").disabled, true);
    f.document.querySelector("#raw-close").click();
  }
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


test("each installed endpoint has a direct switch button and opening the dialog does not switch", async () => {
  for (const [cli, desktop] of [[true, false], [false, true], [true, true]]) {
    const f = await fixture(linkedSource, { invoke: (cmd, args) => {
      if (cmd === "local_clients_status") return { cli: { installed: cli }, desktop: { installed: desktop, running: desktop } };
      if (cmd === "local_clients_switch") {
        assert.equal(args.closeDesktop, desktop);
        assert.equal(args.target, cli && desktop ? "both" : cli ? "cli" : "desktop");
        return { state: "complete", switched: true, clients: [cli && { target: "cli", status: "success" }, desktop && { target: "desktop", status: "success" }].filter(Boolean) };
      }
      throw new Error(`unexpected command: ${cmd}`);
    } });
    const task = f.api.switchLocalClients({ id: "fixture", kind: "codex" });
    await tick();
    const root = f.document.querySelector("dialog");
    const buttons = [...root.querySelectorAll(".modal-foot button")].filter(button => !button.hidden);
    assert.deepEqual(buttons.map(button => button.textContent), ["仅 CLI", "仅 Desktop", "两端一起"]);
    assert.deepEqual(buttons.map(button => button.disabled), [!cli, !desktop, !(cli && desktop)]);
    assert.equal(root.querySelector('input[type="radio"]'), null);
    assert.equal(f.calls.some(call => call.cmd === "local_clients_switch"), false);
    if (cli) assert.match(root.textContent, /运行中的 CLI 会话保留/);
    if (desktop) assert.match(root.textContent, /关闭并重启.*保存工作/);
    f.event(f.button(cli && desktop ? "两端一起" : cli ? "仅 CLI" : "仅 Desktop"), "click");
    assert.equal(await task, true);
    assert.equal(f.calls.filter(c => c.cmd === "local_clients_switch").length, 1);
    assert.equal(f.calls.some(c => /_client_(close|launch)$/.test(c.cmd)), false);
    assert.equal(root.querySelector(".status").classList.contains("ok"), true);
  }
});

test("a Desktop failure remains partial and duplicate switch clicks share one operation", async () => {
  let completed = 0;
  const f = await fixture(linkedSource, { invoke: cmd => cmd === "local_clients_status"
    ? { cli: { installed: true }, desktop: { installed: true } }
    : { state: "complete", switched: false, clients: [{ target: "cli", status: "success" }, { target: "desktop", status: "failed", error: "client_launch_failed" }] } });
  const task = f.api.switchLocalClients({ id: "fixture", kind: "codex" }, () => completed++);
  assert.equal(f.api.switchLocalClients({ id: "fixture", kind: "codex" }), task);
  assert.equal(f.document.querySelectorAll("dialog").length, 1);
  await tick(); await f.button("两端一起").onclick();
  assert.equal(await task, false); assert.equal(completed, 1);
  const status = f.document.querySelector("dialog .status");
  assert.equal(status.classList.contains("bad"), true);
  assert.match(f.document.querySelector(".opencode-content").textContent, /CLI：切换成功/);
  assert.match(f.document.querySelector(".opencode-content").textContent, /Desktop：.*启动失败/);
  assert.equal(f.document.querySelectorAll('.client-endpoint-result[data-result="success"]').length, 1);
});

test("cancelling first Desktop binding never switches the CLI", async () => {
  const f = await fixture(linkedSource, { invoke: cmd => {
    if (cmd === "local_clients_status") return { cli: { installed: true }, desktop: { installed: true, binding: { state: "unbound" } } };
    if (cmd === "claude_desktop_binding_start") return { state: "pending", id: "binding" };
    if (cmd === "claude_desktop_binding_cancel") return { state: "cancelled" };
    throw new Error(`unexpected command: ${cmd}`);
  } });
  void f.api.switchLocalClients({ id: "fixture", kind: "claude" }); await tick();
  const switching = f.button("两端一起").onclick(); await tick();
  await f.button("开始绑定").onclick();
  const binding = [...f.document.querySelectorAll("dialog")].at(-1);
  f.event(binding.querySelector(".modal-foot button"), "click"); await tick(); await switching;
  assert.equal(f.calls.some(c => c.cmd === "local_clients_switch"), false);
  assert.equal(f.calls.filter(c => c.cmd === "claude_desktop_binding_cancel").length, 1);
  assert.equal(f.document.querySelectorAll("dialog").length, 1);
});

test("successful Desktop binding restores its session before linked switching", async () => {
  const order = [];
  const f = await fixture(linkedSource, { invoke: cmd => {
    order.push(cmd);
    if (cmd === "local_clients_status") return { cli: { installed: true }, desktop: { installed: true, binding: { state: "unbound" } } };
    if (cmd === "claude_desktop_binding_start") return { state: "pending", id: "binding" };
    if (cmd === "claude_desktop_binding_finish") return { state: "complete" };
    if (cmd === "local_clients_switch") return { state: "complete", switched: true, clients: [{ target: "cli", status: "success" }, { target: "desktop", status: "success" }] };
    throw new Error(`unexpected command: ${cmd}`);
  } });
  const task = f.api.switchLocalClients({ id: "fixture", kind: "claude" }); await tick();
  const switching = f.button("两端一起").onclick(); await tick();
  await f.button("开始绑定").onclick(); await f.button("完成绑定").onclick(); await switching;
  assert.equal(await task, true);
  assert.ok(order.indexOf("claude_desktop_binding_finish") < order.indexOf("local_clients_switch"));
  assert.equal(order.includes("claude_desktop_binding_cancel"), false);
});

test("a recovered Desktop binding is rechecked before offering a switch", async () => {
  let recovered = false;
  const f = await fixture(linkedSource, { invoke: cmd => {
    if (cmd === "local_clients_status") return { cli: { installed: false }, desktop: { installed: true,
      binding: recovered ? { state: "bound" } : { state: "pending", operationId: "interrupted", recoveryRequired: true } } };
    if (cmd === "claude_desktop_binding_cancel") { recovered = true; return { state: "cancelled" }; }
    throw new Error(`unexpected command: ${cmd}`);
  } });
  void f.api.switchLocalClients({ id: "fixture", kind: "claude" }); await tick();
  assert.equal(f.button("两端一起").disabled, true);
  await f.button("恢复原登录").onclick();
  assert.equal(recovered, true); assert.equal(f.button("仅 Desktop").disabled, false);
  assert.equal(f.calls.some(c => c.cmd === "local_clients_switch"), false);
});

test("a rejected Desktop identity can be rebound after original login recovery", async () => {
  const f=await fixture(linkedSource,{invoke:cmd=>{
    if(cmd==="claude_desktop_binding_start") return {state:"pending",id:"binding-one"};
    if(cmd==="claude_desktop_binding_finish") throw "claude_desktop_account_mismatch";
    if(cmd==="local_clients_status") return {desktop:{installed:true,binding:{state:"unbound"}}};
    throw new Error(`unexpected command: ${cmd}`);
  }});
  void f.api.bindClaudeDesktop("fixture");
  await f.button("开始绑定").onclick(); await f.button("完成绑定").onclick();
  assert.match(f.document.querySelector("dialog .status").textContent,/账户.*不符/);
  await f.button("重新绑定").onclick();
  assert.equal(f.calls.filter(c=>c.cmd==="claude_desktop_binding_start").length,2);
});

test("Codex force write remains available after failure and uses the shared switch IPC", async () => {
  let forced=0;
  const completed = [];
  const f=await fixture(linkedSource,{invoke:(cmd,args)=>{
    if(cmd==="local_clients_status") return {cli:{installed:true},desktop:{installed:false}};
    if(cmd==="local_clients_switch") {
      assert.equal(args.target,"cli"); assert.equal(args.closeDesktop,false);
      if(!args.force) return {state:"complete",switched:false,clients:[{target:"cli",status:"failed",error:"codex_refresh_denied:reused"}]};
      if(++forced===1) throw "credential_write_failed";
      return {state:"complete",switched:true,clients:[{target:"cli",status:"success"}]};
    }
    throw new Error(`unexpected command: ${cmd}`);
  }});
  const account = { id: "fixture", kind: "codex" };
  const done = f.api.switchLocalClients(account, result => completed.push(result.switched));
  await tick();
  await f.button("仅 CLI").onclick();
  assert.equal(f.api.switchLocalClients(account), done);
  assert.deepEqual(completed, []);
  await f.button("写入保存的凭据副本").onclick();
  assert.match(f.document.querySelector("dialog .status").textContent,/credential_write_failed/);
  await f.button("写入保存的凭据副本").onclick();
  assert.equal(await done, true);
  assert.deepEqual(completed, [true]);
  assert.equal(forced,2);
  assert.equal(f.document.querySelector(".client-force-write"),null);
  assert.equal(f.calls.some(c=>/_client_(close|launch)$/.test(c.cmd)),false);
});

test("Claude Desktop switching stays reachable without a CLI refresh token", async () => {
  const f=await fixture(accountSource);
  const row=f.api.accountRow({id:"desktop-only",kind:"claude",token:"fixture",refreshToken:null,status:{alive:false,accountUuid:"fixture-uuid"}});
  const button=row.querySelector('button[title="切换本机 Claude CLI / Desktop 账户"]');
  assert.ok(button);
  assert.equal(button.disabled,false);
});

test("a Codex retry uses the clicked Desktop target after a renewal error", async () => {
  const f = await fixture(linkedSource, { invoke: (cmd, args) => {
    if (cmd === "local_clients_status") return { cli: { installed: true }, desktop: { installed: true } };
    if (!args.force) throw "codex_refresh_denied:reused";
    return { state: "complete", switched: true, clients: [{ target: "desktop", status: "success" }] };
  } });
  const done = f.api.switchLocalClients({ id: "fixture", kind: "codex" });
  await tick();
  await f.button("仅 Desktop").onclick();
  assert.equal(f.button("两端一起").hidden, true);
  await f.button("两端一起").onclick();
  await f.button("写入保存的凭据副本").onclick();
  assert.equal(await done, true);
  const writes = f.calls.filter(call => call.cmd === "local_clients_switch").map(call => call.args);
  assert.deepEqual(writes.map(args => [args.target, args.closeDesktop, args.force]), [
    ["desktop", true, null], ["desktop", true, true],
  ]);
});

test("one button click submits exactly its own endpoints for every provider", async () => {
  for (const kind of ["opencode", "codex", "claude"]) {
    for (const [target, label] of Object.entries({ cli: "仅 CLI", desktop: "仅 Desktop", both: "两端一起" })) {
      const clients = (target === "both" ? ["cli", "desktop"] : [target]).map(target => ({ target, status: "success" }));
      const f = await fixture(linkedSource, { invoke: cmd => cmd === "local_clients_status"
        ? { cli: { installed: true }, desktop: { installed: true, running: true, binding: { state: "bound" } } }
        : { state: "complete", switched: true, clients } });
      const done = f.api.switchLocalClients({ id: "fixture", kind });
      await tick();
      assert.ok(!f.button(label).disabled);
      f.event(f.button(label), "click");
      assert.equal(await done, true);
      const writes = f.calls.filter(call => call.cmd === "local_clients_switch");
      assert.equal(writes.length, 1);
      assert.equal(writes[0].args.target, target);
      assert.equal(writes[0].args.closeDesktop, target !== "cli");
      assert.equal(f.document.querySelectorAll("dialog .client-endpoint-result").length, clients.length);
    }
  }
});

test("CLI selection bypasses unbound, pending and broken Claude Desktop sessions", async () => {
  for(const desktop of [
    {installed:true,binding:{state:"unbound"}},
    {installed:true,binding:{state:"pending",operationId:"other-window"}},
    {installed:true,switchable:false,error:"claude_desktop_path_ambiguous"},
  ]) {
    const f=await fixture(linkedSource,{invoke:(cmd,args)=>{
      if(cmd==="local_clients_status") return {cli:{installed:true},desktop};
      assert.equal(cmd,"local_clients_switch");assert.equal(args.target,"cli");assert.equal(args.closeDesktop,false);
      return {state:"complete",switched:true,clients:[{target:"cli",status:"success"}]};
    }});
    const task=f.api.switchLocalClients({id:"fixture",kind:"claude"});await tick();
    await f.button("仅 CLI").onclick(); assert.equal(await task,true);
    assert.equal(f.calls.some(c=>c.cmd.startsWith("claude_desktop_binding_")),false);
  }
});

test("Desktop selection remains available when the CLI path is broken", async () => {
  const f=await fixture(linkedSource,{invoke:(cmd,args)=>{
    if(cmd==="local_clients_status") return {cli:{installed:false,error:"client_path_invalid"},desktop:{installed:true}};
    assert.equal(cmd,"local_clients_switch");assert.equal(args.target,"desktop");
    return {state:"complete",switched:true,clients:[{target:"desktop",status:"success"}]};
  }});
  const task=f.api.switchLocalClients({id:"fixture",kind:"opencode"});await tick();
  assert.equal(f.button("两端一起").disabled,true);
  await f.button("仅 Desktop").onclick();assert.equal(await task,true);
});

test("double clicks and clicks on another target cannot start a second switch", async () => {
  const pending=deferred();
  const f=await fixture(linkedSource,{invoke:(cmd,args)=>cmd==="local_clients_status"
    ? {cli:{installed:true},desktop:{installed:true}}
    : (assert.equal(args.target,"cli"),pending.promise)});
  const task=f.api.switchLocalClients({id:"fixture",kind:"opencode"});await tick();
  const saving=f.button("仅 CLI").onclick();
  for (const label of ["仅 CLI", "仅 Desktop", "两端一起"]) {
    assert.equal(f.button(label).disabled, true);
    await f.button(label).onclick();
  }
  assert.equal(f.calls.filter(call => call.cmd === "local_clients_switch").length, 1);
  pending.resolve({state:"complete",switched:true,clients:[{target:"cli",status:"success"}]});await saving;await task;
});

test("unavailable endpoints stay disabled and Escape dismisses without switching", async () => {
  const absent=await fixture(linkedSource,{invoke:()=>({cli:{installed:false},desktop:{installed:false}})});
  const done = absent.api.switchLocalClients({id:"fixture",kind:"opencode"});await tick();
  for (const label of ["仅 CLI", "仅 Desktop", "两端一起"]) {
    assert.equal(absent.button(label).disabled, true);
    await absent.button(label).onclick();
  }
  absent.event(absent.document.querySelector("dialog"), "keydown", { key: "Escape" });
  assert.equal(await done, false);
  await tick();
  assert.equal(absent.document.querySelector("dialog"), null);
  assert.equal(absent.calls.some(c=>c.cmd==="local_clients_switch"),false);
});
